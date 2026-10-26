//! Emits the public descriptor schema as JSON for contract generation.
#![expect(
    clippy::print_stdout,
    reason = "This binary emits the JSON schema on stdout."
)]

use artifree_contracts::ServiceDescription;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let schema = schemars::schema_for!(ServiceDescription);
    println!("{}", serde_json::to_string_pretty(&schema)?);
    Ok(())
}
