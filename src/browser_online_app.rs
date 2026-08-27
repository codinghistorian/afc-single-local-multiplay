//! Main-thread browser lobby, admission, prediction, and presentation owner.
//!
//! Browsers cannot run the native worker-thread topology used by Steam. This
//! application keeps HTTP orchestration asynchronous while servicing the
//! existing predicted protocol on the Bevy/JavaScript main thread at the
//! canonical fixed tick rate.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt;
use std::rc::Rc;

use bevy::app::AppExit;
use bevy::prelude::*;
use js_sys::Reflect;
use serde::Serialize;
use serde::de::DeserializeOwned;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    AbortController, Headers, ReadableStreamDefaultReader, Request, RequestInit, RequestMode,
    Response, Url, UrlSearchParams,
};

use crate::arena_defs::{ActiveArena, arena_definitions};
use crate::browser_online_client::{BrowserOnlineClient, BrowserOnlineClientConfig};
use crate::camera::{GameplayCameraControl, UiCamera};
use crate::components::PlayerKeyBindings;
use crate::game_state::{MatchState, RULE_PRESETS};
use crate::headless::HeadlessMatchConfig;
use crate::match_config::headless_config_from_manifest;
use crate::match_presentation::{
    ConfirmedMatchPresentation, MatchPresentationPolicy, OnlinePanelMode, PresentationMusicTrack,
    PresentationPhase, PresentationResultSfx, PresentedLocalOutcome,
};
use crate::network_protocol::{MatchManifest, PeerId, RetryDisposition, SimTick};
use crate::presentation_projection::release_projection_target;
use crate::release_identity::current_release_identity;
use crate::remote_online_client::{
    RemoteOnlineClientPhase, RemoteOnlineClientStatus, RemoteOnlineTerminal,
};
use crate::tick_input::{LocalSeatId, LocalTickInputState};
use crate::user_mode::{UserModeGameplayScene, UserModeState};
use crate::web_api::{
    ApiErrorEnvelope, CreateRoomRequest, GuestSessionResponse, JoinRoomRequest, RoomResponse,
    RoomState, ServiceConfigResponse, TicketModeRequest, TicketRequest, TicketResponse,
    WEB_API_VERSION,
};
use crate::web_endpoint_adapters::{
    AFC_WEBSOCKET_SUBPROTOCOL, BrowserDatagramEndpoint, BrowserTransportPreference,
    WebEndpointConfig, connect_browser_admitted_datagram_endpoint,
};

const HTTP_TIMEOUT_MS: i32 = 10_000;
const MAX_HTTP_RESPONSE_BYTES: usize = 64 * 1_024;
const MAX_ASYNC_EVENTS: usize = 16;
const LOBBY_POLL_INTERVAL_MS: u64 = 750;
const RECONNECT_BASE_DELAY_MS: u64 = 350;
const MAX_RECONNECT_ATTEMPTS: u8 = 8;

type BrowserClient = BrowserOnlineClient<BrowserDatagramEndpoint>;
type AsyncMailbox = Rc<RefCell<VecDeque<BrowserAsyncEvent>>>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BrowserOnlineScreen {
    #[default]
    Dormant,
    Bootstrapping,
    Menu,
    Lobby,
    Connecting,
    Match,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingOperation {
    Bootstrap,
    CreateRoom,
    JoinRoom,
    PollRoom,
    StartRoom,
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
    pub room_code_symbols: String,
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
            room_code_symbols: String::new(),
            room: None,
            client_status: None,
            pending: false,
            scene_requested: false,
            client_active: false,
            retry_available: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Component)]
pub enum BrowserOnlineUiAction {
    CreateRoom,
    JoinRoom,
    FewerPlayers,
    MorePlayers,
    PreviousArena,
    NextArena,
    PreviousRules,
    NextRules,
    StartRoom,
    Leave,
    Back,
    Retry,
}

struct ConnectedPayload {
    endpoint: BrowserDatagramEndpoint,
    match_config: HeadlessMatchConfig,
    peer_id: PeerId,
}

enum BrowserAsyncPayload {
    Bootstrap(Result<(ServiceConfigResponse, GuestSessionResponse), BrowserOperationError>),
    Room {
        operation: PendingOperation,
        result: Result<RoomResponse, BrowserOperationError>,
    },
    Connected {
        reconnect: bool,
        result: Result<ConnectedPayload, BrowserOperationError>,
    },
}

struct BrowserAsyncEvent {
    generation: u64,
    payload: BrowserAsyncPayload,
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
            "room_not_found" => "That private room was not found.".to_owned(),
            "room_state_conflict" => "The room changed state. Please try again.".to_owned(),
            "room_access_denied" => "This guest cannot access that room.".to_owned(),
            "invalid_guest_session" => "The guest session expired. Re-enter Online.".to_owned(),
            "rate_limited" => "Too many requests. Please wait a moment.".to_owned(),
            "service_unavailable" | "transport_capacity" => {
                "The game server is temporarily unavailable.".to_owned()
            }
            "incompatible_release" => {
                "The browser build and server release do not match.".to_owned()
            }
            "request_timeout" => "The server did not respond in time.".to_owned(),
            _ => "Online operation failed. Please try again.".to_owned(),
        }
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

pub struct BrowserOnlineApplication {
    screen: BrowserOnlineScreen,
    api_base: Option<String>,
    service: Option<ServiceConfigResponse>,
    guest: Option<GuestSessionResponse>,
    room: Option<RoomResponse>,
    client: Option<BrowserClient>,
    mailbox: AsyncMailbox,
    generation: u64,
    pending: Option<PendingOperation>,
    room_code_symbols: String,
    maximum_players: u8,
    arena_index: usize,
    rule_index: usize,
    next_room_poll_ms: u64,
    reconnect_due_ms: Option<u64>,
    reconnect_attempts: u8,
    content_marked: bool,
    scene_requested: bool,
    projection_active: bool,
    request_exit: bool,
    notice: Option<String>,
    fatal_error: Option<String>,
    retry_plan: Option<RetryPlan>,
}

impl Default for BrowserOnlineApplication {
    fn default() -> Self {
        Self {
            screen: BrowserOnlineScreen::Dormant,
            api_base: None,
            service: None,
            guest: None,
            room: None,
            client: None,
            mailbox: Rc::new(RefCell::new(VecDeque::with_capacity(MAX_ASYNC_EVENTS))),
            generation: 1,
            pending: None,
            room_code_symbols: String::with_capacity(12),
            maximum_players: 2,
            arena_index: 0,
            rule_index: 0,
            next_room_poll_ms: 0,
            reconnect_due_ms: None,
            reconnect_attempts: 0,
            content_marked: false,
            scene_requested: false,
            projection_active: false,
            request_exit: false,
            notice: None,
            fatal_error: None,
            retry_plan: None,
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
        match resolve_api_base() {
            Ok(api_base) => {
                self.api_base = Some(api_base.clone());
                self.pending = Some(PendingOperation::Bootstrap);
                spawn_bootstrap(self.generation, api_base, Rc::clone(&self.mailbox));
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

    fn start_room_operation(&mut self, operation: PendingOperation) {
        let (Some(api_base), Some(guest)) = (self.api_base.clone(), self.guest.clone()) else {
            self.fail(
                "The guest session is unavailable.".to_owned(),
                RetryPlan::Bootstrap,
            );
            return;
        };
        self.pending = Some(operation);
        self.notice = None;
        let generation = self.generation;
        let mailbox = Rc::clone(&self.mailbox);
        let room = self.room.clone();
        let room_code = self.room_code_symbols.clone();
        let maximum_players = self.maximum_players;
        let arena_index = self.arena_index;
        let rule_index = self.rule_index;
        spawn_local(async move {
            let result = match operation {
                PendingOperation::CreateRoom => {
                    post_json::<_, RoomResponse>(
                        &api_base,
                        "/v1/rooms",
                        Some(&guest.session_token),
                        &CreateRoomRequest {
                            maximum_players,
                            arena_index,
                            rule_index,
                        },
                    )
                    .await
                }
                PendingOperation::JoinRoom => {
                    post_json::<_, RoomResponse>(
                        &api_base,
                        "/v1/rooms/join",
                        Some(&guest.session_token),
                        &JoinRoomRequest { room_code },
                    )
                    .await
                }
                PendingOperation::PollRoom => {
                    let code = room
                        .as_ref()
                        .map(|room| room.room_code.as_str())
                        .ok_or_else(|| BrowserOperationError::local("room_unavailable"));
                    match code {
                        Ok(code) => {
                            get_json::<RoomResponse>(
                                &api_base,
                                &format!("/v1/rooms/{code}"),
                                Some(&guest.session_token),
                            )
                            .await
                        }
                        Err(error) => Err(error),
                    }
                }
                PendingOperation::StartRoom => {
                    let code = room
                        .as_ref()
                        .map(|room| room.room_code.as_str())
                        .ok_or_else(|| BrowserOperationError::local("room_unavailable"));
                    match code {
                        Ok(code) => {
                            post_empty_json::<RoomResponse>(
                                &api_base,
                                &format!("/v1/rooms/{code}/start"),
                                Some(&guest.session_token),
                            )
                            .await
                        }
                        Err(error) => Err(error),
                    }
                }
                _ => Err(BrowserOperationError::local("invalid_room_operation")),
            };
            push_async_event(
                &mailbox,
                BrowserAsyncEvent {
                    generation,
                    payload: BrowserAsyncPayload::Room { operation, result },
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
                "Online session state is incomplete.".to_owned(),
                RetryPlan::Bootstrap,
            );
            return;
        };
        let Some(expected_manifest) = room.manifest else {
            self.fail(
                "The room did not publish a match manifest.".to_owned(),
                RetryPlan::InitialConnection,
            );
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
                        Ok((service, guest)) => {
                            self.service = Some(service);
                            self.guest = Some(guest);
                            self.screen = BrowserOnlineScreen::Menu;
                            self.retry_plan = None;
                        }
                        Err(error) => self.fail(error.player_message(), RetryPlan::Bootstrap),
                    }
                }
                BrowserAsyncPayload::Room { operation, result } => {
                    if self.pending != Some(operation) {
                        continue;
                    }
                    self.pending = None;
                    match result {
                        Ok(room) => {
                            self.room_code_symbols = room
                                .room_code
                                .bytes()
                                .filter(|byte| *byte != b'-')
                                .map(char::from)
                                .collect();
                            self.room = Some(room);
                            self.screen = BrowserOnlineScreen::Lobby;
                            self.notice = None;
                            self.next_room_poll_ms = now_ms.saturating_add(LOBBY_POLL_INTERVAL_MS);
                        }
                        Err(error) => match operation {
                            PendingOperation::CreateRoom | PendingOperation::JoinRoom => {
                                self.screen = BrowserOnlineScreen::Menu;
                                self.notice = Some(error.player_message());
                            }
                            PendingOperation::PollRoom | PendingOperation::StartRoom => {
                                self.screen = BrowserOnlineScreen::Lobby;
                                self.notice = Some(error.player_message());
                                self.next_room_poll_ms =
                                    now_ms.saturating_add(LOBBY_POLL_INTERVAL_MS);
                            }
                            _ => {}
                        },
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
                        Ok(payload) if reconnect => {
                            let Some(client) = &mut self.client else {
                                self.fail(
                                    "The reconnect target no longer exists.".to_owned(),
                                    RetryPlan::InitialConnection,
                                );
                                continue;
                            };
                            if client.peer_id() != payload.peer_id
                                || client.manifest() != &payload.match_config.manifest
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
                                    self.reconnect_attempts = 0;
                                    self.reconnect_due_ms = None;
                                    self.retry_plan = None;
                                    self.content_marked = true;
                                }
                                Err(_) => self.fail(
                                    "The predicted client could not reconnect.".to_owned(),
                                    RetryPlan::Reconnect,
                                ),
                            }
                        }
                        Ok(payload) => {
                            let setup = payload.match_config.local_setup.clone();
                            let arena_index = setup.arena_index;
                            match BrowserOnlineClient::new(
                                payload.endpoint,
                                payload.match_config,
                                payload.peer_id,
                                BrowserOnlineClientConfig::default(),
                            ) {
                                Ok(client) => {
                                    world.insert_resource(setup);
                                    world.resource_mut::<ActiveArena>().select(arena_index);
                                    self.client = Some(client);
                                    self.screen = BrowserOnlineScreen::Match;
                                    self.scene_requested = true;
                                    self.content_marked = false;
                                    self.projection_active = true;
                                    self.retry_plan = None;
                                    self.reconnect_attempts = 0;
                                    self.reconnect_due_ms = None;
                                }
                                Err(_) => self.fail(
                                    "The predicted browser client could not start.".to_owned(),
                                    RetryPlan::InitialConnection,
                                ),
                            }
                        }
                        Err(error) if reconnect && error.reconnect_may_retry() => {
                            self.schedule_reconnect_retry(now_ms, error.player_message());
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
            }
        }
    }

    fn schedule_reconnect_retry(&mut self, now_ms: u64, notice: String) {
        if self.reconnect_attempts >= MAX_RECONNECT_ATTEMPTS {
            self.fail(
                "The authority could not be reached after several attempts.".to_owned(),
                RetryPlan::Reconnect,
            );
            return;
        }
        let shift = u32::from(self.reconnect_attempts.min(4));
        let delay = RECONNECT_BASE_DELAY_MS.saturating_mul(1_u64 << shift);
        self.reconnect_attempts = self.reconnect_attempts.saturating_add(1);
        self.reconnect_due_ms = Some(now_ms.saturating_add(delay));
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
        if scene_ready && let Err(_error) = client.project_latest(world) {
            self.fail(
                "The authoritative match could not be projected.".to_owned(),
                RetryPlan::InitialConnection,
            );
            return;
        }
        match report.terminal {
            Some(RemoteOnlineTerminal::AuthorityDisconnected(disconnect)) => {
                if disconnect.message.retry == RetryDisposition::ReconnectAllowed {
                    if self.pending.is_none() && self.reconnect_due_ms.is_none() {
                        self.reconnect_due_ms =
                            Some(now_ms.saturating_add(RECONNECT_BASE_DELAY_MS));
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
            Some(RemoteOnlineTerminal::Stopped) => self.fail(
                "The online match stopped.".to_owned(),
                RetryPlan::InitialConnection,
            ),
            Some(RemoteOnlineTerminal::Completed(_)) | None => {}
        }
    }

    fn maybe_schedule_work(&mut self, now_ms: u64) {
        if self.pending.is_some() {
            return;
        }
        if let Some(due) = self.reconnect_due_ms
            && now_ms >= due
            && let Some(client) = &self.client
        {
            let last_confirmed = client.status().confirmed_tick.unwrap_or(SimTick::ZERO);
            self.reconnect_due_ms = None;
            self.start_connection(true, Some(last_confirmed));
            return;
        }
        let Some(room) = &self.room else {
            return;
        };
        match room.state {
            RoomState::Active if self.client.is_none() => self.start_connection(false, None),
            RoomState::Open | RoomState::Starting if now_ms >= self.next_room_poll_ms => {
                self.start_room_operation(PendingOperation::PollRoom);
            }
            RoomState::Finished | RoomState::Failed if self.client.is_none() => self.fail(
                "The room ended before this browser connected.".to_owned(),
                RetryPlan::InitialConnection,
            ),
            _ => {}
        }
    }

    fn dispatch(&mut self, action: BrowserOnlineUiAction, now_ms: u64) {
        match action {
            BrowserOnlineUiAction::CreateRoom
                if self.screen == BrowserOnlineScreen::Menu && self.pending.is_none() =>
            {
                self.start_room_operation(PendingOperation::CreateRoom);
            }
            BrowserOnlineUiAction::JoinRoom
                if self.screen == BrowserOnlineScreen::Menu
                    && self.pending.is_none()
                    && self.room_code_symbols.len() == 12 =>
            {
                self.start_room_operation(PendingOperation::JoinRoom);
            }
            BrowserOnlineUiAction::FewerPlayers if self.screen == BrowserOnlineScreen::Menu => {
                self.maximum_players = self.maximum_players.saturating_sub(1).max(2);
            }
            BrowserOnlineUiAction::MorePlayers if self.screen == BrowserOnlineScreen::Menu => {
                self.maximum_players = self.maximum_players.saturating_add(1).min(4);
            }
            BrowserOnlineUiAction::PreviousArena if self.screen == BrowserOnlineScreen::Menu => {
                self.arena_index = if self.arena_index == 0 {
                    arena_definitions().len() - 1
                } else {
                    self.arena_index - 1
                };
            }
            BrowserOnlineUiAction::NextArena if self.screen == BrowserOnlineScreen::Menu => {
                self.arena_index = (self.arena_index + 1) % arena_definitions().len();
            }
            BrowserOnlineUiAction::PreviousRules if self.screen == BrowserOnlineScreen::Menu => {
                self.rule_index = if self.rule_index == 0 {
                    RULE_PRESETS.len() - 1
                } else {
                    self.rule_index - 1
                };
            }
            BrowserOnlineUiAction::NextRules if self.screen == BrowserOnlineScreen::Menu => {
                self.rule_index = (self.rule_index + 1) % RULE_PRESETS.len();
            }
            BrowserOnlineUiAction::StartRoom
                if self.screen == BrowserOnlineScreen::Lobby
                    && self.pending.is_none()
                    && self.room.as_ref().is_some_and(|room| {
                        room.state == RoomState::Open
                            && room.self_is_host()
                            && room.member_count >= 2
                    }) =>
            {
                self.start_room_operation(PendingOperation::StartRoom);
            }
            BrowserOnlineUiAction::Retry if self.screen == BrowserOnlineScreen::Error => {
                self.fatal_error = None;
                match self.retry_plan.take() {
                    Some(RetryPlan::Reconnect) if self.client.is_some() => {
                        self.screen = BrowserOnlineScreen::Match;
                        self.reconnect_attempts = 0;
                        self.reconnect_due_ms = Some(now_ms);
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
            BrowserOnlineUiAction::Leave | BrowserOnlineUiAction::Back => {
                self.request_exit = true;
            }
            _ => {}
        }
    }

    fn sample_inputs(&mut self, inputs: &mut LocalTickInputState) {
        let Some(client) = &mut self.client else {
            return;
        };
        if let Err(_error) = client.sample_local_inputs(inputs) {
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
                post_no_content(&api_base, &format!("/v1/rooms/{code}/leave"), Some(&token)).await;
        });
    }

    fn reset_for_exit(&mut self) {
        self.begin_best_effort_leave();
        if let Some(client) = &mut self.client {
            client.stop();
        }
        self.client = None;
        self.invalidate_operations();
        self.screen = BrowserOnlineScreen::Dormant;
        self.api_base = None;
        self.service = None;
        self.guest = None;
        self.room = None;
        self.room_code_symbols.clear();
        self.notice = None;
        self.fatal_error = None;
        self.retry_plan = None;
        self.reconnect_due_ms = None;
        self.reconnect_attempts = 0;
        self.content_marked = false;
        self.scene_requested = false;
        self.request_exit = false;
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
            room_code_symbols: self.room_code_symbols.clone(),
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
                    "Authority tick {}  •  confirmed {}",
                    status.network_tick.get(),
                    status.confirmed_tick.unwrap_or(SimTick::ZERO).get()
                ),
                RemoteOnlineClientPhase::Countdown => {
                    "Waiting for the authoritative countdown.".to_owned()
                }
                RemoteOnlineClientPhase::Results => {
                    "The authoritative result is confirmed.".to_owned()
                }
                RemoteOnlineClientPhase::Reconnecting => {
                    "Restoring from an authority-retained snapshot…".to_owned()
                }
                _ => "Negotiating the authoritative match…".to_owned(),
            };
            return (phase.to_owned(), detail);
        }
        match self.screen {
            BrowserOnlineScreen::Dormant => ("ONLINE".to_owned(), String::new()),
            BrowserOnlineScreen::Bootstrapping => (
                "ONLINE".to_owned(),
                "Creating a secure guest session…".to_owned(),
            ),
            BrowserOnlineScreen::Menu => (
                "PRIVATE ONLINE".to_owned(),
                format!(
                    "Create: {} players  •  {}  •  {}\nJoin code: {}",
                    self.maximum_players,
                    arena_definitions()[self.arena_index].name,
                    RULE_PRESETS[self.rule_index].label,
                    formatted_room_code(&self.room_code_symbols)
                ),
            ),
            BrowserOnlineScreen::Lobby => {
                let Some(room) = &self.room else {
                    return ("PRIVATE ROOM".to_owned(), "Loading room…".to_owned());
                };
                let connected = room
                    .members
                    .iter()
                    .filter(|member| member.connected)
                    .count();
                (
                    format!("ROOM {}", room.room_code),
                    format!(
                        "Players: {}/{}  •  connected: {}\n{}",
                        room.member_count,
                        room.maximum_players,
                        connected,
                        if room.self_is_host() {
                            "Share the code, then start when everyone has joined."
                        } else {
                            "Waiting for the host to start."
                        }
                    ),
                )
            }
            BrowserOnlineScreen::Connecting => (
                "CONNECTING".to_owned(),
                "Redeeming a short-lived, one-time join ticket…".to_owned(),
            ),
            BrowserOnlineScreen::Match => (
                "ONLINE MATCH".to_owned(),
                "Waiting for the predicted client…".to_owned(),
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
                "generation {}  •  predicted {}  •  Esc leaves online",
                status.generation,
                status.predicted_tick.unwrap_or(SimTick::ZERO).get()
            );
        }
        self.guest.as_ref().map_or_else(
            || "Guest identity is held in memory only.".to_owned(),
            |guest| format!("Guest {}  •  Esc returns to menu", guest.guest_id),
        )
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
        application.reset_for_exit();
        if application.projection_active {
            release_browser_projection_target(world);
            application.projection_active = false;
        }
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

    let snapshot = application.snapshot(online_visible);
    world.insert_non_send_resource(application);
    world.insert_resource(snapshot);
}

fn release_browser_projection_target(world: &mut World) {
    release_projection_target(world);
    if let Some(mut inputs) = world.get_resource_mut::<LocalTickInputState>() {
        inputs.reset_all_sessions();
    }
    if let Some(mut match_state) = world.get_resource_mut::<MatchState>() {
        match_state.return_to_setup();
    }
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
            phase: if snapshot.screen == BrowserOnlineScreen::Lobby {
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
        &format!("/v1/rooms/{room_code}/tickets"),
        Some(session_token),
        &request,
    )
    .await?;
    if response.manifest != expected_manifest {
        return Err(BrowserOperationError::local("manifest_identity_changed"));
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
        &service.websocket_url,
        &response.ticket,
        WebEndpointConfig::default(),
    )
    .await
    .map_err(|error| BrowserOperationError::local(format!("transport_{error:?}")))?;
    Ok(ConnectedPayload {
        endpoint,
        match_config,
        peer_id,
    })
}

fn spawn_bootstrap(generation: u64, api_base: String, mailbox: AsyncMailbox) {
    spawn_local(async move {
        let result = async {
            let service: ServiceConfigResponse = get_json(&api_base, "/v1/config", None).await?;
            validate_service_config(&service)?;
            let guest: GuestSessionResponse =
                post_no_body_json(&api_base, "/v1/guests", None).await?;
            Ok((service, guest))
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
        || service.websocket_subprotocol != AFC_WEBSOCKET_SUBPROTOCOL
        || service.release != current_release_identity().version_line()
    {
        return Err(BrowserOperationError::local("incompatible_release"));
    }
    let page_https = web_sys::window()
        .and_then(|window| window.location().protocol().ok())
        .is_some_and(|protocol| protocol == "https:");
    let websocket_valid = valid_transport_url(
        &service.websocket_url,
        if page_https {
            &["wss:"][..]
        } else {
            &["ws:", "wss:"][..]
        },
    );
    let webtransport_valid = service
        .webtransport_url
        .as_deref()
        .is_none_or(|url| valid_transport_url(url, &["https:"]));
    if !websocket_valid || !webtransport_valid {
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

async fn get_json<ResponseBody: DeserializeOwned>(
    base: &str,
    path: &str,
    bearer: Option<&str>,
) -> Result<ResponseBody, BrowserOperationError> {
    request_json("GET", base, path, bearer, None).await
}

async fn post_no_body_json<ResponseBody: DeserializeOwned>(
    base: &str,
    path: &str,
    bearer: Option<&str>,
) -> Result<ResponseBody, BrowserOperationError> {
    request_json("POST", base, path, bearer, None).await
}

async fn post_empty_json<ResponseBody: DeserializeOwned>(
    base: &str,
    path: &str,
    bearer: Option<&str>,
) -> Result<ResponseBody, BrowserOperationError> {
    request_json("POST", base, path, bearer, Some("{}")).await
}

async fn post_json<RequestBody: Serialize, ResponseBody: DeserializeOwned>(
    base: &str,
    path: &str,
    bearer: Option<&str>,
    body: &RequestBody,
) -> Result<ResponseBody, BrowserOperationError> {
    let body = serde_json::to_string(body)
        .map_err(|_| BrowserOperationError::local("request_encoding"))?;
    request_json("POST", base, path, bearer, Some(&body)).await
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
    let bytes = result?;
    String::from_utf8(bytes).map_err(|_| BrowserOperationError::local("response_encoding"))
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

fn formatted_room_code(symbols: &str) -> String {
    if symbols.is_empty() {
        return "____-____-____".to_owned();
    }
    let mut output = String::with_capacity(14);
    for (index, symbol) in symbols.chars().enumerate() {
        if index != 0 && index % 4 == 0 {
            output.push('-');
        }
        output.push(symbol);
    }
    for index in symbols.len()..12 {
        if index != 0 && index % 4 == 0 {
            output.push('-');
        }
        output.push('_');
    }
    output
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

fn browser_online_button(label: &'static str, action: BrowserOnlineUiAction) -> impl Bundle {
    (
        Button,
        action,
        Node {
            display: Display::None,
            min_width: Val::Px(152.0),
            height: Val::Px(44.0),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            border: UiRect::all(Val::Px(2.0)),
            padding: UiRect::axes(Val::Px(12.0), Val::Px(5.0)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.055, 0.055, 0.07, 0.97)),
        BorderColor::all(Color::srgb(0.38, 0.42, 0.48)),
        children![(
            Text::new(label),
            TextFont {
                font_size: 17.0,
                ..default()
            },
            TextColor(Color::srgb(0.93, 0.88, 0.77)),
            TextLayout::new_with_justify(Justify::Center),
            Pickable::IGNORE,
        )],
    )
}

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
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            padding: UiRect::all(Val::Px(28.0)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.006, 0.008, 0.014, 0.96)),
        GlobalZIndex(900),
        Pickable::IGNORE,
    ));
    root.with_children(|root| {
        root.spawn((
            BrowserOnlineUiPanel,
            Node {
                width: Val::Percent(86.0),
                max_width: Val::Px(920.0),
                min_height: Val::Px(300.0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                row_gap: Val::Px(14.0),
                padding: UiRect::all(Val::Px(22.0)),
                border: UiRect::all(Val::Px(2.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.025, 0.028, 0.04, 0.94)),
            BorderColor::all(Color::srgb(0.32, 0.38, 0.46)),
        ))
        .with_children(|panel| {
            panel.spawn((
                BrowserOnlineUiTitle,
                Text::new("ONLINE"),
                TextFont {
                    font_size: 38.0,
                    ..default()
                },
                TextColor(Color::srgb(0.93, 0.79, 0.52)),
                TextLayout::new_with_justify(Justify::Center),
                Pickable::IGNORE,
            ));
            panel.spawn((
                BrowserOnlineUiDetails,
                Text::new(""),
                TextFont {
                    font_size: 19.0,
                    ..default()
                },
                TextColor(Color::srgb(0.82, 0.84, 0.87)),
                TextLayout::new_with_justify(Justify::Center),
                Node {
                    min_height: Val::Px(76.0),
                    ..default()
                },
                Pickable::IGNORE,
            ));
            panel
                .spawn((
                    Node {
                        width: Val::Percent(100.0),
                        flex_direction: FlexDirection::Row,
                        flex_wrap: FlexWrap::Wrap,
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        column_gap: Val::Px(8.0),
                        row_gap: Val::Px(8.0),
                        ..default()
                    },
                    Pickable::IGNORE,
                ))
                .with_children(|buttons| {
                    for (label, action) in [
                        ("CREATE PRIVATE", BrowserOnlineUiAction::CreateRoom),
                        ("JOIN CODE", BrowserOnlineUiAction::JoinRoom),
                        ("PLAYERS −", BrowserOnlineUiAction::FewerPlayers),
                        ("PLAYERS +", BrowserOnlineUiAction::MorePlayers),
                        ("ARENA −", BrowserOnlineUiAction::PreviousArena),
                        ("ARENA +", BrowserOnlineUiAction::NextArena),
                        ("RULES −", BrowserOnlineUiAction::PreviousRules),
                        ("RULES +", BrowserOnlineUiAction::NextRules),
                        ("START MATCH", BrowserOnlineUiAction::StartRoom),
                        ("LEAVE ONLINE", BrowserOnlineUiAction::Leave),
                        ("BACK", BrowserOnlineUiAction::Back),
                        ("RETRY", BrowserOnlineUiAction::Retry),
                    ] {
                        buttons.spawn(browser_online_button(label, action));
                    }
                });
            panel.spawn((
                BrowserOnlineUiFooter,
                Text::new(""),
                TextFont {
                    font_size: 15.0,
                    ..default()
                },
                TextColor(Color::srgb(0.62, 0.66, 0.72)),
                TextLayout::new_with_justify(Justify::Center),
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
    gamepads: Query<&Gamepad>,
    user_mode: Res<UserModeState>,
    snapshot: Res<BrowserOnlineUiSnapshot>,
    interactions: Query<(&Interaction, &BrowserOnlineUiAction), Changed<Interaction>>,
    mut application: NonSendMut<BrowserOnlineApplication>,
) {
    if !user_mode.online_active() {
        return;
    }
    if snapshot.screen == BrowserOnlineScreen::Menu && !snapshot.pending {
        if keys.just_pressed(KeyCode::Backspace) {
            application.room_code_symbols.pop();
            application.notice = None;
        } else if application.room_code_symbols.len() < 12
            && let Some(symbol) = pressed_room_code_symbol(&keys)
        {
            application.room_code_symbols.push(symbol);
            application.notice = None;
        }
    }
    let pointer_action = interactions.iter().find_map(|(interaction, action)| {
        (*interaction == Interaction::Pressed).then_some(*action)
    });
    let gamepad_accept = gamepads
        .iter()
        .any(|gamepad| gamepad.just_pressed(GamepadButton::South));
    let gamepad_back = gamepads
        .iter()
        .any(|gamepad| gamepad.just_pressed(GamepadButton::East));
    let action = pointer_action.or_else(|| {
        if keys.just_pressed(KeyCode::Escape) || gamepad_back {
            Some(
                if matches!(
                    snapshot.screen,
                    BrowserOnlineScreen::Lobby
                        | BrowserOnlineScreen::Connecting
                        | BrowserOnlineScreen::Match
                ) {
                    BrowserOnlineUiAction::Leave
                } else {
                    BrowserOnlineUiAction::Back
                },
            )
        } else if keys.just_pressed(KeyCode::Enter) || gamepad_accept {
            match snapshot.screen {
                BrowserOnlineScreen::Menu if snapshot.room_code_symbols.len() == 12 => {
                    Some(BrowserOnlineUiAction::JoinRoom)
                }
                BrowserOnlineScreen::Menu => Some(BrowserOnlineUiAction::CreateRoom),
                BrowserOnlineScreen::Lobby
                    if snapshot.room.as_ref().is_some_and(|room| {
                        room.state == RoomState::Open
                            && room.self_is_host()
                            && room.member_count >= 2
                    }) =>
                {
                    Some(BrowserOnlineUiAction::StartRoom)
                }
                BrowserOnlineScreen::Error if snapshot.retry_available => {
                    Some(BrowserOnlineUiAction::Retry)
                }
                _ => None,
            }
        } else {
            None
        }
    });
    if let Some(action) = action
        && browser_action_available(&snapshot, action)
    {
        let now_ms = time.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        application.dispatch(action, now_ms);
    }
}

pub(crate) fn update_browser_online_ui(
    snapshot: Res<BrowserOnlineUiSnapshot>,
    mut roots: Query<
        (&mut Node, &mut BackgroundColor),
        (With<BrowserOnlineUiRoot>, Without<BrowserOnlineUiPanel>),
    >,
    mut panels: Query<
        (&mut Node, &mut BackgroundColor),
        (With<BrowserOnlineUiPanel>, Without<BrowserOnlineUiRoot>),
    >,
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
    mut buttons: Query<
        (&BrowserOnlineUiAction, &mut Node),
        (
            With<Button>,
            Without<BrowserOnlineUiRoot>,
            Without<BrowserOnlineUiPanel>,
        ),
    >,
) {
    for (mut node, mut background) in &mut roots {
        node.display = if snapshot.visible {
            Display::Flex
        } else {
            Display::None
        };
        node.align_items = if snapshot.compact {
            AlignItems::FlexStart
        } else {
            AlignItems::Center
        };
        node.justify_content = if snapshot.compact {
            JustifyContent::FlexEnd
        } else {
            JustifyContent::Center
        };
        *background = BackgroundColor(Color::srgba(
            0.006,
            0.008,
            0.014,
            if snapshot.compact { 0.08 } else { 0.96 },
        ));
    }
    for (mut node, mut background) in &mut panels {
        node.width = if snapshot.compact {
            Val::Percent(58.0)
        } else {
            Val::Percent(86.0)
        };
        node.max_width = if snapshot.compact {
            Val::Px(720.0)
        } else {
            Val::Px(920.0)
        };
        node.min_height = if snapshot.compact {
            Val::Px(0.0)
        } else {
            Val::Px(300.0)
        };
        node.padding = UiRect::all(Val::Px(if snapshot.compact { 10.0 } else { 22.0 }));
        node.row_gap = Val::Px(if snapshot.compact { 5.0 } else { 14.0 });
        *background = BackgroundColor(Color::srgba(
            0.025,
            0.028,
            0.04,
            if snapshot.compact { 0.78 } else { 0.94 },
        ));
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
    for (action, mut node) in &mut buttons {
        node.display = if !snapshot.compact && browser_action_available(&snapshot, *action) {
            Display::Flex
        } else {
            Display::None
        };
    }
}

pub(crate) fn update_browser_online_button_styles(
    snapshot: Res<BrowserOnlineUiSnapshot>,
    mut buttons: Query<
        (
            &Interaction,
            &BrowserOnlineUiAction,
            &mut BackgroundColor,
            &mut BorderColor,
        ),
        With<Button>,
    >,
) {
    for (interaction, action, mut background, mut border) in &mut buttons {
        if !browser_action_available(&snapshot, *action) {
            continue;
        }
        let (fill, outline) = match interaction {
            Interaction::Pressed => (
                Color::srgba(0.3, 0.2, 0.07, 0.99),
                Color::srgb(1.0, 0.76, 0.28),
            ),
            Interaction::Hovered => (
                Color::srgba(0.16, 0.12, 0.06, 0.99),
                Color::srgb(0.9, 0.68, 0.32),
            ),
            Interaction::None => (
                Color::srgba(0.055, 0.055, 0.07, 0.97),
                Color::srgb(0.38, 0.42, 0.48),
            ),
        };
        *background = BackgroundColor(fill);
        *border = BorderColor::all(outline);
    }
}

fn browser_action_available(
    snapshot: &BrowserOnlineUiSnapshot,
    action: BrowserOnlineUiAction,
) -> bool {
    match action {
        BrowserOnlineUiAction::CreateRoom => {
            snapshot.screen == BrowserOnlineScreen::Menu && !snapshot.pending
        }
        BrowserOnlineUiAction::JoinRoom => {
            snapshot.screen == BrowserOnlineScreen::Menu
                && !snapshot.pending
                && snapshot.room_code_symbols.len() == 12
        }
        BrowserOnlineUiAction::FewerPlayers
        | BrowserOnlineUiAction::MorePlayers
        | BrowserOnlineUiAction::PreviousArena
        | BrowserOnlineUiAction::NextArena
        | BrowserOnlineUiAction::PreviousRules
        | BrowserOnlineUiAction::NextRules => {
            snapshot.screen == BrowserOnlineScreen::Menu && !snapshot.pending
        }
        BrowserOnlineUiAction::StartRoom => {
            snapshot.screen == BrowserOnlineScreen::Lobby
                && !snapshot.pending
                && snapshot.room.as_ref().is_some_and(|room| {
                    room.state == RoomState::Open && room.self_is_host() && room.member_count >= 2
                })
        }
        BrowserOnlineUiAction::Leave => matches!(
            snapshot.screen,
            BrowserOnlineScreen::Lobby
                | BrowserOnlineScreen::Connecting
                | BrowserOnlineScreen::Match
        ),
        BrowserOnlineUiAction::Back => matches!(
            snapshot.screen,
            BrowserOnlineScreen::Bootstrapping
                | BrowserOnlineScreen::Menu
                | BrowserOnlineScreen::Error
        ),
        BrowserOnlineUiAction::Retry => {
            snapshot.screen == BrowserOnlineScreen::Error && snapshot.retry_available
        }
    }
}

fn pressed_room_code_symbol(keys: &ButtonInput<KeyCode>) -> Option<char> {
    [
        (KeyCode::Digit0, '0'),
        (KeyCode::Digit1, '1'),
        (KeyCode::Digit2, '2'),
        (KeyCode::Digit3, '3'),
        (KeyCode::Digit4, '4'),
        (KeyCode::Digit5, '5'),
        (KeyCode::Digit6, '6'),
        (KeyCode::Digit7, '7'),
        (KeyCode::Digit8, '8'),
        (KeyCode::Digit9, '9'),
        (KeyCode::KeyA, 'A'),
        (KeyCode::KeyB, 'B'),
        (KeyCode::KeyC, 'C'),
        (KeyCode::KeyD, 'D'),
        (KeyCode::KeyE, 'E'),
        (KeyCode::KeyF, 'F'),
        (KeyCode::KeyG, 'G'),
        (KeyCode::KeyH, 'H'),
        (KeyCode::KeyI, '1'),
        (KeyCode::KeyJ, 'J'),
        (KeyCode::KeyK, 'K'),
        (KeyCode::KeyL, '1'),
        (KeyCode::KeyM, 'M'),
        (KeyCode::KeyN, 'N'),
        (KeyCode::KeyO, '0'),
        (KeyCode::KeyP, 'P'),
        (KeyCode::KeyQ, 'Q'),
        (KeyCode::KeyR, 'R'),
        (KeyCode::KeyS, 'S'),
        (KeyCode::KeyT, 'T'),
        (KeyCode::KeyV, 'V'),
        (KeyCode::KeyW, 'W'),
        (KeyCode::KeyX, 'X'),
        (KeyCode::KeyY, 'Y'),
        (KeyCode::KeyZ, 'Z'),
    ]
    .into_iter()
    .find_map(|(key, symbol)| keys.just_pressed(key).then_some(symbol))
}
