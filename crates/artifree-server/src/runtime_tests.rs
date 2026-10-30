//! Runtime view races across the real registry and artifact service.

use std::error::Error;
use std::future::{Future, poll_fn};
use std::path::PathBuf;
use std::sync::mpsc;
use std::task::Poll;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use artifree_contracts::api::{
    ContentEncoding, CreateArtifactRequest, CreateRuntimeViewRequest, FileUpload, RuntimeView,
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
    let store = SqliteStore::open(&directory.0.join("runtime.sqlite3"))?;
    let token = AccessToken::parse(&"0".repeat(64)).ok_or("invalid fixture token")?;
    Ok(AppState::new(
        store,
        token,
        Settings {
            content_origin: "http://127.0.0.1:7411".into(),
            preview_ttl: Duration::from_secs(60),
            frame_ancestors: "http://127.0.0.1:*".into(),
            limits: RequestLimits::default(),
            change_streams: changes::DEFAULT_STREAMS_PER_ACTOR,
        },
    )?)
}

async fn initialized_view(
    state: &AppState,
    actor: &ActorId,
) -> Result<RuntimeView, Box<dyn Error>> {
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
    let view = operations::create_runtime_view(
        state,
        actor.clone(),
        &created.artifact.id,
        1,
        CreateRuntimeViewRequest {
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
    Ok(view)
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
        let view = initialized_view(&state, &actor).await?;
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
