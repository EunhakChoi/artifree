//! Inbound HTTP API v1. Routing, authentication, and HTTP framing live here;
//! wire conversion is shared with MCP in `operations`, and application
//! behavior stays in `artifree-core`.
//!
//! Every route requires `Authorization: Bearer <token>`. Artifact mutations
//! require an `Idempotency-Key` header. Ephemeral runtime connections and
//! version-guarded grants have their own retry contracts. Storage work runs on Tokio's blocking
//! pool because the core and store are synchronous. The router also mounts
//! the MCP endpoint (`crate::mcp`) and the change feed (`crate::changes`)
//! behind the same token.

use std::future::poll_fn;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use artifree_contracts::api::{
    AgentSessionList, AppData, Artifact, ArtifactList, CancelJobRequest, CancelJobResponse,
    CreateArtifactRequest, CreateRuntimeViewRequest, DataMembers, DataWriteResponse, DeleteDataRequest,
    DuplicateArtifactRequest, EditFilesRequest, EnqueueJobRequest, EnqueueJobResponse, ErrorCode,
    ErrorResponse, Handoff, HandoffActionRequest, Job, JobList, ListArtifactsQuery,
    ListDataMembersQuery, ListJobsQuery,
    PurgeArtifactRequest, PurgeArtifactResponse, ReadDataQuery, RemoveUnreferencedContentRequest,
    RemoveUnreferencedContentResponse, RestoreRevisionRequest, Revision, RevisionList,
    RoomEventReceipt, RoomEventRequest, RoomHeartbeatRequest, RoomJoinRequest, RoomSnapshot,
    RuntimeConnection, RuntimeDownloadOffer, RuntimeDownloadRequest, RuntimeGrant, RuntimeMetadata,
    RuntimeMetadataRequest, RuntimeSourceList, RuntimeSourceListRequest, RuntimeSourcePart,
    RuntimeSourceReadRequest, RuntimeSourceWriteReceipt, RuntimeSourceWriteRequest,
    SaveRevisionRequest, SetDataRequest, SetRuntimeGrantRequest, StageHandoffRequest,
    UnreferencedContent, UpdateArtifactRequest, UpdateArtifactResponse, UpdateDataRequest,
};
use artifree_core::artifacts::{ArtifactError, ArtifactService};
use artifree_core::jobs::{Clock, SystemClock};
use artifree_core::parts::{Boundary, ByteRange};
use artifree_domain::identity::{ActorId, RequestKey, StateGeneration};
use artifree_ports::token_store::TokenStore;
use artifree_ports::user_store::UserStore;
use artifree_providers::storage::SqliteStore;
use artifree_providers::tokens::{SqliteTokenStore, hash_token_secret};
use axum::body::{Body, Bytes, HttpBody};
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{ConnectInfo, DefaultBodyLimit, FromRequestParts, Path, Query, Request, State};
use axum::http::header::{self, HeaderValue};
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};

use crate::auth::{AccessToken, AuthContext};
use crate::changes::{self, Feed};
use crate::data_events::{self as data_events, DataStreamSettings, DataStreams};
use crate::handoffs::Handoffs;
use crate::limits::{Limiter, Refusal, RequestLimits, Slots};
use crate::mcp;
use crate::operations::{self, ApiError};
use crate::origin::normalize_origin;
use crate::preview::{self, PreviewTickets};
use crate::ranges::{self, Requested};
use crate::rooms::{self, ProcessClock, RoomClock, RoomHub, RoomNames};
use crate::runtime_sessions::RuntimeSessions;
use crate::runtime_source::{self, FrameCall};
use crate::tokens;
use crate::users;
use crate::web::WebAssets;

/// Largest accepted request body: a maximum package in base64 plus JSON
/// framing. Clients send a file as UTF-8 text only when its JSON escaping is
/// no larger than base64, so this bound holds for either encoding.
pub const MAX_BODY_BYTES: usize = 36 * 1024 * 1024;

/// Largest accepted body of a write to application data (decision 0031): a
/// value or patch of at most [`artifree_domain::data::MAX_VALUE_BYTES`] bytes
/// of JSON, the path, and the version. A request this large still fails the
/// limits of the data itself when the value is over its own bound.
pub const MAX_DATA_BODY_BYTES: usize = artifree_domain::data::MAX_VALUE_BYTES + 8 * 1024;

/// Server settings that adapters need at request time.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Origin of the preview content server, such as `http://127.0.0.1:7411`.
    /// It must differ from the API and web origins.
    pub content_origin: String,
    /// Lifetime of a preview link.
    pub preview_ttl: Duration,
    /// CSP `frame-ancestors` source list allowed to embed previews.
    pub frame_ancestors: String,
    /// Per-actor request budget shared by HTTP and MCP.
    pub limits: RequestLimits,
    /// Per-actor budget for the requests a runtime frame causes (decision
    /// 0032). It is counted apart from [`Settings::limits`].
    pub frame_limits: RequestLimits,
    /// Per-actor budget for room calls (decision 0034), counted apart from
    /// [`Settings::limits`] and [`Settings::frame_limits`].
    pub room_limits: RequestLimits,
    /// The monotonic clock leases are measured on. Tests replace it.
    pub room_clock: Arc<dyn RoomClock>,
    /// Change streams one actor may keep open, at least 1.
    pub change_streams: u32,
    /// Limits and timings of data streams (decision 0040).
    pub data_streams: DataStreamSettings,
    /// Optional public API and web origin, such as `https://artifree.example.com`.
    pub api_origin: Option<String>,
    /// Trusted reverse proxy networks.
    pub trusted_proxies: Vec<crate::proxy::IpNetwork>,
    /// Allowed Host header values.
    pub allowed_hosts: Vec<String>,
    /// Additional allowed CORS origins for the web interface.
    pub allowed_origins: Vec<String>,
    /// Whether remote MCP access is permitted.
    pub allow_remote_mcp: bool,
    /// The clock that stamps jobs (decision 0030). Tests replace it.
    pub clock: Arc<dyn Clock>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            content_origin: "http://127.0.0.1:7411".into(),
            preview_ttl: Duration::from_secs(15 * 60),
            frame_ancestors: "http://127.0.0.1:* http://localhost:* http://[::1]:*".into(),
            limits: RequestLimits::default(),
            frame_limits: RequestLimits::FRAME,
            room_limits: RequestLimits::ROOM,
            room_clock: Arc::new(ProcessClock::new()),
            change_streams: crate::changes::DEFAULT_STREAMS_PER_ACTOR,
            data_streams: DataStreamSettings::default(),
            api_origin: None,
            trusted_proxies: Vec::new(),
            allowed_hosts: Vec::new(),
            allowed_origins: Vec::new(),
            allow_remote_mcp: false,
            clock: Arc::new(SystemClock),
        }
    }
}

/// A weak view of everything that can still use the state's databases: the
/// state itself (and its clones) and every blocking task a request started.
/// It does not keep them alive.
#[derive(Debug, Clone)]
pub struct DatabaseUsers(std::sync::Weak<SqliteTokenStore>);

impl DatabaseUsers {
    /// How many holders remain. Zero means that no database handle of the
    /// state exists and none can be opened again through it.
    #[must_use]
    pub fn count(&self) -> usize {
        self.0.strong_count()
    }

    #[cfg(test)]
    pub(crate) fn of(store: &Arc<SqliteTokenStore>) -> Self {
        Self(Arc::downgrade(store))
    }
}

/// Shared request state for the API and content routers.
#[derive(Debug, Clone)]
pub struct AppState {
    shared: Arc<Shared>,
}

#[derive(Debug)]
struct Shared {
    artifacts: ArtifactService<SqliteStore>,
    token_store: Arc<SqliteTokenStore>,
    token: AccessToken,
    started_at: String,
    previews: PreviewTickets,
    runtime_sessions: RuntimeSessions,
    handoffs: Handoffs,
    limiter: Limiter,
    frame_limiter: Limiter,
    room_limiter: Limiter,
    rooms: RoomHub,
    /// Downloads being built, per actor (decision 0032).
    download_builds: Slots,
    feed: Feed,
    data_streams: DataStreams,
    draining: AtomicUsize,
    settings: Settings,
    content_policy: HeaderValue,
}

impl AppState {
    /// Combines the application service, the token store, the local token, and settings.
    ///
    /// # Errors
    ///
    /// When the content origin or frame ancestors cannot form a header, or
    /// a limit is zero.
    pub fn new(
        store: SqliteStore,
        token_store: SqliteTokenStore,
        token: AccessToken,
        settings: Settings,
    ) -> Result<Self, String> {
        let mut settings = settings;
        // These are the same checks the command line applies; repeating them
        // keeps a caller of the library from reaching the allow-list or the
        // preview policy with an origin that was never validated.
        if !settings.content_origin.is_empty() {
            settings.content_origin = normalize_origin(&settings.content_origin)
                .map_err(|error| format!("preview content origin: {error}"))?;
        }
        if let Some(ref api_origin) = settings.api_origin {
            let canon =
                normalize_origin(api_origin).map_err(|error| format!("API origin: {error}"))?;
            if !settings.content_origin.is_empty() && canon == settings.content_origin {
                return Err(
                    "preview content origin must differ from API origin to maintain sandbox isolation"
                        .into(),
                );
            }
            settings.api_origin = Some(canon);
        }
        let mut allowed_origins = Vec::with_capacity(settings.allowed_origins.len());
        for entry in &settings.allowed_origins {
            let canon = normalize_origin(entry)
                .map_err(|error| format!("an allowed CORS origin: {error}"))?;
            if !allowed_origins.contains(&canon) {
                allowed_origins.push(canon);
            }
        }
        settings.allowed_origins = allowed_origins;
        let content_policy =
            preview::content_policy(&settings.content_origin, &settings.frame_ancestors)?;
        if settings.change_streams == 0 {
            return Err("change streams per actor must be at least 1".into());
        }
        settings.data_streams = settings.data_streams.validate()?;
        let started_at = crate::tokens::iso_now();
        Ok(Self {
            shared: Arc::new(Shared {
                artifacts: ArtifactService::new(store),
                token_store: Arc::new(token_store),
                token,
                started_at,
                previews: PreviewTickets::new(settings.preview_ttl),
                runtime_sessions: RuntimeSessions::new(Duration::from_secs(24 * 60 * 60)),
                handoffs: Handoffs::new(Duration::from_secs(60 * 60)),
                limiter: Limiter::new(settings.limits.validate()?),
                frame_limiter: Limiter::new(settings.frame_limits.validate()?),
                room_limiter: Limiter::new(settings.room_limits.validate()?),
                rooms: RoomHub::new(Arc::clone(&settings.room_clock)),
                download_builds: Slots::new(crate::runtime_source::MAX_DOWNLOAD_BUILDS),
                feed: Feed::new(settings.change_streams),
                data_streams: DataStreams::new(settings.data_streams),
                draining: AtomicUsize::new(0),
                settings,
                content_policy,
            }),
        })
    }

    /// A view of the holders of this state's databases, for a caller that must
    /// not delete the database files while a blocking task still uses them.
    ///
    /// The identity store is held by the state and by every blocking task that
    /// uses it or the shared state (each `spawn_blocking` closure of this
    /// module clones one of the two), so its count reaches zero only when no
    /// such task is left.
    #[must_use]
    pub fn database_users(&self) -> DatabaseUsers {
        DatabaseUsers(Arc::downgrade(&self.shared.token_store))
    }

    /// Ends open change streams so that a graceful shutdown need not wait
    /// for their readers. Streams opened afterwards end at once.
    pub fn close_streams(&self) {
        self.shared.feed.close();
    }

    /// The identity store, for tests that change the registry directly; the
    /// server's own code reaches it through [`Self::identity`].
    #[cfg(test)]
    pub(crate) fn token_store(&self) -> &SqliteTokenStore {
        &self.shared.token_store
    }

    pub(crate) fn owner_token(&self) -> &AccessToken {
        &self.shared.token
    }

    pub(crate) fn started_at(&self) -> &str {
        &self.shared.started_at
    }

    pub(crate) fn feed(&self) -> &Feed {
        &self.shared.feed
    }

    pub(crate) fn data_streams(&self) -> &DataStreams {
        &self.shared.data_streams
    }

    pub(crate) fn download_builds(&self) -> &Slots {
        &self.shared.download_builds
    }

    pub(crate) fn previews(&self) -> &PreviewTickets {
        &self.shared.previews
    }

    pub(crate) fn runtime_sessions(&self) -> &RuntimeSessions {
        &self.shared.runtime_sessions
    }

    pub(crate) fn rooms(&self) -> &RoomHub {
        &self.shared.rooms
    }

    pub(crate) fn handoffs(&self) -> &Handoffs {
        &self.shared.handoffs
    }

    pub(crate) fn settings(&self) -> &Settings {
        &self.shared.settings
    }

    pub(crate) fn clock(&self) -> &dyn Clock {
        &*self.shared.settings.clock
    }

    pub(crate) fn content_policy(&self) -> &HeaderValue {
        &self.shared.content_policy
    }

    /// Authenticates the bearer token from request headers. Checks the bootstrap
    /// token first, then queries the active token store. Every HTTP route and
    /// the MCP endpoint authenticate through this check.
    ///
    /// The bootstrap token is the local owner profile and the only credential
    /// with administrative authority. An issued token authenticates as the one
    /// user it belongs to, and only while that user is enabled and the token is
    /// not revoked; every other case, a disabled user's token included, is the
    /// same `401` as a token nobody issued (decision 0036).
    pub(crate) async fn authenticate(&self, headers: &HeaderMap) -> Result<AuthContext, ApiError> {
        let presented = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(bearer_token);
        let Some(token) = presented else {
            return Err(ApiError::unauthenticated());
        };
        if self.shared.token.matches(token) {
            return Ok(AuthContext {
                actor: ActorId::owner(),
                token_id: None,
                generation: StateGeneration::FIRST,
                is_admin: true,
            });
        }
        let hash = hash_token_secret(token);
        let token_store = Arc::clone(&self.shared.token_store);
        let record = tokio::task::spawn_blocking(move || token_store.find_by_hash(&hash))
            .await
            .map_err(|_| ApiError::unavailable("token lookup failed"))?
            .map_err(|err| ApiError::unavailable(err.to_string()))?;

        match record {
            Some(record) if record.is_active() => {
                let token_store = Arc::clone(&self.shared.token_store);
                let id = record.id.clone();
                let now = crate::tokens::iso_now();
                tokio::task::spawn_blocking(move || {
                    let _ = token_store.touch(&id, &now);
                });
                Ok(AuthContext {
                    actor: record.actor,
                    token_id: Some(record.id),
                    generation: record.actor_generation,
                    is_admin: false,
                })
            }
            _ => Err(ApiError::unauthenticated()),
        }
    }

    /// Ends everything the server holds in memory for `actor`: change
    /// streams, preview links and view fences, runtime connections and the
    /// grants on them, room participants, and registered handoff inboxes.
    /// Disabling a user runs it after the registry is changed, and anything
    /// that registers afterwards confirms the user with [`Self::ensure_current`].
    /// Connections close before rooms are emptied, so that a call in flight
    /// cannot join again.
    pub(crate) fn revoke_actor(&self, actor: &ActorId) {
        self.shared.feed.revoke_actor(actor);
        self.shared.previews.revoke_actor(actor);
        self.shared.runtime_sessions.revoke_actor(actor);
        self.shared.rooms.leave_actor(actor);
        self.shared.handoffs.revoke_actor(actor);
    }

    /// Confirms that `actor` is still in the state generation the request
    /// authenticated under: registered, enabled, and not disabled and enabled
    /// again since. A request that authenticated before a user was disabled,
    /// and registers something that outlives the request after the disable
    /// already ran its cleanup ([`Self::revoke_actor`]), calls this **after**
    /// registering: either it sees the changed generation and removes its own
    /// registration, or the cleanup comes later and removes it. The
    /// generation is what makes a disable that was undone in the meantime
    /// count: the user is enabled when this looks, and the generation is not
    /// the one the request has. The owner is never disabled and passes.
    ///
    /// Every refusal is the same `401` as a token nobody issued. It removes
    /// everything the user holds, because registrations carry no owner of
    /// their own: what the user opened under the newer generation is closed
    /// too, and its clients open it again. Removing only the late request's
    /// registration needs a removal by identity in each of the five registries
    /// and only the connection registry has one; decision 0036 (item 11)
    /// records why this was kept.
    pub(crate) async fn ensure_current(
        &self,
        actor: &ActorId,
        generation: StateGeneration,
    ) -> Result<(), ApiError> {
        if actor.is_owner() {
            return Ok(());
        }
        let id = actor.clone();
        let user = self
            .identity(move |store| store.user(&id))
            .await?
            .map_err(|err| ApiError::unavailable(err.to_string()))?;
        if user.is_some_and(|user| user.is_enabled() && user.state_generation == generation) {
            return Ok(());
        }
        self.revoke_actor(actor);
        Err(ApiError::unauthenticated())
    }

    /// Runs a call on the identity store (users and tokens, blocking `SQLite`)
    /// off the async executor.
    pub(crate) async fn identity<T, F>(&self, call: F) -> Result<T, ApiError>
    where
        F: FnOnce(&SqliteTokenStore) -> T + Send + 'static,
        T: Send + 'static,
    {
        let store = Arc::clone(&self.shared.token_store);
        tokio::task::spawn_blocking(move || call(&store))
            .await
            .map_err(|_| ApiError::unavailable("the identity store call failed"))
    }

    /// Runs a synchronous application command off the async executor.
    pub(crate) async fn run<T, F>(&self, command: F) -> Result<T, ArtifactError>
    where
        T: Send + 'static,
        F: FnOnce(&ArtifactService<SqliteStore>) -> Result<T, ArtifactError> + Send + 'static,
    {
        let shared = Arc::clone(&self.shared);
        tokio::task::spawn_blocking(move || command(&shared.artifacts))
            .await
            .unwrap_or_else(|_| Err(ArtifactError::Unavailable("worker task failed".into())))
    }
}

/// Routes of HTTP API v1 and the MCP endpoint without web assets.
pub fn router(state: AppState) -> Router {
    router_with_web(state, None)
}

/// Routes of HTTP API v1 and the MCP endpoint mounted behind authentication and limits.
fn api_v1_routes(state: &AppState) -> Router<AppState> {
    Router::new()
        .merge(mcp::routes(state.clone()))
        .merge(tokens::routes())
        .merge(users::routes())
        .route("/v1/artifacts", get(list_artifacts).post(create_artifact))
        .route(
            "/v1/artifacts/{id}",
            get(show_artifact).patch(update_artifact),
        )
        .route(
            "/v1/artifacts/{id}/purge",
            axum::routing::post(purge_artifact),
        )
        .route(
            "/v1/content/unreferenced",
            get(show_unreferenced_content).delete(remove_unreferenced_content),
        )
        .route(
            "/v1/artifacts/{id}/jobs",
            get(list_artifact_jobs).post(enqueue_job),
        )
        .route(
            "/v1/artifacts/{id}/data",
            get(read_data)
                .put(set_data)
                .patch(update_data)
                .delete(delete_data)
                // Inside the API-wide limit, so this one applies.
                .layer(DefaultBodyLimit::max(MAX_DATA_BODY_BYTES)),
        )
        .route(
            "/v1/artifacts/{id}/data/members",
            get(list_data_members),
        )
        .route(
            "/v1/artifacts/{id}/data/events",
            get(data_events::stream),
        )
        .route("/v1/jobs", get(list_jobs))
        .route("/v1/jobs/{id}", get(show_job))
        .route("/v1/jobs/{id}/cancel", axum::routing::post(cancel_job))
        .route(
            "/v1/artifacts/{id}/revisions",
            get(list_revisions).post(save_revision),
        )
        .route("/v1/artifacts/{id}/edits", axum::routing::post(edit_files))
        .route(
            "/v1/artifacts/{id}/revisions/{revision}",
            get(show_revision),
        )
        .route(
            "/v1/artifacts/{id}/revisions/{revision}/files/{*path}",
            get(read_file),
        )
        .route(
            "/v1/artifacts/{id}/revisions/{revision}/export",
            get(export_revision),
        )
        .route(
            "/v1/artifacts/{id}/revisions/{revision}/restore",
            axum::routing::post(restore_revision),
        )
        .route(
            "/v1/artifacts/{id}/revisions/{revision}/duplicate",
            axum::routing::post(duplicate_artifact),
        )
        .route(
            "/v1/artifacts/{id}/revisions/{revision}/previews",
            axum::routing::post(create_preview),
        )
        .route(
            "/v1/artifacts/{id}/revisions/{revision}/runtime-views",
            axum::routing::post(create_runtime_view),
        )
        .route(
            "/v1/runtime-views/{view}/{generation}",
            axum::routing::delete(close_runtime_view),
        )
        .route(
            "/v1/runtime-sessions",
            axum::routing::post(open_runtime_session),
        )
        .route(
            "/v1/runtime-sessions/{session}",
            axum::routing::delete(close_runtime_session),
        )
        .route(
            "/v1/runtime-sessions/{session}/artifacts/{id}/revisions/{revision}/grant",
            axum::routing::put(set_runtime_grant),
        )
        .route("/v1/agent-sessions", get(agent_sessions))
        .route("/v1/handoffs", axum::routing::post(stage_handoff))
        .route("/v1/handoffs/{id}", get(get_handoff))
        .route("/v1/handoffs/{id}/send", axum::routing::post(send_handoff))
        .route(
            "/v1/handoffs/{id}/cancel",
            axum::routing::post(cancel_handoff),
        )
        .route("/v1/changes", get(changes::stream))
        .method_not_allowed_fallback(method_not_allowed)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::map_response(no_store))
        .layer(middleware::from_fn_with_state(state.clone(), admit))
        .merge(frame_routes(state))
        .merge(room_routes(state))
}

/// Largest body of a frame call: the secret, a package path and two numbers.
const MAX_FRAME_BODY_BYTES: usize = 16 * 1024;

/// Largest body of a frame's write: 1.5 MiB, which holds the base64 of the
/// largest file (1,398,104 characters, `MAX_RUNTIME_SOURCE_WRITE_BASE64`) and
/// the small fields around it (decision 0033). Only the write route has this
/// limit; a test writes a file of the largest size.
const MAX_FRAME_WRITE_BODY_BYTES: usize = 3 * 512 * 1024;

/// Largest body of a room call: a state or an event's text is at most 512
/// bytes, which JSON escaping can make six times as long.
const MAX_ROOM_BODY_BYTES: usize = 8 * 1024;

/// Rooms and presence (decision 0034). A tab reaches its participants
/// through the runtime connection in the path; the calls are admitted under
/// the room budget, not the owner's or the frames'.
fn room_routes(state: &AppState) -> Router<AppState> {
    const ROOM: &str = "/v1/runtime-sessions/{session}/artifacts/{id}/room/participants";
    Router::new()
        .route(ROOM, axum::routing::post(join_room))
        .route(
            &format!("{ROOM}/{{participant}}/{{epoch}}/leave"),
            axum::routing::post(leave_room),
        )
        .route(
            &format!("{ROOM}/{{participant}}/{{epoch}}/heartbeat"),
            axum::routing::post(room_heartbeat),
        )
        .route(
            &format!("{ROOM}/{{participant}}/{{epoch}}/events"),
            axum::routing::post(send_room_event),
        )
        .method_not_allowed_fallback(method_not_allowed)
        .layer(DefaultBodyLimit::max(MAX_ROOM_BODY_BYTES))
        .layer(middleware::map_response(no_store))
        .layer(middleware::from_fn_with_state(state.clone(), admit_room))
}

/// What a runtime frame can ask the workspace to ask the server (decisions
/// 0019 and 0032). The workspace page sends these with the account token on
/// the frame's behalf; the frame itself holds none. They are admitted under
/// the frame budget, not the owner's.
fn frame_routes(state: &AppState) -> Router<AppState> {
    const VIEW: &str = "/v1/artifacts/{id}/revisions/{revision}/runtime-views/{view}/{generation}";
    Router::new()
        .route(
            &format!("{VIEW}/metadata"),
            axum::routing::post(runtime_metadata),
        )
        .route(
            &format!("{VIEW}/source/list"),
            axum::routing::post(runtime_source_list),
        )
        .route(
            &format!("{VIEW}/source/read"),
            axum::routing::post(runtime_source_read),
        )
        .route(
            &format!("{VIEW}/source/offer"),
            axum::routing::post(runtime_source_offer),
        )
        .route(
            &format!("{VIEW}/source/download"),
            axum::routing::post(runtime_source_download),
        )
        .route(
            &format!("{VIEW}/source/write"),
            axum::routing::post(runtime_source_write)
                .layer(DefaultBodyLimit::max(MAX_FRAME_WRITE_BODY_BYTES)),
        )
        .method_not_allowed_fallback(method_not_allowed)
        .layer(DefaultBodyLimit::max(MAX_FRAME_BODY_BYTES))
        .layer(middleware::map_response(no_store))
        .layer(middleware::from_fn_with_state(state.clone(), admit_frame))
}

/// Routes of HTTP API v1, the MCP endpoint, and optional production web assets.
pub fn router_with_web(state: AppState, web_assets: Option<WebAssets>) -> Router {
    let api_routes = api_v1_routes(&state);

    let fallback_state = state.clone();
    let fallback_assets = web_assets;
    let fallback_handler = move |req: Request| {
        let state = fallback_state.clone();
        let assets = fallback_assets.clone();
        async move {
            let path = req.uri().path().to_owned();
            if path == "/healthz" {
                return StatusCode::METHOD_NOT_ALLOWED.into_response();
            }
            let is_api = path.starts_with("/v1/") || path == "/v1" || path.starts_with("/mcp");
            if is_api || assets.is_none() {
                // Authenticates first, so callers without a token cannot
                // probe which routes exist.
                return run_admitted(&state, req, |_| async { no_such_route().into_response() })
                    .await;
            }
            let method = req.method().clone();
            if let Some(assets) = assets {
                assets.handle(&method, &path).await
            } else {
                no_such_route().into_response()
            }
        }
    };

    Router::new()
        .merge(api_routes)
        .route("/healthz", get(healthz_get).head(healthz_head))
        .fallback(fallback_handler)
        .layer(middleware::from_fn_with_state(state.clone(), cors))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            proxy_inspection,
        ))
        .with_state(state)
}

/// GET /healthz handler: returns 200 OK "ok\n" without authentication or admission limits.
async fn healthz_get() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "ok\n",
    )
}

/// HEAD /healthz handler: returns 200 OK with empty body.
async fn healthz_head() -> impl IntoResponse {
    StatusCode::OK
}

/// Authenticates the request, then admits it under the actor's limits. A
/// refused request never reaches the application, so this attempt commits
/// nothing; its idempotency key is free unless an earlier attempt used it.
async fn admit(State(state): State<AppState>, request: Request, next: Next) -> Response {
    admit_under(&state.shared.limiter, state.clone(), request, next).await
}

/// As [`admit`] for the requests a runtime frame causes, under the frame
/// budget: whatever a frame floods, the owner's budget is untouched.
async fn admit_frame(State(state): State<AppState>, request: Request, next: Next) -> Response {
    admit_under(&state.shared.frame_limiter, state.clone(), request, next).await
}

/// As [`admit`] for room calls, under the room budget.
async fn admit_room(State(state): State<AppState>, request: Request, next: Next) -> Response {
    admit_under(&state.shared.room_limiter, state.clone(), request, next).await
}

async fn admit_under(
    limiter: &Limiter,
    state: AppState,
    mut request: Request,
    next: Next,
) -> Response {
    let auth = match state.authenticate(request.headers()).await {
        Ok(auth) => auth,
        Err(error) => return error.into_response(),
    };
    let permit = match limiter.admit(&auth.actor) {
        Ok(permit) => permit,
        Err(refusal) => {
            discard(&state, request.into_body()).await;
            return refused(refusal);
        }
    };
    request
        .extensions_mut()
        .insert(Authenticated(auth.actor.clone()));
    request.extensions_mut().insert(auth);
    // The request runs in its own task, which keeps the permit until the
    // handler finishes. A client that disconnects cancels only this wait, so
    // it cannot free the slot while storage work continues.
    let task = tokio::spawn(async move {
        let response = next.run(request).await;
        drop(permit);
        response
    });
    task.await.unwrap_or_else(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Internal,
            "the request failed; retry with the same key to learn its outcome",
        )
        .into_response()
    })
}

/// How long a refused request's body is read before the refusal is sent.
pub const MAX_DRAIN_TIME: Duration = Duration::from_secs(2);
/// Refused bodies read at the same time, across all actors. Refused requests
/// hold no permit, so this bounds the connections and tasks they can occupy.
pub const MAX_DRAINING: usize = 32;

/// Reads and drops a refused request's body so that a client still sending
/// it reads the refusal rather than a reset connection. Nothing is kept in
/// memory, and the reading stops at the API body limit, after
/// [`MAX_DRAIN_TIME`], or at once when [`MAX_DRAINING`] bodies are already
/// being read; a slow or endless sender then gets the refusal regardless.
async fn discard(state: &AppState, mut body: Body) {
    let Some(_reading) = Draining::start(&state.shared.draining) else {
        return;
    };
    let drain = async {
        let mut read = 0;
        while read <= MAX_BODY_BYTES {
            match poll_fn(|context| Pin::new(&mut body).poll_frame(context)).await {
                Some(Ok(frame)) => read += frame.data_ref().map_or(0, Bytes::len),
                _ => break,
            }
        }
    };
    // A timeout stops reading; the refusal is sent either way.
    let _finished_in_time: bool = tokio::time::timeout(MAX_DRAIN_TIME, drain).await.is_ok();
}

/// One refused body counted against [`MAX_DRAINING`]. The count is released
/// on drop, so a request cancelled mid-read, such as when its connection is
/// dropped, does not keep it.
struct Draining<'count>(&'count AtomicUsize);

impl<'count> Draining<'count> {
    /// Counts one more body, or returns `None` when the limit is reached.
    fn start(count: &'count AtomicUsize) -> Option<Self> {
        let reading = Self(count);
        (count.fetch_add(1, Ordering::AcqRel) < MAX_DRAINING).then_some(reading)
    }
}

impl Drop for Draining<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn refused(refusal: Refusal) -> Response {
    let wait = match refusal {
        Refusal::Rate(wait) => wait,
        Refusal::Busy => Duration::from_secs(1),
        Refusal::Unavailable => {
            return ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorCode::Unavailable,
                refusal.to_string(),
            )
            .into_response();
        }
    };
    // Whole seconds, rounded up: retrying at the stated time succeeds.
    let seconds = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
    let mut response = ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        ErrorCode::RateLimited,
        refusal.to_string(),
    )
    .into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(seconds.max(1)));
    response
}

// Unmatched requests authenticate first, like every route, so that callers
// without the token cannot probe which routes and methods exist.
fn no_such_route() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, "no such route")
}

// Axum adds the `Allow` header listing the route's methods.
async fn method_not_allowed(_: Authenticated) -> ApiError {
    ApiError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        ErrorCode::InvalidRequest,
        "this route does not support the request method",
    )
}

/// Runs a handler under authentication and rate limiting admission permits.
async fn run_admitted<F, Fut>(state: &AppState, mut request: Request, inner: F) -> Response
where
    F: FnOnce(Request) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Response> + Send + 'static,
{
    let auth = match state.authenticate(request.headers()).await {
        Ok(auth) => auth,
        Err(error) => return error.into_response(),
    };
    let permit = match state.shared.limiter.admit(&auth.actor) {
        Ok(permit) => permit,
        Err(refusal) => {
            discard(state, request.into_body()).await;
            return refused(refusal);
        }
    };
    request
        .extensions_mut()
        .insert(Authenticated(auth.actor.clone()));
    request.extensions_mut().insert(auth);
    let response = inner(request).await;
    drop(permit);
    response
}

/// Evaluates client connection info, rejects untrusted forwarded headers,
/// and validates the Host header against the allowed hosts.
async fn proxy_inspection(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let peer_ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip());

    let connection_info = match crate::proxy::evaluate_connection(
        req.headers(),
        peer_ip,
        &state.shared.settings.trusted_proxies,
    ) {
        Ok(info) => info,
        Err(err) => return err.into_response(),
    };

    if !crate::proxy::is_host_allowed(&connection_info.host, &state.shared.settings.allowed_hosts) {
        return ApiError::new(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidRequest,
            "host header is not allowed",
        )
        .into_response();
    }

    req.extensions_mut().insert(connection_info);
    next.run(req).await
}

fn is_loopback_origin(origin: &str) -> bool {
    origin.starts_with("http://127.0.0.1:")
        || origin.starts_with("http://localhost:")
        || origin.starts_with("http://[::1]:")
        || origin == "http://127.0.0.1"
        || origin == "http://localhost"
        || origin == "http://[::1]"
}

fn is_origin_permitted(normalized_origin: &str, settings: &Settings) -> bool {
    if let Some(ref api_origin) = settings.api_origin
        && normalized_origin == api_origin
    {
        return true;
    }
    for allowed in &settings.allowed_origins {
        if normalized_origin == allowed {
            return true;
        }
    }
    if is_loopback_origin(normalized_origin) {
        return true;
    }
    // In local dev mode without explicit api_origin or allowed_origins, preserve permissive local behavior
    settings.api_origin.is_none() && settings.allowed_origins.is_empty()
}

/// CORS middleware handling preflights and response headers on `/v1/*` API endpoints.
async fn cors(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let is_api = path.starts_with("/v1/") || path == "/v1";

    if !is_api {
        return next.run(req).await;
    }

    let origin = req.headers().get(header::ORIGIN).cloned();

    let Some(origin_val) = origin else {
        return next.run(req).await;
    };

    let Ok(origin_str) = origin_val.to_str() else {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            ErrorCode::InvalidRequest,
            "origin header is not valid ASCII",
        )
        .into_response();
    };

    if origin_str == "null" {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            ErrorCode::InvalidRequest,
            "requests from preview origin or null origin are forbidden",
        )
        .into_response();
    }

    let Ok(normalized_origin) = normalize_origin(origin_str) else {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            ErrorCode::InvalidRequest,
            "origin header is not a valid HTTP or HTTPS origin",
        )
        .into_response();
    };

    if normalized_origin == state.shared.settings.content_origin {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            ErrorCode::InvalidRequest,
            "requests from preview origin or null origin are forbidden",
        )
        .into_response();
    }

    if !is_origin_permitted(&normalized_origin, &state.shared.settings) {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            ErrorCode::InvalidRequest,
            "origin header is not permitted",
        )
        .into_response();
    }

    if req.method() == Method::OPTIONS {
        return Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin_val)
            .header(
                header::ACCESS_CONTROL_ALLOW_METHODS,
                "GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS",
            )
            .header(
                header::ACCESS_CONTROL_ALLOW_HEADERS,
                "Authorization, Content-Type, If-Match, Idempotency-Key, Last-Event-ID",
            )
            .header(header::ACCESS_CONTROL_MAX_AGE, "86400")
            .header(
                header::ACCESS_CONTROL_EXPOSE_HEADERS,
                "Content-Range, Retry-After, Location",
            )
            .body(axum::body::Body::empty())
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    let mut response = next.run(req).await;
    response
        .headers_mut()
        .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin_val);
    response.headers_mut().insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("Content-Range, Retry-After, Location"),
    );
    response
}

async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let retry = matches!(
            self.body.code,
            ErrorCode::Unavailable | ErrorCode::RateLimited
        );
        let mut response = (self.status, Json(ErrorResponse { error: *self.body })).into_response();
        if self.status == StatusCode::UNAUTHORIZED {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        if retry {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        }
        if let Some(Ok(value)) = self
            .range_size
            .map(|size| HeaderValue::try_from(format!("bytes */{size}")))
        {
            response.headers_mut().insert(header::CONTENT_RANGE, value);
        }
        response
    }
}

impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
        if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
            return Self::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                ErrorCode::PayloadTooLarge,
                format!("request body exceeds {MAX_BODY_BYTES} bytes"),
            );
        }
        Self::invalid(rejection.body_text())
    }
}

impl From<QueryRejection> for ApiError {
    fn from(rejection: QueryRejection) -> Self {
        Self::invalid(rejection.body_text())
    }
}

impl From<PathRejection> for ApiError {
    fn from(rejection: PathRejection) -> Self {
        Self::invalid(rejection.body_text())
    }
}

/// The actor authenticated by the bearer token and admitted by its limits.
/// Only the `admit` middleware creates it; a request without it is refused.
#[derive(Debug, Clone)]
pub(crate) struct Authenticated(pub(crate) ActorId);

impl<S: Sync> FromRequestParts<S> for Authenticated {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, ApiError> {
        parts
            .extensions
            .get::<Self>()
            .cloned()
            .ok_or_else(ApiError::unauthenticated)
    }
}

impl<S: Sync> FromRequestParts<S> for AuthContext {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, ApiError> {
        parts
            .extensions
            .get::<Self>()
            .cloned()
            .ok_or_else(ApiError::unauthenticated)
    }
}

/// An extractor that ensures the caller has administrative privileges (bootstrap owner token).
/// Agent tokens attempting to access an administrative endpoint receive 403 Forbidden.
#[derive(Debug, Clone)]
pub(crate) struct RequireAdmin(
    #[expect(
        dead_code,
        reason = "wrapped AuthContext is retained for future administrative context"
    )]
    pub(crate) AuthContext,
);

impl<S: Sync> FromRequestParts<S> for RequireAdmin {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, ApiError> {
        let auth = parts
            .extensions
            .get::<AuthContext>()
            .cloned()
            .ok_or_else(ApiError::unauthenticated)?;
        if !auth.is_admin {
            return Err(ApiError::forbidden("administrative privileges required"));
        }
        Ok(RequireAdmin(auth))
    }
}

/// An extractor for the purge route (decision 0029, amended in decision
/// 0036): the administrator, or a token issued to a user
/// other than the owner profile. Purge acts on the caller's own artifacts only
/// (the application checks ownership), so a user's token can reach nothing but
/// that user's trash. A token issued to the **owner profile** stays refused
/// (`403`): the owner has the bootstrap token, so an issued owner token is the
/// agent credential of decision 0029. A non-owner user has no other
/// credential, and the server cannot tell a person's token from an agent's;
/// that is the cost of letting a user empty their own trash.
#[derive(Debug, Clone)]
pub(crate) struct RequirePurge;

impl<S: Sync> FromRequestParts<S> for RequirePurge {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, ApiError> {
        let auth = parts
            .extensions
            .get::<AuthContext>()
            .ok_or_else(ApiError::unauthenticated)?;
        if auth.is_admin || !auth.actor.is_owner() {
            Ok(Self)
        } else {
            Err(ApiError::forbidden(
                "purge needs the administrative credential or a user's own token",
            ))
        }
    }
}

/// The credentials of a `Bearer` authorization value. The scheme is
/// case-insensitive and separated from the token by one or more spaces
/// (RFC 9110 section 11.4).
fn bearer_token(value: &str) -> Option<&str> {
    let (scheme, token) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim_start_matches(' '))
}

/// The required `Idempotency-Key` header.
struct IdempotencyKey(RequestKey);

impl<S: Sync> FromRequestParts<S> for IdempotencyKey {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, ApiError> {
        let value = parts
            .headers
            .get("idempotency-key")
            .ok_or_else(|| ApiError::invalid("the Idempotency-Key header is required"))?
            .to_str()
            .map_err(|_| ApiError::invalid("Idempotency-Key must be ASCII"))?;
        operations::request_key("Idempotency-Key", value).map(Self)
    }
}

async fn create_artifact(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    IdempotencyKey(key): IdempotencyKey,
    body: Result<Json<CreateArtifactRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = body?;
    let response = operations::create(&state, actor, key, request).await?;
    let location = format!("/v1/artifacts/{}", response.artifact.id);
    Ok(created(&location, Json(response)))
}

async fn save_revision(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    IdempotencyKey(key): IdempotencyKey,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<SaveRevisionRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Path(id) = path?;
    // An identifier that cannot name an artifact is reported before a
    // malformed body; whether the artifact exists is checked with the request.
    operations::artifact_id(&id)?;
    let Json(request) = body?;
    let response = operations::save(&state, actor, key, &id, request).await?;
    let location = format!(
        "/v1/artifacts/{}/revisions/{}",
        response.artifact.id, response.revision.revision
    );
    Ok(created(&location, Json(response)))
}

async fn edit_files(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    IdempotencyKey(key): IdempotencyKey,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<EditFilesRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Path(id) = path?;
    operations::artifact_id(&id)?;
    let Json(request) = body?;
    let response = operations::edit_files(&state, actor, key, &id, request).await?;
    let location = format!(
        "/v1/artifacts/{}/revisions/{}",
        response.artifact.id, response.revision.revision
    );
    Ok(created(&location, Json(response)))
}

async fn restore_revision(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    IdempotencyKey(key): IdempotencyKey,
    path: Result<Path<(String, u64)>, PathRejection>,
    body: Result<Json<RestoreRevisionRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Path((id, revision)) = path?;
    // An identifier or revision number that cannot name one is reported
    // before a malformed body; whether they exist is checked with the request.
    operations::artifact_id(&id)?;
    operations::revision_number(revision)?;
    let Json(request) = body?;
    let response = operations::restore(&state, actor, key, &id, revision, request).await?;
    let location = format!(
        "/v1/artifacts/{}/revisions/{}",
        response.artifact.id, response.revision.revision
    );
    Ok(created(&location, Json(response)))
}

async fn duplicate_artifact(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    IdempotencyKey(key): IdempotencyKey,
    path: Result<Path<(String, u64)>, PathRejection>,
    body: Result<Json<DuplicateArtifactRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Path((id, revision)) = path?;
    // An identifier or revision number that cannot name one is reported
    // before a malformed body; whether they exist is checked with the request.
    operations::artifact_id(&id)?;
    operations::revision_number(revision)?;
    let Json(request) = body?;
    let response = operations::duplicate(&state, actor, key, &id, revision, request).await?;
    let location = format!("/v1/artifacts/{}", response.artifact.id);
    Ok(created(&location, Json(response)))
}

async fn list_artifacts(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    query: Result<Query<ListArtifactsQuery>, QueryRejection>,
) -> Result<Json<ArtifactList>, ApiError> {
    let Query(query) = query?;
    Ok(Json(operations::list(&state, actor, query).await?))
}

async fn update_artifact(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    IdempotencyKey(key): IdempotencyKey,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<UpdateArtifactRequest>, JsonRejection>,
) -> Result<Json<UpdateArtifactResponse>, ApiError> {
    let Path(id) = path?;
    // An identifier that cannot name an artifact is reported before a
    // malformed body; whether the artifact exists is checked with the request.
    operations::artifact_id(&id)?;
    let Json(request) = body?;
    Ok(Json(
        operations::update(&state, actor, key, &id, request).await?,
    ))
}

/// Enqueues a verification job for one revision (decision 0030). An agent
/// token may: the job reads stored files and changes nothing, and the actor's
/// limit bounds how many wait.
async fn enqueue_job(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    IdempotencyKey(key): IdempotencyKey,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<EnqueueJobRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Path(id) = path?;
    // An identifier that cannot name an artifact is reported before a
    // malformed body; whether the artifact exists is checked with the request.
    operations::artifact_id(&id)?;
    let Json(request) = body?;
    let response: EnqueueJobResponse =
        operations::enqueue_job(&state, actor, key, &id, request).await?;
    let location = format!("/v1/jobs/{}", response.job.id);
    Ok(created(&location, Json(response)))
}

/// Why the body of a data write was refused. The size limit of these routes is
/// not the API-wide one.
fn data_body_error(rejection: &JsonRejection) -> ApiError {
    if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            ErrorCode::PayloadTooLarge,
            format!("request body exceeds {MAX_DATA_BODY_BYTES} bytes"),
        );
    }
    ApiError::invalid(rejection.body_text())
}

/// Reads a value of the caller's application data (decision 0031). An agent
/// token may: it authenticates as the same owner and reads what the owner
/// reads.
async fn read_data(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<ReadDataQuery>, QueryRejection>,
) -> Result<Json<AppData>, ApiError> {
    let Path(id) = path?;
    operations::artifact_id(&id)?;
    let Query(ReadDataQuery { path: at }) = query?;
    Ok(Json(
        operations::read_data(&state, actor, &id, at.as_deref()).await?,
    ))
}

/// One page of the members of a collection of the caller's application data
/// (decision 0040). An agent token may: it reads what the owner reads.
async fn list_data_members(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<ListDataMembersQuery>, QueryRejection>,
) -> Result<Json<DataMembers>, ApiError> {
    let Path(id) = path?;
    operations::artifact_id(&id)?;
    let Query(query) = query?;
    Ok(Json(
        operations::list_data_members(&state, actor, &id, query).await?,
    ))
}

/// Replaces a value of application data. An agent token may write data: it
/// is bounded, conditional on the version, and keyed, but nothing keeps the
/// value it replaces (decision 0031).
async fn set_data(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    IdempotencyKey(key): IdempotencyKey,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<SetDataRequest>, JsonRejection>,
) -> Result<Json<DataWriteResponse>, ApiError> {
    let Path(id) = path?;
    operations::artifact_id(&id)?;
    let Json(request) = body.map_err(|rejection| data_body_error(&rejection))?;
    Ok(Json(
        operations::set_data(&state, actor, key, &id, request).await?,
    ))
}

/// Applies a JSON Merge Patch to application data.
async fn update_data(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    IdempotencyKey(key): IdempotencyKey,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<UpdateDataRequest>, JsonRejection>,
) -> Result<Json<DataWriteResponse>, ApiError> {
    let Path(id) = path?;
    operations::artifact_id(&id)?;
    let Json(request) = body.map_err(|rejection| data_body_error(&rejection))?;
    Ok(Json(
        operations::update_data(&state, actor, key, &id, request).await?,
    ))
}

/// Removes a value of application data.
async fn delete_data(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    IdempotencyKey(key): IdempotencyKey,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<DeleteDataRequest>, JsonRejection>,
) -> Result<Json<DataWriteResponse>, ApiError> {
    let Path(id) = path?;
    operations::artifact_id(&id)?;
    let Json(request) = body.map_err(|rejection| data_body_error(&rejection))?;
    Ok(Json(
        operations::delete_data(&state, actor, key, &id, request).await?,
    ))
}

async fn list_jobs(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    query: Result<Query<ListJobsQuery>, QueryRejection>,
) -> Result<Json<JobList>, ApiError> {
    let Query(query) = query?;
    Ok(Json(
        operations::list_jobs(&state, actor, None, query).await?,
    ))
}

async fn list_artifact_jobs(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<ListJobsQuery>, QueryRejection>,
) -> Result<Json<JobList>, ApiError> {
    let Path(id) = path?;
    let Query(query) = query?;
    Ok(Json(
        operations::list_jobs(&state, actor, Some(&id), query).await?,
    ))
}

async fn show_job(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<String>, PathRejection>,
) -> Result<Json<Job>, ApiError> {
    let Path(id) = path?;
    Ok(Json(operations::show_job(&state, actor, &id).await?))
}

/// Asks to stop a job. The body must be `{}`.
async fn cancel_job(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    IdempotencyKey(key): IdempotencyKey,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<CancelJobRequest>, JsonRejection>,
) -> Result<Json<CancelJobResponse>, ApiError> {
    let Path(id) = path?;
    operations::job_id(&id)?;
    let Json(CancelJobRequest {}) = body?;
    Ok(Json(operations::cancel_job(&state, actor, key, &id).await?))
}

/// Removes the caller's own trashed artifact for good: the administrative
/// credential or a non-owner user's token ([`RequirePurge`]). A token issued to
/// the owner profile is an agent token and must not be able to do what cannot
/// be undone (decision 0029). The guard comes first, so such a token learns
/// nothing about the artifact, the base, or the key; another user's artifact
/// answers as a missing one.
async fn purge_artifact(
    State(state): State<AppState>,
    _: RequirePurge,
    Authenticated(actor): Authenticated,
    IdempotencyKey(key): IdempotencyKey,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<PurgeArtifactRequest>, JsonRejection>,
) -> Result<Json<PurgeArtifactResponse>, ApiError> {
    let Path(id) = path?;
    // An identifier that cannot name an artifact is reported before a
    // malformed body; whether the artifact exists is checked with the request.
    operations::artifact_id(&id)?;
    let Json(request) = body?;
    Ok(Json(
        operations::purge(&state, actor, key, &id, request).await?,
    ))
}

/// The dry run: reads and removes nothing. Administrative like the apply: it
/// reports the size of content shared by every owner and agent (decision
/// 0029).
async fn show_unreferenced_content(
    State(state): State<AppState>,
    RequireAdmin(_): RequireAdmin,
) -> Result<Json<UnreferencedContent>, ApiError> {
    Ok(Json(operations::unreferenced_content(&state).await?))
}

/// Removes the content the dry run reported, once per `Idempotency-Key`, if it
/// is still exactly that set. Administrative credential only; the role is
/// checked before the key and the body.
async fn remove_unreferenced_content(
    State(state): State<AppState>,
    RequireAdmin(_): RequireAdmin,
    Authenticated(actor): Authenticated,
    IdempotencyKey(key): IdempotencyKey,
    body: Result<Json<RemoveUnreferencedContentRequest>, JsonRejection>,
) -> Result<Json<RemoveUnreferencedContentResponse>, ApiError> {
    let Json(request) = body?;
    Ok(Json(
        operations::remove_unreferenced_content(&state, actor, key, request).await?,
    ))
}

async fn show_artifact(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<String>, PathRejection>,
) -> Result<Json<Artifact>, ApiError> {
    let Path(id) = path?;
    Ok(Json(operations::show(&state, actor, &id).await?))
}

async fn list_revisions(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<String>, PathRejection>,
) -> Result<Json<RevisionList>, ApiError> {
    let Path(id) = path?;
    Ok(Json(operations::revisions(&state, actor, &id).await?))
}

async fn show_revision(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<(String, u64)>, PathRejection>,
) -> Result<Json<Revision>, ApiError> {
    let Path((id, revision)) = path?;
    Ok(Json(
        operations::revision(&state, actor, &id, revision).await?,
    ))
}

/// A pinned file, or one byte range of it (decision 0016). A range that
/// starts at or past the end of the file, and `-0`, select nothing: `416`.
async fn read_file(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    headers: HeaderMap,
    path: Result<Path<(String, u64, String)>, PathRejection>,
) -> Result<Response, ApiError> {
    let Path((id, revision, file)) = path?;
    let requested =
        ranges::requested(&headers).map_err(|error| ApiError::invalid(error.to_string()))?;
    let range = match requested {
        Requested::Part(range) => range,
        // The whole file is read for `-0` too, so that it is answered with
        // the file's size, after every check a read makes.
        Requested::Whole | Requested::Nothing => ByteRange::WHOLE,
    };
    let file =
        operations::read_file(&state, actor, &id, revision, &file, range, Boundary::Byte).await?;
    let part = file.part;
    let ranged = requested != Requested::Whole;
    if ranged && (requested == Requested::Nothing || part.content.is_empty()) {
        return Err(ApiError::unsatisfiable(
            part.size,
            format!(
                "the range selects no bytes of the file, which has {} bytes",
                part.size
            ),
        ));
    }
    let content_range = ranged.then(|| {
        let last = part.offset + part.content.len() as u64 - 1;
        format!("bytes {}-{last}/{}", part.offset, part.size)
    });
    let status = if ranged {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let mut response = (
        status,
        [
            (header::CONTENT_TYPE, file.path.media_type().essence()),
            (header::CONTENT_DISPOSITION, "attachment"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (
                header::CONTENT_SECURITY_POLICY,
                "sandbox; default-src 'none'",
            ),
            (header::ACCEPT_RANGES, "bytes"),
        ],
        part.content,
    )
        .into_response();
    if let Some(Ok(value)) = content_range.map(HeaderValue::try_from) {
        response.headers_mut().insert(header::CONTENT_RANGE, value);
    }
    Ok(response)
}

async fn export_revision(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<(String, u64)>, PathRejection>,
) -> Result<Response, ApiError> {
    let Path((id, revision)) = path?;
    let export = operations::export(&state, actor, &id, revision).await?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/zip".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", export.file_name),
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
        ],
        export.archive,
    )
        .into_response())
}

async fn create_preview(
    State(state): State<AppState>,
    auth: AuthContext,
    path: Result<Path<(String, u64)>, PathRejection>,
) -> Result<Response, ApiError> {
    let Path((id, revision)) = path?;
    let preview = operations::preview(
        &state,
        auth.actor,
        auth.token_id,
        auth.generation,
        &id,
        revision,
    )
    .await?;
    let url = preview.url.clone();
    Ok(created(&url, Json(preview)))
}

async fn create_runtime_view(
    State(state): State<AppState>,
    auth: AuthContext,
    path: Result<Path<(String, u64)>, PathRejection>,
    body: Result<Json<CreateRuntimeViewRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Path((id, revision)) = path?;
    let Json(request) = body?;
    let view = operations::create_runtime_view(
        &state,
        auth.actor,
        auth.token_id,
        auth.generation,
        &id,
        revision,
        request,
    )
    .await?;
    let url = view.url.clone();
    Ok(created(&url, Json(view)))
}

async fn runtime_metadata(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<(String, u64, String, u64)>, PathRejection>,
    body: Result<Json<RuntimeMetadataRequest>, JsonRejection>,
) -> Result<Json<RuntimeMetadata>, ApiError> {
    let Path((id, revision, view, generation)) = path?;
    let Json(request) = body.map_err(|rejection| frame_body_error(&rejection))?;
    operations::runtime_metadata(
        &state,
        actor,
        &id,
        revision,
        &view,
        generation,
        &request.initialization,
    )
    .await
    .map(Json)
}

type FramePath = Result<Path<(String, u64, String, u64)>, PathRejection>;

/// A frame call's body is built by the workspace page from what a frame
/// sent, so a parse failure says only that, with no serde text that could
/// quote the frame's input.
fn frame_body_error(rejection: &JsonRejection) -> ApiError {
    if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            ErrorCode::PayloadTooLarge,
            "the frame call is too large",
        );
    }
    ApiError::invalid("the frame call is not a valid request")
}

async fn runtime_source_list(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: FramePath,
    body: Result<Json<RuntimeSourceListRequest>, JsonRejection>,
) -> Result<Json<RuntimeSourceList>, ApiError> {
    let Path((id, revision, view, generation)) = path?;
    let Json(request) = body.map_err(|rejection| frame_body_error(&rejection))?;
    let call = FrameCall {
        actor: &actor,
        artifact: &id,
        revision,
        view: &view,
        generation,
        initialization: &request.initialization,
    };
    runtime_source::list(&state, &call).await.map(Json)
}

async fn runtime_source_read(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: FramePath,
    body: Result<Json<RuntimeSourceReadRequest>, JsonRejection>,
) -> Result<Json<RuntimeSourcePart>, ApiError> {
    let Path((id, revision, view, generation)) = path?;
    let Json(request) = body.map_err(|rejection| frame_body_error(&rejection))?;
    let call = FrameCall {
        actor: &actor,
        artifact: &id,
        revision,
        view: &view,
        generation,
        initialization: &request.initialization,
    };
    runtime_source::read(&state, &call, &request)
        .await
        .map(Json)
}

async fn runtime_source_offer(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: FramePath,
    body: Result<Json<RuntimeDownloadRequest>, JsonRejection>,
) -> Result<Json<RuntimeDownloadOffer>, ApiError> {
    let Path((id, revision, view, generation)) = path?;
    let Json(request) = body.map_err(|rejection| frame_body_error(&rejection))?;
    let call = FrameCall {
        actor: &actor,
        artifact: &id,
        revision,
        view: &view,
        generation,
        initialization: &request.initialization,
    };
    runtime_source::offer(&state, &call, &request)
        .await
        .map(Json)
}

/// Saves one file for the frame as the next revision (decision 0033).
async fn runtime_source_write(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: FramePath,
    body: Result<Json<RuntimeSourceWriteRequest>, JsonRejection>,
) -> Result<Json<RuntimeSourceWriteReceipt>, ApiError> {
    let Path((id, revision, view, generation)) = path?;
    let Json(request) = body.map_err(|rejection| frame_body_error(&rejection))?;
    let call = FrameCall {
        actor: &actor,
        artifact: &id,
        revision,
        view: &view,
        generation,
        initialization: &request.initialization,
    };
    runtime_source::write(&state, &call, &request)
        .await
        .map(Json)
}

/// The bytes of a download the user confirmed, as an attachment that no
/// browser renders: a closed content type, `nosniff`, and a sandbox policy
/// that also holds if the response is ever opened as a document. It is a
/// `POST` so that no link or navigation can reach it.
async fn runtime_source_download(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: FramePath,
    body: Result<Json<RuntimeDownloadRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Path((id, revision, view, generation)) = path?;
    let Json(request) = body.map_err(|rejection| frame_body_error(&rejection))?;
    let call = FrameCall {
        actor: &actor,
        artifact: &id,
        revision,
        view: &view,
        generation,
        initialization: &request.initialization,
    };
    let download = runtime_source::download(&state, &call, &request).await?;
    let disposition =
        HeaderValue::from_str(&format!("attachment; filename=\"{}\"", download.file_name))
            .map_err(|_| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ErrorCode::Internal,
                    "the download name is not a header value",
                )
            })?;
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static(download.content_type),
            ),
            (header::CONTENT_DISPOSITION, disposition),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static("sandbox; default-src 'none'"),
            ),
        ],
        download.bytes,
    )
        .into_response())
}

async fn close_runtime_view(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<(String, u64)>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    let Path((view, generation)) = path?;
    operations::close_runtime_view(&state, &actor, &view, generation)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn open_runtime_session(
    State(state): State<AppState>,
    auth: AuthContext,
) -> Result<Json<RuntimeConnection>, ApiError> {
    operations::open_confirmed_runtime_session(&state, &auth.actor, auth.token_id, auth.generation)
        .await
        .map(Json)
}

async fn close_runtime_session(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<String>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    let Path(session) = path?;
    operations::close_runtime_session(&state, &actor, &session)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn set_runtime_grant(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<(String, String, u64)>, PathRejection>,
    body: Result<Json<SetRuntimeGrantRequest>, JsonRejection>,
) -> Result<Json<RuntimeGrant>, ApiError> {
    let Path((session, id, revision)) = path?;
    let Json(request) = body?;
    operations::set_runtime_grant(&state, actor, &session, &id, revision, request)
        .await
        .map(Json)
}

async fn join_room(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<(String, String)>, PathRejection>,
    body: Result<Json<RoomJoinRequest>, JsonRejection>,
) -> Result<Json<RoomSnapshot>, ApiError> {
    let Path((session, id)) = path?;
    // The body is judged by `rooms` after the connection, so what it says
    // about a malformed body does not tell a live connection from a dead one.
    rooms::join(
        &state,
        &actor,
        &session,
        &id,
        body.map(|Json(request)| request).map_err(ApiError::from),
    )
    .await
    .map(Json)
}

async fn room_heartbeat(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<(String, String, String, String)>, PathRejection>,
    body: Result<Json<RoomHeartbeatRequest>, JsonRejection>,
) -> Result<Json<RoomSnapshot>, ApiError> {
    let Path((session, id, participant, epoch)) = path?;
    let names = RoomNames {
        session: &session,
        artifact: &id,
        participant: &participant,
        epoch: &epoch,
    };
    rooms::heartbeat(
        &state,
        &actor,
        names,
        body.map(|Json(request)| request).map_err(ApiError::from),
    )
    .await
    .map(Json)
}

async fn send_room_event(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<(String, String, String, String)>, PathRejection>,
    body: Result<Json<RoomEventRequest>, JsonRejection>,
) -> Result<Json<RoomEventReceipt>, ApiError> {
    let Path((session, id, participant, epoch)) = path?;
    let names = RoomNames {
        session: &session,
        artifact: &id,
        participant: &participant,
        epoch: &epoch,
    };
    rooms::send(
        &state,
        &actor,
        names,
        body.map(|Json(request)| request).map_err(ApiError::from),
    )
    .await
    .map(Json)
}

async fn leave_room(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<(String, String, String, String)>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    let Path((session, id, participant, epoch)) = path?;
    let names = RoomNames {
        session: &session,
        artifact: &id,
        participant: &participant,
        epoch: &epoch,
    };
    rooms::leave(&state, &actor, names).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn agent_sessions(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    query: Result<Query<HandoffActionRequest>, QueryRejection>,
) -> Result<Json<AgentSessionList>, ApiError> {
    let Query(request) = query?;
    operations::agent_sessions(&state, &actor, &request.workspace_session_id).map(Json)
}

async fn stage_handoff(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    body: Result<Json<StageHandoffRequest>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(request) = body?;
    let workspace = request.workspace_session_id.clone();
    let handoff = operations::stage_handoff(&state, actor, request).await?;
    Ok(created(
        &format!(
            "/v1/handoffs/{}?workspace_session_id={workspace}",
            handoff.id
        ),
        Json(handoff),
    ))
}

async fn get_handoff(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<HandoffActionRequest>, QueryRejection>,
) -> Result<Json<Handoff>, ApiError> {
    let Path(id) = path?;
    let Query(request) = query?;
    operations::get_handoff(&state, &actor, &request.workspace_session_id, &id).map(Json)
}

async fn send_handoff(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<HandoffActionRequest>, JsonRejection>,
) -> Result<Json<Handoff>, ApiError> {
    let Path(id) = path?;
    let Json(request) = body?;
    operations::send_handoff(&state, actor, &request.workspace_session_id, &id)
        .await
        .map(Json)
}

async fn cancel_handoff(
    State(state): State<AppState>,
    Authenticated(actor): Authenticated,
    path: Result<Path<String>, PathRejection>,
    body: Result<Json<HandoffActionRequest>, JsonRejection>,
) -> Result<Json<Handoff>, ApiError> {
    let Path(id) = path?;
    let Json(request) = body?;
    operations::cancel_handoff(&state, &actor, &request.workspace_session_id, &id).map(Json)
}

fn created(location: &str, body: impl IntoResponse) -> Response {
    let mut response = (StatusCode::CREATED, body).into_response();
    if let Ok(value) = HeaderValue::from_str(location) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    response
}
