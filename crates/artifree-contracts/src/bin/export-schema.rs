use artifree_contracts::ServiceDescription;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let schema = schemars::schema_for!(ServiceDescription);
    println!("{}", serde_json::to_string_pretty(&schema)?);
    Ok(())
}
