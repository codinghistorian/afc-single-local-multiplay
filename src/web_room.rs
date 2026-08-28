//! Bounded web lobby, reusable rooms, and fixed-tick hosted authorities.

mod worker;

use core::fmt;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::sync::broadcast;
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

use crate::arena_defs::arena_definitions;
use crate::characters::CharacterKind;
use crate::components::{LocalInputAssignment, ParticipantKind};
use crate::game_state::{LocalSetup, RULE_PRESETS};
use crate::headless::HeadlessMatchConfig;
use crate::match_config::{
    DEFAULT_INPUT_DELAY_TICKS, DEFAULT_ROLLBACK_LIMIT_TICKS, DEFAULT_SNAPSHOT_HISTORY_TICKS,
    MatchBuildOptions, build_headless_match_config,
};
use crate::network_codec::ResultIdentifier;
use crate::network_protocol::{
    AuthorityKind, MAX_FIGHTERS, MatchId, MatchManifest, PeerId, ReconnectClaim, SimTick,
};
use crate::reconnect::{AuthenticatedPeer, AuthenticatedUserId};
use crate::web_api::{
    ChatMessageResponse, ChatReportCategory, ChatScope, RoomVisibility, WebCharacter,
};
use crate::web_endpoint_adapters::ServerDatagramEndpoint;
use crate::web_identity::{
    GuestId, GuestSessionClaims, IssuedGuestSession, JoinTicketClaims, JoinTicketGrant,
    JoinTicketMode, PrivateRoomId, TicketReplayGuard, WebIdentityError, WebTokenKeyring,
};

use worker::WebRoomWorkerHandle;
pub use worker::{
    WebRoomWorkerConfig, WebRoomWorkerError, WebRoomWorkerPhase, WebRoomWorkerSnapshot,
};

const ROOM_CODE_SYMBOLS: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const ROOM_CODE_SYMBOL_COUNT: usize = 12;
const ROOM_CODE_GROUP_COUNT: usize = 3;
const DEFAULT_AGREED_START_TICK: SimTick = SimTick(120);
const MIN_ROOM_PLAYERS: u8 = 2;
const MAX_ROOM_PLAYERS: u8 = MAX_FIGHTERS as u8;
const MAX_ROOM_REGISTRY_CAPACITY: usize = 100_000;
const MAX_NICKNAME_GRAPHEMES: usize = 20;
const MIN_NICKNAME_GRAPHEMES: usize = 3;
const MAX_NICKNAME_BYTES: usize = 64;
const MAX_CHAT_GRAPHEMES: usize = 200;
const MAX_CHAT_BYTES: usize = 512;
const GLOBAL_CHAT_HISTORY: usize = 100;
const ROOM_CHAT_HISTORY: usize = 100;
const GLOBAL_CHAT_RETENTION_SECONDS: u64 = 15 * 60;
const CHAT_BURST_SECONDS: u64 = 10;
const CHAT_BURST_MESSAGES: usize = 5;
const CHAT_WINDOW_SECONDS: u64 = 60;
const CHAT_WINDOW_MESSAGES: usize = 30;
const CHAT_DUPLICATE_SECONDS: u64 = 10;
const MAX_REPORTS: usize = 256;
const REPORT_RETENTION_SECONDS: u64 = 15 * 60;
const MAX_LOBBY_EVENT_SUBSCRIBERS: usize = 4_096;

pub const DEFAULT_MAX_PRIVATE_ROOMS: usize = 512;
pub const DEFAULT_OPEN_ROOM_TTL_SECONDS: u64 = 15 * 60;
pub const DEFAULT_MAX_ROOM_LIFETIME_SECONDS: u64 = 4 * 60 * 60;
pub const DEFAULT_TERMINAL_ROOM_RETENTION_SECONDS: u64 = 5 * 60;
pub const DEFAULT_ROOM_PRESENCE_GRACE_SECONDS: u64 = 30;
pub const DEFAULT_RESULTS_RETURN_CEILING_SECONDS: u64 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WebRoomServiceConfig {
    pub maximum_rooms: usize,
    pub open_room_ttl_seconds: u64,
    pub maximum_room_lifetime_seconds: u64,
    /// Retained for configuration compatibility. Reusable rooms no longer use
    /// terminal retention after an ordinary confirmed result.
    pub terminal_room_retention_seconds: u64,
    pub presence_grace_seconds: u64,
    pub results_return_ceiling_seconds: u64,
    pub replay_cache_entries: usize,
    pub worker: WebRoomWorkerConfig,
}

impl Default for WebRoomServiceConfig {
    fn default() -> Self {
        Self {
            maximum_rooms: DEFAULT_MAX_PRIVATE_ROOMS,
            open_room_ttl_seconds: DEFAULT_OPEN_ROOM_TTL_SECONDS,
            maximum_room_lifetime_seconds: DEFAULT_MAX_ROOM_LIFETIME_SECONDS,
            terminal_room_retention_seconds: DEFAULT_TERMINAL_ROOM_RETENTION_SECONDS,
            presence_grace_seconds: DEFAULT_ROOM_PRESENCE_GRACE_SECONDS,
            results_return_ceiling_seconds: DEFAULT_RESULTS_RETURN_CEILING_SECONDS,
            replay_cache_entries: crate::web_identity::DEFAULT_REPLAY_CACHE_ENTRIES,
            worker: WebRoomWorkerConfig::default(),
        }
    }
}

impl WebRoomServiceConfig {
    pub fn validate(self) -> Result<(), WebRoomError> {
        if self.maximum_rooms == 0
            || self.maximum_rooms > MAX_ROOM_REGISTRY_CAPACITY
            || !(60..=24 * 60 * 60).contains(&self.open_room_ttl_seconds)
            || !(5 * 60..=24 * 60 * 60).contains(&self.maximum_room_lifetime_seconds)
            || self.maximum_room_lifetime_seconds < self.open_room_ttl_seconds
            || !(30..=60 * 60).contains(&self.terminal_room_retention_seconds)
            || !(5..=120).contains(&self.presence_grace_seconds)
            || !(6..=30).contains(&self.results_return_ceiling_seconds)
        {
            return Err(WebRoomError::InvalidConfiguration);
        }
        TicketReplayGuard::new(self.replay_cache_entries).map_err(WebRoomError::Identity)?;
        self.worker.validate().map_err(WebRoomError::Worker)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WebPrivateRoomOptions {
    pub maximum_players: u8,
    pub visibility: RoomVisibility,
    pub arena_index: usize,
    pub rule_index: usize,
}

impl Default for WebPrivateRoomOptions {
    fn default() -> Self {
        Self {
            maximum_players: 4,
            visibility: RoomVisibility::Private,
            arena_index: 0,
            rule_index: 0,
        }
    }
}

impl WebPrivateRoomOptions {
    fn validate(self) -> Result<(), WebRoomError> {
        if !(MIN_ROOM_PLAYERS..=MAX_ROOM_PLAYERS).contains(&self.maximum_players)
            || self.arena_index >= arena_definitions().len()
            || self.rule_index >= RULE_PRESETS.len()
        {
            return Err(WebRoomError::InvalidRoomOptions);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PrivateRoomCode([u8; ROOM_CODE_SYMBOL_COUNT]);

impl PrivateRoomCode {
    fn random() -> Result<Self, WebRoomError> {
        let mut entropy = [0_u8; 8];
        getrandom::fill(&mut entropy).map_err(|_| WebRoomError::RandomnessUnavailable)?;
        let bits = u64::from_be_bytes(entropy);
        let mut symbols = [0_u8; ROOM_CODE_SYMBOL_COUNT];
        for (index, symbol) in symbols.iter_mut().enumerate() {
            let shift = 64 - 5 * (index + 1);
            *symbol = ROOM_CODE_SYMBOLS[((bits >> shift) & 0x1f) as usize];
        }
        Ok(Self(symbols))
    }

    pub fn canonical(self) -> String {
        String::from_utf8(self.0.to_vec()).expect("room code alphabet is ASCII")
    }
}

impl fmt::Display for PrivateRoomCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for group in 0..ROOM_CODE_GROUP_COUNT {
            if group != 0 {
                formatter.write_str("-")?;
            }
            let start = group * 4;
            let end = start + 4;
            formatter.write_str(
                std::str::from_utf8(&self.0[start..end]).expect("room code alphabet is ASCII"),
            )?;
        }
        Ok(())
    }
}

impl FromStr for PrivateRoomCode {
    type Err = WebRoomError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut symbols = [0_u8; ROOM_CODE_SYMBOL_COUNT];
        let mut count = 0;
        for byte in value.bytes() {
            if byte == b'-' {
                continue;
            }
            if count == ROOM_CODE_SYMBOL_COUNT {
                return Err(WebRoomError::InvalidRoomCode);
            }
            let upper = byte.to_ascii_uppercase();
            let canonical = match upper {
                b'O' => b'0',
                b'I' | b'L' => b'1',
                candidate if ROOM_CODE_SYMBOLS.contains(&candidate) => candidate,
                _ => return Err(WebRoomError::InvalidRoomCode),
            };
            symbols[count] = canonical;
            count += 1;
        }
        if count != ROOM_CODE_SYMBOL_COUNT {
            return Err(WebRoomError::InvalidRoomCode);
        }
        Ok(Self(symbols))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebPrivateRoomState {
    Open,
    Starting,
    Active,
    Results,
    Returning,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebRoomMemberView {
    pub peer_id: PeerId,
    pub display_name: String,
    pub is_host: bool,
    pub is_self: bool,
    pub present: bool,
    pub gameplay_connected: bool,
    pub character: WebCharacter,
    pub ready: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebRoomResultView {
    pub match_id: String,
    pub result_id: u64,
    pub final_tick: u64,
    pub final_state_hash: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebPrivateRoomView {
    pub room_code: PrivateRoomCode,
    pub revision: u64,
    pub match_epoch: u64,
    pub visibility: RoomVisibility,
    pub state: WebPrivateRoomState,
    pub maximum_players: u8,
    pub member_count: u8,
    pub arena_index: usize,
    pub rule_index: usize,
    pub members: Vec<WebRoomMemberView>,
    pub room_chat: Vec<ChatMessageResponse>,
    pub manifest: Option<MatchManifest>,
    pub result: Option<WebRoomResultView>,
    pub worker: Option<WebRoomWorkerSnapshot>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebPublicRoomView {
    pub room_code: PrivateRoomCode,
    pub revision: u64,
    pub host_display_name: String,
    pub state: WebPrivateRoomState,
    pub member_count: u8,
    pub maximum_players: u8,
    pub arena_index: usize,
    pub rule_index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebLobbySnapshotView {
    pub revision: u64,
    pub online_guests: u32,
    pub public_rooms: Vec<WebPublicRoomView>,
    pub global_chat: Vec<ChatMessageResponse>,
    pub active_room: Option<WebPrivateRoomView>,
}

#[derive(Clone, Debug)]
pub enum WebLobbyPush {
    StateChanged { revision: u64 },
    Chat(ChatMessageResponse),
}

pub struct WebLobbyConnection {
    pub guest_id: GuestId,
    pub snapshot: WebLobbySnapshotView,
    pub receiver: broadcast::Receiver<WebLobbyPush>,
}

#[derive(Clone, Debug)]
pub struct IssuedWebGuestSession {
    pub issued: IssuedGuestSession,
    pub nickname: String,
    pub display_name: String,
}

/// Privacy-safe process snapshot for readiness dashboards and alerting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WebRoomOperationalSnapshot {
    pub total_rooms: u64,
    pub open_rooms: u64,
    pub starting_rooms: u64,
    pub active_rooms: u64,
    pub results_rooms: u64,
    pub returning_rooms: u64,
    pub finished_rooms: u64,
    pub failed_rooms: u64,
    pub online_guests: u64,
    pub chat_reports: u64,
    pub connected_peers: u64,
    pub maximum_worker_tick_p99_ns: u64,
    pub maximum_worker_tick_ns: u64,
    pub worker_over_budget_observations: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebJoinTicketResponse {
    pub ticket: String,
    pub expires_at_unix_seconds: u64,
    pub peer_id: PeerId,
    pub manifest: MatchManifest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmittedWebPeer {
    pub connection: crate::authority_peer_hub::AuthorityConnectionId,
    pub room_id: PrivateRoomId,
    pub match_id: MatchId,
    pub peer_id: PeerId,
    pub mode: JoinTicketMode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebRoomError {
    InvalidConfiguration,
    InvalidNickname,
    InvalidChatMessage,
    InvalidRoomOptions,
    InvalidRoomCode,
    RandomnessUnavailable,
    RegistryFull,
    RoomCodeExhausted,
    RoomNotFound,
    RoomNotOpen,
    RoomFull,
    GuestIdentityConflict,
    GuestNotRegistered,
    GuestAlreadyInRoom,
    GuestNotMember,
    GuestRoomBanned,
    HostOnly,
    CannotKickSelf,
    MemberNotFound,
    TooFewPlayers,
    MembersNotReady,
    RevisionConflict,
    RoomStarting,
    RoomAlreadyActive,
    RoomNoLongerStarting,
    ResultNotAvailable,
    ResultMismatch,
    AlreadyConnected,
    ReconnectRequired,
    InvalidReconnectTick,
    TicketClaimsMismatch,
    ChatRateLimited,
    ChatTargetNotFound,
    Identity(WebIdentityError),
    Worker(WebRoomWorkerError),
    WorkerTaskCancelled,
}

impl fmt::Display for WebRoomError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "hosted web-room operation failed: {self:?}")
    }
}

impl std::error::Error for WebRoomError {}

impl From<WebIdentityError> for WebRoomError {
    fn from(error: WebIdentityError) -> Self {
        Self::Identity(error)
    }
}

impl From<WebRoomWorkerError> for WebRoomError {
    fn from(error: WebRoomWorkerError) -> Self {
        Self::Worker(error)
    }
}

#[derive(Clone)]
pub struct WebRoomService {
    keyring: Arc<WebTokenKeyring>,
    config: WebRoomServiceConfig,
    registry: Arc<Mutex<WebRoomRegistry>>,
    events: Arc<broadcast::Sender<WebLobbyPush>>,
}

impl WebRoomService {
    pub fn new(
        keyring: WebTokenKeyring,
        config: WebRoomServiceConfig,
    ) -> Result<Self, WebRoomError> {
        config.validate()?;
        let replay = TicketReplayGuard::new(config.replay_cache_entries)?;
        let (events, _) = broadcast::channel(MAX_LOBBY_EVENT_SUBSCRIBERS);
        Ok(Self {
            keyring: Arc::new(keyring),
            config,
            registry: Arc::new(Mutex::new(WebRoomRegistry::new(replay))),
            events: Arc::new(events),
        })
    }

    /// Compatibility helper for transport tests. Player-facing callers use
    /// `issue_named_guest_session`.
    pub fn issue_guest_session(
        &self,
        now_unix_seconds: u64,
    ) -> Result<IssuedGuestSession, WebRoomError> {
        self.issue_named_guest_session("Guest", now_unix_seconds)
            .map(|session| session.issued)
    }

    pub fn issue_named_guest_session(
        &self,
        nickname: &str,
        now_unix_seconds: u64,
    ) -> Result<IssuedWebGuestSession, WebRoomError> {
        let nickname = normalize_nickname(nickname)?;
        let issued = self.keyring.issue_guest_session(now_unix_seconds)?;
        let mut registry = lock_recover(&self.registry);
        registry.maintain(now_unix_seconds, self.config);
        let collision = registry.guests.values().any(|profile| {
            profile.expires_at_unix_seconds >= now_unix_seconds
                && profile.nickname.to_lowercase() == nickname.to_lowercase()
        });
        let display_name = if collision {
            let encoded = issued.claims.guest_id.encoded();
            format!("{nickname}#{}", &encoded[..4])
        } else {
            nickname.clone()
        };
        registry.guests.insert(
            issued.claims.guest_id,
            WebGuestProfile {
                user_id: issued.claims.user_id,
                nickname: nickname.clone(),
                display_name: display_name.clone(),
                expires_at_unix_seconds: issued.claims.expires_at_unix_seconds,
                active_control_connections: 0,
                present_until_unix_seconds: now_unix_seconds
                    .saturating_add(self.config.presence_grace_seconds),
                chat_times: VecDeque::new(),
                recent_messages: VecDeque::new(),
            },
        );
        let revision = registry.bump_lobby_revision();
        drop(registry);
        self.push_state_changed(revision);
        Ok(IssuedWebGuestSession {
            issued,
            nickname,
            display_name,
        })
    }

    pub fn connect_lobby(
        &self,
        guest_session: &str,
        now_unix_seconds: u64,
    ) -> Result<WebLobbyConnection, WebRoomError> {
        let claims = self
            .keyring
            .verify_guest_session(guest_session, now_unix_seconds)?;
        let receiver = self.events.subscribe();
        let mut registry = lock_recover(&self.registry);
        let changed = registry.maintain(now_unix_seconds, self.config);
        let profile = registry.require_guest_mut(claims)?;
        profile.active_control_connections = profile.active_control_connections.saturating_add(1);
        profile.touch(now_unix_seconds, self.config.presence_grace_seconds);
        let revision = registry.bump_lobby_revision();
        let snapshot = registry.lobby_snapshot(claims.guest_id, now_unix_seconds);
        drop(registry);
        if changed {
            self.push_state_changed(revision);
        } else {
            self.push_state_changed(revision);
        }
        Ok(WebLobbyConnection {
            guest_id: claims.guest_id,
            snapshot,
            receiver,
        })
    }

    pub fn disconnect_lobby(&self, guest_id: GuestId, now_unix_seconds: u64) {
        let mut registry = lock_recover(&self.registry);
        let Some(profile) = registry.guests.get_mut(&guest_id) else {
            return;
        };
        profile.active_control_connections = profile.active_control_connections.saturating_sub(1);
        profile.touch(now_unix_seconds, self.config.presence_grace_seconds);
        let revision = registry.bump_lobby_revision();
        drop(registry);
        self.push_state_changed(revision);
    }

    pub fn lobby_snapshot(
        &self,
        guest_session: &str,
        now_unix_seconds: u64,
    ) -> Result<WebLobbySnapshotView, WebRoomError> {
        let claims = self
            .keyring
            .verify_guest_session(guest_session, now_unix_seconds)?;
        let mut registry = lock_recover(&self.registry);
        let changed = registry.maintain(now_unix_seconds, self.config);
        registry.touch_guest(claims, now_unix_seconds, self.config.presence_grace_seconds)?;
        let snapshot = registry.lobby_snapshot(claims.guest_id, now_unix_seconds);
        let revision = registry.lobby_revision;
        drop(registry);
        if changed {
            self.push_state_changed(revision);
        }
        Ok(snapshot)
    }

    pub fn operational_snapshot(&self) -> WebRoomOperationalSnapshot {
        let now = unix_now_fallback();
        let mut registry = lock_recover(&self.registry);
        let changed = registry.maintain(now, self.config);
        let mut operational = WebRoomOperationalSnapshot {
            total_rooms: registry.rooms.len() as u64,
            online_guests: registry
                .guests
                .values()
                .filter(|profile| profile.is_present(now))
                .count() as u64,
            chat_reports: registry.reports.len() as u64,
            ..WebRoomOperationalSnapshot::default()
        };
        for room in registry.rooms.values() {
            match &room.lifecycle {
                RoomLifecycle::Open => {
                    operational.open_rooms = operational.open_rooms.saturating_add(1);
                }
                RoomLifecycle::Starting { .. } => {
                    operational.starting_rooms = operational.starting_rooms.saturating_add(1);
                }
                RoomLifecycle::Active {
                    worker,
                    result_since_unix_seconds,
                    returning_since_unix_seconds,
                    ..
                } => {
                    let snapshot = worker.snapshot();
                    operational.connected_peers = operational
                        .connected_peers
                        .saturating_add(u64::from(snapshot.connected_peers));
                    operational.maximum_worker_tick_p99_ns = operational
                        .maximum_worker_tick_p99_ns
                        .max(snapshot.server_ticks.p99_ns);
                    operational.maximum_worker_tick_ns = operational
                        .maximum_worker_tick_ns
                        .max(snapshot.server_ticks.maximum_ns);
                    operational.worker_over_budget_observations = operational
                        .worker_over_budget_observations
                        .saturating_add(snapshot.server_ticks.over_budget);
                    if returning_since_unix_seconds.is_some() {
                        operational.returning_rooms = operational.returning_rooms.saturating_add(1);
                    } else if result_since_unix_seconds.is_some()
                        || snapshot.phase == WebRoomWorkerPhase::Finished
                    {
                        operational.results_rooms = operational.results_rooms.saturating_add(1);
                        operational.finished_rooms = operational.finished_rooms.saturating_add(1);
                    } else if snapshot.phase == WebRoomWorkerPhase::Failed {
                        operational.failed_rooms = operational.failed_rooms.saturating_add(1);
                    } else {
                        operational.active_rooms = operational.active_rooms.saturating_add(1);
                    }
                }
            }
        }
        let revision = registry.lobby_revision;
        drop(registry);
        if changed {
            self.push_state_changed(revision);
        }
        operational
    }

    pub fn create_room(
        &self,
        guest_session: &str,
        options: WebPrivateRoomOptions,
        now_unix_seconds: u64,
    ) -> Result<WebPrivateRoomView, WebRoomError> {
        options.validate()?;
        let claims = self
            .keyring
            .verify_guest_session(guest_session, now_unix_seconds)?;
        let mut registry = lock_recover(&self.registry);
        registry.maintain(now_unix_seconds, self.config);
        registry.touch_guest(claims, now_unix_seconds, self.config.presence_grace_seconds)?;
        if registry.room_for_guest(claims.guest_id).is_some() {
            return Err(WebRoomError::GuestAlreadyInRoom);
        }
        if registry.rooms.len() >= self.config.maximum_rooms {
            return Err(WebRoomError::RegistryFull);
        }
        let room_id = unique_room_id(&registry)?;
        let room_code = unique_room_code(&registry)?;
        let host = WebRoomMember {
            guest_id: claims.guest_id,
            user_id: claims.user_id,
            peer_id: PeerId::new(1).expect("the first room peer id is non-zero"),
            is_host: true,
            character: WebCharacter::Cat,
            ready: false,
            joined_order: 1,
        };
        let room = WebPrivateRoom {
            room_code,
            options,
            revision: 1,
            match_epoch: 0,
            members: vec![host],
            banned_guests: HashSet::new(),
            room_chat: VecDeque::new(),
            next_peer_id: 2,
            next_joined_order: 2,
            lifecycle: RoomLifecycle::Open,
            created_at_unix_seconds: now_unix_seconds,
            last_activity_unix_seconds: now_unix_seconds,
        };
        registry.codes.insert(room_code, room_id);
        registry.rooms.insert(room_id, room);
        let revision = registry.bump_lobby_revision();
        let view = registry
            .rooms
            .get(&room_id)
            .expect("the room was just inserted")
            .view(claims.guest_id, &registry.guests, now_unix_seconds);
        drop(registry);
        self.push_state_changed(revision);
        Ok(view)
    }

    pub fn create_private_room(
        &self,
        guest_session: &str,
        options: WebPrivateRoomOptions,
        now_unix_seconds: u64,
    ) -> Result<WebPrivateRoomView, WebRoomError> {
        self.create_room(guest_session, options, now_unix_seconds)
    }

    pub fn join_private_room(
        &self,
        guest_session: &str,
        room_code: &str,
        now_unix_seconds: u64,
    ) -> Result<WebPrivateRoomView, WebRoomError> {
        let claims = self
            .keyring
            .verify_guest_session(guest_session, now_unix_seconds)?;
        let room_code = PrivateRoomCode::from_str(room_code)?;
        let mut registry = lock_recover(&self.registry);
        registry.maintain(now_unix_seconds, self.config);
        registry.touch_guest(claims, now_unix_seconds, self.config.presence_grace_seconds)?;
        if let Some(existing_room_id) = registry.room_for_guest(claims.guest_id) {
            let existing = registry
                .rooms
                .get(&existing_room_id)
                .ok_or(WebRoomError::RoomNotFound)?;
            if existing.room_code != room_code {
                return Err(WebRoomError::GuestAlreadyInRoom);
            }
            return Ok(existing.view(claims.guest_id, &registry.guests, now_unix_seconds));
        }
        let room_id = *registry
            .codes
            .get(&room_code)
            .ok_or(WebRoomError::RoomNotFound)?;
        let room = registry
            .rooms
            .get_mut(&room_id)
            .ok_or(WebRoomError::RoomNotFound)?;
        if room.banned_guests.contains(&claims.guest_id) {
            return Err(WebRoomError::GuestRoomBanned);
        }
        if !matches!(room.lifecycle, RoomLifecycle::Open) {
            return Err(WebRoomError::RoomNotOpen);
        }
        if room.members.len() >= usize::from(room.options.maximum_players) {
            return Err(WebRoomError::RoomFull);
        }
        if room
            .members
            .iter()
            .any(|member| member.user_id == claims.user_id)
        {
            return Err(WebRoomError::GuestIdentityConflict);
        }
        let peer_id =
            PeerId::new(room.next_peer_id).map_err(|_| WebRoomError::GuestIdentityConflict)?;
        room.next_peer_id = room
            .next_peer_id
            .checked_add(1)
            .ok_or(WebRoomError::GuestIdentityConflict)?;
        let joined_order = room.next_joined_order;
        room.next_joined_order = room.next_joined_order.saturating_add(1);
        let default_character = WebCharacter::ALL[room.members.len() % WebCharacter::ALL.len()];
        room.members.push(WebRoomMember {
            guest_id: claims.guest_id,
            user_id: claims.user_id,
            peer_id,
            is_host: false,
            character: default_character,
            ready: false,
            joined_order,
        });
        room.clear_ready();
        room.bump_revision();
        room.last_activity_unix_seconds = now_unix_seconds;
        let revision = registry.bump_lobby_revision();
        let view = registry
            .rooms
            .get(&room_id)
            .expect("joined room remains registered")
            .view(claims.guest_id, &registry.guests, now_unix_seconds);
        drop(registry);
        self.push_state_changed(revision);
        Ok(view)
    }

    pub fn private_room_status(
        &self,
        guest_session: &str,
        room_code: &str,
        now_unix_seconds: u64,
    ) -> Result<WebPrivateRoomView, WebRoomError> {
        let claims = self
            .keyring
            .verify_guest_session(guest_session, now_unix_seconds)?;
        let room_code = PrivateRoomCode::from_str(room_code)?;
        let mut registry = lock_recover(&self.registry);
        let changed = registry.maintain(now_unix_seconds, self.config);
        registry.touch_guest(claims, now_unix_seconds, self.config.presence_grace_seconds)?;
        let room_id = *registry
            .codes
            .get(&room_code)
            .ok_or(WebRoomError::RoomNotFound)?;
        {
            let room = registry
                .rooms
                .get_mut(&room_id)
                .ok_or(WebRoomError::RoomNotFound)?;
            room.require_member(claims)?;
            room.last_activity_unix_seconds = now_unix_seconds;
        }
        let view =
            registry.rooms[&room_id].view(claims.guest_id, &registry.guests, now_unix_seconds);
        let revision = registry.lobby_revision;
        drop(registry);
        if changed {
            self.push_state_changed(revision);
        }
        Ok(view)
    }

    pub fn update_room_settings(
        &self,
        guest_session: &str,
        room_code: &str,
        expected_revision: u64,
        arena_index: usize,
        rule_index: usize,
        now_unix_seconds: u64,
    ) -> Result<WebPrivateRoomView, WebRoomError> {
        if arena_index >= arena_definitions().len() || rule_index >= RULE_PRESETS.len() {
            return Err(WebRoomError::InvalidRoomOptions);
        }
        let claims = self
            .keyring
            .verify_guest_session(guest_session, now_unix_seconds)?;
        let room_code = PrivateRoomCode::from_str(room_code)?;
        let mut registry = lock_recover(&self.registry);
        registry.maintain(now_unix_seconds, self.config);
        registry.touch_guest(claims, now_unix_seconds, self.config.presence_grace_seconds)?;
        let room_id = registry.room_id(room_code)?;
        {
            let room = registry
                .rooms
                .get_mut(&room_id)
                .ok_or(WebRoomError::RoomNotFound)?;
            room.require_open_revision(expected_revision)?;
            if !room.require_member(claims)?.is_host {
                return Err(WebRoomError::HostOnly);
            }
            if room.options.arena_index != arena_index || room.options.rule_index != rule_index {
                room.options.arena_index = arena_index;
                room.options.rule_index = rule_index;
                room.clear_ready();
                room.bump_revision();
            }
            room.last_activity_unix_seconds = now_unix_seconds;
        }
        let revision = registry.bump_lobby_revision();
        let view =
            registry.rooms[&room_id].view(claims.guest_id, &registry.guests, now_unix_seconds);
        drop(registry);
        self.push_state_changed(revision);
        Ok(view)
    }

    pub fn select_character(
        &self,
        guest_session: &str,
        room_code: &str,
        expected_revision: u64,
        character: WebCharacter,
        now_unix_seconds: u64,
    ) -> Result<WebPrivateRoomView, WebRoomError> {
        let claims = self
            .keyring
            .verify_guest_session(guest_session, now_unix_seconds)?;
        let room_code = PrivateRoomCode::from_str(room_code)?;
        let mut registry = lock_recover(&self.registry);
        registry.maintain(now_unix_seconds, self.config);
        registry.touch_guest(claims, now_unix_seconds, self.config.presence_grace_seconds)?;
        let room_id = registry.room_id(room_code)?;
        {
            let room = registry
                .rooms
                .get_mut(&room_id)
                .ok_or(WebRoomError::RoomNotFound)?;
            room.require_open_revision(expected_revision)?;
            let member = room.require_member_mut(claims)?;
            if member.character != character {
                member.character = character;
                member.ready = false;
                room.bump_revision();
            }
            room.last_activity_unix_seconds = now_unix_seconds;
        }
        let revision = registry.bump_lobby_revision();
        let view =
            registry.rooms[&room_id].view(claims.guest_id, &registry.guests, now_unix_seconds);
        drop(registry);
        self.push_state_changed(revision);
        Ok(view)
    }

    pub fn set_ready(
        &self,
        guest_session: &str,
        room_code: &str,
        expected_revision: u64,
        ready: bool,
        now_unix_seconds: u64,
    ) -> Result<WebPrivateRoomView, WebRoomError> {
        let claims = self
            .keyring
            .verify_guest_session(guest_session, now_unix_seconds)?;
        let room_code = PrivateRoomCode::from_str(room_code)?;
        let mut registry = lock_recover(&self.registry);
        registry.maintain(now_unix_seconds, self.config);
        registry.touch_guest(claims, now_unix_seconds, self.config.presence_grace_seconds)?;
        let room_id = registry.room_id(room_code)?;
        {
            let room = registry
                .rooms
                .get_mut(&room_id)
                .ok_or(WebRoomError::RoomNotFound)?;
            room.require_open_revision(expected_revision)?;
            let member = room.require_member_mut(claims)?;
            if member.ready != ready {
                member.ready = ready;
                room.bump_revision();
            }
            room.last_activity_unix_seconds = now_unix_seconds;
        }
        let revision = registry.bump_lobby_revision();
        let view =
            registry.rooms[&room_id].view(claims.guest_id, &registry.guests, now_unix_seconds);
        drop(registry);
        self.push_state_changed(revision);
        Ok(view)
    }

    pub fn kick_and_ban_member(
        &self,
        guest_session: &str,
        room_code: &str,
        expected_revision: u64,
        peer_id: PeerId,
        now_unix_seconds: u64,
    ) -> Result<WebPrivateRoomView, WebRoomError> {
        let claims = self
            .keyring
            .verify_guest_session(guest_session, now_unix_seconds)?;
        let room_code = PrivateRoomCode::from_str(room_code)?;
        let mut registry = lock_recover(&self.registry);
        registry.maintain(now_unix_seconds, self.config);
        registry.touch_guest(claims, now_unix_seconds, self.config.presence_grace_seconds)?;
        let room_id = registry.room_id(room_code)?;
        {
            let room = registry
                .rooms
                .get_mut(&room_id)
                .ok_or(WebRoomError::RoomNotFound)?;
            room.require_open_revision(expected_revision)?;
            let host = room.require_member(claims)?;
            if !host.is_host {
                return Err(WebRoomError::HostOnly);
            }
            if host.peer_id == peer_id {
                return Err(WebRoomError::CannotKickSelf);
            }
            let index = room
                .members
                .iter()
                .position(|member| member.peer_id == peer_id)
                .ok_or(WebRoomError::MemberNotFound)?;
            let removed = room.members.remove(index);
            room.banned_guests.insert(removed.guest_id);
            room.clear_ready();
            room.bump_revision();
            room.last_activity_unix_seconds = now_unix_seconds;
        }
        let revision = registry.bump_lobby_revision();
        let view =
            registry.rooms[&room_id].view(claims.guest_id, &registry.guests, now_unix_seconds);
        drop(registry);
        self.push_state_changed(revision);
        Ok(view)
    }

    pub fn leave_private_room(
        &self,
        guest_session: &str,
        room_code: &str,
        now_unix_seconds: u64,
    ) -> Result<(), WebRoomError> {
        let claims = self
            .keyring
            .verify_guest_session(guest_session, now_unix_seconds)?;
        let room_code = PrivateRoomCode::from_str(room_code)?;
        let mut registry = lock_recover(&self.registry);
        registry.maintain(now_unix_seconds, self.config);
        registry.touch_guest(claims, now_unix_seconds, self.config.presence_grace_seconds)?;
        let room_id = registry.room_id(room_code)?;
        let remove_room = {
            let room = registry
                .rooms
                .get_mut(&room_id)
                .ok_or(WebRoomError::RoomNotFound)?;
            if !matches!(room.lifecycle, RoomLifecycle::Open) {
                return Err(WebRoomError::RoomNotOpen);
            }
            let index = room
                .members
                .iter()
                .position(|member| {
                    member.guest_id == claims.guest_id && member.user_id == claims.user_id
                })
                .ok_or(WebRoomError::GuestNotMember)?;
            let was_host = room.members[index].is_host;
            room.members.remove(index);
            if was_host {
                room.migrate_host();
            }
            room.clear_ready();
            room.bump_revision();
            room.last_activity_unix_seconds = now_unix_seconds;
            room.members.is_empty()
        };
        if remove_room {
            registry.rooms.remove(&room_id);
            registry.codes.remove(&room_code);
        }
        let revision = registry.bump_lobby_revision();
        drop(registry);
        self.push_state_changed(revision);
        Ok(())
    }

    pub async fn start_private_room(
        &self,
        guest_session: &str,
        room_code: &str,
        expected_revision: u64,
        now_unix_seconds: u64,
    ) -> Result<WebPrivateRoomView, WebRoomError> {
        let claims = self
            .keyring
            .verify_guest_session(guest_session, now_unix_seconds)?;
        let room_code = PrivateRoomCode::from_str(room_code)?;
        let prepared = {
            let mut registry = lock_recover(&self.registry);
            registry.maintain(now_unix_seconds, self.config);
            registry.touch_guest(claims, now_unix_seconds, self.config.presence_grace_seconds)?;
            registry.prepare_start(room_code, claims, expected_revision, now_unix_seconds)?
        };
        let worker_config = self.config.worker;
        let match_config = prepared.match_config.clone();
        let roster = prepared.roster.clone();
        let built = match tokio::task::spawn_blocking(move || {
            WebRoomWorkerHandle::spawn(match_config, roster, worker_config)
        })
        .await
        {
            Ok(built) => built,
            Err(_) => {
                let revision = lock_recover(&self.registry)
                    .cancel_start(prepared.room_id, prepared.startup_id);
                if let Some(revision) = revision {
                    self.push_state_changed(revision);
                }
                return Err(WebRoomError::WorkerTaskCancelled);
            }
        };
        let worker = match built {
            Ok(worker) => worker,
            Err(error) => {
                let revision = lock_recover(&self.registry)
                    .cancel_start(prepared.room_id, prepared.startup_id);
                if let Some(revision) = revision {
                    self.push_state_changed(revision);
                }
                return Err(WebRoomError::Worker(error));
            }
        };

        let mut registry = lock_recover(&self.registry);
        let result = registry.install_worker(
            prepared.room_id,
            prepared.startup_id,
            prepared.manifest,
            worker.clone(),
            now_unix_seconds,
            claims.guest_id,
        );
        let revision = registry.lobby_revision;
        drop(registry);
        if result.is_err() {
            let _ = worker.request_shutdown();
        } else {
            self.push_state_changed(revision);
        }
        result
    }

    pub fn acknowledge_result(
        &self,
        guest_session: &str,
        room_code: &str,
        match_id: &str,
        now_unix_seconds: u64,
    ) -> Result<WebPrivateRoomView, WebRoomError> {
        let claims = self
            .keyring
            .verify_guest_session(guest_session, now_unix_seconds)?;
        let room_code = PrivateRoomCode::from_str(room_code)?;
        let mut registry = lock_recover(&self.registry);
        registry.maintain(now_unix_seconds, self.config);
        registry.touch_guest(claims, now_unix_seconds, self.config.presence_grace_seconds)?;
        let room_id = registry.room_id(room_code)?;
        {
            let room = registry
                .rooms
                .get_mut(&room_id)
                .ok_or(WebRoomError::RoomNotFound)?;
            room.require_member(claims)?;
            let RoomLifecycle::Active {
                manifest,
                result_since_unix_seconds,
                result_acks,
                ..
            } = &mut room.lifecycle
            else {
                return Err(WebRoomError::ResultNotAvailable);
            };
            if result_since_unix_seconds.is_none() {
                return Err(WebRoomError::ResultNotAvailable);
            }
            if match_id_hex(manifest.match_id) != match_id {
                return Err(WebRoomError::ResultMismatch);
            }
            if result_acks.insert(claims.guest_id) {
                room.bump_revision();
            }
        }
        registry.maintain(now_unix_seconds, self.config);
        let revision = registry.bump_lobby_revision();
        let view =
            registry.rooms[&room_id].view(claims.guest_id, &registry.guests, now_unix_seconds);
        drop(registry);
        self.push_state_changed(revision);
        Ok(view)
    }

    pub fn send_chat(
        &self,
        guest_session: &str,
        scope: ChatScope,
        text: &str,
        now_unix_seconds: u64,
    ) -> Result<ChatMessageResponse, WebRoomError> {
        let claims = self
            .keyring
            .verify_guest_session(guest_session, now_unix_seconds)?;
        let text = normalize_chat(text)?;
        let mut registry = lock_recover(&self.registry);
        registry.maintain(now_unix_seconds, self.config);
        let profile = registry.require_guest_mut(claims)?;
        profile.touch(now_unix_seconds, self.config.presence_grace_seconds);
        profile.admit_chat(&text, now_unix_seconds)?;
        let sender_display_name = profile.display_name.clone();
        let room_id = match scope {
            ChatScope::Global => None,
            ChatScope::Room => Some(
                registry
                    .room_for_guest(claims.guest_id)
                    .ok_or(WebRoomError::GuestNotMember)?,
            ),
        };
        let room_code = room_id.map(|room_id| registry.rooms[&room_id].room_code.to_string());
        let message_id = registry.next_chat_message_id;
        registry.next_chat_message_id = registry.next_chat_message_id.saturating_add(1).max(1);
        let message = ChatMessageResponse {
            message_id,
            scope,
            room_code,
            sender_guest_id: claims.guest_id.encoded(),
            sender_display_name,
            sent_at_unix_seconds: now_unix_seconds,
            text,
        };
        match room_id {
            None => push_bounded(
                &mut registry.global_chat,
                message.clone(),
                GLOBAL_CHAT_HISTORY,
            ),
            Some(room_id) => {
                let room = registry
                    .rooms
                    .get_mut(&room_id)
                    .ok_or(WebRoomError::RoomNotFound)?;
                push_bounded(&mut room.room_chat, message.clone(), ROOM_CHAT_HISTORY);
                room.last_activity_unix_seconds = now_unix_seconds;
            }
        }
        registry.bump_lobby_revision();
        drop(registry);
        let _ = self.events.send(WebLobbyPush::Chat(message.clone()));
        Ok(message)
    }

    pub fn report_chat(
        &self,
        guest_session: &str,
        message_id: Option<u64>,
        target_guest_id: &str,
        category: ChatReportCategory,
        now_unix_seconds: u64,
    ) -> Result<(), WebRoomError> {
        let claims = self
            .keyring
            .verify_guest_session(guest_session, now_unix_seconds)?;
        let mut registry = lock_recover(&self.registry);
        registry.maintain(now_unix_seconds, self.config);
        registry.touch_guest(claims, now_unix_seconds, self.config.presence_grace_seconds)?;
        let target = registry
            .guests
            .keys()
            .copied()
            .find(|guest_id| guest_id.encoded() == target_guest_id)
            .ok_or(WebRoomError::ChatTargetNotFound)?;
        if let Some(message_id) = message_id
            && !registry.message_exists(message_id, target)
        {
            return Err(WebRoomError::ChatTargetNotFound);
        }
        push_bounded(
            &mut registry.reports,
            ChatReportRecord {
                reporter: claims.guest_id,
                target,
                message_id,
                category,
                reported_at_unix_seconds: now_unix_seconds,
            },
            MAX_REPORTS,
        );
        Ok(())
    }

    pub fn chat_visible_to(&self, guest_id: GuestId, message: &ChatMessageResponse) -> bool {
        if message.scope == ChatScope::Global {
            return true;
        }
        let registry = lock_recover(&self.registry);
        registry
            .room_for_guest(guest_id)
            .and_then(|room_id| registry.rooms.get(&room_id))
            .is_some_and(|room| {
                message
                    .room_code
                    .as_deref()
                    .is_some_and(|code| code == room.room_code.to_string())
            })
    }

    pub fn issue_join_ticket(
        &self,
        guest_session: &str,
        room_code: &str,
        mode: JoinTicketMode,
        now_unix_seconds: u64,
    ) -> Result<WebJoinTicketResponse, WebRoomError> {
        let claims = self
            .keyring
            .verify_guest_session(guest_session, now_unix_seconds)?;
        let room_code = PrivateRoomCode::from_str(room_code)?;
        let (grant, manifest) = {
            let mut registry = lock_recover(&self.registry);
            registry.maintain(now_unix_seconds, self.config);
            registry.touch_guest(claims, now_unix_seconds, self.config.presence_grace_seconds)?;
            registry.join_ticket_grant(room_code, claims, mode, now_unix_seconds)?
        };
        let issued = self.keyring.issue_join_ticket(grant, now_unix_seconds)?;
        Ok(WebJoinTicketResponse {
            ticket: issued.token,
            expires_at_unix_seconds: issued.claims.expires_at_unix_seconds,
            peer_id: grant.peer_id,
            manifest,
        })
    }

    pub async fn admit_join_ticket(
        &self,
        ticket: &str,
        endpoint: ServerDatagramEndpoint,
        now_unix_seconds: u64,
    ) -> Result<AdmittedWebPeer, WebRoomError> {
        let claims = self.keyring.verify_join_ticket(ticket, now_unix_seconds)?;
        let worker = {
            let mut registry = lock_recover(&self.registry);
            registry.maintain(now_unix_seconds, self.config);
            registry.consume_admission(claims, now_unix_seconds)?
        };
        let connection = match claims.grant.mode {
            JoinTicketMode::Initial => {
                worker
                    .attach_initial(claims.grant.peer_id, claims.grant.user_id, endpoint)
                    .await?
            }
            JoinTicketMode::Reconnect {
                last_confirmed_tick,
            } => {
                worker
                    .attach_reconnect(
                        claims.grant.user_id,
                        ReconnectClaim {
                            match_id: claims.grant.match_id,
                            peer_id: claims.grant.peer_id,
                            last_confirmed_tick,
                        },
                        endpoint,
                    )
                    .await?
            }
        };
        Ok(AdmittedWebPeer {
            connection,
            room_id: claims.grant.room_id,
            match_id: claims.grant.match_id,
            peer_id: claims.grant.peer_id,
            mode: claims.grant.mode,
        })
    }

    pub async fn shutdown_all(&self) {
        let workers = {
            let mut registry = lock_recover(&self.registry);
            registry.take_all_workers()
        };
        let mut requested = vec![false; workers.len()];
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            for (worker, requested) in workers.iter().zip(&mut requested) {
                if !*requested {
                    *requested = match worker.request_shutdown() {
                        Ok(()) | Err(WebRoomWorkerError::WorkerStopped) => true,
                        Err(WebRoomWorkerError::CommandQueueFull) => false,
                        Err(_) => true,
                    };
                }
            }
            if workers.iter().all(|worker| {
                matches!(
                    worker.snapshot().phase,
                    WebRoomWorkerPhase::Stopped | WebRoomWorkerPhase::Failed
                )
            }) || tokio::time::Instant::now() >= deadline
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        for worker in workers {
            worker.join_if_stopped();
        }
    }

    fn push_state_changed(&self, revision: u64) {
        let _ = self.events.send(WebLobbyPush::StateChanged { revision });
    }
}

#[derive(Clone, Debug)]
struct WebGuestProfile {
    user_id: AuthenticatedUserId,
    nickname: String,
    display_name: String,
    expires_at_unix_seconds: u64,
    active_control_connections: u16,
    present_until_unix_seconds: u64,
    chat_times: VecDeque<u64>,
    recent_messages: VecDeque<(u64, String)>,
}

impl WebGuestProfile {
    fn touch(&mut self, now: u64, grace_seconds: u64) {
        self.present_until_unix_seconds = now.saturating_add(grace_seconds);
    }

    fn is_present(&self, now: u64) -> bool {
        self.expires_at_unix_seconds >= now
            && (self.active_control_connections > 0 || self.present_until_unix_seconds >= now)
    }

    fn admit_chat(&mut self, text: &str, now: u64) -> Result<(), WebRoomError> {
        while self
            .chat_times
            .front()
            .is_some_and(|sent| now.saturating_sub(*sent) >= CHAT_WINDOW_SECONDS)
        {
            self.chat_times.pop_front();
        }
        while self
            .recent_messages
            .front()
            .is_some_and(|(sent, _)| now.saturating_sub(*sent) >= CHAT_DUPLICATE_SECONDS)
        {
            self.recent_messages.pop_front();
        }
        let burst = self
            .chat_times
            .iter()
            .filter(|sent| now.saturating_sub(**sent) < CHAT_BURST_SECONDS)
            .count();
        if burst >= CHAT_BURST_MESSAGES
            || self.chat_times.len() >= CHAT_WINDOW_MESSAGES
            || self.recent_messages.iter().any(|(_, prior)| prior == text)
        {
            return Err(WebRoomError::ChatRateLimited);
        }
        self.chat_times.push_back(now);
        self.recent_messages.push_back((now, text.to_owned()));
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WebRoomMember {
    guest_id: GuestId,
    user_id: AuthenticatedUserId,
    peer_id: PeerId,
    is_host: bool,
    character: WebCharacter,
    ready: bool,
    joined_order: u64,
}

enum RoomLifecycle {
    Open,
    Starting {
        startup_id: [u8; 16],
    },
    Active {
        manifest: Box<MatchManifest>,
        worker: WebRoomWorkerHandle,
        result_since_unix_seconds: Option<u64>,
        returning_since_unix_seconds: Option<u64>,
        result_acks: HashSet<GuestId>,
    },
}

struct WebPrivateRoom {
    room_code: PrivateRoomCode,
    options: WebPrivateRoomOptions,
    revision: u64,
    match_epoch: u64,
    members: Vec<WebRoomMember>,
    banned_guests: HashSet<GuestId>,
    room_chat: VecDeque<ChatMessageResponse>,
    next_peer_id: u64,
    next_joined_order: u64,
    lifecycle: RoomLifecycle,
    created_at_unix_seconds: u64,
    last_activity_unix_seconds: u64,
}

impl WebPrivateRoom {
    fn require_member(&self, guest: GuestSessionClaims) -> Result<&WebRoomMember, WebRoomError> {
        self.members
            .iter()
            .find(|member| member.guest_id == guest.guest_id && member.user_id == guest.user_id)
            .ok_or(WebRoomError::GuestNotMember)
    }

    fn require_member_mut(
        &mut self,
        guest: GuestSessionClaims,
    ) -> Result<&mut WebRoomMember, WebRoomError> {
        self.members
            .iter_mut()
            .find(|member| member.guest_id == guest.guest_id && member.user_id == guest.user_id)
            .ok_or(WebRoomError::GuestNotMember)
    }

    fn require_open_revision(&self, expected_revision: u64) -> Result<(), WebRoomError> {
        if !matches!(self.lifecycle, RoomLifecycle::Open) {
            return Err(WebRoomError::RoomNotOpen);
        }
        if self.revision != expected_revision {
            return Err(WebRoomError::RevisionConflict);
        }
        Ok(())
    }

    fn clear_ready(&mut self) {
        for member in &mut self.members {
            member.ready = false;
        }
    }

    fn bump_revision(&mut self) {
        self.revision = self.revision.saturating_add(1).max(1);
    }

    fn migrate_host(&mut self) {
        for member in &mut self.members {
            member.is_host = false;
        }
        if let Some(member) = self
            .members
            .iter_mut()
            .min_by_key(|member| member.joined_order)
        {
            member.is_host = true;
        }
    }

    fn state_and_snapshot(
        &self,
    ) -> (
        WebPrivateRoomState,
        Option<MatchManifest>,
        Option<WebRoomWorkerSnapshot>,
    ) {
        match &self.lifecycle {
            RoomLifecycle::Open => (WebPrivateRoomState::Open, None, None),
            RoomLifecycle::Starting { .. } => (WebPrivateRoomState::Starting, None, None),
            RoomLifecycle::Active {
                manifest,
                worker,
                result_since_unix_seconds,
                returning_since_unix_seconds,
                ..
            } => {
                let snapshot = worker.snapshot();
                let state = if returning_since_unix_seconds.is_some() {
                    WebPrivateRoomState::Returning
                } else if result_since_unix_seconds.is_some()
                    || snapshot.phase == WebRoomWorkerPhase::Finished
                {
                    WebPrivateRoomState::Results
                } else if snapshot.phase == WebRoomWorkerPhase::Failed {
                    WebPrivateRoomState::Failed
                } else {
                    WebPrivateRoomState::Active
                };
                (state, Some(**manifest), Some(snapshot))
            }
        }
    }

    fn view(
        &self,
        viewer: GuestId,
        guests: &HashMap<GuestId, WebGuestProfile>,
        now: u64,
    ) -> WebPrivateRoomView {
        let (state, manifest, worker_snapshot) = self.state_and_snapshot();
        let connected_mask = worker_snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.connected_peer_mask);
        let members = self
            .members
            .iter()
            .enumerate()
            .map(|(index, member)| WebRoomMemberView {
                peer_id: member.peer_id,
                display_name: guests.get(&member.guest_id).map_or_else(
                    || "Guest".to_owned(),
                    |profile| profile.display_name.clone(),
                ),
                is_host: member.is_host,
                is_self: member.guest_id == viewer,
                present: guests
                    .get(&member.guest_id)
                    .is_some_and(|profile| profile.is_present(now)),
                gameplay_connected: connected_mask & (1_u8 << index) != 0,
                character: member.character,
                ready: member.ready,
            })
            .collect();
        let result = worker_snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.confirmed_result)
            .map(result_view);
        WebPrivateRoomView {
            room_code: self.room_code,
            revision: self.revision,
            match_epoch: self.match_epoch,
            visibility: self.options.visibility,
            state,
            maximum_players: self.options.maximum_players,
            member_count: self.members.len() as u8,
            arena_index: self.options.arena_index,
            rule_index: self.options.rule_index,
            members,
            room_chat: self.room_chat.iter().cloned().collect(),
            manifest,
            result,
            worker: worker_snapshot,
        }
    }

    fn public_view(&self, guests: &HashMap<GuestId, WebGuestProfile>) -> Option<WebPublicRoomView> {
        if self.options.visibility != RoomVisibility::Public
            || !matches!(self.lifecycle, RoomLifecycle::Open)
        {
            return None;
        }
        let host = self.members.iter().find(|member| member.is_host)?;
        Some(WebPublicRoomView {
            room_code: self.room_code,
            revision: self.revision,
            host_display_name: guests.get(&host.guest_id).map_or_else(
                || "Guest".to_owned(),
                |profile| profile.display_name.clone(),
            ),
            state: WebPrivateRoomState::Open,
            member_count: self.members.len() as u8,
            maximum_players: self.options.maximum_players,
            arena_index: self.options.arena_index,
            rule_index: self.options.rule_index,
        })
    }
}

struct PreparedRoomStart {
    room_id: PrivateRoomId,
    startup_id: [u8; 16],
    match_config: HeadlessMatchConfig,
    manifest: MatchManifest,
    roster: Vec<AuthenticatedPeer>,
}

#[derive(Clone, Debug)]
struct ChatReportRecord {
    reporter: GuestId,
    target: GuestId,
    message_id: Option<u64>,
    category: ChatReportCategory,
    reported_at_unix_seconds: u64,
}

struct WebRoomRegistry {
    rooms: HashMap<PrivateRoomId, WebPrivateRoom>,
    codes: HashMap<PrivateRoomCode, PrivateRoomId>,
    guests: HashMap<GuestId, WebGuestProfile>,
    global_chat: VecDeque<ChatMessageResponse>,
    reports: VecDeque<ChatReportRecord>,
    next_chat_message_id: u64,
    lobby_revision: u64,
    replay: TicketReplayGuard,
}

impl WebRoomRegistry {
    fn new(replay: TicketReplayGuard) -> Self {
        Self {
            rooms: HashMap::new(),
            codes: HashMap::new(),
            guests: HashMap::new(),
            global_chat: VecDeque::new(),
            reports: VecDeque::new(),
            next_chat_message_id: 1,
            lobby_revision: 1,
            replay,
        }
    }

    fn bump_lobby_revision(&mut self) -> u64 {
        self.lobby_revision = self.lobby_revision.saturating_add(1).max(1);
        self.lobby_revision
    }

    fn require_guest_mut(
        &mut self,
        claims: GuestSessionClaims,
    ) -> Result<&mut WebGuestProfile, WebRoomError> {
        self.guests
            .get_mut(&claims.guest_id)
            .filter(|profile| profile.user_id == claims.user_id)
            .ok_or(WebRoomError::GuestNotRegistered)
    }

    fn touch_guest(
        &mut self,
        claims: GuestSessionClaims,
        now: u64,
        grace_seconds: u64,
    ) -> Result<(), WebRoomError> {
        self.require_guest_mut(claims)?.touch(now, grace_seconds);
        Ok(())
    }

    fn room_for_guest(&self, guest_id: GuestId) -> Option<PrivateRoomId> {
        self.rooms.iter().find_map(|(room_id, room)| {
            room.members
                .iter()
                .any(|member| member.guest_id == guest_id)
                .then_some(*room_id)
        })
    }

    fn room_id(&self, room_code: PrivateRoomCode) -> Result<PrivateRoomId, WebRoomError> {
        self.codes
            .get(&room_code)
            .copied()
            .ok_or(WebRoomError::RoomNotFound)
    }

    fn lobby_snapshot(&self, viewer: GuestId, now: u64) -> WebLobbySnapshotView {
        let mut public_rooms: Vec<_> = self
            .rooms
            .values()
            .filter(|room| !room.banned_guests.contains(&viewer))
            .filter_map(|room| room.public_view(&self.guests))
            .collect();
        public_rooms.sort_by_key(|room| room.room_code.canonical());
        let active_room = self
            .room_for_guest(viewer)
            .and_then(|room_id| self.rooms.get(&room_id))
            .map(|room| room.view(viewer, &self.guests, now));
        WebLobbySnapshotView {
            revision: self.lobby_revision,
            online_guests: self
                .guests
                .values()
                .filter(|profile| profile.is_present(now))
                .count()
                .try_into()
                .unwrap_or(u32::MAX),
            public_rooms,
            global_chat: self.global_chat.iter().cloned().collect(),
            active_room,
        }
    }

    fn prepare_start(
        &mut self,
        room_code: PrivateRoomCode,
        claims: GuestSessionClaims,
        expected_revision: u64,
        now: u64,
    ) -> Result<PreparedRoomStart, WebRoomError> {
        let room_id = self.room_id(room_code)?;
        let room = self
            .rooms
            .get_mut(&room_id)
            .ok_or(WebRoomError::RoomNotFound)?;
        room.require_open_revision(expected_revision)?;
        if !room.require_member(claims)?.is_host {
            return Err(WebRoomError::HostOnly);
        }
        if room.members.len() < usize::from(MIN_ROOM_PLAYERS) {
            return Err(WebRoomError::TooFewPlayers);
        }
        if !room.members.iter().all(|member| {
            member.ready
                && self
                    .guests
                    .get(&member.guest_id)
                    .is_some_and(|profile| profile.is_present(now))
        }) {
            return Err(WebRoomError::MembersNotReady);
        }

        let match_id = random_match_id()?;
        let mut setup = LocalSetup {
            arena_index: room.options.arena_index,
            rule_index: room.options.rule_index,
            replay_seed: random_u64()?,
            ..LocalSetup::default()
        };
        for slot in &mut setup.slots {
            slot.participant = ParticipantKind::Closed;
            slot.input = LocalInputAssignment::Unassigned;
        }
        let mut human_owners = [None; MAX_FIGHTERS];
        let mut roster = Vec::with_capacity(room.members.len());
        for (index, member) in room.members.iter().copied().enumerate() {
            setup.slots[index].participant = ParticipantKind::Human;
            setup.slots[index].character = character_kind(member.character);
            human_owners[index] = Some(member.peer_id);
            roster.push(AuthenticatedPeer {
                peer_id: member.peer_id,
                user_id: member.user_id,
            });
        }
        let options = MatchBuildOptions {
            match_id,
            authority: AuthorityKind::Dedicated,
            trusted_results: false,
            human_owners,
            agreed_start_tick: DEFAULT_AGREED_START_TICK,
            input_delay_ticks: DEFAULT_INPUT_DELAY_TICKS,
            rollback_limit_ticks: DEFAULT_ROLLBACK_LIMIT_TICKS,
            snapshot_history_ticks: DEFAULT_SNAPSHOT_HISTORY_TICKS,
        };
        let match_config = build_headless_match_config(&setup, options).map_err(|error| {
            WebRoomError::Worker(WebRoomWorkerError::Authority(format!("{error:?}")))
        })?;
        let manifest = match_config.manifest;
        let startup_id = random_nonzero_128()?;
        room.lifecycle = RoomLifecycle::Starting { startup_id };
        room.bump_revision();
        room.last_activity_unix_seconds = now;
        self.bump_lobby_revision();
        Ok(PreparedRoomStart {
            room_id,
            startup_id,
            match_config,
            manifest,
            roster,
        })
    }

    fn cancel_start(&mut self, room_id: PrivateRoomId, startup_id: [u8; 16]) -> Option<u64> {
        let room = self.rooms.get_mut(&room_id)?;
        if matches!(room.lifecycle, RoomLifecycle::Starting { startup_id: active } if active == startup_id)
        {
            room.lifecycle = RoomLifecycle::Open;
            room.clear_ready();
            room.bump_revision();
            Some(self.bump_lobby_revision())
        } else {
            None
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn install_worker(
        &mut self,
        room_id: PrivateRoomId,
        startup_id: [u8; 16],
        manifest: MatchManifest,
        worker: WebRoomWorkerHandle,
        now: u64,
        viewer: GuestId,
    ) -> Result<WebPrivateRoomView, WebRoomError> {
        {
            let room = self
                .rooms
                .get_mut(&room_id)
                .ok_or(WebRoomError::RoomNotFound)?;
            if !matches!(room.lifecycle, RoomLifecycle::Starting { startup_id: active } if active == startup_id)
            {
                return Err(WebRoomError::RoomNoLongerStarting);
            }
            room.lifecycle = RoomLifecycle::Active {
                manifest: Box::new(manifest),
                worker,
                result_since_unix_seconds: None,
                returning_since_unix_seconds: None,
                result_acks: HashSet::new(),
            };
            room.match_epoch = room.match_epoch.saturating_add(1);
            room.bump_revision();
            room.last_activity_unix_seconds = now;
        }
        self.bump_lobby_revision();
        Ok(self.rooms[&room_id].view(viewer, &self.guests, now))
    }

    fn join_ticket_grant(
        &mut self,
        room_code: PrivateRoomCode,
        claims: GuestSessionClaims,
        mode: JoinTicketMode,
        now: u64,
    ) -> Result<(JoinTicketGrant, MatchManifest), WebRoomError> {
        let room_id = self.room_id(room_code)?;
        let room = self
            .rooms
            .get_mut(&room_id)
            .ok_or(WebRoomError::RoomNotFound)?;
        let member_index = room
            .members
            .iter()
            .position(|member| {
                member.guest_id == claims.guest_id && member.user_id == claims.user_id
            })
            .ok_or(WebRoomError::GuestNotMember)?;
        let member = room.members[member_index];
        let RoomLifecycle::Active {
            manifest,
            worker,
            returning_since_unix_seconds,
            ..
        } = &room.lifecycle
        else {
            return if matches!(room.lifecycle, RoomLifecycle::Starting { .. }) {
                Err(WebRoomError::RoomStarting)
            } else {
                Err(WebRoomError::RoomNotOpen)
            };
        };
        if returning_since_unix_seconds.is_some() {
            return Err(WebRoomError::RoomNotOpen);
        }
        let snapshot = worker.snapshot();
        let connected = snapshot.connected_peer_mask & (1_u8 << member_index) != 0;
        match mode {
            JoinTicketMode::Initial => {
                if connected {
                    return Err(WebRoomError::AlreadyConnected);
                }
                if snapshot.simulation_tick != SimTick::ZERO {
                    return Err(WebRoomError::ReconnectRequired);
                }
            }
            JoinTicketMode::Reconnect {
                last_confirmed_tick,
            } => {
                if connected {
                    return Err(WebRoomError::AlreadyConnected);
                }
                if last_confirmed_tick > snapshot.simulation_tick {
                    return Err(WebRoomError::InvalidReconnectTick);
                }
            }
        }
        room.last_activity_unix_seconds = now;
        Ok((
            JoinTicketGrant {
                guest_id: claims.guest_id,
                user_id: claims.user_id,
                room_id,
                match_id: manifest.match_id,
                peer_id: member.peer_id,
                mode,
            },
            **manifest,
        ))
    }

    fn consume_admission(
        &mut self,
        claims: JoinTicketClaims,
        now: u64,
    ) -> Result<WebRoomWorkerHandle, WebRoomError> {
        let room = self
            .rooms
            .get_mut(&claims.grant.room_id)
            .ok_or(WebRoomError::RoomNotFound)?;
        let member_index = room
            .members
            .iter()
            .position(|member| {
                member.guest_id == claims.grant.guest_id
                    && member.user_id == claims.grant.user_id
                    && member.peer_id == claims.grant.peer_id
            })
            .ok_or(WebRoomError::TicketClaimsMismatch)?;
        let RoomLifecycle::Active {
            manifest,
            worker,
            returning_since_unix_seconds,
            ..
        } = &room.lifecycle
        else {
            return Err(WebRoomError::TicketClaimsMismatch);
        };
        if returning_since_unix_seconds.is_some() || manifest.match_id != claims.grant.match_id {
            return Err(WebRoomError::TicketClaimsMismatch);
        }
        let snapshot = worker.snapshot();
        self.replay.consume(claims, now)?;
        if snapshot.connected_peer_mask & (1_u8 << member_index) != 0 {
            return Err(WebRoomError::AlreadyConnected);
        }
        if let JoinTicketMode::Reconnect {
            last_confirmed_tick,
        } = claims.grant.mode
            && last_confirmed_tick > snapshot.simulation_tick
        {
            return Err(WebRoomError::InvalidReconnectTick);
        }
        room.last_activity_unix_seconds = now;
        Ok(worker.clone())
    }

    fn maintain(&mut self, now: u64, config: WebRoomServiceConfig) -> bool {
        let mut changed = false;
        while self.global_chat.front().is_some_and(|message| {
            now.saturating_sub(message.sent_at_unix_seconds) >= GLOBAL_CHAT_RETENTION_SECONDS
        }) {
            self.global_chat.pop_front();
            changed = true;
        }
        while self.reports.front().is_some_and(|report| {
            now.saturating_sub(report.reported_at_unix_seconds) >= REPORT_RETENTION_SECONDS
        }) {
            self.reports.pop_front();
        }

        let room_ids: Vec<_> = self.rooms.keys().copied().collect();
        let mut reopen = Vec::new();
        let mut remove = Vec::new();
        for room_id in room_ids {
            let Some(room) = self.rooms.get_mut(&room_id) else {
                continue;
            };
            if now.saturating_sub(room.created_at_unix_seconds)
                >= config.maximum_room_lifetime_seconds
            {
                remove.push(room_id);
                continue;
            }
            let mut bump_room_revision = false;
            match &mut room.lifecycle {
                RoomLifecycle::Open => {
                    let before = room.members.len();
                    let old_host = room
                        .members
                        .iter()
                        .find(|member| member.is_host)
                        .map(|member| member.guest_id);
                    room.members.retain(|member| {
                        self.guests
                            .get(&member.guest_id)
                            .is_some_and(|profile| profile.is_present(now))
                    });
                    if room.members.len() != before {
                        if old_host.is_some_and(|host| {
                            !room.members.iter().any(|member| member.guest_id == host)
                        }) {
                            room.migrate_host();
                        }
                        room.clear_ready();
                        bump_room_revision = true;
                        room.last_activity_unix_seconds = now;
                        changed = true;
                    }
                    if room.members.is_empty()
                        || now.saturating_sub(room.last_activity_unix_seconds)
                            >= config.open_room_ttl_seconds
                    {
                        remove.push(room_id);
                    }
                }
                RoomLifecycle::Starting { .. } => {}
                RoomLifecycle::Active {
                    worker,
                    result_since_unix_seconds,
                    returning_since_unix_seconds,
                    result_acks,
                    ..
                } => {
                    let snapshot = worker.snapshot();
                    if result_since_unix_seconds.is_none()
                        && snapshot.phase == WebRoomWorkerPhase::Finished
                    {
                        *result_since_unix_seconds = Some(now);
                        bump_room_revision = true;
                        changed = true;
                    }
                    let all_present_acked = result_since_unix_seconds.is_some()
                        && room
                            .members
                            .iter()
                            .filter(|member| {
                                self.guests
                                    .get(&member.guest_id)
                                    .is_some_and(|profile| profile.is_present(now))
                            })
                            .all(|member| result_acks.contains(&member.guest_id));
                    let return_due = result_since_unix_seconds.is_some_and(|since| {
                        all_present_acked
                            || now.saturating_sub(since) >= config.results_return_ceiling_seconds
                    });
                    let failed = snapshot.phase == WebRoomWorkerPhase::Failed;
                    if returning_since_unix_seconds.is_none() && (return_due || failed) {
                        match worker.request_shutdown() {
                            Ok(()) | Err(WebRoomWorkerError::WorkerStopped) => {
                                *returning_since_unix_seconds = Some(now);
                                bump_room_revision = true;
                                changed = true;
                            }
                            Err(WebRoomWorkerError::CommandQueueFull) => {}
                            Err(_) => {
                                *returning_since_unix_seconds = Some(now);
                                bump_room_revision = true;
                                changed = true;
                            }
                        }
                    }
                    if returning_since_unix_seconds.is_some()
                        && matches!(
                            snapshot.phase,
                            WebRoomWorkerPhase::Stopped | WebRoomWorkerPhase::Failed
                        )
                    {
                        reopen.push(room_id);
                    }
                }
            }
            if bump_room_revision {
                room.bump_revision();
            }
        }

        for room_id in reopen {
            let Some(room) = self.rooms.get_mut(&room_id) else {
                continue;
            };
            let old = std::mem::replace(&mut room.lifecycle, RoomLifecycle::Open);
            if let RoomLifecycle::Active { worker, .. } = old {
                worker.join_if_stopped();
            }
            room.clear_ready();
            room.bump_revision();
            room.last_activity_unix_seconds = now;
            changed = true;
        }
        remove.sort_unstable_by_key(|room_id| room_id.encoded());
        remove.dedup();
        for room_id in remove {
            if let Some(room) = self.rooms.remove(&room_id) {
                self.codes.remove(&room.room_code);
                if let RoomLifecycle::Active { worker, .. } = room.lifecycle {
                    let _ = worker.request_shutdown();
                }
                changed = true;
            }
        }
        if changed {
            self.bump_lobby_revision();
        }
        changed
    }

    fn message_exists(&self, message_id: u64, sender: GuestId) -> bool {
        self.global_chat
            .iter()
            .chain(self.rooms.values().flat_map(|room| room.room_chat.iter()))
            .any(|message| {
                message.message_id == message_id && message.sender_guest_id == sender.encoded()
            })
    }

    fn take_all_workers(&mut self) -> Vec<WebRoomWorkerHandle> {
        self.codes.clear();
        self.rooms
            .drain()
            .filter_map(|(_, room)| match room.lifecycle {
                RoomLifecycle::Active { worker, .. } => Some(worker),
                RoomLifecycle::Open | RoomLifecycle::Starting { .. } => None,
            })
            .collect()
    }
}

impl Drop for WebRoomRegistry {
    fn drop(&mut self) {
        for room in self.rooms.values() {
            if let RoomLifecycle::Active { worker, .. } = &room.lifecycle {
                let _ = worker.request_shutdown();
            }
        }
    }
}

fn normalize_nickname(value: &str) -> Result<String, WebRoomError> {
    let normalized: String = value.nfkc().collect();
    let mut collapsed = String::with_capacity(normalized.len());
    let mut prior_space = true;
    for character in normalized.chars() {
        if character.is_whitespace() {
            if !prior_space {
                collapsed.push(' ');
            }
            prior_space = true;
            continue;
        }
        if dangerous_text_character(character) {
            return Err(WebRoomError::InvalidNickname);
        }
        if !(character.is_alphanumeric() || matches!(character, '_' | '-' | '.')) {
            return Err(WebRoomError::InvalidNickname);
        }
        collapsed.push(character);
        prior_space = false;
    }
    let collapsed = collapsed.trim().to_owned();
    let graphemes = collapsed.graphemes(true).count();
    let reserved = ["system", "admin", "administrator", "moderator", "afc"];
    if !(MIN_NICKNAME_GRAPHEMES..=MAX_NICKNAME_GRAPHEMES).contains(&graphemes)
        || collapsed.len() > MAX_NICKNAME_BYTES
        || reserved.contains(&collapsed.to_lowercase().as_str())
    {
        return Err(WebRoomError::InvalidNickname);
    }
    Ok(collapsed)
}

fn normalize_chat(value: &str) -> Result<String, WebRoomError> {
    let normalized: String = value.nfc().collect();
    let mut collapsed = String::with_capacity(normalized.len());
    let mut prior_space = true;
    for character in normalized.chars() {
        if character.is_whitespace() {
            if !prior_space {
                collapsed.push(' ');
            }
            prior_space = true;
        } else {
            if dangerous_text_character(character) {
                return Err(WebRoomError::InvalidChatMessage);
            }
            collapsed.push(character);
            prior_space = false;
        }
    }
    let collapsed = collapsed.trim().to_owned();
    let graphemes = collapsed.graphemes(true).count();
    if graphemes == 0 || graphemes > MAX_CHAT_GRAPHEMES || collapsed.len() > MAX_CHAT_BYTES {
        return Err(WebRoomError::InvalidChatMessage);
    }
    Ok(collapsed)
}

fn dangerous_text_character(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{00ad}'
                | '\u{034f}'
                | '\u{061c}'
                | '\u{115f}'
                | '\u{1160}'
                | '\u{17b4}'
                | '\u{17b5}'
                | '\u{180e}'
                | '\u{200b}'..='\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{feff}'
        )
}

fn character_kind(character: WebCharacter) -> CharacterKind {
    match character {
        WebCharacter::Cat => CharacterKind::Cat,
        WebCharacter::Pig => CharacterKind::Pig,
        WebCharacter::Dog => CharacterKind::Dog,
        WebCharacter::Fox => CharacterKind::Fox,
        WebCharacter::Panda => CharacterKind::Panda,
        WebCharacter::Bee => CharacterKind::Bee,
        WebCharacter::Penguin => CharacterKind::Penguin,
        WebCharacter::Chick => CharacterKind::Chick,
    }
}

fn result_view(result: ResultIdentifier) -> WebRoomResultView {
    WebRoomResultView {
        match_id: match_id_hex(result.match_id),
        result_id: result.result_id.get(),
        final_tick: result.final_tick.get(),
        final_state_hash: result.final_state_hash.0,
    }
}

fn match_id_hex(match_id: MatchId) -> String {
    let mut encoded = String::with_capacity(32);
    for byte in match_id.as_bytes() {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

fn push_bounded<T>(queue: &mut VecDeque<T>, value: T, capacity: usize) {
    if queue.len() >= capacity {
        queue.pop_front();
    }
    queue.push_back(value);
}

fn unique_room_id(registry: &WebRoomRegistry) -> Result<PrivateRoomId, WebRoomError> {
    for _ in 0..16 {
        let room_id = PrivateRoomId::random()?;
        if !registry.rooms.contains_key(&room_id) {
            return Ok(room_id);
        }
    }
    Err(WebRoomError::RoomCodeExhausted)
}

fn unique_room_code(registry: &WebRoomRegistry) -> Result<PrivateRoomCode, WebRoomError> {
    for _ in 0..16 {
        let code = PrivateRoomCode::random()?;
        if !registry.codes.contains_key(&code) {
            return Ok(code);
        }
    }
    Err(WebRoomError::RoomCodeExhausted)
}

fn random_match_id() -> Result<MatchId, WebRoomError> {
    MatchId::new(random_nonzero_128()?).map_err(|_| WebRoomError::RandomnessUnavailable)
}

fn random_nonzero_128() -> Result<[u8; 16], WebRoomError> {
    for _ in 0..4 {
        let mut bytes = [0_u8; 16];
        getrandom::fill(&mut bytes).map_err(|_| WebRoomError::RandomnessUnavailable)?;
        if bytes.iter().any(|byte| *byte != 0) {
            return Ok(bytes);
        }
    }
    Err(WebRoomError::RandomnessUnavailable)
}

fn random_u64() -> Result<u64, WebRoomError> {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes).map_err(|_| WebRoomError::RandomnessUnavailable)?;
    Ok(u64::from_be_bytes(bytes))
}

fn unix_now_fallback() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network_protocol::SeatOwner;
    use crate::web_endpoint_adapters::{ServerDatagramBridge, WebEndpointConfig};
    use crate::web_identity::{WebTokenLifetimes, WebTokenSigningKey};

    fn service() -> WebRoomService {
        let key = WebTokenSigningKey::new(7, [0x5a; 32]).unwrap();
        let keyring = WebTokenKeyring::new(key, None, WebTokenLifetimes::default()).unwrap();
        WebRoomService::new(keyring, WebRoomServiceConfig::default()).unwrap()
    }

    fn ready_pair(
        service: &WebRoomService,
        now: u64,
    ) -> (
        IssuedWebGuestSession,
        IssuedWebGuestSession,
        WebPrivateRoomView,
    ) {
        let host = service
            .issue_named_guest_session("호스트 One", now)
            .unwrap();
        let guest = service.issue_named_guest_session("Guest Two", now).unwrap();
        let created = service
            .create_room(
                &host.issued.token,
                WebPrivateRoomOptions {
                    maximum_players: 2,
                    visibility: RoomVisibility::Public,
                    ..WebPrivateRoomOptions::default()
                },
                now + 1,
            )
            .unwrap();
        let code = created.room_code.to_string();
        let joined = service
            .join_private_room(&guest.issued.token, &code, now + 2)
            .unwrap();
        let host_ready = service
            .set_ready(&host.issued.token, &code, joined.revision, true, now + 3)
            .unwrap();
        let guest_ready = service
            .set_ready(
                &guest.issued.token,
                &code,
                host_ready.revision,
                true,
                now + 4,
            )
            .unwrap();
        (host, guest, guest_ready)
    }

    #[test]
    fn private_room_codes_are_sixty_bit_grouped_and_ambiguity_tolerant() {
        let canonical = PrivateRoomCode::from_str("01AB-CDEF-GHJK").unwrap();
        assert_eq!(canonical.to_string(), "01AB-CDEF-GHJK");
        assert_eq!(
            PrivateRoomCode::from_str("olab-cdef-ghjk").unwrap(),
            canonical
        );
        assert_eq!(canonical.canonical(), "01ABCDEFGHJK");
    }

    #[test]
    fn international_names_and_chat_are_normalized_and_bounded() {
        assert_eq!(
            normalize_nickname("  플레이어   하나  ").unwrap(),
            "플레이어 하나"
        );
        assert_eq!(normalize_chat("안녕\n  world 👋").unwrap(), "안녕 world 👋");
        assert_eq!(
            normalize_nickname("System"),
            Err(WebRoomError::InvalidNickname)
        );
        assert_eq!(
            normalize_chat("bad\u{202e}text"),
            Err(WebRoomError::InvalidChatMessage)
        );
    }

    #[test]
    fn readiness_settings_character_and_public_directory_are_revisioned() {
        let service = service();
        let (host, guest, ready) = ready_pair(&service, 20_000);
        assert!(ready.members.iter().all(|member| member.ready));
        let code = ready.room_code.to_string();
        let changed = service
            .select_character(
                &guest.issued.token,
                &code,
                ready.revision,
                WebCharacter::Chick,
                20_005,
            )
            .unwrap();
        assert!(
            !changed
                .members
                .iter()
                .find(|member| member.is_self)
                .unwrap()
                .ready
        );
        let changed = service
            .update_room_settings(&host.issued.token, &code, changed.revision, 1, 2, 20_006)
            .unwrap();
        assert!(changed.members.iter().all(|member| !member.ready));
        let lobby = service.lobby_snapshot(&guest.issued.token, 20_007).unwrap();
        assert_eq!(lobby.public_rooms.len(), 1);
        assert_eq!(lobby.public_rooms[0].arena_index, 1);
    }

    #[test]
    fn host_kick_bans_rejoin_and_explicit_leave_migrates_host() {
        let service = service();
        let host = service
            .issue_named_guest_session("Host One", 30_000)
            .unwrap();
        let second = service
            .issue_named_guest_session("Second One", 30_000)
            .unwrap();
        let created = service
            .create_room(&host.issued.token, WebPrivateRoomOptions::default(), 30_001)
            .unwrap();
        let code = created.room_code.to_string();
        let joined = service
            .join_private_room(&second.issued.token, &code, 30_002)
            .unwrap();
        let second_peer = joined.self_member().peer_id;
        service
            .kick_and_ban_member(
                &host.issued.token,
                &code,
                joined.revision,
                second_peer,
                30_003,
            )
            .unwrap();
        assert_eq!(
            service.join_private_room(&second.issued.token, &code, 30_004),
            Err(WebRoomError::GuestRoomBanned)
        );
    }

    #[tokio::test]
    async fn ready_room_builds_real_hub_and_consumes_each_ticket_once() {
        let service = service();
        let (host, guest, ready) = ready_pair(&service, 40_000);
        let code = ready.room_code.to_string();
        let active = service
            .start_private_room(&host.issued.token, &code, ready.revision, 40_005)
            .await
            .unwrap();
        assert_eq!(active.state, WebPrivateRoomState::Active);
        let manifest = active.manifest.unwrap();
        assert_eq!(manifest.authority, AuthorityKind::Dedicated);
        assert!(!manifest.trusted_results);
        assert_eq!(manifest.ownership.len(), 2);
        for assignment in manifest.ownership.as_slice() {
            assert!(matches!(assignment.owner, SeatOwner::Peer(_)));
        }

        let host_ticket = service
            .issue_join_ticket(&host.issued.token, &code, JoinTicketMode::Initial, 40_006)
            .unwrap();
        let guest_ticket = service
            .issue_join_ticket(&guest.issued.token, &code, JoinTicketMode::Initial, 40_006)
            .unwrap();
        let (host_endpoint, _host_bridge) =
            ServerDatagramBridge::pair(WebEndpointConfig::default()).unwrap();
        let (guest_endpoint, _guest_bridge) =
            ServerDatagramBridge::pair(WebEndpointConfig::default()).unwrap();
        service
            .admit_join_ticket(&host_ticket.ticket, host_endpoint, 40_007)
            .await
            .unwrap();
        service
            .admit_join_ticket(&guest_ticket.ticket, guest_endpoint, 40_007)
            .await
            .unwrap();
        let (replay_endpoint, _replay_bridge) =
            ServerDatagramBridge::pair(WebEndpointConfig::default()).unwrap();
        assert_eq!(
            service
                .admit_join_ticket(&host_ticket.ticket, replay_endpoint, 40_008)
                .await,
            Err(WebRoomError::Identity(WebIdentityError::TicketReplayed))
        );
        service.shutdown_all().await;
    }

    trait SelfMember {
        fn self_member(&self) -> &WebRoomMemberView;
    }

    impl SelfMember for WebPrivateRoomView {
        fn self_member(&self) -> &WebRoomMemberView {
            self.members.iter().find(|member| member.is_self).unwrap()
        }
    }
}
