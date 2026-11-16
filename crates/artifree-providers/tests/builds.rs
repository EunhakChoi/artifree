//! The builds of `run_profile` jobs and the diagnostics of failed ones in the
//! `SQLite` store (decision 0058 rules 10 to 13 and 15, slice R03ca). Time is a
//! number each call is given, so every test moves it by hand.

mod common;

use artifree_domain::access::Space;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use artifree_domain::MetadataRevision;
use artifree_domain::SourceRevision;
use artifree_domain::assets::{AssetMediaType, AssetVersion};
use artifree_domain::identity::{ActorId, UnixMillis};
use artifree_domain::job::{
    AttemptIdentity, BuildDiagnostic, JobFailure, JobState, ProfileName, Revocation,
    SourceLocation, StopReason, WorkerId,
};
use artifree_domain::package::{FilePackage, MAX_FILE_BYTES, PackagePath};
use artifree_ports::artifact_store::{
    ArtifactStore, CleanupRequest, CommitReceipt, PurgeRequest, RequestFingerprint, RequestIdentity,
};
use artifree_ports::asset_store::{AssetStore, AssetWrite};
use artifree_ports::build_store::{
    BuildPublication, BuildRecord, BuildSnapshot, BuildStore, MAX_BUILDS_PER_ARTIFACT, Publication,
};
use artifree_ports::job_store::{
    ClaimedJob, ExecutionClaim, JobClaim, JobRecord, JobStore, JobStoreError, NewProfileJob,
    ProfileBinding, Retry,
};
use artifree_providers::backup::{backup, restore};
use artifree_providers::storage::SqliteStore;
use common::{
    TempDir, TestResult, actor, create_demo, database_in, digest, key, package, parsed, revise,
    update_metadata,
};
use rusqlite::Connection;

/// The lease of one claim, in milliseconds from the claim: the default of
/// `artifree-server worker run`.
const LEASE_MS: u64 = 30_000;
/// A tool root manifest digest: 64 lowercase hexadecimal digits.
const TOOL_DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
/// An execution directory identity: 32 lowercase hexadecimal digits.
const DIRECTORY: &str = "0123456789abcdef0123456789abcdef";

fn at(ms: i64) -> UnixMillis {
    UnixMillis(ms)
}

fn profile() -> ProfileName {
    parsed("react-v1")
}

fn attempt_id(seed: u8) -> AttemptIdentity {
    AttemptIdentity::from_random([seed; 16])
}

fn output(body: &str) -> FilePackage {
    package(&[
        ("index.html", "<script src=app.js></script>"),
        ("app.js", body),
    ])
}

fn diagnostic(path: Option<&str>, message: &str) -> BuildDiagnostic {
    BuildDiagnostic {
        location: path.map(|path| SourceLocation {
            path: parsed(path),
            line: 3,
            column: 9,
        }),
        message: parsed(message),
    }
}

/// A database with one artifact and the jobs and builds a test makes of it.
struct Fixture {
    root: TempDir,
    store: SqliteStore,
    owner: ActorId,
    created: CommitReceipt,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let root = TempDir::new();
        fs::create_dir(root.join("data"))?;
        let store = SqliteStore::open(&database_in(&root.join("data")))?;
        let owner = actor("owner");
        let created = create_demo(&store, &owner, &key("create"), "Builds", "<h1>1</h1>")?;
        Ok(Self {
            root,
            store,
            owner,
            created,
        })
    }

    fn data(&self) -> PathBuf {
        self.root.join("data")
    }

    fn count(&self, query: &str) -> Result<i64, rusqlite::Error> {
        Connection::open(database_in(&self.data()))?.query_row(query, [], |row| row.get(0))
    }

    /// Commits revisions until the head is `head`.
    fn revise_to(&self, head: u64) -> TestResult {
        let current = self
            .store
            .artifact(&self.created.artifact.id)?
            .ok_or("the artifact is missing")?
            .current_revision
            .0;
        for base in current..head {
            revise(
                &self.store,
                &self.created,
                &key(&format!("revise-{base}")),
                base,
                &format!("<h1>{}</h1>", base + 1),
            )?;
        }
        Ok(())
    }

    /// Enqueues a `run_profile` job for `revision`, claims it as attempt
    /// `seed` and settles the attempt, all at `now`.
    fn settled(
        &self,
        revision: u64,
        seed: u8,
        now: i64,
    ) -> Result<ClaimedJob, Box<dyn std::error::Error>> {
        let name = format!("job-{seed}");
        let request_key = key(&name);
        self.store.enqueue_profile_job(NewProfileJob {
            request: RequestIdentity {
                actor: &self.owner,
                key: &request_key,
                fingerprint: RequestFingerprint(digest(&name)),
            },
            artifact: &self.created.artifact.id,
            revision: SourceRevision(revision),
            profile: &profile(),
            now: at(now),
        })?;
        let claimant: WorkerId = parsed("worker-a");
        let known = [profile()];
        let claimed = self
            .store
            .claim_job(JobClaim {
                worker: &claimant,
                now: at(now),
                lease_ms: LEASE_MS,
                known_profiles: &known,
                execution: Some(ExecutionClaim {
                    attempt: attempt_id(seed),
                    tool_digest: TOOL_DIGEST,
                    machine: "machine-a",
                    boot: "boot-a",
                    cgroupns: "cgroupns-a",
                    directory: DIRECTORY,
                }),
            })?
            .claimed
            .ok_or("no job was due")?;
        self.store.settle_attempt(&attempt_id(seed), at(now))?;
        Ok(claimed)
    }

    fn publish(
        &self,
        claimed: &ClaimedJob,
        seed: u8,
        files: &FilePackage,
        now: i64,
    ) -> Result<(JobRecord, Publication), JobStoreError> {
        self.store.publish_build(BuildPublication {
            lease: &claimed.lease,
            attempt: &attempt_id(seed),
            binding: ProfileBinding {
                profile: &profile(),
                tool_digest: TOOL_DIGEST,
            },
            files,
            now: at(now),
        })
    }

    /// Builds `revision` as attempt `seed` at `now` and returns the new build.
    fn built(
        &self,
        revision: u64,
        seed: u8,
        now: i64,
    ) -> Result<BuildRecord, Box<dyn std::error::Error>> {
        let claimed = self.settled(revision, seed, now)?;
        match self.publish(&claimed, seed, &output(&format!("r{revision}")), now)? {
            (_, Publication::Published(build)) => Ok(build),
            (_, other) => Err(format!("revision {revision} was not published: {other:?}").into()),
        }
    }

    fn build(&self, revision: u64) -> Result<Option<BuildRecord>, JobStoreError> {
        self.store.build(
            &self.created.artifact.id,
            SourceRevision(revision),
            &profile(),
        )
    }

    fn latest(&self) -> Result<Option<BuildRecord>, JobStoreError> {
        self.store
            .latest_build(&self.created.artifact.id, &profile())
    }

    fn files(&self, revision: u64) -> Result<Option<FilePackage>, JobStoreError> {
        self.store.build_files(
            &self.created.artifact.id,
            SourceRevision(revision),
            &profile(),
        )
    }

    fn built_revisions(&self) -> Result<Vec<i64>, rusqlite::Error> {
        let connection = Connection::open(database_in(&self.data()))?;
        let mut statement = connection.prepare("SELECT revision FROM builds ORDER BY revision")?;
        let revisions = statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(revisions)
    }

    /// Moves the artifact to the trash (`true`) or out of it, from metadata
    /// revision `base`.
    fn trash(&self, base: u64, deleted: bool) -> TestResult {
        update_metadata(
            &self.store,
            &self.created,
            &key(&format!("trash-{base}")),
            base,
            None,
            None,
            Some(deleted),
        )?;
        Ok(())
    }
}

/// Rules 10 and 11: a publication stores the build with its job's success; the
/// job's counts are the output's, and the build's files are the output.
#[test]
fn a_publication_stores_the_build_with_the_jobs_success() -> TestResult {
    let fixture = Fixture::new()?;
    let claimed = fixture.settled(1, 1, 100)?;
    let files = output("console.log(1)");

    let (job, publication) = fixture.publish(&claimed, 1, &files, 200)?;

    assert_eq!(job.state, JobState::Succeeded);
    let counts = job.counts.ok_or("a succeeded job has its counts")?;
    assert_eq!(counts.files, 2);
    assert_eq!(counts.bytes, 28 + 14);
    let Publication::Published(build) = publication else {
        return Err(format!("the first build is published: {publication:?}").into());
    };
    assert_eq!(build.job, job.id);
    assert_eq!(build.revision, SourceRevision(1));
    assert_eq!((build.file_count, build.total_bytes), (2, 42));
    assert_eq!(build.tool_digest, TOOL_DIGEST);
    assert_eq!(build.published_at, at(200));
    assert_eq!(fixture.build(1)?, Some(build.clone()));
    assert_eq!(fixture.latest()?, Some(build));
    assert_eq!(fixture.files(1)?, Some(files));
    Ok(())
}

/// Rule 10: a later success for the same revision and profile publishes
/// nothing new. Its job succeeds, the first build and its files stay, and the
/// later output's bytes are not stored.
#[test]
fn a_later_success_for_the_same_revision_keeps_the_first_build() -> TestResult {
    let fixture = Fixture::new()?;
    let first = fixture.built(1, 1, 100)?;
    let claimed = fixture.settled(1, 2, 300)?;

    let (job, publication) = fixture.publish(&claimed, 2, &output("a later body"), 400)?;

    assert_eq!(job.state, JobState::Succeeded);
    assert_eq!(publication, Publication::AlreadyBuilt(first.clone()));
    assert_eq!(fixture.build(1)?, Some(first));
    assert_eq!(fixture.files(1)?, Some(output("r1")));
    assert_eq!(
        fixture.count("SELECT count(*) FROM blobs WHERE content = CAST('a later body' AS BLOB)")?,
        0,
        "the later output is not stored"
    );
    Ok(())
}

/// Rule 11: a refused publication stores no build, no file and no blob.
#[test]
fn a_refused_publication_stores_no_build_and_no_blob() -> TestResult {
    let fixture = Fixture::new()?;
    let claimed = fixture.settled(1, 1, 100)?;
    let files = output("refused body");
    let other_tools = "f".repeat(64);

    let refused = fixture.store.publish_build(BuildPublication {
        lease: &claimed.lease,
        attempt: &attempt_id(1),
        binding: ProfileBinding {
            profile: &profile(),
            tool_digest: &other_tools,
        },
        files: &files,
        now: at(200),
    });

    assert!(
        matches!(refused, Err(JobStoreError::InvalidRequest(_))),
        "a binding of other tools is refused: {refused:?}"
    );
    assert_eq!(fixture.build(1)?, None);
    assert_eq!(fixture.count("SELECT count(*) FROM build_files")?, 0);
    assert_eq!(
        fixture.count("SELECT count(*) FROM blobs WHERE content = CAST('refused body' AS BLOB)")?,
        0,
        "no blob is added outside a publication"
    );
    Ok(())
}

/// Rules 12 and 15: an artifact keeps at most eight builds. The oldest by
/// publication time go first, but the latest build (the highest revision) is
/// kept even when it was published first.
#[test]
fn the_bound_removes_the_oldest_builds_but_never_the_latest() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.revise_to(10)?;
    fixture.built(10, 10, 100)?;
    for revision in 1..=9u8 {
        fixture.built(u64::from(revision), revision, 200 + i64::from(revision))?;
    }

    assert_eq!(
        fixture.built_revisions()?,
        vec![3, 4, 5, 6, 7, 8, 9, 10],
        "revisions 1 and 2 are the oldest that are not the latest"
    );
    assert_eq!(
        usize::try_from(fixture.count("SELECT count(*) FROM builds")?)?,
        MAX_BUILDS_PER_ARTIFACT
    );
    assert_eq!(
        fixture.count("SELECT count(*) FROM build_files")?,
        16,
        "the removed builds' files go with them"
    );
    assert_eq!(
        fixture.latest()?.map(|build| build.revision),
        Some(SourceRevision(10))
    );
    Ok(())
}

/// Rule 12: the build just published is never removed, even when the caller's
/// clock makes it the oldest.
#[test]
fn the_bound_never_removes_the_build_it_publishes() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.revise_to(9)?;
    for revision in 2..=9u8 {
        fixture.built(u64::from(revision), revision, 200 + i64::from(revision))?;
    }

    fixture.built(1, 1, 50)?;

    assert_eq!(
        fixture.built_revisions()?,
        vec![1, 3, 4, 5, 6, 7, 8, 9],
        "revision 2 goes; revision 1, published at an earlier time, stays"
    );
    Ok(())
}

/// Rules 12 and 23: the reclamation of unreferenced content counts build files
/// as references, so a build's bytes outlive a cleanup.
#[test]
fn cleanup_keeps_the_content_of_a_build() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.built(1, 1, 100)?;
    let reviewed = fixture.store.unreferenced_content()?;
    assert_eq!(reviewed.blobs, 0, "a build's content is referenced");

    fixture.store.remove_unreferenced_content(CleanupRequest {
        request: RequestIdentity {
            actor: &fixture.owner,
            key: &key("cleanup"),
            fingerprint: RequestFingerprint(digest("cleanup")),
        },
        base: reviewed.digest,
    })?;

    assert_eq!(fixture.files(1)?, Some(output("r1")));
    Ok(())
}

/// Rule 12: deleting an asset whose bytes a build also holds keeps them.
#[test]
fn deleting_an_asset_keeps_the_content_a_build_shares() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.built(1, 1, 100)?;
    let media: AssetMediaType = parsed("text/plain");
    let (upload_key, delete_key) = (key("upload"), key("delete"));
    let write = |request_key, base| AssetWrite {
        space: Space::Own,
        request: RequestIdentity {
            actor: &fixture.owner,
            key: request_key,
            fingerprint: RequestFingerprint(digest(&format!("asset {base}"))),
        },
        artifact: &fixture.created.artifact.id,
        base: AssetVersion(base),
        expected_source: None,
        now: at(200),
    };
    let uploaded = fixture
        .store
        .upload_asset(write(&upload_key, 0), &media, b"r1", Ok(()))?;

    fixture
        .store
        .delete_asset(write(&delete_key, 1), &uploaded.id)?;

    assert_eq!(fixture.files(1)?, Some(output("r1")));
    Ok(())
}

/// Rule 13: a `build_failed` job keeps its diagnostics in their order, and
/// when another `run_profile` job of the artifact fails, the earlier ones are
/// deleted.
#[test]
fn only_the_newest_failed_job_keeps_diagnostics() -> TestResult {
    let fixture = Fixture::new()?;
    let first = fixture.settled(1, 1, 100)?;
    let kept = [
        diagnostic(Some("src/App.tsx"), "Could not resolve \"./missing\""),
        diagnostic(None, "the build was stopped by the system call filter"),
    ];
    let failed = fixture.store.fail_build(
        &first.lease,
        &attempt_id(1),
        JobFailure::BuildFailed,
        &kept,
        at(200),
    )?;
    assert_eq!(failed.state, JobState::Failed);
    assert_eq!(failed.failure, Some(JobFailure::BuildFailed));
    assert_eq!(fixture.store.diagnostics(&first.job.id)?, kept);

    let second = fixture.settled(1, 2, 300)?;
    fixture.store.fail_build(
        &second.lease,
        &attempt_id(2),
        JobFailure::LimitExceeded,
        &[],
        at(400),
    )?;

    assert_eq!(
        fixture.store.diagnostics(&first.job.id)?,
        Vec::new(),
        "the newer failure deletes the earlier diagnostics"
    );
    assert_eq!(fixture.count("SELECT count(*) FROM job_diagnostics")?, 0);
    Ok(())
}

/// Rules 13 and 18: a build failure that the store does not take is refused
/// before anything is written, and the job keeps running.
#[test]
fn a_build_failure_out_of_its_rules_is_refused() -> TestResult {
    let fixture = Fixture::new()?;
    let claimed = fixture.settled(1, 1, 100)?;
    let one = [diagnostic(None, "message")];
    let too_many = vec![diagnostic(None, "message"); 21];
    for (failure, diagnostics, what) in [
        (
            JobFailure::BuildFailed,
            &too_many[..],
            "build_failed with 21 diagnostics",
        ),
        (
            JobFailure::LimitExceeded,
            &one[..],
            "limit_exceeded with a diagnostic",
        ),
        (
            JobFailure::Internal,
            &[][..],
            "a failure that is not a build's",
        ),
    ] {
        let refused = fixture.store.fail_build(
            &claimed.lease,
            &attempt_id(1),
            failure,
            diagnostics,
            at(200),
        );
        assert!(
            matches!(refused, Err(JobStoreError::InvalidRequest(_))),
            "{what}: {refused:?}"
        );
    }
    let state: String = Connection::open(database_in(&fixture.data()))?.query_row(
        "SELECT state FROM jobs WHERE id = ?1",
        [claimed.job.id.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(state, "running");
    assert_eq!(fixture.count("SELECT count(*) FROM job_diagnostics")?, 0);
    Ok(())
}

/// Rules 13 and 18: a non-zero exit whose messages split into none is still
/// `build_failed`, with no diagnostics.
#[test]
fn build_failed_may_keep_no_diagnostics() -> TestResult {
    let fixture = Fixture::new()?;
    let claimed = fixture.settled(1, 1, 100)?;

    let failed = fixture.store.fail_build(
        &claimed.lease,
        &attempt_id(1),
        JobFailure::BuildFailed,
        &[],
        at(200),
    )?;

    assert_eq!(failed.failure, Some(JobFailure::BuildFailed));
    assert_eq!(fixture.store.diagnostics(&claimed.job.id)?, Vec::new());
    Ok(())
}

/// Rule 13: "newest" is enqueue order. A job enqueued earlier that fails after
/// a newer job failed keeps no diagnostics and deletes none of the newer
/// job's.
#[test]
fn an_older_job_failing_later_keeps_the_newer_jobs_diagnostics() -> TestResult {
    let fixture = Fixture::new()?;
    let older = fixture.settled(1, 1, 100)?;
    let newer = fixture.settled(1, 2, 110)?;
    let kept = [diagnostic(Some("src/App.tsx"), "the newer failure")];
    fixture.store.fail_build(
        &newer.lease,
        &attempt_id(2),
        JobFailure::BuildFailed,
        &kept,
        at(200),
    )?;

    let late = fixture.store.fail_build(
        &older.lease,
        &attempt_id(1),
        JobFailure::BuildFailed,
        &[diagnostic(None, "the older failure")],
        at(300),
    )?;

    assert_eq!(late.state, JobState::Failed);
    assert_eq!(fixture.store.diagnostics(&newer.job.id)?, kept);
    assert_eq!(fixture.store.diagnostics(&older.job.id)?, Vec::new());
    Ok(())
}

/// Rules 13 and 18: a build failure is recorded through `fail_build` only, so
/// `fail_job` cannot end a job as `build_failed` without diagnostics.
#[test]
fn fail_job_refuses_a_build_failure() -> TestResult {
    let fixture = Fixture::new()?;
    let claimed = fixture.settled(1, 1, 100)?;
    for failure in [
        JobFailure::BuildFailed,
        JobFailure::LimitExceeded,
        JobFailure::OutputRejected,
    ] {
        let refused = fixture
            .store
            .fail_job(&claimed.lease, failure, Retry::Never, at(200));
        assert!(
            matches!(refused, Err(JobStoreError::InvalidRequest(_))),
            "{failure:?}: {refused:?}"
        );
    }
    let state: String = Connection::open(database_in(&fixture.data()))?.query_row(
        "SELECT state FROM jobs WHERE id = ?1",
        [claimed.job.id.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(state, "running");
    Ok(())
}

/// 0057 rule 7 through rule 13: a revoked attempt, here by a requested cancel,
/// stores no diagnostics and the revocation is kept.
#[test]
fn a_revoked_attempt_stores_no_diagnostics() -> TestResult {
    let fixture = Fixture::new()?;
    let claimed = fixture.settled(1, 1, 100)?;
    Connection::open(database_in(&fixture.data()))?.execute(
        "UPDATE jobs SET cancel_requested = 1 WHERE id = ?1",
        [claimed.job.id.as_str()],
    )?;

    let refused = fixture.store.fail_build(
        &claimed.lease,
        &attempt_id(1),
        JobFailure::BuildFailed,
        &[diagnostic(Some("src/App.tsx"), "names source text")],
        at(200),
    );

    assert_eq!(refused, Err(JobStoreError::Revoked(StopReason::Cancel)));
    assert_eq!(fixture.count("SELECT count(*) FROM job_diagnostics")?, 0);
    assert_eq!(
        fixture
            .store
            .attempt_row(&attempt_id(1))?
            .and_then(|row| row.revoked),
        Some(Revocation::Cancel)
    );
    Ok(())
}

/// Rule 12: the trash keeps an artifact's builds and diagnostics and serves
/// none of them; recovery serves them again.
#[test]
fn the_trash_keeps_builds_and_diagnostics_and_serves_none() -> TestResult {
    let fixture = Fixture::new()?;
    let build = fixture.built(1, 1, 100)?;
    let failing = fixture.settled(1, 2, 200)?;
    let kept = [diagnostic(None, "message")];
    fixture.store.fail_build(
        &failing.lease,
        &attempt_id(2),
        JobFailure::BuildFailed,
        &kept,
        at(300),
    )?;

    fixture.trash(1, true)?;
    assert_eq!(fixture.build(1)?, None);
    assert_eq!(fixture.latest()?, None);
    assert_eq!(fixture.files(1)?, None);
    assert_eq!(fixture.store.diagnostics(&failing.job.id)?, Vec::new());
    assert_eq!(
        fixture.count("SELECT count(*) FROM builds")?,
        1,
        "kept in the trash"
    );

    fixture.trash(2, false)?;
    assert_eq!(fixture.latest()?, Some(build));
    assert_eq!(fixture.files(1)?, Some(output("r1")));
    assert_eq!(fixture.store.diagnostics(&failing.job.id)?, kept);
    Ok(())
}

/// Rule 12: a purge deletes the artifact's builds, their files, its jobs'
/// diagnostics and the content only its builds held, in its transaction.
#[test]
fn a_purge_deletes_builds_diagnostics_and_their_content() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.built(1, 1, 100)?;
    let failing = fixture.settled(1, 2, 200)?;
    fixture.store.fail_build(
        &failing.lease,
        &attempt_id(2),
        JobFailure::BuildFailed,
        &[diagnostic(Some("src/App.tsx"), "names source text")],
        at(300),
    )?;
    fixture.trash(1, true)?;

    fixture.store.purge_artifact(PurgeRequest {
        request: RequestIdentity {
            actor: &fixture.owner,
            key: &key("purge"),
            fingerprint: RequestFingerprint(digest("purge")),
        },
        artifact: &fixture.created.artifact.id,
        base: MetadataRevision(2),
    })?;

    for table in ["builds", "build_files", "job_diagnostics"] {
        assert_eq!(
            fixture.count(&format!("SELECT count(*) FROM {table}"))?,
            0,
            "the purge deletes {table}"
        );
    }
    assert_eq!(
        fixture.count("SELECT count(*) FROM blobs WHERE content = CAST('r1' AS BLOB)")?,
        0,
        "the content only the build held is deleted"
    );
    Ok(())
}

/// Rule 12: a backup copies the builds and a restore keeps them, bound to the
/// same revisions.
#[test]
fn backup_and_restore_keep_builds() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.revise_to(2)?;
    let build = fixture.built(2, 1, 100)?;
    let backup_file = fixture.root.join("backup.sqlite3");
    backup(&fixture.data(), &backup_file)?;
    let target = fixture.root.join("restored");
    restore(&backup_file, &target)?;
    let restored = SqliteStore::open(&database_in(&target))?;

    assert_eq!(
        restored.latest_build(&fixture.created.artifact.id, &profile())?,
        Some(build)
    );
    assert_eq!(
        restored.build_files(&fixture.created.artifact.id, SourceRevision(2), &profile())?,
        Some(output("r2"))
    );
    Ok(())
}

/// Rule 23: a publication of 25 MiB fits in one transaction well within the
/// worker's lease.
#[test]
fn a_25_mib_publication_fits_within_the_lease() -> TestResult {
    let fixture = Fixture::new()?;
    let claimed = fixture.settled(1, 1, 100)?;
    let mut files = vec![(PackagePath::entry(), b"<script src=0.js></script>".to_vec())];
    for index in 0..5u8 {
        let path: PackagePath = parsed(&format!("{index}.js"));
        let mut content = vec![b'a' + index; MAX_FILE_BYTES];
        if index == 0 {
            content.truncate(MAX_FILE_BYTES - 26);
        }
        files.push((path, content));
    }
    let files = FilePackage::new(files)?;

    let started = Instant::now();
    let (job, publication) = fixture.publish(&claimed, 1, &files, 200)?;
    let elapsed = started.elapsed();

    assert_eq!(job.state, JobState::Succeeded);
    assert!(matches!(publication, Publication::Published(_)));
    assert_eq!(
        job.counts.map(|counts| counts.bytes),
        Some(25 * 1024 * 1024)
    );
    assert!(
        elapsed < Duration::from_millis(LEASE_MS / 2),
        "the publication took {elapsed:?}, more than half the lease"
    );
    assert_eq!(fixture.files(1)?, Some(files));
    Ok(())
}

impl Fixture {
    /// Enqueues a `run_profile` job of `profile` for `revision` at `now` and
    /// leaves it queued.
    fn enqueued(
        &self,
        revision: u64,
        profile: &ProfileName,
        name: &str,
        now: i64,
    ) -> Result<JobRecord, Box<dyn std::error::Error>> {
        Ok(self
            .store
            .enqueue_profile_job(NewProfileJob {
                request: RequestIdentity {
                    actor: &self.owner,
                    key: &key(name),
                    fingerprint: RequestFingerprint(digest(name)),
                },
                artifact: &self.created.artifact.id,
                revision: SourceRevision(revision),
                profile,
                now: at(now),
            })?
            .job)
    }

    fn snapshot(&self) -> Result<Option<BuildSnapshot>, JobStoreError> {
        self.store
            .build_snapshot(&self.created.artifact.id, &profile())
    }

    fn file(&self, revision: u64, path: &str) -> Result<Option<Vec<u8>>, JobStoreError> {
        self.store.build_file(
            &self.created.artifact.id,
            SourceRevision(revision),
            &profile(),
            &parsed(path),
        )
    }
}

/// Rules 15 and 16 (slice R03cc): one snapshot gives the head revision, the
/// latest build, the newest job of the profile for the head by enqueue order, and the
/// diagnostics that job keeps. A job of another revision or another profile is
/// not the revision's.
#[test]
fn the_snapshot_reads_the_newest_job_of_the_revision_by_enqueue_order() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.revise_to(2)?;
    let build = fixture.built(1, 1, 100)?;
    let older = fixture.settled(2, 2, 200)?;
    let kept = [diagnostic(Some("src/main.tsx"), "Could not resolve \"x\"")];
    fixture.store.fail_build(
        &older.lease,
        &attempt_id(2),
        JobFailure::BuildFailed,
        &kept,
        at(210),
    )?;

    let snapshot = fixture.snapshot()?.ok_or("the artifact is in place")?;
    assert_eq!(snapshot.head, SourceRevision(2));
    assert_eq!(snapshot.latest, Some(build.clone()));
    assert_eq!(snapshot.newest_job.map(|job| job.id), Some(older.job.id));
    assert_eq!(snapshot.diagnostics, kept);

    let newer = fixture.enqueued(2, &profile(), "newer", 300)?;
    fixture.enqueued(1, &profile(), "other-revision", 400)?;
    fixture.enqueued(2, &parsed("fixture"), "other-profile", 500)?;
    let snapshot = fixture.snapshot()?.ok_or("the artifact is in place")?;
    assert_eq!(snapshot.latest, Some(build));
    assert_eq!(
        snapshot.newest_job.map(|job| (job.id, job.state)),
        Some((newer.id, JobState::Queued))
    );
    assert_eq!(snapshot.diagnostics, Vec::new());

    fixture.trash(1, true)?;
    assert_eq!(fixture.snapshot()?, None, "the trash serves no snapshot");
    Ok(())
}

/// Rule 14 (slice R03cc): one file of a build is read by its path, and the
/// trash serves none.
#[test]
fn a_build_file_is_read_by_its_path() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.revise_to(2)?;
    fixture.built(1, 1, 100)?;

    assert_eq!(fixture.file(1, "app.js")?, Some(b"r1".to_vec()));
    assert_eq!(
        fixture.file(1, "index.html")?,
        Some(b"<script src=app.js></script>".to_vec())
    );
    assert_eq!(fixture.file(1, "missing.js")?, None);
    assert_eq!(fixture.file(2, "app.js")?, None, "revision 2 has no build");

    fixture.trash(1, true)?;
    assert_eq!(fixture.file(1, "app.js")?, None, "the trash serves no file");
    fixture.trash(2, false)?;
    assert_eq!(fixture.file(1, "app.js")?, Some(b"r1".to_vec()));
    Ok(())
}
