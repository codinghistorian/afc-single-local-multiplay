//! Bounded private-room lifecycle and fixed-tick hosted authority workers.

mod worker;

use core::fmt;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crate::arena_defs::arena_definitions;
use crate::components::{LocalInputAssignment, ParticipantKind};
use crate::game_state::{LocalSetup, RULE_PRESETS};
use crate::headless::HeadlessMatchConfig;
use crate::match_config::{
    DEFAULT_INPUT_DELAY_TICKS, DEFAULT_ROLLBACK_LIMIT_TICKS, DEFAULT_SNAPSHOT_HISTORY_TICKS,
    MatchBuildOptions, build_headless_match_config,
};
use crate::network_protocol::{
    AuthorityKind, MAX_FIGHTERS, MatchId, MatchManifest, PeerId, ReconnectClaim, SimTick,
};
use crate::reconnect::{AuthenticatedPeer, AuthenticatedUserId};
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

pub const DEFAULT_MAX_PRIVATE_ROOMS: usize = 512;
pub const DEFAULT_OPEN_ROOM_TTL_SECONDS: u64 = 15 * 60;
pub const DEFAULT_MAX_ROOM_LIFETIME_SECONDS: u64 = 4 * 60 * 60;
pub const DEFAULT_TERMINAL_ROOM_RETENTION_SECONDS: u64 = 5 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WebRoomServiceConfig {
    pub maximum_rooms: usize,
    pub open_room_ttl_seconds: u64,
    pub maximum_room_lifetime_seconds: u64,
    pub terminal_room_retention_seconds: u64,
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
    pub arena_index: usize,
    pub rule_index: usize,
}

impl Default for WebPrivateRoomOptions {
    fn default() -> Self {
        Self {
            maximum_players: 4,
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
    Finished,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WebRoomMemberView {
    pub peer_id: PeerId,
    pub is_host: bool,
    pub is_self: bool,
    pub connected: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebPrivateRoomView {
    pub room_code: PrivateRoomCode,
    pub state: WebPrivateRoomState,
    pub maximum_players: u8,
    pub member_count: u8,
    pub members: Vec<WebRoomMemberView>,
    pub manifest: Option<MatchManifest>,
    pub worker: Option<WebRoomWorkerSnapshot>,
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
    InvalidRoomOptions,
    InvalidRoomCode,
    RandomnessUnavailable,
    RegistryFull,
    RoomCodeExhausted,
    RoomNotFound,
    RoomNotOpen,
    RoomFull,
    GuestIdentityConflict,
    GuestNotMember,
    HostOnly,
    TooFewPlayers,
    RoomStarting,
    RoomAlreadyActive,
    RoomNoLongerStarting,
    AlreadyConnected,
    ReconnectRequired,
    InvalidReconnectTick,
    TicketClaimsMismatch,
    Identity(WebIdentityError),
    Worker(WebRoomWorkerError),
    WorkerTaskCancelled,
}

impl fmt::Display for WebRoomError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "hosted private-room operation failed: {self:?}")
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
}

impl WebRoomService {
    pub fn new(
        keyring: WebTokenKeyring,
        config: WebRoomServiceConfig,
    ) -> Result<Self, WebRoomError> {
        config.validate()?;
        let replay = TicketReplayGuard::new(config.replay_cache_entries)?;
        Ok(Self {
            keyring: Arc::new(keyring),
            config,
            registry: Arc::new(Mutex::new(WebRoomRegistry::new(replay))),
        })
    }

    pub fn issue_guest_session(
        &self,
        now_unix_seconds: u64,
    ) -> Result<IssuedGuestSession, WebRoomError> {
        self.keyring
            .issue_guest_session(now_unix_seconds)
            .map_err(Into::into)
    }

    pub fn create_private_room(
        &self,
        guest_session: &str,
        options: WebPrivateRoomOptions,
        now_unix_seconds: u64,
    ) -> Result<WebPrivateRoomView, WebRoomError> {
        options.validate()?;
        let guest = self
            .keyring
            .verify_guest_session(guest_session, now_unix_seconds)?;
        let mut registry = lock_recover(&self.registry);
        registry.maintain(now_unix_seconds, self.config);
        if registry.rooms.len() >= self.config.maximum_rooms {
            return Err(WebRoomError::RegistryFull);
        }
        let room_id = unique_room_id(&registry)?;
        let room_code = unique_room_code(&registry)?;
        let host = WebRoomMember {
            guest_id: guest.guest_id,
            user_id: guest.user_id,
            peer_id: PeerId::new(1).expect("the first room peer id is non-zero"),
            is_host: true,
        };
        let room = WebPrivateRoom {
            room_code,
            options,
            members: vec![host],
            next_peer_id: 2,
            lifecycle: RoomLifecycle::Open,
            created_at_unix_seconds: now_unix_seconds,
            last_activity_unix_seconds: now_unix_seconds,
            terminal_since_unix_seconds: None,
        };
        registry.codes.insert(room_code, room_id);
        registry.rooms.insert(room_id, room);
        Ok(registry
            .rooms
            .get(&room_id)
            .expect("the room was just inserted")
            .view(guest.guest_id))
    }

    pub fn join_private_room(
        &self,
        guest_session: &str,
        room_code: &str,
        now_unix_seconds: u64,
    ) -> Result<WebPrivateRoomView, WebRoomError> {
        let guest = self
            .keyring
            .verify_guest_session(guest_session, now_unix_seconds)?;
        let room_code = PrivateRoomCode::from_str(room_code)?;
        let mut registry = lock_recover(&self.registry);
        registry.maintain(now_unix_seconds, self.config);
        let room_id = *registry
            .codes
            .get(&room_code)
            .ok_or(WebRoomError::RoomNotFound)?;
        let room = registry
            .rooms
            .get_mut(&room_id)
            .ok_or(WebRoomError::RoomNotFound)?;
        if let Some(existing) = room
            .members
            .iter()
            .find(|member| member.guest_id == guest.guest_id)
        {
            if existing.user_id != guest.user_id {
                return Err(WebRoomError::GuestIdentityConflict);
            }
            room.last_activity_unix_seconds = now_unix_seconds;
            return Ok(room.view(guest.guest_id));
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
            .any(|member| member.user_id == guest.user_id)
        {
            return Err(WebRoomError::GuestIdentityConflict);
        }
        let peer_id =
            PeerId::new(room.next_peer_id).map_err(|_| WebRoomError::GuestIdentityConflict)?;
        room.next_peer_id = room
            .next_peer_id
            .checked_add(1)
            .ok_or(WebRoomError::GuestIdentityConflict)?;
        room.members.push(WebRoomMember {
            guest_id: guest.guest_id,
            user_id: guest.user_id,
            peer_id,
            is_host: false,
        });
        room.last_activity_unix_seconds = now_unix_seconds;
        Ok(room.view(guest.guest_id))
    }

    pub fn private_room_status(
        &self,
        guest_session: &str,
        room_code: &str,
        now_unix_seconds: u64,
    ) -> Result<WebPrivateRoomView, WebRoomError> {
        let guest = self
            .keyring
            .verify_guest_session(guest_session, now_unix_seconds)?;
        let room_code = PrivateRoomCode::from_str(room_code)?;
        let mut registry = lock_recover(&self.registry);
        registry.maintain(now_unix_seconds, self.config);
        let room_id = *registry
            .codes
            .get(&room_code)
            .ok_or(WebRoomError::RoomNotFound)?;
        let room = registry
            .rooms
            .get_mut(&room_id)
            .ok_or(WebRoomError::RoomNotFound)?;
        room.require_member(guest)?;
        room.last_activity_unix_seconds = now_unix_seconds;
        Ok(room.view(guest.guest_id))
    }

    pub async fn start_private_room(
        &self,
        guest_session: &str,
        room_code: &str,
        now_unix_seconds: u64,
    ) -> Result<WebPrivateRoomView, WebRoomError> {
        let guest = self
            .keyring
            .verify_guest_session(guest_session, now_unix_seconds)?;
        let room_code = PrivateRoomCode::from_str(room_code)?;
        let prepared = {
            let mut registry = lock_recover(&self.registry);
            registry.maintain(now_unix_seconds, self.config);
            registry.prepare_start(room_code, guest, now_unix_seconds)?
        };
        let worker_config = self.config.worker;
        let match_config = prepared.match_config.clone();
        let roster = prepared.roster.clone();
        let built = tokio::task::spawn_blocking(move || {
            WebRoomWorkerHandle::spawn(match_config, roster, worker_config)
        })
        .await
        .map_err(|_| WebRoomError::WorkerTaskCancelled)?;
        let worker = match built {
            Ok(worker) => worker,
            Err(error) => {
                lock_recover(&self.registry).cancel_start(prepared.room_id, prepared.startup_id);
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
            guest.guest_id,
        );
        if result.is_err() {
            let _ = worker.request_shutdown();
        }
        result
    }

    pub fn issue_join_ticket(
        &self,
        guest_session: &str,
        room_code: &str,
        mode: JoinTicketMode,
        now_unix_seconds: u64,
    ) -> Result<WebJoinTicketResponse, WebRoomError> {
        let guest = self
            .keyring
            .verify_guest_session(guest_session, now_unix_seconds)?;
        let room_code = PrivateRoomCode::from_str(room_code)?;
        let (grant, manifest) = {
            let mut registry = lock_recover(&self.registry);
            registry.maintain(now_unix_seconds, self.config);
            registry.join_ticket_grant(room_code, guest, mode, now_unix_seconds)?
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
        for worker in &workers {
            let _ = worker.shutdown().await;
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WebRoomMember {
    guest_id: GuestId,
    user_id: AuthenticatedUserId,
    peer_id: PeerId,
    is_host: bool,
}

enum RoomLifecycle {
    Open,
    Starting {
        startup_id: [u8; 16],
    },
    Active {
        manifest: Box<MatchManifest>,
        worker: WebRoomWorkerHandle,
    },
}

struct WebPrivateRoom {
    room_code: PrivateRoomCode,
    options: WebPrivateRoomOptions,
    members: Vec<WebRoomMember>,
    next_peer_id: u64,
    lifecycle: RoomLifecycle,
    created_at_unix_seconds: u64,
    last_activity_unix_seconds: u64,
    terminal_since_unix_seconds: Option<u64>,
}

impl WebPrivateRoom {
    fn require_member(&self, guest: GuestSessionClaims) -> Result<&WebRoomMember, WebRoomError> {
        self.members
            .iter()
            .find(|member| member.guest_id == guest.guest_id && member.user_id == guest.user_id)
            .ok_or(WebRoomError::GuestNotMember)
    }

    fn view(&self, viewer: GuestId) -> WebPrivateRoomView {
        let (state, manifest, worker_snapshot) = match &self.lifecycle {
            RoomLifecycle::Open => (WebPrivateRoomState::Open, None, None),
            RoomLifecycle::Starting { .. } => (WebPrivateRoomState::Starting, None, None),
            RoomLifecycle::Active {
                manifest, worker, ..
            } => {
                let snapshot = worker.snapshot();
                let state = match snapshot.phase {
                    WebRoomWorkerPhase::Finished | WebRoomWorkerPhase::Stopped => {
                        WebPrivateRoomState::Finished
                    }
                    WebRoomWorkerPhase::Failed => WebPrivateRoomState::Failed,
                    _ => WebPrivateRoomState::Active,
                };
                (state, Some(**manifest), Some(snapshot))
            }
        };
        let connected_mask = worker_snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.connected_peer_mask);
        let members = self
            .members
            .iter()
            .enumerate()
            .map(|(index, member)| WebRoomMemberView {
                peer_id: member.peer_id,
                is_host: member.is_host,
                is_self: member.guest_id == viewer,
                connected: connected_mask & (1_u8 << index) != 0,
            })
            .collect();
        WebPrivateRoomView {
            room_code: self.room_code,
            state,
            maximum_players: self.options.maximum_players,
            member_count: self.members.len() as u8,
            members,
            manifest,
            worker: worker_snapshot,
        }
    }
}

struct PreparedRoomStart {
    room_id: PrivateRoomId,
    startup_id: [u8; 16],
    match_config: HeadlessMatchConfig,
    manifest: MatchManifest,
    roster: Vec<AuthenticatedPeer>,
}

struct WebRoomRegistry {
    rooms: HashMap<PrivateRoomId, WebPrivateRoom>,
    codes: HashMap<PrivateRoomCode, PrivateRoomId>,
    replay: TicketReplayGuard,
}

impl WebRoomRegistry {
    fn new(replay: TicketReplayGuard) -> Self {
        Self {
            rooms: HashMap::new(),
            codes: HashMap::new(),
            replay,
        }
    }

    fn prepare_start(
        &mut self,
        room_code: PrivateRoomCode,
        guest: GuestSessionClaims,
        now: u64,
    ) -> Result<PreparedRoomStart, WebRoomError> {
        let room_id = *self
            .codes
            .get(&room_code)
            .ok_or(WebRoomError::RoomNotFound)?;
        let room = self
            .rooms
            .get_mut(&room_id)
            .ok_or(WebRoomError::RoomNotFound)?;
        let member = *room.require_member(guest)?;
        if !member.is_host {
            return Err(WebRoomError::HostOnly);
        }
        match room.lifecycle {
            RoomLifecycle::Open => {}
            RoomLifecycle::Starting { .. } => return Err(WebRoomError::RoomStarting),
            RoomLifecycle::Active { .. } => return Err(WebRoomError::RoomAlreadyActive),
        }
        if room.members.len() < usize::from(MIN_ROOM_PLAYERS) {
            return Err(WebRoomError::TooFewPlayers);
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
            human_owners[index] = Some(member.peer_id);
            roster.push(AuthenticatedPeer {
                peer_id: member.peer_id,
                user_id: member.user_id,
            });
        }
        let options = MatchBuildOptions {
            match_id,
            authority: AuthorityKind::Dedicated,
            // Guest-only private rooms are authoritative but intentionally not
            // eligible for durable ranked/reward claims.
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
        room.last_activity_unix_seconds = now;
        Ok(PreparedRoomStart {
            room_id,
            startup_id,
            match_config,
            manifest,
            roster,
        })
    }

    fn cancel_start(&mut self, room_id: PrivateRoomId, startup_id: [u8; 16]) {
        if let Some(room) = self.rooms.get_mut(&room_id)
            && matches!(room.lifecycle, RoomLifecycle::Starting { startup_id: active } if active == startup_id)
        {
            room.lifecycle = RoomLifecycle::Open;
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
        };
        room.last_activity_unix_seconds = now;
        Ok(room.view(viewer))
    }

    fn join_ticket_grant(
        &mut self,
        room_code: PrivateRoomCode,
        guest: GuestSessionClaims,
        mode: JoinTicketMode,
        now: u64,
    ) -> Result<(JoinTicketGrant, MatchManifest), WebRoomError> {
        let room_id = *self
            .codes
            .get(&room_code)
            .ok_or(WebRoomError::RoomNotFound)?;
        let room = self
            .rooms
            .get_mut(&room_id)
            .ok_or(WebRoomError::RoomNotFound)?;
        let member_index = room
            .members
            .iter()
            .position(|member| member.guest_id == guest.guest_id && member.user_id == guest.user_id)
            .ok_or(WebRoomError::GuestNotMember)?;
        let member = room.members[member_index];
        let RoomLifecycle::Active {
            manifest, worker, ..
        } = &room.lifecycle
        else {
            return match room.lifecycle {
                RoomLifecycle::Starting { .. } => Err(WebRoomError::RoomStarting),
                RoomLifecycle::Open => Err(WebRoomError::RoomNotOpen),
                RoomLifecycle::Active { .. } => unreachable!(),
            };
        };
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
                guest_id: guest.guest_id,
                user_id: guest.user_id,
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
            manifest, worker, ..
        } = &room.lifecycle
        else {
            return Err(WebRoomError::TicketClaimsMismatch);
        };
        if manifest.match_id != claims.grant.match_id {
            return Err(WebRoomError::TicketClaimsMismatch);
        }
        let snapshot = worker.snapshot();
        // Once the signed room/member/match scope is established, consume the
        // nonce before inspecting mutable connection state. A replay therefore
        // has one stable outcome and cannot become a connection-state oracle.
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

    fn maintain(&mut self, now: u64, config: WebRoomServiceConfig) {
        for room in self.rooms.values_mut() {
            if room.terminal_since_unix_seconds.is_none()
                && let RoomLifecycle::Active { worker, .. } = &room.lifecycle
                && matches!(
                    worker.snapshot().phase,
                    WebRoomWorkerPhase::Finished
                        | WebRoomWorkerPhase::Failed
                        | WebRoomWorkerPhase::Stopped
                )
            {
                room.terminal_since_unix_seconds = Some(now);
            }
        }
        let expired: Vec<_> = self
            .rooms
            .iter()
            .filter_map(|(room_id, room)| {
                let open_expired = matches!(room.lifecycle, RoomLifecycle::Open)
                    && now.saturating_sub(room.last_activity_unix_seconds)
                        >= config.open_room_ttl_seconds;
                let hard_expired = now.saturating_sub(room.created_at_unix_seconds)
                    >= config.maximum_room_lifetime_seconds;
                let terminal_expired = room.terminal_since_unix_seconds.is_some_and(|terminal| {
                    now.saturating_sub(terminal) >= config.terminal_room_retention_seconds
                });
                (open_expired || hard_expired || terminal_expired).then_some(*room_id)
            })
            .collect();
        for room_id in expired {
            if let Some(room) = self.rooms.remove(&room_id) {
                self.codes.remove(&room.room_code);
                if let RoomLifecycle::Active { worker, .. } = room.lifecycle {
                    let _ = worker.request_shutdown();
                }
            }
        }
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

    #[test]
    fn private_room_codes_are_sixty_bit_grouped_and_ambiguity_tolerant() {
        let canonical = PrivateRoomCode::from_str("01AB-CDEF-GHJK").unwrap();
        assert_eq!(canonical.to_string(), "01AB-CDEF-GHJK");
        assert_eq!(
            PrivateRoomCode::from_str("olab-cdef-ghjk").unwrap(),
            canonical
        );
        assert_eq!(canonical.canonical(), "01ABCDEFGHJK");
        assert_eq!(
            PrivateRoomCode::from_str("TOO-SHORT"),
            Err(WebRoomError::InvalidRoomCode)
        );
    }

    #[tokio::test]
    async fn sealed_room_builds_real_hub_and_consumes_each_ticket_once() {
        let service = service();
        let host = service.issue_guest_session(10_000).unwrap();
        let guest = service.issue_guest_session(10_000).unwrap();
        let created = service
            .create_private_room(
                &host.token,
                WebPrivateRoomOptions {
                    maximum_players: 2,
                    ..WebPrivateRoomOptions::default()
                },
                10_001,
            )
            .unwrap();
        let code = created.room_code.to_string();
        let joined = service
            .join_private_room(&guest.token, &code, 10_002)
            .unwrap();
        assert_eq!(joined.member_count, 2);

        let active = service
            .start_private_room(&host.token, &code, 10_003)
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
            .issue_join_ticket(&host.token, &code, JoinTicketMode::Initial, 10_004)
            .unwrap();
        let guest_ticket = service
            .issue_join_ticket(&guest.token, &code, JoinTicketMode::Initial, 10_004)
            .unwrap();
        let (host_endpoint, _host_bridge) =
            ServerDatagramBridge::pair(WebEndpointConfig::default()).unwrap();
        let (guest_endpoint, _guest_bridge) =
            ServerDatagramBridge::pair(WebEndpointConfig::default()).unwrap();
        service
            .admit_join_ticket(&host_ticket.ticket, host_endpoint, 10_005)
            .await
            .unwrap();
        service
            .admit_join_ticket(&guest_ticket.ticket, guest_endpoint, 10_005)
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(40)).await;
        let status = service
            .private_room_status(&host.token, &code, 10_006)
            .unwrap();
        assert_eq!(status.worker.unwrap().connected_peers, 2);

        let (replay_endpoint, _replay_bridge) =
            ServerDatagramBridge::pair(WebEndpointConfig::default()).unwrap();
        assert_eq!(
            service
                .admit_join_ticket(&host_ticket.ticket, replay_endpoint, 10_006)
                .await,
            Err(WebRoomError::Identity(WebIdentityError::TicketReplayed))
        );
        service.shutdown_all().await;
    }

    #[test]
    fn room_membership_is_bounded_idempotent_and_host_sealed() {
        let service = service();
        let host = service.issue_guest_session(20_000).unwrap();
        let second = service.issue_guest_session(20_000).unwrap();
        let third = service.issue_guest_session(20_000).unwrap();
        let created = service
            .create_private_room(
                &host.token,
                WebPrivateRoomOptions {
                    maximum_players: 2,
                    ..WebPrivateRoomOptions::default()
                },
                20_001,
            )
            .unwrap();
        let code = created.room_code.to_string();
        let first_join = service
            .join_private_room(&second.token, &code, 20_002)
            .unwrap();
        let retry = service
            .join_private_room(&second.token, &code, 20_003)
            .unwrap();
        assert_eq!(first_join.members, retry.members);
        assert_eq!(
            service.join_private_room(&third.token, &code, 20_004),
            Err(WebRoomError::RoomFull)
        );
    }
}
