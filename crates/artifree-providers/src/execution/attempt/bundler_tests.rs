//! The bundler measurement (decision 0058, note "R03bcb split", slice R03bcba): the root's
//! bundler builds a test template inside the attempt's sandbox with the argv and environment of
//! rule 1, under the measuring filter, which kills only the calls the public syscall mapping
//! refuses and leaves a kernel record for every call the fixture does not allow.
//!
//! The test is ignored here, because it launches the bundler in its attempt group.
//! `scripts/enforcement/run.sh` (part 15) runs it once per input in `ARTIFREE_MEASURE_CASE`,
//! several times each, and reads the kernel records between runs. The template is test support:
//! the real templates belong to T01b.

use std::env;
use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::os::unix::process::ExitStatusExt;

use artifree_domain::package::{FilePackage, PackagePath};

use super::super::launcher::EntryProgram;
use super::super::profile::MEASUREMENT;
use super::super::staging::{InputListing, VerifiedInput};
use super::resource_tests::evidence_of;
use super::tests::ExecutionDir;
use super::{AttemptPlan, launch_with};

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
/// `SIGSYS`, the signal of a filter kill.
const SIGSYS: i32 = 31;

/// The input of one measured case.
fn input_of(case: &str) -> Result<VerifiedInput, Box<dyn Error>> {
    let mut files: Vec<(PackagePath, Vec<u8>)> = vec![
        (PackagePath::entry(), INDEX.to_vec()),
        ("src/app.css".parse()?, STYLE.to_vec()),
    ];
    match case {
        "template" => files.push(("src/main.tsx".parse()?, MAIN.to_vec())),
        "bad-import" => files.push(("src/main.tsx".parse()?, BAD_IMPORT.to_vec())),
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
    assert_ne!(
        evidence.exit.signal(),
        Some(SIGSYS),
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
