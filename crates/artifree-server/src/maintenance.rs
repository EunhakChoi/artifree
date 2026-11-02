//! `artifree-server backup` and `restore`: argument parsing and the one-line
//! JSON result. The work is in `artifree_providers::backup`.

use std::ffi::OsString;
use std::fmt::Display;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use artifree_providers::backup::{self, Report};

const BACKUP_HELP: &str = "Usage: artifree-server backup --data-dir <dir> --output <file>

Writes a consistent copy of the database in <dir> to the new file <output>,
also while the server runs. It reports success only after proving that the
file restores, and never leaves a partial file. Run it as the user that runs
the server; on Unix a database of another user is refused.

Options:
  --data-dir <dir>   Data directory of the server (required)
  --output <file>    New backup file; must not exist (required)

On success stdout is one JSON line:
{\"event\":\"backup\",\"output\":...,\"schema_version\":...,\"bytes\":...}
A path that is not valid UTF-8 is shown lossily in \"output\" and the line also
has \"path_utf8\":false and the exact path in \"path_bytes\" (an array of byte
values; on Windows \"path_wide\", UTF-16 code units). If the scratch directory
cannot be removed after the backup was published, stderr has a warning, the
backup is kept, and the exit status is still 0.
See docs/backup-restore.md.";

const RESTORE_HELP: &str = "Usage: artifree-server restore --input <file> --data-dir <dir>

Restores a backup file into the new data directory <dir>, which must not
exist. The restored database is upgraded to the current schema and gets a new
change feed identity. The access token is not part of a backup: the first
serve creates one. Run it as the user that will run the server.

Options:
  --input <file>     Backup file made by `artifree-server backup` (required)
  --data-dir <dir>   New data directory; must not exist (required)

On success stdout is one JSON line:
{\"event\":\"restore\",\"data_dir\":...,\"schema_version\":...,\"bytes\":...}
A path that is not valid UTF-8 is shown lossily in \"data_dir\" and the line also
has \"path_utf8\":false and the exact path in \"path_bytes\" (an array of byte
values; on Windows \"path_wide\", UTF-16 code units). If the scratch directory
cannot be removed after the restore was published, stderr has a warning, the
restored data directory is kept, and the exit status is still 0.
See docs/backup-restore.md.";

/// What a parsed command line asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    /// Print the help text.
    Help,
    /// Run with the values of the two required flags, in the order of the usage line.
    Run(PathBuf, PathBuf),
}

/// Parses the arguments after `backup` or `restore`: exactly two required
/// flags with values, each once, or a lone `--help`.
///
/// # Errors
///
/// A message starting with `Unknown arguments.`
pub fn parse(arguments: &[OsString], flags: [&str; 2]) -> Result<Parsed, String> {
    if let [only] = arguments
        && (only == "--help" || only == "-h")
    {
        return Ok(Parsed::Help);
    }
    let mut values: [Option<PathBuf>; 2] = [None, None];
    let mut remaining = arguments.iter();
    while let Some(argument) = remaining.next() {
        let Some(position) = flags.iter().position(|flag| argument == flag) else {
            return Err(format!(
                "Unknown arguments. Unrecognized option {}; use --help.",
                argument.to_string_lossy()
            ));
        };
        let flag = flags[position];
        let Some(value) = remaining.next() else {
            return Err(format!(
                "Unknown arguments. {flag} requires a value; use --help."
            ));
        };
        if value.is_empty() {
            return Err(format!(
                "Unknown arguments. {flag} needs a non-empty path; use --help."
            ));
        }
        if values[position].replace(PathBuf::from(value)).is_some() {
            return Err(format!(
                "Unknown arguments. {flag} was given twice; use --help."
            ));
        }
    }
    match values {
        [Some(first), Some(second)] => Ok(Parsed::Run(first, second)),
        [None, _] => Err(format!(
            "Unknown arguments. {} is required; use --help.",
            flags[0]
        )),
        [_, None] => Err(format!(
            "Unknown arguments. {} is required; use --help.",
            flags[1]
        )),
    }
}

/// Runs `backup`. Prints help or one JSON line.
pub fn backup_command(arguments: &[OsString]) -> Result<(), String> {
    match parse(arguments, ["--data-dir", "--output"])? {
        Parsed::Help => print_text(BACKUP_HELP),
        Parsed::Run(data_dir, output) => {
            let report = backup::backup(&data_dir, &output).map_err(|error| error.to_string())?;
            print_result("backup", "output", &output, &report)
        }
    }
}

/// Runs `restore`. Prints help or one JSON line.
pub fn restore_command(arguments: &[OsString]) -> Result<(), String> {
    match parse(arguments, ["--input", "--data-dir"])? {
        Parsed::Help => print_text(RESTORE_HELP),
        Parsed::Run(input, data_dir) => {
            let report = backup::restore(&input, &data_dir).map_err(|error| error.to_string())?;
            print_result("restore", "data_dir", &data_dir, &report)
        }
    }
}

fn print_text(text: &str) -> Result<(), String> {
    settle(write_line(&mut io::stdout().lock(), text), "the help text")
}

fn print_result(event: &str, key: &str, path: &Path, report: &Report) -> Result<(), String> {
    write_result(
        event,
        key,
        path,
        report,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    )
}

/// Writes the result line to `stdout`, then a warning to `stderr` when a
/// scratch directory was left behind. The work is already done and published
/// at this point, so nothing here may panic or delete it: a closed stdout is
/// ignored, and another write failure is reported with the work's outcome.
fn write_result(
    event: &str,
    key: &str,
    path: &Path,
    report: &Report,
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> Result<(), String> {
    let printed = write_line(stdout, result_line(event, key, path, report));
    if let Some(leftover) = &report.leftover {
        // Diagnostics are best effort: there is no better place to report
        // that stderr cannot be written.
        drop(writeln!(
            stderr,
            "warning: the {event} succeeded, but the scratch directory {} could not be removed ({}); delete it when no backup or restore is running",
            leftover.path.display(),
            leftover.reason
        ));
    }
    settle(
        printed,
        &format!("the result of the {event}, which succeeded and was kept,"),
    )
}

/// The one JSON line. `key` holds the path as text, lossily when it is not
/// valid UTF-8; `path_utf8` is then `false` and the exact path follows in
/// `path_bytes` (Unix) or `path_wide` (Windows).
fn result_line(event: &str, key: &str, path: &Path, report: &Report) -> serde_json::Value {
    let mut line = serde_json::json!({
        "event": event,
        key: path.to_string_lossy(),
        "schema_version": report.schema_version,
        "bytes": report.bytes,
    });
    if path.to_str().is_none() {
        let (units_key, units) = path_units(path);
        line["path_utf8"] = serde_json::json!(false);
        line[units_key] = serde_json::json!(units);
    }
    line
}

#[cfg(unix)]
fn path_units(path: &Path) -> (&'static str, Vec<u8>) {
    use std::os::unix::ffi::OsStrExt;
    ("path_bytes", path.as_os_str().as_bytes().to_vec())
}

#[cfg(windows)]
fn path_units(path: &Path) -> (&'static str, Vec<u16>) {
    use std::os::windows::ffi::OsStrExt;
    ("path_wide", path.as_os_str().encode_wide().collect())
}

/// Writes `text` and a newline, and flushes, without panicking.
fn write_line(out: &mut impl Write, text: impl Display) -> io::Result<()> {
    writeln!(out, "{text}").and_then(|()| out.flush())
}

/// A reader that went away (a closed pipe) is not a failure of the command;
/// any other write failure is, and says what could not be shown.
fn settle(written: io::Result<()>, what: &str) -> Result<(), String> {
    match written {
        Err(error) if error.kind() != io::ErrorKind::BrokenPipe => {
            Err(format!("cannot write {what} to stdout: {error}"))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::string::FromUtf8Error;

    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn flags_may_come_in_either_order() {
        let expected = Ok(Parsed::Run("in".into(), "dir".into()));
        let flags = ["--input", "--data-dir"];
        assert_eq!(
            parse(&args(&["--input", "in", "--data-dir", "dir"]), flags),
            expected
        );
        assert_eq!(
            parse(&args(&["--data-dir", "dir", "--input", "in"]), flags),
            expected
        );
    }

    #[test]
    fn help_is_only_a_lone_flag() {
        let flags = ["--input", "--data-dir"];
        assert_eq!(parse(&args(&["-h"]), flags), Ok(Parsed::Help));
        assert!(parse(&args(&["--help", "--input", "a"]), flags).is_err());
    }

    #[test]
    fn duplicate_missing_and_unknown_flags_are_refused() {
        let flags = ["--input", "--data-dir"];
        for bad in [
            &["--input", "a", "--input", "b", "--data-dir", "c"][..],
            &["--input", "a"],
            &["--data-dir", "c"],
            &["--input", "a", "--data-dir"],
            &["--input", "a", "--data-dir", "c", "--force"],
            &["--input", "", "--data-dir", "c"],
            &["--input", "a", "--data-dir", ""],
            &[],
        ] {
            let Err(error) = parse(&args(bad), flags) else {
                panic!("accepted {bad:?}");
            };
            assert!(error.starts_with("Unknown arguments."), "{bad:?}: {error}");
        }
    }

    fn report(leftover: Option<backup::Leftover>) -> Report {
        Report {
            schema_version: 5,
            bytes: 4096,
            leftover,
        }
    }

    /// A writer that fails every write with `kind`.
    struct Failing(io::ErrorKind);

    impl Write for Failing {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(self.0.into())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(self.0.into())
        }
    }

    #[test]
    fn a_utf8_path_is_one_plain_string_without_the_extra_fields() {
        let line = result_line(
            "backup",
            "output",
            Path::new("/data/b.sqlite3"),
            &report(None),
        );
        assert_eq!(
            line,
            serde_json::json!({
                "event": "backup",
                "output": "/data/b.sqlite3",
                "schema_version": 5,
                "bytes": 4096,
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_utf8_is_marked_and_given_exactly() {
        use std::os::unix::ffi::OsStrExt;
        let path = Path::new(std::ffi::OsStr::from_bytes(b"/d/b\xff\xfe.sqlite3"));
        let line = result_line("restore", "data_dir", path, &report(None));
        assert_eq!(line["path_utf8"], false);
        assert_eq!(
            line["path_bytes"],
            serde_json::json!(b"/d/b\xff\xfe.sqlite3".to_vec())
        );
        assert_eq!(line["data_dir"], "/d/b\u{fffd}\u{fffd}.sqlite3");
    }

    #[test]
    fn a_closed_stdout_is_ignored_and_the_warning_still_goes_to_stderr() -> Result<(), FromUtf8Error>
    {
        let leftover = backup::Leftover {
            path: "/out/.artifree-scratch-ab".into(),
            reason: "permission denied".to_owned(),
        };
        let mut stderr = Vec::new();

        let result = write_result(
            "backup",
            "output",
            Path::new("/out/b"),
            &report(Some(leftover)),
            &mut Failing(io::ErrorKind::BrokenPipe),
            &mut stderr,
        );

        assert_eq!(result, Ok(()));
        let warning = String::from_utf8(stderr)?;
        assert!(
            warning.starts_with("warning: the backup succeeded")
                && warning.contains("/out/.artifree-scratch-ab")
                && warning.contains("permission denied"),
            "{warning}"
        );
        Ok(())
    }

    #[test]
    fn another_stdout_failure_is_reported_as_an_error() {
        let result = write_result(
            "backup",
            "output",
            Path::new("/out/b"),
            &report(None),
            &mut Failing(io::ErrorKind::StorageFull),
            &mut Vec::new(),
        );
        let Err(message) = result else {
            panic!("a full disk must be reported");
        };
        assert!(message.contains("succeeded and was kept"), "{message}");
    }

    #[test]
    fn a_result_without_a_leftover_writes_one_line_and_no_warning() -> Result<(), String> {
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());

        write_result(
            "restore",
            "data_dir",
            Path::new("/d"),
            &report(None),
            &mut stdout,
            &mut stderr,
        )?;

        assert!(stderr.is_empty());
        let text = String::from_utf8(stdout).map_err(|error| error.to_string())?;
        assert_eq!(text.lines().count(), 1, "{text:?}");
        assert!(text.ends_with('\n'));
        Ok(())
    }
}
