//! `artifree-server backup` and `restore`: argument parsing and the one-line
//! JSON result. The work is in `artifree_providers::backup`.

use std::ffi::OsString;
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
            print_result("backup", "output", &output, report);
        }
    }
    Ok(())
}

/// Runs `restore`. Prints help or one JSON line.
pub fn restore_command(arguments: &[OsString]) -> Result<(), String> {
    match parse(arguments, ["--input", "--data-dir"])? {
        Parsed::Help => print_text(RESTORE_HELP),
        Parsed::Run(input, data_dir) => {
            let report = backup::restore(&input, &data_dir).map_err(|error| error.to_string())?;
            print_result("restore", "data_dir", &data_dir, report);
        }
    }
    Ok(())
}

fn print_text(text: &str) {
    println!("{text}");
}

fn print_result(event: &str, key: &str, path: &Path, report: Report) {
    let line = serde_json::json!({
        "event": event,
        key: path.to_string_lossy(),
        "schema_version": report.schema_version,
        "bytes": report.bytes,
    });
    println!("{line}");
}

#[cfg(test)]
mod tests {
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
}
