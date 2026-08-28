//! Browser-only lobby, admission, prediction, and presentation owner.
//!
//! Lobby orchestration runs on the JavaScript/Bevy main thread. Canonical
//! gameplay still enters the existing `BrowserOnlineClient`, which services
//! the predicted AFC protocol at the fixed simulation cadence.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt;
use std::rc::Rc;

use bevy::app::AppExit;
use bevy::prelude::*;
use js_sys::{Function, JSON, Reflect};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    AbortController, Event, Headers, MessageEvent, ReadableStreamDefaultReader, Request,
    RequestInit, RequestMode, Response, Url, UrlSearchParams, WebSocket,
};

use crate::arena_defs::{ActiveArena, arena_definitions};
use crate::browser_online_client::{BrowserOnlineClient, BrowserOnlineClientConfig};
use crate::camera::{GameplayCameraControl, PlayerCameraOverride, UiCamera};
use crate::components::PlayerKeyBindings;
use crate::game_state::{MatchState, RULE_PRESETS};
use crate::headless::HeadlessMatchConfig;
use crate::match_config::headless_config_from_manifest;
use crate::match_presentation::{
    ConfirmedMatchPresentation, MatchPresentationPolicy, OnlinePanelMode, PresentationMusicTrack,
    PresentationPhase, PresentationResultSfx, PresentedLocalOutcome,
};
use crate::network_protocol::{MatchManifest, PeerId, RetryDisposition, SeatOwner, SimTick};
use crate::presentation_projection::release_projection_target;
use crate::release_identity::current_release_identity;
use crate::remote_online_client::{
    RemoteOnlineClientPhase, RemoteOnlineClientStatus, RemoteOnlineTerminal,
};
use crate::tick_input::{LocalSeatId, LocalTickInputState};
use crate::user_mode::{UserModeGameplayScene, UserModeState};
use crate::web_api::{
    AFC_LOBBY_WEBSOCKET_SUBPROTOCOL, ApiErrorEnvelope, ChatReportCategory, ChatScope,
    CreateRoomRequest, GuestSessionRequest, GuestSessionResponse, JoinRoomRequest,
    KickMemberRequest, LobbyClientMessage, LobbyServerMessage, LobbySnapshotResponse,
    ResultAckRequest, RoomResponse, RoomState, RoomVisibility, SelectCharacterRequest,
    ServiceConfigResponse, SetReadyRequest, StartRoomRequest, TicketModeRequest, TicketRequest,
    TicketResponse, UpdateRoomSettingsRequest, WEB_API_VERSION, WebCharacter,
};
use crate::web_endpoint_adapters::{
    AFC_WEBSOCKET_SUBPROTOCOL, BrowserDatagramEndpoint, BrowserTransportPreference,
    WebEndpointConfig, connect_browser_admitted_datagram_endpoint,
};

const HTTP_TIMEOUT_MS: i32 = 10_000;
const MAX_HTTP_RESPONSE_BYTES: usize = 64 * 1_024;
const MAX_ASYNC_EVENTS: usize = 32;
const MAX_DOM_ACTIONS_PER_FRAME: usize = 16;
const LOBBY_SYNC_INTERVAL_MS: u64 = 750;
const CONTROL_RECONNECT_BASE_MS: u64 = 500;
const GAMEPLAY_RECONNECT_BASE_MS: u64 = 350;
const MAX_GAMEPLAY_RECONNECT_ATTEMPTS: u8 = 8;
const RESULTS_PRESENTATION_MS: u64 = 6_000;
const BROWSER_PLAYER_CAMERA_ZOOM_SCALE: f32 = 0.82;
const SESSION_STORAGE_KEY: &str = "afc.browser.session.v2";
const DOM_BRIDGE_KEY: &str = "AFC_LOBBY_BRIDGE";

type BrowserClient = BrowserOnlineClient<BrowserDatagramEndpoint>;
type AsyncMailbox = Rc<RefCell<VecDeque<BrowserAsyncEvent>>>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BrowserOnlineScreen {
    #[default]
    Dormant,
    Bootstrapping,
    Identity,
    Lobby,
    Room,
    Connecting,
    Match,
    Returning,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingOperation {
    Bootstrap,
    Guest,
    RoomMutation,
    LeaveRoom,
    ResultAck,
    ConnectInitial,
    ConnectReconnect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RetryPlan {
    Bootstrap,
    InitialConnection,
    Reconnect,
}

#[derive(Resource, Clone, Debug)]
pub struct BrowserOnlineUiSnapshot {
    pub visible: bool,
    pub screen: BrowserOnlineScreen,
    pub compact: bool,
    pub title: String,
    pub details: String,
    pub footer: String,
    pub room: Option<RoomResponse>,
    pub client_status: Option<RemoteOnlineClientStatus>,
    pub pending: bool,
    pub scene_requested: bool,
    pub client_active: bool,
    pub retry_available: bool,
}

impl Default for BrowserOnlineUiSnapshot {
    fn default() -> Self {
        Self {
            visible: false,
            screen: BrowserOnlineScreen::Dormant,
            compact: false,
            title: "ONLINE".to_owned(),
            details: String::new(),
            footer: String::new(),
            room: None,
            client_status: None,
            pending: false,
            scene_requested: false,
            client_active: false,
            retry_available: false,
        }
    }
}

struct ConnectedPayload {
    endpoint: BrowserDatagramEndpoint,
    match_config: HeadlessMatchConfig,
    peer_id: PeerId,
    countdown_start_tick: Option<SimTick>,
}

struct BootstrapPayload {
    service: ServiceConfigResponse,
    restored: Option<(GuestSessionResponse, LobbySnapshotResponse)>,
    discard_stored_session: bool,
}

#[derive(Clone, Debug)]
enum RestCommand {
    Create {
        maximum_players: u8,
        visibility: RoomVisibility,
    },
    Join {
        room_code: String,
    },
    Settings {
        room_code: String,
        expected_revision: u64,
        arena_index: usize,
        rule_index: usize,
    },
    Character {
        room_code: String,
        expected_revision: u64,
        character: WebCharacter,
    },
    Ready {
        room_code: String,
        expected_revision: u64,
        ready: bool,
    },
    Kick {
        room_code: String,
        expected_revision: u64,
        peer_id: u64,
    },
    Start {
        room_code: String,
        expected_revision: u64,
    },
    Leave {
        room_code: String,
    },
    ResultAck {
        room_code: String,
        match_id: String,
    },
}

impl RestCommand {
    const fn pending_operation(&self) -> PendingOperation {
        match self {
            Self::Leave { .. } => PendingOperation::LeaveRoom,
            Self::ResultAck { .. } => PendingOperation::ResultAck,
            _ => PendingOperation::RoomMutation,
        }
    }
}

enum RestPayload {
    Room(RoomResponse),
    Left,
}

enum BrowserAsyncPayload {
    Bootstrap(Result<BootstrapPayload, BrowserOperationError>),
    Guest(Result<GuestSessionResponse, BrowserOperationError>),
    Rest {
        operation: PendingOperation,
        result: Result<RestPayload, BrowserOperationError>,
    },
    Connected {
        reconnect: bool,
        result: Result<ConnectedPayload, BrowserOperationError>,
    },
    Control(LobbyControlEvent),
}

struct BrowserAsyncEvent {
    generation: u64,
    payload: BrowserAsyncPayload,
}

enum LobbyControlEvent {
    Message(LobbyServerMessage),
    Closed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BrowserOperationError {
    status: Option<u16>,
    code: String,
}

impl BrowserOperationError {
    fn local(code: impl Into<String>) -> Self {
        Self {
            status: None,
            code: code.into(),
        }
    }

    fn player_message(&self) -> String {
        match self.code.as_str() {
            "invalid_nickname" => {
                "Use 3–20 letters or numbers; spaces, dots, hyphens, and underscores are allowed."
                    .to_owned()
            }
            "invalid_chat_message" => "That message cannot be sent.".to_owned(),
            "chat_rate_limited" => "Chat is moving too quickly. Wait a moment.".to_owned(),
            "room_not_found" => "That room was not found.".to_owned(),
            "room_full" => "That room is full.".to_owned(),
            "room_banned" => "The room host removed this guest.".to_owned(),
            "room_not_open" | "room_state_conflict" | "revision_conflict" => {
                "The room changed. Its latest state is being loaded.".to_owned()
            }
            "members_not_ready" => "Every present player must be ready.".to_owned(),
            "too_few_players" => "At least two players are required.".to_owned(),
            "host_only" => "Only the room host can do that.".to_owned(),
            "invalid_guest_session" | "guest_not_registered" => {
                "This guest session expired. Choose a nickname again.".to_owned()
            }
            "rate_limited" => "Too many requests. Please wait a moment.".to_owned(),
            "service_unavailable" | "transport_capacity" | "lobby_capacity" => {
                "The game server is temporarily unavailable.".to_owned()
            }
            "incompatible_release" => {
                "The browser build and game server are different releases.".to_owned()
            }
            "request_timeout" => "The game server did not respond in time.".to_owned(),
            _ => "The online operation failed. Please try again.".to_owned(),
        }
    }

    fn invalidates_guest(&self) -> bool {
        matches!(
            self.code.as_str(),
            "invalid_guest_session" | "guest_not_registered"
        ) || matches!(self.status, Some(401))
    }

    fn reconnect_may_retry(&self) -> bool {
        matches!(self.status, None | Some(409) | Some(429) | Some(502..=504))
    }
}

impl fmt::Display for BrowserOperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "browser online operation failed: {}", self.code)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredBrowserSession {
    api_base: String,
    guest: GuestSessionResponse,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum BrowserDomAction {
    SubmitNickname {
        nickname: String,
    },
    CreateRoom {
        maximum_players: u8,
        visibility: RoomVisibility,
    },
    JoinRoom {
        room_code: String,
    },
    UpdateSettings {
        arena_index: usize,
        rule_index: usize,
    },
    SelectCharacter {
        character: WebCharacter,
    },
    SetReady {
        ready: bool,
    },
    KickMember {
        peer_id: u64,
    },
    StartMatch,
    LeaveRoom,
    SendChat {
        scope: ChatScope,
        text: String,
    },
    Report {
        message_id: Option<u64>,
        guest_id: String,
        category: ChatReportCategory,
    },
    Retry,
    LeaveOnline,
}

struct BrowserLobbySocket {
    socket: WebSocket,
    _on_open: Closure<dyn FnMut(Event)>,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_error: Closure<dyn FnMut(Event)>,
    _on_close: Closure<dyn FnMut(Event)>,
}

impl BrowserLobbySocket {
    fn connect(
        url: &str,
        subprotocol: &str,
        session_token: &str,
        generation: u64,
        mailbox: AsyncMailbox,
    ) -> Result<Self, BrowserOperationError> {
        let socket = WebSocket::new_with_str(url, subprotocol)
            .map_err(|_| BrowserOperationError::local("lobby_socket_unavailable"))?;
        let authentication = serde_json::to_string(&LobbyClientMessage::Authenticate {
            session_token: session_token.to_owned(),
        })
        .map_err(|_| BrowserOperationError::local("request_encoding"))?;

        let open_socket = socket.clone();
        let expected_protocol = subprotocol.to_owned();
        let open_mailbox = Rc::clone(&mailbox);
        let on_open = Closure::wrap(Box::new(move |_event: Event| {
            if open_socket.protocol() != expected_protocol
                || open_socket.send_with_str(&authentication).is_err()
            {
                push_async_event(
                    &open_mailbox,
                    BrowserAsyncEvent {
                        generation,
                        payload: BrowserAsyncPayload::Control(LobbyControlEvent::Closed),
                    },
                );
                let _ = open_socket.close_with_code(1008);
            }
        }) as Box<dyn FnMut(Event)>);
        socket.set_onopen(Some(on_open.as_ref().unchecked_ref()));

        let message_mailbox = Rc::clone(&mailbox);
        let message_socket = socket.clone();
        let on_message = Closure::wrap(Box::new(move |event: MessageEvent| {
            let Some(text) = event.data().as_string() else {
                let _ = message_socket.close_with_code(1003);
                return;
            };
            if text.len() > MAX_HTTP_RESPONSE_BYTES {
                let _ = message_socket.close_with_code(1009);
                return;
            }
            let Ok(message) = serde_json::from_str::<LobbyServerMessage>(&text) else {
                let _ = message_socket.close_with_code(1003);
                return;
            };
            push_async_event(
                &message_mailbox,
                BrowserAsyncEvent {
                    generation,
                    payload: BrowserAsyncPayload::Control(LobbyControlEvent::Message(message)),
                },
            );
        }) as Box<dyn FnMut(MessageEvent)>);
        socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        let error_mailbox = Rc::clone(&mailbox);
        let on_error = Closure::wrap(Box::new(move |_event: Event| {
            push_async_event(
                &error_mailbox,
                BrowserAsyncEvent {
                    generation,
                    payload: BrowserAsyncPayload::Control(LobbyControlEvent::Closed),
                },
            );
        }) as Box<dyn FnMut(Event)>);
        socket.set_onerror(Some(on_error.as_ref().unchecked_ref()));

        let close_mailbox = mailbox;
        let on_close = Closure::wrap(Box::new(move |_event: Event| {
            push_async_event(
                &close_mailbox,
                BrowserAsyncEvent {
                    generation,
                    payload: BrowserAsyncPayload::Control(LobbyControlEvent::Closed),
                },
            );
        }) as Box<dyn FnMut(Event)>);
        socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));

        Ok(Self {
            socket,
            _on_open: on_open,
            _on_message: on_message,
            _on_error: on_error,
            _on_close: on_close,
        })
    }

    fn send(&self, message: &LobbyClientMessage) -> Result<(), BrowserOperationError> {
        if self.socket.ready_state() != WebSocket::OPEN {
            return Err(BrowserOperationError::local("lobby_socket_not_ready"));
        }
        let message = serde_json::to_string(message)
            .map_err(|_| BrowserOperationError::local("request_encoding"))?;
        self.socket
            .send_with_str(&message)
            .map_err(|_| BrowserOperationError::local("lobby_socket_closed"))
    }

    fn is_connecting(&self) -> bool {
        self.socket.ready_state() == WebSocket::CONNECTING
    }
}

impl Drop for BrowserLobbySocket {
    fn drop(&mut self) {
        self.socket.set_onopen(None);
        self.socket.set_onmessage(None);
        self.socket.set_onerror(None);
        self.socket.set_onclose(None);
        let _ = self.socket.close_with_code(1000);
    }
}

pub struct BrowserOnlineApplication {
    screen: BrowserOnlineScreen,
    api_base: Option<String>,
    service: Option<ServiceConfigResponse>,
    guest: Option<GuestSessionResponse>,
    lobby: Option<LobbySnapshotResponse>,
    room: Option<RoomResponse>,
    control: Option<BrowserLobbySocket>,
    client: Option<BrowserClient>,
    mailbox: AsyncMailbox,
    generation: u64,
    pending: Option<PendingOperation>,
    next_lobby_sync_ms: u64,
    control_reconnect_due_ms: Option<u64>,
    control_reconnect_attempts: u8,
    gameplay_reconnect_due_ms: Option<u64>,
    gameplay_reconnect_attempts: u8,
    content_marked: bool,
    scene_requested: bool,
    projection_active: bool,
    camera_fighter_id: Option<usize>,
    results_visible_since_ms: Option<u64>,
    acknowledged_match_id: Option<String>,
    request_exit: bool,
    notice: Option<String>,
    fatal_error: Option<String>,
    retry_plan: Option<RetryPlan>,
    next_client_nonce: u64,
    last_dom_json: String,
}

impl Default for BrowserOnlineApplication {
    fn default() -> Self {
        Self {
            screen: BrowserOnlineScreen::Dormant,
            api_base: None,
            service: None,
            guest: None,
            lobby: None,
            room: None,
            control: None,
            client: None,
            mailbox: Rc::new(RefCell::new(VecDeque::with_capacity(MAX_ASYNC_EVENTS))),
            generation: 1,
            pending: None,
            next_lobby_sync_ms: 0,
            control_reconnect_due_ms: None,
            control_reconnect_attempts: 0,
            gameplay_reconnect_due_ms: None,
            gameplay_reconnect_attempts: 0,
            content_marked: false,
            scene_requested: false,
            projection_active: false,
            camera_fighter_id: None,
            results_visible_since_ms: None,
            acknowledged_match_id: None,
            request_exit: false,
            notice: None,
            fatal_error: None,
            retry_plan: None,
            next_client_nonce: 1,
            last_dom_json: String::new(),
        }
    }
}

impl Drop for BrowserOnlineApplication {
    fn drop(&mut self) {
        if let Some(client) = &mut self.client {
            client.stop();
        }
    }
}

impl BrowserOnlineApplication {
    fn begin_bootstrap(&mut self) {
        self.invalidate_operations();
        self.screen = BrowserOnlineScreen::Bootstrapping;
        self.fatal_error = None;
        self.notice = None;
        self.retry_plan = None;
        self.control = None;
        match resolve_api_base() {
            Ok(api_base) => {
                let stored = load_stored_session(&api_base);
                self.api_base = Some(api_base.clone());
                self.pending = Some(PendingOperation::Bootstrap);
                spawn_bootstrap(self.generation, api_base, stored, Rc::clone(&self.mailbox));
            }
            Err(error) => self.fail(error.player_message(), RetryPlan::Bootstrap),
        }
    }

    fn invalidate_operations(&mut self) {
        self.generation = self.generation.saturating_add(1).max(1);
        self.pending = None;
        self.mailbox.borrow_mut().clear();
    }

    fn fail(&mut self, message: String, retry: RetryPlan) {
        self.screen = BrowserOnlineScreen::Error;
        self.pending = None;
        self.fatal_error = Some(message);
        self.retry_plan = Some(retry);
    }

    fn next_nonce(&mut self) -> u64 {
        let nonce = self.next_client_nonce;
        self.next_client_nonce = self.next_client_nonce.saturating_add(1).max(1);
        nonce
    }

    fn open_control_socket(&mut self, now_ms: u64) {
        let (Some(service), Some(guest)) = (&self.service, &self.guest) else {
            return;
        };
        match BrowserLobbySocket::connect(
            &service.lobby_websocket_url,
            &service.lobby_websocket_subprotocol,
            &guest.session_token,
            self.generation,
            Rc::clone(&self.mailbox),
        ) {
            Ok(socket) => {
                self.control = Some(socket);
                self.control_reconnect_due_ms = None;
                self.next_lobby_sync_ms = now_ms.saturating_add(LOBBY_SYNC_INTERVAL_MS);
            }
            Err(error) => self.schedule_control_reconnect(now_ms, error.player_message()),
        }
    }

    fn schedule_control_reconnect(&mut self, now_ms: u64, notice: String) {
        self.control = None;
        let shift = u32::from(self.control_reconnect_attempts.min(5));
        let delay = CONTROL_RECONNECT_BASE_MS.saturating_mul(1_u64 << shift);
        self.control_reconnect_attempts = self.control_reconnect_attempts.saturating_add(1);
        self.control_reconnect_due_ms = Some(now_ms.saturating_add(delay));
        if self.client.is_none() {
            self.notice = Some(notice);
        }
    }

    fn start_guest(&mut self, nickname: String) {
        let Some(api_base) = self.api_base.clone() else {
            self.begin_bootstrap();
            return;
        };
        if self.pending.is_some() {
            return;
        }
        self.pending = Some(PendingOperation::Guest);
        self.notice = None;
        let generation = self.generation;
        let mailbox = Rc::clone(&self.mailbox);
        spawn_local(async move {
            let result = post_json::<_, GuestSessionResponse>(
                &api_base,
                "/v2/guests",
                None,
                &GuestSessionRequest { nickname },
            )
            .await;
            push_async_event(
                &mailbox,
                BrowserAsyncEvent {
                    generation,
                    payload: BrowserAsyncPayload::Guest(result),
                },
            );
        });
    }

    fn start_rest_command(&mut self, command: RestCommand) {
        let (Some(api_base), Some(guest)) = (self.api_base.clone(), self.guest.clone()) else {
            self.begin_bootstrap();
            return;
        };
        if self.pending.is_some() {
            return;
        }
        let operation = command.pending_operation();
        self.pending = Some(operation);
        self.notice = None;
        let generation = self.generation;
        let mailbox = Rc::clone(&self.mailbox);
        spawn_local(async move {
            let result = execute_rest_command(&api_base, &guest.session_token, command).await;
            push_async_event(
                &mailbox,
                BrowserAsyncEvent {
                    generation,
                    payload: BrowserAsyncPayload::Rest { operation, result },
                },
            );
        });
    }

    fn start_connection(&mut self, reconnect: bool, last_confirmed_tick: Option<SimTick>) {
        let (Some(api_base), Some(service), Some(guest), Some(room)) = (
            self.api_base.clone(),
            self.service.clone(),
            self.guest.clone(),
            self.room.clone(),
        ) else {
            self.fail(
                "The online session is incomplete.".to_owned(),
                RetryPlan::Bootstrap,
            );
            return;
        };
        let Some(expected_manifest) = room.manifest else {
            self.notice = Some("Waiting for the authority manifest…".to_owned());
            return;
        };
        let operation = if reconnect {
            PendingOperation::ConnectReconnect
        } else {
            PendingOperation::ConnectInitial
        };
        self.pending = Some(operation);
        self.screen = BrowserOnlineScreen::Connecting;
        self.notice = None;
        let generation = self.generation;
        let mailbox = Rc::clone(&self.mailbox);
        spawn_local(async move {
            let result = issue_ticket_and_connect(
                &api_base,
                &service,
                &guest.session_token,
                &room.room_code,
                expected_manifest,
                reconnect,
                last_confirmed_tick,
            )
            .await;
            push_async_event(
                &mailbox,
                BrowserAsyncEvent {
                    generation,
                    payload: BrowserAsyncPayload::Connected { reconnect, result },
                },
            );
        });
    }

    fn process_async_events(&mut self, world: &mut World, now_ms: u64) {
        for _ in 0..MAX_ASYNC_EVENTS {
            let Some(event) = self.mailbox.borrow_mut().pop_front() else {
                break;
            };
            if event.generation != self.generation {
                continue;
            }
            match event.payload {
                BrowserAsyncPayload::Bootstrap(result) => {
                    if self.pending != Some(PendingOperation::Bootstrap) {
                        continue;
                    }
                    self.pending = None;
                    match result {
                        Ok(payload) => {
                            if payload.discard_stored_session {
                                clear_stored_session();
                            }
                            self.service = Some(payload.service);
                            if let Some((guest, snapshot)) = payload.restored {
                                self.guest = Some(guest);
                                self.apply_lobby_snapshot(world, snapshot, now_ms);
                                self.open_control_socket(now_ms);
                            } else {
                                self.screen = BrowserOnlineScreen::Identity;
                            }
                            self.retry_plan = None;
                        }
                        Err(error) => self.fail(error.player_message(), RetryPlan::Bootstrap),
                    }
                }
                BrowserAsyncPayload::Guest(result) => {
                    if self.pending != Some(PendingOperation::Guest) {
                        continue;
                    }
                    self.pending = None;
                    match result {
                        Ok(guest) => {
                            self.guest = Some(guest);
                            self.save_session();
                            self.screen = BrowserOnlineScreen::Lobby;
                            self.notice = None;
                            self.open_control_socket(now_ms);
                        }
                        Err(error) => {
                            self.screen = BrowserOnlineScreen::Identity;
                            self.notice = Some(error.player_message());
                        }
                    }
                }
                BrowserAsyncPayload::Rest { operation, result } => {
                    if self.pending != Some(operation) {
                        continue;
                    }
                    self.pending = None;
                    match result {
                        Ok(RestPayload::Room(room)) => {
                            let was_result_ack = operation == PendingOperation::ResultAck;
                            if was_result_ack {
                                self.acknowledged_match_id =
                                    room.result.as_ref().map(|result| result.match_id.clone());
                            }
                            self.room = Some(room.clone());
                            if let Some(lobby) = &mut self.lobby {
                                lobby.active_room = Some(room);
                            }
                            self.notice = None;
                            self.request_control_resync();
                            if was_result_ack {
                                self.finish_match_projection(world);
                                self.screen = BrowserOnlineScreen::Returning;
                            } else if self.client.is_none() {
                                self.screen = match self.room.as_ref().map(|room| room.state) {
                                    Some(RoomState::Open) => BrowserOnlineScreen::Room,
                                    Some(RoomState::Active | RoomState::Starting) => {
                                        BrowserOnlineScreen::Connecting
                                    }
                                    Some(
                                        RoomState::Results
                                        | RoomState::Returning
                                        | RoomState::Failed,
                                    ) => BrowserOnlineScreen::Returning,
                                    None => BrowserOnlineScreen::Lobby,
                                };
                            }
                        }
                        Ok(RestPayload::Left) => {
                            self.room = None;
                            if let Some(lobby) = &mut self.lobby {
                                lobby.active_room = None;
                            }
                            self.screen = BrowserOnlineScreen::Lobby;
                            self.notice = Some("You left the room.".to_owned());
                            self.request_control_resync();
                        }
                        Err(error) if error.invalidates_guest() => {
                            clear_stored_session();
                            self.guest = None;
                            self.control = None;
                            self.screen = BrowserOnlineScreen::Identity;
                            self.notice = Some(error.player_message());
                        }
                        Err(error) => {
                            self.notice = Some(error.player_message());
                            self.request_control_resync();
                            if operation == PendingOperation::ResultAck {
                                self.screen = BrowserOnlineScreen::Returning;
                                self.results_visible_since_ms = Some(now_ms);
                            }
                        }
                    }
                }
                BrowserAsyncPayload::Connected { reconnect, result } => {
                    let expected = if reconnect {
                        PendingOperation::ConnectReconnect
                    } else {
                        PendingOperation::ConnectInitial
                    };
                    if self.pending != Some(expected) {
                        continue;
                    }
                    self.pending = None;
                    match result {
                        Ok(payload) if reconnect && self.client.is_some() => {
                            let Some(client) = &mut self.client else {
                                continue;
                            };
                            if client.peer_id() != payload.peer_id
                                || client.manifest() != &payload.match_config.manifest
                                || client.status().countdown_start_tick
                                    != payload.countdown_start_tick
                            {
                                self.fail(
                                    "The reconnect ticket changed match identity.".to_owned(),
                                    RetryPlan::InitialConnection,
                                );
                                continue;
                            }
                            match client.reconnect(payload.endpoint) {
                                Ok(()) => {
                                    self.screen = BrowserOnlineScreen::Match;
                                    self.gameplay_reconnect_attempts = 0;
                                    self.gameplay_reconnect_due_ms = None;
                                    self.retry_plan = None;
                                    self.content_marked = true;
                                }
                                Err(_) => self.fail(
                                    "The predicted client could not reconnect.".to_owned(),
                                    RetryPlan::Reconnect,
                                ),
                            }
                        }
                        Ok(payload) => self.install_fresh_client(world, payload, reconnect),
                        Err(error) if reconnect && error.reconnect_may_retry() => {
                            self.schedule_gameplay_reconnect(now_ms, error.player_message());
                        }
                        Err(error) => self.fail(
                            error.player_message(),
                            if reconnect {
                                RetryPlan::Reconnect
                            } else {
                                RetryPlan::InitialConnection
                            },
                        ),
                    }
                }
                BrowserAsyncPayload::Control(event) => match event {
                    LobbyControlEvent::Closed => {
                        if self.control.is_some() {
                            self.schedule_control_reconnect(
                                now_ms,
                                "Lobby connection interrupted; reconnecting…".to_owned(),
                            );
                        }
                    }
                    LobbyControlEvent::Message(message) => {
                        self.control_reconnect_attempts = 0;
                        self.process_control_message(world, message, now_ms);
                    }
                },
            }
        }
    }

    fn install_fresh_client(
        &mut self,
        world: &mut World,
        payload: ConnectedPayload,
        reconnect: bool,
    ) {
        let setup = payload.match_config.local_setup.clone();
        let arena_index = setup.arena_index;
        let Some(camera_fighter_id) =
            browser_owned_fighter_id(&payload.match_config.manifest, payload.peer_id)
        else {
            self.fail(
                "The match did not assign this browser a fighter.".to_owned(),
                RetryPlan::InitialConnection,
            );
            return;
        };
        let client = match (reconnect, payload.countdown_start_tick) {
            (true, Some(countdown_start_tick)) => BrowserOnlineClient::new_from_reconnect(
                payload.endpoint,
                payload.match_config,
                payload.peer_id,
                countdown_start_tick,
                BrowserOnlineClientConfig::default(),
            ),
            (false, None) => BrowserOnlineClient::new(
                payload.endpoint,
                payload.match_config,
                payload.peer_id,
                BrowserOnlineClientConfig::default(),
            ),
            _ => {
                self.fail(
                    "The reconnect boundary changed while joining.".to_owned(),
                    RetryPlan::Reconnect,
                );
                return;
            }
        };
        match client {
            Ok(client) => {
                world.insert_resource(setup);
                world.resource_mut::<ActiveArena>().select(arena_index);
                self.client = Some(client);
                self.screen = BrowserOnlineScreen::Match;
                self.scene_requested = true;
                self.content_marked = false;
                self.projection_active = true;
                world
                    .resource_mut::<PlayerCameraOverride>()
                    .follow(camera_fighter_id, BROWSER_PLAYER_CAMERA_ZOOM_SCALE);
                self.camera_fighter_id = Some(camera_fighter_id);
                self.retry_plan = None;
                self.gameplay_reconnect_attempts = 0;
                self.gameplay_reconnect_due_ms = None;
                self.results_visible_since_ms = None;
                self.acknowledged_match_id = None;
            }
            Err(_) => self.fail(
                "The predicted browser client could not start.".to_owned(),
                RetryPlan::InitialConnection,
            ),
        }
    }

    fn process_control_message(
        &mut self,
        world: &mut World,
        message: LobbyServerMessage,
        now_ms: u64,
    ) {
        match message {
            LobbyServerMessage::Snapshot { snapshot } => {
                self.apply_lobby_snapshot(world, *snapshot, now_ms);
            }
            LobbyServerMessage::StateChanged { revision } => {
                if self
                    .lobby
                    .as_ref()
                    .is_none_or(|snapshot| snapshot.revision < revision)
                {
                    self.request_control_resync();
                }
            }
            LobbyServerMessage::ChatMessage { message } => {
                if let Some(lobby) = &mut self.lobby {
                    match message.scope {
                        ChatScope::Global => push_unique_chat(&mut lobby.global_chat, message),
                        ChatScope::Room => {
                            if let Some(room) = &mut lobby.active_room {
                                push_unique_chat(&mut room.room_chat, message.clone());
                            }
                            if let Some(room) = &mut self.room {
                                push_unique_chat(&mut room.room_chat, message);
                            }
                        }
                    }
                }
            }
            LobbyServerMessage::CommandAccepted { .. } => {
                self.notice = None;
            }
            LobbyServerMessage::Pong { .. } => {}
            LobbyServerMessage::Error { code, .. } => {
                self.notice = Some(BrowserOperationError { status: None, code }.player_message());
            }
        }
    }

    fn apply_lobby_snapshot(
        &mut self,
        world: &mut World,
        snapshot: LobbySnapshotResponse,
        now_ms: u64,
    ) {
        let prior_room = self.room.as_ref().map(|room| room.room_code.clone());
        self.room = snapshot.active_room.clone();
        self.lobby = Some(snapshot);
        self.save_session();

        if let Some(room_state) = self.room.as_ref().map(|room| room.state) {
            if room_state == RoomState::Open {
                self.acknowledged_match_id = None;
            }
            if room_state == RoomState::Open
                && self.client.is_some()
                && self.results_visible_since_ms.is_some()
            {
                self.finish_match_projection(world);
            }
            if self.client.is_none() && self.pending.is_none() {
                self.screen = match room_state {
                    RoomState::Open => BrowserOnlineScreen::Room,
                    RoomState::Starting | RoomState::Active => BrowserOnlineScreen::Connecting,
                    RoomState::Results | RoomState::Returning | RoomState::Failed => {
                        if self.results_visible_since_ms.is_none() {
                            self.results_visible_since_ms = Some(now_ms);
                        }
                        BrowserOnlineScreen::Returning
                    }
                };
            }
        } else if self.client.is_none() {
            self.screen = BrowserOnlineScreen::Lobby;
            if prior_room.is_some() {
                self.notice = Some("The room is no longer available.".to_owned());
            }
        }
    }

    fn request_control_resync(&self) {
        if let Some(control) = &self.control {
            let _ = control.send(&LobbyClientMessage::Resync);
        }
    }

    fn schedule_gameplay_reconnect(&mut self, now_ms: u64, notice: String) {
        if self.gameplay_reconnect_attempts >= MAX_GAMEPLAY_RECONNECT_ATTEMPTS {
            self.fail(
                "The authority could not be reached after several attempts.".to_owned(),
                RetryPlan::Reconnect,
            );
            return;
        }
        let shift = u32::from(self.gameplay_reconnect_attempts.min(4));
        let delay = GAMEPLAY_RECONNECT_BASE_MS.saturating_mul(1_u64 << shift);
        self.gameplay_reconnect_attempts = self.gameplay_reconnect_attempts.saturating_add(1);
        self.gameplay_reconnect_due_ms = Some(now_ms.saturating_add(delay));
        self.screen = BrowserOnlineScreen::Match;
        self.notice = Some(notice);
    }

    fn service_client(&mut self, world: &mut World, now_micros: u64, now_ms: u64) {
        let scene_ready = world
            .get_resource::<UserModeGameplayScene>()
            .is_some_and(UserModeGameplayScene::ready_for_battle);
        let Some(client) = &mut self.client else {
            return;
        };
        if scene_ready && !self.content_marked {
            client.mark_content_loaded();
            self.content_marked = true;
        }
        let report = client.service(now_micros);
        if scene_ready && let Err(error) = client.project_latest(world) {
            bevy::log::warn!("browser online presentation projection failed: {error:?}");
            self.fail(
                "The authoritative match could not be projected.".to_owned(),
                RetryPlan::InitialConnection,
            );
            return;
        }
        if client.status().phase == RemoteOnlineClientPhase::Results
            && self.results_visible_since_ms.is_none()
        {
            self.results_visible_since_ms = Some(now_ms);
        }
        match report.terminal {
            Some(RemoteOnlineTerminal::AuthorityDisconnected(disconnect)) => {
                if disconnect.message.retry == RetryDisposition::ReconnectAllowed {
                    if self.pending.is_none() && self.gameplay_reconnect_due_ms.is_none() {
                        self.gameplay_reconnect_due_ms =
                            Some(now_ms.saturating_add(GAMEPLAY_RECONNECT_BASE_MS));
                    }
                } else {
                    self.fail(
                        "The authority ended this match.".to_owned(),
                        RetryPlan::InitialConnection,
                    );
                }
            }
            Some(RemoteOnlineTerminal::Failed(_)) => self.fail(
                "The online match encountered a protocol error.".to_owned(),
                RetryPlan::InitialConnection,
            ),
            Some(RemoteOnlineTerminal::Stopped) if self.results_visible_since_ms.is_none() => self
                .fail(
                    "The online match stopped unexpectedly.".to_owned(),
                    RetryPlan::InitialConnection,
                ),
            Some(RemoteOnlineTerminal::Completed(_))
            | Some(RemoteOnlineTerminal::Stopped)
            | None => {}
        }
    }

    fn maybe_schedule_work(&mut self, now_ms: u64) {
        if self.control.is_none()
            && self.guest.is_some()
            && self.service.is_some()
            && self
                .control_reconnect_due_ms
                .is_none_or(|due| now_ms >= due)
        {
            self.open_control_socket(now_ms);
        }
        if self.control.is_some() && now_ms >= self.next_lobby_sync_ms {
            if self
                .control
                .as_ref()
                .is_some_and(BrowserLobbySocket::is_connecting)
            {
                self.next_lobby_sync_ms = now_ms.saturating_add(LOBBY_SYNC_INTERVAL_MS);
            } else {
                let nonce = self.next_nonce();
                let failed = self.control.as_ref().is_some_and(|control| {
                    control.send(&LobbyClientMessage::Ping { nonce }).is_err()
                        || control.send(&LobbyClientMessage::Resync).is_err()
                });
                if failed {
                    self.schedule_control_reconnect(
                        now_ms,
                        "Lobby connection interrupted; reconnecting…".to_owned(),
                    );
                } else {
                    self.next_lobby_sync_ms = now_ms.saturating_add(LOBBY_SYNC_INTERVAL_MS);
                }
            }
        }
        if self.pending.is_some() {
            return;
        }
        if let Some(due) = self.gameplay_reconnect_due_ms
            && now_ms >= due
            && let Some(client) = &self.client
        {
            let last_confirmed = client.status().confirmed_tick.unwrap_or(SimTick::ZERO);
            let reconnect = client.status().countdown_start_tick.is_some();
            self.gameplay_reconnect_due_ms = None;
            self.start_connection(reconnect, reconnect.then_some(last_confirmed));
            return;
        }
        let Some(room) = &self.room else {
            return;
        };
        match room.state {
            RoomState::Active if self.client.is_none() => {
                let reconnect = room
                    .worker
                    .is_some_and(|worker| worker.countdown_start_tick.is_some());
                self.start_connection(reconnect, reconnect.then_some(SimTick::ZERO));
            }
            RoomState::Results
                if self.results_visible_since_ms.is_some_and(|since| {
                    now_ms.saturating_sub(since) >= RESULTS_PRESENTATION_MS
                }) =>
            {
                if let Some(result) = &room.result {
                    if self.acknowledged_match_id.as_deref() == Some(result.match_id.as_str()) {
                        return;
                    }
                    self.start_rest_command(RestCommand::ResultAck {
                        room_code: room.room_code.clone(),
                        match_id: result.match_id.clone(),
                    });
                }
            }
            RoomState::Failed if self.client.is_none() => {
                self.screen = BrowserOnlineScreen::Returning;
                self.notice = Some("The match authority stopped; restoring the room…".to_owned());
            }
            _ => {}
        }
    }

    fn dispatch_dom(&mut self, action: BrowserDomAction, now_ms: u64) {
        match action {
            BrowserDomAction::SubmitNickname { nickname }
                if self.screen == BrowserOnlineScreen::Identity =>
            {
                self.start_guest(nickname);
            }
            BrowserDomAction::CreateRoom {
                maximum_players,
                visibility,
            } if self.screen == BrowserOnlineScreen::Lobby => {
                self.start_rest_command(RestCommand::Create {
                    maximum_players,
                    visibility,
                });
            }
            BrowserDomAction::JoinRoom { room_code }
                if self.screen == BrowserOnlineScreen::Lobby =>
            {
                self.start_rest_command(RestCommand::Join { room_code });
            }
            BrowserDomAction::UpdateSettings {
                arena_index,
                rule_index,
            } => {
                if let Some(room) = &self.room
                    && room.state == RoomState::Open
                    && room.self_is_host()
                {
                    self.start_rest_command(RestCommand::Settings {
                        room_code: room.room_code.clone(),
                        expected_revision: room.revision,
                        arena_index,
                        rule_index,
                    });
                }
            }
            BrowserDomAction::SelectCharacter { character } => {
                if let Some(room) = &self.room
                    && room.state == RoomState::Open
                {
                    self.start_rest_command(RestCommand::Character {
                        room_code: room.room_code.clone(),
                        expected_revision: room.revision,
                        character,
                    });
                }
            }
            BrowserDomAction::SetReady { ready } => {
                if let Some(room) = &self.room
                    && room.state == RoomState::Open
                {
                    self.start_rest_command(RestCommand::Ready {
                        room_code: room.room_code.clone(),
                        expected_revision: room.revision,
                        ready,
                    });
                }
            }
            BrowserDomAction::KickMember { peer_id } => {
                if let Some(room) = &self.room
                    && room.state == RoomState::Open
                    && room.self_is_host()
                {
                    self.start_rest_command(RestCommand::Kick {
                        room_code: room.room_code.clone(),
                        expected_revision: room.revision,
                        peer_id,
                    });
                }
            }
            BrowserDomAction::StartMatch => {
                if let Some(room) = &self.room
                    && room.state == RoomState::Open
                    && room.self_is_host()
                    && room.all_members_ready()
                {
                    self.start_rest_command(RestCommand::Start {
                        room_code: room.room_code.clone(),
                        expected_revision: room.revision,
                    });
                }
            }
            BrowserDomAction::LeaveRoom => {
                if let Some(room) = &self.room
                    && room.state == RoomState::Open
                {
                    self.start_rest_command(RestCommand::Leave {
                        room_code: room.room_code.clone(),
                    });
                }
            }
            BrowserDomAction::SendChat { scope, text } => {
                let nonce = self.next_nonce();
                let failed = self.control.as_ref().is_none_or(|control| {
                    control
                        .send(&LobbyClientMessage::SendChat {
                            client_nonce: nonce,
                            scope,
                            text,
                        })
                        .is_err()
                });
                if failed {
                    self.schedule_control_reconnect(
                        now_ms,
                        "Chat connection interrupted; reconnecting…".to_owned(),
                    );
                }
            }
            BrowserDomAction::Report {
                message_id,
                guest_id,
                category,
            } => {
                let nonce = self.next_nonce();
                if let Some(control) = &self.control {
                    let _ = control.send(&LobbyClientMessage::Report {
                        client_nonce: nonce,
                        message_id,
                        guest_id,
                        category,
                    });
                }
            }
            BrowserDomAction::Retry if self.screen == BrowserOnlineScreen::Error => {
                self.fatal_error = None;
                match self.retry_plan.take() {
                    Some(RetryPlan::Reconnect) if self.client.is_some() => {
                        self.screen = BrowserOnlineScreen::Match;
                        self.gameplay_reconnect_attempts = 0;
                        self.gameplay_reconnect_due_ms = Some(now_ms);
                    }
                    Some(RetryPlan::InitialConnection)
                        if self
                            .room
                            .as_ref()
                            .is_some_and(|room| room.manifest.is_some()) =>
                    {
                        self.start_connection(false, None);
                    }
                    _ => self.begin_bootstrap(),
                }
            }
            BrowserDomAction::LeaveOnline => self.request_exit = true,
            _ => {}
        }
    }

    fn sample_inputs(&mut self, inputs: &mut LocalTickInputState) {
        let Some(client) = &mut self.client else {
            return;
        };
        if client.sample_local_inputs(inputs).is_err() {
            self.fail(
                "Local browser input could not be submitted.".to_owned(),
                RetryPlan::InitialConnection,
            );
        }
    }

    fn accepts_gameplay_input(&self) -> bool {
        self.client.as_ref().is_some_and(|client| {
            matches!(
                client.status().phase,
                RemoteOnlineClientPhase::Fighting | RemoteOnlineClientPhase::ConfirmingResult
            )
        })
    }

    fn begin_best_effort_leave(&self) {
        let (Some(api_base), Some(guest), Some(room)) = (&self.api_base, &self.guest, &self.room)
        else {
            return;
        };
        if room.state != RoomState::Open {
            return;
        }
        let api_base = api_base.clone();
        let token = guest.session_token.clone();
        let code = room.room_code.clone();
        spawn_local(async move {
            let _ =
                post_no_content(&api_base, &format!("/v2/rooms/{code}/leave"), Some(&token)).await;
        });
    }

    fn finish_match_projection(&mut self, world: &mut World) {
        if let Some(client) = &mut self.client {
            client.stop();
        }
        self.client = None;
        if self.projection_active {
            release_browser_projection_target(world);
            self.projection_active = false;
        }
        self.scene_requested = false;
        self.content_marked = false;
        self.camera_fighter_id = None;
        self.results_visible_since_ms = None;
        self.gameplay_reconnect_due_ms = None;
        self.gameplay_reconnect_attempts = 0;
    }

    fn reset_for_exit(&mut self, world: &mut World) {
        self.begin_best_effort_leave();
        self.finish_match_projection(world);
        self.control = None;
        self.invalidate_operations();
        self.screen = BrowserOnlineScreen::Dormant;
        self.api_base = None;
        self.service = None;
        self.guest = None;
        self.lobby = None;
        self.room = None;
        self.notice = None;
        self.fatal_error = None;
        self.retry_plan = None;
        self.acknowledged_match_id = None;
        self.control_reconnect_due_ms = None;
        self.control_reconnect_attempts = 0;
        self.request_exit = false;
    }

    fn save_session(&self) {
        let (Some(api_base), Some(guest)) = (&self.api_base, &self.guest) else {
            return;
        };
        save_stored_session(&StoredBrowserSession {
            api_base: api_base.clone(),
            guest: guest.clone(),
        });
    }

    fn snapshot(&self, visible: bool) -> BrowserOnlineUiSnapshot {
        let client_status = self.client.as_ref().map(BrowserOnlineClient::status);
        let compact = client_status.is_some_and(|status| {
            matches!(
                status.phase,
                RemoteOnlineClientPhase::Countdown
                    | RemoteOnlineClientPhase::Fighting
                    | RemoteOnlineClientPhase::ConfirmingResult
                    | RemoteOnlineClientPhase::Reconnecting
            )
        });
        let (title, mut details) = self.snapshot_text(client_status);
        if let Some(notice) = &self.notice {
            details.push_str("\n\n");
            details.push_str(notice);
        }
        BrowserOnlineUiSnapshot {
            visible,
            screen: self.screen,
            compact,
            title,
            details,
            footer: self.snapshot_footer(client_status),
            room: self.room.clone(),
            client_status,
            pending: self.pending.is_some(),
            scene_requested: self.scene_requested,
            client_active: self.client.is_some(),
            retry_available: self.retry_plan.is_some(),
        }
    }

    fn snapshot_text(&self, status: Option<RemoteOnlineClientStatus>) -> (String, String) {
        if let Some(error) = &self.fatal_error {
            return ("ONLINE ERROR".to_owned(), error.clone());
        }
        if let Some(status) = status {
            let phase = match status.phase {
                RemoteOnlineClientPhase::Connecting => "CONNECTING",
                RemoteOnlineClientPhase::Loading => "LOADING",
                RemoteOnlineClientPhase::Synchronizing => "SYNCHRONIZING",
                RemoteOnlineClientPhase::Ready => "READY",
                RemoteOnlineClientPhase::Countdown => "GET READY",
                RemoteOnlineClientPhase::Fighting => "ONLINE MATCH",
                RemoteOnlineClientPhase::ConfirmingResult => "CONFIRMING RESULT",
                RemoteOnlineClientPhase::Results => "RESULTS",
                RemoteOnlineClientPhase::Reconnecting => "RECONNECTING",
                RemoteOnlineClientPhase::Stopped => "STOPPED",
                RemoteOnlineClientPhase::Failed => "ONLINE ERROR",
            };
            let detail = match status.phase {
                RemoteOnlineClientPhase::Fighting => format!(
                    "Client clock {}  •  predicted {}  •  authority confirmed {}",
                    status.network_tick.get(),
                    status.predicted_tick.unwrap_or(SimTick::ZERO).get(),
                    status.confirmed_tick.unwrap_or(SimTick::ZERO).get()
                ),
                RemoteOnlineClientPhase::Countdown => {
                    "Waiting for the authoritative countdown.".to_owned()
                }
                RemoteOnlineClientPhase::Results => {
                    "The authoritative result is confirmed.".to_owned()
                }
                RemoteOnlineClientPhase::Reconnecting => {
                    "Restoring an authority-retained snapshot…".to_owned()
                }
                _ => "Negotiating the authoritative match…".to_owned(),
            };
            return (phase.to_owned(), detail);
        }
        match self.screen {
            BrowserOnlineScreen::Dormant => ("ONLINE".to_owned(), String::new()),
            BrowserOnlineScreen::Bootstrapping => (
                "ONLINE".to_owned(),
                "Contacting the game server…".to_owned(),
            ),
            BrowserOnlineScreen::Identity => (
                "CHOOSE A NICKNAME".to_owned(),
                "Your guest identity lasts for this browser tab.".to_owned(),
            ),
            BrowserOnlineScreen::Lobby => (
                "ONLINE LOBBY".to_owned(),
                "Chat, join a public room, enter a private code, or create a room.".to_owned(),
            ),
            BrowserOnlineScreen::Room => {
                let details = self.room.as_ref().map_or_else(
                    || "Loading the room…".to_owned(),
                    |room| {
                        format!(
                            "Room {}  •  {}/{} players",
                            room.room_code, room.member_count, room.maximum_players
                        )
                    },
                );
                ("MATCH ROOM".to_owned(), details)
            }
            BrowserOnlineScreen::Connecting => (
                "CONNECTING".to_owned(),
                "Redeeming a short-lived, one-time join ticket…".to_owned(),
            ),
            BrowserOnlineScreen::Match => (
                "ONLINE MATCH".to_owned(),
                "Waiting for the predicted client…".to_owned(),
            ),
            BrowserOnlineScreen::Returning => (
                "RETURNING TO ROOM".to_owned(),
                "The authority is closing this match safely…".to_owned(),
            ),
            BrowserOnlineScreen::Error => (
                "ONLINE ERROR".to_owned(),
                "Online operation failed.".to_owned(),
            ),
        }
    }

    fn snapshot_footer(&self, status: Option<RemoteOnlineClientStatus>) -> String {
        if let Some(status) = status {
            return format!(
                "generation {}  •  predicted {}",
                status.generation,
                status.predicted_tick.unwrap_or(SimTick::ZERO).get()
            );
        }
        self.guest.as_ref().map_or_else(
            || "No guest identity is active.".to_owned(),
            |guest| format!("{}  •  session restored in this tab", guest.display_name),
        )
    }

    fn render_dom(&mut self, visible: bool, now_ms: u64) {
        let state = BrowserDomState::from_application(self, visible, now_ms);
        let Ok(json) = serde_json::to_string(&state) else {
            return;
        };
        if json == self.last_dom_json {
            return;
        }
        if call_dom_bridge("render", &json).is_ok() {
            self.last_dom_json = json;
        }
    }
}

#[derive(Serialize)]
struct BrowserDomGuest<'a> {
    guest_id: &'a str,
    nickname: &'a str,
    display_name: &'a str,
}

#[derive(Serialize)]
struct BrowserDomClient {
    phase: &'static str,
    generation: u64,
    network_tick: u64,
    predicted_tick: Option<u64>,
    confirmed_tick: Option<u64>,
    countdown_start_tick: Option<u64>,
    hard_resync_requests: u64,
    hard_resync_snapshots_applied: u64,
    reconnect_replication_deferred: u64,
    committed_relays: u64,
    state_hash_messages: u64,
    state_delta_messages: u64,
    stale_state_messages: u64,
    matched_authority_states: u64,
    rollback_corrections: u64,
    clock_replies_accepted: u64,
    clock_replies_discarded_rtt: u64,
    received_datagrams: u64,
    receive_budget_exhaustions: u64,
    inbound_queue_overflows: u64,
    last_hard_resync_reason: Option<String>,
    failure_key: Option<&'static str>,
    failure_detail_code: Option<u16>,
}

impl BrowserDomClient {
    fn from_status(status: RemoteOnlineClientStatus) -> Self {
        Self {
            phase: match status.phase {
                RemoteOnlineClientPhase::Connecting => "connecting",
                RemoteOnlineClientPhase::Loading => "loading",
                RemoteOnlineClientPhase::Synchronizing => "synchronizing",
                RemoteOnlineClientPhase::Ready => "ready",
                RemoteOnlineClientPhase::Countdown => "countdown",
                RemoteOnlineClientPhase::Fighting => "fighting",
                RemoteOnlineClientPhase::ConfirmingResult => "confirming_result",
                RemoteOnlineClientPhase::Results => "results",
                RemoteOnlineClientPhase::Reconnecting => "reconnecting",
                RemoteOnlineClientPhase::Stopped => "stopped",
                RemoteOnlineClientPhase::Failed => "failed",
            },
            generation: status.generation,
            network_tick: status.network_tick.get(),
            predicted_tick: status.predicted_tick.map(SimTick::get),
            confirmed_tick: status.confirmed_tick.map(SimTick::get),
            countdown_start_tick: status.countdown_start_tick.map(SimTick::get),
            hard_resync_requests: status.protocol.hard_resync_requests,
            hard_resync_snapshots_applied: status.protocol.hard_resync_snapshots_applied,
            reconnect_replication_deferred: status.protocol.reconnect_replication_deferred,
            committed_relays: status.protocol.committed_relays,
            state_hash_messages: status.protocol.state_hash_messages,
            state_delta_messages: status.protocol.state_delta_messages,
            stale_state_messages: status.protocol.stale_state_messages,
            matched_authority_states: status.protocol.matched_authority_states,
            rollback_corrections: status.protocol.rollback_corrections,
            clock_replies_accepted: status.protocol.clock_replies_accepted,
            clock_replies_discarded_rtt: status.protocol.clock_replies_discarded_rtt,
            received_datagrams: status.runtime.received_datagrams,
            receive_budget_exhaustions: status.runtime.receive_budget_exhaustions,
            inbound_queue_overflows: status.runtime.inbound_queue_overflows,
            last_hard_resync_reason: status
                .last_hard_resync_reason
                .map(|reason| format!("{reason:?}")),
            failure_key: status.failure.map(|failure| failure.message_key()),
            failure_detail_code: status.failure.map(|failure| failure.detail_code),
        }
    }
}

#[derive(Serialize)]
struct BrowserDomState<'a> {
    visible: bool,
    screen: &'static str,
    pending: bool,
    notice: Option<&'a str>,
    error: Option<&'a str>,
    online_guests: u32,
    guest: Option<BrowserDomGuest<'a>>,
    client: Option<BrowserDomClient>,
    camera_fighter_id: Option<usize>,
    public_rooms: &'a [crate::web_api::PublicRoomSummary],
    global_chat: &'a [crate::web_api::ChatMessageResponse],
    room: Option<&'a RoomResponse>,
    arena_names: Vec<&'static str>,
    rule_names: Vec<&'static str>,
    characters: Vec<&'static str>,
    result_return_ms: Option<u64>,
}

impl<'a> BrowserDomState<'a> {
    fn from_application(
        application: &'a BrowserOnlineApplication,
        visible: bool,
        now_ms: u64,
    ) -> Self {
        let lobby = application.lobby.as_ref();
        let empty_rooms = &[];
        let empty_chat = &[];
        Self {
            visible,
            screen: match application.screen {
                BrowserOnlineScreen::Dormant => "dormant",
                BrowserOnlineScreen::Bootstrapping => "bootstrapping",
                BrowserOnlineScreen::Identity => "identity",
                BrowserOnlineScreen::Lobby => "lobby",
                BrowserOnlineScreen::Room => "room",
                BrowserOnlineScreen::Connecting => "connecting",
                BrowserOnlineScreen::Match => "match",
                BrowserOnlineScreen::Returning => "returning",
                BrowserOnlineScreen::Error => "error",
            },
            pending: application.pending.is_some(),
            notice: application.notice.as_deref(),
            error: application.fatal_error.as_deref(),
            online_guests: lobby.map_or(0, |lobby| lobby.online_guests),
            guest: application.guest.as_ref().map(|guest| BrowserDomGuest {
                guest_id: &guest.guest_id,
                nickname: &guest.nickname,
                display_name: &guest.display_name,
            }),
            client: application
                .client
                .as_ref()
                .map(BrowserOnlineClient::status)
                .map(BrowserDomClient::from_status),
            camera_fighter_id: application.camera_fighter_id,
            public_rooms: lobby.map_or(empty_rooms, |lobby| lobby.public_rooms.as_slice()),
            global_chat: lobby.map_or(empty_chat, |lobby| lobby.global_chat.as_slice()),
            room: application.room.as_ref(),
            arena_names: arena_definitions().iter().map(|arena| arena.name).collect(),
            rule_names: RULE_PRESETS.iter().map(|rules| rules.label).collect(),
            characters: WebCharacter::PLAYER_SELECTABLE
                .iter()
                .map(|character| character.label())
                .collect(),
            result_return_ms: application
                .results_visible_since_ms
                .map(|since| RESULTS_PRESENTATION_MS.saturating_sub(now_ms.saturating_sub(since))),
        }
    }
}

pub(crate) fn drive_browser_online_application(world: &mut World) {
    let now_micros = world
        .get_resource::<Time<Real>>()
        .map(|time| time.elapsed().as_micros().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0);
    let now_ms = now_micros / 1_000;
    let online_visible = world
        .get_resource::<UserModeState>()
        .is_some_and(UserModeState::online_active);
    let Some(mut application) = world.remove_non_send_resource::<BrowserOnlineApplication>() else {
        return;
    };

    if application.request_exit
        || (!online_visible && application.screen != BrowserOnlineScreen::Dormant)
    {
        application.reset_for_exit(world);
        if let Some(mut user_mode) = world.get_resource_mut::<UserModeState>() {
            user_mode.leave_online();
        }
    } else if online_visible {
        if application.screen == BrowserOnlineScreen::Dormant {
            application.begin_bootstrap();
        }
        application.process_async_events(world, now_ms);
        application.maybe_schedule_work(now_ms);
        application.service_client(world, now_micros, now_ms);
        application.maybe_schedule_work(now_ms);
    }

    application.render_dom(online_visible, now_ms);
    let snapshot = application.snapshot(online_visible);
    world.insert_non_send_resource(application);
    world.insert_resource(snapshot);
}

fn release_browser_projection_target(world: &mut World) {
    release_projection_target(world);
    if let Some(mut camera) = world.get_resource_mut::<PlayerCameraOverride>() {
        camera.clear();
    }
    if let Some(mut inputs) = world.get_resource_mut::<LocalTickInputState>() {
        inputs.reset_all_sessions();
    }
    if let Some(mut match_state) = world.get_resource_mut::<MatchState>() {
        match_state.return_to_setup();
    }
}

fn browser_owned_fighter_id(manifest: &MatchManifest, peer_id: PeerId) -> Option<usize> {
    manifest
        .ownership
        .as_slice()
        .iter()
        .find(|assignment| assignment.owner == SeatOwner::Peer(peer_id))
        .map(|assignment| usize::from(assignment.fighter.get()))
}

pub(crate) fn offline_local_input_enabled(snapshot: Res<BrowserOnlineUiSnapshot>) -> bool {
    !snapshot.client_active
}

pub(crate) fn sample_browser_online_render_input(
    keys: Res<ButtonInput<KeyCode>>,
    gamepads: Query<(Entity, &Gamepad)>,
    camera: Res<GameplayCameraControl>,
    bindings: Res<PlayerKeyBindings>,
    mut inputs: ResMut<LocalTickInputState>,
    mut application: NonSendMut<BrowserOnlineApplication>,
) {
    if application.client.is_none() {
        return;
    }
    let seat = LocalSeatId::new(0).expect("browser guest owns local ordinal zero");
    if !application.accepts_gameplay_input() {
        inputs.reset_all_input();
        return;
    }
    if let Some(binding) = bindings.bindings_for_player(0) {
        inputs.merge_render_sample(
            seat,
            crate::fighter::sample_bound_tick_input(&keys, camera.yaw, binding, false),
        );
    }
    if let Some((_, gamepad)) = gamepads.iter().min_by_key(|(entity, _)| entity.index()) {
        inputs.merge_render_sample(
            seat,
            crate::fighter::sample_gamepad_tick_input(gamepad, camera.yaw),
        );
    }
    application.sample_inputs(&mut inputs);
}

pub(crate) fn teardown_browser_online_on_exit(
    mut exits: MessageReader<AppExit>,
    mut application: NonSendMut<BrowserOnlineApplication>,
) {
    if exits.read().next().is_some() {
        application.begin_best_effort_leave();
        if let Some(client) = &mut application.client {
            client.stop();
        }
    }
}

pub(crate) fn derive_match_presentation_policy(
    snapshot: Res<BrowserOnlineUiSnapshot>,
    active_arena: Res<ActiveArena>,
    confirmed: Option<Res<ConfirmedMatchPresentation>>,
    mut policy: ResMut<MatchPresentationPolicy>,
) {
    let next = browser_presentation_policy(&snapshot, active_arena.index(), confirmed.as_deref());
    if *policy != next {
        *policy = next;
    }
}

fn browser_presentation_policy(
    snapshot: &BrowserOnlineUiSnapshot,
    arena_index: usize,
    confirmed: Option<&ConfirmedMatchPresentation>,
) -> MatchPresentationPolicy {
    if !snapshot.visible {
        return MatchPresentationPolicy::default();
    }
    let Some(status) = snapshot.client_status else {
        return MatchPresentationPolicy {
            phase: if snapshot.screen == BrowserOnlineScreen::Room {
                PresentationPhase::Lobby
            } else if snapshot.screen == BrowserOnlineScreen::Error {
                PresentationPhase::Error
            } else {
                PresentationPhase::Menu
            },
            panel: OnlinePanelMode::Full,
            gameplay_hud_visible: false,
            music: if snapshot.screen == BrowserOnlineScreen::Error {
                PresentationMusicTrack::None
            } else {
                PresentationMusicTrack::Menu
            },
            result_sfx: None,
        };
    };
    let (phase, panel, hud, music) = match status.phase {
        RemoteOnlineClientPhase::Countdown => (
            PresentationPhase::Countdown,
            OnlinePanelMode::CountdownStrip,
            true,
            PresentationMusicTrack::Arena(arena_index),
        ),
        RemoteOnlineClientPhase::Fighting => (
            PresentationPhase::Fighting,
            OnlinePanelMode::FightStrip,
            true,
            PresentationMusicTrack::Arena(arena_index),
        ),
        RemoteOnlineClientPhase::Reconnecting => (
            PresentationPhase::Reconnecting,
            OnlinePanelMode::ReconnectStrip,
            true,
            PresentationMusicTrack::Arena(arena_index),
        ),
        RemoteOnlineClientPhase::ConfirmingResult => (
            PresentationPhase::ConfirmingResult,
            OnlinePanelMode::ConfirmingStrip,
            true,
            PresentationMusicTrack::Arena(arena_index),
        ),
        RemoteOnlineClientPhase::Results => (
            PresentationPhase::Results,
            OnlinePanelMode::Results,
            false,
            PresentationMusicTrack::None,
        ),
        RemoteOnlineClientPhase::Failed | RemoteOnlineClientPhase::Stopped => (
            PresentationPhase::Error,
            OnlinePanelMode::Full,
            false,
            PresentationMusicTrack::None,
        ),
        _ => (
            PresentationPhase::Menu,
            OnlinePanelMode::Full,
            false,
            PresentationMusicTrack::Menu,
        ),
    };
    let result_sfx = if status.phase == RemoteOnlineClientPhase::Results {
        confirmed.map(|result| {
            let kind = match result.local_outcome {
                PresentedLocalOutcome::Victory | PresentedLocalOutcome::Mixed => {
                    PresentationResultSfx::Victory
                }
                _ => PresentationResultSfx::Defeat,
            };
            (result.key, kind)
        })
    } else {
        None
    };
    MatchPresentationPolicy {
        phase,
        panel,
        gameplay_hud_visible: hud,
        music,
        result_sfx,
    }
}

async fn execute_rest_command(
    api_base: &str,
    session_token: &str,
    command: RestCommand,
) -> Result<RestPayload, BrowserOperationError> {
    let room = match command {
        RestCommand::Create {
            maximum_players,
            visibility,
        } => {
            post_json(
                api_base,
                "/v2/rooms",
                Some(session_token),
                &CreateRoomRequest {
                    maximum_players,
                    visibility,
                },
            )
            .await?
        }
        RestCommand::Join { room_code } => {
            post_json(
                api_base,
                "/v2/rooms/join",
                Some(session_token),
                &JoinRoomRequest { room_code },
            )
            .await?
        }
        RestCommand::Settings {
            room_code,
            expected_revision,
            arena_index,
            rule_index,
        } => {
            patch_json(
                api_base,
                &format!("/v2/rooms/{room_code}/settings"),
                Some(session_token),
                &UpdateRoomSettingsRequest {
                    expected_revision,
                    arena_index,
                    rule_index,
                },
            )
            .await?
        }
        RestCommand::Character {
            room_code,
            expected_revision,
            character,
        } => {
            patch_json(
                api_base,
                &format!("/v2/rooms/{room_code}/members/self/character"),
                Some(session_token),
                &SelectCharacterRequest {
                    expected_revision,
                    character,
                },
            )
            .await?
        }
        RestCommand::Ready {
            room_code,
            expected_revision,
            ready,
        } => {
            patch_json(
                api_base,
                &format!("/v2/rooms/{room_code}/members/self/ready"),
                Some(session_token),
                &SetReadyRequest {
                    expected_revision,
                    ready,
                },
            )
            .await?
        }
        RestCommand::Kick {
            room_code,
            expected_revision,
            peer_id,
        } => {
            post_json(
                api_base,
                &format!("/v2/rooms/{room_code}/members/kick"),
                Some(session_token),
                &KickMemberRequest {
                    expected_revision,
                    peer_id,
                },
            )
            .await?
        }
        RestCommand::Start {
            room_code,
            expected_revision,
        } => {
            post_json(
                api_base,
                &format!("/v2/rooms/{room_code}/start"),
                Some(session_token),
                &StartRoomRequest { expected_revision },
            )
            .await?
        }
        RestCommand::ResultAck {
            room_code,
            match_id,
        } => {
            post_json(
                api_base,
                &format!("/v2/rooms/{room_code}/results/ack"),
                Some(session_token),
                &ResultAckRequest { match_id },
            )
            .await?
        }
        RestCommand::Leave { room_code } => {
            post_no_content(
                api_base,
                &format!("/v2/rooms/{room_code}/leave"),
                Some(session_token),
            )
            .await?;
            return Ok(RestPayload::Left);
        }
    };
    Ok(RestPayload::Room(room))
}

async fn issue_ticket_and_connect(
    api_base: &str,
    service: &ServiceConfigResponse,
    session_token: &str,
    room_code: &str,
    expected_manifest: MatchManifest,
    reconnect: bool,
    last_confirmed_tick: Option<SimTick>,
) -> Result<ConnectedPayload, BrowserOperationError> {
    let request = TicketRequest {
        mode: if reconnect {
            TicketModeRequest::Reconnect
        } else {
            TicketModeRequest::Initial
        },
        last_confirmed_tick: last_confirmed_tick.map(SimTick::get),
    };
    let response: TicketResponse = post_json(
        api_base,
        &format!("/v2/rooms/{room_code}/tickets"),
        Some(session_token),
        &request,
    )
    .await?;
    if response.manifest != expected_manifest {
        return Err(BrowserOperationError::local("manifest_identity_changed"));
    }
    let countdown_start_tick = response.countdown_start_tick.map(SimTick);
    if reconnect != countdown_start_tick.is_some() {
        return Err(BrowserOperationError::local(
            "reconnect_boundary_unavailable",
        ));
    }
    let match_config = headless_config_from_manifest(response.manifest)
        .map_err(|_| BrowserOperationError::local("incompatible_release"))?;
    let peer_id = PeerId::new(response.peer_id)
        .map_err(|_| BrowserOperationError::local("invalid_peer_identity"))?;
    let preference = if service.webtransport_url.is_some() {
        BrowserTransportPreference::WebTransportPreferred
    } else {
        BrowserTransportPreference::WebSocketOnly
    };
    let endpoint = connect_browser_admitted_datagram_endpoint(
        preference,
        service.webtransport_url.as_deref().unwrap_or(""),
        &service.gameplay_websocket_url,
        &response.ticket,
        WebEndpointConfig::default(),
    )
    .await
    .map_err(|error| BrowserOperationError::local(format!("transport_{error:?}")))?;
    Ok(ConnectedPayload {
        endpoint,
        match_config,
        peer_id,
        countdown_start_tick,
    })
}

fn spawn_bootstrap(
    generation: u64,
    api_base: String,
    stored: Option<StoredBrowserSession>,
    mailbox: AsyncMailbox,
) {
    spawn_local(async move {
        let result = async {
            let service: ServiceConfigResponse = get_json(&api_base, "/v2/config", None).await?;
            validate_service_config(&service)?;
            let mut discard_stored_session = false;
            let restored = if let Some(stored) = stored {
                match get_json::<LobbySnapshotResponse>(
                    &api_base,
                    "/v2/lobby",
                    Some(&stored.guest.session_token),
                )
                .await
                {
                    Ok(snapshot) => Some((stored.guest, snapshot)),
                    Err(error) if error.invalidates_guest() => {
                        discard_stored_session = true;
                        None
                    }
                    Err(error) => return Err(error),
                }
            } else {
                None
            };
            Ok(BootstrapPayload {
                service,
                restored,
                discard_stored_session,
            })
        }
        .await;
        push_async_event(
            &mailbox,
            BrowserAsyncEvent {
                generation,
                payload: BrowserAsyncPayload::Bootstrap(result),
            },
        );
    });
}

fn validate_service_config(service: &ServiceConfigResponse) -> Result<(), BrowserOperationError> {
    if service.api_version != WEB_API_VERSION
        || service.lobby_websocket_subprotocol != AFC_LOBBY_WEBSOCKET_SUBPROTOCOL
        || service.gameplay_websocket_subprotocol != AFC_WEBSOCKET_SUBPROTOCOL
        || service.release != current_release_identity().version_line()
    {
        return Err(BrowserOperationError::local("incompatible_release"));
    }
    let page_https = web_sys::window()
        .and_then(|window| window.location().protocol().ok())
        .is_some_and(|protocol| protocol == "https:");
    let websocket_protocols = if page_https {
        &["wss:"][..]
    } else {
        &["ws:", "wss:"][..]
    };
    let lobby_valid = valid_transport_url(&service.lobby_websocket_url, websocket_protocols);
    let gameplay_valid = valid_transport_url(&service.gameplay_websocket_url, websocket_protocols);
    let webtransport_valid = service
        .webtransport_url
        .as_deref()
        .is_none_or(|url| valid_transport_url(url, &["https:"]));
    if !lobby_valid || !gameplay_valid || !webtransport_valid {
        return Err(BrowserOperationError::local("invalid_transport_url"));
    }
    Ok(())
}

fn valid_transport_url(value: &str, allowed_protocols: &[&str]) -> bool {
    Url::new(value).is_ok_and(|url| {
        allowed_protocols.contains(&url.protocol().as_str())
            && !url.host().is_empty()
            && url.username().is_empty()
            && url.password().is_empty()
            && url.hash().is_empty()
    })
}

fn resolve_api_base() -> Result<String, BrowserOperationError> {
    let window = web_sys::window().ok_or_else(|| BrowserOperationError::local("window_missing"))?;
    let location = window.location();
    let query_override = location
        .search()
        .ok()
        .and_then(|search| UrlSearchParams::new_with_str(&search).ok())
        .and_then(|parameters| parameters.get("afc_server"));
    let global_override = Reflect::get(window.as_ref(), &JsValue::from_str("AFC_WEB_API_URL"))
        .ok()
        .and_then(|value| value.as_string())
        .filter(|value| !value.trim().is_empty());
    let meta_override = window
        .document()
        .and_then(|document| {
            document
                .query_selector("meta[name='afc-web-api-url']")
                .ok()
                .flatten()
        })
        .and_then(|element| element.get_attribute("content"))
        .filter(|value| !value.trim().is_empty());
    let candidate = query_override
        .or(global_override)
        .or(meta_override)
        .or_else(|| location.origin().ok())
        .ok_or_else(|| BrowserOperationError::local("api_base_missing"))?;
    let parsed =
        Url::new(candidate.trim()).map_err(|_| BrowserOperationError::local("api_base_invalid"))?;
    if !parsed.username().is_empty()
        || !parsed.password().is_empty()
        || !parsed.search().is_empty()
        || !parsed.hash().is_empty()
        || parsed.pathname() != "/"
    {
        return Err(BrowserOperationError::local("api_base_invalid"));
    }
    let page_https = location
        .protocol()
        .ok()
        .is_some_and(|value| value == "https:");
    if (page_https && parsed.protocol() != "https:")
        || (!page_https && !matches!(parsed.protocol().as_str(), "http:" | "https:"))
    {
        return Err(BrowserOperationError::local("api_base_insecure"));
    }
    Ok(parsed.origin())
}

fn load_stored_session(api_base: &str) -> Option<StoredBrowserSession> {
    let storage = web_sys::window()?.session_storage().ok().flatten()?;
    let encoded = storage.get_item(SESSION_STORAGE_KEY).ok().flatten()?;
    let session: StoredBrowserSession = serde_json::from_str(&encoded).ok()?;
    (session.api_base == api_base).then_some(session)
}

fn save_stored_session(session: &StoredBrowserSession) {
    let Some(storage) =
        web_sys::window().and_then(|window| window.session_storage().ok().flatten())
    else {
        return;
    };
    if let Ok(encoded) = serde_json::to_string(session) {
        let _ = storage.set_item(SESSION_STORAGE_KEY, &encoded);
    }
}

fn clear_stored_session() {
    if let Some(storage) =
        web_sys::window().and_then(|window| window.session_storage().ok().flatten())
    {
        let _ = storage.remove_item(SESSION_STORAGE_KEY);
    }
}

fn push_async_event(mailbox: &AsyncMailbox, event: BrowserAsyncEvent) {
    let mut mailbox = mailbox.borrow_mut();
    if mailbox
        .iter()
        .any(|queued| queued.generation > event.generation)
    {
        return;
    }
    mailbox.retain(|queued| queued.generation >= event.generation);
    if mailbox.len() == MAX_ASYNC_EVENTS {
        mailbox.pop_front();
    }
    mailbox.push_back(event);
}

fn push_unique_chat(
    messages: &mut Vec<crate::web_api::ChatMessageResponse>,
    message: crate::web_api::ChatMessageResponse,
) {
    if messages
        .iter()
        .any(|existing| existing.message_id == message.message_id)
    {
        return;
    }
    messages.push(message);
    if messages.len() > 100 {
        messages.remove(0);
    }
}

fn call_dom_bridge(method: &str, json: &str) -> Result<JsValue, BrowserOperationError> {
    let window = web_sys::window().ok_or_else(|| BrowserOperationError::local("window_missing"))?;
    let bridge = Reflect::get(window.as_ref(), &JsValue::from_str(DOM_BRIDGE_KEY))
        .map_err(|_| BrowserOperationError::local("dom_bridge_missing"))?;
    let function = Reflect::get(&bridge, &JsValue::from_str(method))
        .map_err(|_| BrowserOperationError::local("dom_bridge_missing"))?
        .dyn_into::<Function>()
        .map_err(|_| BrowserOperationError::local("dom_bridge_missing"))?;
    function
        .call1(&bridge, &JsValue::from_str(json))
        .map_err(|_| BrowserOperationError::local("dom_bridge_failed"))
}

fn drain_dom_actions() -> Vec<BrowserDomAction> {
    let Ok(value) = call_dom_bridge("drainActions", "") else {
        return Vec::new();
    };
    let Ok(encoded) = JSON::stringify(&value) else {
        return Vec::new();
    };
    let Some(encoded) = encoded.as_string() else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<BrowserDomAction>>(&encoded).unwrap_or_default()
}

async fn get_json<ResponseBody: DeserializeOwned>(
    base: &str,
    path: &str,
    bearer: Option<&str>,
) -> Result<ResponseBody, BrowserOperationError> {
    request_json("GET", base, path, bearer, None).await
}

async fn post_json<RequestBody: Serialize, ResponseBody: DeserializeOwned>(
    base: &str,
    path: &str,
    bearer: Option<&str>,
    body: &RequestBody,
) -> Result<ResponseBody, BrowserOperationError> {
    request_body_json("POST", base, path, bearer, body).await
}

async fn patch_json<RequestBody: Serialize, ResponseBody: DeserializeOwned>(
    base: &str,
    path: &str,
    bearer: Option<&str>,
    body: &RequestBody,
) -> Result<ResponseBody, BrowserOperationError> {
    request_body_json("PATCH", base, path, bearer, body).await
}

async fn request_body_json<RequestBody: Serialize, ResponseBody: DeserializeOwned>(
    method: &str,
    base: &str,
    path: &str,
    bearer: Option<&str>,
    body: &RequestBody,
) -> Result<ResponseBody, BrowserOperationError> {
    let body = serde_json::to_string(body)
        .map_err(|_| BrowserOperationError::local("request_encoding"))?;
    request_json(method, base, path, bearer, Some(&body)).await
}

async fn post_no_content(
    base: &str,
    path: &str,
    bearer: Option<&str>,
) -> Result<(), BrowserOperationError> {
    let (status, body) = request_text("POST", base, path, bearer, None).await?;
    if !(200..300).contains(&status) {
        return Err(http_status_error(status, &body));
    }
    Ok(())
}

async fn request_json<ResponseBody: DeserializeOwned>(
    method: &str,
    base: &str,
    path: &str,
    bearer: Option<&str>,
    body: Option<&str>,
) -> Result<ResponseBody, BrowserOperationError> {
    let (status, body) = request_text(method, base, path, bearer, body).await?;
    if !(200..300).contains(&status) {
        return Err(http_status_error(status, &body));
    }
    serde_json::from_str(&body).map_err(|_| BrowserOperationError::local("response_encoding"))
}

async fn request_text(
    method: &str,
    base: &str,
    path: &str,
    bearer: Option<&str>,
    body: Option<&str>,
) -> Result<(u16, String), BrowserOperationError> {
    let window = web_sys::window().ok_or_else(|| BrowserOperationError::local("window_missing"))?;
    let controller = AbortController::new()
        .map_err(|_| BrowserOperationError::local("request_initialization"))?;
    let init = RequestInit::new();
    init.set_method(method);
    init.set_mode(RequestMode::Cors);
    init.set_signal(Some(&controller.signal()));
    let body_value = body.map(JsValue::from_str);
    if let Some(body) = body_value.as_ref() {
        init.set_body(body);
    }
    let request = Request::new_with_str_and_init(&format!("{base}{path}"), &init)
        .map_err(|_| BrowserOperationError::local("request_initialization"))?;
    let headers: Headers = request.headers();
    headers
        .set("Accept", "application/json")
        .map_err(|_| BrowserOperationError::local("request_initialization"))?;
    if body.is_some() {
        headers
            .set("Content-Type", "application/json")
            .map_err(|_| BrowserOperationError::local("request_initialization"))?;
    }
    if let Some(token) = bearer {
        headers
            .set("Authorization", &format!("Bearer {token}"))
            .map_err(|_| BrowserOperationError::local("request_initialization"))?;
    }
    let timeout_controller = controller.clone();
    let callback = Closure::wrap(Box::new(move || timeout_controller.abort()) as Box<dyn FnMut()>);
    let timeout_id = window
        .set_timeout_with_callback_and_timeout_and_arguments_0(
            callback.as_ref().unchecked_ref(),
            HTTP_TIMEOUT_MS,
        )
        .map_err(|_| BrowserOperationError::local("request_initialization"))?;
    let result = async {
        let response = JsFuture::from(window.fetch_with_request(&request))
            .await
            .map_err(|_| BrowserOperationError::local("request_timeout"))?
            .dyn_into::<Response>()
            .map_err(|_| BrowserOperationError::local("response_encoding"))?;
        let status = response.status();
        let text = read_response_text_bounded(&response).await?;
        Ok((status, text))
    }
    .await;
    window.clear_timeout_with_handle(timeout_id);
    drop(callback);
    result
}

async fn read_response_text_bounded(response: &Response) -> Result<String, BrowserOperationError> {
    if response
        .headers()
        .get("Content-Length")
        .ok()
        .flatten()
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > MAX_HTTP_RESPONSE_BYTES)
    {
        return Err(BrowserOperationError::local("response_too_large"));
    }
    let Some(body) = response.body() else {
        return Ok(String::new());
    };
    let reader = ReadableStreamDefaultReader::new(&body)
        .map_err(|_| BrowserOperationError::local("response_encoding"))?;
    let result: Result<Vec<u8>, BrowserOperationError> = async {
        let mut bytes = Vec::with_capacity(1_024);
        loop {
            let value = JsFuture::from(reader.read())
                .await
                .map_err(|_| BrowserOperationError::local("response_encoding"))?;
            let done = Reflect::get(&value, &JsValue::from_str("done"))
                .ok()
                .and_then(|done| done.as_bool())
                .unwrap_or(true);
            if done {
                break;
            }
            let chunk = Reflect::get(&value, &JsValue::from_str("value"))
                .map_err(|_| BrowserOperationError::local("response_encoding"))?;
            let chunk = js_sys::Uint8Array::new(&chunk);
            let chunk_length = chunk.length() as usize;
            if chunk_length == 0 {
                continue;
            }
            let next_length = bytes
                .len()
                .checked_add(chunk_length)
                .ok_or_else(|| BrowserOperationError::local("response_too_large"))?;
            if next_length > MAX_HTTP_RESPONSE_BYTES {
                return Err(BrowserOperationError::local("response_too_large"));
            }
            let old_length = bytes.len();
            bytes.resize(next_length, 0);
            chunk.copy_to(&mut bytes[old_length..]);
        }
        Ok(bytes)
    }
    .await;
    if result.is_err() {
        let _ = JsFuture::from(reader.cancel()).await;
    }
    reader.release_lock();
    String::from_utf8(result?).map_err(|_| BrowserOperationError::local("response_encoding"))
}

fn http_status_error(status: u16, body: &str) -> BrowserOperationError {
    let code = serde_json::from_str::<ApiErrorEnvelope>(body)
        .map(|error| error.error.code)
        .unwrap_or_else(|_| format!("http_{status}"));
    BrowserOperationError {
        status: Some(status),
        code,
    }
}

#[derive(Component)]
pub struct BrowserOnlineUiRoot;

#[derive(Component)]
pub(crate) struct BrowserOnlineUiPanel;

#[derive(Component)]
pub(crate) struct BrowserOnlineUiTitle;

#[derive(Component)]
pub(crate) struct BrowserOnlineUiDetails;

#[derive(Component)]
pub(crate) struct BrowserOnlineUiFooter;

pub(crate) fn setup_browser_online_ui(
    mut commands: Commands,
    ui_cameras: Query<Entity, With<UiCamera>>,
) {
    let mut root = commands.spawn((
        BrowserOnlineUiRoot,
        Node {
            display: Display::None,
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: Val::Px(0.0),
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            justify_content: JustifyContent::FlexEnd,
            align_items: AlignItems::FlexStart,
            padding: UiRect::all(Val::Px(18.0)),
            ..default()
        },
        GlobalZIndex(900),
        Pickable::IGNORE,
    ));
    root.with_children(|root| {
        root.spawn((
            BrowserOnlineUiPanel,
            Node {
                width: Val::Percent(58.0),
                max_width: Val::Px(720.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(4.0),
                padding: UiRect::all(Val::Px(10.0)),
                border: UiRect::all(Val::Px(2.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.025, 0.028, 0.04, 0.78)),
            BorderColor::all(Color::srgb(0.32, 0.38, 0.46)),
        ))
        .with_children(|panel| {
            panel.spawn((
                BrowserOnlineUiTitle,
                Text::new("ONLINE"),
                TextFont {
                    font_size: 24.0,
                    ..default()
                },
                TextColor(Color::srgb(0.93, 0.79, 0.52)),
                Pickable::IGNORE,
            ));
            panel.spawn((
                BrowserOnlineUiDetails,
                Text::new(""),
                TextFont {
                    font_size: 15.0,
                    ..default()
                },
                TextColor(Color::srgb(0.82, 0.84, 0.87)),
                TextLayout::new_with_justify(Justify::Center),
                Pickable::IGNORE,
            ));
            panel.spawn((
                BrowserOnlineUiFooter,
                Text::new(""),
                TextFont {
                    font_size: 12.0,
                    ..default()
                },
                TextColor(Color::srgb(0.62, 0.66, 0.72)),
                Pickable::IGNORE,
            ));
        });
    });
    if let Some(camera) = ui_cameras.iter().next() {
        root.insert(UiTargetCamera(camera));
    }
}

pub(crate) fn handle_browser_online_ui_input(
    time: Res<Time<Real>>,
    keys: Res<ButtonInput<KeyCode>>,
    user_mode: Res<UserModeState>,
    mut application: NonSendMut<BrowserOnlineApplication>,
) {
    if !user_mode.online_active() {
        return;
    }
    let now_ms = time.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    for action in drain_dom_actions()
        .into_iter()
        .take(MAX_DOM_ACTIONS_PER_FRAME)
    {
        application.dispatch_dom(action, now_ms);
    }
    if keys.just_pressed(KeyCode::Escape) && !dom_text_entry_focused() {
        application.dispatch_dom(BrowserDomAction::LeaveOnline, now_ms);
    }
}

fn dom_text_entry_focused() -> bool {
    web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.active_element())
        .is_some_and(|element| {
            matches!(element.tag_name().as_str(), "INPUT" | "TEXTAREA" | "SELECT")
        })
}

pub(crate) fn update_browser_online_ui(
    snapshot: Res<BrowserOnlineUiSnapshot>,
    mut roots: Query<&mut Node, With<BrowserOnlineUiRoot>>,
    mut titles: Query<
        &mut Text,
        (
            With<BrowserOnlineUiTitle>,
            Without<BrowserOnlineUiDetails>,
            Without<BrowserOnlineUiFooter>,
        ),
    >,
    mut details: Query<
        &mut Text,
        (
            With<BrowserOnlineUiDetails>,
            Without<BrowserOnlineUiTitle>,
            Without<BrowserOnlineUiFooter>,
        ),
    >,
    mut footers: Query<
        &mut Text,
        (
            With<BrowserOnlineUiFooter>,
            Without<BrowserOnlineUiTitle>,
            Without<BrowserOnlineUiDetails>,
        ),
    >,
) {
    for mut node in &mut roots {
        node.display = if snapshot.visible && snapshot.compact {
            Display::Flex
        } else {
            Display::None
        };
    }
    for mut title in &mut titles {
        **title = snapshot.title.clone();
    }
    for mut detail in &mut details {
        **detail = snapshot.details.clone();
    }
    for mut footer in &mut footers {
        **footer = snapshot.footer.clone();
    }
}

pub(crate) fn update_browser_online_button_styles() {}
