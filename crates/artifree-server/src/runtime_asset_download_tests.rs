//! Runtime asset downloads that race a grant change, a connection close or
//! another download for the actor's build slots (decision 0045, rules 12 and
//! 13).
//!
//! A download takes a build slot after its identifier is parsed, and holds it
//! until it returns. Its last check of the binding and the grant is synchronous
//! and follows its association lookup, so a grant withdrawn or a connection
//! closed after the bytes were read releases none of them. The slots are shared
//! with source downloads. These tests hold each call at the blocking task it
//! waits for, so that the change lands where the test wants it.

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
    FileUpload, RuntimeAssetDownloadRequest, RuntimeDownloadKind, RuntimeDownloadRequest,
    RuntimeView, SaveRevisionRequest, SetRuntimeGrantRequest, UploadAssetRequest,
};
use artifree_domain::identity::{ActorId, StateGeneration};
use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::SqliteTokenStore;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use tower::ServiceExt;

use crate::auth::AccessToken;
use crate::http::{AppState, Settings};
use crate::limits::RequestLimits;
use crate::operations::{self, ApiError};
use crate::runtime_assets;
use crate::runtime_source::{self, FrameCall};
use crate::{changes, preview};

/// A 1x1 PNG with a text chunk and an unknown ancillary chunk, stored as sent.
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAACnRFWHRDb21tZW50AGhpoqJYZgAAAAJwclZ0enrhbIZhAAAADUlEQVR4nGNgaPjPAAADgwGAJwGhsAAAAABJRU5ErkJggg==";
/// The text of `index.html`, the one file of the artifact's source.
const INDEX_HTML: &str =
    "<!doctype html><html><head><!-- artifree-runtime --></head><body>test</body></html>";

/// A fresh directory for one test's `SQLite` files, removed when the test ends.
struct TestDirectory(PathBuf);

/// Keeps the directories of tests that start in the same nanosecond apart.
static DIRECTORIES: AtomicU64 = AtomicU64::new(0);

impl TestDirectory {
    fn new() -> Result<Self, Box<dyn Error>> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let count = DIRECTORIES.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "artifree-asset-download-{}-{nonce}-{count}",
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
        format!("asset-download-save-{base}").parse()?,
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

/// An artifact whose manifest declares `assets.download` and `source.download`.
/// Its head is revision 2, the one after the manifest, and it holds no assets.
/// Returns its identifier.
async fn declared_artifact(state: &AppState, actor: &ActorId) -> Result<String, Box<dyn Error>> {
    let created = operations::create(
        state,
        actor.clone(),
        "asset-download-artifact".parse()?,
        CreateArtifactRequest {
            title: "Asset download".into(),
            files: vec![FileUpload {
                path: "index.html".into(),
                encoding: ContentEncoding::Utf8,
                content: INDEX_HTML.into(),
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
        r#"{"sdk":"artifree.app.v1","capabilities":["assets.download","source.download"]}"#,
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
        "asset-download-owner".parse()?,
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

/// Opens a connection on `artifact`, grants it `capabilities`, and creates a
/// view pinned to revision 2. The view's bootstrap is served, as the frame
/// would serve it, so the view is live. Returns the view and the connection's
/// session identifier.
async fn granted_view(
    state: &AppState,
    actor: &ActorId,
    artifact: &str,
    capabilities: &[&str],
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
            capabilities: capabilities.iter().map(|name| (*name).to_owned()).collect(),
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
    assert_eq!(
        view.granted, capabilities,
        "the grant must list exactly the requested capabilities"
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

/// The frame's request for the asset `asset` of `view`.
fn asset_download_request(view: &RuntimeView, asset: &str) -> RuntimeAssetDownloadRequest {
    RuntimeAssetDownloadRequest {
        initialization: view.initialization.clone(),
        asset_id: asset.into(),
    }
}

/// The frame's request for the file `index.html` of the pinned source of `view`.
fn source_download_request(view: &RuntimeView) -> RuntimeDownloadRequest {
    RuntimeDownloadRequest {
        initialization: view.initialization.clone(),
        kind: RuntimeDownloadKind::File,
        path: Some("index.html".into()),
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

/// Polls `request` once: its output if it has completed, and `None` while it
/// is still waiting. A test that must not see a wait uses this, so that a wait
/// reports as a failure instead of blocking the test.
async fn poll_once<T>(request: &mut Pin<Box<impl Future<Output = T>>>) -> Option<T> {
    poll_fn(|context| match request.as_mut().poll(context) {
        Poll::Ready(output) => Poll::Ready(Some(output)),
        Poll::Pending => Poll::Ready(None),
    })
    .await
}

/// Returns once every blocking call queued so far has run. The pool has one
/// thread in these tests, so the calls run in the order they were queued.
async fn settle() -> Result<(), Box<dyn Error>> {
    tokio::task::spawn_blocking(|| ()).await?;
    Ok(())
}

/// Holds the blocking thread, then polls `request` once, so that the blocking
/// job it queues next waits behind the held thread. The caller lets the thread
/// go with [`let_run`].
async fn hold_next_job<T>(
    request: &mut Pin<Box<impl Future<Output = T>>>,
) -> Result<(mpsc::Sender<()>, tokio::task::JoinHandle<()>), Box<dyn Error>> {
    let (release, blocker) = hold_blocking_thread().await?;
    assert!(
        pends(request).await,
        "the request must wait behind the held thread"
    );
    Ok((release, blocker))
}

/// Lets the held thread go, so that the job queued behind it runs, and returns
/// once every job queued so far has run.
async fn let_run(
    release: &mpsc::Sender<()>,
    blocker: tokio::task::JoinHandle<()>,
) -> Result<(), Box<dyn Error>> {
    release.send(())?;
    blocker.await?;
    settle().await
}

/// Drives `download` past its first artifact lookup, its build slot, the read
/// of its bytes and its second artifact lookup. It is held where its
/// association lookup waits behind a blocking thread the test holds. The caller
/// lets the thread go with [`let_run`].
async fn park_before_the_association_lookup<T>(
    download: &mut Pin<Box<impl Future<Output = T>>>,
) -> Result<(mpsc::Sender<()>, tokio::task::JoinHandle<()>), Box<dyn Error>> {
    for _ in 0..3 {
        let (release, blocker) = hold_next_job(download).await?;
        let_run(&release, blocker).await?;
    }
    hold_next_job(download).await
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

/// The refusal of a download that found both build slots taken: `429` with the
/// `rate_limited` code, given at once, without waiting for any blocking job.
async fn refused_at_once<T>(
    download: &mut Pin<Box<impl Future<Output = Result<T, ApiError>>>>,
) -> Result<(), Box<dyn Error>> {
    let outcome = poll_once(download)
        .await
        .ok_or("a download over the slot limit must be refused at once, not wait")?;
    let error = refusal(outcome, StatusCode::TOO_MANY_REQUESTS)?;
    assert_eq!(error.body.code, ErrorCode::RateLimited);
    Ok(())
}

/// With nothing changed, a download parked at its association lookup releases
/// its bytes under the name of the asset. The refusals below therefore come
/// from the change alone, not from a parked download that cannot finish.
#[test]
fn a_parked_download_releases_its_bytes_when_nothing_changes() -> Result<(), Box<dyn Error>> {
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
        let (view, _session) =
            granted_view(&state, &actor, &artifact, &["assets.download"]).await?;
        let call = frame_call(&actor, &view);
        let request = asset_download_request(&view, &asset);
        let mut download = Box::pin(runtime_assets::download(&state, &call, &request));
        let (release, blocker) = park_before_the_association_lookup(&mut download).await?;
        let_run(&release, blocker).await?;
        let downloaded = download.await.map_err(|error| error.body.message)?;
        assert_eq!(
            downloaded.bytes,
            BASE64.decode(PNG)?,
            "the stored bytes must be released"
        );
        assert_eq!(downloaded.file_name, format!("asset-{asset}.png"));
        Ok::<(), Box<dyn Error>>(())
    })
}

/// A grant withdrawn after a download's association lookup, and before its
/// final check, releases no bytes: `403`. The bytes were read and the lookup
/// found the asset, so only the final check can refuse the call.
#[test]
fn a_download_whose_grant_is_withdrawn_before_its_final_check_releases_no_bytes()
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
        let (view, session) = granted_view(&state, &actor, &artifact, &["assets.download"]).await?;
        let call = frame_call(&actor, &view);
        let request = asset_download_request(&view, &asset);
        let mut download = Box::pin(runtime_assets::download(&state, &call, &request));
        let (release, blocker) = park_before_the_association_lookup(&mut download).await?;
        // The withdrawal's job queues behind the association lookup, so the grant
        // changes after that lookup and before the download's final check.
        let mut withdrawal = Box::pin(withdraw_grant(&state, &actor, &session, &view));
        assert!(
            pends(&mut withdrawal).await,
            "the withdrawal must wait behind the association lookup"
        );
        let_run(&release, blocker).await?;
        withdrawal.await?;
        refusal(download.await, StatusCode::FORBIDDEN)?;
        Ok::<(), Box<dyn Error>>(())
    })
}

/// A connection closed after a download's association lookup, and before its
/// final check, releases no bytes: `404`, the answer for any closed connection.
#[test]
fn a_download_whose_connection_is_closed_before_its_final_check_releases_no_bytes()
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
        let (view, _session) =
            granted_view(&state, &actor, &artifact, &["assets.download"]).await?;
        let call = frame_call(&actor, &view);
        let request = asset_download_request(&view, &asset);
        let mut download = Box::pin(runtime_assets::download(&state, &call, &request));
        let (release, blocker) = park_before_the_association_lookup(&mut download).await?;
        let_run(&release, blocker).await?;
        state.revoke_actor(&actor);
        refusal(download.await, StatusCode::NOT_FOUND)?;
        Ok::<(), Box<dyn Error>>(())
    })
}

/// Two downloads, one of each kind, hold both build slots of the actor. The
/// third of either kind is refused with `429 rate_limited` at once, before it
/// reads any bytes. A download that returns frees its slot for the next one.
#[test]
fn asset_and_source_downloads_share_two_slots_and_a_third_is_refused() -> Result<(), Box<dyn Error>>
{
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
        let (view, _session) = granted_view(
            &state,
            &actor,
            &artifact,
            &["assets.download", "source.download"],
        )
        .await?;
        let call = frame_call(&actor, &view);
        let asset_request = asset_download_request(&view, &asset);
        let file_request = source_download_request(&view);
        let mut asset_download = Box::pin(runtime_assets::download(&state, &call, &asset_request));
        let mut file_download = Box::pin(runtime_source::download(&state, &call, &file_request));
        let mut third_asset = Box::pin(runtime_assets::download(&state, &call, &asset_request));
        let mut third_file = Box::pin(runtime_source::download(&state, &call, &file_request));
        // Each call queues its first artifact lookup behind one held thread, so
        // the lookups run in the order the calls were made.
        let (release, blocker) = hold_blocking_thread().await?;
        assert!(
            pends(&mut asset_download).await,
            "the asset download must wait for its lookup"
        );
        assert!(
            pends(&mut file_download).await,
            "the source download must wait for its lookup"
        );
        assert!(
            pends(&mut third_asset).await,
            "the third asset download must wait for its lookup"
        );
        assert!(
            pends(&mut third_file).await,
            "the third source download must wait for its lookup"
        );
        let_run(&release, blocker).await?;
        // The first two take the two slots, then queue their reads behind a
        // second held thread, so both hold a slot while they wait.
        let (release, blocker) = hold_blocking_thread().await?;
        assert!(
            pends(&mut asset_download).await,
            "the asset download must hold a slot while it reads its bytes"
        );
        assert!(
            pends(&mut file_download).await,
            "the source download must hold a slot while it reads its file"
        );
        // With both slots taken, the third of either kind is refused at once.
        refused_at_once(&mut third_asset).await?;
        refused_at_once(&mut third_file).await?;
        let_run(&release, blocker).await?;
        // The asset download returns and frees its slot. The next asset download
        // is built while the source download still holds the other slot.
        let first = asset_download.await.map_err(|error| error.body.message)?;
        assert_eq!(first.bytes, BASE64.decode(PNG)?);
        let next = runtime_assets::download(&state, &call, &asset_request)
            .await
            .map_err(|error| error.body.message)?;
        assert_eq!(
            next.bytes,
            BASE64.decode(PNG)?,
            "a freed slot must build the next download"
        );
        let stored = file_download.await.map_err(|error| error.body.message)?;
        assert_eq!(stored.bytes, INDEX_HTML.as_bytes());
        Ok::<(), Box<dyn Error>>(())
    })
}
