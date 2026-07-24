//! Production, transport-independent authority-side peer orchestration.
//!
//! Unlike the acceptance-only multi-peer network lab, this type owns only the
//! authority half of each connection.  It combines bounded per-peer runtimes
//! with the one canonical [`AuthorityMatch`], and is suitable for a listen
//! worker or a dedicated-server worker.  A platform adapter authenticates a
//! stable user before attaching an endpoint; Steam types never cross this
//! boundary.

use crate::authority::{
    AuthorityMatch, AuthorityMatchError, AuthoritySimulation, AuthorityTickReport,
};
use crate::authority_input::{
    AuthorityInputConfig, AuthorityInputOrigin, AuthoritySeatCommitOverride,
};
use crate::multiplayer_observability::{
    MultiplayerObservability, OnlineAuditCode, OnlineAuditScope, ServerTickDistribution,
};
use crate::multiplayer_security::{
    BanEntry, BanProvider, BanReason, LocalBanRegistry, PeerSecurityGuard, PeerSecurityMetrics,
    SecurityDisposition, SecurityPolicy, SecurityPolicyError, SecurityViolation,
};
use crate::network_codec::{
    Handshake, ProcessedInputAck, ResultId, ResultIdentifier, StateHashAndAcks, WireMessage,
};
use crate::network_io::NonBlockingDatagramEndpoint;
use crate::network_protocol::{
    ClockProbe, ClockProbeId, ClockReply, CommittedInputRecord, CommittedInputRelay,
    CommittedInputSource, CommittedSeatInputWindow, DisconnectCode, DisconnectMessage, InputFrame,
    MAX_INPUT_FRAMES_PER_WINDOW, MAX_RESYNC_INPUT_TAIL_TICKS, MAX_SEATS, MatchManifest, PeerId,
    ProtocolValidationError, ReconnectClaim, ResyncApplied, ResyncReason, ResyncRequest,
    RetryDisposition, SeatId, SeatOwner, SimTick, StartMessage, StateHash, TransferId,
};
use crate::network_runtime::{
    NetworkRuntime, PeerRole, ReliableSendHandle, ReliableSendStatus, RuntimeAbuseSignal,
    RuntimeConfig, RuntimeConfigError, RuntimeConnectionState, RuntimeEvent, RuntimeMetrics,
    RuntimeQueueError,
};
use crate::reconnect::{
    AuthenticatedPeer, AuthenticatedUserId, ReclaimReservation, ReconnectError, ReconnectPolicy,
    ReconnectRegistry, SubstituteControl,
};
use crate::resync_transfer::{AuthorityResyncTransfer, ResyncTransferError};
use crate::session::{DEFAULT_COUNTDOWN_LEAD_TICKS, MAX_COUNTDOWN_LEAD_TICKS};
use crate::session_clock::MIN_CLOCK_SYNC_SAMPLES;
use crate::snapshot::{CanonicalSnapshot, SnapshotError};
use crate::state_sync::{
    AuthoritySnapshotHistory, AuthorityStateSyncCoordinator, DEFAULT_STATE_SYNC_HISTORY_ENTRIES,
    FullResyncReason, PeerStateSyncError, PeerStateUpdateOutcome, StateSyncError,
};
use core::fmt;

/// There can be no more remote peers than fighter seats.
pub const MAX_AUTHORITY_PEERS: usize = MAX_SEATS;
pub const DEFAULT_STATE_DELTA_INTERVAL_TICKS: u8 = 3;
pub const DEFAULT_AUTHORITY_TICK_BUDGET_NS: u64 = 1_000_000;
/// A peer that just consumed a full repair must remain on the repaired
/// baseline for at least two seconds before asking the authority to encode
/// another snapshot.
pub const DEFAULT_PEER_REPAIR_REQUEST_COOLDOWN_TICKS: u64 = 2 * 60;
/// No peer may cause more than a small fixed number of full snapshot encodes in
/// one minute. Initial sync and reconnect do not consume this peer-authored
/// budget; every new in-fight client request does.
pub const DEFAULT_PEER_REPAIR_REQUEST_WINDOW_TICKS: u64 = 60 * 60;
pub const DEFAULT_MAX_PEER_REPAIR_REQUESTS_PER_WINDOW: u8 = 3;
pub const DEFAULT_TYPED_DISCONNECT_TIMEOUT_TICKS: u32 = 120;
pub const MAX_TYPED_DISCONNECT_TIMEOUT_TICKS: u32 = 600;
const CLOCK_SYNC_SAMPLE_CAPACITY: usize = MIN_CLOCK_SYNC_SAMPLES as usize;
const SERVER_SHUTDOWN_DETAIL_CODE: u16 = 1;

/// A retained, hash-matching baseline proves that a dense delta is a packet
/// sizing decision rather than evidence that the client is desynchronized.
/// The state hash queued alongside the omitted delta still drives the normal
/// client-requested repair path if prediction actually disagrees.
#[inline]
const fn should_start_proactive_repair(_reason: FullResyncReason) -> bool {
    // A repair must be fenced by the client that owns the local input
    // generation. Until the wire contract carries the authority's accepted
    // cursor and a new resume tick, an unsolicited snapshot can overtake more
    // authored future input than the seven-frame redundancy history can replay.
    false
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AuthorityConnectionId {
    slot: u8,
    generation: u32,
}

impl AuthorityConnectionId {
    pub const fn slot(self) -> u8 {
        self.slot
    }

    pub const fn generation(self) -> u32 {
        self.generation
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthorityPeerHubConfig {
    pub runtime: RuntimeConfig,
    pub reconnect: ReconnectPolicy,
    pub security: SecurityPolicy,
    pub state_history_entries: usize,
    pub state_delta_interval_ticks: u8,
    pub countdown_lead_ticks: u32,
    pub server_tick_budget_ns: u64,
    pub peer_repair_request_cooldown_ticks: u64,
    pub peer_repair_request_window_ticks: u64,
    pub max_peer_repair_requests_per_window: u8,
    pub typed_disconnect_timeout_ticks: u32,
}

impl Default for AuthorityPeerHubConfig {
    fn default() -> Self {
        Self {
            runtime: RuntimeConfig::default(),
            reconnect: ReconnectPolicy::default(),
            security: SecurityPolicy::default(),
            state_history_entries: DEFAULT_STATE_SYNC_HISTORY_ENTRIES,
            state_delta_interval_ticks: DEFAULT_STATE_DELTA_INTERVAL_TICKS,
            countdown_lead_ticks: DEFAULT_COUNTDOWN_LEAD_TICKS,
            server_tick_budget_ns: DEFAULT_AUTHORITY_TICK_BUDGET_NS,
            peer_repair_request_cooldown_ticks: DEFAULT_PEER_REPAIR_REQUEST_COOLDOWN_TICKS,
            peer_repair_request_window_ticks: DEFAULT_PEER_REPAIR_REQUEST_WINDOW_TICKS,
            max_peer_repair_requests_per_window: DEFAULT_MAX_PEER_REPAIR_REQUESTS_PER_WINDOW,
            typed_disconnect_timeout_ticks: DEFAULT_TYPED_DISCONNECT_TIMEOUT_TICKS,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AuthorityPeerPhase {
    #[default]
    AwaitingHandshake,
    AwaitingManifestAcceptance,
    AwaitingInitialResyncRequest,
    InitialSyncInFlight,
    AwaitingInitialSyncDeclaration,
    AwaitingReady,
    Ready,
    Countdown,
    Fighting,
    ReconnectHandshake,
    ReconnectSyncInFlight,
    ReconnectAwaitingClock,
    RepairSyncInFlight,
    Closing,
}

impl AuthorityPeerPhase {
    const fn accepts_live_input(self) -> bool {
        // A synchronized client can cross the agreed countdown boundary
        // slightly before the authority's next local network pump. Buffering
        // its bounded future input in Countdown is safe: the authority input
        // collector still enforces identity, ownership, sequence, and the
        // configured future-tick window, while canonical simulation remains
        // blocked until the authority reaches the start boundary.
        matches!(
            self,
            Self::Countdown | Self::Fighting | Self::RepairSyncInFlight
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AuthorityAdvanceOutcome {
    #[default]
    WaitingForReady,
    WaitingForStartTick,
    Advanced,
    Finished,
}

/// Global authority lifecycle fence. Once draining begins it is monotonic:
/// admission, inbound gameplay handling, countdown work, and canonical
/// simulation advancement can never resume.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AuthorityShutdownState {
    #[default]
    Running,
    Draining,
    Drained,
}

/// Why a physically retained Closing generation left the hub.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorityClosingCompletion {
    Acknowledged,
    TimedOut,
    TransportClosed,
}

/// Exact identity retained until a Closing generation physically retires.
///
/// `disconnect` is `None` only for a post-result drain, where sending a
/// no-contest terminal would violate the already-confirmed result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthorityPeerDrainEvent {
    pub peer_id: PeerId,
    pub user_id: AuthenticatedUserId,
    pub connection: AuthorityConnectionId,
    pub disconnect: Option<DisconnectMessage>,
    pub completion: AuthorityClosingCompletion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthorityPeerIdentity {
    pub peer_id: PeerId,
    pub user_id: AuthenticatedUserId,
    pub connection: AuthorityConnectionId,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AuthorityPeerHubMetrics {
    pub connections_attached: u64,
    pub authentication_rejections: u64,
    pub active_ban_rejections: u64,
    pub stale_connection_operations: u64,
    pub peers_rejected: u64,
    pub spoofed_messages: u64,
    pub malformed_or_abusive_disconnects: u64,
    /// Expected endpoint closures observed after the authority published its
    /// canonical result. These are lifecycle completion, not peer abuse.
    pub post_result_transport_closures: u64,
    pub security_violations: u64,
    pub security_warnings: u64,
    pub security_kicks: u64,
    pub temporary_bans: u64,
    pub platform_bans: u64,
    pub typed_disconnects_queued: u64,
    pub typed_disconnects_deferred: u64,
    pub typed_disconnects_acknowledged: u64,
    pub typed_disconnects_timed_out: u64,
    pub typed_disconnects_transport_closed: u64,
    pub authority_ticks: u64,
    pub startup_ticks_blocked: u64,
    pub startup_input_deadlines: u64,
    pub countdown_broadcasts_deferred: u64,
    pub input_batches_accepted: u64,
    pub input_batches_rejected: u64,
    pub state_packets_queued: u64,
    pub state_packets_deferred: u64,
    pub resyncs_started: u64,
    pub repair_requests_coalesced: u64,
    pub repair_requests_rate_limited: u64,
    pub repair_request_budgets_exhausted: u64,
    pub resyncs_applied: u64,
    pub reconnects_completed: u64,
    /// Disconnected peers whose retained seats permanently became authority bots.
    pub reconnect_grace_expirations: u64,
    pub results_queued: u64,
    pub results_deferred: u64,
}

#[derive(Debug)]
pub enum AuthorityPeerHubError<E> {
    Protocol(ProtocolValidationError),
    RuntimeConfig(RuntimeConfigError),
    RuntimeQueue(RuntimeQueueError),
    Security(SecurityPolicyError),
    Reconnect(ReconnectError),
    StateSync(StateSyncError),
    PeerStateSync(PeerStateSyncError),
    Resync(ResyncTransferError),
    Snapshot(SnapshotError),
    Authority(AuthorityMatchError<E>),
    InvalidConfig,
    EmptyRoster,
    Capacity,
    UnknownPeer(PeerId),
    DuplicatePeer(PeerId),
    IdentityMismatch(PeerId),
    ActiveBan,
    StaleConnection(AuthorityConnectionId),
    InitialAttachAfterCountdown,
    ReconnectBeforeDisconnect,
    TimelineRegression,
    TimelineExhausted,
    ShutdownInProgress,
    DrainEventsFull,
}

impl<E> fmt::Display for AuthorityPeerHubError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Protocol(_) => "protocol validation failed",
            Self::RuntimeConfig(_) => "runtime configuration failed",
            Self::RuntimeQueue(_) => "runtime queue failed",
            Self::Security(_) => "authority security policy failed",
            Self::Reconnect(_) => "reconnect policy failed",
            Self::StateSync(_) => "authority state history failed",
            Self::PeerStateSync(_) => "peer state synchronization failed",
            Self::Resync(_) => "reliable snapshot transfer failed",
            Self::Snapshot(_) => "canonical snapshot failed",
            Self::Authority(_) => "canonical authority failed",
            Self::InvalidConfig => "authority peer hub configuration is invalid",
            Self::EmptyRoster => "authority peer roster is empty",
            Self::Capacity => "authority peer capacity is exhausted",
            Self::UnknownPeer(_) => "authority peer is not in the authenticated roster",
            Self::DuplicatePeer(_) => "authority peer already has a live connection",
            Self::IdentityMismatch(_) => "authenticated identity does not own this peer",
            Self::ActiveBan => "authenticated identity has an active ban",
            Self::StaleConnection(_) => "authority connection generation is stale",
            Self::InitialAttachAfterCountdown => "initial peer attached after startup closed",
            Self::ReconnectBeforeDisconnect => "peer tried to reconnect before disconnecting",
            Self::TimelineRegression => "authority network clock regressed",
            Self::TimelineExhausted => "authority timeline identifier space is exhausted",
            Self::ShutdownInProgress => "authority shutdown fence is active",
            Self::DrainEventsFull => "authority closing-event queue is full",
        };
        formatter.write_str(message)
    }
}

impl<E: fmt::Debug> std::error::Error for AuthorityPeerHubError<E> {}

impl<E> From<ProtocolValidationError> for AuthorityPeerHubError<E> {
    fn from(value: ProtocolValidationError) -> Self {
        Self::Protocol(value)
    }
}

impl<E> From<RuntimeConfigError> for AuthorityPeerHubError<E> {
    fn from(value: RuntimeConfigError) -> Self {
        Self::RuntimeConfig(value)
    }
}

impl<E> From<RuntimeQueueError> for AuthorityPeerHubError<E> {
    fn from(value: RuntimeQueueError) -> Self {
        Self::RuntimeQueue(value)
    }
}

impl<E> From<SecurityPolicyError> for AuthorityPeerHubError<E> {
    fn from(value: SecurityPolicyError) -> Self {
        Self::Security(value)
    }
}

impl<E> From<ReconnectError> for AuthorityPeerHubError<E> {
    fn from(value: ReconnectError) -> Self {
        Self::Reconnect(value)
    }
}

impl<E> From<StateSyncError> for AuthorityPeerHubError<E> {
    fn from(value: StateSyncError) -> Self {
        Self::StateSync(value)
    }
}

impl<E> From<PeerStateSyncError> for AuthorityPeerHubError<E> {
    fn from(value: PeerStateSyncError) -> Self {
        Self::PeerStateSync(value)
    }
}

impl<E> From<ResyncTransferError> for AuthorityPeerHubError<E> {
    fn from(value: ResyncTransferError) -> Self {
        Self::Resync(value)
    }
}

impl<E> From<SnapshotError> for AuthorityPeerHubError<E> {
    fn from(value: SnapshotError) -> Self {
        Self::Snapshot(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TransferPurpose {
    Initial,
    Repair,
    Reconnect(ReclaimReservation),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TransferStage {
    Begin,
    InputTail,
    Chunks,
    WaitingApplied,
}

struct PendingTransfer {
    transfer: AuthorityResyncTransfer,
    purpose: TransferPurpose,
    stage: TransferStage,
    next_chunk: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingTypedDisconnect {
    message: DisconnectMessage,
    send: Option<ReliableSendHandle>,
    deadline_tick: SimTick,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PeerRepairRequestBudget {
    window_started_at: Option<SimTick>,
    last_started_at: Option<SimTick>,
    started_in_window: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PeerRepairBudgetOutcome {
    Allowed,
    Cooldown,
    Exhausted,
}

impl PeerRepairRequestBudget {
    fn try_start(
        &mut self,
        now: SimTick,
        cooldown_ticks: u64,
        window_ticks: u64,
        maximum: u8,
    ) -> PeerRepairBudgetOutcome {
        if self
            .last_started_at
            .is_some_and(|last| now.get().saturating_sub(last.get()) < cooldown_ticks)
        {
            return PeerRepairBudgetOutcome::Cooldown;
        }

        if self
            .window_started_at
            .is_none_or(|started| now.get().saturating_sub(started.get()) >= window_ticks)
        {
            self.window_started_at = Some(now);
            self.started_in_window = 0;
        }
        if self.started_in_window >= maximum {
            return PeerRepairBudgetOutcome::Exhausted;
        }

        self.started_in_window = self.started_in_window.saturating_add(1);
        self.last_started_at = Some(now);
        PeerRepairBudgetOutcome::Allowed
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct CommittedInputHistory {
    records: [Option<CommittedInputRecord>; MAX_INPUT_FRAMES_PER_WINDOW],
    len: u8,
}

impl CommittedInputHistory {
    fn push(&mut self, record: CommittedInputRecord) {
        let retained = self.len().min(MAX_INPUT_FRAMES_PER_WINDOW - 1);
        for index in (0..retained).rev() {
            self.records[index + 1] = self.records[index];
        }
        self.records[0] = Some(record);
        self.len = (retained + 1) as u8;
    }

    const fn len(&self) -> usize {
        self.len as usize
    }

    fn window(&self) -> Result<CommittedSeatInputWindow, ProtocolValidationError> {
        self.window_with_len(self.len())
    }

    fn window_with_len(
        &self,
        len: usize,
    ) -> Result<CommittedSeatInputWindow, ProtocolValidationError> {
        if len == 0 || len > self.len() {
            return Err(ProtocolValidationError::InvalidTickWindow);
        }
        let mut records = [CommittedInputRecord::default(); MAX_INPUT_FRAMES_PER_WINDOW];
        for (target, source) in records[..len].iter_mut().zip(self.records.iter()) {
            *target = source.expect("committed input history prefix is dense");
        }
        CommittedSeatInputWindow::from_newest_first(&records[..len])
    }
}

struct AuthorityPeerLink<E: NonBlockingDatagramEndpoint> {
    peer_id: PeerId,
    user_id: AuthenticatedUserId,
    connection: AuthorityConnectionId,
    phase: AuthorityPeerPhase,
    runtime: NetworkRuntime<E>,
    transfer: Option<PendingTransfer>,
    applied_sync: Option<ResyncApplied>,
    resume_input_tick: Option<SimTick>,
    pending_result: Option<ResultIdentifier>,
    queued_result: Option<ResultIdentifier>,
    pending_disconnect: Option<PendingTypedDisconnect>,
    post_result_close_deadline: Option<SimTick>,
    clock_samples: [Option<ClockProbeId>; CLOCK_SYNC_SAMPLE_CAPACITY],
    clock_sample_count: u8,
    pending_clock_replies: [Option<ClockReply>; CLOCK_SYNC_SAMPLE_CAPACITY],
    pending_reclaim_completion: Option<(ReclaimReservation, ResyncApplied)>,
    next_transfer_id: u32,
    security: PeerSecurityGuard,
}

impl<E: NonBlockingDatagramEndpoint> AuthorityPeerLink<E> {
    fn new(
        peer_id: PeerId,
        user_id: AuthenticatedUserId,
        connection: AuthorityConnectionId,
        phase: AuthorityPeerPhase,
        endpoint: E,
        compatibility: crate::network_protocol::CompatibilityId,
        runtime_config: RuntimeConfig,
        security_policy: SecurityPolicy,
        now: SimTick,
    ) -> Result<Self, AuthorityPeerLinkError> {
        Ok(Self {
            peer_id,
            user_id,
            connection,
            phase,
            runtime: NetworkRuntime::new(
                endpoint,
                PeerRole::Authority,
                compatibility,
                runtime_config,
            )
            .map_err(AuthorityPeerLinkError::Runtime)?,
            transfer: None,
            applied_sync: None,
            resume_input_tick: None,
            pending_result: None,
            queued_result: None,
            pending_disconnect: None,
            post_result_close_deadline: None,
            clock_samples: [None; CLOCK_SYNC_SAMPLE_CAPACITY],
            clock_sample_count: 0,
            pending_clock_replies: [None; CLOCK_SYNC_SAMPLE_CAPACITY],
            pending_reclaim_completion: None,
            next_transfer_id: 1,
            security: PeerSecurityGuard::new(security_policy, now)
                .map_err(AuthorityPeerLinkError::Security)?,
        })
    }

    fn clock_synchronized(&self) -> bool {
        usize::from(self.clock_sample_count) >= CLOCK_SYNC_SAMPLE_CAPACITY
    }

    fn has_clock_probe(&self, probe_id: ClockProbeId) -> bool {
        self.clock_samples
            .iter()
            .flatten()
            .any(|id| *id == probe_id)
            || self
                .pending_clock_replies
                .iter()
                .flatten()
                .any(|reply| reply.probe_id == probe_id)
    }

    fn allocate_transfer_id(&mut self) -> Result<TransferId, ProtocolValidationError> {
        let transfer = TransferId::new(self.next_transfer_id)?;
        self.next_transfer_id = self.next_transfer_id.checked_add(1).unwrap_or(0);
        Ok(transfer)
    }
}

enum AuthorityPeerLinkError {
    Runtime(RuntimeConfigError),
    Security(SecurityPolicyError),
}

/// One production authority match and all of its remote peer connections.
///
/// Call `pump_network` and `try_advance` from the same fixed-rate worker.  This
/// ensures endpoint work, input deadlines, and canonical stepping have a single
/// owner while every connection still has an isolated bounded outbound queue.
pub struct AuthorityPeerHub<S, E, B = LocalBanRegistry>
where
    S: AuthoritySimulation<Snapshot = CanonicalSnapshot>,
    E: NonBlockingDatagramEndpoint,
{
    manifest: MatchManifest,
    authority: AuthorityMatch<S>,
    config: AuthorityPeerHubConfig,
    expected: [Option<AuthenticatedPeer>; MAX_AUTHORITY_PEERS],
    expected_count: usize,
    peers: [Option<AuthorityPeerLink<E>>; MAX_AUTHORITY_PEERS],
    repair_request_budgets: [PeerRepairRequestBudget; MAX_AUTHORITY_PEERS],
    slot_generations: [u32; MAX_AUTHORITY_PEERS],
    reconnect: ReconnectRegistry,
    state_history: AuthoritySnapshotHistory,
    peer_state: AuthorityStateSyncCoordinator,
    committed_histories: [CommittedInputHistory; MAX_SEATS],
    network_tick: SimTick,
    countdown_start_tick: Option<SimTick>,
    result: Option<ResultIdentifier>,
    shutdown: AuthorityShutdownState,
    shutdown_exempt_peer: Option<PeerId>,
    drain_events: [Option<AuthorityPeerDrainEvent>; MAX_AUTHORITY_PEERS],
    drain_event_head: u8,
    drain_event_len: u8,
    metrics: AuthorityPeerHubMetrics,
    bans: B,
    observability: MultiplayerObservability,
    audit_monotonic_ms: u64,
}

impl<S, E> AuthorityPeerHub<S, E, LocalBanRegistry>
where
    S: AuthoritySimulation<Snapshot = CanonicalSnapshot>,
    E: NonBlockingDatagramEndpoint,
{
    pub fn new(
        manifest: MatchManifest,
        simulation: S,
        input_config: AuthorityInputConfig,
        authenticated_peers: &[AuthenticatedPeer],
        config: AuthorityPeerHubConfig,
    ) -> Result<Self, AuthorityPeerHubError<S::Error>> {
        Self::new_with_ban_provider(
            manifest,
            simulation,
            input_config,
            authenticated_peers,
            config,
            LocalBanRegistry::default(),
        )
    }
}

impl<S, E, B> AuthorityPeerHub<S, E, B>
where
    S: AuthoritySimulation<Snapshot = CanonicalSnapshot>,
    E: NonBlockingDatagramEndpoint,
    B: BanProvider,
{
    pub fn new_with_ban_provider(
        manifest: MatchManifest,
        simulation: S,
        input_config: AuthorityInputConfig,
        authenticated_peers: &[AuthenticatedPeer],
        config: AuthorityPeerHubConfig,
        bans: B,
    ) -> Result<Self, AuthorityPeerHubError<S::Error>> {
        manifest.validate_for_start(SimTick::ZERO)?;
        config.runtime.validate()?;
        config.reconnect.validate()?;
        config.security.validate()?;
        if config.state_delta_interval_ticks == 0
            || config.countdown_lead_ticks == 0
            || config.countdown_lead_ticks > MAX_COUNTDOWN_LEAD_TICKS
            || config.server_tick_budget_ns == 0
            || config.peer_repair_request_cooldown_ticks == 0
            || config.peer_repair_request_window_ticks < config.peer_repair_request_cooldown_ticks
            || config.max_peer_repair_requests_per_window == 0
            || config.typed_disconnect_timeout_ticks == 0
            || config.typed_disconnect_timeout_ticks > MAX_TYPED_DISCONNECT_TIMEOUT_TICKS
        {
            return Err(AuthorityPeerHubError::InvalidConfig);
        }
        if authenticated_peers.is_empty() {
            return Err(AuthorityPeerHubError::EmptyRoster);
        }
        if authenticated_peers.len() > MAX_AUTHORITY_PEERS {
            return Err(AuthorityPeerHubError::Capacity);
        }

        let authority = AuthorityMatch::new(manifest, simulation, input_config)
            .map_err(AuthorityPeerHubError::Authority)?;
        if authority.simulation().current_tick() != SimTick::ZERO {
            return Err(AuthorityPeerHubError::InitialAttachAfterCountdown);
        }
        let initial_snapshot = authority
            .snapshot_at(SimTick::ZERO)
            .expect("AuthorityMatch retains its validated initial snapshot");
        let mut state_history =
            AuthoritySnapshotHistory::new(manifest.match_id, config.state_history_entries)?;
        state_history.record_snapshot(initial_snapshot)?;
        let peer_state =
            AuthorityStateSyncCoordinator::new(manifest.match_id, authenticated_peers.len())?;
        let reconnect = ReconnectRegistry::new(
            manifest.match_id,
            &manifest.ownership,
            authenticated_peers,
            config.reconnect,
        )?;
        let observability = MultiplayerObservability::new(config.server_tick_budget_ns)
            .map_err(|_| AuthorityPeerHubError::InvalidConfig)?;

        let mut expected = [None; MAX_AUTHORITY_PEERS];
        for (slot, peer) in authenticated_peers.iter().copied().enumerate() {
            expected[slot] = Some(peer);
        }

        Ok(Self {
            manifest,
            authority,
            config,
            expected,
            expected_count: authenticated_peers.len(),
            peers: std::array::from_fn(|_| None),
            repair_request_budgets: [PeerRepairRequestBudget::default(); MAX_AUTHORITY_PEERS],
            slot_generations: [0; MAX_AUTHORITY_PEERS],
            reconnect,
            state_history,
            peer_state,
            committed_histories: [CommittedInputHistory::default(); MAX_SEATS],
            network_tick: SimTick::ZERO,
            countdown_start_tick: None,
            result: None,
            shutdown: AuthorityShutdownState::Running,
            shutdown_exempt_peer: None,
            drain_events: [None; MAX_AUTHORITY_PEERS],
            drain_event_head: 0,
            drain_event_len: 0,
            metrics: AuthorityPeerHubMetrics::default(),
            bans,
            observability,
            audit_monotonic_ms: 0,
        })
    }

    pub const fn manifest(&self) -> &MatchManifest {
        &self.manifest
    }

    pub const fn authority(&self) -> &AuthorityMatch<S> {
        &self.authority
    }

    pub fn authority_mut(&mut self) -> &mut AuthorityMatch<S> {
        &mut self.authority
    }

    pub const fn network_tick(&self) -> SimTick {
        self.network_tick
    }

    pub const fn metrics(&self) -> AuthorityPeerHubMetrics {
        self.metrics
    }

    /// Bounded, non-canonical authority diagnostics. The returned state never
    /// contains packet payloads, authentication material, addresses, or names.
    pub const fn observability(&self) -> &MultiplayerObservability {
        &self.observability
    }

    /// Records one complete authority-worker service duration. Wall time is
    /// diagnostic only and never enters canonical simulation state.
    pub fn observe_server_tick(&mut self, duration_ns: u64) {
        self.observability.observe_server_tick(duration_ns);
    }

    pub fn server_tick_distribution(&self) -> ServerTickDistribution {
        self.observability.server_tick_distribution()
    }

    /// Operator/publisher integrations can seed or inspect the configured ban
    /// provider without exposing it to simulation code.
    pub const fn ban_provider(&self) -> &B {
        &self.bans
    }

    pub fn ban_provider_mut(&mut self) -> &mut B {
        &mut self.bans
    }

    pub fn peer_security_metrics(&self, peer_id: PeerId) -> Option<PeerSecurityMetrics> {
        self.peer_index(peer_id).and_then(|index| {
            self.peers[index]
                .as_ref()
                .map(|peer| peer.security.metrics())
        })
    }

    pub fn peer_runtime_metrics(&self, peer_id: PeerId) -> Option<RuntimeMetrics> {
        self.peer_index(peer_id).and_then(|index| {
            self.peers[index]
                .as_ref()
                .map(|peer| *peer.runtime.metrics())
        })
    }

    /// Applies a platform authentication-session revocation to one exact
    /// physical generation. A delayed revocation can never select a reconnect
    /// replacement by peer id.
    pub fn revoke_authentication(
        &mut self,
        connection: AuthorityConnectionId,
    ) -> Result<(), AuthorityPeerHubError<S::Error>> {
        let index = usize::from(connection.slot);
        let Some(link) = self.peers.get(index).and_then(Option::as_ref) else {
            self.metrics.stale_connection_operations =
                self.metrics.stale_connection_operations.saturating_add(1);
            return Err(AuthorityPeerHubError::StaleConnection(connection));
        };
        if link.connection != connection {
            self.metrics.stale_connection_operations =
                self.metrics.stale_connection_operations.saturating_add(1);
            return Err(AuthorityPeerHubError::StaleConnection(connection));
        }
        let _ =
            self.observe_peer_violation(index, SecurityViolation::AuthenticationRevoked, true)?;
        Ok(())
    }

    /// Records a permanent platform/publisher ban and immediately removes any
    /// matching link. This accepts only the already-authenticated numeric user
    /// identity; ticket bytes and persona data never cross the hub boundary.
    pub fn enforce_platform_ban(
        &mut self,
        user_id: AuthenticatedUserId,
    ) -> Result<(), AuthorityPeerHubError<S::Error>> {
        if let Some(index) = self
            .peers
            .iter()
            .position(|slot| slot.as_ref().is_some_and(|peer| peer.user_id == user_id))
        {
            let _ = self.observe_peer_violation(index, SecurityViolation::PlatformBan, true)?;
            return Ok(());
        }

        let peer_id = self.expected[..self.expected_count]
            .iter()
            .flatten()
            .find(|peer| peer.user_id == user_id)
            .map(|peer| peer.peer_id);
        let offenses = self
            .bans
            .lookup(user_id, self.network_tick)
            .map_or(1, |entry| entry.offenses.saturating_add(1));
        self.bans.record(BanEntry {
            user: user_id,
            reason: BanReason::PlatformBan,
            issued_at: self.network_tick,
            expires_at: None,
            offenses,
        })?;
        self.metrics.platform_bans = self.metrics.platform_bans.saturating_add(1);
        self.record_audit(
            OnlineAuditScope {
                match_id: Some(self.manifest.match_id),
                peer_id,
                tick: Some(self.network_tick),
                ..OnlineAuditScope::default()
            },
            OnlineAuditCode::PeerAdmissionBanned,
            u64::from(SecurityViolation::PlatformBan.detail_code()),
            u64::from(offenses),
        );
        Ok(())
    }

    pub const fn countdown_broadcast(&self) -> bool {
        self.countdown_start_tick.is_some()
    }

    pub const fn countdown_start_tick(&self) -> Option<SimTick> {
        self.countdown_start_tick
    }

    pub const fn confirmed_result(&self) -> Option<ResultIdentifier> {
        self.result
    }

    pub const fn shutdown_state(&self) -> AuthorityShutdownState {
        self.shutdown
    }

    pub const fn shutdown_drained(&self) -> bool {
        matches!(self.shutdown, AuthorityShutdownState::Drained)
    }

    /// Returns the next exact physical-retirement record. The queue is fixed
    /// to the maximum number of simultaneous peer generations. Producers
    /// leave a completed link in Closing if the consumer has not drained
    /// enough records, so identity is never overwritten or dropped.
    pub fn try_next_drain_event(&mut self) -> Option<AuthorityPeerDrainEvent> {
        if self.drain_event_len == 0 {
            return None;
        }
        let index = usize::from(self.drain_event_head);
        let event = self.drain_events[index].take();
        self.drain_event_head =
            ((usize::from(self.drain_event_head) + 1) % MAX_AUTHORITY_PEERS) as u8;
        self.drain_event_len = self.drain_event_len.saturating_sub(1);
        event
    }

    /// Includes Closing generations so platform/authentication owners can
    /// retain the exact physical identity until the drain event arrives.
    pub fn peer_identity(&self, peer_id: PeerId) -> Option<AuthorityPeerIdentity> {
        self.peer_index(peer_id).and_then(|index| {
            self.peers[index]
                .as_ref()
                .map(|peer| AuthorityPeerIdentity {
                    peer_id: peer.peer_id,
                    user_id: peer.user_id,
                    connection: peer.connection,
                })
        })
    }

    pub fn peer_phase(&self, peer_id: PeerId) -> Option<AuthorityPeerPhase> {
        self.peer_index(peer_id)
            .and_then(|index| self.peers[index].as_ref().map(|peer| peer.phase))
    }

    pub fn connection_for_peer(&self, peer_id: PeerId) -> Option<AuthorityConnectionId> {
        self.peer_index(peer_id).and_then(|index| {
            self.peers[index].as_ref().and_then(|peer| {
                (peer.phase != AuthorityPeerPhase::Closing).then_some(peer.connection)
            })
        })
    }

    /// Resolves only the exact live physical generation. Unlike a peer lookup,
    /// this can never select a reconnect replacement after a stale close
    /// notification was delayed or backpressured.
    pub fn peer_for_connection(&self, connection: AuthorityConnectionId) -> Option<PeerId> {
        self.peers
            .get(usize::from(connection.slot))
            .and_then(Option::as_ref)
            .filter(|peer| peer.connection == connection)
            .map(|peer| peer.peer_id)
    }

    pub fn authenticated_user_for_peer(&self, peer_id: PeerId) -> Option<AuthenticatedUserId> {
        self.peer_index(peer_id).and_then(|index| {
            self.peers[index].as_ref().and_then(|peer| {
                (peer.phase != AuthorityPeerPhase::Closing).then_some(peer.user_id)
            })
        })
    }

    /// Installs the irreversible global shutdown fence before touching any
    /// link. Active/countdown remote peers receive one typed no-contest
    /// terminal. If a result is already canonical, remotes instead drain the
    /// reliable result path without receiving a contradictory no-contest.
    ///
    /// The excluded peer is the in-process listen-host endpoint. It remains
    /// physically attached until the worker observes [`Self::shutdown_drained`]
    /// and detaches it as the final local teardown step.
    pub fn begin_shutdown(
        &mut self,
        excluded_peer: PeerId,
    ) -> Result<(), AuthorityPeerHubError<S::Error>> {
        match self.shutdown {
            AuthorityShutdownState::Running => {
                self.shutdown = AuthorityShutdownState::Draining;
                self.shutdown_exempt_peer = Some(excluded_peer);
            }
            AuthorityShutdownState::Draining | AuthorityShutdownState::Drained
                if self.shutdown_exempt_peer == Some(excluded_peer) =>
            {
                return Ok(());
            }
            AuthorityShutdownState::Draining | AuthorityShutdownState::Drained => {
                return Err(AuthorityPeerHubError::ShutdownInProgress);
            }
        }

        let has_confirmed_result = self.result.is_some();
        for index in 0..MAX_AUTHORITY_PEERS {
            let Some(link) = self.peers[index].as_ref() else {
                continue;
            };
            if link.peer_id == excluded_peer || link.phase == AuthorityPeerPhase::Closing {
                continue;
            }
            if has_confirmed_result {
                self.begin_post_result_close(index)?;
            } else {
                self.begin_typed_disconnect(
                    index,
                    DisconnectMessage {
                        match_id: Some(self.manifest.match_id),
                        code: DisconnectCode::ServerShutdown,
                        retry: RetryDisposition::MatchEndedNoContest,
                        detail_code: SERVER_SHUTDOWN_DETAIL_CODE,
                        last_confirmed_tick: Some(self.authority.simulation().current_tick()),
                    },
                )?;
            }
        }
        self.refresh_shutdown_state();
        Ok(())
    }

    pub fn attach_initial(
        &mut self,
        peer_id: PeerId,
        user_id: AuthenticatedUserId,
        endpoint: E,
    ) -> Result<AuthorityConnectionId, AuthorityPeerHubError<S::Error>> {
        if self.shutdown != AuthorityShutdownState::Running {
            return Err(AuthorityPeerHubError::ShutdownInProgress);
        }
        if self.countdown_start_tick.is_some()
            || self.authority.simulation().current_tick() != SimTick::ZERO
        {
            return Err(AuthorityPeerHubError::InitialAttachAfterCountdown);
        }
        self.validate_admission(peer_id, user_id)?;
        self.attach_link(
            peer_id,
            user_id,
            endpoint,
            AuthorityPeerPhase::AwaitingHandshake,
        )
    }

    /// Reserves the authenticated user's retained seats before the replacement
    /// endpoint becomes eligible to send gameplay input.
    pub fn attach_reconnect(
        &mut self,
        user_id: AuthenticatedUserId,
        claim: ReconnectClaim,
        endpoint: E,
    ) -> Result<AuthorityConnectionId, AuthorityPeerHubError<S::Error>> {
        if self.shutdown != AuthorityShutdownState::Running {
            return Err(AuthorityPeerHubError::ShutdownInProgress);
        }
        self.validate_admission(claim.peer_id, user_id)?;
        let closing_connection = match self.peer_index(claim.peer_id) {
            Some(index)
                if self.peers[index]
                    .as_ref()
                    .is_some_and(|link| link.phase == AuthorityPeerPhase::Closing) =>
            {
                Some(
                    self.peers[index]
                        .as_ref()
                        .expect("closing peer index")
                        .connection,
                )
            }
            Some(_) => return Err(AuthorityPeerHubError::DuplicatePeer(claim.peer_id)),
            None => None,
        };
        if let Some(index) = self.peer_index(claim.peer_id)
            && self.peers[index]
                .as_ref()
                .is_some_and(|link| link.user_id != user_id)
        {
            return Err(AuthorityPeerHubError::IdentityMismatch(claim.peer_id));
        }
        let authority_tick = self.authority.simulation().current_tick();
        self.resolve_substitute_control(claim.peer_id, authority_tick)?;
        let reservation = self
            .reconnect
            .begin_reclaim(user_id, claim, authority_tick)?;
        if let Some(connection) = closing_connection {
            let index = usize::from(connection.slot);
            if !self.complete_closing(index, AuthorityClosingCompletion::TransportClosed)? {
                self.reconnect.abort_reclaim(
                    claim.peer_id,
                    reservation.attempt_id,
                    authority_tick,
                )?;
                return Err(AuthorityPeerHubError::DrainEventsFull);
            }
        }
        let connection = match self.attach_link(
            claim.peer_id,
            user_id,
            endpoint,
            AuthorityPeerPhase::ReconnectHandshake,
        ) {
            Ok(connection) => connection,
            Err(error) => {
                self.reconnect.abort_reclaim(
                    claim.peer_id,
                    reservation.attempt_id,
                    authority_tick,
                )?;
                return Err(error);
            }
        };
        let snapshot = self
            .authority
            .snapshot_at(reservation.snapshot_tick)
            .expect("reclaim reserves the authority's current retained snapshot")
            .clone();
        if let Err(error) = self.prepare_transfer_for_peer(
            claim.peer_id,
            ResyncRequest {
                match_id: self.manifest.match_id,
                peer_id: claim.peer_id,
                reason: ResyncReason::Reconnect,
                last_confirmed_tick: claim.last_confirmed_tick,
                last_confirmed_hash: StateHash(0),
            },
            snapshot,
            TransferPurpose::Reconnect(reservation),
        ) {
            self.detach(connection)?;
            return Err(error);
        }
        Ok(connection)
    }

    pub fn detach(
        &mut self,
        connection: AuthorityConnectionId,
    ) -> Result<PeerId, AuthorityPeerHubError<S::Error>> {
        let index = usize::from(connection.slot);
        let Some(link) = self.peers.get(index).and_then(Option::as_ref) else {
            self.metrics.stale_connection_operations =
                self.metrics.stale_connection_operations.saturating_add(1);
            return Err(AuthorityPeerHubError::StaleConnection(connection));
        };
        if link.connection != connection {
            self.metrics.stale_connection_operations =
                self.metrics.stale_connection_operations.saturating_add(1);
            return Err(AuthorityPeerHubError::StaleConnection(connection));
        }
        let peer_id = link.peer_id;
        if link.phase != AuthorityPeerPhase::Closing {
            self.logical_disconnect(index)?;
        }
        self.retire_physical_connection(connection)?;
        Ok(peer_id)
    }

    fn logical_disconnect(
        &mut self,
        index: usize,
    ) -> Result<PeerId, AuthorityPeerHubError<S::Error>> {
        let authority_tick = self.authority.simulation().current_tick();
        let (peer_id, connection, pending_reclaim, already_closing) = {
            let link = self.peers[index].as_ref().expect("live peer index");
            (
                link.peer_id,
                link.connection,
                self.reconnect.pending_reclaim(link.peer_id)?,
                link.phase == AuthorityPeerPhase::Closing,
            )
        };
        if already_closing {
            return Ok(peer_id);
        }
        if let Some(pending) = pending_reclaim {
            self.reconnect
                .abort_reclaim(peer_id, pending.attempt_id, authority_tick)?;
        }
        self.peer_state.disconnect_peer(peer_id);
        if self.reconnect.substitute_control(peer_id, authority_tick)?
            == SubstituteControl::Connected
        {
            self.reconnect.record_disconnect(peer_id, authority_tick)?;
        }

        let link = self.peers[index].as_mut().expect("live peer index");
        link.phase = AuthorityPeerPhase::Closing;
        link.runtime.prepare_for_terminal_disconnect();
        link.transfer = None;
        link.applied_sync = None;
        link.resume_input_tick = None;
        link.pending_result = None;
        link.queued_result = None;
        link.pending_disconnect = None;
        link.post_result_close_deadline = None;
        link.clock_samples.fill(None);
        link.clock_sample_count = 0;
        link.pending_clock_replies.fill(None);
        link.pending_reclaim_completion = None;

        self.record_audit(
            OnlineAuditScope {
                match_id: Some(self.manifest.match_id),
                peer_id: Some(peer_id),
                tick: Some(authority_tick),
                ..OnlineAuditScope::default()
            },
            OnlineAuditCode::PeerDisconnected,
            u64::from(connection.generation),
            0,
        );
        Ok(peer_id)
    }

    fn retire_physical_connection(
        &mut self,
        connection: AuthorityConnectionId,
    ) -> Result<PeerId, AuthorityPeerHubError<S::Error>> {
        let index = usize::from(connection.slot);
        let Some(link) = self.peers.get(index).and_then(Option::as_ref) else {
            self.metrics.stale_connection_operations =
                self.metrics.stale_connection_operations.saturating_add(1);
            return Err(AuthorityPeerHubError::StaleConnection(connection));
        };
        if link.connection != connection {
            self.metrics.stale_connection_operations =
                self.metrics.stale_connection_operations.saturating_add(1);
            return Err(AuthorityPeerHubError::StaleConnection(connection));
        }
        let peer_id = link.peer_id;
        self.peers[index] = None;
        Ok(peer_id)
    }

    /// Pumps bounded work for every live authority-side endpoint. One slow peer
    /// can fill only its own runtime queue and never prevents another peer from
    /// being pumped or from receiving state/result traffic.
    pub fn pump_network(&mut self, now: SimTick) -> Result<(), AuthorityPeerHubError<S::Error>> {
        self.pump_network_at(now, now.get())
    }

    /// Timestamped production variant. `monotonic_ms` is used only for the
    /// bounded privacy-safe audit ring and is clamped if the platform clock
    /// stalls or regresses; canonical progression remains tick-driven.
    pub fn pump_network_at(
        &mut self,
        now: SimTick,
        monotonic_ms: u64,
    ) -> Result<(), AuthorityPeerHubError<S::Error>> {
        if now < self.network_tick {
            return Err(AuthorityPeerHubError::TimelineRegression);
        }
        self.network_tick = now;
        self.audit_monotonic_ms = self.audit_monotonic_ms.max(monotonic_ms);

        if self.shutdown != AuthorityShutdownState::Running {
            self.pump_shutdown_links(now)?;
            return Ok(());
        }

        // A client also enters Fighting at this exact network boundary and may
        // send its first InputBatch immediately. Promote authority-side peer
        // phases before consuming inbound events so that boundary input cannot
        // be rejected merely because phase maintenance ran later in this pump.
        if self
            .countdown_start_tick
            .is_some_and(|start_tick| now >= start_tick)
        {
            for peer in self.peers.iter_mut().flatten() {
                if peer.phase == AuthorityPeerPhase::Countdown {
                    peer.phase = AuthorityPeerPhase::Fighting;
                }
            }
        }

        for index in 0..MAX_AUTHORITY_PEERS {
            let Some(_) = self.peers[index].as_ref() else {
                continue;
            };
            let (report, abuse_signal, before, after, connection) = {
                let link = self.peers[index].as_mut().expect("checked live peer slot");
                let before = *link.runtime.metrics();
                let report = link.runtime.pump(now);
                let abuse_signal = link.runtime.take_abuse_signal();
                let after = *link.runtime.metrics();
                (report, abuse_signal, before, after, link.connection)
            };
            self.observe_runtime_metrics(before, after);

            if self.peers[index]
                .as_ref()
                .is_some_and(|link| link.phase == AuthorityPeerPhase::Closing)
            {
                self.finish_closing_if_ready(index, now, report.connection)?;
                continue;
            }

            if report.connection != RuntimeConnectionState::Active {
                if self.result.is_some() {
                    self.metrics.post_result_transport_closures = self
                        .metrics
                        .post_result_transport_closures
                        .saturating_add(1);
                } else {
                    self.metrics.malformed_or_abusive_disconnects = self
                        .metrics
                        .malformed_or_abusive_disconnects
                        .saturating_add(1);
                }
                let peer_id = self.peers[index]
                    .as_ref()
                    .expect("peer remains live until detach")
                    .peer_id;
                self.record_audit(
                    OnlineAuditScope {
                        match_id: Some(self.manifest.match_id),
                        peer_id: Some(peer_id),
                        tick: Some(self.authority.simulation().current_tick()),
                        ..OnlineAuditScope::default()
                    },
                    OnlineAuditCode::TransportClosed,
                    runtime_connection_code(report.connection),
                    0,
                );
                self.detach(connection)?;
                continue;
            }

            if self.observe_runtime_violations(index, before, after, abuse_signal)? {
                continue;
            }
            self.peers[index]
                .as_mut()
                .expect("security processing retained peer")
                .security
                .observe_clean_tick(now)?;

            loop {
                let event = self.peers[index]
                    .as_mut()
                    .and_then(|peer| peer.runtime.try_next_event());
                let Some(event) = event else { break };
                if !self.handle_event(index, event)? {
                    break;
                }
            }
            if self.peers[index].is_some() {
                self.service_peer_outbound(index)?;
            }
        }

        self.maybe_start_countdown()?;
        Ok(())
    }

    /// After the global fence, endpoint retries/ACKs are the only permitted
    /// work. Inbound protocol events remain unconsumed and can never reach
    /// input, resync, readiness, or canonical-state handlers.
    fn pump_shutdown_links(&mut self, now: SimTick) -> Result<(), AuthorityPeerHubError<S::Error>> {
        for index in 0..MAX_AUTHORITY_PEERS {
            let Some(link) = self.peers[index].as_ref() else {
                continue;
            };
            if link.phase != AuthorityPeerPhase::Closing {
                debug_assert_eq!(Some(link.peer_id), self.shutdown_exempt_peer);
                continue;
            }
            let (report, before, after) = {
                let link = self.peers[index].as_mut().expect("checked live peer slot");
                let before = *link.runtime.metrics();
                let report = link.runtime.pump(now);
                let after = *link.runtime.metrics();
                (report, before, after)
            };
            self.observe_runtime_metrics(before, after);
            if self.peers[index]
                .as_ref()
                .is_some_and(|link| link.phase == AuthorityPeerPhase::Closing)
            {
                self.finish_closing_if_ready(index, now, report.connection)?;
            }
        }
        self.refresh_shutdown_state();
        Ok(())
    }

    fn finish_closing_if_ready(
        &mut self,
        index: usize,
        now: SimTick,
        connection_state: RuntimeConnectionState,
    ) -> Result<bool, AuthorityPeerHubError<S::Error>> {
        if self.peers[index]
            .as_ref()
            .is_some_and(|link| link.post_result_close_deadline.is_some())
        {
            self.service_post_result_outbound(index)?;
        }

        let (pending_disconnect, post_result_deadline, reliable_pending, status) = {
            let link = self.peers[index].as_ref().expect("closing peer index");
            (
                link.pending_disconnect,
                link.post_result_close_deadline,
                link.runtime.reliable_pending_len(),
                link.pending_disconnect.and_then(|pending| {
                    pending
                        .send
                        .map(|send| link.runtime.reliable_send_status(send))
                }),
            )
        };
        let completion = if status == Some(ReliableSendStatus::Acknowledged) {
            Some(AuthorityClosingCompletion::Acknowledged)
        } else if status == Some(ReliableSendStatus::Exhausted)
            || connection_state == RuntimeConnectionState::RetryExhausted
        {
            Some(AuthorityClosingCompletion::TimedOut)
        } else if matches!(
            connection_state,
            RuntimeConnectionState::RemoteDisconnect
                | RuntimeConnectionState::TransportDisconnected
        ) {
            Some(AuthorityClosingCompletion::TransportClosed)
        } else if pending_disconnect.is_some_and(|pending| now >= pending.deadline_tick)
            || post_result_deadline.is_some_and(|deadline| now >= deadline)
        {
            Some(AuthorityClosingCompletion::TimedOut)
        } else if post_result_deadline.is_some()
            && self.peers[index]
                .as_ref()
                .is_some_and(|link| link.pending_result.is_none())
            && reliable_pending == 0
        {
            Some(AuthorityClosingCompletion::Acknowledged)
        } else {
            None
        };
        if let Some(completion) = completion {
            return self.complete_closing(index, completion);
        }

        if pending_disconnect.is_some_and(|pending| pending.send.is_none()) {
            let pending = pending_disconnect.expect("checked terminal Disconnect");
            let retry = self.peers[index]
                .as_mut()
                .expect("closing peer index")
                .runtime
                .queue_tracked_disconnect(pending.message);
            if let Ok(send) = retry {
                self.peers[index]
                    .as_mut()
                    .expect("closing peer index")
                    .pending_disconnect
                    .as_mut()
                    .expect("Closing retains its terminal Disconnect")
                    .send = Some(send);
                self.metrics.typed_disconnects_queued =
                    self.metrics.typed_disconnects_queued.saturating_add(1);
            }
        }
        Ok(false)
    }

    fn service_post_result_outbound(
        &mut self,
        index: usize,
    ) -> Result<(), AuthorityPeerHubError<S::Error>> {
        let Some(result) = self.peers[index]
            .as_ref()
            .and_then(|link| link.pending_result)
        else {
            return Ok(());
        };
        match self.peers[index]
            .as_mut()
            .expect("post-result Closing peer")
            .runtime
            .queue_message(WireMessage::ResultIdentifier(result))
        {
            Ok(_) => {
                let link = self.peers[index]
                    .as_mut()
                    .expect("post-result Closing peer");
                link.pending_result = None;
                link.queued_result = Some(result);
                self.metrics.results_queued = self.metrics.results_queued.saturating_add(1);
            }
            Err(RuntimeQueueError::OutboundQueueFull) => {
                self.metrics.results_deferred = self.metrics.results_deferred.saturating_add(1);
            }
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    fn complete_closing(
        &mut self,
        index: usize,
        completion: AuthorityClosingCompletion,
    ) -> Result<bool, AuthorityPeerHubError<S::Error>> {
        if usize::from(self.drain_event_len) == MAX_AUTHORITY_PEERS {
            return Ok(false);
        }
        let link = self.peers[index].as_ref().expect("closing peer index");
        let event = AuthorityPeerDrainEvent {
            peer_id: link.peer_id,
            user_id: link.user_id,
            connection: link.connection,
            disconnect: link.pending_disconnect.map(|pending| pending.message),
            completion,
        };
        let queue_index = (usize::from(self.drain_event_head) + usize::from(self.drain_event_len))
            % MAX_AUTHORITY_PEERS;
        self.drain_events[queue_index] = Some(event);
        self.drain_event_len = self.drain_event_len.saturating_add(1);

        if event.disconnect.is_some() {
            match completion {
                AuthorityClosingCompletion::Acknowledged => {
                    self.metrics.typed_disconnects_acknowledged = self
                        .metrics
                        .typed_disconnects_acknowledged
                        .saturating_add(1);
                }
                AuthorityClosingCompletion::TimedOut => {
                    self.metrics.typed_disconnects_timed_out =
                        self.metrics.typed_disconnects_timed_out.saturating_add(1);
                }
                AuthorityClosingCompletion::TransportClosed => {
                    self.metrics.typed_disconnects_transport_closed = self
                        .metrics
                        .typed_disconnects_transport_closed
                        .saturating_add(1);
                }
            }
        }
        self.retire_physical_connection(event.connection)?;
        Ok(true)
    }

    /// Advances at most one canonical simulation tick. The caller supplies the
    /// deterministic authority bot policy used after the neutral reconnect
    /// interval. The returned frame must name exactly the requested seat/tick.
    pub fn try_advance(
        &mut self,
        mut disconnected_bot: impl FnMut(PeerId, SeatId, SimTick) -> InputFrame,
    ) -> Result<
        (AuthorityAdvanceOutcome, Option<AuthorityTickReport>),
        AuthorityPeerHubError<S::Error>,
    > {
        if self.shutdown != AuthorityShutdownState::Running {
            return Ok((AuthorityAdvanceOutcome::Finished, None));
        }
        let Some(countdown_start_tick) = self.countdown_start_tick else {
            self.metrics.startup_ticks_blocked =
                self.metrics.startup_ticks_blocked.saturating_add(1);
            return Ok((AuthorityAdvanceOutcome::WaitingForReady, None));
        };
        if self.network_tick < countdown_start_tick {
            self.metrics.startup_ticks_blocked =
                self.metrics.startup_ticks_blocked.saturating_add(1);
            return Ok((AuthorityAdvanceOutcome::WaitingForStartTick, None));
        }
        if self.result.is_some() {
            return Ok((AuthorityAdvanceOutcome::Finished, None));
        }

        let tick = self.authority.simulation().current_tick().next();
        if self.metrics.authority_ticks == 0 {
            let waiting_for_first_peer_input =
                self.manifest.ownership.as_slice().iter().any(|assignment| {
                    let SeatOwner::Peer(peer_id) = assignment.owner else {
                        return false;
                    };
                    let connected = self.peer_index(peer_id).is_some_and(|index| {
                        self.peers[index]
                            .as_ref()
                            .is_some_and(|peer| peer.phase.accepts_live_input())
                    });
                    connected && !self.authority.has_buffered_input(assignment.seat, tick)
                });
            if waiting_for_first_peer_input {
                // Countdown is the bounded grace period: clients prefill their
                // negotiated input lead before this shared boundary. Once the
                // boundary arrives, canonical time must not drift behind the
                // session clock because one peer withheld input; the normal
                // deterministic missing-input policy commits tick one instead.
                self.metrics.startup_input_deadlines =
                    self.metrics.startup_input_deadlines.saturating_add(1);
            }
        }
        let mut overrides = [AuthoritySeatCommitOverride::Normal; MAX_SEATS];
        for assignment_index in 0..self.manifest.ownership.len() {
            let assignment = self.manifest.ownership.as_slice()[assignment_index];
            let SeatOwner::Peer(peer_id) = assignment.owner else {
                continue;
            };
            let link_accepts_input = self.peer_index(peer_id).is_some_and(|index| {
                self.peers[index].as_ref().is_some_and(|peer| {
                    peer.phase.accepts_live_input()
                        && peer.resume_input_tick.is_none_or(|resume| tick >= resume)
                })
            });
            if link_accepts_input {
                continue;
            }
            overrides[usize::from(assignment.seat.get())] =
                match self.resolve_substitute_control(peer_id, tick)? {
                    SubstituteControl::Connected | SubstituteControl::NeutralInput => {
                        AuthoritySeatCommitOverride::ForceNeutral
                    }
                    SubstituteControl::BotTakeover | SubstituteControl::PermanentBotReplacement => {
                        AuthoritySeatCommitOverride::DisconnectedBot {
                            peer_id,
                            frame: disconnected_bot(peer_id, assignment.seat, tick),
                        }
                    }
                };
        }

        let report = self
            .authority
            .step_with_overrides(&overrides)
            .map_err(AuthorityPeerHubError::Authority)?;
        self.metrics.authority_ticks = self.metrics.authority_ticks.saturating_add(1);
        let substituted = report
            .committed_inputs
            .iter()
            .filter(|record| record.was_substituted())
            .count() as u64;
        let counters = self.observability.counters_mut();
        counters.inputs_substituted = counters.inputs_substituted.saturating_add(substituted);
        let snapshot = self
            .authority
            .snapshot_at(report.tick)
            .expect("the authority retains the snapshot it just reported")
            .clone();
        self.state_history.record_snapshot(&snapshot)?;
        self.observability.counters_mut().history_high_water = self
            .observability
            .counters()
            .history_high_water
            .max(u32::try_from(self.state_history.len()).unwrap_or(u32::MAX));
        self.replicate_tick(&report, snapshot)?;
        Ok((AuthorityAdvanceOutcome::Advanced, Some(report)))
    }

    fn attach_link(
        &mut self,
        peer_id: PeerId,
        user_id: AuthenticatedUserId,
        endpoint: E,
        phase: AuthorityPeerPhase,
    ) -> Result<AuthorityConnectionId, AuthorityPeerHubError<S::Error>> {
        if self.peer_index(peer_id).is_some() {
            return Err(AuthorityPeerHubError::DuplicatePeer(peer_id));
        }
        let Some(slot) = self.peers.iter().position(Option::is_none) else {
            return Err(AuthorityPeerHubError::Capacity);
        };
        let generation = self.slot_generations[slot]
            .checked_add(1)
            .filter(|generation| *generation != 0)
            .ok_or(AuthorityPeerHubError::TimelineExhausted)?;
        self.slot_generations[slot] = generation;
        let connection = AuthorityConnectionId {
            slot: slot as u8,
            generation,
        };
        let link = AuthorityPeerLink::new(
            peer_id,
            user_id,
            connection,
            phase,
            endpoint,
            self.manifest.compatibility,
            self.config.runtime,
            self.config.security,
            self.network_tick,
        )
        .map_err(|error| match error {
            AuthorityPeerLinkError::Runtime(error) => AuthorityPeerHubError::RuntimeConfig(error),
            AuthorityPeerLinkError::Security(error) => AuthorityPeerHubError::Security(error),
        })?;
        self.peer_state.connect_peer(peer_id)?;
        self.peers[slot] = Some(link);
        self.metrics.connections_attached = self.metrics.connections_attached.saturating_add(1);
        self.record_audit(
            OnlineAuditScope {
                match_id: Some(self.manifest.match_id),
                peer_id: Some(peer_id),
                tick: Some(self.network_tick),
                ..OnlineAuditScope::default()
            },
            OnlineAuditCode::PeerAuthenticated,
            u64::from(connection.generation),
            u64::from(connection.slot),
        );
        Ok(connection)
    }

    fn validate_admission(
        &mut self,
        peer_id: PeerId,
        user_id: AuthenticatedUserId,
    ) -> Result<(), AuthorityPeerHubError<S::Error>> {
        let Some(expected) = self.expected[..self.expected_count]
            .iter()
            .flatten()
            .find(|expected| expected.peer_id == peer_id)
        else {
            self.record_authentication_rejection(peer_id, SecurityViolation::SpoofedIdentity);
            return Err(AuthorityPeerHubError::UnknownPeer(peer_id));
        };
        if expected.user_id != user_id {
            self.record_authentication_rejection(peer_id, SecurityViolation::SpoofedIdentity);
            return Err(AuthorityPeerHubError::IdentityMismatch(peer_id));
        }
        if self.bans.lookup(user_id, self.network_tick).is_some() {
            self.metrics.authentication_rejections =
                self.metrics.authentication_rejections.saturating_add(1);
            self.metrics.active_ban_rejections =
                self.metrics.active_ban_rejections.saturating_add(1);
            self.record_audit(
                OnlineAuditScope {
                    match_id: Some(self.manifest.match_id),
                    peer_id: Some(peer_id),
                    tick: Some(self.network_tick),
                    ..OnlineAuditScope::default()
                },
                OnlineAuditCode::PeerAuthenticationRejected,
                u64::from(SecurityViolation::PlatformBan.detail_code()),
                1,
            );
            return Err(AuthorityPeerHubError::ActiveBan);
        }
        Ok(())
    }

    fn peer_index(&self, peer_id: PeerId) -> Option<usize> {
        self.peers
            .iter()
            .position(|slot| slot.as_ref().is_some_and(|peer| peer.peer_id == peer_id))
    }

    fn expected_peer_index(&self, peer_id: PeerId) -> Option<usize> {
        self.expected[..self.expected_count]
            .iter()
            .position(|slot| slot.is_some_and(|peer| peer.peer_id == peer_id))
    }

    fn handle_event(
        &mut self,
        index: usize,
        event: RuntimeEvent,
    ) -> Result<bool, AuthorityPeerHubError<S::Error>> {
        let message = match event {
            RuntimeEvent::Message(message) => message,
            RuntimeEvent::SessionError(_) => {
                self.observe_peer_violation(
                    index,
                    SecurityViolation::InvalidSessionTransition,
                    true,
                )?;
                return Ok(false);
            }
            RuntimeEvent::TransportDisconnected => {
                if self.result.is_some() {
                    self.metrics.post_result_transport_closures = self
                        .metrics
                        .post_result_transport_closures
                        .saturating_add(1);
                    let connection = self.peers[index]
                        .as_ref()
                        .expect("live peer index")
                        .connection;
                    self.detach(connection)?;
                } else {
                    self.reject_slot(index)?;
                }
                return Ok(false);
            }
        };
        let connected_peer = self.peers[index].as_ref().expect("live peer index").peer_id;
        let spoofed_identity = message_claimed_peer(&message)
            .is_some_and(|claimed_peer| claimed_peer != connected_peer);
        let accepted = match message {
            WireMessage::Handshake(handshake) => self.handle_handshake(index, handshake)?,
            WireMessage::Start(message) => self.handle_start(index, message)?,
            WireMessage::InputBatch(batch) => return self.handle_input(index, batch),
            WireMessage::ResyncRequest(request) => self.handle_resync_request(index, request)?,
            WireMessage::ResyncApplied(applied) => self.handle_resync_applied(index, applied)?,
            WireMessage::ClockProbe(probe) => self.handle_clock_probe(index, probe)?,
            WireMessage::Disconnect(_) => {
                self.reject_slot(index)?;
                return Ok(false);
            }
            _ => {
                self.observe_peer_violation(index, SecurityViolation::WrongDirection, true)?;
                return Ok(false);
            }
        };
        if !accepted {
            self.metrics.spoofed_messages = self.metrics.spoofed_messages.saturating_add(1);
            self.observe_peer_violation(
                index,
                if spoofed_identity {
                    SecurityViolation::SpoofedIdentity
                } else {
                    SecurityViolation::InvalidSessionTransition
                },
                true,
            )?;
        }
        Ok(accepted)
    }

    fn handle_handshake(
        &mut self,
        index: usize,
        handshake: Handshake,
    ) -> Result<bool, AuthorityPeerHubError<S::Error>> {
        if handshake.compatibility != self.manifest.compatibility {
            return Ok(false);
        }
        let link = self.peers[index].as_mut().expect("live peer index");
        match link.phase {
            AuthorityPeerPhase::AwaitingHandshake => {
                link.runtime
                    .queue_start_message(StartMessage::Manifest(self.manifest))?;
                link.phase = AuthorityPeerPhase::AwaitingManifestAcceptance;
                Ok(true)
            }
            AuthorityPeerPhase::ReconnectHandshake => {
                link.phase = AuthorityPeerPhase::ReconnectSyncInFlight;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn handle_start(
        &mut self,
        index: usize,
        message: StartMessage,
    ) -> Result<bool, AuthorityPeerHubError<S::Error>> {
        let link = self.peers[index].as_mut().expect("live peer index");
        match (link.phase, message) {
            (
                AuthorityPeerPhase::AwaitingManifestAcceptance,
                StartMessage::ManifestAccepted {
                    match_id,
                    peer_id,
                    manifest_hash,
                },
            ) if match_id == self.manifest.match_id
                && peer_id == link.peer_id
                && manifest_hash == self.manifest.manifest_hash =>
            {
                link.phase = AuthorityPeerPhase::AwaitingInitialResyncRequest;
                Ok(true)
            }
            (
                AuthorityPeerPhase::AwaitingInitialSyncDeclaration,
                StartMessage::InitialSyncApplied {
                    match_id,
                    peer_id,
                    snapshot_tick,
                    snapshot_hash,
                },
            ) if link.applied_sync.is_some_and(|applied| {
                match_id == applied.match_id
                    && peer_id == applied.peer_id
                    && snapshot_tick == applied.snapshot_tick
                    && snapshot_hash == applied.snapshot_hash
            }) =>
            {
                link.phase = AuthorityPeerPhase::AwaitingReady;
                Ok(true)
            }
            (AuthorityPeerPhase::AwaitingReady, StartMessage::Ready { match_id, peer_id })
                if match_id == self.manifest.match_id && peer_id == link.peer_id =>
            {
                if !link.clock_synchronized() {
                    return Ok(false);
                }
                link.phase = AuthorityPeerPhase::Ready;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn handle_clock_probe(
        &mut self,
        index: usize,
        probe: ClockProbe,
    ) -> Result<bool, AuthorityPeerHubError<S::Error>> {
        let link = self.peers[index].as_mut().expect("live peer index");
        if probe.match_id != self.manifest.match_id
            || probe.peer_id != link.peer_id
            || !matches!(
                link.phase,
                AuthorityPeerPhase::AwaitingInitialResyncRequest
                    | AuthorityPeerPhase::InitialSyncInFlight
                    | AuthorityPeerPhase::AwaitingInitialSyncDeclaration
                    | AuthorityPeerPhase::AwaitingReady
                    | AuthorityPeerPhase::Ready
                    | AuthorityPeerPhase::Countdown
                    | AuthorityPeerPhase::Fighting
                    | AuthorityPeerPhase::ReconnectSyncInFlight
                    | AuthorityPeerPhase::ReconnectAwaitingClock
                    | AuthorityPeerPhase::RepairSyncInFlight
            )
        {
            return Ok(false);
        }
        if link.has_clock_probe(probe.probe_id) {
            return Ok(true);
        }
        let reply = ClockReply {
            match_id: probe.match_id,
            peer_id: link.peer_id,
            probe_id: probe.probe_id,
            authority_tick: self.network_tick,
        };
        if link.phase == AuthorityPeerPhase::ReconnectSyncInFlight {
            // Control and Resync are independent reliable channels. A probe sent
            // after ResyncApplied may arrive first, so retain it without replying.
            // Releasing the reply only after the applied acknowledgement creates
            // the causal admission signal the replacement client waits for.
            let Some(slot) = link
                .pending_clock_replies
                .iter_mut()
                .find(|slot| slot.is_none())
            else {
                return Ok(false);
            };
            *slot = Some(reply);
            return Ok(true);
        }
        match link.runtime.queue_message(WireMessage::ClockReply(reply)) {
            Ok(_) => {
                Self::record_clock_sample(link, probe.probe_id);
                self.maybe_complete_reclaim(index)?;
                Ok(true)
            }
            Err(RuntimeQueueError::OutboundQueueFull) => {
                let Some(slot) = link
                    .pending_clock_replies
                    .iter_mut()
                    .find(|slot| slot.is_none())
                else {
                    return Ok(false);
                };
                *slot = Some(reply);
                Ok(true)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn record_clock_sample(link: &mut AuthorityPeerLink<E>, probe_id: ClockProbeId) {
        if link
            .clock_samples
            .iter()
            .flatten()
            .any(|id| *id == probe_id)
        {
            return;
        }
        let count = usize::from(link.clock_sample_count);
        if count < CLOCK_SYNC_SAMPLE_CAPACITY {
            link.clock_samples[count] = Some(probe_id);
            link.clock_sample_count = link.clock_sample_count.saturating_add(1);
        }
    }

    fn handle_input(
        &mut self,
        index: usize,
        batch: crate::network_protocol::InputBatch,
    ) -> Result<bool, AuthorityPeerHubError<S::Error>> {
        let (peer_id, accepts) = {
            let link = self.peers[index].as_ref().expect("live peer index");
            (
                link.peer_id,
                link.phase.accepts_live_input()
                    && link.resume_input_tick.is_none_or(|resume| {
                        batch
                            .as_slice()
                            .iter()
                            .all(|window| window.newest().is_some_and(|frame| frame.tick >= resume))
                    }),
            )
        };
        if !accepts {
            self.metrics.input_batches_rejected =
                self.metrics.input_batches_rejected.saturating_add(1);
            self.observability.counters_mut().inputs_rejected = self
                .observability
                .counters()
                .inputs_rejected
                .saturating_add(1);
            self.observe_peer_violation(index, SecurityViolation::InvalidSessionTransition, true)?;
            return Ok(false);
        }
        let report = match self.authority.ingest_peer_batch(peer_id, &batch) {
            Ok(report) => report,
            Err(error) => {
                if error == ProtocolValidationError::PeerMismatch {
                    self.metrics.spoofed_messages = self.metrics.spoofed_messages.saturating_add(1);
                }
                self.metrics.input_batches_rejected =
                    self.metrics.input_batches_rejected.saturating_add(1);
                self.observability.counters_mut().inputs_rejected = self
                    .observability
                    .counters()
                    .inputs_rejected
                    .saturating_add(1);
                self.observe_peer_violation(index, input_protocol_violation(error), true)?;
                return Ok(false);
            }
        };
        if self
            .peer_state
            .observe_validated_input_batch(peer_id, &batch, &self.state_history)
            .is_err()
        {
            self.metrics.input_batches_rejected =
                self.metrics.input_batches_rejected.saturating_add(1);
            self.observability.counters_mut().inputs_rejected = self
                .observability
                .counters()
                .inputs_rejected
                .saturating_add(1);
            self.observe_peer_violation(index, SecurityViolation::InvalidInput, true)?;
            return Ok(false);
        }
        self.metrics.input_batches_accepted = self.metrics.input_batches_accepted.saturating_add(1);
        let counters = self.observability.counters_mut();
        counters.inputs_accepted = counters
            .inputs_accepted
            .saturating_add(u64::from(report.accepted));
        counters.inputs_rejected = counters
            .inputs_rejected
            .saturating_add(u64::from(report.rejected));

        // Stale, committed-late, and duplicate frames are expected under delay,
        // redundancy, rollback correction, and retransmission. They remain
        // observable rejections but must not accrue security score. Only values
        // that violate the bounded input contract or seat ownership are abuse.
        let security_rejections = report
            .rejections
            .invalid
            .saturating_add(report.rejections.unowned)
            .saturating_add(report.rejections.future)
            .saturating_add(report.rejections.sequence)
            .saturating_add(report.rejections.conflicting)
            .saturating_add(report.rejections.capacity);
        if security_rejections > 0 {
            let violation = if report.rejections.unowned > 0 {
                SecurityViolation::InvalidSeatOwnership
            } else {
                SecurityViolation::InvalidInput
            };
            self.record_audit(
                OnlineAuditScope {
                    match_id: Some(self.manifest.match_id),
                    peer_id: Some(peer_id),
                    tick: Some(self.authority.simulation().current_tick()),
                    ..OnlineAuditScope::default()
                },
                OnlineAuditCode::InputRejected,
                u64::from(security_rejections),
                u64::from(violation.detail_code()),
            );
            if self.observe_peer_violation(index, violation, false)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn handle_resync_request(
        &mut self,
        index: usize,
        request: ResyncRequest,
    ) -> Result<bool, AuthorityPeerHubError<S::Error>> {
        if request.validate().is_err() {
            return Ok(false);
        }
        let (peer_id, phase) = {
            let link = self.peers[index].as_ref().expect("live peer index");
            (link.peer_id, link.phase)
        };
        if request.match_id != self.manifest.match_id || request.peer_id != peer_id {
            return Ok(false);
        }
        let current_snapshot_tick = self.authority.simulation().current_tick();
        if request.last_confirmed_tick > current_snapshot_tick {
            // This field is peer-authored. Passing a future value into
            // AuthorityResyncTransfer would surface RequestAheadOfSnapshot as
            // a hub-global invariant error. Classify it at the trust boundary
            // instead so handle_event records the violation and detaches only
            // the attributable peer.
            return Ok(false);
        }
        if phase == AuthorityPeerPhase::RepairSyncInFlight
            && matches!(
                request.reason,
                ResyncReason::HashMismatch | ResyncReason::HistoryExpired
            )
        {
            let matches_active_repair = self.peers[index]
                .as_ref()
                .and_then(|link| link.transfer.as_ref())
                .is_some_and(|pending| {
                    pending.purpose == TransferPurpose::Repair
                        && request.last_confirmed_tick <= pending.transfer.begin().snapshot_tick
                });
            if matches_active_repair {
                // Reliable request retries can cross the active repair
                // declaration. The snapshot already satisfies the same client
                // fence, so this is an idempotent no-op rather than a phase
                // attack.
                self.metrics.repair_requests_coalesced =
                    self.metrics.repair_requests_coalesced.saturating_add(1);
                return Ok(true);
            }
            return Ok(false);
        }
        let purpose = match (phase, request.reason) {
            (AuthorityPeerPhase::AwaitingInitialResyncRequest, ResyncReason::InitialSync) => {
                TransferPurpose::Initial
            }
            (
                AuthorityPeerPhase::Fighting,
                ResyncReason::HashMismatch | ResyncReason::HistoryExpired,
            ) => TransferPurpose::Repair,
            _ => return Ok(false),
        };
        if purpose == TransferPurpose::Repair {
            let budget_index = self
                .expected_peer_index(peer_id)
                .expect("a live peer is always part of the authenticated roster");
            match self.repair_request_budgets[budget_index].try_start(
                self.network_tick,
                self.config.peer_repair_request_cooldown_ticks,
                self.config.peer_repair_request_window_ticks,
                self.config.max_peer_repair_requests_per_window,
            ) {
                PeerRepairBudgetOutcome::Allowed => {}
                PeerRepairBudgetOutcome::Cooldown => {
                    self.metrics.repair_requests_rate_limited =
                        self.metrics.repair_requests_rate_limited.saturating_add(1);
                    // A valid request inside the cooldown is a bounded,
                    // attributable rate violation, not a session-state error.
                    // Score it explicitly and consume the message here so the
                    // generic dispatcher cannot turn the first duplicate into
                    // an immediate forced disconnect.
                    self.observe_peer_violation(
                        index,
                        SecurityViolation::ReceiveBudgetFlood,
                        false,
                    )?;
                    return Ok(true);
                }
                PeerRepairBudgetOutcome::Exhausted => {
                    self.metrics.repair_request_budgets_exhausted = self
                        .metrics
                        .repair_request_budgets_exhausted
                        .saturating_add(1);
                    // Repeated requests beyond the fixed window budget accrue
                    // normal policy score and eventually isolate only this
                    // peer. The request itself has already been handled.
                    self.observe_peer_violation(
                        index,
                        SecurityViolation::ReceiveBudgetFlood,
                        false,
                    )?;
                    return Ok(true);
                }
            }
        }
        let tick = current_snapshot_tick;
        let snapshot = self
            .authority
            .snapshot_at(tick)
            .expect("the current authority snapshot is retained")
            .clone();
        self.prepare_transfer_for_peer(peer_id, request, snapshot, purpose)?;
        self.peers[index].as_mut().expect("live peer index").phase = match purpose {
            TransferPurpose::Initial => AuthorityPeerPhase::InitialSyncInFlight,
            TransferPurpose::Repair => AuthorityPeerPhase::RepairSyncInFlight,
            TransferPurpose::Reconnect(_) => unreachable!(),
        };
        Ok(true)
    }

    fn handle_resync_applied(
        &mut self,
        index: usize,
        applied: ResyncApplied,
    ) -> Result<bool, AuthorityPeerHubError<S::Error>> {
        let (peer_id, purpose) = {
            let link = self.peers[index].as_mut().expect("live peer index");
            let Some(pending) = link.transfer.as_mut() else {
                return Ok(false);
            };
            if pending.stage != TransferStage::WaitingApplied {
                return Ok(false);
            }
            if pending.transfer.validate_applied(&applied).is_err() {
                return Ok(false);
            }
            (link.peer_id, pending.purpose)
        };
        self.peer_state
            .observe_validated_resync_applied(peer_id, &applied)?;
        let link = self.peers[index].as_mut().expect("live peer index");
        link.transfer = None;
        link.applied_sync = Some(applied);
        match purpose {
            TransferPurpose::Initial => {
                link.phase = AuthorityPeerPhase::AwaitingInitialSyncDeclaration;
            }
            TransferPurpose::Repair => {
                link.phase = AuthorityPeerPhase::Fighting;
                if let Some(result) = self.result {
                    link.pending_result = Some(result);
                }
            }
            TransferPurpose::Reconnect(reservation) => {
                link.pending_reclaim_completion = Some((reservation, applied));
                link.phase = AuthorityPeerPhase::ReconnectAwaitingClock;
            }
        }
        self.metrics.resyncs_applied = self.metrics.resyncs_applied.saturating_add(1);
        self.record_audit(
            OnlineAuditScope {
                match_id: Some(self.manifest.match_id),
                peer_id: Some(peer_id),
                tick: Some(applied.snapshot_tick),
                ..OnlineAuditScope::default()
            },
            OnlineAuditCode::HardResyncApplied,
            u64::from(applied.transfer_id.get()),
            0,
        );
        self.maybe_complete_reclaim(index)?;
        Ok(true)
    }

    fn maybe_complete_reclaim(
        &mut self,
        index: usize,
    ) -> Result<(), AuthorityPeerHubError<S::Error>> {
        let completion = self.peers[index].as_ref().and_then(|link| {
            link.clock_synchronized()
                .then_some(link.pending_reclaim_completion)
                .flatten()
                .map(|pending| (link.peer_id, pending))
        });
        let Some((peer_id, (reservation, applied))) = completion else {
            return Ok(());
        };
        let authority_tick = self.authority.simulation().current_tick();
        self.resolve_substitute_control(peer_id, authority_tick)?;
        let admission = match self.reconnect.complete_reclaim(
            peer_id,
            reservation.attempt_id,
            applied.snapshot_tick,
            authority_tick,
        ) {
            Ok(admission) => admission,
            Err(ReconnectError::GraceExpired) => {
                self.reject_slot(index)?;
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        self.authority
            .begin_peer_input_epoch(peer_id, admission.resume_input_tick)?;
        let link = self.peers[index].as_mut().expect("live peer index");
        link.pending_reclaim_completion = None;
        link.resume_input_tick = Some(admission.resume_input_tick);
        link.phase = AuthorityPeerPhase::Fighting;
        if let Some(result) = self.result {
            link.pending_result = Some(result);
        }
        self.metrics.reconnects_completed = self.metrics.reconnects_completed.saturating_add(1);
        self.observability.counters_mut().reconnects =
            self.observability.counters().reconnects.saturating_add(1);
        self.record_audit(
            OnlineAuditScope {
                match_id: Some(self.manifest.match_id),
                peer_id: Some(peer_id),
                tick: Some(applied.snapshot_tick),
                ..OnlineAuditScope::default()
            },
            OnlineAuditCode::PeerReconnected,
            u64::from(reservation.attempt_id.get()),
            admission.resume_input_tick.get(),
        );
        Ok(())
    }

    fn resolve_substitute_control(
        &mut self,
        peer_id: PeerId,
        tick: SimTick,
    ) -> Result<SubstituteControl, AuthorityPeerHubError<S::Error>> {
        let resolution = self.reconnect.advance_substitute_control(peer_id, tick)?;
        if let Some(replacement) = resolution.permanent_bot_replacement {
            let grace_ticks = self.config.reconnect.grace_ticks;
            self.metrics.reconnect_grace_expirations =
                self.metrics.reconnect_grace_expirations.saturating_add(1);
            self.record_audit(
                OnlineAuditScope {
                    match_id: Some(self.manifest.match_id),
                    peer_id: Some(replacement.peer_id),
                    tick: Some(replacement.effective_tick),
                    ..OnlineAuditScope::default()
                },
                OnlineAuditCode::ReconnectGraceExpired,
                u64::from(replacement.seat_mask),
                u64::from(grace_ticks),
            );
        }
        Ok(resolution.control)
    }

    fn prepare_transfer_for_peer(
        &mut self,
        peer_id: PeerId,
        request: ResyncRequest,
        snapshot: CanonicalSnapshot,
        purpose: TransferPurpose,
    ) -> Result<(), AuthorityPeerHubError<S::Error>> {
        let index = self
            .peer_index(peer_id)
            .ok_or(AuthorityPeerHubError::UnknownPeer(peer_id))?;
        let transfer_id = self.peers[index]
            .as_mut()
            .expect("live peer index")
            .allocate_transfer_id()?;
        let tick = snapshot.header.tick;
        let mut windows = [CommittedSeatInputWindow::default(); MAX_SEATS];
        let count = self.manifest.ownership.len();
        if tick == SimTick::ZERO {
            for (index, assignment) in self.manifest.ownership.as_slice().iter().enumerate() {
                windows[index] =
                    CommittedSeatInputWindow::from_newest_first(&[CommittedInputRecord {
                        frame: InputFrame {
                            tick,
                            seat: assignment.seat,
                            ..InputFrame::default()
                        },
                        fighter: assignment.fighter,
                        source: CommittedInputSource::MissingSubstitute,
                    }])?;
            }
        } else {
            let tail_len = self
                .manifest
                .ownership
                .as_slice()
                .iter()
                .map(|assignment| {
                    self.committed_histories[usize::from(assignment.seat.get())].len()
                })
                .min()
                .unwrap_or(0)
                .min(MAX_RESYNC_INPUT_TAIL_TICKS);
            if tail_len == 0 {
                return Err(ProtocolValidationError::InvalidTickWindow.into());
            }
            for (index, assignment) in self.manifest.ownership.as_slice().iter().enumerate() {
                let window = self.committed_histories[usize::from(assignment.seat.get())]
                    .window_with_len(tail_len)?;
                if window
                    .newest()
                    .is_none_or(|record| record.frame.tick != tick)
                {
                    return Err(ProtocolValidationError::InvalidTickWindow.into());
                }
                windows[index] = window;
            }
        }
        // Snapshot bytes and the exact committed input suffix are copied into one
        // immutable transfer before authority simulation can advance again.
        let transfer = AuthorityResyncTransfer::from_snapshot(
            request,
            transfer_id,
            &snapshot,
            &windows[..count],
        )?;
        self.peers[index]
            .as_mut()
            .expect("live peer index")
            .transfer = Some(PendingTransfer {
            transfer,
            purpose,
            stage: TransferStage::Begin,
            next_chunk: 0,
        });
        self.metrics.resyncs_started = self.metrics.resyncs_started.saturating_add(1);
        self.observability.counters_mut().hard_resyncs =
            self.observability.counters().hard_resyncs.saturating_add(1);
        self.record_audit(
            OnlineAuditScope {
                match_id: Some(self.manifest.match_id),
                peer_id: Some(peer_id),
                tick: Some(tick),
                ..OnlineAuditScope::default()
            },
            OnlineAuditCode::HardResyncStarted,
            u64::from(transfer_id.get()),
            transfer_purpose_code(purpose),
        );
        Ok(())
    }

    fn service_peer_outbound(
        &mut self,
        index: usize,
    ) -> Result<(), AuthorityPeerHubError<S::Error>> {
        if self.peers[index]
            .as_ref()
            .is_some_and(|link| link.phase == AuthorityPeerPhase::Closing)
        {
            return Ok(());
        }
        let clock_progress = {
            let link = self.peers[index].as_mut().expect("live peer index");
            if link.phase != AuthorityPeerPhase::ReconnectSyncInFlight
                && let Some(reply) = link.pending_clock_replies[0]
            {
                match link.runtime.queue_message(WireMessage::ClockReply(reply)) {
                    Ok(_) => {
                        for offset in 1..CLOCK_SYNC_SAMPLE_CAPACITY {
                            link.pending_clock_replies[offset - 1] =
                                link.pending_clock_replies[offset];
                        }
                        link.pending_clock_replies[CLOCK_SYNC_SAMPLE_CAPACITY - 1] = None;
                        Self::record_clock_sample(link, reply.probe_id);
                        true
                    }
                    Err(RuntimeQueueError::OutboundQueueFull) => false,
                    Err(error) => return Err(error.into()),
                }
            } else {
                false
            }
        };
        if clock_progress {
            self.maybe_complete_reclaim(index)?;
        }
        let link = self.peers[index].as_mut().expect("live peer index");
        if let Some(result) = link.pending_result {
            match link
                .runtime
                .queue_message(WireMessage::ResultIdentifier(result))
            {
                Ok(_) => {
                    link.pending_result = None;
                    link.queued_result = Some(result);
                    self.metrics.results_queued = self.metrics.results_queued.saturating_add(1);
                }
                Err(RuntimeQueueError::OutboundQueueFull) => {
                    self.metrics.results_deferred = self.metrics.results_deferred.saturating_add(1);
                }
                Err(error) => return Err(error.into()),
            }
        }

        loop {
            if link.runtime.outbound_len() >= self.config.runtime.outbound_capacity {
                break;
            }
            let Some(pending) = link.transfer.as_mut() else {
                break;
            };
            let message = match pending.stage {
                TransferStage::Begin => {
                    pending.stage = TransferStage::InputTail;
                    WireMessage::ResyncBegin(pending.transfer.begin())
                }
                TransferStage::InputTail => {
                    pending.stage = TransferStage::Chunks;
                    WireMessage::ResyncInputTail(pending.transfer.input_tail())
                }
                TransferStage::Chunks => {
                    let Some(chunk) = pending.transfer.chunks_from(pending.next_chunk)?.next()
                    else {
                        pending.stage = TransferStage::WaitingApplied;
                        break;
                    };
                    pending.next_chunk += 1;
                    if pending.next_chunk == pending.transfer.begin().chunk_count {
                        pending.stage = TransferStage::WaitingApplied;
                    }
                    WireMessage::ResyncChunk(chunk)
                }
                TransferStage::WaitingApplied => break,
            };
            match link.runtime.queue_message(message) {
                Ok(_) => {}
                Err(RuntimeQueueError::OutboundQueueFull) => break,
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn maybe_start_countdown(&mut self) -> Result<(), AuthorityPeerHubError<S::Error>> {
        if self.countdown_start_tick.is_some() {
            return Ok(());
        }
        let all_ready = self.expected[..self.expected_count]
            .iter()
            .flatten()
            .all(|expected| {
                self.peer_index(expected.peer_id).is_some_and(|index| {
                    self.peers[index]
                        .as_ref()
                        .is_some_and(|peer| peer.phase == AuthorityPeerPhase::Ready)
                })
            });
        if !all_ready {
            return Ok(());
        }
        let after_lead = SimTick(
            self.network_tick
                .0
                .checked_add(u64::from(self.config.countdown_lead_ticks))
                .ok_or(AuthorityPeerHubError::TimelineExhausted)?,
        );
        let start_tick = self.manifest.agreed_start_tick.max(after_lead);
        let countdown = StartMessage::Countdown {
            match_id: self.manifest.match_id,
            start_tick,
        };
        countdown.validate_against_manifest(&self.manifest)?;
        let can_commit = self.peers.iter().flatten().all(|peer| {
            peer.runtime
                .preflight_message(&WireMessage::Start(countdown))
                .is_ok()
        });
        if !can_commit {
            self.metrics.countdown_broadcasts_deferred =
                self.metrics.countdown_broadcasts_deferred.saturating_add(1);
            return Ok(());
        }

        // Every peer passed an exact, non-mutating runtime preflight above.
        // Runtimes are single-owner values and no queue mutation occurs between
        // these loops, so all messages are installed before any phase/global
        // countdown state is committed.
        for peer in self.peers.iter_mut().flatten() {
            peer.runtime.queue_start_message(countdown)?;
        }
        for peer in self.peers.iter_mut().flatten() {
            peer.phase = AuthorityPeerPhase::Countdown;
        }
        self.countdown_start_tick = Some(start_tick);
        self.record_audit(
            OnlineAuditScope {
                match_id: Some(self.manifest.match_id),
                tick: Some(self.network_tick),
                ..OnlineAuditScope::default()
            },
            OnlineAuditCode::CountdownStarted,
            start_tick.get(),
            u64::try_from(self.expected_count).unwrap_or(u64::MAX),
        );
        Ok(())
    }

    fn record_audit(
        &mut self,
        scope: OnlineAuditScope,
        code: OnlineAuditCode,
        value_a: u64,
        value_b: u64,
    ) {
        // Sequence exhaustion is a diagnostics-only terminal condition after
        // u64::MAX records. It must not perturb canonical match progression.
        let _ = self.observability.audit_mut().push(
            self.audit_monotonic_ms,
            scope,
            code,
            value_a,
            value_b,
        );
    }

    fn record_authentication_rejection(&mut self, peer_id: PeerId, violation: SecurityViolation) {
        self.metrics.authentication_rejections =
            self.metrics.authentication_rejections.saturating_add(1);
        self.metrics.security_violations = self.metrics.security_violations.saturating_add(1);
        self.record_audit(
            OnlineAuditScope {
                match_id: Some(self.manifest.match_id),
                peer_id: Some(peer_id),
                tick: Some(self.network_tick),
                ..OnlineAuditScope::default()
            },
            OnlineAuditCode::PeerAuthenticationRejected,
            u64::from(violation.detail_code()),
            0,
        );
    }

    fn observe_runtime_metrics(&mut self, before: RuntimeMetrics, after: RuntimeMetrics) {
        let counters = self.observability.counters_mut();
        counters.packets_in = counters.packets_in.saturating_add(
            after
                .received_datagrams
                .saturating_sub(before.received_datagrams),
        );
        counters.bytes_in = counters
            .bytes_in
            .saturating_add(after.received_bytes.saturating_sub(before.received_bytes));
        counters.packets_out = counters
            .packets_out
            .saturating_add(after.sent_datagrams.saturating_sub(before.sent_datagrams));
        counters.bytes_out = counters
            .bytes_out
            .saturating_add(after.sent_bytes.saturating_sub(before.sent_bytes));
        self.observability.observe_queue_depth(
            after
                .inbound_high_water
                .max(after.outbound_high_water)
                .max(after.reliable_high_water),
        );
    }

    fn observe_runtime_violations(
        &mut self,
        index: usize,
        before: RuntimeMetrics,
        after: RuntimeMetrics,
        signal: RuntimeAbuseSignal,
    ) -> Result<bool, AuthorityPeerHubError<S::Error>> {
        let queue_overflows = after
            .inbound_queue_overflows
            .saturating_sub(before.inbound_queue_overflows)
            .saturating_add(
                after
                    .ack_queue_overflows
                    .saturating_sub(before.ack_queue_overflows),
            );
        let violations = [
            (
                SecurityViolation::MalformedEnvelope,
                after
                    .malformed_datagrams
                    .saturating_sub(before.malformed_datagrams),
            ),
            (
                SecurityViolation::DecodeRejected,
                after
                    .decode_rejections
                    .saturating_sub(before.decode_rejections),
            ),
            (
                SecurityViolation::WrongDirection,
                after
                    .direction_rejections
                    .saturating_sub(before.direction_rejections),
            ),
            (
                SecurityViolation::ReceiveBudgetFlood,
                after
                    .receive_budget_exhaustions
                    .saturating_sub(before.receive_budget_exhaustions),
            ),
            (SecurityViolation::QueueFlood, queue_overflows),
            (
                SecurityViolation::ReliableWindowAbuse,
                after
                    .reliable_reorder_overflows
                    .saturating_sub(before.reliable_reorder_overflows),
            ),
            (
                SecurityViolation::ConflictingIdempotentMessage,
                after
                    .conflicting_idempotent_messages
                    .saturating_sub(before.conflicting_idempotent_messages),
            ),
        ];
        let mut fallback = SecurityViolation::MalformedEnvelope;
        let mut observed_any = false;
        for (violation, count) in violations {
            if count > 0 {
                fallback = violation;
                observed_any = true;
            }
            // Runtime work itself is capped at 64 datagrams per pump. Keep the
            // score loop explicitly bounded even if a custom endpoint reports a
            // corrupt/saturated metric delta.
            for _ in 0..count.min(64) {
                if self.observe_peer_violation(index, violation, false)? {
                    return Ok(true);
                }
            }
        }

        if signal == RuntimeAbuseSignal::Disconnect {
            // Preserve the runtime's independent fail-closed threshold even
            // when a custom policy uses higher aggregate score thresholds.
            if observed_any {
                self.force_security_disconnect(index, fallback)?;
            } else {
                self.observe_peer_violation(index, fallback, true)?;
            }
            return Ok(true);
        }
        if signal == RuntimeAbuseSignal::Warning && !observed_any {
            // This can occur only when an integration restores runtime metrics
            // from a prior service interval. Retain a stable warning reason.
            let _ =
                self.observe_peer_violation(index, SecurityViolation::ReceiveBudgetFlood, false)?;
        }
        Ok(false)
    }

    fn force_security_disconnect(
        &mut self,
        index: usize,
        violation: SecurityViolation,
    ) -> Result<(), AuthorityPeerHubError<S::Error>> {
        let Some(link) = self.peers[index].as_ref() else {
            return Ok(());
        };
        let peer_id = link.peer_id;
        let score = link.security.score();
        self.metrics.security_kicks = self.metrics.security_kicks.saturating_add(1);
        self.record_audit(
            OnlineAuditScope {
                match_id: Some(self.manifest.match_id),
                peer_id: Some(peer_id),
                tick: Some(self.network_tick),
                ..OnlineAuditScope::default()
            },
            OnlineAuditCode::PeerKicked,
            u64::from(violation.detail_code()),
            u64::from(score),
        );
        self.begin_typed_disconnect(
            index,
            DisconnectMessage {
                match_id: Some(self.manifest.match_id),
                code: violation.disconnect_code(),
                retry: RetryDisposition::ReturnToLobby,
                detail_code: violation.detail_code(),
                last_confirmed_tick: Some(self.authority.simulation().current_tick()),
            },
        )?;
        self.metrics.malformed_or_abusive_disconnects = self
            .metrics
            .malformed_or_abusive_disconnects
            .saturating_add(1);
        Ok(())
    }

    /// Returns true when the peer was detached.
    fn observe_peer_violation(
        &mut self,
        index: usize,
        violation: SecurityViolation,
        force_disconnect: bool,
    ) -> Result<bool, AuthorityPeerHubError<S::Error>> {
        let Some(link) = self.peers[index].as_mut() else {
            return Ok(true);
        };
        let peer_id = link.peer_id;
        let user_id = link.user_id;
        let decision = link.security.observe_violation(
            Some(self.manifest.match_id),
            Some(self.authority.simulation().current_tick()),
            violation,
            self.network_tick,
        )?;
        self.metrics.security_violations = self.metrics.security_violations.saturating_add(1);
        if violation == SecurityViolation::InvalidInput
            || violation == SecurityViolation::InvalidSeatOwnership
        {
            self.record_audit(
                OnlineAuditScope {
                    match_id: Some(self.manifest.match_id),
                    peer_id: Some(peer_id),
                    tick: Some(self.authority.simulation().current_tick()),
                    ..OnlineAuditScope::default()
                },
                OnlineAuditCode::InputRejected,
                u64::from(violation.detail_code()),
                u64::from(decision.accumulated_score),
            );
        }

        if decision.disposition == SecurityDisposition::Warn {
            self.metrics.security_warnings = self.metrics.security_warnings.saturating_add(1);
            self.record_audit(
                OnlineAuditScope {
                    match_id: Some(self.manifest.match_id),
                    peer_id: Some(peer_id),
                    tick: Some(self.network_tick),
                    ..OnlineAuditScope::default()
                },
                OnlineAuditCode::PeerWarned,
                u64::from(violation.detail_code()),
                u64::from(decision.accumulated_score),
            );
        }

        let policy_disconnect = matches!(
            decision.disposition,
            SecurityDisposition::Kick
                | SecurityDisposition::TemporaryBan
                | SecurityDisposition::PlatformBan
        );
        if !force_disconnect && !policy_disconnect {
            return Ok(false);
        }

        let mut ban_error = None;
        match decision.disposition {
            SecurityDisposition::TemporaryBan => {
                self.metrics.temporary_bans = self.metrics.temporary_bans.saturating_add(1);
                if let Err(error) =
                    self.record_security_ban(user_id, violation, false, self.network_tick)
                {
                    ban_error = Some(error);
                }
                self.record_audit(
                    OnlineAuditScope {
                        match_id: Some(self.manifest.match_id),
                        peer_id: Some(peer_id),
                        tick: Some(self.network_tick),
                        ..OnlineAuditScope::default()
                    },
                    OnlineAuditCode::PeerTemporarilyBanned,
                    u64::from(violation.detail_code()),
                    u64::from(decision.accumulated_score),
                );
            }
            SecurityDisposition::PlatformBan => {
                self.metrics.platform_bans = self.metrics.platform_bans.saturating_add(1);
                if let Err(error) =
                    self.record_security_ban(user_id, violation, true, self.network_tick)
                {
                    ban_error = Some(error);
                }
                self.record_audit(
                    OnlineAuditScope {
                        match_id: Some(self.manifest.match_id),
                        peer_id: Some(peer_id),
                        tick: Some(self.network_tick),
                        ..OnlineAuditScope::default()
                    },
                    OnlineAuditCode::PeerAdmissionBanned,
                    u64::from(violation.detail_code()),
                    u64::from(decision.accumulated_score),
                );
            }
            SecurityDisposition::Accept | SecurityDisposition::Warn | SecurityDisposition::Kick => {
            }
        }

        if matches!(
            decision.disposition,
            SecurityDisposition::Accept | SecurityDisposition::Warn | SecurityDisposition::Kick
        ) {
            self.metrics.security_kicks = self.metrics.security_kicks.saturating_add(1);
        }
        self.record_audit(
            OnlineAuditScope {
                match_id: Some(self.manifest.match_id),
                peer_id: Some(peer_id),
                tick: Some(self.network_tick),
                ..OnlineAuditScope::default()
            },
            OnlineAuditCode::PeerKicked,
            u64::from(violation.detail_code()),
            u64::from(decision.accumulated_score),
        );

        let disconnect = decision.disconnect.unwrap_or(DisconnectMessage {
            match_id: Some(self.manifest.match_id),
            code: violation.disconnect_code(),
            retry: RetryDisposition::ReturnToLobby,
            detail_code: violation.detail_code(),
            last_confirmed_tick: Some(self.authority.simulation().current_tick()),
        });
        self.begin_typed_disconnect(index, disconnect)?;
        self.metrics.malformed_or_abusive_disconnects = self
            .metrics
            .malformed_or_abusive_disconnects
            .saturating_add(1);
        if let Some(error) = ban_error {
            return Err(error.into());
        }
        debug_assert!(self.connection_for_peer(peer_id).is_none());
        debug_assert!(self.peer_index(peer_id).is_none_or(|slot| {
            self.peers[slot]
                .as_ref()
                .is_some_and(|peer| peer.phase == AuthorityPeerPhase::Closing)
        }));
        Ok(true)
    }

    fn record_security_ban(
        &mut self,
        user_id: AuthenticatedUserId,
        violation: SecurityViolation,
        permanent: bool,
        now: SimTick,
    ) -> Result<(), SecurityPolicyError> {
        let offenses = self
            .bans
            .lookup(user_id, now)
            .map_or(1, |entry| entry.offenses.saturating_add(1));
        let expires_at = if permanent {
            None
        } else {
            Some(SimTick(
                now.get()
                    .checked_add(self.config.security.temporary_ban_ticks)
                    .ok_or(SecurityPolicyError::TimelineRegression)?,
            ))
        };
        let reason = match violation {
            SecurityViolation::SpoofedIdentity | SecurityViolation::InvalidSeatOwnership => {
                BanReason::SpoofedIdentity
            }
            SecurityViolation::PlatformBan => BanReason::PlatformBan,
            _ => BanReason::RepeatedProtocolAbuse,
        };
        self.bans.record(BanEntry {
            user: user_id,
            reason,
            issued_at: now,
            expires_at,
            offenses,
        })
    }

    fn begin_post_result_close(
        &mut self,
        index: usize,
    ) -> Result<(), AuthorityPeerHubError<S::Error>> {
        let Some(link) = self.peers[index].as_ref() else {
            return Ok(());
        };
        if link.phase == AuthorityPeerPhase::Closing {
            return Ok(());
        }
        debug_assert!(
            self.result.is_some(),
            "post-result retirement requires a canonical result"
        );
        let result = self
            .result
            .expect("post-result retirement requires a canonical result");
        let deadline_tick = SimTick(
            self.network_tick
                .get()
                .checked_add(u64::from(self.config.typed_disconnect_timeout_ticks))
                .ok_or(AuthorityPeerHubError::TimelineExhausted)?,
        );
        let authority_tick = self.authority.simulation().current_tick();
        let (peer_id, connection, pending_reclaim) = {
            let link = self.peers[index].as_ref().expect("live peer index");
            (
                link.peer_id,
                link.connection,
                self.reconnect.pending_reclaim(link.peer_id)?,
            )
        };
        if let Some(pending) = pending_reclaim {
            self.reconnect
                .abort_reclaim(peer_id, pending.attempt_id, authority_tick)?;
        }
        self.peer_state.disconnect_peer(peer_id);
        if self.reconnect.substitute_control(peer_id, authority_tick)?
            == SubstituteControl::Connected
        {
            self.reconnect.record_disconnect(peer_id, authority_tick)?;
        }

        let link = self.peers[index].as_mut().expect("live peer index");
        link.phase = AuthorityPeerPhase::Closing;
        // Do not call prepare_for_terminal_disconnect here. The reliable
        // ResultIdentifier and its ordered predecessors must retain their
        // exact ACK/retry lifecycle, and no contradictory Disconnect follows.
        link.transfer = None;
        link.applied_sync = None;
        link.resume_input_tick = None;
        if link.queued_result != Some(result) {
            link.pending_result = Some(result);
        }
        link.pending_disconnect = None;
        link.post_result_close_deadline = Some(deadline_tick);
        link.clock_samples.fill(None);
        link.clock_sample_count = 0;
        link.pending_clock_replies.fill(None);
        link.pending_reclaim_completion = None;

        self.record_audit(
            OnlineAuditScope {
                match_id: Some(self.manifest.match_id),
                peer_id: Some(peer_id),
                tick: Some(authority_tick),
                ..OnlineAuditScope::default()
            },
            OnlineAuditCode::PeerDisconnected,
            u64::from(connection.generation),
            1,
        );
        Ok(())
    }

    fn begin_typed_disconnect(
        &mut self,
        index: usize,
        disconnect: DisconnectMessage,
    ) -> Result<(), AuthorityPeerHubError<S::Error>> {
        let Some(link) = self.peers[index].as_ref() else {
            return Ok(());
        };
        if link.phase == AuthorityPeerPhase::Closing {
            return Ok(());
        }
        let deadline_tick = SimTick(
            self.network_tick
                .get()
                .checked_add(u64::from(self.config.typed_disconnect_timeout_ticks))
                .ok_or(AuthorityPeerHubError::TimelineExhausted)?,
        );

        self.metrics.peers_rejected = self.metrics.peers_rejected.saturating_add(1);
        self.logical_disconnect(index)?;
        let queued = self.peers[index]
            .as_mut()
            .expect("logical disconnect retains physical link")
            .runtime
            .queue_tracked_disconnect(disconnect);
        match queued {
            Ok(send) => {
                self.peers[index]
                    .as_mut()
                    .expect("logical disconnect retains physical link")
                    .pending_disconnect = Some(PendingTypedDisconnect {
                    message: disconnect,
                    send: Some(send),
                    deadline_tick,
                });
                self.metrics.typed_disconnects_queued =
                    self.metrics.typed_disconnects_queued.saturating_add(1);
            }
            Err(_) => {
                self.peers[index]
                    .as_mut()
                    .expect("logical disconnect retains physical link")
                    .pending_disconnect = Some(PendingTypedDisconnect {
                    message: disconnect,
                    send: None,
                    deadline_tick,
                });
                self.metrics.typed_disconnects_deferred =
                    self.metrics.typed_disconnects_deferred.saturating_add(1);
            }
        }
        Ok(())
    }

    fn refresh_shutdown_state(&mut self) {
        if self.shutdown != AuthorityShutdownState::Draining {
            return;
        }
        let exempt = self.shutdown_exempt_peer;
        let remote_remains = self
            .peers
            .iter()
            .flatten()
            .any(|peer| Some(peer.peer_id) != exempt);
        if !remote_remains {
            self.shutdown = AuthorityShutdownState::Drained;
        }
    }

    fn reject_slot(&mut self, index: usize) -> Result<(), AuthorityPeerHubError<S::Error>> {
        let Some(connection) = self.peers[index].as_ref().map(|peer| peer.connection) else {
            return Ok(());
        };
        self.metrics.peers_rejected = self.metrics.peers_rejected.saturating_add(1);
        self.detach(connection)?;
        Ok(())
    }

    fn replicate_tick(
        &mut self,
        report: &AuthorityTickReport,
        snapshot: CanonicalSnapshot,
    ) -> Result<(), AuthorityPeerHubError<S::Error>> {
        let relay = self.committed_input_message(report)?;
        let state = self.state_message(report)?;
        let result = match report.final_result_id {
            Some(result_id) => Some(ResultIdentifier {
                match_id: self.manifest.match_id,
                result_id: ResultId::new(result_id)?,
                final_tick: report.tick,
                final_state_hash: report.state_hash,
            }),
            None => None,
        };
        if let Some(result) = result {
            self.result = Some(result);
            self.record_audit(
                OnlineAuditScope {
                    match_id: Some(self.manifest.match_id),
                    tick: Some(result.final_tick),
                    ..OnlineAuditScope::default()
                },
                OnlineAuditCode::ResultConfirmed,
                result.result_id.get(),
                result.final_state_hash.0,
            );
        }

        let connected: [Option<PeerId>; MAX_AUTHORITY_PEERS] = std::array::from_fn(|index| {
            self.peers[index].as_ref().and_then(|peer| {
                matches!(
                    peer.phase,
                    AuthorityPeerPhase::Fighting
                        | AuthorityPeerPhase::RepairSyncInFlight
                        | AuthorityPeerPhase::ReconnectSyncInFlight
                        | AuthorityPeerPhase::ReconnectAwaitingClock
                )
                .then_some(peer.peer_id)
            })
        });
        for peer_id in connected.into_iter().flatten() {
            let index = self
                .peer_index(peer_id)
                .expect("connected peer still present");
            self.queue_latest(index, WireMessage::CommittedInputRelay(relay))?;
            self.queue_latest(index, WireMessage::StateHashAndAcks(state))?;
            if report.tick.get() % u64::from(self.config.state_delta_interval_ticks) == 0
                || result.is_some()
            {
                match self.peer_state.build_latest_for_peer(
                    &mut self.state_history,
                    peer_id,
                    state.as_slice(),
                )? {
                    PeerStateUpdateOutcome::Delta { message, .. } => {
                        self.queue_latest(index, WireMessage::StateDeltaAndAcks(message))?;
                    }
                    PeerStateUpdateOutcome::FullResyncRequired { required, .. } => {
                        if should_start_proactive_repair(required.reason)
                            && self.peers[index]
                                .as_ref()
                                .is_some_and(|peer| peer.transfer.is_none())
                        {
                            self.prepare_transfer_for_peer(
                                peer_id,
                                ResyncRequest {
                                    match_id: self.manifest.match_id,
                                    peer_id,
                                    reason: ResyncReason::HistoryExpired,
                                    last_confirmed_tick: SimTick::ZERO,
                                    last_confirmed_hash: StateHash(0),
                                },
                                snapshot.clone(),
                                TransferPurpose::Repair,
                            )?;
                            self.peers[index].as_mut().expect("live peer").phase =
                                AuthorityPeerPhase::RepairSyncInFlight;
                        }
                    }
                    PeerStateUpdateOutcome::AwaitingBaselineAcknowledgement { .. } => {}
                }
            }
            if let Some(result) = result
                && self.peers[index]
                    .as_ref()
                    .is_some_and(|peer| matches!(peer.phase, AuthorityPeerPhase::Fighting))
            {
                self.peers[index]
                    .as_mut()
                    .expect("live peer")
                    .pending_result = Some(result);
            }
            self.service_peer_outbound(index)?;
        }
        Ok(())
    }

    fn queue_latest(
        &mut self,
        index: usize,
        message: WireMessage,
    ) -> Result<(), AuthorityPeerHubError<S::Error>> {
        let outcome = self.peers[index]
            .as_mut()
            .expect("live peer")
            .runtime
            .queue_message(message);
        match outcome {
            Ok(_) => {
                self.metrics.state_packets_queued =
                    self.metrics.state_packets_queued.saturating_add(1);
            }
            Err(RuntimeQueueError::OutboundQueueFull) => {
                self.metrics.state_packets_deferred =
                    self.metrics.state_packets_deferred.saturating_add(1);
            }
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    fn state_message(
        &self,
        report: &AuthorityTickReport,
    ) -> Result<StateHashAndAcks, ProtocolValidationError> {
        let acknowledgement = self.authority.processed_input_acknowledgement();
        let mut acks = [ProcessedInputAck::default(); MAX_SEATS];
        let mut count = 0;
        for seat in acknowledgement.as_slice() {
            let Some(processed) = seat.processed_input else {
                continue;
            };
            acks[count] = ProcessedInputAck {
                seat: seat.seat,
                processed_through: processed.tick,
                sequence: processed.sequence,
            };
            count += 1;
        }
        StateHashAndAcks::new(
            self.manifest.match_id,
            report.tick,
            report.state_hash,
            &acks[..count],
        )
    }

    fn committed_input_message(
        &mut self,
        report: &AuthorityTickReport,
    ) -> Result<CommittedInputRelay, ProtocolValidationError> {
        for record in report.committed_inputs.iter() {
            let source = match record.origin {
                AuthorityInputOrigin::Peer(peer) => CommittedInputSource::Peer(peer),
                AuthorityInputOrigin::AuthorityBot | AuthorityInputOrigin::DisconnectedBot(_) => {
                    CommittedInputSource::AuthorityBot
                }
                AuthorityInputOrigin::MissingSubstitute => CommittedInputSource::MissingSubstitute,
            };
            self.committed_histories[usize::from(record.frame.seat.get())].push(
                CommittedInputRecord {
                    frame: record.frame,
                    fighter: record.fighter,
                    source,
                },
            );
        }
        let mut windows = [CommittedSeatInputWindow::default(); MAX_SEATS];
        let mut count = 0;
        for assignment in self.manifest.ownership.as_slice() {
            windows[count] =
                self.committed_histories[usize::from(assignment.seat.get())].window()?;
            count += 1;
        }
        CommittedInputRelay::new(self.manifest.match_id, report.tick, &windows[..count])
    }
}

fn message_claimed_peer(message: &WireMessage) -> Option<PeerId> {
    match message {
        WireMessage::Start(
            StartMessage::ManifestAccepted { peer_id, .. }
            | StartMessage::InitialSyncApplied { peer_id, .. }
            | StartMessage::Ready { peer_id, .. },
        ) => Some(*peer_id),
        WireMessage::InputBatch(batch) => Some(batch.peer_id),
        WireMessage::ResyncRequest(request) => Some(request.peer_id),
        WireMessage::ResyncApplied(applied) => Some(applied.peer_id),
        WireMessage::ClockProbe(probe) => Some(probe.peer_id),
        _ => None,
    }
}

const fn input_protocol_violation(error: ProtocolValidationError) -> SecurityViolation {
    match error {
        ProtocolValidationError::PeerMismatch => SecurityViolation::SpoofedIdentity,
        ProtocolValidationError::UnownedSeat
        | ProtocolValidationError::SeatOwnedByDifferentPeer
        | ProtocolValidationError::AuthorityOwnedSeat => SecurityViolation::InvalidSeatOwnership,
        _ => SecurityViolation::InvalidInput,
    }
}

const fn runtime_connection_code(connection: RuntimeConnectionState) -> u64 {
    match connection {
        RuntimeConnectionState::Active => 0,
        RuntimeConnectionState::RemoteDisconnect => 1,
        RuntimeConnectionState::TransportDisconnected => 2,
        RuntimeConnectionState::RetryExhausted => 3,
    }
}

const fn transfer_purpose_code(purpose: TransferPurpose) -> u64 {
    match purpose {
        TransferPurpose::Initial => 1,
        TransferPurpose::Repair => 2,
        TransferPurpose::Reconnect(_) => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority_input::{AuthorityInputStatus, DEFAULT_MAX_FUTURE_INPUT_TICKS};
    use crate::determinism::SimEntityKind;
    use crate::network_io::{AfcDatagram, InProcessEndpoint, SendOutcome};
    use crate::network_protocol::{
        AuthorityKind, BuildId, CompatibilityId, DefinitionId, DisconnectCode, FighterId,
        FighterSlotConfig, GameplayContentHash, InputBatch, InputButtons, InputSequence,
        ManifestHash, ProtocolVersion, QuantizedAxis, ReplayFormatVersion, SeatAssignment,
        SeatInputWindow, SeatOwnership, SimulationVersion, StateBaselineAck, TeamId,
    };
    use crate::network_runtime::QueueDisposition;
    use crate::snapshot::{
        ArenaRuntimeSnapshot, CanonicalSnapshot, FighterSnapshot, MatchStateSnapshot,
        MatchStatsSnapshot, PoolAllocatorSnapshot, SnapshotHeader,
    };

    #[test]
    fn state_sync_failures_never_start_an_unfenced_proactive_repair() {
        assert!(!should_start_proactive_repair(
            FullResyncReason::DeltaTooDense
        ));
        assert!(!should_start_proactive_repair(
            FullResyncReason::BaselineMissing
        ));
        assert!(!should_start_proactive_repair(
            FullResyncReason::BaselineHashMismatch {
                retained_hash: StateHash(7),
            }
        ));
    }

    fn peer(index: usize) -> PeerId {
        PeerId::new(index as u64 + 1).unwrap()
    }

    fn user(index: usize) -> AuthenticatedUserId {
        AuthenticatedUserId::new(index as u64 + 101).unwrap()
    }

    fn compatibility() -> CompatibilityId {
        CompatibilityId {
            protocol: ProtocolVersion::new(1).unwrap(),
            simulation: SimulationVersion::new(1).unwrap(),
            replay: ReplayFormatVersion::new(1).unwrap(),
            build: BuildId::new([7; 16]).unwrap(),
            gameplay_content: GameplayContentHash::new([8; 32]).unwrap(),
        }
    }

    fn manifest() -> MatchManifest {
        let assignments: [SeatAssignment; MAX_SEATS] =
            std::array::from_fn(|index| SeatAssignment {
                seat: SeatId::new(index as u8).unwrap(),
                fighter: FighterId::new(index as u8).unwrap(),
                owner: SeatOwner::Peer(peer(index)),
            });
        let ownership = SeatOwnership::from_assignments(&assignments).unwrap();
        let slots = std::array::from_fn(|index| FighterSlotConfig {
            occupied: true,
            fighter: FighterId::new(index as u8).unwrap(),
            team: TeamId::new(index as u8).unwrap(),
            character: DefinitionId::new(index as u16).unwrap(),
            style: DefinitionId::new(1).unwrap(),
            equipment: DefinitionId::new(0).unwrap(),
        });
        MatchManifest {
            compatibility: compatibility(),
            manifest_hash: ManifestHash(0x1122_3344),
            match_id: crate::network_protocol::MatchId::new(*b"peer-hub-test-01").unwrap(),
            authority: AuthorityKind::Dedicated,
            trusted_results: true,
            arena: DefinitionId::new(1).unwrap(),
            rules: DefinitionId::new(1).unwrap(),
            slots,
            ownership,
            master_gameplay_seed: 42,
            rng_scheme_version: 1,
            tick_rate_hz: 60,
            input_delay_ticks: 2,
            rollback_limit_ticks: 12,
            snapshot_history_ticks: 32,
            agreed_start_tick: SimTick(5),
        }
    }

    fn roster() -> [AuthenticatedPeer; MAX_AUTHORITY_PEERS] {
        std::array::from_fn(|index| AuthenticatedPeer {
            peer_id: peer(index),
            user_id: user(index),
        })
    }

    fn snapshot(
        match_id: crate::network_protocol::MatchId,
        tick: SimTick,
        salt: u64,
    ) -> CanonicalSnapshot {
        let allocators = SimEntityKind::ALL
            .into_iter()
            .map(|kind| PoolAllocatorSnapshot::empty(kind, 1).unwrap())
            .collect();
        let mut arena = ArenaRuntimeSnapshot::default();
        arena.arena_ticks = salt;
        CanonicalSnapshot {
            header: SnapshotHeader::new(1, 1, 1, *match_id.as_bytes(), tick, 42),
            match_state: MatchStateSnapshot::default(),
            fighters: FighterId::ALL.map(FighterSnapshot::empty),
            arena,
            allocators,
            dynamic_objects: Vec::new(),
            rng_streams: Vec::new(),
            stats: MatchStatsSnapshot {
                gameplay_ticks: tick.get(),
                ..MatchStatsSnapshot::default()
            },
        }
    }

    struct ToySimulation {
        match_id: crate::network_protocol::MatchId,
        tick: SimTick,
        salt: u64,
        finish_tick: Option<SimTick>,
    }

    impl AuthoritySimulation for ToySimulation {
        type Snapshot = CanonicalSnapshot;
        type Error = &'static str;

        fn current_tick(&self) -> SimTick {
            self.tick
        }

        fn step(
            &mut self,
            inputs: &crate::authority_input::CommittedTickInputs,
        ) -> Result<(), Self::Error> {
            if inputs.tick != self.tick.next() {
                return Err("non-contiguous tick");
            }
            self.tick = inputs.tick;
            self.salt = self.salt.wrapping_mul(109).wrapping_add(self.tick.get());
            for record in inputs.iter() {
                self.salt = self
                    .salt
                    .wrapping_add(record.frame.movement_x.get() as i64 as u64)
                    .wrapping_add(u64::from(record.frame.held_buttons.bits()));
            }
            Ok(())
        }

        fn capture_snapshot(&self) -> Result<Self::Snapshot, Self::Error> {
            Ok(snapshot(self.match_id, self.tick, self.salt))
        }

        fn final_result_id(&self) -> Option<u64> {
            (Some(self.tick) == self.finish_tick).then_some(9001)
        }
    }

    type TestHub = AuthorityPeerHub<ToySimulation, InProcessEndpoint>;
    type TestClient = NetworkRuntime<InProcessEndpoint>;

    fn make_hub(
        finish_tick: Option<u64>,
        endpoint_capacity: usize,
        mut config: AuthorityPeerHubConfig,
    ) -> (TestHub, Vec<TestClient>) {
        config.runtime.outbound_capacity = config.runtime.outbound_capacity.max(16);
        let manifest = manifest();
        let simulation = ToySimulation {
            match_id: manifest.match_id,
            tick: SimTick::ZERO,
            salt: 17,
            finish_tick: finish_tick.map(SimTick),
        };
        let mut hub = AuthorityPeerHub::new(
            manifest,
            simulation,
            AuthorityInputConfig::default(),
            &roster(),
            config,
        )
        .unwrap();
        let mut clients = Vec::new();
        for index in 0..MAX_AUTHORITY_PEERS {
            let (client_endpoint, authority_endpoint) =
                InProcessEndpoint::pair(endpoint_capacity).unwrap();
            hub.attach_initial(peer(index), user(index), authority_endpoint)
                .unwrap();
            clients.push(
                NetworkRuntime::new(
                    client_endpoint,
                    PeerRole::Client,
                    compatibility(),
                    config.runtime,
                )
                .unwrap(),
            );
        }
        (hub, clients)
    }

    fn mark_all_ready(hub: &mut TestHub) {
        for index in 0..MAX_AUTHORITY_PEERS {
            let peer_id = hub.peers[index].as_ref().unwrap().peer_id;
            {
                let link = hub.peers[index].as_mut().unwrap();
                link.phase = AuthorityPeerPhase::AwaitingReady;
                for probe in 1..=CLOCK_SYNC_SAMPLE_CAPACITY {
                    AuthorityPeerHub::<ToySimulation, InProcessEndpoint>::record_clock_sample(
                        link,
                        ClockProbeId::new(probe as u32).unwrap(),
                    );
                }
            }
            assert!(
                hub.handle_start(
                    index,
                    StartMessage::Ready {
                        match_id: hub.manifest.match_id,
                        peer_id,
                    },
                )
                .unwrap()
            );
        }
    }

    fn force_fighting(hub: &mut TestHub) {
        // Keep gameplay-focused tests on their compact historical timeline;
        // countdown-lead behavior has a dedicated late-readiness test below.
        hub.config.countdown_lead_ticks = 1;
        mark_all_ready(hub);
        hub.pump_network(SimTick::ZERO).unwrap();
        assert!(hub.countdown_broadcast());
        hub.pump_network(hub.countdown_start_tick().unwrap())
            .unwrap();
        assert!(
            hub.peers
                .iter()
                .flatten()
                .all(|peer| peer.phase == AuthorityPeerPhase::Fighting)
        );
    }

    fn input(tick: u64, index: usize) -> InputFrame {
        InputFrame {
            tick: SimTick(tick),
            seat: SeatId::new(index as u8).unwrap(),
            movement_x: QuantizedAxis::new(20 + index as i8).unwrap(),
            movement_y: QuantizedAxis::default(),
            held_buttons: InputButtons::new(InputButtons::LIGHT).unwrap(),
            pressed_buttons: InputButtons::new(InputButtons::LIGHT).unwrap(),
            released_buttons: InputButtons::default(),
            sequence: InputSequence(tick as u16),
        }
    }

    fn batch(tick: u64, claimed_peer: usize, seat: usize) -> InputBatch {
        let window = SeatInputWindow::from_newest_first(&[input(tick, seat)]).unwrap();
        InputBatch::new(manifest().match_id, peer(claimed_peer), &[window]).unwrap()
    }

    fn neutral_bot(peer_id: PeerId, seat: SeatId, tick: SimTick) -> InputFrame {
        InputFrame {
            tick,
            seat,
            movement_x: QuantizedAxis::default(),
            movement_y: QuantizedAxis::default(),
            held_buttons: InputButtons::default(),
            pressed_buttons: InputButtons::default(),
            released_buttons: InputButtons::default(),
            sequence: InputSequence((tick.get() as u16) ^ peer_id.get() as u16),
        }
    }

    fn typed_disconnect() -> DisconnectMessage {
        DisconnectMessage {
            match_id: Some(manifest().match_id),
            code: DisconnectCode::Kicked,
            retry: RetryDisposition::ReturnToLobby,
            detail_code: 41,
            last_confirmed_tick: Some(SimTick::ZERO),
        }
    }

    #[test]
    fn global_shutdown_fences_gameplay_and_ack_drains_every_remote_generation() {
        let mut config = AuthorityPeerHubConfig::default();
        config.runtime.reliable_retry_interval_ticks = 1;
        let (mut hub, mut clients) = make_hub(None, 64, config);
        force_fighting(&mut hub);
        let host = peer(0);
        let authority_tick = hub.authority.simulation().current_tick();
        let host_connection = hub.connection_for_peer(host).unwrap();

        hub.begin_shutdown(host).unwrap();

        assert_eq!(hub.shutdown_state(), AuthorityShutdownState::Draining);
        assert_eq!(hub.connection_for_peer(host), Some(host_connection));
        assert_eq!(
            hub.try_advance(neutral_bot).unwrap(),
            (AuthorityAdvanceOutcome::Finished, None)
        );
        assert_eq!(hub.authority.simulation().current_tick(), authority_tick);
        let (_client_endpoint, authority_endpoint) = InProcessEndpoint::pair(64).unwrap();
        assert!(matches!(
            hub.attach_initial(peer(1), user(1), authority_endpoint),
            Err(AuthorityPeerHubError::ShutdownInProgress)
        ));

        clients[1]
            .queue_message(WireMessage::InputBatch(batch(1, 1, 1)))
            .unwrap();
        let first_shutdown_tick = hub.network_tick().next().get();
        for tick in first_shutdown_tick..first_shutdown_tick + 30 {
            for client in clients.iter_mut().skip(1) {
                client.pump(SimTick(tick));
            }
            hub.pump_network(SimTick(tick)).unwrap();
            if hub.shutdown_drained() {
                break;
            }
        }

        assert!(hub.shutdown_drained());
        assert_eq!(hub.authority.simulation().current_tick(), authority_tick);
        assert!(
            !hub.authority
                .has_buffered_input(SeatId::new(1).unwrap(), authority_tick.next())
        );
        assert_eq!(hub.connection_for_peer(host), Some(host_connection));
        let mut drained = Vec::new();
        while let Some(event) = hub.try_next_drain_event() {
            drained.push(event);
        }
        assert_eq!(drained.len(), MAX_AUTHORITY_PEERS - 1);
        for event in drained {
            assert_ne!(event.peer_id, host);
            assert_eq!(event.completion, AuthorityClosingCompletion::Acknowledged);
            assert_eq!(
                event.disconnect,
                Some(DisconnectMessage {
                    match_id: Some(manifest().match_id),
                    code: DisconnectCode::ServerShutdown,
                    retry: RetryDisposition::MatchEndedNoContest,
                    detail_code: SERVER_SHUTDOWN_DETAIL_CODE,
                    last_confirmed_tick: Some(authority_tick),
                })
            );
        }
    }

    #[test]
    fn global_shutdown_missing_acks_retires_at_exact_bounded_deadline() {
        let mut config = AuthorityPeerHubConfig::default();
        config.typed_disconnect_timeout_ticks = 3;
        let (mut hub, _clients) = make_hub(None, 64, config);

        hub.begin_shutdown(peer(0)).unwrap();
        hub.pump_network(SimTick(1)).unwrap();
        hub.pump_network(SimTick(2)).unwrap();
        assert!(!hub.shutdown_drained());
        hub.pump_network(SimTick(3)).unwrap();

        assert!(hub.shutdown_drained());
        let mut timed_out = 0;
        while let Some(event) = hub.try_next_drain_event() {
            assert_eq!(event.completion, AuthorityClosingCompletion::TimedOut);
            timed_out += 1;
        }
        assert_eq!(timed_out, MAX_AUTHORITY_PEERS - 1);
        assert_eq!(
            hub.metrics().typed_disconnects_timed_out,
            (MAX_AUTHORITY_PEERS - 1) as u64
        );
    }

    #[test]
    fn confirmed_result_shutdown_never_emits_a_no_contest_terminal() {
        let mut config = AuthorityPeerHubConfig::default();
        config.runtime.reliable_retry_interval_ticks = 1;
        let (mut hub, mut clients) = make_hub(Some(1), 64, config);
        force_fighting(&mut hub);
        let repairing_index = hub.peer_index(peer(1)).unwrap();
        hub.peers[repairing_index].as_mut().unwrap().phase = AuthorityPeerPhase::RepairSyncInFlight;
        let report = hub.try_advance(neutral_bot).unwrap().1.unwrap();
        let result = hub.confirmed_result().expect("canonical confirmed result");
        assert_eq!(result.final_tick, report.tick);
        assert_eq!(result.final_state_hash, report.state_hash);
        assert_eq!(
            hub.peers[repairing_index].as_ref().unwrap().queued_result,
            None
        );

        hub.begin_shutdown(peer(0)).unwrap();
        let first_shutdown_tick = hub.network_tick().next().get();
        for tick in first_shutdown_tick..first_shutdown_tick + 24 {
            hub.pump_network(SimTick(tick)).unwrap();
            for client in clients.iter_mut().skip(1) {
                client.pump(SimTick(tick));
            }
            if hub.shutdown_drained() {
                break;
            }
        }

        assert!(hub.shutdown_drained());
        assert_eq!(hub.confirmed_result(), Some(result));
        for client in clients.iter_mut().skip(1) {
            let messages: Vec<_> = std::iter::from_fn(|| client.try_next_event()).collect();
            assert!(messages.iter().any(|event| {
                matches!(
                    event,
                    RuntimeEvent::Message(WireMessage::ResultIdentifier(received))
                        if *received == result
                )
            }));
            assert!(!messages.iter().any(|event| {
                matches!(event, RuntimeEvent::Message(WireMessage::Disconnect(_)))
            }));
        }
        let mut drained = 0;
        while let Some(event) = hub.try_next_drain_event() {
            assert_eq!(event.disconnect, None);
            assert_eq!(event.completion, AuthorityClosingCompletion::Acknowledged);
            drained += 1;
        }
        assert_eq!(drained, MAX_AUTHORITY_PEERS - 1);
    }

    #[test]
    fn authentication_revocation_retains_exact_identity_until_physical_drain() {
        let mut config = AuthorityPeerHubConfig::default();
        config.runtime.reliable_retry_interval_ticks = 1;
        let (mut hub, mut clients) = make_hub(None, 64, config);
        let identity = hub.peer_identity(peer(0)).unwrap();

        hub.revoke_authentication(identity.connection).unwrap();
        assert_eq!(hub.peer_identity(peer(0)), Some(identity));
        assert_eq!(hub.connection_for_peer(peer(0)), None);
        hub.pump_network(SimTick(1)).unwrap();
        clients[0].pump(SimTick(1));
        hub.pump_network(SimTick(2)).unwrap();

        let drained = hub.try_next_drain_event().unwrap();
        assert_eq!(drained.peer_id, identity.peer_id);
        assert_eq!(drained.user_id, identity.user_id);
        assert_eq!(drained.connection, identity.connection);
        assert_eq!(
            drained.disconnect.map(|message| message.detail_code),
            Some(SecurityViolation::AuthenticationRevoked.detail_code())
        );
        assert_eq!(drained.completion, AuthorityClosingCompletion::Acknowledged);
        assert_eq!(hub.peer_identity(peer(0)), None);
    }

    #[test]
    fn typed_disconnect_closes_only_after_exact_ack_and_duplicate_delivery_is_idempotent() {
        let mut config = AuthorityPeerHubConfig::default();
        config.runtime.reliable_retry_interval_ticks = 1;
        let (mut hub, mut clients) = make_hub(None, 64, config);
        let index = hub.peer_index(peer(0)).unwrap();
        let old_connection = hub.peers[index].as_ref().unwrap().connection;

        hub.begin_typed_disconnect(index, typed_disconnect())
            .unwrap();
        assert_eq!(hub.peer_phase(peer(0)), Some(AuthorityPeerPhase::Closing));
        assert_eq!(hub.connection_for_peer(peer(0)), None);
        assert_eq!(hub.authenticated_user_for_peer(peer(0)), None);
        assert!(hub.peers[index].is_some());
        assert_eq!(hub.metrics().peers_rejected, 1);

        // Hold the receiver for one retry so two identical reliable packets
        // arrive. Only one typed event is delivered, but its ACK closes the
        // exact tracked authority send.
        hub.pump_network(SimTick(1)).unwrap();
        hub.pump_network(SimTick(2)).unwrap();
        clients[0].pump(SimTick(2));
        let disconnect_events = std::iter::from_fn(|| clients[0].try_next_event())
            .filter(|event| {
                matches!(
                    event,
                    RuntimeEvent::Message(WireMessage::Disconnect(message))
                        if *message == typed_disconnect()
                )
            })
            .count();
        assert_eq!(disconnect_events, 1);
        hub.pump_network(SimTick(3)).unwrap();

        assert!(hub.peers[usize::from(old_connection.slot())].is_none());
        assert_eq!(hub.metrics().typed_disconnects_queued, 1);
        assert_eq!(hub.metrics().typed_disconnects_acknowledged, 1);
        assert_eq!(hub.metrics().typed_disconnects_timed_out, 0);
        assert_eq!(hub.metrics().typed_disconnects_transport_closed, 0);
    }

    #[test]
    fn lost_disconnect_ack_retires_physical_link_at_bounded_deadline() {
        let mut config = AuthorityPeerHubConfig::default();
        config.typed_disconnect_timeout_ticks = 3;
        let (mut hub, _clients) = make_hub(None, 64, config);
        let index = hub.peer_index(peer(0)).unwrap();

        hub.begin_typed_disconnect(index, typed_disconnect())
            .unwrap();
        hub.pump_network(SimTick(1)).unwrap();
        hub.pump_network(SimTick(2)).unwrap();
        assert!(hub.peers[index].is_some());
        hub.pump_network(SimTick(3)).unwrap();

        assert!(hub.peers[index].is_none());
        assert_eq!(hub.metrics().typed_disconnects_timed_out, 1);
        assert_eq!(hub.metrics().typed_disconnects_acknowledged, 0);
    }

    #[test]
    fn full_control_queue_defers_until_predecessor_acks_admit_disconnect() {
        let mut config = AuthorityPeerHubConfig::default();
        config.runtime.reliable_reorder_capacity =
            crate::network_runtime::MAX_RELIABLE_REORDER_MESSAGES;
        let (mut hub, mut clients) = make_hub(None, 64, config);
        let index = hub.peer_index(peer(0)).unwrap();
        let old_connection = hub.peers[index].as_ref().unwrap().connection;
        let manifest = hub.manifest;
        let outbound_capacity = hub.config.runtime.outbound_capacity;
        for _ in 0..outbound_capacity {
            hub.peers[index]
                .as_mut()
                .unwrap()
                .runtime
                .queue_start_message(StartMessage::Manifest(manifest))
                .unwrap();
        }

        hub.begin_typed_disconnect(index, typed_disconnect())
            .unwrap();

        assert_eq!(hub.peer_phase(peer(0)), Some(AuthorityPeerPhase::Closing));
        assert_eq!(hub.connection_for_peer(peer(0)), None);
        assert_eq!(hub.authenticated_user_for_peer(peer(0)), None);
        assert!(hub.peers[index].is_some());
        assert_eq!(hub.metrics().peers_rejected, 1);
        assert_eq!(hub.metrics().typed_disconnects_queued, 0);
        assert_eq!(hub.metrics().typed_disconnects_deferred, 1);

        hub.pump_network(SimTick(1)).unwrap();
        clients[0].pump(SimTick(1));
        while clients[0].try_next_event().is_some() {}
        hub.pump_network(SimTick(2)).unwrap();
        assert!(hub.peers[index].is_some());
        assert_eq!(hub.metrics().typed_disconnects_queued, 1);

        hub.pump_network(SimTick(3)).unwrap();
        clients[0].pump(SimTick(3));
        assert!(
            std::iter::from_fn(|| clients[0].try_next_event())
                .any(|event| matches!(event, RuntimeEvent::Message(WireMessage::Disconnect(_))))
        );
        hub.pump_network(SimTick(4)).unwrap();

        assert!(hub.peers[usize::from(old_connection.slot())].is_none());
        assert_eq!(hub.metrics().typed_disconnects_acknowledged, 1);
        assert_eq!(hub.metrics().typed_disconnects_timed_out, 0);
    }

    #[test]
    fn permanently_full_control_queue_times_out_without_premature_retirement() {
        let mut config = AuthorityPeerHubConfig::default();
        config.typed_disconnect_timeout_ticks = 3;
        let (mut hub, _clients) = make_hub(None, 64, config);
        let index = hub.peer_index(peer(0)).unwrap();
        let manifest = hub.manifest;
        let outbound_capacity = hub.config.runtime.outbound_capacity;
        for _ in 0..outbound_capacity {
            hub.peers[index]
                .as_mut()
                .unwrap()
                .runtime
                .queue_start_message(StartMessage::Manifest(manifest))
                .unwrap();
        }
        hub.begin_typed_disconnect(index, typed_disconnect())
            .unwrap();

        hub.pump_network(SimTick(1)).unwrap();
        hub.pump_network(SimTick(2)).unwrap();
        assert!(hub.peers[index].is_some());
        assert_eq!(hub.metrics().typed_disconnects_queued, 0);
        hub.pump_network(SimTick(3)).unwrap();

        assert!(hub.peers[index].is_none());
        assert_eq!(hub.metrics().typed_disconnects_deferred, 1);
        assert_eq!(hub.metrics().typed_disconnects_queued, 0);
        assert_eq!(hub.metrics().typed_disconnects_timed_out, 1);
    }

    #[test]
    fn closing_transport_loss_is_an_immediate_raw_close_fallback() {
        let (mut hub, mut clients) = make_hub(None, 64, AuthorityPeerHubConfig::default());
        let index = hub.peer_index(peer(0)).unwrap();
        hub.begin_typed_disconnect(index, typed_disconnect())
            .unwrap();
        drop(clients.remove(0));

        hub.pump_network(SimTick(1)).unwrap();

        assert!(hub.peers[index].is_none());
        assert_eq!(hub.metrics().typed_disconnects_transport_closed, 1);
        assert_eq!(hub.metrics().typed_disconnects_acknowledged, 0);
    }

    #[test]
    fn client_disconnect_fields_are_ignored_as_an_untrusted_close_request() {
        let (mut hub, mut clients) = make_hub(None, 64, AuthorityPeerHubConfig::default());
        clients[0]
            .queue_message(WireMessage::Disconnect(DisconnectMessage {
                match_id: None,
                code: DisconnectCode::ServerShutdown,
                retry: RetryDisposition::Fatal,
                detail_code: u16::MAX,
                last_confirmed_tick: Some(SimTick(u64::MAX)),
            }))
            .unwrap();
        clients[0].pump(SimTick(1));
        hub.pump_network(SimTick(1)).unwrap();

        assert_eq!(hub.connection_for_peer(peer(0)), None);
        assert!(hub.peer_index(peer(0)).is_none());
        assert!(hub.ban_provider_mut().lookup(user(0), SimTick(1)).is_none());
        assert_eq!(hub.metrics().platform_bans, 0);
        assert_eq!(hub.metrics().temporary_bans, 0);
        assert_eq!(hub.metrics().typed_disconnects_queued, 0);
    }

    #[test]
    fn closing_purges_result_work_and_never_services_gameplay_again() {
        let (mut hub, mut clients) = make_hub(None, 64, AuthorityPeerHubConfig::default());
        let index = hub.peer_index(peer(0)).unwrap();
        let result = ResultIdentifier {
            match_id: hub.manifest.match_id,
            result_id: ResultId::new(71).unwrap(),
            final_tick: SimTick(9),
            final_state_hash: StateHash(11),
        };
        {
            let link = hub.peers[index].as_mut().unwrap();
            link.phase = AuthorityPeerPhase::Fighting;
            link.pending_result = Some(result);
            link.runtime
                .queue_message(WireMessage::ResultIdentifier(result))
                .unwrap();
        }

        hub.begin_typed_disconnect(index, typed_disconnect())
            .unwrap();
        hub.service_peer_outbound(index).unwrap();
        assert!(
            hub.peers[index]
                .as_ref()
                .is_some_and(|link| link.pending_result.is_none() && link.transfer.is_none())
        );
        clients[0]
            .queue_message(WireMessage::InputBatch(batch(1, 0, 0)))
            .unwrap();
        clients[0].pump(SimTick(1));
        hub.pump_network(SimTick(1)).unwrap();
        assert_eq!(hub.metrics().input_batches_accepted, 0);
        assert!(
            !hub.authority
                .has_buffered_input(SeatId::new(0).unwrap(), SimTick(1))
        );

        for tick in 2..=4 {
            hub.pump_network(SimTick(tick)).unwrap();
            clients[0].pump(SimTick(tick));
        }

        while let Some(event) = clients[0].try_next_event() {
            assert!(!matches!(
                event,
                RuntimeEvent::Message(WireMessage::ResultIdentifier(_))
            ));
        }
    }

    #[test]
    fn same_identity_reconnect_replaces_closing_generation_and_isolates_stale_operations() {
        let (mut hub, _clients) = make_hub(None, 256, AuthorityPeerHubConfig::default());
        force_fighting(&mut hub);
        let index = hub.peer_index(peer(0)).unwrap();
        let old_connection = hub.peers[index].as_ref().unwrap().connection;
        hub.begin_typed_disconnect(index, typed_disconnect())
            .unwrap();
        let (_new_client, authority_endpoint) = InProcessEndpoint::pair(256).unwrap();

        let new_connection = hub
            .attach_reconnect(
                user(0),
                ReconnectClaim {
                    match_id: hub.manifest.match_id,
                    peer_id: peer(0),
                    last_confirmed_tick: SimTick::ZERO,
                },
                authority_endpoint,
            )
            .unwrap();

        assert_ne!(new_connection.generation(), old_connection.generation());
        assert_eq!(hub.connection_for_peer(peer(0)), Some(new_connection));
        assert_eq!(
            hub.peer_phase(peer(0)),
            Some(AuthorityPeerPhase::ReconnectHandshake)
        );
        assert!(matches!(
            hub.detach(old_connection),
            Err(AuthorityPeerHubError::StaleConnection(_))
        ));
        assert_eq!(hub.connection_for_peer(peer(0)), Some(new_connection));
        assert_eq!(hub.metrics().typed_disconnects_transport_closed, 1);
    }

    #[test]
    fn typed_disconnect_timeout_configuration_is_bounded() {
        for invalid in [0, MAX_TYPED_DISCONNECT_TIMEOUT_TICKS + 1] {
            let mut config = AuthorityPeerHubConfig::default();
            config.typed_disconnect_timeout_ticks = invalid;
            let manifest = manifest();
            let simulation = ToySimulation {
                match_id: manifest.match_id,
                tick: SimTick::ZERO,
                salt: 17,
                finish_tick: None,
            };
            assert!(matches!(
                AuthorityPeerHub::<ToySimulation, InProcessEndpoint>::new(
                    manifest,
                    simulation,
                    AuthorityInputConfig::default(),
                    &roster(),
                    config,
                ),
                Err(AuthorityPeerHubError::InvalidConfig)
            ));
        }
    }

    #[test]
    fn four_peer_authority_never_progresses_before_global_readiness() {
        let (mut hub, _clients) = make_hub(None, 64, AuthorityPeerHubConfig::default());
        let (outcome, report) = hub.try_advance(neutral_bot).unwrap();
        assert_eq!(outcome, AuthorityAdvanceOutcome::WaitingForReady);
        assert!(report.is_none());
        assert_eq!(hub.authority.simulation().current_tick(), SimTick::ZERO);

        force_fighting(&mut hub);
        let (outcome, report) = hub.try_advance(neutral_bot).unwrap();
        assert_eq!(outcome, AuthorityAdvanceOutcome::Advanced);
        assert_eq!(report.unwrap().tick, SimTick(1));
    }

    #[test]
    fn countdown_broadcast_retries_transactionally_after_one_peer_queue_pressure() {
        let mut config = AuthorityPeerHubConfig::default();
        config.runtime.outbound_capacity = 16;
        let (mut hub, mut clients) = make_hub(None, 64, config);
        let pressured = MAX_AUTHORITY_PEERS - 1;
        for id in 1..=config.runtime.outbound_capacity {
            hub.peers[pressured]
                .as_mut()
                .unwrap()
                .runtime
                .queue_message(WireMessage::ResultIdentifier(ResultIdentifier {
                    match_id: manifest().match_id,
                    result_id: ResultId::new(id as u64).unwrap(),
                    final_tick: SimTick(id as u64),
                    final_state_hash: StateHash(id as u64),
                }))
                .unwrap();
        }
        mark_all_ready(&mut hub);

        hub.maybe_start_countdown().unwrap();
        assert_eq!(hub.countdown_start_tick(), None);
        assert_eq!(hub.metrics().countdown_broadcasts_deferred, 1);
        assert!(hub.peers.iter().flatten().all(|link| {
            link.phase == AuthorityPeerPhase::Ready
                && (link.peer_id == peer(pressured) || link.runtime.outbound_len() == 0)
        }));

        hub.peers[pressured]
            .as_mut()
            .unwrap()
            .runtime
            .pump(SimTick(1));
        clients[pressured].pump(SimTick(1));
        hub.peers[pressured]
            .as_mut()
            .unwrap()
            .runtime
            .pump(SimTick(1));
        assert_eq!(
            hub.peers[pressured]
                .as_ref()
                .unwrap()
                .runtime
                .reliable_pending_len(),
            0
        );

        hub.maybe_start_countdown().unwrap();
        assert!(hub.countdown_start_tick().is_some());
        assert!(
            hub.peers
                .iter()
                .flatten()
                .all(|peer| peer.phase == AuthorityPeerPhase::Countdown)
        );
        assert!(
            hub.peers
                .iter()
                .flatten()
                .all(|peer| peer.runtime.reliable_pending_len() == 1)
        );
    }

    #[test]
    fn first_input_at_countdown_boundary_is_processed_after_phase_promotion() {
        let mut config = AuthorityPeerHubConfig::default();
        config.countdown_lead_ticks = 1;
        let (mut hub, mut clients) = make_hub(None, 64, config);
        mark_all_ready(&mut hub);
        hub.pump_network(SimTick::ZERO).unwrap();
        let start_tick = hub.countdown_start_tick().unwrap();

        clients[0]
            .queue_message(WireMessage::InputBatch(batch(1, 0, 0)))
            .unwrap();
        clients[0].pump(start_tick);
        hub.pump_network(start_tick).unwrap();

        assert_eq!(hub.peer_phase(peer(0)), Some(AuthorityPeerPhase::Fighting));
        assert_eq!(hub.metrics().input_batches_accepted, 1);
        assert!(hub.connection_for_peer(peer(0)).is_some());
    }

    #[test]
    fn countdown_phase_buffers_only_bounded_valid_future_input() {
        let mut config = AuthorityPeerHubConfig::default();
        config.countdown_lead_ticks = 10;
        let (mut hub, _clients) = make_hub(None, 64, config);
        mark_all_ready(&mut hub);
        hub.pump_network(SimTick::ZERO).unwrap();
        assert_eq!(hub.peer_phase(peer(0)), Some(AuthorityPeerPhase::Countdown));

        assert!(hub.handle_input(0, batch(1, 0, 0)).unwrap());
        assert_eq!(hub.authority.input_metrics().accepted_peer_frames, 1);
        assert_eq!(hub.metrics().security_violations, 0);
        assert!(hub.connection_for_peer(peer(0)).is_some());

        let outside_window = DEFAULT_MAX_FUTURE_INPUT_TICKS + 2;
        assert!(hub.handle_input(0, batch(outside_window, 0, 0)).unwrap());
        assert_eq!(hub.authority.input_metrics().accepted_peer_frames, 1);
        assert_eq!(hub.authority.input_metrics().rejected_future_frames, 1);
        assert_eq!(hub.metrics().security_violations, 1);
        assert_eq!(hub.metrics().security_kicks, 0);
        assert!(hub.connection_for_peer(peer(0)).is_some());
        assert_eq!(
            hub.try_advance(neutral_bot).unwrap().0,
            AuthorityAdvanceOutcome::WaitingForStartTick
        );
    }

    #[test]
    fn committed_late_correction_frames_are_observed_without_security_score() {
        let (mut hub, mut clients) = make_hub(None, 64, AuthorityPeerHubConfig::default());
        force_fighting(&mut hub);
        for tick in 1..=3 {
            hub.try_advance(neutral_bot).unwrap();
            clients[0]
                .queue_message(WireMessage::InputBatch(batch(tick, 0, 0)))
                .unwrap();
            clients[0].pump(SimTick(10 + tick));
            hub.pump_network(SimTick(10 + tick)).unwrap();
        }

        assert!(hub.connection_for_peer(peer(0)).is_some());
        assert_eq!(hub.metrics().security_violations, 0);
        assert_eq!(hub.metrics().security_kicks, 0);
        assert!(hub.observability().counters().inputs_rejected >= 3);
    }

    #[test]
    fn readiness_after_manifest_proposal_selects_a_new_future_boundary() {
        let mut config = AuthorityPeerHubConfig::default();
        config.countdown_lead_ticks = 10;
        let (mut hub, _clients) = make_hub(None, 64, config);
        hub.pump_network(SimTick(500)).unwrap();
        mark_all_ready(&mut hub);

        hub.pump_network(SimTick(500)).unwrap();
        assert_eq!(hub.countdown_start_tick(), Some(SimTick(510)));
        assert!(
            hub.peers
                .iter()
                .flatten()
                .all(|peer| peer.phase == AuthorityPeerPhase::Countdown)
        );
        assert_eq!(
            hub.try_advance(neutral_bot).unwrap().0,
            AuthorityAdvanceOutcome::WaitingForStartTick
        );
        hub.pump_network(SimTick(510)).unwrap();
        assert!(
            hub.peers
                .iter()
                .flatten()
                .all(|peer| peer.phase == AuthorityPeerPhase::Fighting)
        );
    }

    #[test]
    fn ready_requires_three_distinct_clock_replies_reserved_by_the_hub() {
        let (mut hub, _clients) = make_hub(None, 64, AuthorityPeerHubConfig::default());
        hub.peers[0].as_mut().unwrap().phase = AuthorityPeerPhase::AwaitingReady;
        let ready = StartMessage::Ready {
            match_id: hub.manifest.match_id,
            peer_id: peer(0),
        };
        assert!(!hub.handle_start(0, ready).unwrap());

        for value in [1, 1, 2] {
            assert!(
                hub.handle_clock_probe(
                    0,
                    ClockProbe {
                        match_id: hub.manifest.match_id,
                        peer_id: peer(0),
                        probe_id: ClockProbeId::new(value).unwrap(),
                    },
                )
                .unwrap()
            );
        }
        assert_eq!(hub.peers[0].as_ref().unwrap().clock_sample_count, 2);
        assert!(!hub.handle_start(0, ready).unwrap());
        assert!(
            hub.handle_clock_probe(
                0,
                ClockProbe {
                    match_id: hub.manifest.match_id,
                    peer_id: peer(0),
                    probe_id: ClockProbeId::new(3).unwrap(),
                },
            )
            .unwrap()
        );
        assert!(hub.handle_start(0, ready).unwrap());
    }

    #[test]
    fn fighting_accepts_periodic_clock_refresh_without_changing_peer_phase() {
        let (mut hub, _clients) = make_hub(None, 64, AuthorityPeerHubConfig::default());
        force_fighting(&mut hub);

        assert!(
            hub.handle_clock_probe(
                0,
                ClockProbe {
                    match_id: hub.manifest.match_id,
                    peer_id: peer(0),
                    probe_id: ClockProbeId::new(99).unwrap(),
                },
            )
            .unwrap()
        );
        assert_eq!(hub.peer_phase(peer(0)), Some(AuthorityPeerPhase::Fighting));
        assert_eq!(hub.metrics().security_violations, 0);
    }

    #[test]
    fn in_match_repair_keeps_accepting_authenticated_gameplay_input() {
        let (mut hub, _clients) = make_hub(None, 64, AuthorityPeerHubConfig::default());
        force_fighting(&mut hub);
        hub.peers[0].as_mut().unwrap().phase = AuthorityPeerPhase::RepairSyncInFlight;

        assert!(hub.handle_input(0, batch(1, 0, 0)).unwrap());
        assert_eq!(hub.metrics().input_batches_accepted, 1);
        assert_eq!(hub.metrics().input_batches_rejected, 0);
        assert_eq!(hub.metrics().security_violations, 0);

        let report = hub.try_advance(neutral_bot).unwrap().1.unwrap();
        assert!(matches!(
            report.committed_inputs.by_seat[0].unwrap().origin,
            AuthorityInputOrigin::Peer(owner) if owner == peer(0)
        ));
    }

    #[test]
    fn four_authenticated_peers_ingest_only_their_owned_seats() {
        let (mut hub, mut clients) = make_hub(None, 64, AuthorityPeerHubConfig::default());
        force_fighting(&mut hub);
        for (index, client) in clients.iter_mut().enumerate() {
            assert!(matches!(
                client.queue_message(WireMessage::InputBatch(batch(1, index, index))),
                Ok(QueueDisposition::Queued | QueueDisposition::ReplacedLatest)
            ));
            client.pump(SimTick(5));
        }
        hub.pump_network(SimTick(5)).unwrap();
        let report = hub.try_advance(neutral_bot).unwrap().1.unwrap();
        assert_eq!(report.committed_inputs.len(), 4);
        for index in 0..MAX_SEATS {
            let record = report.committed_inputs.by_seat[index].unwrap();
            assert_eq!(record.origin, AuthorityInputOrigin::Peer(peer(index)));
            assert_eq!(record.status, AuthorityInputStatus::Committed);
        }
    }

    #[test]
    fn slow_peer_backpressure_isolated_from_other_clients_and_authority_ticks() {
        let mut config = AuthorityPeerHubConfig::default();
        config.runtime.outbound_capacity = 8;
        config.runtime.max_send_datagrams_per_pump = 4;
        let (mut hub, mut clients) = make_hub(None, 4, config);
        force_fighting(&mut hub);
        let mut fast_state_messages = 0;
        for tick in 1..=20 {
            hub.try_advance(neutral_bot).unwrap();
            hub.pump_network(SimTick(5 + tick)).unwrap();
            // Client zero is intentionally never pumped.
            for client in clients.iter_mut().skip(1) {
                client.pump(SimTick(5 + tick));
                while let Some(event) = client.try_next_event() {
                    if matches!(
                        event,
                        RuntimeEvent::Message(WireMessage::StateHashAndAcks(_))
                            | RuntimeEvent::Message(WireMessage::StateDeltaAndAcks(_))
                    ) {
                        fast_state_messages += 1;
                    }
                }
            }
        }
        assert_eq!(hub.authority.simulation().current_tick(), SimTick(20));
        assert!(fast_state_messages > 0);
        assert!(
            hub.peers[0]
                .as_ref()
                .unwrap()
                .runtime
                .metrics()
                .send_would_block
                > 0
        );
    }

    #[test]
    fn spoofed_peer_claim_is_rejected_without_disturbing_other_connections() {
        let (mut hub, mut clients) = make_hub(None, 64, AuthorityPeerHubConfig::default());
        force_fighting(&mut hub);
        let victim_connection = hub.connection_for_peer(peer(0)).unwrap();
        let other_connection = hub.connection_for_peer(peer(1)).unwrap();

        clients[0]
            .queue_message(WireMessage::InputBatch(batch(1, 1, 1)))
            .unwrap();
        clients[0].pump(SimTick(5));
        hub.pump_network(SimTick(5)).unwrap();

        assert!(hub.connection_for_peer(peer(0)).is_none());
        assert_eq!(hub.connection_for_peer(peer(1)), Some(other_connection));
        assert_eq!(hub.peer_phase(peer(0)), Some(AuthorityPeerPhase::Closing));
        hub.pump_network(SimTick(6)).unwrap();
        clients[0].pump(SimTick(6));
        hub.pump_network(SimTick(7)).unwrap();
        assert!(matches!(
            hub.detach(victim_connection),
            Err(AuthorityPeerHubError::StaleConnection(_))
        ));
        assert_eq!(hub.metrics.spoofed_messages, 1);
    }

    #[test]
    fn malformed_datagrams_disconnect_only_the_abusive_endpoint() {
        let mut config = AuthorityPeerHubConfig::default();
        config.runtime.abuse_warning_threshold = 1;
        config.runtime.abuse_disconnect_threshold = 1;
        let manifest = manifest();
        let simulation = ToySimulation {
            match_id: manifest.match_id,
            tick: SimTick::ZERO,
            salt: 1,
            finish_tick: None,
        };
        let mut hub = AuthorityPeerHub::new(
            manifest,
            simulation,
            AuthorityInputConfig::default(),
            &roster(),
            config,
        )
        .unwrap();
        let (mut malicious, authority_endpoint) = InProcessEndpoint::pair(8).unwrap();
        hub.attach_initial(peer(0), user(0), authority_endpoint)
            .unwrap();
        let (_healthy, healthy_authority) = InProcessEndpoint::pair(8).unwrap();
        hub.attach_initial(peer(1), user(1), healthy_authority)
            .unwrap();
        assert_eq!(
            malicious.try_send(AfcDatagram::try_from_slice(b"not-afc").unwrap()),
            SendOutcome::Sent
        );
        hub.pump_network(SimTick::ZERO).unwrap();
        assert!(hub.connection_for_peer(peer(0)).is_none());
        assert!(hub.connection_for_peer(peer(1)).is_some());
        assert_eq!(hub.metrics.malformed_or_abusive_disconnects, 1);
    }

    #[test]
    fn active_platform_ban_rejects_initial_and_reconnect_admission() {
        let manifest = manifest();
        let simulation = ToySimulation {
            match_id: manifest.match_id,
            tick: SimTick::ZERO,
            salt: 1,
            finish_tick: None,
        };
        let mut bans = LocalBanRegistry::default();
        bans.record_permanent(user(0), BanReason::PlatformBan, SimTick::ZERO)
            .unwrap();
        let mut hub = AuthorityPeerHub::new_with_ban_provider(
            manifest,
            simulation,
            AuthorityInputConfig::default(),
            &roster(),
            AuthorityPeerHubConfig::default(),
            bans,
        )
        .unwrap();
        let (_client, authority_endpoint) = InProcessEndpoint::pair(8).unwrap();
        assert!(matches!(
            hub.attach_initial(peer(0), user(0), authority_endpoint),
            Err(AuthorityPeerHubError::ActiveBan)
        ));

        let (_client, authority_endpoint) = InProcessEndpoint::pair(8).unwrap();
        assert!(matches!(
            hub.attach_reconnect(
                user(0),
                ReconnectClaim {
                    match_id: manifest.match_id,
                    peer_id: peer(0),
                    last_confirmed_tick: SimTick::ZERO,
                },
                authority_endpoint,
            ),
            Err(AuthorityPeerHubError::ActiveBan)
        ));
        assert_eq!(hub.metrics().active_ban_rejections, 2);
        assert_eq!(hub.connection_for_peer(peer(0)), None);
    }

    #[test]
    fn authentication_revocation_queues_typed_disconnect_and_blocks_reconnect() {
        let (mut hub, _clients) = make_hub(None, 64, AuthorityPeerHubConfig::default());
        let connection = hub.connection_for_peer(peer(0)).unwrap();
        hub.revoke_authentication(connection).unwrap();
        assert_eq!(hub.connection_for_peer(peer(0)), None);
        assert_eq!(hub.metrics().temporary_bans, 1);
        assert_eq!(hub.metrics().typed_disconnects_queued, 1);
        assert!(
            hub.ban_provider_mut()
                .lookup(user(0), SimTick::ZERO)
                .is_some()
        );

        let (_client, authority_endpoint) = InProcessEndpoint::pair(64).unwrap();
        assert!(matches!(
            hub.attach_reconnect(
                user(0),
                ReconnectClaim {
                    match_id: manifest().match_id,
                    peer_id: peer(0),
                    last_confirmed_tick: SimTick::ZERO,
                },
                authority_endpoint,
            ),
            Err(AuthorityPeerHubError::ActiveBan)
        ));
    }

    #[test]
    fn abusive_peer_isolated_while_healthy_peer_receives_canonical_result() {
        let mut config = AuthorityPeerHubConfig::default();
        config.countdown_lead_ticks = 1;
        config.runtime.abuse_warning_threshold = 1;
        config.runtime.abuse_disconnect_threshold = 1;
        let manifest = manifest();
        let simulation = ToySimulation {
            match_id: manifest.match_id,
            tick: SimTick::ZERO,
            salt: 7,
            finish_tick: Some(SimTick(2)),
        };
        let mut hub = AuthorityPeerHub::new(
            manifest,
            simulation,
            AuthorityInputConfig::default(),
            &roster(),
            config,
        )
        .unwrap();
        let (mut malicious, authority_endpoint) = InProcessEndpoint::pair(256).unwrap();
        hub.attach_initial(peer(0), user(0), authority_endpoint)
            .unwrap();
        let mut healthy = Vec::new();
        for index in 1..MAX_AUTHORITY_PEERS {
            let (client_endpoint, authority_endpoint) = InProcessEndpoint::pair(256).unwrap();
            hub.attach_initial(peer(index), user(index), authority_endpoint)
                .unwrap();
            healthy.push(
                NetworkRuntime::new(
                    client_endpoint,
                    PeerRole::Client,
                    compatibility(),
                    config.runtime,
                )
                .unwrap(),
            );
        }
        force_fighting(&mut hub);

        assert_eq!(
            malicious.try_send(AfcDatagram::try_from_slice(b"not-afc").unwrap()),
            SendOutcome::Sent
        );
        hub.pump_network(SimTick(6)).unwrap();
        assert!(hub.connection_for_peer(peer(0)).is_none());
        assert!(hub.connection_for_peer(peer(1)).is_some());

        let first = hub.try_advance(neutral_bot).unwrap().1.unwrap();
        assert_eq!(first.tick, SimTick(1));
        let final_report = hub.try_advance(neutral_bot).unwrap().1.unwrap();
        let result = hub.confirmed_result().unwrap();
        assert_eq!(result.final_tick, final_report.tick);
        assert_eq!(result.final_state_hash, final_report.state_hash);

        hub.pump_network(SimTick(7)).unwrap();
        for client in &mut healthy {
            client.pump(SimTick(7));
        }
        let received = healthy[0]
            .try_next_event()
            .into_iter()
            .chain(std::iter::from_fn(|| healthy[0].try_next_event()))
            .any(|event| {
                matches!(
                    event,
                    RuntimeEvent::Message(WireMessage::ResultIdentifier(candidate))
                        if candidate == result
                )
            });
        assert!(received);
        assert!(hub.observability().counters().packets_in > 0);
        assert!(
            hub.observability()
                .audit()
                .iter()
                .any(|record| record.code == OnlineAuditCode::PeerKicked)
        );
    }

    #[test]
    fn reconnect_is_transactional_and_stale_connection_generation_fails_closed() {
        let mut config = AuthorityPeerHubConfig::default();
        config.reconnect = ReconnectPolicy {
            grace_ticks: 120,
            neutral_input_ticks: 2,
        };
        let (mut hub, mut clients) = make_hub(None, 256, config);
        force_fighting(&mut hub);
        let old_connection = hub.connection_for_peer(peer(0)).unwrap();
        hub.detach(old_connection).unwrap();
        let (client_endpoint, authority_endpoint) = InProcessEndpoint::pair(256).unwrap();
        let mut reconnect_client = NetworkRuntime::new(
            client_endpoint,
            PeerRole::Client,
            compatibility(),
            config.runtime,
        )
        .unwrap();
        let new_connection = hub
            .attach_reconnect(
                user(0),
                ReconnectClaim {
                    match_id: manifest().match_id,
                    peer_id: peer(0),
                    last_confirmed_tick: SimTick::ZERO,
                },
                authority_endpoint,
            )
            .unwrap();
        assert_ne!(old_connection.generation(), new_connection.generation());
        reconnect_client
            .queue_message(WireMessage::Handshake(Handshake {
                compatibility: compatibility(),
            }))
            .unwrap();
        reconnect_client.pump(SimTick(5));
        hub.pump_network(SimTick(5)).unwrap();

        for probe in 1..=CLOCK_SYNC_SAMPLE_CAPACITY {
            reconnect_client
                .queue_message(WireMessage::ClockProbe(ClockProbe {
                    match_id: manifest().match_id,
                    peer_id: peer(0),
                    probe_id: ClockProbeId::new(probe as u32).unwrap(),
                }))
                .unwrap();
        }
        reconnect_client.pump(SimTick(5));

        let first = hub.try_advance(neutral_bot).unwrap().1.unwrap();
        assert!(matches!(
            first.committed_inputs.by_seat[0].unwrap().origin,
            AuthorityInputOrigin::MissingSubstitute
        ));
        assert_eq!(
            hub.peer_phase(peer(0)),
            Some(AuthorityPeerPhase::ReconnectSyncInFlight)
        );

        let mut begin = None;
        for round in 0..16 {
            hub.pump_network(SimTick(6 + round)).unwrap();
            reconnect_client.pump(SimTick(6 + round));
            while let Some(event) = reconnect_client.try_next_event() {
                if let RuntimeEvent::Message(WireMessage::ResyncBegin(message)) = event {
                    begin = Some(message);
                }
            }
            if begin.is_some()
                && hub.peers[usize::from(new_connection.slot())]
                    .as_ref()
                    .and_then(|peer| peer.transfer.as_ref())
                    .is_some_and(|pending| pending.stage == TransferStage::WaitingApplied)
            {
                break;
            }
        }
        let begin = begin.expect("reconnect received transfer declaration");
        reconnect_client
            .queue_message(WireMessage::ResyncApplied(ResyncApplied {
                match_id: begin.match_id,
                transfer_id: begin.transfer_id,
                peer_id: peer(0),
                snapshot_tick: begin.snapshot_tick,
                snapshot_hash: begin.snapshot_hash,
            }))
            .unwrap();
        reconnect_client.pump(SimTick(30));
        hub.pump_network(SimTick(30)).unwrap();
        assert_eq!(
            hub.peer_phase(peer(0)),
            Some(AuthorityPeerPhase::ReconnectAwaitingClock)
        );
        // The three probes arrived before ResyncApplied and were held. Their
        // replies are released only now, proving acknowledgement causality.
        hub.pump_network(SimTick(31)).unwrap();
        hub.pump_network(SimTick(32)).unwrap();
        assert_eq!(hub.peer_phase(peer(0)), Some(AuthorityPeerPhase::Fighting));
        assert_eq!(hub.metrics.reconnects_completed, 1);
        assert!(matches!(
            hub.detach(old_connection),
            Err(AuthorityPeerHubError::StaleConnection(_))
        ));
        clients.clear();
    }

    #[test]
    fn grace_expiry_permanently_keeps_bot_origin_and_rejects_same_identity_reclaim() {
        let mut config = AuthorityPeerHubConfig::default();
        config.reconnect = ReconnectPolicy {
            grace_ticks: 4,
            neutral_input_ticks: 2,
        };
        let (mut hub, _clients) = make_hub(None, 256, config);
        force_fighting(&mut hub);
        let old_connection = hub.connection_for_peer(peer(0)).unwrap();
        hub.detach(old_connection).unwrap();

        let first = hub.try_advance(neutral_bot).unwrap().1.unwrap();
        assert_eq!(
            first.committed_inputs.by_seat[0].unwrap().origin,
            AuthorityInputOrigin::MissingSubstitute
        );
        for expected_tick in 2..=5 {
            let report = hub.try_advance(neutral_bot).unwrap().1.unwrap();
            assert_eq!(report.tick, SimTick(expected_tick));
            assert_eq!(
                report.committed_inputs.by_seat[0].unwrap().origin,
                AuthorityInputOrigin::DisconnectedBot(peer(0))
            );
        }

        assert_eq!(hub.metrics().reconnect_grace_expirations, 1);
        let expiry_records: Vec<_> = hub
            .observability()
            .audit()
            .iter()
            .filter(|record| record.code == OnlineAuditCode::ReconnectGraceExpired)
            .collect();
        assert_eq!(expiry_records.len(), 1);
        assert_eq!(expiry_records[0].scope.peer_id, Some(peer(0)));
        assert_eq!(expiry_records[0].scope.tick, Some(SimTick(4)));
        assert_eq!(expiry_records[0].value_a, 0b0001);
        assert_eq!(expiry_records[0].value_b, 4);

        let (_client, authority_endpoint) = InProcessEndpoint::pair(256).unwrap();
        assert!(matches!(
            hub.attach_reconnect(
                user(0),
                ReconnectClaim {
                    match_id: manifest().match_id,
                    peer_id: peer(0),
                    last_confirmed_tick: SimTick(3),
                },
                authority_endpoint,
            ),
            Err(AuthorityPeerHubError::Reconnect(
                ReconnectError::GraceExpired
            ))
        ));
        assert_eq!(hub.connection_for_peer(peer(0)), None);
        assert_eq!(hub.metrics().reconnect_grace_expirations, 1);
        assert_eq!(
            hub.observability()
                .audit()
                .iter()
                .filter(|record| record.code == OnlineAuditCode::ReconnectGraceExpired)
                .count(),
            1
        );
    }

    #[test]
    fn future_resync_request_detaches_only_offender_and_authority_keeps_advancing() {
        let (mut hub, mut clients) = make_hub(None, 256, AuthorityPeerHubConfig::default());
        force_fighting(&mut hub);
        let first = hub.try_advance(neutral_bot).unwrap().1.unwrap();
        let healthy_connection = hub.connection_for_peer(peer(1)).unwrap();

        clients[0]
            .queue_message(WireMessage::ResyncRequest(ResyncRequest {
                match_id: hub.manifest.match_id,
                peer_id: peer(0),
                reason: ResyncReason::HashMismatch,
                last_confirmed_tick: SimTick(first.tick.get() + 10_000),
                last_confirmed_hash: StateHash(0xBAD0_F00D),
            }))
            .unwrap();
        clients[0].pump(SimTick(6));

        // A peer-authored future tick is an attributed protocol violation, not
        // a ResyncTransferError that escapes the authority pump.
        hub.pump_network(SimTick(6)).unwrap();
        assert_eq!(hub.connection_for_peer(peer(0)), None);
        assert_eq!(hub.connection_for_peer(peer(1)), Some(healthy_connection));
        assert_eq!(hub.metrics().malformed_or_abusive_disconnects, 1);
        assert!(hub.metrics().security_violations >= 1);

        let next = hub.try_advance(neutral_bot).unwrap().1.unwrap();
        assert_eq!(next.tick, first.tick.next());
        hub.pump_network(SimTick(7)).unwrap();
        assert_eq!(hub.connection_for_peer(peer(1)), Some(healthy_connection));
    }

    #[test]
    fn forged_baseline_acks_detach_only_offenders_without_starting_repairs() {
        let (mut hub, mut clients) = make_hub(None, 256, AuthorityPeerHubConfig::default());
        force_fighting(&mut hub);
        let first = hub.try_advance(neutral_bot).unwrap().1.unwrap();
        let latest = hub.state_history.latest_baseline().unwrap();
        assert_eq!(latest.tick, first.tick);
        let healthy_connection = hub.connection_for_peer(peer(2)).unwrap();

        let future = batch(2, 0, 0)
            .with_state_baseline_ack(StateBaselineAck {
                tick: SimTick(latest.tick.get() + 10_000),
                hash: StateHash(0xBAD0_F00D),
            })
            .unwrap();
        let wrong_retained_hash = batch(2, 1, 1)
            .with_state_baseline_ack(StateBaselineAck {
                tick: latest.tick,
                hash: StateHash(latest.hash.0 ^ 1),
            })
            .unwrap();
        clients[0]
            .queue_message(WireMessage::InputBatch(future))
            .unwrap();
        clients[1]
            .queue_message(WireMessage::InputBatch(wrong_retained_hash))
            .unwrap();
        clients[0].pump(SimTick(6));
        clients[1].pump(SimTick(6));

        hub.pump_network(SimTick(6)).unwrap();
        assert_eq!(hub.connection_for_peer(peer(0)), None);
        assert_eq!(hub.connection_for_peer(peer(1)), None);
        assert_eq!(hub.connection_for_peer(peer(2)), Some(healthy_connection));
        assert_eq!(hub.metrics().resyncs_started, 0);
        assert_eq!(hub.peer_state.metrics().future_acknowledgements, 1);
        assert_eq!(hub.peer_state.metrics().authority_hash_mismatches, 1);

        let next = hub.try_advance(neutral_bot).unwrap().1.unwrap();
        assert_eq!(next.tick, first.tick.next());
        hub.pump_network(SimTick(7)).unwrap();
        assert_eq!(hub.connection_for_peer(peer(2)), Some(healthy_connection));
    }

    #[test]
    fn expired_ack_requires_a_subsequent_client_request_to_start_repair() {
        let mut config = AuthorityPeerHubConfig::default();
        config.state_history_entries = 2;
        config.state_delta_interval_ticks = 1;
        let (mut hub, mut clients) = make_hub(None, 256, config);
        force_fighting(&mut hub);

        let first = hub.try_advance(neutral_bot).unwrap().1.unwrap();
        let baseline_1 = hub.state_history.latest_baseline().unwrap();
        assert_eq!(baseline_1.tick, first.tick);
        let healthy_connection = hub.connection_for_peer(peer(1)).unwrap();

        for (index, client) in clients.iter_mut().enumerate().take(2) {
            client
                .queue_message(WireMessage::InputBatch(
                    batch(2, index, index)
                        .with_state_baseline_ack(StateBaselineAck {
                            tick: baseline_1.tick,
                            hash: baseline_1.hash,
                        })
                        .unwrap(),
                ))
                .unwrap();
            client.pump(SimTick(6));
        }
        hub.pump_network(SimTick(6)).unwrap();

        let second = hub.try_advance(neutral_bot).unwrap().1.unwrap();
        let baseline_2 = hub.state_history.latest_baseline().unwrap();
        assert_eq!(baseline_2.tick, second.tick);
        clients[1]
            .queue_message(WireMessage::InputBatch(
                batch(3, 1, 1)
                    .with_state_baseline_ack(StateBaselineAck {
                        tick: baseline_2.tick,
                        hash: baseline_2.hash,
                    })
                    .unwrap(),
            ))
            .unwrap();
        clients[1].pump(SimTick(7));
        hub.pump_network(SimTick(7)).unwrap();

        let third = hub.try_advance(neutral_bot).unwrap().1.unwrap();
        let baseline_3 = hub.state_history.latest_baseline().unwrap();
        assert_eq!(baseline_3.tick, third.tick);
        assert_eq!(hub.peer_phase(peer(0)), Some(AuthorityPeerPhase::Fighting));
        assert_eq!(hub.metrics().resyncs_started, 0);
        assert!(
            hub.peers[hub.peer_index(peer(0)).unwrap()]
                .as_ref()
                .unwrap()
                .transfer
                .is_none()
        );

        clients[0]
            .queue_message(WireMessage::ResyncRequest(ResyncRequest {
                match_id: hub.manifest.match_id,
                peer_id: peer(0),
                reason: ResyncReason::HistoryExpired,
                last_confirmed_tick: baseline_1.tick,
                last_confirmed_hash: baseline_1.hash,
            }))
            .unwrap();
        clients[0].pump(SimTick(8));
        hub.pump_network(SimTick(8)).unwrap();
        assert_eq!(
            hub.peer_phase(peer(0)),
            Some(AuthorityPeerPhase::RepairSyncInFlight)
        );
        assert_eq!(hub.metrics().resyncs_started, 1);

        for (input_tick, offered_tick, offered_hash, now) in [
            (4, SimTick::ZERO, StateHash(0xBAD0), SimTick(10)),
            (
                5,
                baseline_1.tick,
                StateHash(baseline_1.hash.0 ^ 0xBAD1),
                SimTick(11),
            ),
        ] {
            clients[0]
                .queue_message(WireMessage::InputBatch(
                    batch(input_tick, 0, 0)
                        .with_state_baseline_ack(StateBaselineAck {
                            tick: offered_tick,
                            hash: offered_hash,
                        })
                        .unwrap(),
                ))
                .unwrap();
            clients[0].pump(now);
            hub.pump_network(now).unwrap();
            assert_eq!(
                hub.peer_state.acknowledged_baseline(peer(0)).unwrap(),
                Some(baseline_1)
            );
            assert_eq!(hub.metrics().resyncs_started, 1);
        }

        clients[1]
            .queue_message(WireMessage::InputBatch(
                batch(4, 1, 1)
                    .with_state_baseline_ack(StateBaselineAck {
                        tick: baseline_3.tick,
                        hash: baseline_3.hash,
                    })
                    .unwrap(),
            ))
            .unwrap();
        clients[1].pump(SimTick(12));
        hub.pump_network(SimTick(12)).unwrap();
        assert_eq!(
            hub.peer_state.acknowledged_baseline(peer(1)).unwrap(),
            Some(baseline_3)
        );
        assert_eq!(hub.peer_state.metrics().expired_acknowledgements, 2);
        assert_eq!(hub.connection_for_peer(peer(1)), Some(healthy_connection));
        assert_eq!(hub.metrics().resyncs_started, 1);
    }

    #[test]
    fn repair_budget_enforces_cooldown_and_window_cap() {
        let mut budget = PeerRepairRequestBudget::default();
        assert_eq!(
            budget.try_start(SimTick(10), 10, 100, 3),
            PeerRepairBudgetOutcome::Allowed
        );
        assert_eq!(
            budget.try_start(SimTick(19), 10, 100, 3),
            PeerRepairBudgetOutcome::Cooldown
        );
        assert_eq!(
            budget.try_start(SimTick(20), 10, 100, 3),
            PeerRepairBudgetOutcome::Allowed
        );
        assert_eq!(
            budget.try_start(SimTick(30), 10, 100, 3),
            PeerRepairBudgetOutcome::Allowed
        );
        assert_eq!(
            budget.try_start(SimTick(40), 10, 100, 3),
            PeerRepairBudgetOutcome::Exhausted
        );
        assert_eq!(
            budget.try_start(SimTick(110), 10, 100, 3),
            PeerRepairBudgetOutcome::Allowed
        );
    }

    #[test]
    fn first_repair_inside_cooldown_is_scored_and_dropped_without_disconnect() {
        let mut config = AuthorityPeerHubConfig::default();
        config.peer_repair_request_cooldown_ticks = 10;
        config.peer_repair_request_window_ticks = 100;
        config.max_peer_repair_requests_per_window = 3;
        let (mut hub, mut clients) = make_hub(None, 256, config);
        force_fighting(&mut hub);
        hub.try_advance(neutral_bot).unwrap();

        for now in [SimTick(10), SimTick(11)] {
            if now == SimTick(11) {
                let peer = hub.peers[0].as_mut().expect("first request is retained");
                peer.transfer = None;
                peer.phase = AuthorityPeerPhase::Fighting;
            }
            clients[0]
                .queue_message(WireMessage::ResyncRequest(ResyncRequest {
                    match_id: hub.manifest.match_id,
                    peer_id: peer(0),
                    reason: ResyncReason::HistoryExpired,
                    last_confirmed_tick: SimTick::ZERO,
                    last_confirmed_hash: StateHash(0),
                }))
                .unwrap();
            clients[0].pump(now);
            hub.pump_network(now).unwrap();
        }

        assert!(hub.connection_for_peer(peer(0)).is_some());
        assert_eq!(hub.metrics().resyncs_started, 1);
        assert_eq!(hub.metrics().repair_requests_rate_limited, 1);
        assert_eq!(hub.metrics().security_violations, 1);
        assert_eq!(hub.metrics().spoofed_messages, 0);
        assert_eq!(hub.metrics().malformed_or_abusive_disconnects, 0);
    }

    #[test]
    fn sequential_peer_repairs_hit_fixed_budget_while_healthy_peer_survives() {
        let mut config = AuthorityPeerHubConfig::default();
        config.peer_repair_request_cooldown_ticks = 1;
        config.peer_repair_request_window_ticks = 100;
        config.max_peer_repair_requests_per_window = 3;
        let (mut hub, mut clients) = make_hub(None, 256, config);
        force_fighting(&mut hub);
        let first = hub.try_advance(neutral_bot).unwrap().1.unwrap();
        let healthy_connection = hub.connection_for_peer(peer(1)).unwrap();

        for attempt in 0..9 {
            if attempt > 0 {
                let malicious = hub.peers[0].as_mut().expect("peer lives through budget");
                // Model a client that completed each prior transfer, then
                // immediately asks the authority to allocate another one.
                malicious.transfer = None;
                malicious.phase = AuthorityPeerPhase::Fighting;
            }
            let now = SimTick(10 + attempt * 2);
            clients[0]
                .queue_message(WireMessage::ResyncRequest(ResyncRequest {
                    match_id: hub.manifest.match_id,
                    peer_id: peer(0),
                    reason: ResyncReason::HistoryExpired,
                    last_confirmed_tick: SimTick::ZERO,
                    last_confirmed_hash: StateHash(0),
                }))
                .unwrap();
            clients[0].pump(now);
            hub.pump_network(now).unwrap();
            if attempt < 8 {
                assert!(hub.connection_for_peer(peer(0)).is_some());
            }
        }

        assert_eq!(hub.metrics().resyncs_started, 3);
        assert_eq!(hub.metrics().repair_request_budgets_exhausted, 6);
        assert_eq!(hub.connection_for_peer(peer(0)), None);
        assert_eq!(hub.connection_for_peer(peer(1)), Some(healthy_connection));
        let next = hub.try_advance(neutral_bot).unwrap().1.unwrap();
        assert_eq!(next.tick, first.tick.next());
    }

    #[test]
    fn crossing_client_repair_request_coalesces_and_result_is_delivered() {
        let (mut hub, mut clients) = make_hub(Some(2), 256, AuthorityPeerHubConfig::default());
        force_fighting(&mut hub);
        let first = hub.try_advance(neutral_bot).unwrap().1.unwrap();
        let snapshot = hub
            .authority()
            .snapshot_at(first.tick)
            .expect("first tick snapshot")
            .clone();
        let authority_request = ResyncRequest {
            match_id: hub.manifest.match_id,
            peer_id: peer(0),
            reason: ResyncReason::HistoryExpired,
            last_confirmed_tick: SimTick::ZERO,
            last_confirmed_hash: StateHash(0),
        };
        hub.prepare_transfer_for_peer(
            peer(0),
            authority_request,
            snapshot,
            TransferPurpose::Repair,
        )
        .unwrap();
        let index = hub.peer_index(peer(0)).unwrap();
        hub.peers[index].as_mut().unwrap().phase = AuthorityPeerPhase::RepairSyncInFlight;
        let active_transfer = hub.peers[index]
            .as_ref()
            .unwrap()
            .transfer
            .as_ref()
            .unwrap()
            .transfer
            .begin()
            .transfer_id;

        clients[0]
            .queue_message(WireMessage::ResyncRequest(ResyncRequest {
                match_id: hub.manifest.match_id,
                peer_id: peer(0),
                reason: ResyncReason::HashMismatch,
                last_confirmed_tick: first.tick,
                last_confirmed_hash: first.state_hash,
            }))
            .unwrap();
        clients[0].pump(SimTick(6));
        hub.pump_network(SimTick(6)).unwrap();
        assert_eq!(hub.metrics().repair_requests_coalesced, 1);
        assert_eq!(hub.metrics().security_violations, 0);
        assert_eq!(
            hub.peers[index]
                .as_ref()
                .unwrap()
                .transfer
                .as_ref()
                .unwrap()
                .transfer
                .begin()
                .transfer_id,
            active_transfer
        );

        let mut begin = None;
        for tick in 7..20 {
            hub.pump_network(SimTick(tick)).unwrap();
            clients[0].pump(SimTick(tick));
            while let Some(event) = clients[0].try_next_event() {
                if let RuntimeEvent::Message(WireMessage::ResyncBegin(message)) = event
                    && message.transfer_id == active_transfer
                {
                    begin = Some(message);
                }
            }
            if begin.is_some()
                && hub.peers[index]
                    .as_ref()
                    .and_then(|peer| peer.transfer.as_ref())
                    .is_some_and(|pending| pending.stage == TransferStage::WaitingApplied)
            {
                break;
            }
        }
        let begin = begin.expect("repair declaration reached client");
        clients[0]
            .queue_message(WireMessage::ResyncApplied(ResyncApplied {
                match_id: begin.match_id,
                transfer_id: begin.transfer_id,
                peer_id: peer(0),
                snapshot_tick: begin.snapshot_tick,
                snapshot_hash: begin.snapshot_hash,
            }))
            .unwrap();
        clients[0].pump(SimTick(20));
        hub.pump_network(SimTick(20)).unwrap();
        assert_eq!(hub.peer_phase(peer(0)), Some(AuthorityPeerPhase::Fighting));

        let final_report = hub.try_advance(neutral_bot).unwrap().1.unwrap();
        let result = hub.confirmed_result().unwrap();
        assert_eq!(result.final_state_hash, final_report.state_hash);
        hub.pump_network(SimTick(21)).unwrap();
        clients[0].pump(SimTick(21));
        let mut received = false;
        while let Some(event) = clients[0].try_next_event() {
            if matches!(
                event,
                RuntimeEvent::Message(WireMessage::ResultIdentifier(candidate)) if candidate == result
            ) {
                received = true;
            }
        }
        assert!(received);
        assert!(hub.connection_for_peer(peer(0)).is_some());
    }

    #[test]
    fn final_result_survives_slow_receiver_and_is_delivered_once() {
        let mut config = AuthorityPeerHubConfig::default();
        config.runtime.reliable_retry_interval_ticks = 2;
        let (mut hub, mut clients) = make_hub(Some(2), 64, config);
        force_fighting(&mut hub);
        hub.try_advance(neutral_bot).unwrap();
        hub.try_advance(neutral_bot).unwrap();
        assert_eq!(hub.confirmed_result().unwrap().result_id.get(), 9001);
        assert_eq!(
            hub.try_advance(neutral_bot).unwrap().0,
            AuthorityAdvanceOutcome::Finished
        );

        for tick in [7, 9, 11] {
            hub.pump_network(SimTick(tick)).unwrap();
        }
        let client = &mut clients[0];
        client.pump(SimTick(12));
        let mut results = Vec::new();
        while let Some(event) = client.try_next_event() {
            if let RuntimeEvent::Message(WireMessage::ResultIdentifier(result)) = event {
                results.push(result);
            }
        }
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], hub.confirmed_result().unwrap());
        client.pump(SimTick(13));
        hub.pump_network(SimTick(13)).unwrap();
        client.pump(SimTick(14));
        assert!(!matches!(
            client.try_next_event(),
            Some(RuntimeEvent::Message(WireMessage::ResultIdentifier(_)))
        ));
    }
}
