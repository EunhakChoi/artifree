//! Emits the public JSON Schemas for contract generation.
//!
//! Output: `{ "service_description": <schema>, "api": <schema>, "mcp": <tools> }`.
//! The API schema's root accepts any v1 JSON request or response body, the
//! data of each change feed event, and the manifest of a source export; each body and its parts are `$defs`
//! entries. The query of the artifact list is a `$defs` entry only: it is
//! not a body, and with every field optional it would admit any object. The
//! body of a duplicate is a `$defs` entry only for the same reason: its
//! fields are optional, so the empty object is one. The MCP entry is the tool catalog
//! that `tools/list` returns, with the server instructions and the schema of
//! an error result.
#![expect(
    clippy::print_stdout,
    reason = "This binary emits the JSON schemas on stdout."
)]

use artifree_contracts::api::{
    AgentSessionList, AppData, Artifact, ArtifactChange, ArtifactList, CancelJobRequest,
    CancelJobResponse, CommitResponse, CreateArtifactRequest, CreateRuntimeViewRequest,
    CreateTokenRequest, CreateTokenResponse, CreateUserRequest, DataMembers, DataWriteResponse,
    DeleteDataRequest, DuplicateArtifactRequest, DuplicateArtifactResponse, EditFilesRequest,
    EnqueueJobRequest, EnqueueJobResponse, ErrorResponse, FeedPosition, Handoff,
    HandoffActionRequest, Job, JobList, ListArtifactsQuery, ListJobsQuery, Preview,
    PurgeArtifactRequest, PurgeArtifactResponse, ReadDataMembersQuery, ReadDataQuery,
    RemoveUnreferencedContentRequest, RemoveUnreferencedContentResponse, RestoreRevisionRequest,
    Revision, RevisionList, RoomEventReceipt, RoomEventRequest, RoomHeartbeatRequest,
    RoomJoinRequest, RoomSnapshot, RotateTokenResponse, RuntimeConnection, RuntimeDownloadOffer,
    RuntimeDownloadRequest, RuntimeGrant, RuntimeMetadata, RuntimeMetadataRequest,
    RuntimeSourceList, RuntimeSourceListRequest, RuntimeSourcePart, RuntimeSourceReadRequest,
    RuntimeSourceWriteReceipt, RuntimeSourceWriteRequest, RuntimeView, SaveRevisionRequest,
    SetDataRequest, SetRuntimeGrantRequest, SourceExportManifest, StageHandoffRequest, TokenInfo,
    TokenList, UnreferencedContent, UpdateArtifactRequest, UpdateArtifactResponse,
    UpdateDataRequest, UserInfo, UserList,
};
use artifree_contracts::{ServiceDescription, mcp};
use schemars::generate::SchemaSettings;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut generator = SchemaSettings::draft2020_12().into_generator();
    // A vector: Serde serializes arrays of up to 32 elements only.
    let bodies = vec![
        generator.subschema_for::<CreateArtifactRequest>(),
        generator.subschema_for::<SaveRevisionRequest>(),
        generator.subschema_for::<EditFilesRequest>(),
        generator.subschema_for::<RestoreRevisionRequest>(),
        generator.subschema_for::<UpdateArtifactRequest>(),
        generator.subschema_for::<CommitResponse>(),
        generator.subschema_for::<DuplicateArtifactResponse>(),
        generator.subschema_for::<UpdateArtifactResponse>(),
        generator.subschema_for::<PurgeArtifactRequest>(),
        generator.subschema_for::<PurgeArtifactResponse>(),
        generator.subschema_for::<UnreferencedContent>(),
        generator.subschema_for::<RemoveUnreferencedContentRequest>(),
        generator.subschema_for::<RemoveUnreferencedContentResponse>(),
        generator.subschema_for::<Artifact>(),
        generator.subschema_for::<ArtifactList>(),
        generator.subschema_for::<Revision>(),
        generator.subschema_for::<RevisionList>(),
        generator.subschema_for::<Preview>(),
        generator.subschema_for::<RuntimeConnection>(),
        generator.subschema_for::<AgentSessionList>(),
        generator.subschema_for::<StageHandoffRequest>(),
        generator.subschema_for::<HandoffActionRequest>(),
        generator.subschema_for::<Handoff>(),
        generator.subschema_for::<CreateRuntimeViewRequest>(),
        generator.subschema_for::<RuntimeView>(),
        generator.subschema_for::<SetRuntimeGrantRequest>(),
        generator.subschema_for::<RuntimeGrant>(),
        generator.subschema_for::<RuntimeMetadataRequest>(),
        generator.subschema_for::<RuntimeMetadata>(),
        generator.subschema_for::<RuntimeSourceListRequest>(),
        generator.subschema_for::<RuntimeSourceList>(),
        generator.subschema_for::<RuntimeSourceReadRequest>(),
        generator.subschema_for::<RuntimeSourcePart>(),
        generator.subschema_for::<RuntimeDownloadRequest>(),
        generator.subschema_for::<RuntimeDownloadOffer>(),
        generator.subschema_for::<RuntimeSourceWriteRequest>(),
        generator.subschema_for::<RuntimeSourceWriteReceipt>(),
        generator.subschema_for::<RoomJoinRequest>(),
        generator.subschema_for::<RoomHeartbeatRequest>(),
        generator.subschema_for::<RoomEventRequest>(),
        generator.subschema_for::<RoomEventReceipt>(),
        generator.subschema_for::<RoomSnapshot>(),
        generator.subschema_for::<ErrorResponse>(),
        generator.subschema_for::<ArtifactChange>(),
        generator.subschema_for::<FeedPosition>(),
        generator.subschema_for::<SourceExportManifest>(),
        generator.subschema_for::<CreateTokenRequest>(),
        generator.subschema_for::<CreateTokenResponse>(),
        generator.subschema_for::<RotateTokenResponse>(),
        generator.subschema_for::<TokenList>(),
        generator.subschema_for::<TokenInfo>(),
        generator.subschema_for::<CreateUserRequest>(),
        generator.subschema_for::<UserInfo>(),
        generator.subschema_for::<UserList>(),
        generator.subschema_for::<EnqueueJobRequest>(),
        generator.subschema_for::<EnqueueJobResponse>(),
        generator.subschema_for::<CancelJobResponse>(),
        generator.subschema_for::<Job>(),
        generator.subschema_for::<JobList>(),
        generator.subschema_for::<AppData>(),
        generator.subschema_for::<DataMembers>(),
        generator.subschema_for::<SetDataRequest>(),
        generator.subschema_for::<UpdateDataRequest>(),
        generator.subschema_for::<DeleteDataRequest>(),
        generator.subschema_for::<DataWriteResponse>(),
    ];
    // Registers these definitions for generated clients.
    generator.subschema_for::<ListArtifactsQuery>();
    generator.subschema_for::<DuplicateArtifactRequest>();
    generator.subschema_for::<ListJobsQuery>();
    // The read query has one optional field, so the root would accept any
    // empty object.
    generator.subschema_for::<ReadDataQuery>();
    generator.subschema_for::<ReadDataMembersQuery>();
    // The cancel body has no fields, so the root would accept any empty object.
    generator.subschema_for::<CancelJobRequest>();
    let api = serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://artifree.invalid/schemas/api-v1.json",
        "title": "ApiBody",
        "description": "Any JSON request or response body of HTTP API v1, the data of a change feed event, or the manifest of a source export.",
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
