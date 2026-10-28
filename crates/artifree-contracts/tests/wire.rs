//! Behavioral checks for the public contracts' Serde boundary.
use artifree_contracts::ServiceDescription;
use artifree_contracts::api::{
    Artifact, ArtifactChange, ArtifactList, CommitResponse, CreateArtifactRequest,
    DuplicateArtifactResponse, ErrorResponse, FeedPosition, Preview, Revision, RevisionList,
    SaveRevisionRequest, UpdateArtifactResponse,
};
use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde_json::Value;

fn descriptor() -> Value {
    serde_json::json!({
        "name": "Artifree", "version": "0.1.0", "contract_version": "0.1.0",
        "phase": "early-development", "server_framework": null, "async_runtime": null,
        "http_api": false, "mcp_api": false,
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

fn save_request() -> Value {
    serde_json::json!({
        "base_revision": 3,
        "changes": [
            { "op": "put", "path": "index.html", "encoding": "utf8", "content": "<p>x</p>" },
            { "op": "delete", "path": "old.css" }
        ]
    })
}

fn save_error(value: Value) -> String {
    match serde_json::from_value::<SaveRevisionRequest>(value) {
        Ok(parsed) => panic!("Invalid change was accepted: {parsed:?}"),
        Err(error) => error.to_string(),
    }
}

#[test]
fn save_request_round_trips_tagged_changes() -> Result<(), serde_json::Error> {
    let expected = save_request();
    let parsed: SaveRevisionRequest = serde_json::from_value(expected.clone())?;
    assert_eq!(serde_json::to_value(parsed)?, expected);
    Ok(())
}

#[test]
fn save_request_rejects_unknown_operations_and_fields() {
    let mut unknown_op = save_request();
    unknown_op["changes"][1]["op"] = Value::String("rename".into());
    assert!(save_error(unknown_op).contains("unknown variant `rename`"));

    let mut extra = save_request();
    extra["changes"][1]["content"] = Value::String("ignored?".into());
    assert!(save_error(extra).contains("unknown field `content`"));

    let mut missing = save_request();
    missing["changes"][0]
        .as_object_mut()
        .map(|change| change.remove("encoding"));
    assert!(save_error(missing).contains("missing field `encoding`"));

    let mut negative = save_request();
    negative["base_revision"] = serde_json::json!(-1);
    assert!(save_error(negative).contains("invalid value"));
}

#[test]
fn create_request_rejects_unknown_encodings_and_fields() {
    let request = |files: Value| serde_json::json!({ "title": "T", "files": files });
    for (files, message) in [
        (
            serde_json::json!([{ "path": "index.html", "encoding": "hex", "content": "" }]),
            "unknown variant `hex`",
        ),
        (
            serde_json::json!([{ "path": "index.html", "encoding": "utf8", "content": "", "mode": 755 }]),
            "unknown field `mode`",
        ),
    ] {
        let Err(error) = serde_json::from_value::<CreateArtifactRequest>(request(files)) else {
            panic!("Invalid upload accepted; expected {message}");
        };
        assert!(
            error.to_string().contains(message),
            "Expected {message:?}, got {error}"
        );
    }
}

/// Whether the schema Schemars derives for reading `T`, the published one,
/// differs from the one it derives for writing it.
fn published_unlike_written<T: JsonSchema>() -> bool {
    let schema = |settings: SchemaSettings| settings.into_generator().into_root_schema_for::<T>();
    schema(SchemaSettings::draft2020_12()) != schema(SchemaSettings::draft2020_12().for_serialize())
}

/// Clients read responses with the published schema, where an `Option`
/// would be optional. Each response must be published as the server writes
/// it: every field it always writes required, with its type.
#[test]
fn response_schemas_describe_what_the_server_writes() {
    let differing: Vec<&str> = [
        ("Artifact", published_unlike_written::<Artifact>()),
        ("ArtifactList", published_unlike_written::<ArtifactList>()),
        (
            "CommitResponse",
            published_unlike_written::<CommitResponse>(),
        ),
        (
            "DuplicateArtifactResponse",
            published_unlike_written::<DuplicateArtifactResponse>(),
        ),
        (
            "UpdateArtifactResponse",
            published_unlike_written::<UpdateArtifactResponse>(),
        ),
        ("Revision", published_unlike_written::<Revision>()),
        ("RevisionList", published_unlike_written::<RevisionList>()),
        ("Preview", published_unlike_written::<Preview>()),
        ("ErrorResponse", published_unlike_written::<ErrorResponse>()),
        (
            "ArtifactChange",
            published_unlike_written::<ArtifactChange>(),
        ),
        ("FeedPosition", published_unlike_written::<FeedPosition>()),
    ]
    .into_iter()
    .filter_map(|(name, differs)| differs.then_some(name))
    .collect();
    assert_eq!(differing, Vec::<&str>::new());
}
