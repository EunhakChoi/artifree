//! Inspection command only; no HTTP listener is started.
#![expect(
    clippy::print_stdout,
    reason = "Help, version, and JSON are the CLI protocol."
)]
#![expect(
    clippy::print_stderr,
    reason = "Command failures must be reported on stderr."
)]

use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => {
            print_help();
            Ok(())
        }
        [arg] if arg == "--help" || arg == "-h" => {
            print_help();
            Ok(())
        }
        [arg] if arg == "--version" => {
            println!("artifree-server {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        [arg] if arg == "--describe" => {
            let json = serde_json::to_string_pretty(&artifree_server::describe())
                .map_err(|error| format!("Could not serialize scaffold inventory: {error}"))?;
            println!("{json}");
            Ok(())
        }
        _ => Err("Unknown arguments. Use --help. No HTTP server exists in this scaffold.".into()),
    }
}

fn print_help() {
    println!(
        "Artifree server scaffold\n\nUsage: artifree-server [--describe | --version | --help]\n\n--describe emits the build inventory as JSON. No network listener is started.\nThe HTTP framework and async runtime are intentionally undecided."
    );
}
