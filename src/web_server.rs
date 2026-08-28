//! Production composition root for hosted browser multiplayer.
//!
//! The HTTP API owns guest sessions and private-room lifecycle. WebSocket and
//! WebTransport tasks must complete the reliable, one-time ticket admission
//! exchange before their bounded endpoint is attached to a room's
//! [`crate::authority_peer_hub::AuthorityPeerHub`].

mod config;
mod rate_limit;
mod webtransport;

use std::fmt::Write as _;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, State};
use axum::http::header::{
    ACCESS_CONTROL_ALLOW_HEADERS, ACCESS_CONTROL_ALLOW_METHODS, ACCESS_CONTROL_ALLOW_ORIGIN,
    AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE, ORIGIN, RETRY_AFTER, SEC_WEBSOCKET_PROTOCOL,
    STRICT_TRANSPORT_SECURITY, VARY,
};
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use serde::Serialize;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, watch};
use tracing::{info, warn};

pub use config::{
    WebDeploymentMode, WebServerConfig, WebServerConfigError, WebTransportListenerConfig,
};
pub use rate_limit::{WebRateLimitConfig, WebRateLimitConfigError};

use rate_limit::{RateBucket, RateLimited, WebRateLimiter};

use crate::network_protocol::SimTick;
use crate::release_identity::current_release_identity;
use crate::web_api::{
    AFC_LOBBY_WEBSOCKET_SUBPROTOCOL, ApiErrorBody, ApiErrorEnvelope, CreateRoomRequest,
    GuestSessionRequest, GuestSessionResponse, JoinRoomRequest, KickMemberRequest,
    LobbyClientMessage, LobbyServerMessage, LobbySnapshotResponse, PublicRoomSummary,
    ResultAckRequest, RoomMemberResponse, RoomResponse, RoomResultResponse, RoomState,
    RoomWorkerPhase, RoomWorkerResponse, SelectCharacterRequest, ServiceConfigResponse,
    SetReadyRequest, StartRoomRequest, TicketModeRequest, TicketRequest, TicketResponse,
    UpdateRoomSettingsRequest, WEB_API_VERSION,
};
use crate::web_endpoint_adapters::{
    AFC_WEBSOCKET_SUBPROTOCOL, ServerDatagramBridge, WebEndpointConfig,
    acknowledge_websocket_admission, receive_websocket_admission, run_websocket_datagram_adapter,
};
use crate::web_identity::JoinTicketMode;
use crate::web_room::{
    WebLobbyPush, WebLobbySnapshotView, WebPrivateRoomOptions, WebPrivateRoomState,
    WebPrivateRoomView, WebPublicRoomView, WebRoomError, WebRoomMemberView, WebRoomResultView,
    WebRoomService, WebRoomWorkerPhase, WebRoomWorkerSnapshot,
};

const MAX_BEARER_BYTES: usize = 512;
const WS_POLICY_VIOLATION: u16 = 1008;

#[derive(Default)]
pub(super) struct WebServerMetrics {
    http_requests: AtomicU64,
    rejected_origins: AtomicU64,
    rate_limited: AtomicU64,
    guest_sessions_issued: AtomicU64,
    rooms_created: AtomicU64,
    rooms_joined: AtomicU64,
    rooms_started: AtomicU64,
    rooms_returned: AtomicU64,
    tickets_issued: AtomicU64,
    lobby_websocket_active: AtomicUsize,
    lobby_websocket_admitted: AtomicU64,
    lobby_websocket_rejected: AtomicU64,
    chat_messages_accepted: AtomicU64,
    chat_messages_rejected: AtomicU64,
    chat_reports: AtomicU64,
    websocket_active: AtomicUsize,
    websocket_admitted: AtomicU64,
    websocket_rejected: AtomicU64,
    websocket_adapter_errors: AtomicU64,
    webtransport_active: AtomicUsize,
    webtransport_admitted: AtomicU64,
    webtransport_rejected: AtomicU64,
    webtransport_adapter_errors: AtomicU64,
    transport_active: AtomicUsize,
}

#[derive(Clone)]
pub struct WebServerState {
    rooms: WebRoomService,
    endpoint: WebEndpointConfig,
    admission_timeout: Duration,
    allowed_origins: Arc<[String]>,
    trusted_proxy_ips: Arc<[IpAddr]>,
    public_lobby_websocket_url: Arc<str>,
    public_websocket_url: Arc<str>,
    public_webtransport_url: Option<Arc<str>>,
    deployment: WebDeploymentMode,
    rate_limiter: Arc<WebRateLimiter>,
    metrics: Arc<WebServerMetrics>,
    ready: Arc<AtomicBool>,
    maximum_transport_sessions: usize,
}

impl WebServerState {
    pub fn new(config: &WebServerConfig) -> Result<Self, WebServerConfigError> {
        config.validate()?;
        let rooms = WebRoomService::new(config.token_keyring.clone(), config.room)
            .map_err(|_| WebServerConfigError::InvalidRoomConfiguration)?;
        let rate_limiter = WebRateLimiter::new(config.rate_limit)
            .map_err(|_| WebServerConfigError::InvalidRateLimitConfiguration)?;
        let public_lobby_websocket_url = config
            .public_websocket_url
            .strip_suffix("/v2/connect/ws")
            .map(|base| format!("{base}/v2/lobby/ws"))
            .ok_or(WebServerConfigError::InvalidPublicUrl)?;
        Ok(Self {
            rooms,
            endpoint: config.endpoint,
            admission_timeout: config.admission_timeout,
            allowed_origins: config.allowed_origins.clone().into(),
            trusted_proxy_ips: config.trusted_proxy_ips.clone().into(),
            public_lobby_websocket_url: Arc::from(public_lobby_websocket_url),
            public_websocket_url: Arc::from(config.public_websocket_url.as_str()),
            public_webtransport_url: config.public_webtransport_url.as_deref().map(Arc::from),
            deployment: config.deployment,
            rate_limiter: Arc::new(rate_limiter),
            metrics: Arc::new(WebServerMetrics::default()),
            ready: Arc::new(AtomicBool::new(true)),
            maximum_transport_sessions: config.maximum_transport_sessions,
        })
    }

    pub fn room_service(&self) -> &WebRoomService {
        &self.rooms
    }

    fn origin_allowed(&self, origin: &str) -> bool {
        self.allowed_origins.iter().any(|allowed| allowed == origin)
    }

    fn rate_limit(&self, address: IpAddr, bucket: RateBucket, now: u64) -> Result<(), ApiError> {
        self.rate_limiter
            .check(address, bucket, now)
            .map_err(|limited| {
                self.metrics.rate_limited.fetch_add(1, Ordering::Relaxed);
                ApiError::rate_limited(limited)
            })
    }

    fn set_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::Release);
    }
}

pub fn web_router(state: WebServerState, maximum_body_bytes: usize) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/metrics", get(metrics))
        .route("/v2/config", get(service_config).options(preflight))
        .route("/v2/guests", post(issue_guest).options(preflight))
        .route("/v2/lobby", get(lobby_snapshot).options(preflight))
        .route("/v2/lobby/ws", get(lobby_websocket_upgrade))
        .route("/v2/rooms", post(create_room).options(preflight))
        .route("/v2/rooms/join", post(join_room).options(preflight))
        .route("/v2/rooms/{room_code}", get(room_status).options(preflight))
        .route(
            "/v2/rooms/{room_code}/settings",
            patch(update_room_settings).options(preflight),
        )
        .route(
            "/v2/rooms/{room_code}/members/self/character",
            patch(select_character).options(preflight),
        )
        .route(
            "/v2/rooms/{room_code}/members/self/ready",
            patch(set_ready).options(preflight),
        )
        .route(
            "/v2/rooms/{room_code}/members/kick",
            post(kick_member).options(preflight),
        )
        .route(
            "/v2/rooms/{room_code}/start",
            post(start_room).options(preflight),
        )
        .route(
            "/v2/rooms/{room_code}/results/ack",
            post(acknowledge_result).options(preflight),
        )
        .route(
            "/v2/rooms/{room_code}/leave",
            post(leave_room).options(preflight),
        )
        .route(
            "/v2/rooms/{room_code}/tickets",
            post(issue_ticket).options(preflight),
        )
        .route("/v2/connect/ws", get(gameplay_websocket_upgrade))
        .layer(DefaultBodyLimit::max(maximum_body_bytes))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            security_boundary,
        ))
        .with_state(state)
}

pub async fn run_web_server_until<F>(
    config: WebServerConfig,
    shutdown: F,
) -> Result<(), WebServerRunError>
where
    F: Future<Output = ()> + Send + 'static,
{
    config.validate()?;
    let state = WebServerState::new(&config)?;
    let bound_webtransport = match config.webtransport.as_ref() {
        Some(config) => Some(webtransport::bind_webtransport_listener(config).await?),
        None => None,
    };
    let router = web_router(state.clone(), config.maximum_request_body_bytes);
    let listener = TcpListener::bind(config.http_bind)
        .await
        .map_err(WebServerRunError::HttpBind)?;
    let bound_http = listener.local_addr().map_err(WebServerRunError::HttpBind)?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut http_shutdown = shutdown_rx.clone();
    let mut http = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            while !*http_shutdown.borrow() {
                if http_shutdown.changed().await.is_err() {
                    break;
                }
            }
        })
        .await
    });
    let mut webtransport = if let Some(listener) = bound_webtransport {
        let wt_state = state.clone();
        let wt_shutdown = shutdown_rx.clone();
        Some(tokio::spawn(async move {
            listener.run(wt_state, wt_shutdown).await
        }))
    } else {
        None
    };
    info!(http_bind = %bound_http, "afc-web-server ready");

    tokio::pin!(shutdown);
    let stop = if let Some(webtransport) = webtransport.as_mut() {
        tokio::select! {
            biased;
            result = &mut http => WebServerStop::Http(result),
            result = webtransport => WebServerStop::WebTransport(result),
            _ = &mut shutdown => WebServerStop::Requested,
        }
    } else {
        tokio::select! {
            biased;
            result = &mut http => WebServerStop::Http(result),
            _ = &mut shutdown => WebServerStop::Requested,
        }
    };
    let (http_completed, webtransport_completed, stop_error) = match stop {
        WebServerStop::Requested => (false, false, None),
        WebServerStop::Http(result) => (true, false, Some(unexpected_http_stop(result))),
        WebServerStop::WebTransport(result) => {
            (false, true, Some(unexpected_webtransport_stop(result)))
        }
    };
    state.set_ready(false);
    let _ = shutdown_tx.send(true);
    state.rooms.shutdown_all().await;

    let http_result = if http_completed {
        Ok(())
    } else {
        finish_http_task(http).await
    };
    let webtransport_result = if webtransport_completed {
        Ok(())
    } else if let Some(webtransport) = webtransport {
        finish_webtransport_task(webtransport).await
    } else {
        Ok(())
    };
    if let Some(error) = stop_error {
        return Err(error);
    }
    http_result?;
    webtransport_result?;
    Ok(())
}

type HttpTaskResult = Result<Result<(), std::io::Error>, tokio::task::JoinError>;
type WebTransportTaskResult = Result<Result<(), WebServerRunError>, tokio::task::JoinError>;

enum WebServerStop {
    Requested,
    Http(HttpTaskResult),
    WebTransport(WebTransportTaskResult),
}

fn unexpected_http_stop(result: HttpTaskResult) -> WebServerRunError {
    match result {
        Ok(Ok(())) => WebServerRunError::HttpStopped,
        Ok(Err(error)) => WebServerRunError::HttpServe(error),
        Err(_) => WebServerRunError::TaskCancelled,
    }
}

fn unexpected_webtransport_stop(result: WebTransportTaskResult) -> WebServerRunError {
    match result {
        Ok(Ok(())) => WebServerRunError::WebTransportStopped,
        Ok(Err(error)) => error,
        Err(_) => WebServerRunError::TaskCancelled,
    }
}

async fn finish_http_task(
    mut task: tokio::task::JoinHandle<Result<(), std::io::Error>>,
) -> Result<(), WebServerRunError> {
    match tokio::time::timeout(Duration::from_secs(10), &mut task).await {
        Ok(Ok(Ok(()))) => Ok(()),
        Ok(Ok(Err(error))) => Err(WebServerRunError::HttpServe(error)),
        Ok(Err(_)) => Err(WebServerRunError::TaskCancelled),
        Err(_) => {
            task.abort();
            let _ = task.await;
            Err(WebServerRunError::ShutdownTimeout)
        }
    }
}

async fn finish_webtransport_task(
    mut task: tokio::task::JoinHandle<Result<(), WebServerRunError>>,
) -> Result<(), WebServerRunError> {
    match tokio::time::timeout(Duration::from_secs(10), &mut task).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err(WebServerRunError::TaskCancelled),
        Err(_) => {
            task.abort();
            let _ = task.await;
            Err(WebServerRunError::ShutdownTimeout)
        }
    }
}

#[derive(Debug)]
pub enum WebServerRunError {
    Config(WebServerConfigError),
    HttpBind(std::io::Error),
    HttpServe(std::io::Error),
    HttpStopped,
    WebTransport(String),
    WebTransportStopped,
    TaskCancelled,
    ShutdownTimeout,
}

impl std::fmt::Display for WebServerRunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "afc-web-server failed: {self:?}")
    }
}

impl std::error::Error for WebServerRunError {}

impl From<WebServerConfigError> for WebServerRunError {
    fn from(error: WebServerConfigError) -> Self {
        Self::Config(error)
    }
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn ready(State(state): State<WebServerState>) -> Response {
    if state.ready.load(Ordering::Acquire) {
        (StatusCode::OK, Json(HealthResponse { status: "ready" })).into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(HealthResponse { status: "draining" }),
        )
            .into_response()
    }
}

async fn metrics(State(state): State<WebServerState>) -> Response {
    let metrics = &state.metrics;
    let rooms = state.rooms.operational_snapshot();
    let mut body = String::with_capacity(1_024);
    for (name, value) in [
        (
            "afc_web_http_requests_total",
            metrics.http_requests.load(Ordering::Relaxed),
        ),
        (
            "afc_web_rejected_origins_total",
            metrics.rejected_origins.load(Ordering::Relaxed),
        ),
        (
            "afc_web_rate_limited_total",
            metrics.rate_limited.load(Ordering::Relaxed),
        ),
        (
            "afc_web_guest_sessions_issued_total",
            metrics.guest_sessions_issued.load(Ordering::Relaxed),
        ),
        (
            "afc_web_rooms_created_total",
            metrics.rooms_created.load(Ordering::Relaxed),
        ),
        (
            "afc_web_rooms_joined_total",
            metrics.rooms_joined.load(Ordering::Relaxed),
        ),
        (
            "afc_web_rooms_started_total",
            metrics.rooms_started.load(Ordering::Relaxed),
        ),
        (
            "afc_web_rooms_returned_total",
            metrics.rooms_returned.load(Ordering::Relaxed),
        ),
        (
            "afc_web_join_tickets_issued_total",
            metrics.tickets_issued.load(Ordering::Relaxed),
        ),
        (
            "afc_web_lobby_websocket_admitted_total",
            metrics.lobby_websocket_admitted.load(Ordering::Relaxed),
        ),
        (
            "afc_web_lobby_websocket_rejected_total",
            metrics.lobby_websocket_rejected.load(Ordering::Relaxed),
        ),
        (
            "afc_web_chat_messages_accepted_total",
            metrics.chat_messages_accepted.load(Ordering::Relaxed),
        ),
        (
            "afc_web_chat_messages_rejected_total",
            metrics.chat_messages_rejected.load(Ordering::Relaxed),
        ),
        (
            "afc_web_chat_reports_total",
            metrics.chat_reports.load(Ordering::Relaxed),
        ),
        (
            "afc_web_websocket_admitted_total",
            metrics.websocket_admitted.load(Ordering::Relaxed),
        ),
        (
            "afc_web_websocket_rejected_total",
            metrics.websocket_rejected.load(Ordering::Relaxed),
        ),
        (
            "afc_web_websocket_adapter_errors_total",
            metrics.websocket_adapter_errors.load(Ordering::Relaxed),
        ),
        (
            "afc_web_webtransport_admitted_total",
            metrics.webtransport_admitted.load(Ordering::Relaxed),
        ),
        (
            "afc_web_webtransport_rejected_total",
            metrics.webtransport_rejected.load(Ordering::Relaxed),
        ),
        (
            "afc_web_webtransport_adapter_errors_total",
            metrics.webtransport_adapter_errors.load(Ordering::Relaxed),
        ),
    ] {
        let _ = writeln!(body, "# TYPE {name} counter\n{name} {value}");
    }
    for (name, value) in [
        (
            "afc_web_websocket_active",
            metrics.websocket_active.load(Ordering::Relaxed) as u64,
        ),
        (
            "afc_web_lobby_websocket_active",
            metrics.lobby_websocket_active.load(Ordering::Relaxed) as u64,
        ),
        (
            "afc_web_webtransport_active",
            metrics.webtransport_active.load(Ordering::Relaxed) as u64,
        ),
        (
            "afc_web_transport_active",
            metrics.transport_active.load(Ordering::Relaxed) as u64,
        ),
        ("afc_web_rooms", rooms.total_rooms),
        ("afc_web_rooms_open", rooms.open_rooms),
        ("afc_web_rooms_starting", rooms.starting_rooms),
        ("afc_web_rooms_active", rooms.active_rooms),
        ("afc_web_rooms_results", rooms.results_rooms),
        ("afc_web_rooms_returning", rooms.returning_rooms),
        ("afc_web_rooms_finished", rooms.finished_rooms),
        ("afc_web_rooms_failed", rooms.failed_rooms),
        ("afc_web_lobby_online_guests", rooms.online_guests),
        ("afc_web_chat_reports_retained", rooms.chat_reports),
        ("afc_web_connected_peers", rooms.connected_peers),
        (
            "afc_web_worker_tick_p99_nanoseconds_max",
            rooms.maximum_worker_tick_p99_ns,
        ),
        (
            "afc_web_worker_tick_nanoseconds_max",
            rooms.maximum_worker_tick_ns,
        ),
        (
            "afc_web_worker_over_budget_observations",
            rooms.worker_over_budget_observations,
        ),
    ] {
        let _ = writeln!(body, "# TYPE {name} gauge\n{name} {value}");
    }
    (
        [(CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
        body,
    )
        .into_response()
}

async fn service_config(State(state): State<WebServerState>) -> Json<ServiceConfigResponse> {
    Json(ServiceConfigResponse {
        api_version: WEB_API_VERSION,
        lobby_websocket_url: state.public_lobby_websocket_url.to_string(),
        gameplay_websocket_url: state.public_websocket_url.to_string(),
        webtransport_url: state
            .public_webtransport_url
            .as_ref()
            .map(ToString::to_string),
        lobby_websocket_subprotocol: AFC_LOBBY_WEBSOCKET_SUBPROTOCOL.to_owned(),
        gameplay_websocket_subprotocol: AFC_WEBSOCKET_SUBPROTOCOL.to_owned(),
        release: current_release_identity().version_line(),
    })
}

async fn issue_guest(
    State(state): State<WebServerState>,
    connect: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<GuestSessionRequest>,
) -> Result<Json<GuestSessionResponse>, ApiError> {
    let now = unix_now()?;
    state.rate_limit(
        client_ip(&state, connect, &headers)?,
        RateBucket::GuestSession,
        now,
    )?;
    let issued = state
        .rooms
        .issue_named_guest_session(&request.nickname, now)
        .map_err(map_room_error)?;
    state
        .metrics
        .guest_sessions_issued
        .fetch_add(1, Ordering::Relaxed);
    Ok(Json(GuestSessionResponse {
        guest_id: issued.issued.claims.guest_id.encoded(),
        nickname: issued.nickname,
        display_name: issued.display_name,
        session_token: issued.issued.token,
        expires_at_unix_seconds: issued.issued.claims.expires_at_unix_seconds,
    }))
}

async fn lobby_snapshot(
    State(state): State<WebServerState>,
    connect: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<LobbySnapshotResponse>, ApiError> {
    let now = unix_now()?;
    state.rate_limit(
        client_ip(&state, connect, &headers)?,
        RateBucket::RoomRead,
        now,
    )?;
    let snapshot = state
        .rooms
        .lobby_snapshot(bearer(&headers)?, now)
        .map_err(map_room_error)?;
    Ok(Json(snapshot.into()))
}

async fn create_room(
    State(state): State<WebServerState>,
    connect: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<CreateRoomRequest>,
) -> Result<Json<RoomResponse>, ApiError> {
    let now = unix_now()?;
    state.rate_limit(
        client_ip(&state, connect, &headers)?,
        RateBucket::RoomMutation,
        now,
    )?;
    let token = bearer(&headers)?;
    let room = state
        .rooms
        .create_private_room(
            token,
            WebPrivateRoomOptions {
                maximum_players: request.maximum_players,
                visibility: request.visibility,
                ..WebPrivateRoomOptions::default()
            },
            now,
        )
        .map_err(map_room_error)?;
    state.metrics.rooms_created.fetch_add(1, Ordering::Relaxed);
    Ok(Json(room.into()))
}

async fn update_room_settings(
    State(state): State<WebServerState>,
    connect: ConnectInfo<SocketAddr>,
    Path(room_code): Path<String>,
    headers: HeaderMap,
    Json(request): Json<UpdateRoomSettingsRequest>,
) -> Result<Json<RoomResponse>, ApiError> {
    let now = unix_now()?;
    state.rate_limit(
        client_ip(&state, connect, &headers)?,
        RateBucket::RoomMutation,
        now,
    )?;
    let room = state
        .rooms
        .update_room_settings(
            bearer(&headers)?,
            &room_code,
            request.expected_revision,
            request.arena_index,
            request.rule_index,
            now,
        )
        .map_err(map_room_error)?;
    Ok(Json(room.into()))
}

async fn select_character(
    State(state): State<WebServerState>,
    connect: ConnectInfo<SocketAddr>,
    Path(room_code): Path<String>,
    headers: HeaderMap,
    Json(request): Json<SelectCharacterRequest>,
) -> Result<Json<RoomResponse>, ApiError> {
    let now = unix_now()?;
    state.rate_limit(
        client_ip(&state, connect, &headers)?,
        RateBucket::RoomMutation,
        now,
    )?;
    let room = state
        .rooms
        .select_character(
            bearer(&headers)?,
            &room_code,
            request.expected_revision,
            request.character,
            now,
        )
        .map_err(map_room_error)?;
    Ok(Json(room.into()))
}

async fn set_ready(
    State(state): State<WebServerState>,
    connect: ConnectInfo<SocketAddr>,
    Path(room_code): Path<String>,
    headers: HeaderMap,
    Json(request): Json<SetReadyRequest>,
) -> Result<Json<RoomResponse>, ApiError> {
    let now = unix_now()?;
    state.rate_limit(
        client_ip(&state, connect, &headers)?,
        RateBucket::RoomMutation,
        now,
    )?;
    let room = state
        .rooms
        .set_ready(
            bearer(&headers)?,
            &room_code,
            request.expected_revision,
            request.ready,
            now,
        )
        .map_err(map_room_error)?;
    Ok(Json(room.into()))
}

async fn kick_member(
    State(state): State<WebServerState>,
    connect: ConnectInfo<SocketAddr>,
    Path(room_code): Path<String>,
    headers: HeaderMap,
    Json(request): Json<KickMemberRequest>,
) -> Result<Json<RoomResponse>, ApiError> {
    let now = unix_now()?;
    state.rate_limit(
        client_ip(&state, connect, &headers)?,
        RateBucket::RoomMutation,
        now,
    )?;
    let peer_id = crate::network_protocol::PeerId::new(request.peer_id)
        .map_err(|_| ApiError::bad_request("invalid_peer_id"))?;
    let room = state
        .rooms
        .kick_and_ban_member(
            bearer(&headers)?,
            &room_code,
            request.expected_revision,
            peer_id,
            now,
        )
        .map_err(map_room_error)?;
    Ok(Json(room.into()))
}

async fn join_room(
    State(state): State<WebServerState>,
    connect: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<JoinRoomRequest>,
) -> Result<Json<RoomResponse>, ApiError> {
    let now = unix_now()?;
    state.rate_limit(
        client_ip(&state, connect, &headers)?,
        RateBucket::RoomMutation,
        now,
    )?;
    let room = state
        .rooms
        .join_private_room(bearer(&headers)?, &request.room_code, now)
        .map_err(map_room_error)?;
    state.metrics.rooms_joined.fetch_add(1, Ordering::Relaxed);
    Ok(Json(room.into()))
}

async fn room_status(
    State(state): State<WebServerState>,
    connect: ConnectInfo<SocketAddr>,
    Path(room_code): Path<String>,
    headers: HeaderMap,
) -> Result<Json<RoomResponse>, ApiError> {
    let now = unix_now()?;
    state.rate_limit(
        client_ip(&state, connect, &headers)?,
        RateBucket::RoomRead,
        now,
    )?;
    let room = state
        .rooms
        .private_room_status(bearer(&headers)?, &room_code, now)
        .map_err(map_room_error)?;
    Ok(Json(room.into()))
}

async fn start_room(
    State(state): State<WebServerState>,
    connect: ConnectInfo<SocketAddr>,
    Path(room_code): Path<String>,
    headers: HeaderMap,
    Json(request): Json<StartRoomRequest>,
) -> Result<Json<RoomResponse>, ApiError> {
    let now = unix_now()?;
    state.rate_limit(
        client_ip(&state, connect, &headers)?,
        RateBucket::RoomMutation,
        now,
    )?;
    let room = state
        .rooms
        .start_private_room(
            bearer(&headers)?,
            &room_code,
            request.expected_revision,
            now,
        )
        .await
        .map_err(map_room_error)?;
    state.metrics.rooms_started.fetch_add(1, Ordering::Relaxed);
    Ok(Json(room.into()))
}

async fn acknowledge_result(
    State(state): State<WebServerState>,
    connect: ConnectInfo<SocketAddr>,
    Path(room_code): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ResultAckRequest>,
) -> Result<Json<RoomResponse>, ApiError> {
    let now = unix_now()?;
    state.rate_limit(
        client_ip(&state, connect, &headers)?,
        RateBucket::RoomMutation,
        now,
    )?;
    let room = state
        .rooms
        .acknowledge_result(bearer(&headers)?, &room_code, &request.match_id, now)
        .map_err(map_room_error)?;
    state.metrics.rooms_returned.fetch_add(1, Ordering::Relaxed);
    Ok(Json(room.into()))
}

async fn leave_room(
    State(state): State<WebServerState>,
    connect: ConnectInfo<SocketAddr>,
    Path(room_code): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let now = unix_now()?;
    state.rate_limit(
        client_ip(&state, connect, &headers)?,
        RateBucket::RoomMutation,
        now,
    )?;
    state
        .rooms
        .leave_private_room(bearer(&headers)?, &room_code, now)
        .map_err(map_room_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn issue_ticket(
    State(state): State<WebServerState>,
    connect: ConnectInfo<SocketAddr>,
    Path(room_code): Path<String>,
    headers: HeaderMap,
    Json(request): Json<TicketRequest>,
) -> Result<Json<TicketResponse>, ApiError> {
    let now = unix_now()?;
    state.rate_limit(
        client_ip(&state, connect, &headers)?,
        RateBucket::RoomMutation,
        now,
    )?;
    let mode = match (request.mode, request.last_confirmed_tick) {
        (TicketModeRequest::Initial, None) => JoinTicketMode::Initial,
        (TicketModeRequest::Reconnect, Some(tick)) => JoinTicketMode::Reconnect {
            last_confirmed_tick: SimTick(tick),
        },
        _ => return Err(ApiError::bad_request("invalid_ticket_mode")),
    };
    let issued = state
        .rooms
        .issue_join_ticket(bearer(&headers)?, &room_code, mode, now)
        .map_err(map_room_error)?;
    state.metrics.tickets_issued.fetch_add(1, Ordering::Relaxed);
    Ok(Json(TicketResponse {
        ticket: issued.ticket,
        expires_at_unix_seconds: issued.expires_at_unix_seconds,
        peer_id: issued.peer_id.get(),
        manifest: issued.manifest,
    }))
}

async fn lobby_websocket_upgrade(
    State(state): State<WebServerState>,
    connect: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    headers
        .get(ORIGIN)
        .and_then(|value| value.to_str().ok())
        .filter(|origin| state.origin_allowed(origin))
        .ok_or_else(|| ApiError::forbidden("origin_rejected"))?;
    if !websocket_protocol_offered(&headers, AFC_LOBBY_WEBSOCKET_SUBPROTOCOL) {
        return Err(ApiError::bad_request("websocket_subprotocol_required"));
    }
    let now = unix_now()?;
    let address = client_ip(&state, connect, &headers)?;
    state.rate_limit(address, RateBucket::Admission, now)?;
    let active =
        OwnedLobbyConnection::try_new(Arc::clone(&state.metrics), state.maximum_transport_sessions)
            .ok_or_else(|| ApiError::unavailable("lobby_capacity"))?;
    Ok(upgrade
        .protocols([AFC_LOBBY_WEBSOCKET_SUBPROTOCOL])
        .on_upgrade(move |socket| handle_lobby_websocket(socket, state, address, active)))
}

async fn handle_lobby_websocket(
    mut socket: WebSocket,
    state: WebServerState,
    address: IpAddr,
    _active: OwnedLobbyConnection,
) {
    const MAX_LOBBY_MESSAGE_BYTES: usize = 4 * 1_024;
    let first = tokio::time::timeout(state.admission_timeout, socket.recv()).await;
    let token = match first {
        Ok(Some(Ok(Message::Text(text)))) if text.len() <= MAX_LOBBY_MESSAGE_BYTES => {
            match serde_json::from_str::<LobbyClientMessage>(&text) {
                Ok(LobbyClientMessage::Authenticate { session_token })
                    if !session_token.is_empty()
                        && session_token.len() <= MAX_BEARER_BYTES
                        && !session_token.bytes().any(|byte| byte.is_ascii_whitespace()) =>
                {
                    session_token
                }
                _ => {
                    reject_lobby_socket(&mut socket).await;
                    state
                        .metrics
                        .lobby_websocket_rejected
                        .fetch_add(1, Ordering::Relaxed);
                    return;
                }
            }
        }
        _ => {
            reject_lobby_socket(&mut socket).await;
            state
                .metrics
                .lobby_websocket_rejected
                .fetch_add(1, Ordering::Relaxed);
            return;
        }
    };
    let now = match unix_now() {
        Ok(now) => now,
        Err(_) => {
            reject_lobby_socket(&mut socket).await;
            return;
        }
    };
    let mut connection = match state.rooms.connect_lobby(&token, now) {
        Ok(connection) => connection,
        Err(_) => {
            reject_lobby_socket(&mut socket).await;
            state
                .metrics
                .lobby_websocket_rejected
                .fetch_add(1, Ordering::Relaxed);
            return;
        }
    };
    state
        .metrics
        .lobby_websocket_admitted
        .fetch_add(1, Ordering::Relaxed);
    if send_lobby_message(
        &mut socket,
        &LobbyServerMessage::Snapshot {
            snapshot: Box::new(connection.snapshot.into()),
        },
    )
    .await
    .is_err()
    {
        state.rooms.disconnect_lobby(connection.guest_id, now);
        return;
    }

    loop {
        tokio::select! {
            inbound = socket.recv() => {
                let Some(Ok(message)) = inbound else { break; };
                match message {
                    Message::Text(text) if text.len() <= MAX_LOBBY_MESSAGE_BYTES => {
                        let parsed = serde_json::from_str::<LobbyClientMessage>(&text);
                        let Ok(command) = parsed else {
                            let _ = send_lobby_error(&mut socket, None, "invalid_control_message").await;
                            continue;
                        };
                        let now = match unix_now() {
                            Ok(now) => now,
                            Err(_) => break,
                        };
                        match command {
                            LobbyClientMessage::Authenticate { .. } => {
                                let _ = send_lobby_error(&mut socket, None, "already_authenticated").await;
                            }
                            LobbyClientMessage::SendChat { client_nonce, scope, text } => {
                                if state.rate_limit(address, RateBucket::RoomMutation, now).is_err() {
                                    state.metrics.chat_messages_rejected.fetch_add(1, Ordering::Relaxed);
                                    let _ = send_lobby_error(&mut socket, Some(client_nonce), "rate_limited").await;
                                    continue;
                                }
                                match state.rooms.send_chat(&token, scope, &text, now) {
                                    Ok(_) => {
                                        state.metrics.chat_messages_accepted.fetch_add(1, Ordering::Relaxed);
                                        let _ = send_lobby_message(
                                            &mut socket,
                                            &LobbyServerMessage::CommandAccepted { client_nonce },
                                        ).await;
                                    }
                                    Err(error) => {
                                        state.metrics.chat_messages_rejected.fetch_add(1, Ordering::Relaxed);
                                        let _ = send_lobby_error(
                                            &mut socket,
                                            Some(client_nonce),
                                            room_error_code(&error),
                                        ).await;
                                    }
                                }
                            }
                            LobbyClientMessage::Report {
                                client_nonce,
                                message_id,
                                guest_id,
                                category,
                            } => {
                                if state.rate_limit(address, RateBucket::RoomMutation, now).is_err() {
                                    let _ = send_lobby_error(&mut socket, Some(client_nonce), "rate_limited").await;
                                    continue;
                                }
                                match state.rooms.report_chat(
                                    &token,
                                    message_id,
                                    &guest_id,
                                    category,
                                    now,
                                ) {
                                    Ok(()) => {
                                        state.metrics.chat_reports.fetch_add(1, Ordering::Relaxed);
                                        let _ = send_lobby_message(
                                            &mut socket,
                                            &LobbyServerMessage::CommandAccepted { client_nonce },
                                        ).await;
                                    }
                                    Err(error) => {
                                        let _ = send_lobby_error(
                                            &mut socket,
                                            Some(client_nonce),
                                            room_error_code(&error),
                                        ).await;
                                    }
                                }
                            }
                            LobbyClientMessage::Resync => {
                                match state.rooms.lobby_snapshot(&token, now) {
                                    Ok(snapshot) => {
                                        if send_lobby_message(
                                            &mut socket,
                                            &LobbyServerMessage::Snapshot { snapshot: Box::new(snapshot.into()) },
                                        ).await.is_err() {
                                            break;
                                        }
                                    }
                                    Err(error) => {
                                        let _ = send_lobby_error(&mut socket, None, room_error_code(&error)).await;
                                    }
                                }
                            }
                            LobbyClientMessage::Ping { nonce } => {
                                if send_lobby_message(&mut socket, &LobbyServerMessage::Pong { nonce }).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    Message::Ping(payload) => {
                        if socket.send(Message::Pong(payload)).await.is_err() { break; }
                    }
                    Message::Pong(_) => {}
                    Message::Close(_) => break,
                    Message::Binary(_) | Message::Text(_) => {
                        let _ = send_lobby_error(&mut socket, None, "invalid_control_message").await;
                    }
                }
            }
            event = connection.receiver.recv() => {
                match event {
                    Ok(WebLobbyPush::StateChanged { revision }) => {
                        if send_lobby_message(
                            &mut socket,
                            &LobbyServerMessage::StateChanged { revision },
                        ).await.is_err() {
                            break;
                        }
                    }
                    Ok(WebLobbyPush::Chat(message)) => {
                        if state.rooms.chat_visible_to(connection.guest_id, &message)
                            && send_lobby_message(
                                &mut socket,
                                &LobbyServerMessage::ChatMessage { message },
                            ).await.is_err()
                        {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let now = match unix_now() { Ok(now) => now, Err(_) => break };
                        let Ok(snapshot) = state.rooms.lobby_snapshot(&token, now) else { break; };
                        if send_lobby_message(
                            &mut socket,
                            &LobbyServerMessage::Snapshot { snapshot: Box::new(snapshot.into()) },
                        ).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
    state
        .rooms
        .disconnect_lobby(connection.guest_id, unix_now().unwrap_or(now));
}

async fn send_lobby_message(
    socket: &mut WebSocket,
    message: &LobbyServerMessage,
) -> Result<(), ()> {
    let encoded = serde_json::to_string(message).map_err(|_| ())?;
    socket
        .send(Message::Text(encoded.into()))
        .await
        .map_err(|_| ())
}

async fn send_lobby_error(
    socket: &mut WebSocket,
    client_nonce: Option<u64>,
    code: &str,
) -> Result<(), ()> {
    send_lobby_message(
        socket,
        &LobbyServerMessage::Error {
            client_nonce,
            code: code.to_owned(),
        },
    )
    .await
}

async fn reject_lobby_socket(socket: &mut WebSocket) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code: WS_POLICY_VIOLATION,
            reason: "lobby admission rejected".into(),
        })))
        .await;
}

async fn gameplay_websocket_upgrade(
    State(state): State<WebServerState>,
    connect: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let origin = headers
        .get(ORIGIN)
        .and_then(|value| value.to_str().ok())
        .filter(|origin| state.origin_allowed(origin))
        .ok_or_else(|| ApiError::forbidden("origin_rejected"))?;
    let _ = origin;
    if !websocket_protocol_offered(&headers, AFC_WEBSOCKET_SUBPROTOCOL) {
        return Err(ApiError::bad_request("websocket_subprotocol_required"));
    }
    let now = unix_now()?;
    let address = client_ip(&state, connect, &headers)?;
    state.rate_limit(address, RateBucket::Admission, now)?;
    let active = OwnedActiveConnection::try_websocket(
        Arc::clone(&state.metrics),
        state.maximum_transport_sessions,
    )
    .ok_or_else(|| ApiError::unavailable("transport_capacity"))?;
    Ok(upgrade
        .protocols([AFC_WEBSOCKET_SUBPROTOCOL])
        .on_upgrade(move |socket| handle_websocket(socket, state, active)))
}

async fn handle_websocket(
    mut socket: WebSocket,
    state: WebServerState,
    _active: OwnedActiveConnection,
) {
    let admitted = async {
        let ticket = receive_websocket_admission(&mut socket, state.admission_timeout)
            .await
            .map_err(|_| ())?;
        let now = unix_now().map_err(|_| ())?;
        let (endpoint, bridge) = ServerDatagramBridge::pair(state.endpoint).map_err(|_| ())?;
        state
            .rooms
            .admit_join_ticket(&ticket, endpoint, now)
            .await
            .map_err(|_| ())?;
        acknowledge_websocket_admission(&mut socket)
            .await
            .map_err(|_| ())?;
        Ok::<_, ()>(bridge)
    }
    .await;
    let Ok(bridge) = admitted else {
        state
            .metrics
            .websocket_rejected
            .fetch_add(1, Ordering::Relaxed);
        warn!("WebSocket admission rejected");
        let _ = socket
            .send(Message::Close(Some(CloseFrame {
                code: WS_POLICY_VIOLATION,
                reason: "admission rejected".into(),
            })))
            .await;
        return;
    };
    state
        .metrics
        .websocket_admitted
        .fetch_add(1, Ordering::Relaxed);
    if run_websocket_datagram_adapter(socket, bridge)
        .await
        .is_err()
    {
        state
            .metrics
            .websocket_adapter_errors
            .fetch_add(1, Ordering::Relaxed);
        warn!("WebSocket gameplay transport retired with an adapter error");
    }
}

async fn preflight() -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn security_boundary(
    State(state): State<WebServerState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    state.metrics.http_requests.fetch_add(1, Ordering::Relaxed);
    let origin = request
        .headers()
        .get(ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    if request.uri().path().starts_with("/v2/")
        && origin
            .as_deref()
            .is_some_and(|origin| !state.origin_allowed(origin))
    {
        state
            .metrics
            .rejected_origins
            .fetch_add(1, Ordering::Relaxed);
        return ApiError::forbidden("origin_rejected").into_response();
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("cross-origin"),
    );
    if state.deployment == WebDeploymentMode::Production {
        headers.insert(
            STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        );
    }
    if let Some(origin) = origin
        && state.origin_allowed(&origin)
        && let Ok(origin) = HeaderValue::from_str(&origin)
    {
        headers.insert(ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        headers.insert(VARY, HeaderValue::from_static("Origin"));
        headers.insert(
            ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST, PATCH, OPTIONS"),
        );
        headers.insert(
            ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("Authorization, Content-Type"),
        );
    }
    response
}

impl From<WebRoomMemberView> for RoomMemberResponse {
    fn from(member: WebRoomMemberView) -> Self {
        Self {
            peer_id: member.peer_id.get(),
            display_name: member.display_name,
            is_host: member.is_host,
            is_self: member.is_self,
            present: member.present,
            gameplay_connected: member.gameplay_connected,
            character: member.character,
            ready: member.ready,
        }
    }
}

impl From<WebRoomResultView> for RoomResultResponse {
    fn from(result: WebRoomResultView) -> Self {
        Self {
            match_id: result.match_id,
            result_id: result.result_id,
            final_tick: result.final_tick,
            final_state_hash: result.final_state_hash,
        }
    }
}

impl From<WebRoomWorkerSnapshot> for RoomWorkerResponse {
    fn from(worker: WebRoomWorkerSnapshot) -> Self {
        Self {
            phase: worker_phase(worker.phase),
            network_tick: worker.network_tick.get(),
            simulation_tick: worker.simulation_tick.get(),
            connected_peers: worker.connected_peers,
        }
    }
}

impl From<WebPrivateRoomView> for RoomResponse {
    fn from(room: WebPrivateRoomView) -> Self {
        Self {
            room_code: room.room_code.to_string(),
            revision: room.revision,
            match_epoch: room.match_epoch,
            visibility: room.visibility,
            state: room_state(room.state),
            maximum_players: room.maximum_players,
            member_count: room.member_count,
            arena_index: room.arena_index,
            rule_index: room.rule_index,
            members: room.members.into_iter().map(Into::into).collect(),
            room_chat: room.room_chat,
            manifest: room.manifest,
            result: room.result.map(Into::into),
            worker: room.worker.map(Into::into),
        }
    }
}

impl From<WebPublicRoomView> for PublicRoomSummary {
    fn from(room: WebPublicRoomView) -> Self {
        Self {
            room_code: room.room_code.to_string(),
            revision: room.revision,
            host_display_name: room.host_display_name,
            state: room_state(room.state),
            member_count: room.member_count,
            maximum_players: room.maximum_players,
            arena_index: room.arena_index,
            rule_index: room.rule_index,
        }
    }
}

impl From<WebLobbySnapshotView> for LobbySnapshotResponse {
    fn from(snapshot: WebLobbySnapshotView) -> Self {
        Self {
            revision: snapshot.revision,
            online_guests: snapshot.online_guests,
            public_rooms: snapshot.public_rooms.into_iter().map(Into::into).collect(),
            global_chat: snapshot.global_chat,
            active_room: snapshot.active_room.map(Into::into),
        }
    }
}

const fn room_state(state: WebPrivateRoomState) -> RoomState {
    match state {
        WebPrivateRoomState::Open => RoomState::Open,
        WebPrivateRoomState::Starting => RoomState::Starting,
        WebPrivateRoomState::Active => RoomState::Active,
        WebPrivateRoomState::Results => RoomState::Results,
        WebPrivateRoomState::Returning => RoomState::Returning,
        WebPrivateRoomState::Failed => RoomState::Failed,
    }
}

const fn worker_phase(phase: WebRoomWorkerPhase) -> RoomWorkerPhase {
    match phase {
        WebRoomWorkerPhase::Starting => RoomWorkerPhase::Starting,
        WebRoomWorkerPhase::WaitingForPeers => RoomWorkerPhase::WaitingForPeers,
        WebRoomWorkerPhase::Countdown => RoomWorkerPhase::Countdown,
        WebRoomWorkerPhase::Fighting => RoomWorkerPhase::Fighting,
        WebRoomWorkerPhase::Finished => RoomWorkerPhase::Finished,
        WebRoomWorkerPhase::Draining => RoomWorkerPhase::Draining,
        WebRoomWorkerPhase::Stopped => RoomWorkerPhase::Stopped,
        WebRoomWorkerPhase::Failed => RoomWorkerPhase::Failed,
    }
}

#[derive(Clone, Copy, Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    retry_after_seconds: Option<u64>,
}

impl ApiError {
    const fn bad_request(code: &'static str) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code)
    }

    const fn unauthorized(code: &'static str) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, code)
    }

    const fn forbidden(code: &'static str) -> Self {
        Self::new(StatusCode::FORBIDDEN, code)
    }

    const fn unavailable(code: &'static str) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, code)
    }

    const fn new(status: StatusCode, code: &'static str) -> Self {
        Self {
            status,
            code,
            retry_after_seconds: None,
        }
    }

    const fn rate_limited(limited: RateLimited) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "rate_limited",
            retry_after_seconds: Some(limited.retry_after_seconds),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (
            self.status,
            Json(ApiErrorEnvelope {
                error: ApiErrorBody {
                    code: self.code.to_owned(),
                },
            }),
        )
            .into_response();
        if let Some(seconds) = self.retry_after_seconds
            && let Ok(value) = HeaderValue::from_str(&seconds.to_string())
        {
            response.headers_mut().insert(RETRY_AFTER, value);
        }
        response
    }
}

fn map_room_error(error: WebRoomError) -> ApiError {
    match error {
        WebRoomError::InvalidNickname => ApiError::bad_request("invalid_nickname"),
        WebRoomError::InvalidChatMessage => ApiError::bad_request("invalid_chat_message"),
        WebRoomError::InvalidRoomOptions
        | WebRoomError::InvalidRoomCode
        | WebRoomError::InvalidReconnectTick => ApiError::bad_request("invalid_request"),
        WebRoomError::RoomNotFound | WebRoomError::MemberNotFound => {
            ApiError::new(StatusCode::NOT_FOUND, "room_not_found")
        }
        WebRoomError::GuestNotMember
        | WebRoomError::GuestRoomBanned
        | WebRoomError::HostOnly
        | WebRoomError::CannotKickSelf => ApiError::forbidden("room_access_denied"),
        WebRoomError::RoomFull
        | WebRoomError::GuestIdentityConflict
        | WebRoomError::GuestAlreadyInRoom
        | WebRoomError::RoomNotOpen
        | WebRoomError::TooFewPlayers
        | WebRoomError::MembersNotReady
        | WebRoomError::RevisionConflict
        | WebRoomError::RoomStarting
        | WebRoomError::RoomAlreadyActive
        | WebRoomError::RoomNoLongerStarting
        | WebRoomError::ResultNotAvailable
        | WebRoomError::ResultMismatch
        | WebRoomError::AlreadyConnected
        | WebRoomError::ReconnectRequired
        | WebRoomError::TicketClaimsMismatch => {
            ApiError::new(StatusCode::CONFLICT, "room_state_conflict")
        }
        WebRoomError::ChatRateLimited => {
            ApiError::new(StatusCode::TOO_MANY_REQUESTS, "chat_rate_limited")
        }
        WebRoomError::ChatTargetNotFound => {
            ApiError::new(StatusCode::NOT_FOUND, "chat_target_not_found")
        }
        WebRoomError::GuestNotRegistered | WebRoomError::Identity(_) => {
            ApiError::unauthorized("invalid_guest_session")
        }
        WebRoomError::InvalidConfiguration
        | WebRoomError::RandomnessUnavailable
        | WebRoomError::RegistryFull
        | WebRoomError::RoomCodeExhausted
        | WebRoomError::Worker(_)
        | WebRoomError::WorkerTaskCancelled => ApiError::unavailable("service_unavailable"),
    }
}

fn room_error_code(error: &WebRoomError) -> &'static str {
    map_room_error(error.clone()).code
}

fn bearer(headers: &HeaderMap) -> Result<&str, ApiError> {
    let mut values = headers.get_all(AUTHORIZATION).iter();
    let value = values
        .next()
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::unauthorized("bearer_required"))?;
    if values.next().is_some() {
        return Err(ApiError::unauthorized("bearer_required"));
    }
    let token = value
        .strip_prefix("Bearer ")
        .filter(|token| {
            !token.is_empty()
                && token.len() <= MAX_BEARER_BYTES
                && !token.bytes().any(|byte| byte.is_ascii_whitespace())
        })
        .ok_or_else(|| ApiError::unauthorized("bearer_required"))?;
    Ok(token)
}

fn websocket_protocol_offered(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get_all(SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|protocol| protocol.trim() == expected)
}

fn client_ip(
    state: &WebServerState,
    ConnectInfo(address): ConnectInfo<SocketAddr>,
    headers: &HeaderMap,
) -> Result<IpAddr, ApiError> {
    const MAX_FORWARDED_CHAIN: usize = 16;
    const MAX_FORWARDED_BYTES: usize = 1_024;

    let socket_ip = canonical_ip(address.ip());
    if !state.trusted_proxy_ips.contains(&socket_ip) {
        return Ok(socket_ip);
    }
    let mut chain = Vec::with_capacity(4);
    let mut bytes = 0_usize;
    for value in headers.get_all("x-forwarded-for").iter() {
        let value = value
            .to_str()
            .map_err(|_| ApiError::bad_request("invalid_forwarded_for"))?;
        bytes = bytes.saturating_add(value.len());
        if bytes > MAX_FORWARDED_BYTES {
            return Err(ApiError::bad_request("invalid_forwarded_for"));
        }
        for address in value.split(',').map(str::trim) {
            if address.is_empty() || chain.len() >= MAX_FORWARDED_CHAIN {
                return Err(ApiError::bad_request("invalid_forwarded_for"));
            }
            chain.push(
                address
                    .parse::<IpAddr>()
                    .map(canonical_ip)
                    .map_err(|_| ApiError::bad_request("invalid_forwarded_for"))?,
            );
        }
    }
    if chain.is_empty() {
        return Ok(socket_ip);
    }
    chain.push(socket_ip);
    Ok(chain
        .iter()
        .rev()
        .copied()
        .find(|address| !state.trusted_proxy_ips.contains(address))
        .unwrap_or(chain[0]))
}

pub(super) fn canonical_ip(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(address) => address
            .to_ipv4_mapped()
            .map_or(IpAddr::V6(address), IpAddr::V4),
        address => address,
    }
}

fn unix_now() -> Result<u64, ApiError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| ApiError::unavailable("clock_unavailable"))
}

pub(super) fn unix_now_for_transport() -> Result<u64, ()> {
    unix_now().map_err(|_| ())
}

struct OwnedLobbyConnection {
    metrics: Arc<WebServerMetrics>,
}

impl OwnedLobbyConnection {
    fn try_new(metrics: Arc<WebServerMetrics>, maximum: usize) -> Option<Self> {
        metrics
            .lobby_websocket_active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < maximum).then_some(active.saturating_add(1))
            })
            .ok()?;
        Some(Self { metrics })
    }
}

impl Drop for OwnedLobbyConnection {
    fn drop(&mut self) {
        self.metrics
            .lobby_websocket_active
            .fetch_sub(1, Ordering::AcqRel);
    }
}

pub(super) struct OwnedActiveConnection {
    counter: Arc<WebServerMetrics>,
    webtransport: bool,
}

impl OwnedActiveConnection {
    fn try_websocket(metrics: Arc<WebServerMetrics>, maximum: usize) -> Option<Self> {
        Self::try_reserve(metrics, maximum, false)
    }

    pub(super) fn try_webtransport(metrics: Arc<WebServerMetrics>, maximum: usize) -> Option<Self> {
        Self::try_reserve(metrics, maximum, true)
    }

    fn try_reserve(
        metrics: Arc<WebServerMetrics>,
        maximum: usize,
        webtransport: bool,
    ) -> Option<Self> {
        metrics
            .transport_active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < maximum).then_some(active.saturating_add(1))
            })
            .ok()?;
        if webtransport {
            metrics.webtransport_active.fetch_add(1, Ordering::AcqRel);
        } else {
            metrics.websocket_active.fetch_add(1, Ordering::AcqRel);
        }
        Some(Self {
            counter: metrics,
            webtransport,
        })
    }
}

impl Drop for OwnedActiveConnection {
    fn drop(&mut self) {
        if self.webtransport {
            self.counter
                .webtransport_active
                .fetch_sub(1, Ordering::AcqRel);
        } else {
            self.counter.websocket_active.fetch_sub(1, Ordering::AcqRel);
        }
        self.counter.transport_active.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::connect_info::MockConnectInfo;
    use axum::http::Method;
    use axum::http::header::ACCESS_CONTROL_REQUEST_METHOD;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::protocol::Message as TungsteniteMessage;
    use tower::ServiceExt;

    use crate::browser_online_client::{BrowserOnlineClient, BrowserOnlineClientConfig};
    use crate::match_config::headless_config_from_manifest;
    use crate::network_io::{
        AfcDatagram, InProcessEndpoint, NonBlockingDatagramEndpoint, ReceiveOutcome, SendOutcome,
    };
    use crate::remote_online_client::RemoteOnlineClientPhase;
    use crate::web_admission::{ADMISSION_ACCEPTED_FRAME, encode_admission_request};
    use crate::web_identity::{WebTokenLifetimes, WebTokenSigningKey};

    fn test_config() -> WebServerConfig {
        let current = WebTokenSigningKey::new(1, [0x44; 32]).unwrap();
        WebServerConfig {
            deployment: WebDeploymentMode::Development,
            http_bind: "127.0.0.1:0".parse().unwrap(),
            webtransport: None,
            public_websocket_url: "ws://127.0.0.1:8080/v2/connect/ws".to_owned(),
            public_webtransport_url: None,
            allowed_origins: vec!["https://html-classic.itch.zone".to_owned()],
            trusted_proxy_ips: Vec::new(),
            token_keyring: crate::web_identity::WebTokenKeyring::new(
                current,
                None,
                WebTokenLifetimes::default(),
            )
            .unwrap(),
            room: crate::web_room::WebRoomServiceConfig::default(),
            endpoint: WebEndpointConfig::default(),
            rate_limit: WebRateLimitConfig::default(),
            admission_timeout: Duration::from_secs(2),
            maximum_request_body_bytes: 16 * 1_024,
            maximum_transport_sessions: 64,
        }
    }

    #[tokio::test]
    async fn guest_and_room_http_flow_is_bearer_authenticated_and_cors_exact() {
        let config = test_config();
        let state = WebServerState::new(&config).unwrap();
        let app = web_router(state.clone(), config.maximum_request_body_bytes)
            .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 41_000))));
        let origin = "https://html-classic.itch.zone";
        let guest = app
            .clone()
            .oneshot(
                Request::post("/v2/guests")
                    .header(ORIGIN, origin)
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"nickname":"Host"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(guest.status(), StatusCode::OK);
        assert_eq!(
            guest.headers().get(ACCESS_CONTROL_ALLOW_ORIGIN).unwrap(),
            origin
        );
        let guest: GuestSessionResponse = serde_json::from_slice(
            &axum::body::to_bytes(guest.into_body(), 4_096)
                .await
                .unwrap(),
        )
        .unwrap();

        let room = app
            .clone()
            .oneshot(
                Request::post("/v2/rooms")
                    .header(ORIGIN, origin)
                    .header(AUTHORIZATION, format!("Bearer {}", guest.session_token))
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"maximum_players":2,"visibility":"private"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(room.status(), StatusCode::OK);
        let room: RoomResponse = serde_json::from_slice(
            &axum::body::to_bytes(room.into_body(), 16_384)
                .await
                .unwrap(),
        )
        .unwrap();
        let metrics = app
            .clone()
            .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(metrics.status(), StatusCode::OK);
        let metrics = String::from_utf8(
            axum::body::to_bytes(metrics.into_body(), 32_768)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(metrics.contains("afc_web_rooms 1\n"));
        assert!(metrics.contains("afc_web_rooms_open 1\n"));
        assert!(metrics.contains("afc_web_connected_peers 0\n"));
        assert!(metrics.contains("afc_web_worker_tick_p99_nanoseconds_max 0\n"));
        assert!(metrics.contains("afc_web_websocket_adapter_errors_total 0\n"));
        assert!(metrics.contains("afc_web_webtransport_adapter_errors_total 0\n"));
        let left = app
            .clone()
            .oneshot(
                Request::post(format!("/v2/rooms/{}/leave", room.room_code))
                    .header(ORIGIN, origin)
                    .header(AUTHORIZATION, format!("Bearer {}", guest.session_token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(left.status(), StatusCode::NO_CONTENT);

        let rejected = app
            .clone()
            .oneshot(
                Request::post("/v2/guests")
                    .header(ORIGIN, "https://attacker.example")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"nickname":"Attacker"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::FORBIDDEN);

        let preflight = app
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/v2/rooms")
                    .header(ORIGIN, origin)
                    .header(ACCESS_CONTROL_REQUEST_METHOD, "POST")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(preflight.status(), StatusCode::NO_CONTENT);
        state.rooms.shutdown_all().await;
    }

    #[tokio::test]
    async fn listener_startup_failure_is_returned_before_readiness() {
        let mut config = test_config();
        let absent = std::env::temp_dir().join(format!(
            "afc-absent-webtransport-{}-{}",
            std::process::id(),
            getrandom::u64().unwrap()
        ));
        config.webtransport = Some(WebTransportListenerConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            certificate_pem: absent.join("certificate.pem"),
            private_key_pem: absent.join("private-key.pem"),
        });
        config.public_webtransport_url = Some("https://127.0.0.1:4433/v2/connect/wt".to_owned());

        let result = tokio::time::timeout(
            Duration::from_secs(2),
            run_web_server_until(config, std::future::pending()),
        )
        .await
        .expect("startup failure must not hang");
        assert!(matches!(result, Err(WebServerRunError::WebTransport(_))));
    }

    #[tokio::test]
    async fn requested_shutdown_cleanly_stops_bound_listeners() {
        tokio::time::timeout(
            Duration::from_secs(2),
            run_web_server_until(test_config(), async {}),
        )
        .await
        .expect("requested shutdown must not hang")
        .unwrap();
    }

    #[test]
    fn bearer_parser_is_strict_and_never_accepts_ambiguous_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer abc"));
        assert_eq!(bearer(&headers).unwrap(), "abc");
        headers.append(AUTHORIZATION, HeaderValue::from_static("Bearer def"));
        assert!(bearer(&headers).is_err());
    }

    #[test]
    fn forwarded_client_identity_is_used_only_through_exact_trusted_hops() {
        let mut config = test_config();
        config.trusted_proxy_ips = vec!["127.0.0.1".parse().unwrap(), "10.0.0.2".parse().unwrap()];
        let state = WebServerState::new(&config).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.7, 10.0.0.2"),
        );
        assert_eq!(
            client_ip(
                &state,
                ConnectInfo("127.0.0.1:41000".parse().unwrap()),
                &headers,
            )
            .unwrap(),
            "198.51.100.7".parse::<IpAddr>().unwrap()
        );

        headers.insert("x-forwarded-for", HeaderValue::from_static("not-an-ip"));
        assert!(
            client_ip(
                &state,
                ConnectInfo("127.0.0.1:41000".parse().unwrap()),
                &headers,
            )
            .is_err()
        );
        assert_eq!(
            client_ip(
                &state,
                ConnectInfo("192.0.2.9:41000".parse().unwrap()),
                &headers,
            )
            .unwrap(),
            "192.0.2.9".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn transport_capacity_is_reserved_atomically_across_protocols() {
        let metrics = Arc::new(WebServerMetrics::default());
        let first = OwnedActiveConnection::try_websocket(Arc::clone(&metrics), 1).unwrap();
        assert!(OwnedActiveConnection::try_websocket(Arc::clone(&metrics), 1).is_none());
        assert!(OwnedActiveConnection::try_webtransport(Arc::clone(&metrics), 1).is_none());
        assert_eq!(metrics.websocket_active.load(Ordering::Acquire), 1);
        assert_eq!(metrics.transport_active.load(Ordering::Acquire), 1);
        drop(first);
        let second = OwnedActiveConnection::try_webtransport(Arc::clone(&metrics), 1).unwrap();
        assert_eq!(metrics.webtransport_active.load(Ordering::Acquire), 1);
        assert_eq!(metrics.transport_active.load(Ordering::Acquire), 1);
        drop(second);
        assert_eq!(metrics.transport_active.load(Ordering::Acquire), 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn real_websocket_admission_drives_two_predicted_clients_to_fighting() {
        let config = test_config();
        let state = WebServerState::new(&config).unwrap();
        let app = web_router(state.clone(), config.maximum_request_body_bytes);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await
            .unwrap();
        });

        let now = unix_now().unwrap();
        let host = state.rooms.issue_guest_session(now).unwrap();
        let guest = state.rooms.issue_guest_session(now).unwrap();
        let created = state
            .rooms
            .create_private_room(
                &host.token,
                WebPrivateRoomOptions {
                    maximum_players: 2,
                    ..WebPrivateRoomOptions::default()
                },
                now,
            )
            .unwrap();
        let room_code = created.room_code.to_string();
        let joined = state
            .rooms
            .join_private_room(&guest.token, &room_code, now)
            .unwrap();
        let host_ready = state
            .rooms
            .set_ready(&host.token, &room_code, joined.revision, true, now)
            .unwrap();
        let guest_ready = state
            .rooms
            .set_ready(&guest.token, &room_code, host_ready.revision, true, now)
            .unwrap();
        let active = state
            .rooms
            .start_private_room(&host.token, &room_code, guest_ready.revision, now)
            .await
            .unwrap();
        let manifest = active.manifest.unwrap();
        let host_ticket = state
            .rooms
            .issue_join_ticket(&host.token, &room_code, JoinTicketMode::Initial, now)
            .unwrap();
        let guest_ticket = state
            .rooms
            .issue_join_ticket(&guest.token, &room_code, JoinTicketMode::Initial, now)
            .unwrap();

        let (host_endpoint, host_bridge) = connect_test_websocket(
            address,
            "https://html-classic.itch.zone",
            &host_ticket.ticket,
        )
        .await;
        let (guest_endpoint, guest_bridge) = connect_test_websocket(
            address,
            "https://html-classic.itch.zone",
            &guest_ticket.ticket,
        )
        .await;
        let mut host_client = BrowserOnlineClient::new(
            host_endpoint,
            headless_config_from_manifest(manifest).unwrap(),
            host_ticket.peer_id,
            BrowserOnlineClientConfig::default(),
        )
        .unwrap();
        let mut guest_client = BrowserOnlineClient::new(
            guest_endpoint,
            headless_config_from_manifest(manifest).unwrap(),
            guest_ticket.peer_id,
            BrowserOnlineClientConfig::default(),
        )
        .unwrap();
        host_client.mark_content_loaded();
        guest_client.mark_content_loaded();

        let mut monotonic_micros = 0_u64;
        for _ in 0..360 {
            host_client.service(monotonic_micros);
            guest_client.service(monotonic_micros);
            if host_client.status().phase == RemoteOnlineClientPhase::Fighting
                && guest_client.status().phase == RemoteOnlineClientPhase::Fighting
            {
                break;
            }
            monotonic_micros = monotonic_micros.saturating_add(16_667);
            tokio::time::sleep(Duration::from_millis(17)).await;
        }
        assert_eq!(
            host_client.status().phase,
            RemoteOnlineClientPhase::Fighting,
            "host never entered the canonical fight: {:?}",
            host_client.status()
        );
        assert_eq!(
            guest_client.status().phase,
            RemoteOnlineClientPhase::Fighting,
            "guest never entered the canonical fight: {:?}",
            guest_client.status()
        );
        assert!(
            host_client
                .status()
                .confirmed_tick
                .is_some_and(|tick| tick > SimTick::ZERO)
        );
        assert!(
            guest_client
                .status()
                .confirmed_tick
                .is_some_and(|tick| tick > SimTick::ZERO)
        );

        drop(host_client);
        drop(guest_client);
        state.rooms.shutdown_all().await;
        host_bridge.abort();
        guest_bridge.abort();
        let _ = shutdown_tx.send(());
        server.await.unwrap();
    }

    async fn connect_test_websocket(
        address: SocketAddr,
        origin: &str,
        ticket: &str,
    ) -> (InProcessEndpoint, tokio::task::JoinHandle<()>) {
        let mut request = format!("ws://{address}/v2/connect/ws")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert(ORIGIN, HeaderValue::from_str(origin).unwrap());
        request.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static(AFC_WEBSOCKET_SUBPROTOCOL),
        );
        let (mut socket, response) = tokio_tungstenite::connect_async(request).await.unwrap();
        assert_eq!(
            response.headers().get(SEC_WEBSOCKET_PROTOCOL).unwrap(),
            AFC_WEBSOCKET_SUBPROTOCOL
        );
        socket
            .send(TungsteniteMessage::Binary(
                encode_admission_request(ticket).unwrap().into(),
            ))
            .await
            .unwrap();
        let accepted = socket.next().await.unwrap().unwrap().into_data();
        assert_eq!(accepted.as_ref(), ADMISSION_ACCEPTED_FRAME);

        let (client_endpoint, bridge_endpoint) = InProcessEndpoint::pair(512).unwrap();
        let bridge = tokio::spawn(run_test_websocket_bridge(socket, bridge_endpoint));
        (client_endpoint, bridge)
    }

    async fn run_test_websocket_bridge(
        mut socket: tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        mut endpoint: InProcessEndpoint,
    ) {
        let mut interval = tokio::time::interval(Duration::from_millis(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                message = socket.next() => {
                    let Some(Ok(TungsteniteMessage::Binary(bytes))) = message else {
                        break;
                    };
                    let Ok(datagram) = AfcDatagram::try_from_slice(&bytes) else {
                        break;
                    };
                    if !matches!(endpoint.try_send(datagram), SendOutcome::Sent) {
                        break;
                    }
                }
                _ = interval.tick() => {
                    for _ in 0..64 {
                        match endpoint.try_receive() {
                            ReceiveOutcome::Received(datagram) => {
                                if socket
                                    .send(TungsteniteMessage::Binary(
                                        datagram.as_slice().to_vec().into(),
                                    ))
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                            }
                            ReceiveOutcome::Empty => break,
                            ReceiveOutcome::Disconnected
                            | ReceiveOutcome::Oversized { .. }
                            | ReceiveOutcome::IoError(_) => return,
                        }
                    }
                }
            }
        }
    }
}
