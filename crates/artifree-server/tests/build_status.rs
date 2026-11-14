//! The core's build reads against a real `SQLite` store (decision 0058 rules 13
//! to 16, slice R03cc): the head status follows the newest job of the head by
//! enqueue order, the latest build stays across a newer revision, a build
//! preview is refused for a revision without a build, and a caller who may not
//! read the revision is given no status, no diagnostics and no build file.
//! Time is a number each call is given, so nothing sleeps.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use artifree_core::artifacts::{ArtifactError, ArtifactService, MetadataChange, Missing};
use artifree_core::build_reads::{BuildStatus, HeadBuild};
use artifree_domain::identity::{ActorId, ArtifactId, RequestKey, Title, UnixMillis};
use artifree_domain::job::{
    AttemptIdentity, BuildDiagnostic, JobFailure, JobId, ProfileName, SourceLocation, WorkerId,
};
use artifree_domain::package::{FileChange, FilePackage, PackagePath};
use artifree_domain::{MetadataRevision, SourceRevision};
use artifree_ports::build_store::{BuildPublication, BuildStore, Publication};
use artifree_ports::job_store::{ClaimedJob, ExecutionClaim, JobClaim, JobStore, ProfileBinding};
use artifree_providers::storage::{DATABASE_FILE, SqliteStore};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const LEASE_MS: u64 = 30_000;
const TOOL_DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const DIRECTORY: &str = "0123456789abcdef0123456789abcdef";

/// A directory for one test's database, removed when the test ends.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> std::io::Result<Self> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "artifree-build-status-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        // A directory left by an earlier run with the same name is cleared first.
        if path.exists() {
            std::fs::remove_dir_all(&path)?;
        }
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Drop cannot report a failure; a directory left behind is cleared by the
        // next run that uses the same name.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn parsed<T>(text: &str) -> Result<T, Box<dyn std::error::Error>>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    text.parse::<T>()
        .map_err(|error| format!("invalid fixture {text:?}: {error}").into())
}

fn at(ms: i64) -> UnixMillis {
    UnixMillis(ms)
}

fn attempt(seed: u8) -> AttemptIdentity {
    AttemptIdentity::from_random([seed; 16])
}

/// A store with one artifact at revision 1, owned by `owner`. The service and
/// the worker's side use two connections to the same database file.
struct Fixture {
    store: SqliteStore,
    service: ArtifactService<SqliteStore>,
    owner: ActorId,
    artifact: ArtifactId,
    profile: ProfileName,
    dir: TempDir,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let path = dir.0.join(DATABASE_FILE);
        let store = SqliteStore::open(&path)?;
        let service = ArtifactService::new(SqliteStore::open(&path)?);
        let owner: ActorId = parsed("owner")?;
        let package = FilePackage::new(vec![(
            PackagePath::entry(),
            b"<script type=\"module\" src=\"./app.js\"></script>".to_vec(),
        )])?;
        let created = service.create(
            &owner,
            &parsed::<RequestKey>("create-key")?,
            &parsed::<Title>("Builds")?,
            &package,
        )?;
        Ok(Self {
            store,
            service,
            owner,
            artifact: created.artifact.id,
            profile: parsed("react-v1")?,
            dir,
        })
    }

    fn status(&self) -> Result<BuildStatus, ArtifactError> {
        self.service
            .build_status(&self.owner, &self.artifact, &self.profile)
    }

    /// Enqueues a job of the profile for `revision` through the core.
    fn enqueue(
        &self,
        revision: u64,
        name: &str,
        now: i64,
    ) -> Result<JobId, Box<dyn std::error::Error>> {
        Ok(self
            .service
            .enqueue_profile_job(
                &self.owner,
                &parsed(&format!("enqueue-{name}"))?,
                &self.artifact,
                SourceRevision(revision),
                &self.profile,
                at(now),
            )?
            .job
            .id)
    }

    /// Claims the next due job as attempt `seed` and settles the attempt.
    fn claim(&self, seed: u8, now: i64) -> Result<ClaimedJob, Box<dyn std::error::Error>> {
        let worker: WorkerId = parsed("worker-a")?;
        let known = [self.profile.clone()];
        let claimed = self
            .store
            .claim_job(JobClaim {
                worker: &worker,
                now: at(now),
                lease_ms: LEASE_MS,
                known_profiles: &known,
                execution: Some(ExecutionClaim {
                    attempt: attempt(seed),
                    tool_digest: TOOL_DIGEST,
                    machine: "machine-a",
                    boot: "boot-a",
                    cgroupns: "cgroupns-a",
                    directory: DIRECTORY,
                }),
            })?
            .claimed
            .ok_or("no job was due")?;
        self.store.settle_attempt(&attempt(seed), at(now))?;
        Ok(claimed)
    }

    fn fail(
        &self,
        claimed: &ClaimedJob,
        seed: u8,
        diagnostics: &[BuildDiagnostic],
        now: i64,
    ) -> TestResult {
        self.store.fail_build(
            &claimed.lease,
            &attempt(seed),
            JobFailure::BuildFailed,
            diagnostics,
            at(now),
        )?;
        Ok(())
    }

    fn publish(&self, claimed: &ClaimedJob, seed: u8, now: i64) -> TestResult {
        let files = FilePackage::new(vec![
            (
                PackagePath::entry(),
                b"<script type=\"module\" src=\"./app.js\"></script>".to_vec(),
            ),
            (parsed("app.js")?, b"console.log(1);".to_vec()),
        ])?;
        let (_, publication) = self.store.publish_build(BuildPublication {
            lease: &claimed.lease,
            attempt: &attempt(seed),
            binding: ProfileBinding {
                profile: &self.profile,
                tool_digest: TOOL_DIGEST,
            },
            files: &files,
            now: at(now),
        })?;
        if !matches!(publication, Publication::Published(_)) {
            return Err(format!("not published: {publication:?}").into());
        }
        Ok(())
    }

    /// Commits revision `base + 1`.
    fn revise(&self, base: u64) -> TestResult {
        self.service.save(
            &self.owner,
            &parsed(&format!("revise-{base}"))?,
            &self.artifact,
            SourceRevision(base),
            vec![FileChange::Put {
                path: parsed("notes.txt")?,
                content: format!("revision {}", base + 1).into_bytes(),
            }],
        )?;
        Ok(())
    }

    /// Moves the artifact to the trash (`true`) or out of it, from metadata
    /// revision `base`.
    fn trash(&self, base: u64, deleted: bool) -> TestResult {
        let change = MetadataChange::new(None, None, Some(deleted)).ok_or("an empty change")?;
        self.service.update(
            &self.owner,
            &parsed(&format!("trash-key-{base}"))?,
            &self.artifact,
            MetadataRevision(base),
            &change,
        )?;
        Ok(())
    }
}

fn diagnostic() -> Result<BuildDiagnostic, Box<dyn std::error::Error>> {
    Ok(BuildDiagnostic {
        location: Some(SourceLocation {
            path: parsed("src/main.tsx")?,
            line: 1,
            column: 16,
        }),
        message: parsed("Could not resolve \"left-pad\"")?,
    })
}

/// Rules 15 and 16: the head status is the state of the newest job for the
/// head by enqueue order until the head has a build, and the latest build
/// stays when a newer revision has none.
#[test]
fn the_head_status_follows_the_newest_job_of_the_head() -> TestResult {
    let fixture = Fixture::new()?;
    let status = fixture.status()?;
    assert_eq!(
        (status.head, status.latest, status.head_build),
        (SourceRevision(1), None, HeadBuild::None),
        "no job for the head"
    );

    let first = fixture.enqueue(1, "first", 100)?;
    assert_eq!(
        fixture.status()?.head_build,
        HeadBuild::Pending { job: first.clone() },
        "a queued job is pending"
    );
    let claimed = fixture.claim(1, 110)?;
    assert_eq!(
        fixture.status()?.head_build,
        HeadBuild::Pending { job: first.clone() },
        "a running job is pending"
    );
    let kept = [diagnostic()?];
    fixture.fail(&claimed, 1, &kept, 120)?;
    assert_eq!(
        fixture.status()?.head_build,
        HeadBuild::Failed {
            job: first,
            failure: JobFailure::BuildFailed,
            diagnostics: kept.to_vec(),
        },
        "a failed job gives its failure and diagnostics"
    );

    let second = fixture.enqueue(1, "second", 200)?;
    assert_eq!(
        fixture.status()?.head_build,
        HeadBuild::Pending { job: second },
        "an older failed job does not set the status"
    );
    let claimed = fixture.claim(2, 210)?;
    fixture.publish(&claimed, 2, 220)?;
    let status = fixture.status()?;
    assert_eq!(status.head_build, HeadBuild::Built);
    let latest = status.latest.ok_or("the head has a build")?;
    assert_eq!(latest.revision, SourceRevision(1));

    fixture.revise(1)?;
    let status = fixture.status()?;
    assert_eq!(
        (status.head, status.latest, status.head_build),
        (SourceRevision(2), Some(latest), HeadBuild::None),
        "the last good build stays until a newer one succeeds"
    );
    Ok(())
}

/// Rule 16: a cancelled newest job leaves the head without a status. A
/// `run_profile` job is cancelled when the trash takes its artifact while its
/// attempt is settled.
#[test]
fn a_cancelled_newest_job_is_no_status() -> TestResult {
    let fixture = Fixture::new()?;
    let job = fixture.enqueue(1, "cancelled", 100)?;
    fixture.claim(1, 110)?;
    fixture.trash(1, true)?;
    fixture.trash(2, false)?;
    let state: String = rusqlite::Connection::open(fixture.dir.0.join(DATABASE_FILE))?.query_row(
        "SELECT state FROM jobs WHERE id = ?1",
        [job.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(state, "cancelled");
    assert_eq!(fixture.status()?.head_build, HeadBuild::None);
    Ok(())
}

/// Rule 13's read rule and rule 14: a caller who may not read the revision is
/// given no head status, so no diagnostics, and no build file or build
/// preview; the trash answers the same for the owner, and a recovery serves
/// again.
#[test]
fn a_caller_who_may_not_read_the_revision_is_refused() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.enqueue(1, "built", 100)?;
    let claimed = fixture.claim(1, 110)?;
    fixture.publish(&claimed, 1, 120)?;
    fixture.revise(1)?;
    fixture.enqueue(2, "failed", 200)?;
    let claimed = fixture.claim(2, 210)?;
    fixture.fail(&claimed, 2, &[diagnostic()?], 220)?;
    let app: PackagePath = parsed("app.js")?;
    let revision = SourceRevision(1);

    let other: ActorId = parsed("other")?;
    let missing = ArtifactError::NotFound(Missing::Artifact);
    assert_eq!(
        fixture
            .service
            .build_status(&other, &fixture.artifact, &fixture.profile),
        Err(missing.clone())
    );
    assert_eq!(
        fixture
            .service
            .preview_build(&other, &fixture.artifact, revision, &fixture.profile)
            .map(drop),
        Err(missing.clone())
    );
    assert_eq!(
        fixture.service.read_build_file(
            &other,
            &fixture.artifact,
            revision,
            &fixture.profile,
            &app
        ),
        Err(ArtifactError::NotFound(Missing::Artifact))
    );

    fixture.trash(1, true)?;
    assert_eq!(fixture.status(), Err(missing), "the trash gives no status");
    assert_eq!(
        fixture.service.read_build_file(
            &fixture.owner,
            &fixture.artifact,
            revision,
            &fixture.profile,
            &app
        ),
        Err(ArtifactError::NotFound(Missing::Artifact)),
        "the trash serves no build file"
    );

    fixture.trash(2, false)?;
    assert!(matches!(
        fixture.status()?.head_build,
        HeadBuild::Failed { ref diagnostics, .. } if diagnostics.len() == 1
    ));
    assert_eq!(
        fixture.service.read_build_file(
            &fixture.owner,
            &fixture.artifact,
            revision,
            &fixture.profile,
            &app
        )?,
        b"console.log(1);".to_vec()
    );
    Ok(())
}

/// Rule 14: a build preview is refused at issue for a revision without a
/// build and for a revision the artifact does not have; a build file the
/// build does not hold is a missing file.
#[test]
fn a_build_preview_needs_a_build_of_the_revision() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.enqueue(1, "built", 100)?;
    let claimed = fixture.claim(1, 110)?;
    fixture.publish(&claimed, 1, 120)?;
    fixture.revise(1)?;
    let preview = |revision: u64, profile: &ProfileName| {
        fixture
            .service
            .preview_build(
                &fixture.owner,
                &fixture.artifact,
                SourceRevision(revision),
                profile,
            )
            .map(|build| build.revision)
    };

    assert_eq!(preview(1, &fixture.profile), Ok(SourceRevision(1)));
    assert_eq!(
        preview(2, &fixture.profile),
        Err(ArtifactError::NotFound(Missing::Build)),
        "revision 2 has no build"
    );
    assert_eq!(
        preview(1, &parsed("react-v1-check")?),
        Err(ArtifactError::NotFound(Missing::Build)),
        "another profile has no build of revision 1"
    );
    for revision in [0, 3] {
        assert_eq!(
            preview(revision, &fixture.profile),
            Err(ArtifactError::NotFound(Missing::Revision)),
            "revision {revision}"
        );
    }
    assert_eq!(
        fixture.service.read_build_file(
            &fixture.owner,
            &fixture.artifact,
            SourceRevision(1),
            &fixture.profile,
            &parsed("app.css")?
        ),
        Err(ArtifactError::NotFound(Missing::File))
    );
    for revision in [0, 3] {
        assert_eq!(
            fixture.service.read_build_file(
                &fixture.owner,
                &fixture.artifact,
                SourceRevision(revision),
                &fixture.profile,
                &parsed("app.js")?
            ),
            Err(ArtifactError::NotFound(Missing::Revision)),
            "a build file of revision {revision}"
        );
    }
    Ok(())
}
