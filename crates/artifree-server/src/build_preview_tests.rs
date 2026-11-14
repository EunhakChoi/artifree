//! Build previews on the content origin (decision 0058 rule 14, slice R03cc):
//! the management preview operation with a build target, the `/b/{ticket}/`
//! route under the content security headers, the refusal at issue, and the
//! revocations a build ticket shares with a source ticket.

use std::error::Error;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use artifree_contracts::api::{ContentEncoding, CreateArtifactRequest, FileUpload};
use artifree_core::artifacts::MetadataChange;
use artifree_domain::identity::{
    ActorId, ArtifactId, RequestKey, StateGeneration, TokenId, UnixMillis,
};
use artifree_domain::job::{AttemptIdentity, ProfileName, WorkerId};
use artifree_domain::package::{FilePackage, PackagePath};
use artifree_domain::{MetadataRevision, SourceRevision};
use artifree_ports::build_store::{BuildPublication, BuildStore};
use artifree_ports::job_store::{ExecutionClaim, JobClaim, JobStore, ProfileBinding};
use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::SqliteTokenStore;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode, header};
use tower::ServiceExt;

use crate::auth::AccessToken;
use crate::http::{AppState, Settings};
use crate::limits::RequestLimits;
use crate::preview::PreviewTarget;
use crate::{changes, operations, preview};

const ORIGIN: &str = "http://127.0.0.1:7411";
const DATABASE: &str = "builds.sqlite3";
const TOOL_DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const INDEX: &[u8] = b"<!doctype html><script type=\"module\" src=\"./app.js\"></script>";
const SOURCE: &[u8] = b"<!doctype html><p>source</p>";
const APP: &[u8] = b"console.log(\"built\");";

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Result<Self, Box<dyn Error>> {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = std::env::temp_dir().join(format!(
            "artifree-build-preview-{}-{nonce}",
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

fn profile() -> Result<ProfileName, Box<dyn Error>> {
    Ok("react-v1".parse()?)
}

/// A server state with one artifact of the owner at revision 1, built or not.
struct World {
    state: AppState,
    store: SqliteStore,
    owner: ActorId,
    artifact: String,
    _directory: TestDirectory,
}

impl World {
    async fn new() -> Result<Self, Box<dyn Error>> {
        let directory = TestDirectory::new()?;
        let path = directory.0.join(DATABASE);
        let state = AppState::new(
            SqliteStore::open(&path)?,
            SqliteTokenStore::open(&path)?,
            AccessToken::parse(&"0".repeat(64)).ok_or("invalid fixture token")?,
            Settings {
                content_origin: ORIGIN.into(),
                preview_ttl: Duration::from_secs(60),
                frame_ancestors: "http://127.0.0.1:*".into(),
                limits: RequestLimits::default(),
                change_streams: changes::DEFAULT_STREAMS_PER_ACTOR,
                ..Settings::default()
            },
        )?;
        let owner = ActorId::owner();
        let created = operations::create(
            &state,
            owner.clone(),
            "create-key".parse()?,
            CreateArtifactRequest {
                title: "Build".into(),
                files: vec![FileUpload {
                    path: "index.html".into(),
                    encoding: ContentEncoding::Utf8,
                    content: String::from_utf8(SOURCE.to_vec())?,
                }],
            },
        )
        .await
        .map_err(|error| error.body.message)?;
        Ok(Self {
            state,
            store: SqliteStore::open(&path)?,
            owner,
            artifact: created.artifact.id,
            _directory: directory,
        })
    }

    async fn built() -> Result<Self, Box<dyn Error>> {
        let world = Self::new().await?;
        world.publish().await?;
        Ok(world)
    }

    fn artifact_id(&self) -> Result<ArtifactId, Box<dyn Error>> {
        Ok(self.artifact.parse()?)
    }

    /// Runs a `react-v1` job of revision 1 to a published build of `INDEX`
    /// and `app.js`, as the worker's store calls do.
    async fn publish(&self) -> Result<(), Box<dyn Error>> {
        let owner = self.owner.clone();
        let artifact = self.artifact_id()?;
        let key: RequestKey = "build-key".parse()?;
        let name = profile()?;
        self.state
            .run(move |artifacts| {
                artifacts.enqueue_profile_job(
                    &owner,
                    &key,
                    &artifact,
                    SourceRevision(1),
                    &name,
                    UnixMillis(100),
                )
            })
            .await
            .map_err(|error| error.to_string())?;
        let attempt = AttemptIdentity::from_random([7; 16]);
        let name = profile()?;
        let known = [name.clone()];
        let worker: WorkerId = "worker-a".parse()?;
        let claimed = self
            .store
            .claim_job(JobClaim {
                worker: &worker,
                now: UnixMillis(110),
                lease_ms: 30_000,
                known_profiles: &known,
                execution: Some(ExecutionClaim {
                    attempt,
                    tool_digest: TOOL_DIGEST,
                    machine: "machine-a",
                    boot: "boot-a",
                    cgroupns: "cgroupns-a",
                    directory: "0123456789abcdef0123456789abcdef",
                }),
            })?
            .claimed
            .ok_or("no job was due")?;
        self.store.settle_attempt(&attempt, UnixMillis(110))?;
        let files = FilePackage::new(vec![
            (PackagePath::entry(), INDEX.to_vec()),
            ("app.js".parse()?, APP.to_vec()),
        ])?;
        self.store.publish_build(BuildPublication {
            lease: &claimed.lease,
            attempt: &attempt,
            binding: ProfileBinding {
                profile: &name,
                tool_digest: TOOL_DIGEST,
            },
            files: &files,
            now: UnixMillis(120),
        })?;
        Ok(())
    }

    /// Issues a preview of revision 1 for `target` and returns the path of
    /// its URL on the content origin.
    async fn issue(
        &self,
        token: Option<TokenId>,
        target: PreviewTarget,
    ) -> Result<String, Box<dyn Error>> {
        let preview = operations::preview(
            &self.state,
            self.owner.clone(),
            token,
            StateGeneration::FIRST,
            &self.artifact,
            1,
            target,
        )
        .await
        .map_err(|error| error.body.message)?;
        assert_eq!(preview.revision, 1);
        Ok(preview
            .url
            .strip_prefix(ORIGIN)
            .ok_or("the preview uses another origin")?
            .to_owned())
    }

    async fn build_ticket(&self, token: Option<TokenId>) -> Result<String, Box<dyn Error>> {
        self.issue(token, PreviewTarget::Build(profile()?)).await
    }

    async fn get(&self, path: &str) -> Result<(StatusCode, HeaderMap, Vec<u8>), Box<dyn Error>> {
        let response = preview::router(self.state.clone())
            .oneshot(Request::get(path).body(Body::empty())?)
            .await?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = to_bytes(response.into_body(), usize::MAX).await?.to_vec();
        Ok((status, headers, body))
    }

    async fn status(&self, path: &str) -> Result<StatusCode, Box<dyn Error>> {
        Ok(self.get(path).await?.0)
    }

    /// Moves the artifact to the trash (`true`) or out of it, from metadata
    /// revision `base`.
    async fn trash(&self, base: u64, deleted: bool) -> Result<(), Box<dyn Error>> {
        let owner = self.owner.clone();
        let artifact = self.artifact_id()?;
        let change = MetadataChange::new(None, None, Some(deleted)).ok_or("an empty change")?;
        let key: RequestKey = format!("trash-key-{base}").parse()?;
        self.state
            .run(move |artifacts| {
                artifacts.update(&owner, &key, &artifact, MetadataRevision(base), &change)
            })
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

fn header_text<'a>(headers: &'a HeaderMap, name: &header::HeaderName) -> &'a str {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
}

/// A build ticket serves its build's files under `/b/` with the content
/// security headers of a source preview, and nothing else.
#[tokio::test]
async fn a_build_ticket_serves_its_build_under_the_sandbox() -> Result<(), Box<dyn Error>> {
    let world = World::built().await?;
    let entry = world.build_ticket(None).await?;
    assert!(entry.starts_with("/b/") && entry.ends_with('/'), "{entry}");

    let (status, headers, body) = world.get(&entry).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, INDEX, "the build's entry, not the revision's");
    assert_eq!(
        header_text(&headers, &header::CONTENT_TYPE),
        "text/html; charset=utf-8"
    );
    let policy = header_text(&headers, &header::CONTENT_SECURITY_POLICY);
    assert!(
        policy.starts_with(&format!("sandbox {}; ", preview::SANDBOX)),
        "{policy}"
    );
    assert!(!policy.contains("allow-same-origin"), "{policy}");
    assert_eq!(
        header_text(&headers, &header::X_CONTENT_TYPE_OPTIONS),
        "nosniff"
    );
    assert_eq!(header_text(&headers, &header::CACHE_CONTROL), "no-store");

    let (status, headers, body) = world.get(&format!("{entry}app.js")).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, APP);
    assert_eq!(
        header_text(&headers, &header::CONTENT_TYPE),
        "text/javascript; charset=utf-8"
    );
    assert!(
        header_text(&headers, &header::CONTENT_SECURITY_POLICY).starts_with("sandbox "),
        "a module script is served under the same policy"
    );

    assert_eq!(
        world.status(&format!("{entry}app.css")).await?,
        StatusCode::NOT_FOUND,
        "a file the build does not hold"
    );
    let (status, headers, _) = world.get(entry.trim_end_matches('/')).await?;
    assert_eq!(status, StatusCode::PERMANENT_REDIRECT);
    assert_eq!(header_text(&headers, &header::LOCATION), entry);

    let secret = entry.trim_start_matches("/b/");
    for path in [format!("/p/{secret}"), format!("/r/{secret}app.js")] {
        assert_eq!(
            world.status(&path).await?,
            StatusCode::NOT_FOUND,
            "a build ticket serves no source: {path}"
        );
    }
    let source = world.issue(None, PreviewTarget::Source).await?;
    assert_eq!(world.get(&source).await?.2, SOURCE);
    let source_secret = source.trim_start_matches("/p/");
    assert_eq!(
        world.status(&format!("/b/{source_secret}")).await?,
        StatusCode::NOT_FOUND,
        "a source ticket serves no build"
    );
    Ok(())
}

/// A revision without a build is refused at issue, before a ticket exists.
#[tokio::test]
async fn a_build_preview_is_refused_without_a_build() -> Result<(), Box<dyn Error>> {
    let world = World::new().await?;
    let refused = operations::preview(
        &world.state,
        world.owner.clone(),
        None,
        StateGeneration::FIRST,
        &world.artifact,
        1,
        PreviewTarget::Build(profile()?),
    )
    .await;
    let Err(error) = refused else {
        return Err("a revision without a build was given a build preview".into());
    };
    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert_eq!(world.state.previews().live_tickets(&world.owner), 0);
    Ok(())
}

/// Token revocation, actor revocation and the trash end a build ticket as
/// they end a source ticket; a recovery serves it again, as it does a source
/// ticket.
#[tokio::test]
async fn a_build_ticket_is_revoked_like_a_source_ticket() -> Result<(), Box<dyn Error>> {
    let world = World::built().await?;
    let token = TokenId::from_random([3; 16]);
    let by_token = world.build_ticket(Some(token.clone())).await?;
    let kept = world.build_ticket(None).await?;
    world.state.previews().revoke_token(&token);
    assert_eq!(world.status(&by_token).await?, StatusCode::NOT_FOUND);
    assert_eq!(world.status(&kept).await?, StatusCode::OK);

    world.trash(1, true).await?;
    assert_eq!(world.status(&kept).await?, StatusCode::NOT_FOUND);
    world.trash(2, false).await?;
    assert_eq!(world.status(&kept).await?, StatusCode::OK);

    world.state.revoke_actor(&world.owner);
    assert_eq!(world.status(&kept).await?, StatusCode::NOT_FOUND);
    assert_eq!(world.state.previews().live_tickets(&world.owner), 0);
    Ok(())
}
