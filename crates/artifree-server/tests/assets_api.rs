//! Real HTTP asset envelopes, exact reads, conditional replay and authority.

use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::SqliteTokenStore;
use artifree_server::auth::AccessToken;
use artifree_server::http::{self, AppState, Settings};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode, header};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use tower::ServiceExt;
type TestResult = Result<(), Box<dyn std::error::Error>>;
const OWNER: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
struct Harness {
    dir: PathBuf,
    api: Router,
}
struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    bytes: Vec<u8>,
}
impl Reply {
    fn json(&self) -> Result<Value, serde_json::Error> {
        serde_json::from_slice(&self.bytes)
    }
}
impl Harness {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        static SEQUENCE: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "artifree-assets-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir)?;
        let db = dir.join("db.sqlite3");
        let state = AppState::new(
            SqliteStore::open(&db)?,
            SqliteTokenStore::open(&db)?,
            AccessToken::parse(OWNER).ok_or("token")?,
            Settings::default(),
        )?;
        Ok(Self {
            dir,
            api: http::router(state),
        })
    }
    async fn call(
        &self,
        method: &str,
        path: &str,
        key: Option<&str>,
        body: Option<Value>,
    ) -> Result<Reply, Box<dyn std::error::Error>> {
        self.raw(method, path, Some(OWNER), key, body.map(|v| v.to_string()))
            .await
    }
    async fn raw(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        key: Option<&str>,
        body: Option<String>,
    ) -> Result<Reply, Box<dyn std::error::Error>> {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(token) = token {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        if let Some(key) = key {
            request = request.header("Idempotency-Key", format!("asset-test-{key}"));
        }
        let response = self
            .api
            .clone()
            .oneshot(request.body(Body::from(body.unwrap_or_default()))?)
            .await?;
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = to_bytes(response.into_body(), 10 * 1024 * 1024)
            .await?
            .to_vec();
        Ok(Reply {
            status,
            headers,
            bytes,
        })
    }
    async fn artifact(&self) -> Result<String, Box<dyn std::error::Error>> {
        let result=self.call("POST","/v1/artifacts",Some("create"),Some(json!({"title":"Upload fixture","files":[{"path":"index.html","encoding":"utf8","content":"<h1>original fixture</h1>"}]}))).await?;
        assert_eq!(result.status, StatusCode::CREATED, "{:?}", result.json());
        Ok(result.json()?["artifact"]["id"]
            .as_str()
            .ok_or("artifact")?
            .to_owned())
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
fn upload(base: u64, media: &str, bytes: &[u8]) -> Value {
    json!({"base_asset_version":base,"content_type":media,"content_base64":STANDARD.encode(bytes)})
}
fn secure(reply: &Reply) {
    assert_eq!(reply.headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(reply.headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(reply.headers[header::REFERRER_POLICY], "no-referrer");
}
#[tokio::test]
async fn upload_read_delete_and_lost_reply_replay_preserve_exact_bytes() -> TestResult {
    let h = Harness::new()?;
    let artifact = h.artifact().await?;
    let path = format!("/v1/artifacts/{artifact}/assets");
    let bytes = b"\xef\xbb\xbfa,b\r\n1,2\r\n";
    let request = upload(0, "text/csv", bytes);
    let first = h
        .call("POST", &path, Some("upload"), Some(request.clone()))
        .await?;
    secure(&first);
    assert_eq!(first.status, StatusCode::CREATED, "{:?}", first.json());
    let receipt = first.json()?;
    let id = receipt["asset"]["id"].as_str().ok_or("asset")?;
    assert_eq!(id.len(), 64);
    assert_eq!(receipt["asset_version"], 1);
    assert_eq!(receipt["asset"]["size_bytes"], bytes.len());
    assert_eq!(first.headers[header::LOCATION], format!("{path}/{id}"));
    let content = h
        .call("GET", &format!("{path}/{id}/content"), None, None)
        .await?;
    secure(&content);
    assert_eq!(content.bytes, bytes);
    assert_eq!(content.headers[header::CONTENT_TYPE], "text/csv");
    assert!(
        content.headers[header::CONTENT_DISPOSITION]
            .to_str()?
            .starts_with("attachment;")
    );
    assert_eq!(
        content.headers[header::CONTENT_SECURITY_POLICY],
        "sandbox; default-src 'none'"
    );
    let meta = h.call("GET", &format!("{path}/{id}"), None, None).await?;
    assert_eq!(meta.json()?["asset"], receipt["asset"]);
    let deleted = h
        .call(
            "DELETE",
            &format!("{path}/{id}"),
            Some("delete"),
            Some(json!({"base_asset_version":1})),
        )
        .await?;
    assert_eq!(deleted.json()?["deleted"], true);
    let replay = h.call("POST", &path, Some("upload"), Some(request)).await?;
    assert_eq!(replay.status, StatusCode::CREATED);
    assert_eq!(replay.json()?["asset"], receipt["asset"]);
    assert_eq!(replay.json()?["asset_version"], 1);
    assert_eq!(replay.json()?["replayed"], true);
    let missing = h
        .call("GET", &format!("{path}/{id}/content"), None, None)
        .await?;
    secure(&missing);
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    let noop = h
        .call(
            "DELETE",
            &format!("{path}/{id}"),
            Some("noop"),
            Some(json!({"base_asset_version":2})),
        )
        .await?;
    assert_eq!(noop.json()?["deleted"], false);
    assert_eq!(noop.json()?["asset_version"], 2);
    let list = h.call("GET", &path, None, None).await?;
    assert_eq!(list.json()?["assets"], json!([]));
    assert_eq!(list.json()?["asset_version"], 2);
    Ok(())
}
#[tokio::test]
async fn envelopes_and_content_errors_do_not_consume_a_key() -> TestResult {
    let h = Harness::new()?;
    let artifact = h.artifact().await?;
    let path = format!("/v1/artifacts/{artifact}/assets");
    for (name, request, status, code) in [
        ("empty", upload(0, "text/plain", b""), 422, "invalid_asset"),
        (
            "utf8",
            upload(0, "text/plain", &[0xff]),
            422,
            "invalid_asset",
        ),
        (
            "control",
            upload(0, "text/csv", b"a\0b"),
            422,
            "invalid_asset",
        ),
        (
            "type",
            upload(0, "image/svg+xml", b"<svg/>"),
            422,
            "asset_type_unsupported",
        ),
        (
            "jpeg",
            upload(0, "image/jpeg", b"\xff\xd8\xff\xd9"),
            422,
            "asset_type_unsupported",
        ),
        (
            "png",
            upload(0, "image/png", b"not png"),
            422,
            "invalid_asset",
        ),
        (
            "mime",
            upload(0, "Text/Plain", b"a"),
            400,
            "invalid_request",
        ),
        (
            "parameter",
            upload(0, "text/plain;charset=utf-8", b"a"),
            400,
            "invalid_request",
        ),
        (
            "base64",
            json!({"base_asset_version":0,"content_type":"text/plain","content_base64":"YQ"}),
            400,
            "invalid_request",
        ),
        (
            "fields",
            json!({"base_asset_version":0,"content_type":"text/plain","content_base64":"YQ==","actor":"bob"}),
            400,
            "invalid_request",
        ),
    ] {
        let result = h.call("POST", &path, Some("retry"), Some(request)).await?;
        secure(&result);
        assert_eq!(
            result.status.as_u16(),
            status,
            "{name}: {:?}",
            result.json()
        );
        assert_eq!(result.json()?["error"]["code"], code, "{name}");
    }
    let good = h
        .call(
            "POST",
            &path,
            Some("retry"),
            Some(upload(0, "text/plain", b"a")),
        )
        .await?;
    assert_eq!(good.status, StatusCode::CREATED);
    let stale = h
        .call(
            "POST",
            &path,
            Some("fresh"),
            Some(upload(0, "text/plain", b"")),
        )
        .await?;
    assert_eq!(stale.json()?["error"]["code"], "asset_conflict");
    assert_eq!(stale.json()?["error"]["current_asset_version"], 1);
    let reused = h
        .call(
            "POST",
            &path,
            Some("retry"),
            Some(upload(0, "text/plain", b"b")),
        )
        .await?;
    assert_eq!(reused.json()?["error"]["code"], "request_key_reused");
    Ok(())
}
#[tokio::test]
async fn authentication_identity_and_outer_bounds_precede_content_validation() -> TestResult {
    let h = Harness::new()?;
    let artifact = h.artifact().await?;
    let path = format!("/v1/artifacts/{artifact}/assets");
    let unauth = h.raw("POST", &path, None, None, Some("{".into())).await?;
    secure(&unauth);
    assert_eq!(unauth.status, StatusCode::UNAUTHORIZED);
    let invalid = h
        .raw(
            "POST",
            "/v1/artifacts/invalid/assets",
            Some(OWNER),
            Some("invalid"),
            Some("{".into()),
        )
        .await?;
    assert_eq!(invalid.status, StatusCode::NOT_FOUND);
    let overflow = h
        .raw(
            "POST",
            &path,
            Some(OWNER),
            Some("big"),
            Some(" ".repeat(8 * 1024 * 1024 + 1)),
        )
        .await?;
    secure(&overflow);
    assert_eq!(overflow.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(overflow.json()?["error"]["code"], "payload_too_large");
    let absent = format!("/v1/artifacts/art_{}/assets", "0".repeat(32));
    let missing = h
        .call(
            "POST",
            &absent,
            Some("missing"),
            Some(upload(0, "text/plain", b"")),
        )
        .await?;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    Ok(())
}
#[tokio::test]
async fn exact_five_mib_text_succeeds_and_one_more_byte_reserves_nothing() -> TestResult {
    let h = Harness::new()?;
    let artifact = h.artifact().await?;
    let path = format!("/v1/artifacts/{artifact}/assets");
    let too_big = vec![b'a'; 5 * 1024 * 1024 + 1];
    let refused = h
        .call(
            "POST",
            &path,
            Some("boundary"),
            Some(upload(0, "text/plain", &too_big)),
        )
        .await?;
    assert_eq!(refused.json()?["error"]["code"], "asset_limit_exceeded");
    let good = h
        .call(
            "POST",
            &path,
            Some("boundary"),
            Some(upload(0, "text/plain", &too_big[..5 * 1024 * 1024])),
        )
        .await?;
    assert_eq!(good.status, StatusCode::CREATED, "{:?}", good.json());
    assert_eq!(good.json()?["asset"]["size_bytes"], 5 * 1024 * 1024);
    Ok(())
}

#[test]
fn policy_retirement_replay_skips_validator_and_validation_holds_no_database_lock() -> TestResult {
    use artifree_core::artifacts::ArtifactService;
    use artifree_core::assets::AssetUpload;
    use artifree_domain::assets::{AssetMediaType, AssetRejection, AssetVersion};
    use artifree_domain::identity::{ActorId, RequestKey, Title, UnixMillis};
    use artifree_domain::package::FilePackage;
    use artifree_ports::artifact_store::Delivery;
    use std::sync::mpsc;
    use std::time::Duration;
    let h = Harness::new()?;
    let store = SqliteStore::open(&h.dir.join("db.sqlite3"))?;
    let service = ArtifactService::new(store);
    let actor: ActorId = "local".parse()?;
    let create_key: RequestKey = "create-service".parse()?;
    let title: Title = "Service fixture".parse()?;
    let package = FilePackage::new(vec![("index.html".parse()?, b"<h1>fixture</h1>".to_vec())])?;
    let artifact = service
        .create(&actor, &create_key, &title, &package)?
        .artifact
        .id;
    let media: AssetMediaType = "text/plain".parse()?;
    let key: RequestKey = "upload-service".parse()?;
    let upload = AssetUpload {
        base: AssetVersion(0),
        content_type: &media,
        bytes: b"a",
        now: UnixMillis(10),
    };
    let first = service.upload_asset(&actor, &key, &artifact, upload)?;
    let replay = service.upload_asset_with_validator(&actor, &key, &artifact, upload, |_, _| {
        panic!("a committed replay must skip retired policy")
    })?;
    assert_eq!(replay.delivery, Delivery::Replayed);
    assert_eq!(replay.id, first.id);
    let fresh_key: RequestKey = "upload-retired".parse()?;
    let fresh = service.upload_asset_with_validator(
        &actor,
        &fresh_key,
        &artifact,
        AssetUpload {
            base: AssetVersion(1),
            ..upload
        },
        |_, _| Err(AssetRejection::UnsupportedType),
    );
    assert_eq!(
        fresh,
        Err(artifree_ports::asset_store::AssetStoreError::Rejected(
            AssetRejection::UnsupportedType
        ))
    );
    // A write from another connection commits while content validation is held.
    // Channels provide deterministic ordering; timeout bounds a broken lock guard.
    let (entered_tx, entered_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let worker_service = ArtifactService::new(SqliteStore::open(&h.dir.join("db.sqlite3"))?);
    let worker_actor = actor.clone();
    let worker_artifact = artifact.clone();
    let worker = std::thread::spawn(move || {
        let key: RequestKey = "held-upload"
            .parse()
            .unwrap_or_else(|error| panic!("{error}"));
        let media: AssetMediaType = "text/plain"
            .parse()
            .unwrap_or_else(|error| panic!("{error}"));
        worker_service.upload_asset_with_validator(
            &worker_actor,
            &key,
            &worker_artifact,
            AssetUpload {
                base: AssetVersion(1),
                content_type: &media,
                bytes: b"b",
                now: UnixMillis(11),
            },
            |_, _| {
                entered_tx
                    .send(())
                    .unwrap_or_else(|error| panic!("{error}"));
                resume_rx
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap_or_else(|error| panic!("{error}"));
                Ok(())
            },
        )
    });
    entered_rx.recv_timeout(Duration::from_secs(10))?;
    let write_result = rusqlite::Connection::open(h.dir.join("db.sqlite3"))?.execute(
        "UPDATE artifacts SET updated_at=updated_at+1 WHERE id=?1",
        [artifact.as_str()],
    );
    resume_tx.send(())?;
    assert_eq!(
        write_result?, 1,
        "unrelated write committed while validator remained held"
    );
    assert_eq!(
        worker.join().map_err(|_| "worker failed")??.version,
        AssetVersion(2)
    );
    Ok(())
}

struct RaceResult {
    winner: artifree_ports::asset_store::AssetReceipt,
    loser: Result<
        artifree_ports::asset_store::AssetReceipt,
        artifree_ports::asset_store::AssetStoreError,
    >,
}
fn competing_upload(held_bytes: &'static [u8]) -> Result<RaceResult, Box<dyn std::error::Error>> {
    use artifree_core::artifacts::ArtifactService;
    use artifree_core::assets::AssetUpload;
    use artifree_domain::assets::{AssetMediaType, AssetRejection, AssetVersion};
    use artifree_domain::identity::{ActorId, RequestKey, Title, UnixMillis};
    use artifree_domain::package::FilePackage;
    use std::sync::mpsc;
    use std::time::Duration;
    let harness = Harness::new()?;
    let database = harness.dir.join("db.sqlite3");
    let service = ArtifactService::new(SqliteStore::open(&database)?);
    let actor: ActorId = "local".parse()?;
    let create: RequestKey = "create-race".parse()?;
    let title: Title = "Race".parse()?;
    let package = FilePackage::new(vec![("index.html".parse()?, b"<h1>race</h1>".to_vec())])?;
    let artifact = service
        .create(&actor, &create, &title, &package)?
        .artifact
        .id;
    let media: AssetMediaType = "text/plain".parse()?;
    let key: RequestKey = "upload-race".parse()?;
    let worker_service = ArtifactService::new(SqliteStore::open(&database)?);
    let worker_actor = actor.clone();
    let worker_artifact = artifact.clone();
    let worker_key = key.clone();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let media: AssetMediaType = "text/plain"
            .parse()
            .unwrap_or_else(|error| panic!("{error}"));
        worker_service.upload_asset_with_validator(
            &worker_actor,
            &worker_key,
            &worker_artifact,
            AssetUpload {
                base: AssetVersion(0),
                content_type: &media,
                bytes: held_bytes,
                now: UnixMillis(20),
            },
            |_, _| {
                entered_tx
                    .send(())
                    .unwrap_or_else(|error| panic!("{error}"));
                resume_rx
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap_or_else(|error| panic!("{error}"));
                Err(AssetRejection::Invalid)
            },
        )
    });
    entered_rx.recv_timeout(Duration::from_secs(10))?;
    let winner = service.upload_asset(
        &actor,
        &key,
        &artifact,
        AssetUpload {
            base: AssetVersion(0),
            content_type: &media,
            bytes: b"a",
            now: UnixMillis(20),
        },
    );
    resume_tx.send(())?;
    let loser = worker.join().map_err(|_| "worker failed")?;
    let winner = winner?;
    let snapshot = service.assets(&actor, &artifact)?;
    assert_eq!(
        (snapshot.version, snapshot.assets.len(), snapshot.bytes),
        (AssetVersion(1), 1, 1)
    );
    let receipts: i64 = rusqlite::Connection::open(database)?.query_row(
        "SELECT count(*) FROM asset_requests",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(receipts, 1);
    Ok(RaceResult { winner, loser })
}
#[test]
fn matching_concurrent_loser_replays_despite_its_policy_rejection() -> TestResult {
    let race = competing_upload(b"a")?;
    let loser = race.loser?;
    assert_eq!(loser.id, race.winner.id);
    assert_eq!(
        loser.delivery,
        artifree_ports::artifact_store::Delivery::Replayed
    );
    Ok(())
}
#[test]
fn different_byte_concurrent_loser_gets_key_reused_despite_its_policy_rejection() -> TestResult {
    let race = competing_upload(b"b")?;
    assert_eq!(
        race.loser,
        Err(artifree_ports::asset_store::AssetStoreError::RequestKeyReused)
    );
    Ok(())
}

#[tokio::test]
async fn restricted_rgba_png_preserves_transparent_pixel_bytes() -> TestResult {
    let harness = Harness::new()?;
    let artifact = harness.artifact().await?;
    let path = format!("/v1/artifacts/{artifact}/assets");
    // Original fixture: 1x1 RGBA8, pixel (0,128,255,0), filter 0; only
    // IHDR, complete zlib IDAT and IEND, each with an independently computed CRC.
    let bytes=STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgaPjPAAADgwGAJwGhsAAAAABJRU5ErkJggg==")?;
    let uploaded = harness
        .call(
            "POST",
            &path,
            Some("transparent-png"),
            Some(upload(0, "image/png", &bytes)),
        )
        .await?;
    assert_eq!(
        uploaded.status,
        StatusCode::CREATED,
        "{:?}",
        uploaded.json()
    );
    let receipt = uploaded.json()?;
    let id = receipt["asset"]["id"].as_str().ok_or("asset id")?;
    let content = harness
        .call("GET", &format!("{path}/{id}/content"), None, None)
        .await?;
    secure(&content);
    assert_eq!(content.status, StatusCode::OK);
    assert_eq!(content.headers[header::CONTENT_TYPE], "image/png");
    assert_eq!(content.bytes, bytes);
    Ok(())
}

#[tokio::test]
async fn source_save_and_restore_never_rewind_current_assets_or_their_version() -> TestResult {
    let harness = Harness::new()?;
    let artifact = harness.artifact().await?;
    let path = format!("/v1/artifacts/{artifact}/assets");
    let uploaded = harness
        .call(
            "POST",
            &path,
            Some("before-source-change"),
            Some(upload(0, "text/plain", b"independent current asset")),
        )
        .await?;
    assert_eq!(uploaded.status, StatusCode::CREATED);
    let original = uploaded.json()?;
    let id = original["asset"]["id"].as_str().ok_or("asset")?;
    let saved=harness.call("POST",&format!("/v1/artifacts/{artifact}/revisions"),Some("source-change"),Some(json!({"base_revision":1,"changes":[{"op":"put","path":"index.html","encoding":"utf8","content":"<h1>new source</h1>"}]}))).await?;
    assert_eq!(saved.status, StatusCode::CREATED);
    let restored = harness
        .call(
            "POST",
            &format!("/v1/artifacts/{artifact}/revisions/1/restore"),
            Some("source-restore"),
            Some(json!({"base_revision":2})),
        )
        .await?;
    assert_eq!(restored.status, StatusCode::CREATED);
    assert_eq!(restored.json()?["artifact"]["current_revision"], 3);
    let list = harness.call("GET", &path, None, None).await?;
    assert_eq!(list.json()?["asset_version"], 1);
    assert_eq!(list.json()?["assets"], json!([original["asset"]]));
    let content = harness
        .call("GET", &format!("{path}/{id}/content"), None, None)
        .await?;
    assert_eq!(content.bytes, b"independent current asset");
    let next = harness
        .call(
            "POST",
            &path,
            Some("after-source-restore"),
            Some(upload(1, "text/plain", b"new current asset")),
        )
        .await?;
    assert_eq!(next.status, StatusCode::CREATED);
    assert_eq!(next.json()?["asset_version"], 2);
    Ok(())
}
