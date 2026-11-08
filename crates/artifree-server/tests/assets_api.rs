//! Real HTTP asset envelopes, exact reads, conditional replay and authority.

use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::SqliteTokenStore;
use artifree_server::auth::AccessToken;
use artifree_server::http::{self, AppState, Settings};
use artifree_server::limits::RequestLimits;
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
        Self::configured(Settings::default())
    }
    /// A harness whose request budget admits the few hundred uploads that
    /// fill an actor's asset limit.
    fn unthrottled() -> Result<Self, Box<dyn std::error::Error>> {
        Self::configured(Settings {
            limits: RequestLimits {
                burst: 1000,
                ..RequestLimits::default()
            },
            ..Settings::default()
        })
    }
    fn configured(settings: Settings) -> Result<Self, Box<dyn std::error::Error>> {
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
            settings,
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
            "asset_type_unsupported",
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
async fn valid_and_invalid_png_uploads_are_unsupported_and_leave_the_key_free() -> TestResult {
    let harness = Harness::new()?;
    let artifact = harness.artifact().await?;
    let path = format!("/v1/artifacts/{artifact}/assets");
    // Independently authored valid 1x1 RGBA PNG. Validity never enables admission.
    let valid_png = STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgaPjPAAADgwGAJwGhsAAAAABJRU5ErkJggg==")?;
    for bytes in [valid_png.as_slice(), b"not PNG", b""] {
        let refused = harness
            .call(
                "POST",
                &path,
                Some("image-policy"),
                Some(upload(0, "image/png", bytes)),
            )
            .await?;
        secure(&refused);
        assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(refused.json()?["error"]["code"], "asset_type_unsupported");
        let list = harness.call("GET", &path, None, None).await?;
        assert_eq!(list.json()?["assets"], json!([]));
        assert_eq!(list.json()?["asset_version"], 0);
    }
    let text = harness
        .call(
            "POST",
            &path,
            Some("image-policy"),
            Some(upload(0, "text/plain", b"permitted text")),
        )
        .await?;
    assert_eq!(
        text.status,
        StatusCode::CREATED,
        "image rejection must leave the key free"
    );
    let receipt = text.json()?;
    let id = receipt["asset"]["id"].as_str().ok_or("asset id")?;
    let content = harness
        .call("GET", &format!("{path}/{id}/content"), None, None)
        .await?;
    assert_eq!(content.headers[header::CONTENT_TYPE], "text/plain");
    assert_eq!(content.bytes, b"permitted text");
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

#[tokio::test]
async fn historical_png_receipts_replay_before_current_policy_without_recreating_bytes()
-> TestResult {
    use artifree_core::artifacts::ArtifactService;
    use artifree_core::assets::AssetUpload;
    use artifree_domain::assets::AssetVersion;
    use artifree_domain::identity::UnixMillis;
    let harness = Harness::new()?;
    let artifact = harness.artifact().await?;
    let id = artifact.parse()?;
    let actor = "local".parse()?;
    let key = "asset-test-historical-png".parse()?;
    let media = "image/png".parse()?;
    let bytes = STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGNgaPjPAAADgwGAJwGhsAAAAABJRU5ErkJggg==")?;
    let service = ArtifactService::new(SqliteStore::open(&harness.dir.join("db.sqlite3"))?);
    // Seed a receipt under an earlier trusted policy, not current HTTP admission.
    let original = service.upload_asset_with_validator(
        &actor,
        &key,
        &id,
        AssetUpload {
            base: AssetVersion(0),
            content_type: &media,
            bytes: &bytes,
            now: UnixMillis(1),
        },
        |_, _| Ok(()),
    )?;
    let path = format!("/v1/artifacts/{artifact}/assets");
    let request = upload(0, "image/png", &bytes);
    let replay = harness
        .call("POST", &path, Some("historical-png"), Some(request.clone()))
        .await?;
    assert_eq!(replay.status, StatusCode::CREATED);
    assert_eq!(replay.json()?["asset"]["content_type"], "image/png");
    assert_eq!(replay.json()?["asset"]["id"], original.id.as_str());
    assert_eq!(replay.json()?["replayed"], true);
    let asset_path = format!("{path}/{}", original.id.as_str());
    let metadata = harness.call("GET", &asset_path, None, None).await?;
    assert_eq!(metadata.status, StatusCode::OK);
    assert_eq!(metadata.json()?["asset"]["content_type"], "image/png");
    let stored = harness
        .call("GET", &format!("{asset_path}/content"), None, None)
        .await?;
    assert_eq!(stored.status, StatusCode::OK);
    assert_eq!(stored.headers[header::CONTENT_TYPE], "image/png");
    assert_eq!(stored.bytes, bytes);
    let fresh = harness
        .call(
            "POST",
            &path,
            Some("fresh-png"),
            Some(upload(1, "image/png", &bytes)),
        )
        .await?;
    assert_eq!(fresh.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(fresh.json()?["error"]["code"], "asset_type_unsupported");
    let deleted = harness
        .call(
            "DELETE",
            &asset_path,
            Some("delete-historical"),
            Some(json!({"base_asset_version":1})),
        )
        .await?;
    assert_eq!(deleted.json()?["deleted"], true);
    let deleted_replay = harness
        .call("POST", &path, Some("historical-png"), Some(request))
        .await?;
    assert_eq!(deleted_replay.status, StatusCode::CREATED);
    assert_eq!(deleted_replay.json()?["replayed"], true);
    assert_eq!(deleted_replay.json()?["asset_version"], 1);
    let content = harness
        .call("GET", &format!("{asset_path}/content"), None, None)
        .await?;
    assert_eq!(content.status, StatusCode::NOT_FOUND);
    let list = harness.call("GET", &path, None, None).await?;
    assert_eq!(list.json()?["assets"], json!([]));
    assert_eq!(list.json()?["asset_version"], 2);
    Ok(())
}

// --- duplicates that copy the asset space (decision 0045 rule 10) ----------

impl Harness {
    async fn named_artifact(&self, key: &str) -> Result<String, Box<dyn std::error::Error>> {
        let body = json!({"title": key, "files": [{"path": "index.html", "encoding": "utf8",
            "content": "<h1>fixture</h1>"}]});
        let created = self
            .call("POST", "/v1/artifacts", Some(key), Some(body))
            .await?;
        assert_eq!(created.status, StatusCode::CREATED, "{:?}", created.json());
        Ok(created.json()?["artifact"]["id"]
            .as_str()
            .ok_or("artifact")?
            .to_owned())
    }
    async fn duplicate(
        &self,
        artifact: &str,
        revision: u64,
        key: &str,
        body: Value,
    ) -> Result<Reply, Box<dyn std::error::Error>> {
        self.call(
            "POST",
            &format!("/v1/artifacts/{artifact}/revisions/{revision}/duplicate"),
            Some(key),
            Some(body),
        )
        .await
    }
    async fn assets_of(&self, artifact: &str) -> Result<Value, Box<dyn std::error::Error>> {
        let list = self
            .call(
                "GET",
                &format!("/v1/artifacts/{artifact}/assets"),
                None,
                None,
            )
            .await?;
        assert_eq!(list.status, StatusCode::OK, "{:?}", list.json());
        Ok(list.json()?)
    }
    async fn put_asset(
        &self,
        artifact: &str,
        key: &str,
        base: u64,
        bytes: &[u8],
    ) -> Result<Value, Box<dyn std::error::Error>> {
        let reply = self
            .call(
                "POST",
                &format!("/v1/artifacts/{artifact}/assets"),
                Some(key),
                Some(upload(base, "text/plain", bytes)),
            )
            .await?;
        assert_eq!(reply.status, StatusCode::CREATED, "{:?}", reply.json());
        Ok(reply.json()?["asset"].clone())
    }
}
fn copy_id(reply: &Reply) -> Result<String, Box<dyn std::error::Error>> {
    assert_eq!(reply.status, StatusCode::CREATED, "{:?}", reply.json());
    Ok(reply.json()?["artifact"]["id"]
        .as_str()
        .ok_or("artifact")?
        .to_owned())
}

#[tokio::test]
async fn a_duplicate_copies_current_assets_only_when_asked() -> TestResult {
    let h = Harness::new()?;
    let source = h.artifact().await?;
    let first = h.put_asset(&source, "first", 0, b"first bytes").await?;
    let saved = h
        .call(
            "POST",
            &format!("/v1/artifacts/{source}/revisions"),
            Some("save"),
            Some(
                json!({"base_revision": 1, "changes": [{"op": "put", "path": "index.html",
                "encoding": "utf8", "content": "<h1>second</h1>"}]}),
            ),
        )
        .await?;
    assert_eq!(saved.status, StatusCode::CREATED);
    let second = h
        .put_asset(&source, "second", 1, b"after revision 2")
        .await?;

    let plain = h.duplicate(&source, 1, "plain", json!({})).await?;
    assert_eq!(plain.json()?["copied"], json!(["files"]));
    let plain_assets = h.assets_of(&copy_id(&plain)?).await?;
    assert_eq!(
        (
            plain_assets["assets"].clone(),
            plain_assets["asset_version"].clone()
        ),
        (json!([]), json!(0))
    );

    // Revision 1 predates the second upload; the copy takes the current space.
    let copy = h
        .duplicate(&source, 1, "assets", json!({"copy_assets": true}))
        .await?;
    assert_eq!(copy.json()?["copied"], json!(["files", "assets"]));
    let copy = copy_id(&copy)?;
    let copied = h.assets_of(&copy).await?;
    assert_eq!(copied["assets"], h.assets_of(&source).await?["assets"]);
    assert_eq!(copied["asset_version"], 1);
    let mut ids = [first["id"].clone(), second["id"].clone()];
    ids.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    assert_eq!(
        copied["assets"]
            .as_array()
            .ok_or("assets")?
            .iter()
            .map(|asset| asset["id"].clone())
            .collect::<Vec<_>>(),
        ids
    );
    let id = first["id"].as_str().ok_or("id")?;
    let content = h
        .call(
            "GET",
            &format!("/v1/artifacts/{copy}/assets/{id}/content"),
            None,
            None,
        )
        .await?;
    assert_eq!(content.bytes, b"first bytes");

    let both = h
        .duplicate(
            &source,
            2,
            "both",
            json!({"copy_data": true, "copy_assets": true}),
        )
        .await?;
    assert_eq!(both.json()?["copied"], json!(["files", "data", "assets"]));

    // An empty space is still reported as copied, and the copy stays at 0.
    let empty = h.named_artifact("empty-source").await?;
    let empty_copy = h
        .duplicate(&empty, 1, "empty", json!({"copy_assets": true}))
        .await?;
    assert_eq!(empty_copy.json()?["copied"], json!(["files", "assets"]));
    assert_eq!(
        h.assets_of(&copy_id(&empty_copy)?).await?["asset_version"],
        0
    );
    Ok(())
}

#[tokio::test]
async fn copy_assets_is_part_of_the_duplicate_fingerprint() -> TestResult {
    let h = Harness::new()?;
    let source = h.artifact().await?;
    h.put_asset(&source, "upload", 0, b"bytes").await?;
    let plain = h.duplicate(&source, 1, "plain", json!({})).await?;
    let plain_id = copy_id(&plain)?;
    let explicit = h
        .duplicate(&source, 1, "plain", json!({"copy_assets": false}))
        .await?;
    assert_eq!(
        explicit.json()?["replayed"],
        true,
        "false is the old request"
    );
    assert_eq!(copy_id(&explicit)?, plain_id);
    for (key, body) in [
        ("plain", json!({"copy_assets": true})),
        ("plain", json!({"copy_data": true, "copy_assets": true})),
    ] {
        let reused = h.duplicate(&source, 1, key, body.clone()).await?;
        assert_eq!(
            (reused.status, reused.json()?["error"]["code"].clone()),
            (
                StatusCode::UNPROCESSABLE_ENTITY,
                json!("request_key_reused")
            ),
            "{body}"
        );
    }
    h.duplicate(&source, 1, "data", json!({"copy_data": true}))
        .await
        .and_then(|reply| copy_id(&reply))?;
    let reused = h
        .duplicate(
            &source,
            1,
            "data",
            json!({"copy_data": true, "copy_assets": true}),
        )
        .await?;
    assert_eq!(reused.json()?["error"]["code"], "request_key_reused");
    let malformed = h
        .duplicate(&source, 1, "malformed", json!({"copy_assets": "yes"}))
        .await?;
    assert_eq!(malformed.status, StatusCode::BAD_REQUEST);
    Ok(())
}

#[tokio::test]
async fn an_asset_copy_over_the_actor_limit_is_refused_and_copies_nothing() -> TestResult {
    let h = Harness::unthrottled()?;
    let source = h.artifact().await?;
    for n in 0..64 {
        h.put_asset(&source, &format!("source-{n}"), n, b"a")
            .await?;
    }
    // Two full spaces in the trash still count: 192 associations, so the
    // 64 copied ones fit exactly, and one more association does not.
    for space in 0..2 {
        let full = h.named_artifact(&format!("full-{space}")).await?;
        for n in 0..64 {
            h.put_asset(&full, &format!("full-{space}-{n}"), n, b"a")
                .await?;
        }
        let trashed = h
            .call(
                "PATCH",
                &format!("/v1/artifacts/{full}"),
                Some(&format!("trash-{space}")),
                Some(json!({"base_metadata_revision": 1, "deleted": true})),
            )
            .await?;
        assert_eq!(trashed.status, StatusCode::OK, "{:?}", trashed.json());
    }
    let extra = h.named_artifact("extra").await?;
    let one = h.put_asset(&extra, "extra-asset", 0, b"a").await?;
    let listed = h.call("GET", "/v1/artifacts", None, None).await?.json()?;

    let refused = h
        .duplicate(&source, 1, "copy", json!({"copy_assets": true}))
        .await?;
    assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(refused.json()?["error"]["code"], "asset_limit_exceeded");
    assert_eq!(
        h.call("GET", "/v1/artifacts", None, None).await?.json()?,
        listed,
        "no copy was created"
    );

    let one = one["id"].as_str().ok_or("id")?;
    let freed = h
        .call(
            "DELETE",
            &format!("/v1/artifacts/{extra}/assets/{one}"),
            Some("free"),
            Some(json!({"base_asset_version": 1})),
        )
        .await?;
    assert_eq!(freed.json()?["deleted"], true);
    let copied = h
        .duplicate(&source, 1, "copy", json!({"copy_assets": true}))
        .await?;
    assert_eq!(copied.json()?["replayed"], false, "the refusal kept no key");
    let copy = copy_id(&copied)?;
    assert_eq!(
        h.assets_of(&copy).await?["assets"].as_array().map(Vec::len),
        Some(64)
    );
    Ok(())
}

#[tokio::test]
async fn an_asset_copy_replays_after_its_source_changes_or_is_purged() -> TestResult {
    let h = Harness::new()?;
    let source = h.artifact().await?;
    h.put_asset(&source, "before", 0, b"before copy").await?;
    let first = h
        .duplicate(&source, 1, "copy", json!({"copy_assets": true}))
        .await?;
    let copy = copy_id(&first)?;
    let copied = h.assets_of(&copy).await?;

    h.put_asset(&source, "after", 1, b"after copy").await?;
    let replay = h
        .duplicate(&source, 1, "copy", json!({"copy_assets": true}))
        .await?;
    assert_eq!(
        (replay.json()?["replayed"].clone(), copy_id(&replay)?),
        (json!(true), copy.clone())
    );
    assert_eq!(replay.json()?["copied"], json!(["files", "assets"]));
    assert_eq!(
        h.assets_of(&copy).await?,
        copied,
        "the replay copies nothing"
    );

    let trashed = h
        .call(
            "PATCH",
            &format!("/v1/artifacts/{source}"),
            Some("trash"),
            Some(json!({"base_metadata_revision": 1, "deleted": true})),
        )
        .await?;
    assert_eq!(trashed.status, StatusCode::OK, "{:?}", trashed.json());
    let purged = h
        .call(
            "POST",
            &format!("/v1/artifacts/{source}/purge"),
            Some("purge"),
            Some(json!({"base_metadata_revision": 2})),
        )
        .await?;
    assert_eq!(purged.status, StatusCode::OK, "{:?}", purged.json());
    let after_purge = h
        .duplicate(&source, 1, "copy", json!({"copy_assets": true}))
        .await?;
    assert_eq!(
        (
            after_purge.json()?["replayed"].clone(),
            copy_id(&after_purge)?
        ),
        (json!(true), copy.clone())
    );
    assert_eq!(h.assets_of(&copy).await?, copied);
    Ok(())
}
