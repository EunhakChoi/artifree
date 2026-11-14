//! The bundler in the attempt's sandbox (decision 0058). The measurement (note "R03bcb split",
//! slice R03bcba) builds a test template with the argv and environment of rule 1 under the
//! measuring filter, which kills only the calls the public syscall mapping refuses and leaves a
//! kernel record for every call the fixture does not allow. The `react-v1` tests (slice R03bcbb)
//! build it as the profile, under the profile's own filter and the limits of 0042 rule 5, and show
//! that the filter kills a call outside it.
//!
//! The tests are ignored here, because they launch in an attempt group.
//! `scripts/enforcement/run.sh` runs the measurement once per input in `ARTIFREE_MEASURE_CASE`
//! (part 15), and the `react-v1` tests once per case (part 16). The template is test support: the
//! real templates belong to T01b.

use std::env;
use std::error::Error;
use std::fmt::Write as _;
use std::fs;

use artifree_domain::package::{FilePackage, PackagePath};

use super::super::AdmissionRefusal;
use super::super::collection::{CollectionRefusal, PackageReason};
use super::super::launcher::{EntryProgram, KILLED_BY_SIGSYS};
use super::super::probe_group::PROBE_LIMITS;
use super::super::profile::{MEASUREMENT, REACT_V1, REACT_V1_FILTER_CHECK};
use super::super::staging::InputRefusal;
use super::super::staging::{InputListing, VerifiedInput};
use super::mapping_tests::{UNSHARE, check_refused_by_kill};
use super::resource_tests::evidence_of;
use super::tests::{ExecutionDir, assert_no_attempt_groups};
use super::{AttemptFailure, AttemptPlan, OutputRefusal, launch_with};

/// The template's entry document.
const INDEX: &[u8] = include_bytes!("bundler_template/index.html");
/// The template's entry module, which imports `react`, `react-dom/client` and its CSS.
const MAIN: &[u8] = include_bytes!("bundler_template/main.tsx");
/// The template's style sheet.
const STYLE: &[u8] = include_bytes!("bundler_template/app.css");
/// An entry module that imports a package the root does not hold.
const BAD_IMPORT: &[u8] = b"import pad from \"left-pad\";\nconsole.log(pad(\"x\", 3));\n";
/// How many generated modules the larger input imports, below the package's 256 files.
const LARGE_MODULES: usize = 200;

/// The input of one measured case.
fn input_of(case: &str) -> Result<VerifiedInput, Box<dyn Error>> {
    let mut files: Vec<(PackagePath, Vec<u8>)> = vec![
        (PackagePath::entry(), INDEX.to_vec()),
        ("src/app.css".parse()?, STYLE.to_vec()),
    ];
    match case {
        "template" => files.push(("src/main.tsx".parse()?, MAIN.to_vec())),
        "bad-import" => files.push(("src/main.tsx".parse()?, BAD_IMPORT.to_vec())),
        // The bad import, mapped to a module of the revision by a path mapping that the bundler
        // would read from `tsconfig.json`.
        "path-mapping" => {
            files.push(("src/main.tsx".parse()?, BAD_IMPORT.to_vec()));
            files.push((
                "src/pad.ts".parse()?,
                b"export default (text: string, width: number) => text.padStart(width);\n".to_vec(),
            ));
            files.push((
                "tsconfig.json".parse()?,
                b"{ \"compilerOptions\": { \"paths\": { \"left-pad\": [\"./src/pad.ts\"] } } }\n"
                    .to_vec(),
            ));
        }
        "large" => {
            let mut main = String::from(
                "import { createRoot } from \"react-dom/client\";\nimport \"./app.css\";\n",
            );
            for index in 0..LARGE_MODULES {
                writeln!(
                    main,
                    "import {{ Part{index} }} from \"./parts/part{index}\";"
                )?;
                let module = format!(
                    "import {{ useMemo }} from \"react\";\n\
                     export function Part{index}({{ seed }}: {{ seed: number }}) {{\n  \
                     const value = useMemo(() => Array.from({{ length: {index} + 8 }}, (_, at) => at * seed).join(\",\"), [seed]);\n  \
                     return <li data-part=\"{index}\">{{value}}</li>;\n}}\n"
                );
                files.push((
                    format!("src/parts/part{index}.tsx").parse()?,
                    module.into_bytes(),
                ));
            }
            main.push_str("const root = document.getElementById(\"root\");\nif (root !== null) {\n  createRoot(root).render(<ul>\n");
            for index in 0..LARGE_MODULES {
                writeln!(main, "    <Part{index} seed={{{index}}} />")?;
            }
            main.push_str("  </ul>);\n}\n");
            files.push(("src/main.tsx".parse()?, main.into_bytes()));
        }
        other => return Err(format!("unknown measured case {other:?}").into()),
    }
    let package = FilePackage::new(files).map_err(|error| format!("{error:?}"))?;
    let listing = InputListing::of(&package);
    VerifiedInput::verify(package, listing).map_err(|refusal| format!("{refusal:?}").into())
}

#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_the_bundler_runs_under_the_measuring_filter() -> Result<(), Box<dyn Error>> {
    let case = env::var("ARTIFREE_MEASURE_CASE")?;
    let plan = AttemptPlan {
        program: EntryProgram::Server,
        profile: &MEASUREMENT,
        ..AttemptPlan::fixture()
    };
    let dir = ExecutionDir::new("bundler")?;
    let evidence = evidence_of(launch_with(&plan, input_of(&case)?, dir.path()))?;
    // Written before any assertion, so that a failed run still leaves its report behind.
    if let Some(report) = env::var_os("ARTIFREE_ATTEMPT_REPORT") {
        fs::write(report, evidence.to_string())?;
    }
    let stderr = String::from_utf8_lossy(&evidence.stderr);
    // The launcher reports the payload's death by `SIGSYS` as this exit status.
    assert_ne!(
        evidence.exit.code(),
        Some(KILLED_BY_SIGSYS),
        "the {case} build was killed by the filter; stderr: {stderr}"
    );
    if case == "bad-import" {
        assert_eq!(evidence.exit.code(), Some(1), "stderr: {stderr}");
        assert!(
            stderr.contains("Could not resolve \"left-pad\""),
            "the unknown import must be named; stderr: {stderr}"
        );
    } else {
        assert_eq!(
            evidence.exit.code(),
            Some(0),
            "the {case} build must succeed; stderr: {stderr}"
        );
    }
    Ok(())
}

/// The plan of a `react-v1` attempt: the server as the entry program, as for a job.
fn react_v1_plan() -> AttemptPlan {
    AttemptPlan {
        program: EntryProgram::Server,
        profile: &REACT_V1,
        ..AttemptPlan::fixture()
    }
}

/// Decision 0058 rules 1 to 4 (slice R03bcbb): `react-v1` builds the case in
/// `ARTIFREE_REACT_CASE` under its own filter and the limits of 0042 rule 5. The template exits 0,
/// and its output is collected within the output limits and is refused only
/// for its missing `index.html`, which R03c's composition adds (rule 8). A bad import exits 1
/// with the one message that names the import.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_react_v1_builds_the_template_within_the_limits() -> Result<(), Box<dyn Error>> {
    let case = env::var("ARTIFREE_REACT_CASE")?;
    let dir = ExecutionDir::new("react-v1")?;
    let evidence = evidence_of(launch_with(&react_v1_plan(), input_of(&case)?, dir.path()))?;
    if let Some(report) = env::var_os("ARTIFREE_ATTEMPT_REPORT") {
        fs::write(report, evidence.to_string())?;
    }
    let stderr = String::from_utf8_lossy(&evidence.stderr);
    let limits: Vec<(&str, &str)> = evidence
        .limits
        .iter()
        .map(|(control, value)| (*control, value.as_str()))
        .collect();
    assert_eq!(
        limits, PROBE_LIMITS,
        "the attempt group holds the limits of 0042"
    );
    match case.as_str() {
        "template" => {
            assert_eq!(
                evidence.exit.code(),
                Some(0),
                "the template must build; stderr: {stderr}"
            );
            assert_eq!(
                evidence.output.as_ref().err(),
                Some(&OutputRefusal::Refused(CollectionRefusal::Package(
                    PackageReason::MissingEntry
                ))),
                "the output is read within its limits and lacks only index.html"
            );
        }
        "bad-import" => {
            assert_eq!(evidence.exit.code(), Some(1), "stderr: {stderr}");
            assert!(
                stderr.contains("Could not resolve \"left-pad\""),
                "the unknown import must be named; stderr: {stderr}"
            );
            // The one message, within the profile's message bound (`--log-limit`).
            assert!(
                stderr.trim_end().ends_with("1 error"),
                "stderr must end with its count; stderr: {stderr}"
            );
            assert_eq!(
                evidence.output.as_ref().err(),
                Some(&OutputRefusal::NotCollected)
            );
        }
        other => return Err(format!("unknown react-v1 case {other:?}").into()),
    }
    assert_no_attempt_groups()
}

/// Decision 0058 rule 3 (slice R03bcbb): a call outside `react-v1`'s filter kills the attempt.
/// The fixture's payload runs under that filter, and its `thread-refusal` scenario calls
/// `unshare` from a thread; the kernel holds exactly that refusal.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_react_v1_kills_a_call_outside_its_filter() -> Result<(), Box<dyn Error>> {
    // The server is the entry program, as for the measurement: the fixture program is built
    // without the feature, so it knows neither test entry.
    let plan = AttemptPlan {
        program: EntryProgram::Server,
        profile: &REACT_V1_FILTER_CHECK,
        ..AttemptPlan::fixture()
    };
    check_refused_by_kill(
        "react-v1-filter",
        &plan,
        "thread-refusal started",
        "thread-refusal returned",
        UNSHARE,
    )
}

/// Decision 0058 rule 19 for `react-v1` (slice R03bcbb): with one byte appended to a file of the
/// root's `react` package, the attempt is refused before it starts.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_react_v1_refuses_a_modified_root() -> Result<(), Box<dyn Error>> {
    let dir = ExecutionDir::new("react-v1-modified")?;
    let outcome = launch_with(&react_v1_plan(), input_of("template")?, dir.path());
    match outcome {
        Err(refusal) => assert_eq!(
            refusal.failure,
            AttemptFailure::Refused(AdmissionRefusal::ToolRootMismatch)
        ),
        Ok(evidence) => {
            return Err(
                format!("the attempt must be refused, it exited {:?}", evidence.exit).into(),
            );
        }
    }
    assert_no_attempt_groups()
}

/// Decision 0058 rule 2 (slice R03bcbb): an input that would change how the bundler resolves an
/// import is refused before it is staged. The bad import is mapped to a module of the revision by
/// `tsconfig.json`, which the bundler would otherwise read and then build the import.
#[test]
#[ignore = "runs only in the Linux enforcement job"]
fn job_react_v1_refuses_resolver_configuration() -> Result<(), Box<dyn Error>> {
    let dir = ExecutionDir::new("react-v1-resolver")?;
    let outcome = launch_with(&react_v1_plan(), input_of("path-mapping")?, dir.path());
    match outcome {
        Err(refusal) => assert_eq!(
            refusal.failure,
            AttemptFailure::Input(InputRefusal::ResolverFile)
        ),
        Ok(evidence) => {
            return Err(format!(
                "the attempt must be refused, it exited {:?}; stderr: {}",
                evidence.exit,
                String::from_utf8_lossy(&evidence.stderr)
            )
            .into());
        }
    }
    assert_no_attempt_groups()
}
