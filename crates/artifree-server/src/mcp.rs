//! Inbound MCP adapter: the agent-facing artifact tools over MCP Streamable
//! HTTP at [`MCP_PATH`] on the API listener (decision 0004).
//!
//! Each tool calls the same `operations` as HTTP API v1, so validation, owner
//! checks, revision conflicts, retry keys, exact bytes, and error codes are
//! the same. The endpoint authenticates the bearer token before the MCP SDK
//! sees a request, then serves statelessly: no MCP session outlives a request,
//! so retries rely on the tools' idempotency keys, not on transport state.
//!
//! These tools are for agents. They are not runtime capabilities of a
//! previewed page, which has none.

use std::sync::Arc;

use artifree_contracts::api::{
    CommitResponse, ContentEncoding, CreateArtifactRequest, DuplicateArtifactRequest,
    DuplicateArtifactResponse, ErrorResponse, ListArtifactsQuery, RestoreRevisionRequest,
    SaveRevisionRequest, UpdateArtifactRequest,
};
use artifree_contracts::mcp::{
    self, ArtifactArguments, CreateArtifactArguments, DuplicateArtifactArguments, FileArguments,
    FileContent, RestoreRevisionArguments, RevisionArguments, SaveRevisionArguments,
    ToolDefinition, UpdateArtifactArguments,
};
use artifree_core::artifacts::ArtifactError;
use artifree_domain::identity::ActorId;
use axum::Router;
use axum::http::request::Parts;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use rmcp::model::{
    CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock,
    Implementation, JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities,
    ServerConfig, Tool, ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::http::{AppState, Authenticated, MAX_BODY_BYTES};
use crate::operations::{self, ApiError};

/// Path of the MCP endpoint on the API listener.
pub const MCP_PATH: &str = "/mcp";

/// How long clients may reuse a `tools/list` answer. The catalog changes only
/// with the server build.
const TOOL_LIST_TTL_MS: u64 = 5 * 60 * 1000;

/// The MCP endpoint. The API router authenticates and admits every request
/// before it reaches the SDK (`http::router`).
///
/// The SDK also rejects a `Host` other than loopback names, which stops DNS
/// rebinding, and any request carrying an `Origin`: MCP clients are not web
/// pages, and the web workspace does not use this endpoint.
pub(crate) fn routes(state: AppState) -> Router<AppState> {
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_max_request_body_bytes(MAX_BODY_BYTES)
        .enforce_origin_validation();
    let service = StreamableHttpService::new(
        move || {
            Ok(ArtifactTools {
                state: state.clone(),
            })
        },
        Arc::new(NeverSessionManager::default()),
        config,
    );
    Router::new().route_service(MCP_PATH, service)
}

/// The MCP server for one request.
#[derive(Debug, Clone)]
struct ArtifactTools {
    state: AppState,
}

impl ServerHandler for ArtifactTools {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("artifree", env!("CARGO_PKG_VERSION")).with_title("Artifree"),
            )
            .with_instructions(mcp::SERVER_INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let tools = mcp::tools()
            .into_iter()
            .map(tool)
            .collect::<Result<Vec<Tool>, ErrorData>>()?;
        Ok(ListToolsResult::with_all_items(tools)
            .with_ttl_ms(TOOL_LIST_TTL_MS)
            .with_cache_scope(CacheScope::Public))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        // The API router's admission layer inserts the actor; the SDK
        // forwards the HTTP request parts. Without both, nothing may run.
        let actor = context
            .extensions
            .get::<Parts>()
            .and_then(|parts| parts.extensions.get::<Authenticated>())
            .map(|Authenticated(actor)| actor.clone())
            .ok_or_else(|| ErrorData::internal_error("unauthenticated tool call", None))?;
        let arguments = Value::Object(request.arguments.unwrap_or_default());
        match dispatch(&self.state, actor, &request.name, arguments).await {
            Some(Ok(value)) => Ok(CallToolResult::structured(value).into()),
            Some(Err(error)) => Ok(failure(&error).into()),
            // An unknown tool is a protocol error; everything a tool rejects
            // is a tool error the model can read and correct.
            None => Err(ErrorData::invalid_params(
                format!("unknown tool: {}", request.name),
                None,
            )),
        }
    }
}

/// Runs the named tool, or returns `None` for an unknown name.
async fn dispatch(
    state: &AppState,
    actor: ActorId,
    name: &str,
    arguments: Value,
) -> Option<Result<Value, ApiError>> {
    let outcome = match name {
        "list_artifacts" => {
            json(async {
                let query: ListArtifactsQuery = parse(arguments)?;
                operations::list(state, actor, query).await
            })
            .await
        }
        "get_artifact" => {
            json(async {
                let ArtifactArguments { artifact_id } = parse(arguments)?;
                operations::show(state, actor, &artifact_id).await
            })
            .await
        }
        "create_artifact" => {
            json(async {
                let CreateArtifactArguments {
                    title,
                    files,
                    idempotency_key,
                } = parse(arguments)?;
                let key = operations::request_key("idempotency_key", &idempotency_key)?;
                operations::create(state, actor, key, CreateArtifactRequest { title, files }).await
            })
            .await
        }
        "save_revision" => {
            json(async {
                let SaveRevisionArguments {
                    artifact_id,
                    base_revision,
                    changes,
                    idempotency_key,
                } = parse(arguments)?;
                let key = operations::request_key("idempotency_key", &idempotency_key)?;
                let request = SaveRevisionRequest {
                    base_revision,
                    changes,
                };
                operations::save(state, actor, key, &artifact_id, request).await
            })
            .await
        }
        "restore_revision" => json(restore_revision(state, actor, arguments)).await,
        "duplicate_artifact" => json(duplicate_artifact(state, actor, arguments)).await,
        "update_artifact" => {
            json(async {
                let UpdateArtifactArguments {
                    artifact_id,
                    base_metadata_revision,
                    title,
                    pinned,
                    deleted,
                    idempotency_key,
                } = parse(arguments)?;
                let key = operations::request_key("idempotency_key", &idempotency_key)?;
                let request = UpdateArtifactRequest {
                    base_metadata_revision,
                    title,
                    pinned,
                    deleted,
                };
                operations::update(state, actor, key, &artifact_id, request).await
            })
            .await
        }
        "list_revisions" => {
            json(async {
                let ArtifactArguments { artifact_id } = parse(arguments)?;
                operations::revisions(state, actor, &artifact_id).await
            })
            .await
        }
        "get_revision" => {
            json(async {
                let RevisionArguments {
                    artifact_id,
                    revision,
                } = parse(arguments)?;
                operations::revision(state, actor, &artifact_id, revision).await
            })
            .await
        }
        "read_file" => json(read_file(state, actor, arguments)).await,
        "create_preview" => {
            json(async {
                let RevisionArguments {
                    artifact_id,
                    revision,
                } = parse(arguments)?;
                operations::preview(state, actor, &artifact_id, revision).await
            })
            .await
        }
        _ => return None,
    };
    Some(outcome)
}

/// Parses tool arguments. A mismatch is a tool error, not a protocol error,
/// so the calling model sees the reason.
fn parse<T: DeserializeOwned>(arguments: Value) -> Result<T, ApiError> {
    serde_json::from_value(arguments)
        .map_err(|error| ApiError::invalid(format!("arguments: {error}")))
}

async fn json<T: Serialize>(
    outcome: impl Future<Output = Result<T, ApiError>>,
) -> Result<Value, ApiError> {
    let value = outcome.await?;
    serde_json::to_value(value)
        .map_err(|error| ArtifactError::Unavailable(format!("result encoding: {error}")).into())
}

async fn restore_revision(
    state: &AppState,
    actor: ActorId,
    arguments: Value,
) -> Result<CommitResponse, ApiError> {
    let RestoreRevisionArguments {
        artifact_id,
        revision,
        base_revision,
        idempotency_key,
    } = parse(arguments)?;
    let key = operations::request_key("idempotency_key", &idempotency_key)?;
    let request = RestoreRevisionRequest { base_revision };
    operations::restore(state, actor, key, &artifact_id, revision, request).await
}

async fn duplicate_artifact(
    state: &AppState,
    actor: ActorId,
    arguments: Value,
) -> Result<DuplicateArtifactResponse, ApiError> {
    let DuplicateArtifactArguments {
        artifact_id,
        revision,
        title,
        idempotency_key,
    } = parse(arguments)?;
    let key = operations::request_key("idempotency_key", &idempotency_key)?;
    let request = DuplicateArtifactRequest { title };
    operations::duplicate(state, actor, key, &artifact_id, revision, request).await
}

async fn read_file(
    state: &AppState,
    actor: ActorId,
    arguments: Value,
) -> Result<FileContent, ApiError> {
    let FileArguments {
        artifact_id,
        revision,
        path,
    } = parse(arguments)?;
    let file = operations::read_file(state, actor, &artifact_id, revision, &path).await?;
    let size = file.content.len() as u64;
    // Valid UTF-8 travels as text; JSON strings carry every Unicode scalar
    // value, so both forms reproduce the stored bytes exactly.
    let (encoding, content) = match String::from_utf8(file.content) {
        Ok(text) => (ContentEncoding::Utf8, text),
        Err(error) => (ContentEncoding::Base64, BASE64.encode(error.into_bytes())),
    };
    Ok(FileContent {
        media_type: file.path.media_type().essence().to_owned(),
        path: file.path.to_string(),
        revision: file.revision.0,
        size,
        encoding,
        content,
    })
}

/// The error tool result: the HTTP error body with `isError: true`.
fn failure(error: &ApiError) -> CallToolResult {
    let body = ErrorResponse {
        error: error.body.clone(),
    };
    match serde_json::to_value(body) {
        Ok(value) => CallToolResult::structured_error(value),
        Err(_) => CallToolResult::error(vec![ContentBlock::text(error.body.message.clone())]),
    }
}

fn tool(definition: ToolDefinition) -> Result<Tool, ErrorData> {
    let object = |schema: Value| -> Result<Arc<JsonObject>, ErrorData> {
        match schema {
            Value::Object(object) => Ok(Arc::new(object)),
            _ => Err(ErrorData::internal_error(
                "tool schema is not an object",
                None,
            )),
        }
    };
    let effect = definition.effect;
    Ok(Tool::new(
        definition.name,
        definition.description,
        object(definition.input_schema.into())?,
    )
    .with_title(definition.title)
    .with_raw_output_schema(object(definition.output_schema.into())?)
    .with_annotations(
        ToolAnnotations::new()
            .read_only(effect.read_only())
            .destructive(false)
            .idempotent(effect.idempotent())
            .open_world(false),
    ))
}
