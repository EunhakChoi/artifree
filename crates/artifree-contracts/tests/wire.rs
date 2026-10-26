//! Behavioral checks for the public descriptor's Serde boundary.
use artifree_contracts::ServiceDescription;
use serde_json::Value;

fn descriptor() -> Value {
    serde_json::json!({
        "name": "Artifree", "version": "0.1.0", "contract_version": "0.1.0",
        "phase": "scaffolding", "server_framework": null, "async_runtime": null,
        "http_listening": false,
        "components": [{ "id": "web", "name": "Web", "layer": "presentation",
            "language": "TypeScript", "path": "apps/web", "status": "scaffold", "summary": "Development shell." }]
    })
}

fn assert_rejected(value: Value, message: &str) {
    let Err(error) = serde_json::from_value::<ServiceDescription>(value) else {
        panic!("Invalid wire input was accepted; expected {message}");
    };
    assert!(error.is_data(), "Expected a data validation error: {error}");
    assert!(
        error.to_string().contains(message),
        "Expected {message:?}, got {error}"
    );
}

#[test]
fn round_trip_preserves_explicit_nulls_and_snake_case() -> Result<(), serde_json::Error> {
    let expected = descriptor();
    let parsed: ServiceDescription = serde_json::from_value(expected.clone())?;
    assert_eq!(serde_json::to_value(parsed)?, expected);
    Ok(())
}

#[test]
fn rejects_unknown_top_level_field() {
    let mut invalid = descriptor();
    invalid["surprise"] = Value::Bool(true);
    assert_rejected(invalid, "unknown field `surprise`");
}

#[test]
fn rejects_unknown_nested_field() {
    let mut invalid = descriptor();
    invalid["components"][0]["surprise"] = Value::Bool(true);
    assert_rejected(invalid, "unknown field `surprise`");
}

#[test]
fn rejects_unsupported_layer() {
    let mut invalid = descriptor();
    invalid["components"][0]["layer"] = Value::String("database".into());
    assert_rejected(invalid, "unknown variant `database`");
}
