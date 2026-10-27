//! Emits the public JSON Schemas for contract generation.
//!
//! Output: `{ "service_description": <schema>, "api": <schema>, "mcp": <tools> }`.
//! The API schema's root accepts any v1 JSON request or response body, the
//! query of the artifact list, and the data of each change feed event; each
//! body and its parts are `$defs` entries. The MCP entry is the tool catalog
//! that `tools/list` returns, with the server instructions and the schema of
//! an error result.
#![expect(
    clippy::print_stdout,
    reason = "This binary emits the JSON schemas on stdout."
)]

use artifree_contracts::api::{
    Artifact, ArtifactChange, ArtifactList, CommitResponse, CreateArtifactRequest, ErrorResponse,
    FeedPosition, ListArtifactsQuery, Preview, Revision, RevisionList, SaveRevisionRequest,
    UpdateArtifactRequest, UpdateArtifactResponse,
};
use artifree_contracts::{ServiceDescription, mcp};
use schemars::generate::SchemaSettings;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut generator = SchemaSettings::draft2020_12().into_generator();
    let bodies = [
        generator.subschema_for::<CreateArtifactRequest>(),
        generator.subschema_for::<SaveRevisionRequest>(),
        generator.subschema_for::<UpdateArtifactRequest>(),
        generator.subschema_for::<CommitResponse>(),
        generator.subschema_for::<UpdateArtifactResponse>(),
        generator.subschema_for::<Artifact>(),
        generator.subschema_for::<ListArtifactsQuery>(),
        generator.subschema_for::<ArtifactList>(),
        generator.subschema_for::<Revision>(),
        generator.subschema_for::<RevisionList>(),
        generator.subschema_for::<Preview>(),
        generator.subschema_for::<ErrorResponse>(),
        generator.subschema_for::<ArtifactChange>(),
        generator.subschema_for::<FeedPosition>(),
    ];
    let api = serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://artifree.invalid/schemas/api-v1.json",
        "title": "ApiBody",
        "description": "Any JSON request or response body of HTTP API v1, or the data of a change feed event.",
        "anyOf": bodies,
        "$defs": generator.take_definitions(true),
    });
    let output = serde_json::json!({
        "service_description": schemars::schema_for!(ServiceDescription),
        "api": api,
        "mcp": {
            "instructions": mcp::SERVER_INSTRUCTIONS,
            "tools": mcp::tools(),
            "error_result": mcp::error_schema(),
        },
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
