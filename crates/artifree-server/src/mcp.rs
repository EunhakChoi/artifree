//! Inbound MCP adapter: the agent-facing artifact tools over MCP Streamable
//! HTTP at [`MCP_PATH`] on the API listener (decision 0004).
//!
//! Each tool calls the same `operations` as HTTP API v1, so validation, owner
//! checks, revision conflicts, retry keys, exact bytes, and error codes are
//! the same. The endpoint authenticates the bearer token before the MCP SDK
//! sees a request, then serves statelessly: no MCP session outlives a request,
//! so retries rely on the tools' idempotency keys, not on transport state.
//!
//! These tools are for agents, separate from the metadata-only runtime
//! bridge of an opted-in workspace preview.

use std::sync::Arc;

use artifree_contracts::api::ErrorCode;
use artifree_contracts::api::{
    CancelJobResponse, CommitResponse, ContentEncoding, CreateArtifactRequest, DeleteDataRequest,
    DuplicateArtifactRequest, DuplicateArtifactResponse, EditFilesRequest, EnqueueJobRequest,
    EnqueueJobResponse, ErrorResponse, JsonValue, ListArtifactsQuery, ListJobsQuery,
    RestoreRevisionRequest, SaveRevisionRequest, SetDataRequest, UpdateArtifactRequest,
    UpdateArtifactResponse, UpdateDataRequest,
};
use artifree_contracts::mcp::{
    self, AcknowledgeHandoffArguments, ArtifactArguments, CancelJobArguments,
    CreateArtifactArguments, DeleteDataArguments, DuplicateArtifactArguments, EditFilesArguments,
    EnqueueJobArguments, FileArguments, FileContent, GetDataArguments, JobArguments,
    ListJobsArguments, PollHandoffArguments, RegisterHandoffSessionArguments,
    RestoreRevisionArguments, RevisionArguments, SaveRevisionArguments, SetDataArguments,
    ToolDefinition, UpdateArtifactArguments, UpdateDataArguments,
};
use artifree_core::artifacts::ArtifactError;
use artifree_core::parts::{Boundary, ByteRange};
use artifree_domain::identity::{ActorId, StateGeneration};
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
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

use crate::auth::{AuthContext, hex};
use crate::http::{AppState, MAX_BODY_BYTES};
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
    let mut config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_max_request_body_bytes(MAX_BODY_BYTES)
        .enforce_origin_validation();

    if state.settings().allow_remote_mcp || !state.settings().allowed_hosts.is_empty() {
        let mut hosts = config.allowed_hosts.clone();
        for host in &state.settings().allowed_hosts {
            let normalized = crate::proxy::normalize_host(host);
            if !hosts.contains(&normalized) {
                hosts.push(normalized);
            }
        }
        config = config.with_allowed_hosts(hosts);
    }

    let service = StreamableHttpService::new(
        {
            let state = state.clone();
            move || {
                Ok(ArtifactTools {
                    state: state.clone(),
                })
            }
        },
        Arc::new(NeverSessionManager::default()),
        config,
    );
    Router::new()
        .route_service(MCP_PATH, service)
        .route_layer(middleware::from_fn_with_state(state, enforce_mcp_access))
}

/// Returns whether the resolved client is known and loopback. Missing or
/// unknown connection information is not loopback (fail closed).
fn is_known_loopback(info: Option<&crate::proxy::ClientConnectionInfo>) -> bool {
    info.and_then(|info| info.client_ip)
        .is_some_and(|ip| ip.to_canonical().is_loopback())
}

/// Enforces loopback or explicitly enabled remote access to the MCP endpoint.
async fn enforce_mcp_access(
    State(state): State<AppState>,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    // Only the proxy-resolved client counts. A request that never went through
    // `proxy_inspection` has no `ClientConnectionInfo` and is treated as unknown.
    let is_loopback_client =
        is_known_loopback(req.extensions().get::<crate::proxy::ClientConnectionInfo>());
    let is_allowed = state.settings().allow_remote_mcp || is_loopback_client;

    if !is_allowed {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            ErrorCode::InvalidRequest,
            "MCP endpoint is restricted to loopback clients only",
        )
        .into_response();
    }

    next.run(req).await
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
        // The API router's admission layer inserts the auth context; the SDK
        // forwards the HTTP request parts. Without both, nothing may run.
        let auth = context
            .extensions
            .get::<Parts>()
            .and_then(|parts| parts.extensions.get::<AuthContext>())
            .cloned()
            .ok_or_else(|| ErrorData::internal_error("unauthenticated tool call", None))?;
        let arguments = Value::Object(request.arguments.unwrap_or_default());
        match dispatch(&self.state, auth, &request.name, arguments).await {
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

/// Runs the named tool, or `None` for an unknown name; crate-visible for the race tests.
pub(crate) async fn dispatch(
    state: &AppState,
    auth: AuthContext,
    name: &str,
    arguments: Value,
) -> Option<Result<Value, ApiError>> {
    let actor = auth.actor.clone();
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
        "create_artifact" => json(create_artifact(state, actor, arguments)).await,
        "save_revision" => json(save_revision(state, actor, arguments)).await,
        "edit_files" => json(edit_files(state, actor, arguments)).await,
        "restore_revision" => json(restore_revision(state, actor, arguments)).await,
        "duplicate_artifact" => json(duplicate_artifact(state, actor, arguments)).await,
        "update_artifact" => json(update_artifact(state, actor, arguments)).await,
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
                operations::preview(
                    state,
                    actor,
                    auth.token_id,
                    auth.generation,
                    &artifact_id,
                    revision,
                )
                .await
            })
            .await
        }
        _ => return dispatch_jobs(state, actor, auth.generation, name, arguments).await,
    };
    Some(outcome)
}

/// Routes the application data tools (decision 0031): the same operations as
/// the data routes of HTTP, so keys, access, errors, and limits are the same.
async fn dispatch_data(
    state: &AppState,
    actor: ActorId,
    generation: StateGeneration,
    name: &str,
    arguments: Value,
) -> Option<Result<Value, ApiError>> {
    let outcome = match name {
        "get_data" => {
            json(async {
                let GetDataArguments { artifact_id, path } = parse(arguments)?;
                operations::read_data(state, actor, &artifact_id, path.as_deref()).await
            })
            .await
        }
        "set_data" => {
            json(async {
                let SetDataArguments {
                    artifact_id,
                    path,
                    base_data_version,
                    value,
                    idempotency_key,
                } = parse(arguments)?;
                let key = operations::request_key("idempotency_key", &idempotency_key)?;
                let value = json_text("value", value)?;
                let request = SetDataRequest {
                    path,
                    base_data_version,
                    value,
                };
                operations::set_data(state, actor, key, &artifact_id, request, None).await
            })
            .await
        }
        "update_data" => {
            json(async {
                let UpdateDataArguments {
                    artifact_id,
                    path,
                    base_data_version,
                    patch,
                    idempotency_key,
                } = parse(arguments)?;
                let key = operations::request_key("idempotency_key", &idempotency_key)?;
                let patch = json_text("patch", patch)?;
                let request = UpdateDataRequest {
                    path,
                    base_data_version,
                    patch,
                };
                operations::update_data(state, actor, key, &artifact_id, request, None).await
            })
            .await
        }
        "delete_data" => {
            json(async {
                let DeleteDataArguments {
                    artifact_id,
                    path,
                    base_data_version,
                    idempotency_key,
                } = parse(arguments)?;
                let key = operations::request_key("idempotency_key", &idempotency_key)?;
                let request = DeleteDataRequest {
                    path,
                    base_data_version,
                };
                operations::delete_data(state, actor, key, &artifact_id, request, None).await
            })
            .await
        }
        _ => return dispatch_handoffs(state, actor, generation, name, arguments).await,
    };
    Some(outcome)
}

/// A JSON value given as text, as the tools of application data take it. Text
/// that is not JSON at all is `invalid_request` as a malformed body is over
/// HTTP (`400`); text that is JSON but breaks the rules of application data is
/// refused later, by the same code as over HTTP (decision 0031).
fn json_text(field: &str, text: String) -> Result<JsonValue, ApiError> {
    JsonValue::from_text(text)
        .ok_or_else(|| ApiError::invalid(format!("{field}: not valid JSON text")))
}

/// Routes the job tools (decision 0030): the same operations as the job
/// routes of HTTP, so keys, access, errors, and limits are the same.
async fn dispatch_jobs(
    state: &AppState,
    actor: ActorId,
    generation: StateGeneration,
    name: &str,
    arguments: Value,
) -> Option<Result<Value, ApiError>> {
    let outcome = match name {
        "enqueue_job" => json(enqueue_job(state, actor, arguments)).await,
        "get_job" => {
            json(async {
                let JobArguments { job_id } = parse(arguments)?;
                operations::show_job(state, actor, &job_id).await
            })
            .await
        }
        "list_jobs" => {
            json(async {
                let ListJobsArguments {
                    artifact_id,
                    limit,
                    page_token,
                } = parse(arguments)?;
                let query = ListJobsQuery { limit, page_token };
                operations::list_jobs(state, actor, artifact_id.as_deref(), query).await
            })
            .await
        }
        "cancel_job" => json(cancel_job(state, actor, arguments)).await,
        _ => return dispatch_data(state, actor, generation, name, arguments).await,
    };
    Some(outcome)
}

/// Routes the separate agent inbox protocol without assuming a page callback.
async fn dispatch_handoffs(
    state: &AppState,
    actor: ActorId,
    generation: StateGeneration,
    name: &str,
    arguments: Value,
) -> Option<Result<Value, ApiError>> {
    let outcome = match name {
        "register_handoff_session" => {
            json(async {
                let RegisterHandoffSessionArguments { name } = parse(arguments)?;
                operations::register_confirmed_handoff_session(state, &actor, generation, &name)
                    .await
            })
            .await
        }
        "poll_handoffs" => {
            json(async {
                let PollHandoffArguments {
                    session_id,
                    session_secret,
                } = parse(arguments)?;
                operations::poll_handoffs(state, actor, &session_id, &session_secret).await
            })
            .await
        }
        "acknowledge_handoff" => {
            json(async {
                let AcknowledgeHandoffArguments {
                    session_id,
                    session_secret,
                    delivery_id,
                } = parse(arguments)?;
                operations::acknowledge_handoff(
                    state,
                    &actor,
                    &session_id,
                    &session_secret,
                    &delivery_id,
                )
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

async fn create_artifact(
    state: &AppState,
    actor: ActorId,
    arguments: Value,
) -> Result<CommitResponse, ApiError> {
    let CreateArtifactArguments {
        title,
        files,
        idempotency_key,
    } = parse(arguments)?;
    let key = operations::request_key("idempotency_key", &idempotency_key)?;
    operations::create(state, actor, key, CreateArtifactRequest { title, files }).await
}

async fn save_revision(
    state: &AppState,
    actor: ActorId,
    arguments: Value,
) -> Result<CommitResponse, ApiError> {
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
}

async fn edit_files(
    state: &AppState,
    actor: ActorId,
    arguments: Value,
) -> Result<CommitResponse, ApiError> {
    let EditFilesArguments {
        artifact_id,
        base_revision,
        edits,
        idempotency_key,
    } = parse(arguments)?;
    let key = operations::request_key("idempotency_key", &idempotency_key)?;
    let request = EditFilesRequest {
        base_revision,
        edits,
    };
    operations::edit_files(state, actor, key, &artifact_id, request).await
}

async fn enqueue_job(
    state: &AppState,
    actor: ActorId,
    arguments: Value,
) -> Result<EnqueueJobResponse, ApiError> {
    let EnqueueJobArguments {
        artifact_id,
        revision,
        kind,
        idempotency_key,
    } = parse(arguments)?;
    let key = operations::request_key("idempotency_key", &idempotency_key)?;
    let request = EnqueueJobRequest { kind, revision };
    operations::enqueue_job(state, actor, key, &artifact_id, request).await
}

async fn cancel_job(
    state: &AppState,
    actor: ActorId,
    arguments: Value,
) -> Result<CancelJobResponse, ApiError> {
    let CancelJobArguments {
        job_id,
        idempotency_key,
    } = parse(arguments)?;
    let key = operations::request_key("idempotency_key", &idempotency_key)?;
    operations::cancel_job(state, actor, key, &job_id).await
}

async fn update_artifact(
    state: &AppState,
    actor: ActorId,
    arguments: Value,
) -> Result<UpdateArtifactResponse, ApiError> {
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
        copy_data,
        idempotency_key,
    } = parse(arguments)?;
    let key = operations::request_key("idempotency_key", &idempotency_key)?;
    let request = DuplicateArtifactRequest { title, copy_data };
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
        offset,
        length,
    } = parse(arguments)?;
    // A part of a UTF-8 file ends between characters, so that each part is
    // text and the parts join into the file (decision 0016).
    let range = ByteRange::From {
        offset: offset.unwrap_or(0),
        length,
    };
    let file = operations::read_file(
        state,
        actor,
        &artifact_id,
        revision,
        &path,
        range,
        Boundary::Character,
    )
    .await?;
    let part = file.part;
    let next_offset = part.next_offset();
    // Parts of a UTF-8 file travel as text, and parts of any other file as
    // base64, even where the bytes of the part alone would be UTF-8. JSON
    // strings carry every Unicode scalar value, so both forms reproduce the
    // stored bytes exactly.
    let (encoding, content) = match String::from_utf8(part.content) {
        Ok(text) if part.text => (ContentEncoding::Utf8, text),
        Ok(text) => (ContentEncoding::Base64, BASE64.encode(text)),
        Err(error) => (ContentEncoding::Base64, BASE64.encode(error.into_bytes())),
    };
    Ok(FileContent {
        media_type: file.path.media_type().essence().to_owned(),
        path: file.path.to_string(),
        revision: file.revision.0,
        size: part.size,
        sha256: hex(&part.sha256),
        offset: part.offset,
        next_offset,
        encoding,
        content,
    })
}

/// The error tool result: the HTTP error body with `isError: true`.
fn failure(error: &ApiError) -> CallToolResult {
    let body = ErrorResponse {
        error: (*error.body).clone(),
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
            .destructive(effect.destructive())
            .idempotent(effect.idempotent())
            .open_world(false),
    ))
}

#[cfg(test)]
mod access_tests {
    use super::*;
    use crate::proxy::ClientConnectionInfo;

    fn info(client_ip: Option<&str>) -> ClientConnectionInfo {
        ClientConnectionInfo {
            client_ip: client_ip.and_then(|ip| ip.parse().ok()),
            host: String::new(),
            proto: "http".to_owned(),
            is_proxied: false,
        }
    }

    #[test]
    fn only_a_known_loopback_client_is_loopback() {
        assert!(is_known_loopback(Some(&info(Some("127.0.0.1")))));
        assert!(is_known_loopback(Some(&info(Some("::1")))));
        assert!(is_known_loopback(Some(&info(Some("::ffff:127.0.0.1")))));
        assert!(!is_known_loopback(Some(&info(Some("203.0.113.9")))));
        assert!(!is_known_loopback(Some(&info(None))), "unknown client");
        assert!(
            !is_known_loopback(None),
            "a request that skipped proxy inspection must fail closed"
        );
    }
}
