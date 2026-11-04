//! `GET /v1/artifacts/{id}/data/events`: a server-sent stream of one watched
//! path of the caller's application data (decision 0040).
//!
//! The stream sends a `snapshot`, then a `change` with the full new value
//! whenever the data version is seen to have moved. Nothing is replayed and
//! nothing is kept per artifact or per path: a wake-up is one process-wide
//! counter, and each stream reads storage itself, at most once per throttle
//! interval. Every wait of the open and of the producer is raced against the
//! signals that must end it, so a stalled read cannot keep a place or a
//! registration.

use std::collections::HashMap;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use artifree_contracts::api::{
    AppData, DATA_EVENT_MAX_BYTES, DataEventsQuery, DataStreamTooLarge, ErrorResponse, JsonValue,
};
use artifree_core::artifacts::ArtifactError;
use artifree_domain::data::DataPath;
use artifree_domain::identity::{ActorId, ArtifactId, TokenId};
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::header::{self, HeaderValue};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_core::Stream;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use crate::auth::AuthContext;
use crate::http::AppState;
use crate::operations::{self, ApiError};

/// Limits and timings of data streams. The defaults are the documented ones
/// (decision 0040); embedders and tests may replace them with
/// [`AppState::set_data_stream_settings`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataStreamSettings {
    /// Streams one actor may keep open.
    pub streams_per_actor: u32,
    /// Streams the process may keep open.
    pub streams_per_process: u32,
    /// Opens in progress and streams open that one actor may have registered.
    pub registrations_per_actor: u32,
    /// Registrations the process may hold.
    pub registrations_per_process: u32,
    /// The least time between two reads of one stream.
    pub throttle: Duration,
    /// Idle time after which a stream reads anyway, which re-checks access.
    pub heartbeat: Duration,
    /// How long a stream lives before it ends cleanly.
    pub lifetime: Duration,
    /// How long the whole open may take.
    pub open_timeout: Duration,
}

impl Default for DataStreamSettings {
    fn default() -> Self {
        Self {
            streams_per_actor: 8,
            streams_per_process: 128,
            registrations_per_actor: 16,
            registrations_per_process: 256,
            throttle: Duration::from_millis(250),
            heartbeat: Duration::from_secs(15),
            lifetime: Duration::from_secs(10 * 60),
            open_timeout: Duration::from_secs(30),
        }
    }
}

/// `Retry-After` seconds when an actor has too many streams.
const ACTOR_RETRY_SECONDS: u32 = 5;
/// `Retry-After` seconds when the process is full or an open took too long.
const PROCESS_RETRY_SECONDS: u32 = 10;

/// Which limit refused a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    Actor,
    Process,
    /// The counts are unusable (a lock was poisoned).
    Broken,
}

fn refusal_response(refusal: Refusal) -> Response {
    let (status, code, seconds, message) = match refusal {
        Refusal::Actor => (
            StatusCode::TOO_MANY_REQUESTS,
            artifree_contracts::api::ErrorCode::RateLimited,
            ACTOR_RETRY_SECONDS,
            "too many data streams are open; close one or retry after the given delay",
        ),
        Refusal::Process | Refusal::Broken => (
            StatusCode::SERVICE_UNAVAILABLE,
            artifree_contracts::api::ErrorCode::Unavailable,
            PROCESS_RETRY_SECONDS,
            "the server cannot take another data stream now; retry after the given delay",
        ),
    };
    retry_after(ApiError::new(status, code, message), seconds)
}

fn retry_after(error: ApiError, seconds: u32) -> Response {
    let mut response = error.into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
    response
}

#[derive(Debug)]
struct Entry {
    id: u64,
    actor: ActorId,
    token: Option<TokenId>,
    revoked: watch::Sender<bool>,
}

#[derive(Debug, Default)]
struct Counts {
    next_id: u64,
    entries: Vec<Entry>,
    places: HashMap<ActorId, u32>,
    places_total: u32,
}

impl Counts {
    fn registrations_of(&self, actor: &ActorId) -> usize {
        self.entries
            .iter()
            .filter(|entry| &entry.actor == actor)
            .count()
    }

    fn check(&self, actor: &ActorId, settings: &DataStreamSettings) -> Result<(), Refusal> {
        let placed = self.places.get(actor).copied().unwrap_or(0);
        if placed >= settings.streams_per_actor
            || self.registrations_of(actor)
                >= usize::try_from(settings.registrations_per_actor).unwrap_or(usize::MAX)
        {
            return Err(Refusal::Actor);
        }
        if self.places_total >= settings.streams_per_process
            || self.entries.len()
                >= usize::try_from(settings.registrations_per_process).unwrap_or(usize::MAX)
        {
            return Err(Refusal::Process);
        }
        Ok(())
    }
}

/// Where a test may act while an open is in progress (decision 0040,
/// item 7). Production code does nothing at these points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Hold {
    /// The request authenticated; nothing is registered yet.
    BeforeRegistration,
    /// Registered; the token is not yet checked again.
    AfterRegistration,
}

#[cfg(test)]
type Hook = Arc<dyn Fn(Hold) + Send + Sync>;

/// What all data streams of the process share.
pub(crate) struct DataStreams {
    wrote: watch::Sender<u64>,
    settings: Mutex<DataStreamSettings>,
    counts: Arc<Mutex<Counts>>,
    reads: AtomicU64,
    #[cfg(test)]
    hook: Mutex<Option<Hook>>,
}

impl std::fmt::Debug for DataStreams {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DataStreams")
            .finish_non_exhaustive()
    }
}

impl DataStreams {
    pub(crate) fn new() -> Self {
        Self {
            wrote: watch::Sender::new(0),
            settings: Mutex::new(DataStreamSettings::default()),
            counts: Arc::default(),
            reads: AtomicU64::new(0),
            #[cfg(test)]
            hook: Mutex::new(None),
        }
    }

    pub(crate) fn settings(&self) -> DataStreamSettings {
        self.settings.lock().map(|s| *s).unwrap_or_default()
    }

    pub(crate) fn set_settings(&self, settings: DataStreamSettings) {
        if let Ok(mut current) = self.settings.lock() {
            *current = settings;
        }
    }

    /// Tells streams that a data write committed. It runs after the commit,
    /// takes no lock a stream holds, and cannot fail.
    pub(crate) fn wrote(&self) {
        self.wrote
            .send_modify(|count| *count = count.wrapping_add(1));
    }

    pub(crate) fn reads(&self) -> u64 {
        self.reads.load(Ordering::Relaxed)
    }

    /// Whether `actor` could take a stream now. It reads no storage.
    fn precheck(&self, actor: &ActorId) -> Result<(), Refusal> {
        let settings = self.settings();
        let counts = self.counts.lock().map_err(|_| Refusal::Broken)?;
        counts.check(actor, &settings)
    }

    /// Registers an open, so that a revocation reaches it. The registration
    /// is removed when it is dropped.
    fn register(
        &self,
        actor: &ActorId,
        token: Option<&TokenId>,
    ) -> Result<(Registration, watch::Receiver<bool>), Refusal> {
        let settings = self.settings();
        let mut counts = self.counts.lock().map_err(|_| Refusal::Broken)?;
        // Registrations are bounded on their own: a request that never
        // reaches a place still counts toward the limits.
        counts.check(actor, &settings)?;
        let id = counts.next_id;
        counts.next_id = counts.next_id.wrapping_add(1);
        let (revoked, receiver) = watch::channel(false);
        counts.entries.push(Entry {
            id,
            actor: actor.clone(),
            token: token.cloned(),
            revoked,
        });
        Ok((
            Registration {
                id,
                counts: Arc::clone(&self.counts),
            },
            receiver,
        ))
    }

    /// Takes a place for a running stream, with the limits checked again at
    /// the moment it is taken.
    fn take_place(&self, actor: &ActorId) -> Result<Place, Refusal> {
        let settings = self.settings();
        let mut counts = self.counts.lock().map_err(|_| Refusal::Broken)?;
        let placed = counts.places.get(actor).copied().unwrap_or(0);
        if placed >= settings.streams_per_actor {
            return Err(Refusal::Actor);
        }
        if counts.places_total >= settings.streams_per_process {
            return Err(Refusal::Process);
        }
        *counts.places.entry(actor.clone()).or_default() += 1;
        counts.places_total += 1;
        Ok(Place {
            counts: Arc::clone(&self.counts),
            actor: actor.clone(),
        })
    }

    /// Ends every stream opened with `token`.
    pub(crate) fn revoke_token(&self, token: &TokenId) {
        if let Ok(counts) = self.counts.lock() {
            for entry in &counts.entries {
                if entry.token.as_ref() == Some(token) {
                    entry.revoked.send_replace(true);
                }
            }
        }
    }

    /// Ends every stream of `actor`.
    pub(crate) fn revoke_actor(&self, actor: &ActorId) {
        if let Ok(counts) = self.counts.lock() {
            for entry in &counts.entries {
                if &entry.actor == actor {
                    entry.revoked.send_replace(true);
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn set_hook(&self, hook: impl Fn(Hold) + Send + Sync + 'static) {
        if let Ok(mut current) = self.hook.lock() {
            *current = Some(Arc::new(hook));
        }
    }

    #[cfg(test)]
    fn hold(&self, point: Hold) {
        let hook = self.hook.lock().ok().and_then(|hook| hook.clone());
        if let Some(hook) = hook {
            hook(point);
        }
    }

    #[cfg(not(test))]
    #[expect(clippy::unused_self, reason = "the test build acts at this point")]
    fn hold(&self, _point: Hold) {}

    /// Registrations and places held now, for tests.
    #[cfg(test)]
    pub(crate) fn held(&self) -> (usize, u32) {
        self.counts.lock().map_or((usize::MAX, u32::MAX), |c| {
            (c.entries.len(), c.places_total)
        })
    }
}

/// One registered open or stream; dropping it removes the entry.
#[derive(Debug)]
struct Registration {
    id: u64,
    counts: Arc<Mutex<Counts>>,
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Ok(mut counts) = self.counts.lock() {
            counts.entries.retain(|entry| entry.id != self.id);
        }
    }
}

/// One running stream of an actor; dropping it frees the place.
#[derive(Debug)]
struct Place {
    counts: Arc<Mutex<Counts>>,
    actor: ActorId,
}

impl Drop for Place {
    fn drop(&mut self) {
        // A poisoned lock refuses every later stream, so the count left
        // behind does not matter.
        if let Ok(mut counts) = self.counts.lock() {
            if let Some(count) = counts.places.get_mut(&self.actor) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    counts.places.remove(&self.actor);
                }
            }
            counts.places_total = counts.places_total.saturating_sub(1);
        }
    }
}

/// Why a wait ended without its result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    Closing,
    Revoked,
    Reader,
    Deadline,
}

/// The signals every await of a stream is raced against, polled first.
struct Race {
    closing: watch::Receiver<bool>,
    revoked: watch::Receiver<bool>,
    deadline: Instant,
    reader: Option<mpsc::Sender<Result<Event, Infallible>>>,
}

impl Race {
    /// Awaits `work` unless shutdown, revocation, the reader leaving, or the
    /// deadline comes first. Cancelling the call leaves nothing half set.
    async fn run<T>(&mut self, work: impl Future<Output = T>) -> Result<T, Ended> {
        let reader = &self.reader;
        tokio::select! {
            biased;
            _ = self.closing.wait_for(|closing| *closing) => Err(Ended::Closing),
            _ = self.revoked.wait_for(|revoked| *revoked) => Err(Ended::Revoked),
            () = async {
                match reader {
                    Some(sender) => sender.closed().await,
                    None => std::future::pending().await,
                }
            } => Err(Ended::Reader),
            () = tokio::time::sleep_until(self.deadline) => Err(Ended::Deadline),
            output = work => Ok(output),
        }
    }
}

/// Process-wide wake-ups: a data write in this process, or an artifact
/// commit (trash, purge, restore) through the change feed.
struct Wake {
    wrote: watch::Receiver<u64>,
    commits: watch::Receiver<()>,
}

impl Wake {
    /// Marks everything announced so far as seen. A read made after this
    /// call includes every write announced before it.
    fn mark_seen(&mut self) {
        let _ = self.wrote.borrow_and_update();
        let _ = self.commits.borrow_and_update();
    }

    /// Returns when something was announced since the last `mark_seen`, or
    /// after `idle`.
    async fn wait(&mut self, idle: Duration) {
        let announced = async {
            let woke = tokio::select! {
                changed = self.wrote.changed() => changed.is_ok(),
                changed = self.commits.changed() => changed.is_ok(),
            };
            if !woke {
                // The senders live as long as the state; never spin.
                std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            () = announced => {}
            () = tokio::time::sleep(idle) => {}
        }
    }
}

/// What a read found.
struct Read {
    version: u64,
    /// The canonical text of the value at the path, if there is one.
    text: Option<String>,
}

/// Reads the watched path. The count of reads is the seam tests use to prove
/// coalescing.
async fn read(
    state: &AppState,
    actor: &ActorId,
    artifact: &ArtifactId,
    path: &DataPath,
) -> Result<Read, ArtifactError> {
    state.data_streams().reads.fetch_add(1, Ordering::Relaxed);
    let (actor, artifact, path) = (actor.clone(), artifact.clone(), path.clone());
    state
        .run(move |artifacts| {
            let read = artifacts.data(&actor, &artifact, &path)?;
            Ok(Read {
                version: read.version.0,
                text: read.value.map(|value| value.to_canonical()),
            })
        })
        .await
}

fn event_of(name: &str, data: &impl serde::Serialize) -> Option<Event> {
    Event::default().event(name).json_data(data).ok()
}

/// What the producer does after an event.
enum Flow {
    Continue,
    Stop,
}

struct Producer {
    state: AppState,
    actor: ActorId,
    artifact: ArtifactId,
    path: DataPath,
    sender: mpsc::Sender<Result<Event, Infallible>>,
    race: Race,
    wake: Wake,
    settings: DataStreamSettings,
    /// The newest version sent on this connection.
    last: u64,
    /// Held until the producer ends.
    _place: Place,
    _registration: Registration,
}

impl Producer {
    async fn run(mut self, first: Read) {
        let _ended: Result<(), Ended> = self.drive(first).await;
    }

    async fn drive(&mut self, first: Read) -> Result<(), Ended> {
        if matches!(self.publish("snapshot", first).await?, Flow::Stop) {
            return Ok(());
        }
        let mut last_read = Instant::now();
        loop {
            self.race
                .run(self.wake.wait(self.settings.heartbeat))
                .await?;
            // A burst while waiting here is one more read, not many.
            self.race
                .run(tokio::time::sleep_until(last_read + self.settings.throttle))
                .await?;
            self.wake.mark_seen();
            last_read = Instant::now();
            let found = self
                .race
                .run(read(&self.state, &self.actor, &self.artifact, &self.path))
                .await?;
            match found {
                Ok(read) if read.version > self.last => {
                    if matches!(self.publish("change", read).await?, Flow::Stop) {
                        return Ok(());
                    }
                }
                // Unchanged: nothing to send.
                Ok(read) if read.version == self.last => {}
                Err(ArtifactError::DataCorrupt(reason)) => {
                    self.send_error(&reason).await?;
                    return Ok(());
                }
                // A version that went back (a restored database), an artifact
                // that is gone (trash, purge), or failing storage: the reader
                // opens again and learns which.
                Ok(_) | Err(_) => return Ok(()),
            }
        }
    }

    async fn publish(&mut self, name: &'static str, read: Read) -> Result<Flow, Ended> {
        self.last = read.version;
        let path = self.path.to_string();
        let artifact_id = self.artifact.to_string();
        let event = if read
            .text
            .as_ref()
            .is_some_and(|text| text.len() > DATA_EVENT_MAX_BYTES)
        {
            event_of(
                "too_large",
                &DataStreamTooLarge {
                    artifact_id,
                    path,
                    data_version: read.version,
                    limit_bytes: DATA_EVENT_MAX_BYTES as u64,
                },
            )
            .map(|event| (event, Flow::Stop))
        } else {
            let value = read.text.map(JsonValue::from_text);
            if matches!(value, Some(None)) {
                return Ok(Flow::Stop);
            }
            let data = AppData {
                artifact_id,
                path,
                data_version: read.version,
                exists: value.is_some(),
                value: value.flatten(),
            };
            event_of(name, &data).map(|event| (event, Flow::Continue))
        };
        let Some((event, flow)) = event else {
            return Ok(Flow::Stop);
        };
        self.send(event).await?;
        Ok(flow)
    }

    async fn send_error(&mut self, reason: &str) -> Result<(), Ended> {
        let body = ErrorResponse {
            error: artifree_contracts::api::ErrorBody {
                code: artifree_contracts::api::ErrorCode::Internal,
                message: format!("stored data is invalid: {reason}"),
                current_revision: None,
                current_metadata_revision: None,
                current_data_version: None,
                conflicting_files: None,
                conflicting_files_total: None,
            },
        };
        if let Some(event) = event_of("error", &body) {
            self.send(event).await?;
        }
        Ok(())
    }

    /// Sends one event, waiting while the reader is behind.
    async fn send(&mut self, event: Event) -> Result<(), Ended> {
        let sender = self.sender.clone();
        let sent = self.race.run(sender.send(Ok(event))).await?;
        sent.map_err(|_| Ended::Reader)
    }
}

/// The events a producer sends, as the response body's stream.
struct Events(mpsc::Receiver<Result<Event, Infallible>>);

impl Stream for Events {
    type Item = Result<Event, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(context)
    }
}

fn ended_response(ended: Ended) -> Response {
    match ended {
        Ended::Revoked => ApiError::unauthenticated().into_response(),
        Ended::Closing => ApiError::unavailable("the server is shutting down").into_response(),
        Ended::Reader => ApiError::unavailable("the reader left").into_response(),
        Ended::Deadline => retry_after(
            ApiError::unavailable("opening the data stream took too long"),
            PROCESS_RETRY_SECONDS,
        ),
    }
}

/// `GET /v1/artifacts/{id}/data/events`.
pub(crate) async fn events(
    State(state): State<AppState>,
    auth: AuthContext,
    headers: HeaderMap,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<DataEventsQuery>, QueryRejection>,
) -> Result<Response, ApiError> {
    let Path(id) = path?;
    let id = operations::artifact_id(&id)?;
    let Query(DataEventsQuery { path: at }) = query?;
    let at = operations::data_path("path", at.as_deref().unwrap_or_default())?;
    if headers.contains_key("last-event-id") {
        return Err(ApiError::invalid(
            "data streams cannot resume; open one again for a new snapshot",
        ));
    }
    // Refused before anything is registered or read.
    if let Err(refusal) = state.data_streams().precheck(&auth.actor) {
        return Ok(refusal_response(refusal));
    }
    Ok(match open(&state, auth, id, at).await {
        Ok(response) | Err(response) => response,
    })
}

/// The open path. Its order is a decision (0040, item 6): register, then
/// re-check the token, then the user, then read, then take the place.
async fn open(
    state: &AppState,
    auth: AuthContext,
    artifact: ArtifactId,
    path: DataPath,
) -> Result<Response, Response> {
    let streams = state.data_streams();
    let settings = streams.settings();
    streams.hold(Hold::BeforeRegistration);
    let (registration, revoked) = streams
        .register(&auth.actor, auth.token_id.as_ref())
        .map_err(refusal_response)?;
    let (sender, receiver) = mpsc::channel(1);
    let mut race = Race {
        closing: state.feed().closing(),
        revoked,
        deadline: Instant::now() + settings.open_timeout,
        reader: None,
    };
    streams.hold(Hold::AfterRegistration);
    // Either this sees a revocation that came before the registration, or the
    // registry walk of the revocation sees the entry.
    if let (Some(id), Some(hash)) = (auth.token_id.clone(), auth.token_hash.clone()) {
        let active = race
            .run(state.identity(move |store| {
                use artifree_ports::token_store::TokenStore as _;
                store.token_is_active(&id, &hash)
            }))
            .await
            .map_err(ended_response)?
            .map_err(IntoResponse::into_response)?;
        match active {
            Ok(true) => {}
            Ok(false) => return Err(ApiError::unauthenticated().into_response()),
            Err(error) => return Err(ApiError::unavailable(error.to_string()).into_response()),
        }
    }
    race.run(state.ensure_current(&auth.actor, auth.generation))
        .await
        .map_err(ended_response)?
        .map_err(IntoResponse::into_response)?;
    let mut wake = Wake {
        wrote: streams.wrote.subscribe(),
        commits: state.feed().commits(),
    };
    wake.mark_seen();
    let first = race
        .run(read(state, &auth.actor, &artifact, &path))
        .await
        .map_err(ended_response)?
        .map_err(|error| ApiError::from(error).into_response())?;
    let place = streams.take_place(&auth.actor).map_err(refusal_response)?;
    race.deadline = Instant::now() + settings.lifetime;
    race.reader = Some(sender.clone());
    let producer = Producer {
        state: state.clone(),
        actor: auth.actor,
        artifact,
        path,
        sender,
        race,
        wake,
        settings,
        last: 0,
        _place: place,
        _registration: registration,
    };
    tokio::spawn(producer.run(first));
    Ok(Sse::new(Events(receiver))
        .keep_alive(KeepAlive::new().interval(settings.heartbeat))
        .into_response())
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU32;

    use artifree_domain::identity::TokenId;
    use artifree_ports::token_store::{NewTokenRecord, TokenStore as _};
    use artifree_providers::storage::SqliteStore;
    use artifree_providers::tokens::{SqliteTokenStore, hash_token_secret};
    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt as _;

    use super::*;
    use crate::auth::AccessToken;
    use crate::http::{self, Settings};

    type TestResult = Result<(), Box<dyn Error>>;

    const OWNER: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const SECRET: &str = "art_first";

    struct Rig {
        directory: PathBuf,
        state: AppState,
        api: Router,
        artifact: String,
        token: TokenId,
    }

    impl Rig {
        async fn new() -> Result<Self, Box<dyn Error>> {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let directory = std::env::temp_dir().join(format!(
                "artifree-data-stream-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let _removed = std::fs::remove_dir_all(&directory);
            std::fs::create_dir_all(&directory)?;
            let database = directory.join("db.sqlite3");
            let state = AppState::new(
                SqliteStore::open(&database)?,
                SqliteTokenStore::open(&database)?,
                AccessToken::parse(OWNER).ok_or("fixture token")?,
                Settings {
                    content_origin: "http://127.0.0.1:7411".into(),
                    ..Settings::default()
                },
            )?;
            let token = TokenId::from_random([3; 16]);
            state.token_store().create(&NewTokenRecord {
                id: token.clone(),
                actor: ActorId::owner(),
                name: "agent".into(),
                secret_hash: hash_token_secret(SECRET),
                prefix: "art_...".into(),
                created_at: "2026-11-01T00:00:00Z".into(),
            })?;
            let api = http::router(state.clone());
            let created = api
                .clone()
                .oneshot(
                    Request::post("/v1/artifacts")
                        .header("authorization", format!("Bearer {OWNER}"))
                        .header("content-type", "application/json")
                        .header("idempotency-key", "create-stream-rig")
                        .body(Body::from(
                            r#"{"title":"T","files":[{"path":"index.html","encoding":"utf8","content":"x"}]}"#,
                        ))?,
                )
                .await?;
            let bytes = axum::body::to_bytes(created.into_body(), usize::MAX).await?;
            let body: serde_json::Value = serde_json::from_slice(&bytes)?;
            let artifact = body["artifact"]["id"]
                .as_str()
                .ok_or("artifact id")?
                .to_owned();
            Ok(Self {
                directory,
                state,
                api,
                artifact,
                token,
            })
        }

        async fn open(&self) -> Result<Response, Box<dyn Error>> {
            Ok(self
                .api
                .clone()
                .oneshot(
                    Request::get(format!("/v1/artifacts/{}/data/events", self.artifact))
                        .header("authorization", format!("Bearer {SECRET}"))
                        .body(Body::empty())?,
                )
                .await?)
        }

        /// A second handle on the database, for a hook that acts mid-open.
        fn other_store(&self) -> Result<SqliteTokenStore, Box<dyn Error>> {
            Ok(SqliteTokenStore::open(&self.directory.join("db.sqlite3"))?)
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _removed = std::fs::remove_dir_all(&self.directory);
        }
    }

    /// Runs `test` on a runtime with ONE blocking thread, so that a parked
    /// blocking task stalls every storage call.
    fn on_a_runtime_with_one_blocking_thread<F: Future<Output = TestResult>>(
        test: F,
    ) -> TestResult {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()?
            .block_on(test)
    }

    /// Parks the blocking pool until the returned sender is dropped or used.
    fn park_blocking_pool() -> std::sync::mpsc::Sender<()> {
        let (release, parked) = std::sync::mpsc::channel::<()>();
        drop(tokio::task::spawn_blocking(move || {
            let _ = parked.recv();
        }));
        release
    }

    async fn until(mut done: impl FnMut() -> bool) -> TestResult {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !done() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .map_err(|_| "the condition did not hold within 5 s".into())
    }

    #[tokio::test]
    async fn a_token_in_good_standing_opens_a_stream_and_holds_one_of_each() -> TestResult {
        let rig = Rig::new().await?;
        let response = rig.open().await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(rig.state.data_streams().held(), (1, 1));
        drop(response);
        until(|| rig.state.data_streams().held() == (0, 0)).await
    }

    #[tokio::test]
    async fn a_token_revoked_between_authentication_and_registration_opens_nothing() -> TestResult {
        let rig = Rig::new().await?;
        let store = rig.other_store()?;
        let token = rig.token.clone();
        rig.state.data_streams().set_hook(move |point| {
            if point == Hold::BeforeRegistration {
                store.revoke(&token, "2026-11-01T00:10:00Z").ok();
            }
        });
        let response = rig.open().await?;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(rig.state.data_streams().held(), (0, 0));
        Ok(())
    }

    #[tokio::test]
    async fn a_token_revoked_between_registration_and_the_recheck_opens_nothing() -> TestResult {
        let rig = Rig::new().await?;
        let store = rig.other_store()?;
        let token = rig.token.clone();
        rig.state.data_streams().set_hook(move |point| {
            if point == Hold::AfterRegistration {
                store.revoke(&token, "2026-11-01T00:10:00Z").ok();
            }
        });
        let response = rig.open().await?;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(rig.state.data_streams().held(), (0, 0));
        Ok(())
    }

    #[tokio::test]
    async fn a_token_rotated_between_registration_and_the_recheck_opens_nothing() -> TestResult {
        let rig = Rig::new().await?;
        let store = rig.other_store()?;
        let token = rig.token.clone();
        rig.state.data_streams().set_hook(move |point| {
            if point == Hold::AfterRegistration {
                store
                    .rotate(&token, &hash_token_secret("art_second"), "art_...")
                    .ok();
            }
        });
        let response = rig.open().await?;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(rig.state.data_streams().held(), (0, 0));
        Ok(())
    }

    #[test]
    fn a_stalled_blocking_pool_cannot_keep_an_open_after_revocation() -> TestResult {
        on_a_runtime_with_one_blocking_thread(async {
            let rig = Rig::new().await?;
            let release = std::sync::Mutex::new(None);
            let hold = Arc::new(release);
            let sink = Arc::clone(&hold);
            rig.state.data_streams().set_hook(move |point| {
                if point == Hold::AfterRegistration
                    && let Ok(mut slot) = sink.lock()
                {
                    *slot = Some(park_blocking_pool());
                }
            });
            let token = rig.token.clone();
            let streams = &rig.state;
            let opening = rig.open();
            let revoking = async {
                until(|| streams.data_streams().held().0 == 1).await?;
                streams.data_streams().revoke_token(&token);
                Ok::<(), Box<dyn Error>>(())
            };
            let (response, revoked) = tokio::time::timeout(Duration::from_secs(5), async {
                tokio::join!(opening, revoking)
            })
            .await
            .map_err(|_| "the open stayed stuck behind the blocking pool")?;
            revoked?;
            assert_eq!(response?.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(rig.state.data_streams().held(), (0, 0));
            Ok(())
        })
    }

    #[test]
    fn the_open_timeout_releases_everything_and_says_when_to_retry() -> TestResult {
        on_a_runtime_with_one_blocking_thread(async {
            let rig = Rig::new().await?;
            rig.state.set_data_stream_settings(DataStreamSettings {
                open_timeout: Duration::from_millis(100),
                ..DataStreamSettings::default()
            });
            let release = Arc::new(std::sync::Mutex::new(None));
            let sink = Arc::clone(&release);
            rig.state.data_streams().set_hook(move |point| {
                if point == Hold::AfterRegistration
                    && let Ok(mut slot) = sink.lock()
                {
                    *slot = Some(park_blocking_pool());
                }
            });
            let response = tokio::time::timeout(Duration::from_secs(5), rig.open())
                .await
                .map_err(|_| "the open did not time out")??;
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(
                response
                    .headers()
                    .get(header::RETRY_AFTER)
                    .map(HeaderValue::as_bytes),
                Some(b"10".as_slice())
            );
            assert_eq!(rig.state.data_streams().held(), (0, 0));
            Ok(())
        })
    }

    #[test]
    fn a_stalled_read_cannot_keep_a_running_stream_after_revocation() -> TestResult {
        on_a_runtime_with_one_blocking_thread(async {
            let rig = Rig::new().await?;
            rig.state.set_data_stream_settings(DataStreamSettings {
                throttle: Duration::from_millis(5),
                ..DataStreamSettings::default()
            });
            let response = rig.open().await?;
            assert_eq!(response.status(), StatusCode::OK);
            let _release = park_blocking_pool();
            let reads = rig.state.data_stream_reads();
            rig.state.data_streams().wrote();
            // The producer counts its read, then waits for a pool that is stuck.
            until(|| rig.state.data_stream_reads() > reads).await?;
            rig.state.data_streams().revoke_token(&rig.token);
            until(|| rig.state.data_streams().held() == (0, 0)).await?;
            drop(response);
            Ok(())
        })
    }

    #[test]
    fn registrations_and_places_are_bounded_and_freed() -> Result<(), Box<dyn Error>> {
        let streams = DataStreams::new();
        streams.set_settings(DataStreamSettings {
            streams_per_actor: 1,
            streams_per_process: 2,
            registrations_per_actor: 2,
            registrations_per_process: 3,
            ..DataStreamSettings::default()
        });
        let alice: ActorId = "u-00000000000000000000000000000001".parse()?;
        let bob: ActorId = "u-00000000000000000000000000000002".parse()?;
        let first = streams.register(&alice, None).map_err(|_| "first")?;
        let second = streams.register(&alice, None).map_err(|_| "second")?;
        assert_eq!(
            streams.register(&alice, None).err(),
            Some(Refusal::Actor),
            "a third registration of one actor"
        );
        let third = streams.register(&bob, None).map_err(|_| "bob")?;
        assert_eq!(
            streams.register(&bob, None).err(),
            Some(Refusal::Process),
            "a fourth registration of the process"
        );
        drop((first, second, third));
        assert_eq!(streams.held(), (0, 0));

        let place = streams.take_place(&alice).map_err(|_| "place")?;
        assert_eq!(streams.take_place(&alice).err(), Some(Refusal::Actor));
        let other = streams.take_place(&bob).map_err(|_| "bob's place")?;
        let carol: ActorId = "u-00000000000000000000000000000003".parse()?;
        assert_eq!(streams.take_place(&carol).err(), Some(Refusal::Process));
        assert_eq!(streams.precheck(&alice).err(), Some(Refusal::Actor));
        drop((place, other));
        assert_eq!(streams.held(), (0, 0));
        assert!(
            streams.take_place(&alice).is_ok(),
            "a freed place is reusable"
        );
        Ok(())
    }
}
