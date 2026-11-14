//! The enforcement cases of builds (decision 0058, slice R03cb): a `react-v1` job runs the
//! root's bundler through the real worker, and its end is the one rule 18 names. The template is
//! published as a build, a bad import ends as `build_failed` with the bundler's message split
//! into a diagnostic (rule 23's assumption for R03cb), resolver configuration ends as
//! `build_failed` with its fixed diagnostic, and an `index.html` that does not load the bundle ends
//! as `output_rejected`. The `react-v1` check runs the fixture's payload under `react-v1`'s filter,
//! so that a filter kill ends as `build_failed` with its fixed diagnostic and the log bound ends as
//! `limit_exceeded`. The template's build is then previewed under `/b/` (slice R03cc): each of
//! its files is served from the build, under the content security policy.
//!
//! One ignored test runs the case in `ARTIFREE_BUILD_CASE`. `scripts/enforcement/run.sh` (part
//! 17) runs it once per case, with the payload scenario each `react-v1` check case needs.

use artifree_domain::job::BuildDiagnostic;
use artifree_ports::build_store::BuildStore;
use artifree_server::auth::AccessToken;
use artifree_server::http::{AppState, Settings};
use artifree_server::limits::RequestLimits;
use artifree_server::{changes, preview};
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt;

use super::*;

/// A stored diagnostic as its location and its message.
type Shown = (Option<(String, u32, u32)>, String);

/// The environment variable that names the build case.
const BUILD_CASE: &str = "ARTIFREE_BUILD_CASE";
/// The template's entry document, which loads the bundle as rule 7 requires.
const INDEX: &[u8] =
    include_bytes!("../../../artifree-providers/src/execution/attempt/bundler_template/index.html");
/// The template's entry module.
const MAIN: &[u8] =
    include_bytes!("../../../artifree-providers/src/execution/attempt/bundler_template/main.tsx");
/// The template's style sheet.
const STYLE: &[u8] =
    include_bytes!("../../../artifree-providers/src/execution/attempt/bundler_template/app.css");
/// An entry module that imports a package the root does not hold.
const BAD_IMPORT: &[u8] = b"import pad from \"left-pad\";\nconsole.log(pad(\"x\", 3));\n";
/// An entry document that does not load the bundle.
const NO_SCRIPT: &[u8] = b"<!doctype html>\n<div id=\"root\"></div>\n";

/// One build case: the profile, the revision's files, and the end it must reach.
struct BuildCase {
    profile: &'static str,
    files: Vec<(&'static str, &'static [u8])>,
    end: End,
}

/// The end a build case must reach.
#[derive(Debug, PartialEq, Eq)]
enum End {
    /// Succeeded, with a build of these files.
    Published(Vec<String>),
    /// Failed with this failure and these diagnostics, as (location, message).
    Failed(JobFailure, Vec<Shown>),
}

/// The case `name` names.
fn build_case(name: &str) -> Result<BuildCase, Box<dyn Error>> {
    let page = |index: &'static [u8], main: &'static [u8]| {
        vec![
            ("index.html", index),
            ("src/main.tsx", main),
            ("src/app.css", STYLE),
        ]
    };
    let template = |main: &'static [u8]| page(INDEX, main);
    let no_file = |text: &str| (None, text.to_owned());
    let case = match name {
        "template" => BuildCase {
            profile: "react-v1",
            files: template(MAIN),
            end: End::Published(
                ["app.css", "app.js", "index.html"]
                    .map(str::to_owned)
                    .to_vec(),
            ),
        },
        "bad-import" => BuildCase {
            profile: "react-v1",
            files: template(BAD_IMPORT),
            end: End::Failed(
                JobFailure::BuildFailed,
                vec![(
                    Some(("src/main.tsx".to_owned(), 1, 16)),
                    "Could not resolve \"left-pad\"".to_owned(),
                )],
            ),
        },
        "resolver" => {
            let mut files = template(BAD_IMPORT);
            files.push(("tsconfig.json", b"{}\n"));
            BuildCase {
                profile: "react-v1",
                files,
                end: End::Failed(
                    JobFailure::BuildFailed,
                    vec![no_file(
                        "tsconfig.json would change how imports resolve; react-v1 builds only \
                         with its preinstalled packages, so remove it",
                    )],
                ),
            }
        }
        "no-script" => BuildCase {
            profile: "react-v1",
            files: page(NO_SCRIPT, MAIN),
            end: End::Failed(JobFailure::OutputRejected, Vec::new()),
        },
        // The payload's `thread-refusal` scenario, which run.sh selects.
        "filter" => BuildCase {
            profile: "react-v1-check",
            files: vec![("index.html", INDEX)],
            end: End::Failed(
                JobFailure::BuildFailed,
                vec![no_file(
                    "the bundler was stopped because it made a system call outside its filter",
                )],
            ),
        },
        // The payload's `flood` scenario, which run.sh selects.
        "flood" => BuildCase {
            profile: "react-v1-check",
            files: vec![("index.html", INDEX)],
            end: End::Failed(JobFailure::LimitExceeded, Vec::new()),
        },
        other => return Err(format!("unknown build case {other:?}").into()),
    };
    Ok(case)
}

/// The content origin of the preview.
const ORIGIN: &str = "http://127.0.0.1:7411";

/// A diagnostic as (location, message).
fn shown(diagnostic: BuildDiagnostic) -> Shown {
    (
        diagnostic
            .location
            .map(|at| (at.path.as_str().to_owned(), at.line, at.column)),
        diagnostic.message.as_str().to_owned(),
    )
}

impl Store {
    /// Registers the owner and creates its artifact at revision 1 with `files`.
    fn create_build_artifact(&self, files: &[(&str, &[u8])]) -> TestResult {
        SqliteTokenStore::open(&self.path)?.register(&NewUser {
            id: owner()?,
            label: parsed::<UserLabel>("Owner")?,
            created_at: "2026-11-15T00:00:00Z".to_owned(),
        })?;
        let files = files
            .iter()
            .map(|(path, content)| Ok((parsed::<PackagePath>(path)?, content.to_vec())))
            .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
        self.service()?.create(
            &owner()?,
            &parsed::<RequestKey>("create-artifact")?,
            &parsed::<Title>("Build")?,
            &FilePackage::new(files)?,
        )?;
        Ok(())
    }

    /// Enqueues one job of `profile` for the artifact at revision 1.
    fn enqueue_build(&self, profile: &str) -> Result<JobId, Box<dyn Error>> {
        let receipt = self.service()?.enqueue_profile_job(
            &owner()?,
            &parsed::<RequestKey>(&format!("build-{profile}"))?,
            &self.artifact()?,
            SourceRevision(1),
            &parsed::<ProfileName>(profile)?,
            SystemClock.now(),
        )?;
        Ok(receipt.job.id)
    }

    /// The end the job reached, once it is succeeded or failed.
    fn build_end(&self, job: &JobId, profile: &ProfileName) -> Result<End, Box<dyn Error>> {
        let row = poll(within(VERDICT_DEADLINE), "the build job to end", || {
            let row = self.job_row(job)?;
            Ok(matches!(row.state.as_str(), "succeeded" | "failed").then_some(row))
        })?;
        let artifact = self.artifact()?;
        let files = self
            .port
            .build_files(&artifact, SourceRevision(1), profile)?;
        if row.state == "succeeded" {
            let files = files.ok_or("a succeeded build job published no build")?;
            return Ok(End::Published(
                files
                    .files()
                    .map(|(path, _)| path.as_str().to_owned())
                    .collect(),
            ));
        }
        if files.is_some() {
            return Err("a failed build job published a build".into());
        }
        let failure = row.failure.ok_or("a failed job names no failure")?;
        let failure: JobFailure = parsed(&failure)?;
        let diagnostics = self.port.diagnostics(job)?.into_iter().map(shown).collect();
        Ok(End::Failed(failure, diagnostics))
    }
}

impl Store {
    /// Issues a build preview of revision 1 through the management preview operation and checks
    /// that `/b/` serves each file of the published build under the content security policy
    /// (decision 0058 rule 14, slice R03cc).
    fn preview_the_build(&self, profile: &ProfileName) -> TestResult {
        let files = self
            .port
            .build_files(&self.artifact()?, SourceRevision(1), profile)?
            .ok_or("the build is missing")?;
        let state = AppState::new(
            SqliteStore::open(&self.path)?,
            SqliteTokenStore::open(&self.path)?,
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
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(async {
            let url = state
                .issue_build_preview(owner()?, self.artifact()?.as_str(), 1, profile.clone())
                .await?;
            let entry = url
                .strip_prefix(ORIGIN)
                .ok_or("the preview uses another origin")?;
            let router = preview::router(state);
            let entry_document = PackagePath::entry();
            let served = files
                .files()
                .map(|(path, content)| (format!("{entry}{path}"), path, content))
                .chain(std::iter::once((
                    entry.to_owned(),
                    &entry_document,
                    files
                        .file(&entry_document)
                        .ok_or("the build has no entry")?,
                )));
            for (target, path, content) in served {
                let response = router
                    .clone()
                    .oneshot(Request::get(&target).body(Body::empty())?)
                    .await?;
                assert_eq!(response.status(), StatusCode::OK, "{target}");
                let header_of = |name: header::HeaderName| {
                    response
                        .headers()
                        .get(name)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("")
                        .to_owned()
                };
                assert_eq!(
                    header_of(header::CONTENT_TYPE),
                    path.media_type().essence(),
                    "{target}"
                );
                assert!(
                    header_of(header::CONTENT_SECURITY_POLICY)
                        .starts_with(&format!("sandbox {}; ", preview::SANDBOX)),
                    "{target}"
                );
                let body = to_bytes(response.into_body(), usize::MAX).await?;
                assert_eq!(body.as_ref(), content, "{target}");
            }
            Ok::<(), Box<dyn Error>>(())
        })
    }
}

/// Decision 0058 rules 7, 8, 13 and 18 (slice R03cb): the job of the case in
/// `ARTIFREE_BUILD_CASE` ends as rule 18 names, its attempt settled.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_a_build_ends_as_its_case_names() -> TestResult {
    let name = std::env::var(BUILD_CASE)?;
    let case = build_case(&name)?;
    let scratch = Scratch::new(&format!("build-{name}"))?;
    let store = Store::new(&scratch.data())?;
    store.create_build_artifact(&case.files)?;
    let job = store.enqueue_build(case.profile)?;
    let _worker = Worker::spawn(&scratch, &[])?;
    let end = store.build_end(&job, &parsed(case.profile)?)?;
    assert_eq!(end, case.end, "the {name} build");
    if matches!(end, End::Published(_)) {
        store.preview_the_build(&parsed(case.profile)?)?;
    }
    assert_eq!(
        store.attempt_of(&job)?.state,
        AttemptState::Settled,
        "the {name} build's attempt must be settled"
    );
    Ok(())
}
