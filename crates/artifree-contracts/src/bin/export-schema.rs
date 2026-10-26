//! Emits the public JSON Schemas for contract generation.
//!
//! Output: `{ "service_description": <schema>, "api": <schema> }`. The API
//! schema's root accepts any v1 JSON request or response body; each body and
//! its parts are `$defs` entries.
#![expect(
    clippy::print_stdout,
    reason = "This binary emits the JSON schemas on stdout."
)]

use artifree_contracts::ServiceDescription;
use artifree_contracts::api::{
    ArtifactList, CommitResponse, CreateArtifactRequest, ErrorResponse, Preview, RevisionList,
    SaveRevisionRequest,
};
use schemars::generate::SchemaSettings;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut generator = SchemaSettings::draft2020_12().into_generator();
    let bodies = [
        generator.subschema_for::<CreateArtifactRequest>(),
        generator.subschema_for::<SaveRevisionRequest>(),
        generator.subschema_for::<CommitResponse>(),
        generator.subschema_for::<ArtifactList>(),
        generator.subschema_for::<RevisionList>(),
        generator.subschema_for::<Preview>(),
        generator.subschema_for::<ErrorResponse>(),
    ];
    let api = serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://artifree.invalid/schemas/api-v1.json",
        "title": "ApiBody",
        "description": "Any JSON request or response body of HTTP API v1.",
        "anyOf": bodies,
        "$defs": generator.take_definitions(true),
    });
    let output = serde_json::json!({
        "service_description": schemars::schema_for!(ServiceDescription),
        "api": api,
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
