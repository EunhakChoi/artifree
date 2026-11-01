//! The `backup` and `restore` commands of the installed binary: exit codes,
//! one JSON line on stdout, diagnostics on stderr.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use artifree_providers::storage::{DATABASE_FILE, SCHEMA_VERSION, SqliteStore};

type TestResult = Result<(), Box<dyn Error>>;

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("artifree-cli-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path)
            .unwrap_or_else(|error| panic!("cannot create {}: {error}", path.display()));
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> Result<Output, Box<dyn Error>> {
    Ok(Command::new(env!("CARGO_BIN_EXE_artifree-server"))
        .args(args)
        .output()?)
}

fn one_json_line(output: &Output) -> Result<serde_json::Value, Box<dyn Error>> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "diagnostics on success: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout.clone())?;
    assert_eq!(text.lines().count(), 1, "exactly one line, got {text:?}");
    assert!(text.ends_with('\n'));
    Ok(serde_json::from_str(&text)?)
}

fn data_dir_with_database(root: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let data = root.join("data");
    std::fs::create_dir(&data)?;
    SqliteStore::open(&data.join(DATABASE_FILE))?;
    Ok(data)
}

#[test]
fn backup_then_restore_print_one_json_line_each() -> TestResult {
    let scratch = Scratch::new("roundtrip");
    let data = data_dir_with_database(&scratch.0)?;
    let output = scratch.0.join("backup.sqlite3");
    let target = scratch.0.join("restored");

    let saved = run(&[
        "backup".as_ref(),
        "--data-dir".as_ref(),
        data.as_os_str(),
        "--output".as_ref(),
        output.as_os_str(),
    ])?;
    let saved = one_json_line(&saved)?;
    assert_eq!(saved["event"], "backup");
    assert_eq!(saved["output"], output.to_string_lossy().as_ref());
    assert_eq!(saved["schema_version"], SCHEMA_VERSION);
    assert_eq!(saved["bytes"], std::fs::metadata(&output)?.len());

    let restored = run(&[
        "restore".as_ref(),
        "--data-dir".as_ref(),
        target.as_os_str(),
        "--input".as_ref(),
        output.as_os_str(),
    ])?;
    let restored = one_json_line(&restored)?;
    assert_eq!(restored["event"], "restore");
    assert_eq!(restored["data_dir"], target.to_string_lossy().as_ref());
    assert_eq!(restored["schema_version"], SCHEMA_VERSION);
    SqliteStore::open(&target.join(DATABASE_FILE))?;
    Ok(())
}

#[test]
fn help_prints_usage_and_exits_zero() -> TestResult {
    for (command, usage) in [
        ("backup", "Usage: artifree-server backup"),
        ("restore", "Usage: artifree-server restore"),
    ] {
        for flag in ["--help", "-h"] {
            let output = run(&[command, flag])?;
            assert_eq!(output.status.code(), Some(0), "{command} {flag}");
            assert!(output.stderr.is_empty());
            assert!(
                String::from_utf8(output.stdout)?.starts_with(usage),
                "{command} {flag}"
            );
        }
    }
    Ok(())
}

#[test]
fn bad_flags_exit_one_with_stderr_only() -> TestResult {
    for args in [
        vec!["backup"],
        vec!["backup", "--data-dir", "a"],
        vec![
            "backup",
            "--data-dir",
            "a",
            "--output",
            "b",
            "--output",
            "c",
        ],
        vec!["backup", "--data-dir", "a", "--output", "b", "--force"],
        vec!["backup", "--data-dir", "a", "--output"],
        vec!["backup", "--help", "--output", "b"],
        vec!["backup", "--data-dir", "", "--output", "b"],
        vec!["backup", "--data-dir", "a", "--output", ""],
        vec!["restore", "--input", "", "--data-dir", "c"],
        vec!["restore"],
        vec!["restore", "--input", "a", "--input", "b", "--data-dir", "c"],
        vec!["restore", "--input", "a", "--data-dir", "b", "extra"],
    ] {
        let output = run(&args)?;
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(
            String::from_utf8(output.stderr)?.starts_with("Unknown arguments."),
            "{args:?}"
        );
    }
    Ok(())
}

#[test]
fn a_refused_restore_exits_one_and_prints_only_to_stderr() -> TestResult {
    let scratch = Scratch::new("refused");
    let data = data_dir_with_database(&scratch.0)?;
    let output = scratch.0.join("backup.sqlite3");
    let saved = run(&[
        "backup".as_ref(),
        "--data-dir".as_ref(),
        data.as_os_str(),
        "--output".as_ref(),
        output.as_os_str(),
    ])?;
    assert!(saved.status.success());

    // The data directory exists: nothing is restored into it.
    let refused = run(&[
        "restore".as_ref(),
        "--input".as_ref(),
        output.as_os_str(),
        "--data-dir".as_ref(),
        data.as_os_str(),
    ])?;
    assert_eq!(refused.status.code(), Some(1));
    assert!(refused.stdout.is_empty());
    assert!(String::from_utf8(refused.stderr)?.contains("already exists"));

    // The output exists: nothing is replaced.
    let again = run(&[
        "backup".as_ref(),
        "--data-dir".as_ref(),
        data.as_os_str(),
        "--output".as_ref(),
        output.as_os_str(),
    ])?;
    assert_eq!(again.status.code(), Some(1));
    assert!(again.stdout.is_empty());
    assert!(String::from_utf8(again.stderr)?.contains("already exists"));
    Ok(())
}

/// Arguments that are not UTF-8 are reported or used as paths, never a panic
/// (exit code 101).
#[cfg(unix)]
#[test]
fn arguments_that_are_not_utf8_do_not_panic() -> TestResult {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let scratch = Scratch::new("utf8");
    let bad = scratch.0.join(OsString::from_vec(vec![b'd', 0xff, 0xfe]));
    let output = scratch.0.join("out.sqlite3");
    let missing = run(&[
        "backup".as_ref(),
        "--data-dir".as_ref(),
        bad.as_os_str(),
        "--output".as_ref(),
        output.as_os_str(),
    ])?;
    assert_eq!(missing.status.code(), Some(1), "a path is used as it is");
    assert!(missing.stdout.is_empty());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("no database at"));

    for args in [
        vec![OsString::from("serve"), OsString::from_vec(vec![0xff])],
        vec![OsString::from_vec(vec![0xff, 0xfe])],
        vec![OsString::from("backup"), OsString::from_vec(vec![0xc0])],
    ] {
        let rejected = run(&args)?;
        assert_eq!(rejected.status.code(), Some(1), "{args:?}");
        assert!(rejected.stdout.is_empty());
        assert!(String::from_utf8_lossy(&rejected.stderr).starts_with("Unknown arguments."));
    }
    Ok(())
}
