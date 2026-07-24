//! Production listen-authority composition.
//!
//! A listen match still has one canonical, render-free authority.  The host is
//! not granted an in-memory gameplay shortcut: its predicted client connects to
//! the authority through the same AFC handshake, synchronization, input,
//! correction, reconnect, and result protocol as every Steam peer.
//!
//! The authority worker is the sole owner of [`LiveSimulationDriver`] and
//! [`AuthorityPeerHub`].  The application thread communicates with it only
//! through bounded commands and a latest-wins status mailbox.  Consequently a
//! blocked renderer cannot pause authority deadlines or mutate canonical state.

use core::fmt;
use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
#[cfg(test)]
use std::sync::mpsc::TryRecvError;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::authority::AuthoritySimulation;
use crate::authority_input::AuthorityInputConfig;
use crate::authority_peer_hub::{
    AuthorityAdvanceOutcome, AuthorityClosingCompletion, AuthorityConnectionId, AuthorityPeerHub,
    AuthorityPeerHubConfig, AuthorityPeerHubError, AuthorityPeerHubMetrics, AuthorityPeerPhase,
    AuthorityShutdownState, MAX_AUTHORITY_PEERS,
};
use crate::authority_thread::{AUTHORITY_THREAD_TICK_RATE_HZ, SixtyHzSchedule};
use crate::headless::{HeadlessBuildError, HeadlessMatchConfig, build_headless_simulation};
use crate::live_authority::{LiveSimulationDriver, LiveSimulationError};
use crate::multiplayer_diagnostics::{
    AcceptedInputTail, AuthorityDiagnosticsWriter, AuthorityIncidentBundle,
    AuthorityOperationalSnapshot, AuthorityWorkerDiagnosticSnapshot, DiagnosticsCounterSnapshot,
    DiagnosticsCounters, REPLAY_CHECKPOINT_INTERVAL_TICKS, REPLAY_KEYFRAME_INTERVAL_TICKS,
    SecurityDiagnosticSnapshot, resolve_diagnostics_root,
};
use crate::multiplayer_observability::{MultiplayerCounterSnapshot, ServerTickDistribution};
use crate::network_codec::ResultIdentifier;
use crate::network_io::{
    AfcDatagram, DEFAULT_IN_PROCESS_QUEUE_PACKETS, InProcessConfigError, InProcessEndpoint,
    NonBlockingDatagramEndpoint, ReceiveOutcome, SendOutcome,
};
use crate::network_protocol::{
    InputButtons, InputFrame, InputSequence, PeerId, QuantizedAxis, ReconnectClaim, SeatId,
    SeatOwner, SimTick,
};
use crate::online_failure::{
    OnlineFailure, OnlineFailureCode, OnlineFailureSeverity, OnlineRecoveryAction,
};
use crate::reconnect::{AuthenticatedPeer, AuthenticatedUserId};
use crate::remote_online_client::{
    RemoteOnlineClient, RemoteOnlineClientConfig, RemoteOnlineClientStartError,
};
use crate::replay::{AuthorityReplayRecorder, Replay};
use crate::steam_transport::SteamDatagramEndpoint;

pub const DEFAULT_LISTEN_COMMAND_CAPACITY: usize = 32;
pub const MAX_LISTEN_COMMAND_CAPACITY: usize = 256;
pub const DEFAULT_LISTEN_COMMANDS_PER_SERVICE: usize = 32;
pub const MAX_LISTEN_COMMANDS_PER_SERVICE: usize = 256;
pub const DEFAULT_LISTEN_EVENT_CAPACITY: usize = 32;
pub const MAX_LISTEN_EVENT_CAPACITY: usize = 256;
pub const DEFAULT_OPERATIONAL_EXPORT_INTERVAL_TICKS: u32 = 3_600;
pub const MIN_OPERATIONAL_EXPORT_INTERVAL_TICKS: u32 = 600;

const LISTEN_MAILBOX_SIGNAL_CAPACITY: usize = 1;
const LISTEN_DIAGNOSTICS_FINALIZE_GRACE: Duration = Duration::from_millis(50);
const NANOS_PER_TICK: u64 = 1_000_000_000 / AUTHORITY_THREAD_TICK_RATE_HZ as u64;

/// Type-erased endpoint owned by an authority peer link.
///
/// This is deliberately a closed enum rather than a trait object: dispatch is
/// allocation-free, every variant is `Send`, and adding a production transport
/// requires an explicit review at this trust boundary.
pub enum ListenDatagramEndpoint {
    InProcess(InProcessEndpoint),
    Steam(SteamDatagramEndpoint),
}

impl fmt::Debug for ListenDatagramEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InProcess(_) => "ListenDatagramEndpoint::InProcess",
            Self::Steam(_) => "ListenDatagramEndpoint::Steam",
        })
    }
}

impl From<InProcessEndpoint> for ListenDatagramEndpoint {
    fn from(endpoint: InProcessEndpoint) -> Self {
        Self::InProcess(endpoint)
    }
}

impl From<SteamDatagramEndpoint> for ListenDatagramEndpoint {
    fn from(endpoint: SteamDatagramEndpoint) -> Self {
        Self::Steam(endpoint)
    }
}

impl NonBlockingDatagramEndpoint for ListenDatagramEndpoint {
    fn try_send(&mut self, datagram: AfcDatagram) -> SendOutcome {
        match self {
            Self::InProcess(endpoint) => endpoint.try_send(datagram),
            Self::Steam(endpoint) => endpoint.try_send(datagram),
        }
    }

    fn try_receive(&mut self) -> ReceiveOutcome {
        match self {
            Self::InProcess(endpoint) => endpoint.try_receive(),
            Self::Steam(endpoint) => endpoint.try_receive(),
        }
    }
}

/// Immutable authenticated mapping frozen when the lobby manifest is committed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListenAuthenticatedRoster {
    peers: [AuthenticatedPeer; MAX_AUTHORITY_PEERS],
    len: u8,
    host: AuthenticatedPeer,
}

impl ListenAuthenticatedRoster {
    pub fn new(
        match_config: &HeadlessMatchConfig,
        host: AuthenticatedPeer,
        peers: impl IntoIterator<Item = AuthenticatedPeer>,
    ) -> Result<Self, ListenRosterError> {
        let empty = AuthenticatedPeer {
            peer_id: PeerId::default(),
            user_id: AuthenticatedUserId::default(),
        };
        let mut roster = Self {
            peers: [empty; MAX_AUTHORITY_PEERS],
            len: 0,
            host,
        };
        for peer in peers {
            if usize::from(roster.len) == MAX_AUTHORITY_PEERS {
                return Err(ListenRosterError::Capacity);
            }
            if peer.peer_id.validate().is_err() || peer.user_id.get() == 0 {
                return Err(ListenRosterError::InvalidIdentity);
            }
            if roster
                .as_slice()
                .iter()
                .any(|existing| existing.peer_id == peer.peer_id)
            {
                return Err(ListenRosterError::DuplicatePeer(peer.peer_id));
            }
            if roster
                .as_slice()
                .iter()
                .any(|existing| existing.user_id == peer.user_id)
            {
                return Err(ListenRosterError::DuplicateUser(peer.user_id));
            }
            if !match_config
                .manifest
                .ownership
                .peer_owns_any_seat(peer.peer_id)
            {
                return Err(ListenRosterError::PeerOwnsNoSeat(peer.peer_id));
            }
            roster.peers[usize::from(roster.len)] = peer;
            roster.len = roster.len.saturating_add(1);
        }
        if roster.is_empty() {
            return Err(ListenRosterError::Empty);
        }
        let Some(committed_host) = roster
            .as_slice()
            .iter()
            .find(|peer| peer.peer_id == host.peer_id)
        else {
            return Err(ListenRosterError::HostMissing);
        };
        if committed_host.user_id != host.user_id {
            return Err(ListenRosterError::HostIdentityMismatch);
        }
        for assignment in match_config.manifest.ownership.as_slice() {
            let SeatOwner::Peer(owner) = assignment.owner else {
                continue;
            };
            if !roster.as_slice().iter().any(|peer| peer.peer_id == owner) {
                return Err(ListenRosterError::ManifestPeerMissing(owner));
            }
        }
        Ok(roster)
    }

    pub const fn len(self) -> usize {
        self.len as usize
    }

    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    pub const fn host(self) -> AuthenticatedPeer {
        self.host
    }

    pub fn as_slice(&self) -> &[AuthenticatedPeer] {
        &self.peers[..self.len()]
    }

    pub fn iter(&self) -> impl Iterator<Item = AuthenticatedPeer> + '_ {
        self.as_slice().iter().copied()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListenRosterError {
    Empty,
    Capacity,
    InvalidIdentity,
    DuplicatePeer(PeerId),
    DuplicateUser(AuthenticatedUserId),
    PeerOwnsNoSeat(PeerId),
    HostMissing,
    HostIdentityMismatch,
    ManifestPeerMissing(PeerId),
}

impl fmt::Display for ListenRosterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid listen-authority roster: {self:?}")
    }
}

impl std::error::Error for ListenRosterError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListenAuthorityConfig {
    pub input: AuthorityInputConfig,
    pub hub: AuthorityPeerHubConfig,
    pub command_capacity: usize,
    pub max_commands_per_service: usize,
    pub event_capacity: usize,
    pub host_endpoint_queue_packets: usize,
    /// Zero disables periodic snapshots. Terminal replay/incident persistence
    /// remains enabled for production listen matches.
    pub operational_export_interval_ticks: u32,
}

impl Default for ListenAuthorityConfig {
    fn default() -> Self {
        Self {
            input: AuthorityInputConfig::default(),
            hub: AuthorityPeerHubConfig::default(),
            command_capacity: DEFAULT_LISTEN_COMMAND_CAPACITY,
            max_commands_per_service: DEFAULT_LISTEN_COMMANDS_PER_SERVICE,
            event_capacity: DEFAULT_LISTEN_EVENT_CAPACITY,
            host_endpoint_queue_packets: DEFAULT_IN_PROCESS_QUEUE_PACKETS,
            operational_export_interval_ticks: DEFAULT_OPERATIONAL_EXPORT_INTERVAL_TICKS,
        }
    }
}

impl ListenAuthorityConfig {
    pub fn validate(self) -> Result<(), ListenAuthorityConfigError> {
        self.input
            .validate()
            .map_err(|_| ListenAuthorityConfigError::Input)?;
        if self.command_capacity == 0 || self.command_capacity > MAX_LISTEN_COMMAND_CAPACITY {
            return Err(ListenAuthorityConfigError::CommandCapacity);
        }
        if self.max_commands_per_service == 0
            || self.max_commands_per_service > MAX_LISTEN_COMMANDS_PER_SERVICE
        {
            return Err(ListenAuthorityConfigError::CommandServiceLimit);
        }
        if self.event_capacity == 0 || self.event_capacity > MAX_LISTEN_EVENT_CAPACITY {
            return Err(ListenAuthorityConfigError::EventCapacity);
        }
        if self.host_endpoint_queue_packets == 0 {
            return Err(ListenAuthorityConfigError::HostEndpointCapacity);
        }
        if self.operational_export_interval_ticks != 0
            && self.operational_export_interval_ticks < MIN_OPERATIONAL_EXPORT_INTERVAL_TICKS
        {
            return Err(ListenAuthorityConfigError::OperationalExportCadence);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListenAuthorityConfigError {
    Input,
    CommandCapacity,
    CommandServiceLimit,
    EventCapacity,
    HostEndpointCapacity,
    OperationalExportCadence,
}

impl fmt::Display for ListenAuthorityConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid listen-authority worker configuration: {self:?}"
        )
    }
}

impl std::error::Error for ListenAuthorityConfigError {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ListenAuthorityPhase {
    #[default]
    Starting,
    WaitingForPeers,
    Synchronizing,
    Countdown,
    Fighting,
    Results,
    Draining,
    Drained,
    Stopped,
    Failed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ListenAuthorityPeerStatus {
    pub expected: bool,
    pub peer_id: PeerId,
    pub user_id: AuthenticatedUserId,
    pub connection: Option<AuthorityConnectionId>,
    pub phase: Option<AuthorityPeerPhase>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ListenAuthorityWorkerMetrics {
    pub command_queue_capacity: usize,
    pub command_queue_depth: usize,
    pub command_queue_high_water: usize,
    pub commands_queued: u64,
    pub commands_full: u64,
    pub commands_disconnected: u64,
    pub commands_processed: u64,
    pub worker_iterations: u64,
    pub simulated_ticks: u64,
    pub waiting_iterations: u64,
    pub late_tick_starts: u64,
    pub maximum_tick_lateness_ns: u64,
    pub total_service_duration_ns: u64,
    pub maximum_service_duration_ns: u64,
    pub over_budget_iterations: u64,
    pub status_publications: u64,
    pub status_notifications_coalesced: u64,
    pub events_published: u64,
    pub events_dropped: u64,
    pub event_queue_high_water: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ListenAuthorityStatus {
    pub phase: ListenAuthorityPhase,
    pub network_tick: SimTick,
    pub authority_tick: SimTick,
    pub countdown_start_tick: Option<SimTick>,
    pub result: Option<ResultIdentifier>,
    pub peers: [ListenAuthorityPeerStatus; MAX_AUTHORITY_PEERS],
    pub peer_count: u8,
    pub hub: AuthorityPeerHubMetrics,
    pub observability: MultiplayerCounterSnapshot,
    pub server_ticks: ServerTickDistribution,
    pub worker: ListenAuthorityWorkerMetrics,
    pub diagnostics: DiagnosticsCounterSnapshot,
    pub failure: Option<OnlineFailure>,
}

impl ListenAuthorityStatus {
    pub fn peer(self, peer_id: PeerId) -> Option<ListenAuthorityPeerStatus> {
        self.peers[..usize::from(self.peer_count)]
            .iter()
            .copied()
            .find(|peer| peer.peer_id == peer_id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListenAuthorityOperation {
    AttachInitial,
    AttachReconnect,
    Detach,
    RevokeAuthentication,
    EnforcePlatformBan,
    BeginShutdown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListenAuthorityCommandRejection {
    UnknownPeer,
    IdentityMismatch,
    AlreadyConnected,
    NotConnected,
    StartupClosed,
    ReconnectNotAvailable,
    ReconnectDenied,
    Capacity,
    Protocol,
    Internal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListenAuthorityEvent {
    InitialAttached {
        peer_id: PeerId,
        connection: AuthorityConnectionId,
    },
    ReconnectAttached {
        peer_id: PeerId,
        connection: AuthorityConnectionId,
    },
    Detached {
        peer_id: PeerId,
        connection: AuthorityConnectionId,
    },
    AuthenticationRevoked {
        peer_id: PeerId,
        user_id: AuthenticatedUserId,
        connection: AuthorityConnectionId,
    },
    PlatformBanEnforced {
        user_id: AuthenticatedUserId,
        peer_id: Option<PeerId>,
        connection: Option<AuthorityConnectionId>,
    },
    TerminalDrained {
        peer_id: PeerId,
        user_id: AuthenticatedUserId,
        connection: AuthorityConnectionId,
        disconnect: Option<crate::network_protocol::DisconnectMessage>,
        completion: AuthorityClosingCompletion,
    },
    ShutdownStarted,
    ShutdownDrained,
    CommandRejected {
        operation: ListenAuthorityOperation,
        peer_id: PeerId,
        reason: ListenAuthorityCommandRejection,
    },
    SecurityCommandRejected {
        operation: ListenAuthorityOperation,
        peer_id: Option<PeerId>,
        user_id: Option<AuthenticatedUserId>,
        reason: ListenAuthorityCommandRejection,
    },
}

impl ListenAuthorityEvent {
    const fn is_lifecycle_fact(&self) -> bool {
        matches!(
            self,
            Self::InitialAttached { .. }
                | Self::ReconnectAttached { .. }
                | Self::Detached { .. }
                | Self::AuthenticationRevoked { .. }
                | Self::PlatformBanEnforced { .. }
                | Self::TerminalDrained { .. }
                | Self::ShutdownStarted
                | Self::ShutdownDrained
        )
    }
}

pub enum ListenAuthorityCommand {
    AttachInitial {
        peer_id: PeerId,
        user_id: AuthenticatedUserId,
        endpoint: ListenDatagramEndpoint,
    },
    AttachReconnect {
        user_id: AuthenticatedUserId,
        claim: ReconnectClaim,
        endpoint: ListenDatagramEndpoint,
    },
    Detach {
        connection: AuthorityConnectionId,
    },
    RevokeAuthentication {
        connection: AuthorityConnectionId,
    },
    EnforcePlatformBan {
        user_id: AuthenticatedUserId,
    },
    BeginShutdown,
    #[cfg(test)]
    AdvanceManual(u16),
    #[cfg(test)]
    FinishCanonicalMatch,
    #[cfg(test)]
    BlockCommandService(Arc<AtomicBool>),
    Stop,
}

impl fmt::Debug for ListenAuthorityCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AttachInitial {
                peer_id, user_id, ..
            } => formatter
                .debug_struct("AttachInitial")
                .field("peer_id", peer_id)
                .field("user_id", user_id)
                .finish_non_exhaustive(),
            Self::AttachReconnect { user_id, claim, .. } => formatter
                .debug_struct("AttachReconnect")
                .field("user_id", user_id)
                .field("claim", claim)
                .finish_non_exhaustive(),
            Self::Detach { connection } => formatter
                .debug_struct("Detach")
                .field("connection", connection)
                .finish(),
            Self::RevokeAuthentication { connection } => formatter
                .debug_struct("RevokeAuthentication")
                .field("connection", connection)
                .finish(),
            Self::EnforcePlatformBan { user_id } => formatter
                .debug_struct("EnforcePlatformBan")
                .field("user_id", user_id)
                .finish(),
            Self::BeginShutdown => formatter.write_str("BeginShutdown"),
            #[cfg(test)]
            Self::AdvanceManual(iterations) => formatter
                .debug_tuple("AdvanceManual")
                .field(iterations)
                .finish(),
            #[cfg(test)]
            Self::FinishCanonicalMatch => formatter.write_str("FinishCanonicalMatch"),
            #[cfg(test)]
            Self::BlockCommandService(_) => formatter.write_str("BlockCommandService(..)"),
            Self::Stop => formatter.write_str("Stop"),
        }
    }
}

#[derive(Debug)]
pub enum ListenAuthoritySubmitOutcome {
    Queued,
    Full(ListenAuthorityCommand),
    Disconnected(ListenAuthorityCommand),
}

impl ListenAuthoritySubmitOutcome {
    pub const fn is_queued(&self) -> bool {
        matches!(self, Self::Queued)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListenAuthorityExit {
    Stopped,
    CommandChannelDisconnected,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListenAuthorityTerminal {
    pub exit: ListenAuthorityExit,
    pub status: ListenAuthorityStatus,
}

#[derive(Debug)]
pub enum ListenAuthorityStartError {
    InvalidConfig(ListenAuthorityConfigError),
    InvalidMatch(HeadlessBuildError),
    InvalidRoster(ListenRosterError),
    TickRateMismatch { manifest_hz: u16 },
    HostEndpoint(InProcessConfigError),
    Spawn(io::Error),
    WorkerBootstrap(OnlineFailure),
    WorkerExitedDuringBootstrap,
    HostClient(RemoteOnlineClientStartError),
    InvalidDiagnosticsRoot,
}

impl fmt::Display for ListenAuthorityStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "listen authority could not start: {self:?}")
    }
}

impl std::error::Error for ListenAuthorityStartError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListenAuthorityJoinError {
    WorkerPanicked,
}

impl fmt::Display for ListenAuthorityJoinError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("listen-authority worker panicked")
    }
}

impl std::error::Error for ListenAuthorityJoinError {}

#[derive(Default)]
struct SharedListenMetrics {
    command_queue_depth: AtomicUsize,
    command_queue_high_water: AtomicUsize,
    commands_queued: AtomicU64,
    commands_full: AtomicU64,
    commands_disconnected: AtomicU64,
    commands_processed: AtomicU64,
    worker_iterations: AtomicU64,
    simulated_ticks: AtomicU64,
    waiting_iterations: AtomicU64,
    late_tick_starts: AtomicU64,
    maximum_tick_lateness_ns: AtomicU64,
    total_service_duration_ns: AtomicU64,
    maximum_service_duration_ns: AtomicU64,
    over_budget_iterations: AtomicU64,
    status_publications: AtomicU64,
    status_notifications_coalesced: AtomicU64,
    events_published: AtomicU64,
    events_dropped: AtomicU64,
    event_queue_high_water: AtomicUsize,
    diagnostics: Arc<DiagnosticsCounters>,
}

impl SharedListenMetrics {
    fn snapshot(&self, command_capacity: usize) -> ListenAuthorityWorkerMetrics {
        ListenAuthorityWorkerMetrics {
            command_queue_capacity: command_capacity,
            command_queue_depth: self
                .command_queue_depth
                .load(Ordering::Relaxed)
                .min(command_capacity),
            command_queue_high_water: self
                .command_queue_high_water
                .load(Ordering::Relaxed)
                .min(command_capacity),
            commands_queued: self.commands_queued.load(Ordering::Relaxed),
            commands_full: self.commands_full.load(Ordering::Relaxed),
            commands_disconnected: self.commands_disconnected.load(Ordering::Relaxed),
            commands_processed: self.commands_processed.load(Ordering::Relaxed),
            worker_iterations: self.worker_iterations.load(Ordering::Relaxed),
            simulated_ticks: self.simulated_ticks.load(Ordering::Relaxed),
            waiting_iterations: self.waiting_iterations.load(Ordering::Relaxed),
            late_tick_starts: self.late_tick_starts.load(Ordering::Relaxed),
            maximum_tick_lateness_ns: self.maximum_tick_lateness_ns.load(Ordering::Relaxed),
            total_service_duration_ns: self.total_service_duration_ns.load(Ordering::Relaxed),
            maximum_service_duration_ns: self.maximum_service_duration_ns.load(Ordering::Relaxed),
            over_budget_iterations: self.over_budget_iterations.load(Ordering::Relaxed),
            status_publications: self.status_publications.load(Ordering::Relaxed),
            status_notifications_coalesced: self
                .status_notifications_coalesced
                .load(Ordering::Relaxed),
            events_published: self.events_published.load(Ordering::Relaxed),
            events_dropped: self.events_dropped.load(Ordering::Relaxed),
            event_queue_high_water: self.event_queue_high_water.load(Ordering::Relaxed),
        }
    }
}

impl From<ListenAuthorityWorkerMetrics> for AuthorityWorkerDiagnosticSnapshot {
    fn from(value: ListenAuthorityWorkerMetrics) -> Self {
        Self {
            command_queue_capacity: value.command_queue_capacity as u64,
            command_queue_depth: value.command_queue_depth as u64,
            command_queue_high_water: value.command_queue_high_water as u64,
            commands_queued: value.commands_queued,
            commands_full: value.commands_full,
            commands_disconnected: value.commands_disconnected,
            commands_processed: value.commands_processed,
            worker_iterations: value.worker_iterations,
            simulated_ticks: value.simulated_ticks,
            waiting_iterations: value.waiting_iterations,
            late_tick_starts: value.late_tick_starts,
            maximum_tick_lateness_ns: value.maximum_tick_lateness_ns,
            total_service_duration_ns: value.total_service_duration_ns,
            maximum_service_duration_ns: value.maximum_service_duration_ns,
            over_budget_iterations: value.over_budget_iterations,
            status_publications: value.status_publications,
            status_notifications_coalesced: value.status_notifications_coalesced,
            events_published: value.events_published,
            events_dropped: value.events_dropped,
            event_queue_high_water: value.event_queue_high_water as u64,
        }
    }
}

struct ListenMailboxState {
    status: ListenAuthorityStatus,
    lifecycle_events: VecDeque<ListenAuthorityEvent>,
    telemetry_events: VecDeque<ListenAuthorityEvent>,
    lifecycle_overflowed: bool,
    terminal: Option<ListenAuthorityTerminal>,
}

struct ListenMailboxPublisher {
    state: Arc<Mutex<ListenMailboxState>>,
    signal: SyncSender<()>,
    event_capacity: usize,
    metrics: Arc<SharedListenMetrics>,
}

impl ListenMailboxPublisher {
    fn publish_status(&self, status: ListenAuthorityStatus) {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .status = status;
        self.metrics
            .status_publications
            .fetch_add(1, Ordering::Relaxed);
        match self.signal.try_send(()) {
            Ok(()) => {}
            Err(TrySendError::Full(())) => {
                self.metrics
                    .status_notifications_coalesced
                    .fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Disconnected(())) => {}
        }
    }

    fn publish_event(&self, event: ListenAuthorityEvent) {
        let (depth, published) = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if event.is_lifecycle_fact() {
                if state.lifecycle_events.len() == self.event_capacity {
                    // Lifecycle identity and shutdown facts are never evicted.
                    // Saturation raises a sticky worker-fatal fence instead.
                    state.lifecycle_overflowed = true;
                    self.metrics.events_dropped.fetch_add(1, Ordering::Relaxed);
                    (state.lifecycle_events.len(), false)
                } else {
                    state.lifecycle_events.push_back(event);
                    (state.lifecycle_events.len(), true)
                }
            } else {
                if state.telemetry_events.len() == self.event_capacity {
                    state.telemetry_events.pop_front();
                    self.metrics.events_dropped.fetch_add(1, Ordering::Relaxed);
                }
                state.telemetry_events.push_back(event);
                (state.telemetry_events.len(), true)
            }
        };
        if published {
            self.metrics
                .events_published
                .fetch_add(1, Ordering::Relaxed);
        }
        self.metrics
            .event_queue_high_water
            .fetch_max(depth, Ordering::Relaxed);
        let _ = self.signal.try_send(());
    }

    fn lifecycle_overflowed(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .lifecycle_overflowed
    }

    fn finish(&self, terminal: ListenAuthorityTerminal) {
        {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.status = terminal.status;
            state.terminal = Some(terminal);
        }
        let _ = self.signal.try_send(());
    }
}

struct ListenMailboxInbox {
    state: Arc<Mutex<ListenMailboxState>>,
    signal: Receiver<()>,
}

impl ListenMailboxInbox {
    fn status(&self) -> ListenAuthorityStatus {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .status
    }

    fn terminal(&self) -> Option<ListenAuthorityTerminal> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .terminal
    }

    fn drain(&mut self) -> ListenAuthorityUpdate {
        while self.signal.try_recv().is_ok() {}
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let mut events: Vec<ListenAuthorityEvent> = state.lifecycle_events.drain(..).collect();
        events.extend(state.telemetry_events.drain(..));
        ListenAuthorityUpdate {
            status: state.status,
            events,
            terminal: state.terminal,
        }
    }

    #[cfg(test)]
    fn wait_for_iteration(&self, expected: u64, timeout: Duration) -> bool {
        let deadline = Instant::now().checked_add(timeout);
        loop {
            if self.status().worker.worker_iterations >= expected {
                return true;
            }
            if self.terminal().is_some() {
                return false;
            }
            let remaining = deadline
                .map(|deadline| deadline.saturating_duration_since(Instant::now()))
                .unwrap_or(timeout);
            match self.signal.recv_timeout(remaining) {
                Ok(()) => {}
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return false,
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ListenAuthorityUpdate {
    pub status: ListenAuthorityStatus,
    pub events: Vec<ListenAuthorityEvent>,
    pub terminal: Option<ListenAuthorityTerminal>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListenWorkerClockMode {
    Realtime,
    #[cfg(test)]
    Manual,
}

/// Main-thread handle for the canonical listen authority.
pub struct ListenAuthorityWorker {
    commands: Option<SyncSender<ListenAuthorityCommand>>,
    force_shutdown: Arc<AtomicBool>,
    join: Option<JoinHandle<ListenAuthorityTerminal>>,
    inbox: ListenMailboxInbox,
    metrics: Arc<SharedListenMetrics>,
    command_capacity: usize,
}

impl ListenAuthorityWorker {
    pub fn status(&self) -> ListenAuthorityStatus {
        self.inbox.status()
    }

    pub fn result(&self) -> Option<ResultIdentifier> {
        self.status().result
    }

    pub fn terminal(&self) -> Option<ListenAuthorityTerminal> {
        self.inbox.terminal()
    }

    pub fn metrics(&self) -> ListenAuthorityWorkerMetrics {
        self.metrics.snapshot(self.command_capacity)
    }

    pub fn drain_update(&mut self) -> ListenAuthorityUpdate {
        self.inbox.drain()
    }

    pub fn try_attach_initial(
        &self,
        peer_id: PeerId,
        user_id: AuthenticatedUserId,
        endpoint: impl Into<ListenDatagramEndpoint>,
    ) -> ListenAuthoritySubmitOutcome {
        self.try_submit(ListenAuthorityCommand::AttachInitial {
            peer_id,
            user_id,
            endpoint: endpoint.into(),
        })
    }

    pub fn try_attach_reconnect(
        &self,
        user_id: AuthenticatedUserId,
        claim: ReconnectClaim,
        endpoint: impl Into<ListenDatagramEndpoint>,
    ) -> ListenAuthoritySubmitOutcome {
        self.try_submit(ListenAuthorityCommand::AttachReconnect {
            user_id,
            claim,
            endpoint: endpoint.into(),
        })
    }

    pub fn try_detach(&self, connection: AuthorityConnectionId) -> ListenAuthoritySubmitOutcome {
        self.try_submit(ListenAuthorityCommand::Detach { connection })
    }

    /// Applies a platform authentication-session revocation on the authority
    /// thread. The bounded command queue preserves the hub's single-owner
    /// model and returns ownership of the command under backpressure.
    pub fn try_revoke_authentication(
        &self,
        connection: AuthorityConnectionId,
    ) -> ListenAuthoritySubmitOutcome {
        self.try_submit(ListenAuthorityCommand::RevokeAuthentication { connection })
    }

    /// Records a permanent platform/publisher ban and removes any matching
    /// live link on the authority thread. Only the authenticated numeric user
    /// identity crosses this boundary.
    pub fn try_enforce_platform_ban(
        &self,
        user_id: AuthenticatedUserId,
    ) -> ListenAuthoritySubmitOutcome {
        self.try_submit(ListenAuthorityCommand::EnforcePlatformBan { user_id })
    }

    /// Begins graceful authority teardown without blocking the application
    /// thread. Unlike [`Self::request_stop`], this leaves the worker alive long
    /// enough to ACK-track every remote terminal or reach its bounded deadline.
    pub fn try_begin_graceful_shutdown(&self) -> ListenAuthoritySubmitOutcome {
        self.try_submit(ListenAuthorityCommand::BeginShutdown)
    }

    pub fn try_submit(&self, command: ListenAuthorityCommand) -> ListenAuthoritySubmitOutcome {
        let Some(sender) = self.commands.as_ref() else {
            self.metrics
                .commands_disconnected
                .fetch_add(1, Ordering::Relaxed);
            return ListenAuthoritySubmitOutcome::Disconnected(command);
        };
        let depth = self
            .metrics
            .command_queue_depth
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        match sender.try_send(command) {
            Ok(()) => {
                self.metrics
                    .command_queue_high_water
                    .fetch_max(depth.min(self.command_capacity), Ordering::Relaxed);
                self.metrics.commands_queued.fetch_add(1, Ordering::Relaxed);
                ListenAuthoritySubmitOutcome::Queued
            }
            Err(TrySendError::Full(command)) => {
                self.metrics
                    .command_queue_depth
                    .fetch_sub(1, Ordering::AcqRel);
                self.metrics.commands_full.fetch_add(1, Ordering::Relaxed);
                ListenAuthoritySubmitOutcome::Full(command)
            }
            Err(TrySendError::Disconnected(command)) => {
                self.metrics
                    .command_queue_depth
                    .fetch_sub(1, Ordering::AcqRel);
                self.metrics
                    .commands_disconnected
                    .fetch_add(1, Ordering::Relaxed);
                ListenAuthoritySubmitOutcome::Disconnected(command)
            }
        }
    }

    /// Emergency bounded fallback. This intentionally bypasses graceful
    /// terminal delivery; normal online teardown should submit
    /// [`Self::try_begin_graceful_shutdown`] and wait for `Drained`.
    pub fn request_stop(&mut self) {
        self.force_shutdown.store(true, Ordering::Release);
        if let Some(commands) = self.commands.take() {
            let _ = commands.try_send(ListenAuthorityCommand::Stop);
        }
    }

    pub fn join(&mut self) -> Result<ListenAuthorityTerminal, ListenAuthorityJoinError> {
        self.commands.take();
        let Some(join) = self.join.take() else {
            return self
                .terminal()
                .ok_or(ListenAuthorityJoinError::WorkerPanicked);
        };
        join.join()
            .map_err(|_| ListenAuthorityJoinError::WorkerPanicked)
    }

    pub fn shutdown(&mut self) -> Result<ListenAuthorityTerminal, ListenAuthorityJoinError> {
        self.request_stop();
        self.join()
    }

    #[cfg(test)]
    fn advance_manual(&self, iterations: u16) -> bool {
        if iterations == 0 {
            return false;
        }
        let before = self.metrics().worker_iterations;
        if !self
            .try_submit(ListenAuthorityCommand::AdvanceManual(iterations))
            .is_queued()
        {
            return false;
        }
        self.inbox.wait_for_iteration(
            before.saturating_add(u64::from(iterations)),
            Duration::from_secs(5),
        )
    }

    #[cfg(test)]
    fn finish_canonical_match(&self) -> bool {
        self.try_submit(ListenAuthorityCommand::FinishCanonicalMatch)
            .is_queued()
    }
}

impl Drop for ListenAuthorityWorker {
    fn drop(&mut self) {
        self.request_stop();
        let _ = self.join();
    }
}

/// Host-side composition.  The two fields remain separately observable so the
/// render application can sample/project the client without touching authority
/// state.
pub struct ListenOnlineMatch {
    pub authority: ListenAuthorityWorker,
    pub host_client: RemoteOnlineClient,
}

impl ListenOnlineMatch {
    pub fn spawn(
        match_config: HeadlessMatchConfig,
        roster: ListenAuthenticatedRoster,
        authority_config: ListenAuthorityConfig,
        host_client_config: RemoteOnlineClientConfig,
    ) -> Result<Self, ListenAuthorityStartError> {
        let diagnostics = resolve_diagnostics_root();
        Self::spawn_with_clock(
            match_config,
            roster,
            authority_config,
            host_client_config,
            ListenWorkerClockMode::Realtime,
            Some(diagnostics.path),
            diagnostics.invalid_override,
        )
    }

    pub fn spawn_from_peers(
        match_config: HeadlessMatchConfig,
        host: AuthenticatedPeer,
        peers: impl IntoIterator<Item = AuthenticatedPeer>,
        authority_config: ListenAuthorityConfig,
        host_client_config: RemoteOnlineClientConfig,
    ) -> Result<Self, ListenAuthorityStartError> {
        let roster = ListenAuthenticatedRoster::new(&match_config, host, peers)
            .map_err(ListenAuthorityStartError::InvalidRoster)?;
        Self::spawn(match_config, roster, authority_config, host_client_config)
    }

    /// Explicit absolute root for managed deployments and deterministic tests.
    /// Shipping callers normally use [`Self::spawn`] and `AFC_DIAGNOSTICS_ROOT`.
    pub fn spawn_with_diagnostics_root(
        match_config: HeadlessMatchConfig,
        roster: ListenAuthenticatedRoster,
        authority_config: ListenAuthorityConfig,
        host_client_config: RemoteOnlineClientConfig,
        diagnostics_root: PathBuf,
    ) -> Result<Self, ListenAuthorityStartError> {
        if !diagnostics_root.is_absolute() {
            return Err(ListenAuthorityStartError::InvalidDiagnosticsRoot);
        }
        Self::spawn_with_clock(
            match_config,
            roster,
            authority_config,
            host_client_config,
            ListenWorkerClockMode::Realtime,
            Some(diagnostics_root),
            false,
        )
    }

    fn spawn_with_clock(
        match_config: HeadlessMatchConfig,
        roster: ListenAuthenticatedRoster,
        authority_config: ListenAuthorityConfig,
        host_client_config: RemoteOnlineClientConfig,
        clock: ListenWorkerClockMode,
        diagnostics_root: Option<PathBuf>,
        invalid_diagnostics_override: bool,
    ) -> Result<Self, ListenAuthorityStartError> {
        authority_config
            .validate()
            .map_err(ListenAuthorityStartError::InvalidConfig)?;
        match_config
            .validate()
            .map_err(ListenAuthorityStartError::InvalidMatch)?;
        if u32::from(match_config.manifest.tick_rate_hz) != AUTHORITY_THREAD_TICK_RATE_HZ {
            return Err(ListenAuthorityStartError::TickRateMismatch {
                manifest_hz: match_config.manifest.tick_rate_hz,
            });
        }
        // Revalidate a possibly hand-constructed fixed roster against this
        // exact manifest before either thread receives an endpoint.
        let roster = ListenAuthenticatedRoster::new(&match_config, roster.host(), roster.iter())
            .map_err(ListenAuthorityStartError::InvalidRoster)?;
        let (host_client_endpoint, host_authority_endpoint) =
            InProcessEndpoint::pair(authority_config.host_endpoint_queue_packets)
                .map_err(ListenAuthorityStartError::HostEndpoint)?;

        let mailbox_state = Arc::new(Mutex::new(ListenMailboxState {
            status: ListenAuthorityStatus::default(),
            lifecycle_events: VecDeque::with_capacity(authority_config.event_capacity),
            telemetry_events: VecDeque::with_capacity(authority_config.event_capacity),
            lifecycle_overflowed: false,
            terminal: None,
        }));
        let (signal_tx, signal_rx) = mpsc::sync_channel(LISTEN_MAILBOX_SIGNAL_CAPACITY);
        let metrics = Arc::new(SharedListenMetrics::default());
        if invalid_diagnostics_override {
            metrics.diagnostics.observe_invalid_root_override();
        }
        let publisher = ListenMailboxPublisher {
            state: Arc::clone(&mailbox_state),
            signal: signal_tx,
            event_capacity: authority_config.event_capacity,
            metrics: Arc::clone(&metrics),
        };
        let (command_tx, command_rx) = mpsc::sync_channel(authority_config.command_capacity);
        let force_shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&force_shutdown);
        let worker_metrics = Arc::clone(&metrics);
        let (startup_tx, startup_rx) = mpsc::sync_channel(1);
        let worker_config = match_config.clone();
        let join = thread::Builder::new()
            .name("afc-listen-authority-60hz".to_owned())
            .spawn(move || {
                run_listen_authority_worker(
                    worker_config,
                    roster,
                    authority_config,
                    ListenDatagramEndpoint::InProcess(host_authority_endpoint),
                    clock,
                    command_rx,
                    &worker_shutdown,
                    worker_metrics,
                    publisher,
                    startup_tx,
                    diagnostics_root,
                )
            })
            .map_err(ListenAuthorityStartError::Spawn)?;

        let mut authority = ListenAuthorityWorker {
            commands: Some(command_tx),
            force_shutdown,
            join: Some(join),
            inbox: ListenMailboxInbox {
                state: mailbox_state,
                signal: signal_rx,
            },
            metrics,
            command_capacity: authority_config.command_capacity,
        };
        match startup_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(failure)) => {
                let _ = authority.join();
                return Err(ListenAuthorityStartError::WorkerBootstrap(failure));
            }
            Err(_) => {
                let _ = authority.join();
                return Err(ListenAuthorityStartError::WorkerExitedDuringBootstrap);
            }
        }

        let host_endpoint = ListenDatagramEndpoint::InProcess(host_client_endpoint);
        let host_client = match clock {
            ListenWorkerClockMode::Realtime => RemoteOnlineClient::spawn(
                host_endpoint,
                match_config,
                roster.host().peer_id,
                host_client_config,
            ),
            #[cfg(test)]
            ListenWorkerClockMode::Manual => RemoteOnlineClient::spawn_manual(
                host_endpoint,
                match_config,
                roster.host().peer_id,
                host_client_config,
            ),
        }
        .map_err(|error| {
            let _ = authority.shutdown();
            ListenAuthorityStartError::HostClient(error)
        })?;

        Ok(Self {
            authority,
            host_client,
        })
    }

    #[cfg(test)]
    fn spawn_manual(
        match_config: HeadlessMatchConfig,
        roster: ListenAuthenticatedRoster,
        authority_config: ListenAuthorityConfig,
        host_client_config: RemoteOnlineClientConfig,
    ) -> Result<Self, ListenAuthorityStartError> {
        Self::spawn_with_clock(
            match_config,
            roster,
            authority_config,
            host_client_config,
            ListenWorkerClockMode::Manual,
            None,
            false,
        )
    }

    #[cfg(test)]
    fn spawn_manual_with_diagnostics(
        match_config: HeadlessMatchConfig,
        roster: ListenAuthenticatedRoster,
        authority_config: ListenAuthorityConfig,
        host_client_config: RemoteOnlineClientConfig,
        diagnostics_root: PathBuf,
    ) -> Result<Self, ListenAuthorityStartError> {
        Self::spawn_with_clock(
            match_config,
            roster,
            authority_config,
            host_client_config,
            ListenWorkerClockMode::Manual,
            Some(diagnostics_root),
            false,
        )
    }
}

enum CommandService {
    Continue,
    BeginShutdown,
    Failed,
    Stop,
    #[cfg(test)]
    Advance(u16),
}

type LiveListenHub = AuthorityPeerHub<LiveSimulationDriver, ListenDatagramEndpoint>;

#[allow(clippy::too_many_arguments)]
fn run_listen_authority_worker(
    match_config: HeadlessMatchConfig,
    roster: ListenAuthenticatedRoster,
    config: ListenAuthorityConfig,
    host_endpoint: ListenDatagramEndpoint,
    clock: ListenWorkerClockMode,
    commands: Receiver<ListenAuthorityCommand>,
    force_shutdown: &AtomicBool,
    metrics: Arc<SharedListenMetrics>,
    publisher: ListenMailboxPublisher,
    startup: SyncSender<Result<(), OnlineFailure>>,
    diagnostics_root: Option<PathBuf>,
) -> ListenAuthorityTerminal {
    publisher.publish_status(ListenAuthorityStatus {
        phase: ListenAuthorityPhase::Starting,
        worker: metrics.snapshot(config.command_capacity),
        diagnostics: metrics.diagnostics.snapshot(),
        ..ListenAuthorityStatus::default()
    });
    let simulation = match build_headless_simulation(match_config.clone()) {
        Ok(simulation) => simulation,
        Err(_) => {
            let failure = listen_internal_failure(1);
            let _ = startup.send(Err(failure));
            return finish_failed_bootstrap(&publisher, &metrics, config.command_capacity, failure);
        }
    };
    let mut hub = match AuthorityPeerHub::new(
        match_config.manifest,
        simulation,
        config.input,
        roster.as_slice(),
        config.hub,
    ) {
        Ok(hub) => hub,
        Err(_) => {
            let failure = listen_internal_failure(2);
            let _ = startup.send(Err(failure));
            return finish_failed_bootstrap(&publisher, &metrics, config.command_capacity, failure);
        }
    };
    let host = roster.host();
    if hub
        .attach_initial(host.peer_id, host.user_id, host_endpoint)
        .is_err()
    {
        let failure = listen_internal_failure(3);
        let _ = startup.send(Err(failure));
        return finish_failed_bootstrap(&publisher, &metrics, config.command_capacity, failure);
    }

    let initial_snapshot = hub
        .authority()
        .snapshot_at(SimTick::ZERO)
        .expect("listen authority retains its validated initial snapshot")
        .clone();
    let mut latest_snapshot = initial_snapshot.clone();
    let mut accepted_input_tail = AcceptedInputTail::default();
    let mut replay_recorder = diagnostics_root.as_ref().and_then(|_| {
        match AuthorityReplayRecorder::new(*hub.manifest(), initial_snapshot) {
            Ok(recorder) => Some(recorder),
            Err(_) => {
                metrics.diagnostics.observe_recorder_failure();
                None
            }
        }
    });
    let mut diagnostics_writer = diagnostics_root.as_ref().and_then(|root| {
        match AuthorityDiagnosticsWriter::start(root.clone(), Arc::clone(&metrics.diagnostics)) {
            Ok(writer) => Some(writer),
            Err(_) => {
                metrics.diagnostics.observe_writer_start_failure();
                None
            }
        }
    });
    let mut deferred_replay: Option<Replay> = None;
    let _ = startup.send(Ok(()));

    let mut network_tick = SimTick::ZERO;
    let epoch = Instant::now();
    let mut schedule = SixtyHzSchedule::new();
    let mut server_ticks = ServerTickDistribution::default();
    let mut draining = false;
    let mut graceful_drained = false;
    #[cfg(test)]
    let mut manual_iterations = 0_u32;

    publisher.publish_status(make_status(
        &hub,
        roster,
        &metrics,
        config.command_capacity,
        ListenAuthorityPhase::WaitingForPeers,
        server_ticks,
        None,
    ));

    let (exit, failure) = 'worker: loop {
        if force_shutdown.load(Ordering::Acquire) {
            break (ListenAuthorityExit::Stopped, None);
        }

        match clock {
            ListenWorkerClockMode::Realtime => {
                let deadline = schedule.deadline();
                let mut serviced = 0;
                while serviced < config.max_commands_per_service {
                    if force_shutdown.load(Ordering::Acquire) {
                        break 'worker (ListenAuthorityExit::Stopped, None);
                    }
                    let now = Instant::now();
                    if now >= deadline {
                        break;
                    }
                    match commands.recv_timeout(deadline.duration_since(now)) {
                        Ok(command) => {
                            dequeue_command(&metrics);
                            serviced += 1;
                            let service =
                                service_command(command, &mut hub, &publisher, host.peer_id);
                            publish_hub_drain_events(&mut hub, &publisher);
                            if publisher.lifecycle_overflowed() {
                                break 'worker (
                                    ListenAuthorityExit::Failed,
                                    Some(listen_internal_failure(12)),
                                );
                            }
                            match service {
                                CommandService::Continue => {}
                                CommandService::BeginShutdown => draining = true,
                                CommandService::Failed => {
                                    break 'worker (
                                        ListenAuthorityExit::Failed,
                                        Some(listen_internal_failure(11)),
                                    );
                                }
                                CommandService::Stop => {
                                    break 'worker (ListenAuthorityExit::Stopped, None);
                                }
                                #[cfg(test)]
                                CommandService::Advance(_) => {}
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => {
                            if draining {
                                break;
                            }
                            break 'worker (ListenAuthorityExit::CommandChannelDisconnected, None);
                        }
                    }
                }
                let sleep_start = Instant::now();
                if sleep_start < deadline {
                    thread::sleep(deadline.duration_since(sleep_start));
                }
                let lateness = Instant::now().saturating_duration_since(deadline);
                if !lateness.is_zero() {
                    metrics.late_tick_starts.fetch_add(1, Ordering::Relaxed);
                    metrics
                        .maximum_tick_lateness_ns
                        .fetch_max(duration_ns(lateness), Ordering::Relaxed);
                }
            }
            #[cfg(test)]
            ListenWorkerClockMode::Manual => {
                while manual_iterations == 0 {
                    if force_shutdown.load(Ordering::Acquire) {
                        break 'worker (ListenAuthorityExit::Stopped, None);
                    }
                    let command = match commands.recv_timeout(Duration::from_millis(10)) {
                        Ok(command) => command,
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => {
                            if draining {
                                manual_iterations = 1;
                                break;
                            }
                            break 'worker (ListenAuthorityExit::CommandChannelDisconnected, None);
                        }
                    };
                    dequeue_command(&metrics);
                    let service = service_command(command, &mut hub, &publisher, host.peer_id);
                    publish_hub_drain_events(&mut hub, &publisher);
                    if publisher.lifecycle_overflowed() {
                        break 'worker (
                            ListenAuthorityExit::Failed,
                            Some(listen_internal_failure(12)),
                        );
                    }
                    match service {
                        CommandService::Continue => {}
                        CommandService::BeginShutdown => {
                            draining = true;
                            manual_iterations = manual_iterations.saturating_add(1);
                        }
                        CommandService::Failed => {
                            break 'worker (
                                ListenAuthorityExit::Failed,
                                Some(listen_internal_failure(11)),
                            );
                        }
                        CommandService::Stop => {
                            break 'worker (ListenAuthorityExit::Stopped, None);
                        }
                        CommandService::Advance(iterations) => {
                            manual_iterations =
                                manual_iterations.saturating_add(u32::from(iterations));
                        }
                    }
                }
                for _ in 0..config.max_commands_per_service.saturating_sub(1) {
                    match commands.try_recv() {
                        Ok(command) => {
                            dequeue_command(&metrics);
                            let service =
                                service_command(command, &mut hub, &publisher, host.peer_id);
                            publish_hub_drain_events(&mut hub, &publisher);
                            if publisher.lifecycle_overflowed() {
                                break 'worker (
                                    ListenAuthorityExit::Failed,
                                    Some(listen_internal_failure(12)),
                                );
                            }
                            match service {
                                CommandService::Continue => {}
                                CommandService::BeginShutdown => draining = true,
                                CommandService::Failed => {
                                    break 'worker (
                                        ListenAuthorityExit::Failed,
                                        Some(listen_internal_failure(11)),
                                    );
                                }
                                CommandService::Stop => {
                                    break 'worker (ListenAuthorityExit::Stopped, None);
                                }
                                CommandService::Advance(iterations) => {
                                    manual_iterations =
                                        manual_iterations.saturating_add(u32::from(iterations));
                                }
                            }
                        }
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            if draining {
                                break;
                            }
                            break 'worker (ListenAuthorityExit::CommandChannelDisconnected, None);
                        }
                    }
                }
                manual_iterations = manual_iterations.saturating_sub(1);
            }
        }

        if force_shutdown.load(Ordering::Acquire) {
            break (ListenAuthorityExit::Stopped, None);
        }
        network_tick = match network_tick.0.checked_add(1).map(SimTick) {
            Some(tick) => tick,
            None => {
                break (
                    ListenAuthorityExit::Failed,
                    Some(listen_internal_failure(4)),
                );
            }
        };
        let service_start = Instant::now();
        let monotonic_ms = epoch.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        if hub.pump_network_at(network_tick, monotonic_ms).is_err() {
            break (
                ListenAuthorityExit::Failed,
                Some(listen_internal_failure(5)),
            );
        }
        publish_hub_drain_events(&mut hub, &publisher);
        if publisher.lifecycle_overflowed() {
            break (
                ListenAuthorityExit::Failed,
                Some(listen_internal_failure(12)),
            );
        }
        if draining {
            metrics.waiting_iterations.fetch_add(1, Ordering::Relaxed);
            let service_ns = duration_ns(service_start.elapsed());
            metrics
                .total_service_duration_ns
                .fetch_add(service_ns, Ordering::Relaxed);
            metrics
                .maximum_service_duration_ns
                .fetch_max(service_ns, Ordering::Relaxed);
            if service_ns > NANOS_PER_TICK {
                metrics
                    .over_budget_iterations
                    .fetch_add(1, Ordering::Relaxed);
            }
            hub.observe_server_tick(service_ns);
            metrics.worker_iterations.fetch_add(1, Ordering::Relaxed);

            if hub.shutdown_drained() {
                if let Some(connection) = hub.connection_for_peer(host.peer_id)
                    && hub.detach(connection).is_err()
                {
                    break (
                        ListenAuthorityExit::Failed,
                        Some(listen_internal_failure(10)),
                    );
                }
                graceful_drained = true;
                publisher.publish_event(ListenAuthorityEvent::ShutdownDrained);
                if publisher.lifecycle_overflowed() {
                    break (
                        ListenAuthorityExit::Failed,
                        Some(listen_internal_failure(12)),
                    );
                }
                publisher.publish_status(make_status(
                    &hub,
                    roster,
                    &metrics,
                    config.command_capacity,
                    ListenAuthorityPhase::Drained,
                    hub.server_tick_distribution(),
                    None,
                ));
                break (ListenAuthorityExit::Stopped, None);
            }

            publisher.publish_status(make_status(
                &hub,
                roster,
                &metrics,
                config.command_capacity,
                ListenAuthorityPhase::Draining,
                hub.server_tick_distribution(),
                None,
            ));
            if matches!(clock, ListenWorkerClockMode::Realtime) {
                schedule.advance();
            }
            continue;
        }
        let seed = match_config.manifest.master_gameplay_seed;
        let outcome = hub.try_advance(|peer, seat, tick| {
            deterministic_disconnected_bot_frame(seed, peer, seat, tick)
        });
        match outcome {
            Ok((AuthorityAdvanceOutcome::Advanced, Some(report))) => {
                metrics.simulated_ticks.fetch_add(1, Ordering::Relaxed);
                let snapshot = hub
                    .authority()
                    .snapshot_at(report.tick)
                    .expect("listen authority retains the snapshot it just reported")
                    .clone();
                latest_snapshot = snapshot.clone();
                if accepted_input_tail.push_report(&report).is_err() {
                    metrics.diagnostics.observe_recorder_failure();
                }
                if let Some(mut recorder) = replay_recorder.take() {
                    let checkpoint = report
                        .tick
                        .get()
                        .is_multiple_of(REPLAY_CHECKPOINT_INTERVAL_TICKS);
                    let keyframe = report
                        .tick
                        .get()
                        .is_multiple_of(REPLAY_KEYFRAME_INTERVAL_TICKS);
                    match recorder.record_tick(&report, &snapshot, checkpoint, keyframe) {
                        Ok(()) => {
                            if let Some(result_id) = report.final_result_id {
                                match recorder.finish(&snapshot, result_id) {
                                    Ok(replay) => {
                                        deferred_replay = match diagnostics_writer.as_ref() {
                                            Some(writer) => writer.try_queue_replay(replay).err(),
                                            None => Some(replay),
                                        };
                                    }
                                    Err(_) => metrics.diagnostics.observe_recorder_failure(),
                                }
                            } else {
                                replay_recorder = Some(recorder);
                            }
                        }
                        Err(_) => metrics.diagnostics.observe_recorder_failure(),
                    }
                }
            }
            Ok((AuthorityAdvanceOutcome::WaitingForReady, None))
            | Ok((AuthorityAdvanceOutcome::WaitingForStartTick, None))
            | Ok((AuthorityAdvanceOutcome::Finished, None)) => {
                metrics.waiting_iterations.fetch_add(1, Ordering::Relaxed);
            }
            Ok(_) => {
                break (
                    ListenAuthorityExit::Failed,
                    Some(listen_internal_failure(6)),
                );
            }
            Err(_) => {
                break (
                    ListenAuthorityExit::Failed,
                    Some(listen_internal_failure(7)),
                );
            }
        }
        // Flush control, state, and result messages queued by the canonical step
        // before this endpoint service budget ends.
        if hub.pump_network_at(network_tick, monotonic_ms).is_err() {
            break (
                ListenAuthorityExit::Failed,
                Some(listen_internal_failure(8)),
            );
        }
        let service_duration = service_start.elapsed();
        let service_ns = duration_ns(service_duration);
        metrics
            .total_service_duration_ns
            .fetch_add(service_ns, Ordering::Relaxed);
        metrics
            .maximum_service_duration_ns
            .fetch_max(service_ns, Ordering::Relaxed);
        if service_ns > NANOS_PER_TICK {
            metrics
                .over_budget_iterations
                .fetch_add(1, Ordering::Relaxed);
        }
        hub.observe_server_tick(service_ns);
        if network_tick
            .get()
            .is_multiple_of(u64::from(AUTHORITY_THREAD_TICK_RATE_HZ))
            || hub.confirmed_result().is_some()
        {
            server_ticks = hub.server_tick_distribution();
        }
        if config.operational_export_interval_ticks != 0
            && network_tick
                .get()
                .is_multiple_of(u64::from(config.operational_export_interval_ticks))
            && let Some(writer) = diagnostics_writer.as_ref()
        {
            match make_operational_snapshot(
                &hub,
                roster,
                &metrics,
                config.command_capacity,
                server_ticks,
                false,
            ) {
                Ok(snapshot) => writer.try_queue_periodic(snapshot),
                Err(_) => metrics.diagnostics.observe_persistence_failure(),
            }
        }
        metrics.worker_iterations.fetch_add(1, Ordering::Relaxed);
        publisher.publish_status(make_status(
            &hub,
            roster,
            &metrics,
            config.command_capacity,
            classify_phase(&hub, roster),
            server_ticks,
            None,
        ));
        if matches!(clock, ListenWorkerClockMode::Realtime) {
            schedule.advance();
        }
    };

    metrics.command_queue_depth.store(0, Ordering::Relaxed);
    let terminal_phase = if failure.is_some() {
        ListenAuthorityPhase::Failed
    } else if graceful_drained {
        ListenAuthorityPhase::Drained
    } else {
        ListenAuthorityPhase::Stopped
    };
    server_ticks = hub.server_tick_distribution();
    let abnormal_failure = failure.or_else(|| {
        matches!(exit, ListenAuthorityExit::CommandChannelDisconnected)
            .then_some(listen_internal_failure(9))
    });
    if let Some(snapshot) = hub
        .authority()
        .snapshot_at(hub.authority().simulation().current_tick())
    {
        latest_snapshot = snapshot.clone();
    }
    let terminal_operational = match make_operational_snapshot(
        &hub,
        roster,
        &metrics,
        config.command_capacity,
        server_ticks,
        true,
    ) {
        Ok(snapshot) => Some(snapshot),
        Err(_) => {
            metrics.diagnostics.observe_persistence_failure();
            None
        }
    };
    let incident = abnormal_failure.and_then(|failure| {
        let Some(operational) = terminal_operational.clone() else {
            metrics.diagnostics.observe_persistence_failure();
            return None;
        };
        match AuthorityIncidentBundle::new(
            operational,
            failure,
            latest_snapshot,
            accepted_input_tail.to_vec(),
            hub.observability().audit(),
        ) {
            Ok(incident) => Some(incident),
            Err(_) => {
                metrics.diagnostics.observe_persistence_failure();
                None
            }
        }
    });
    if diagnostics_writer.is_none() && diagnostics_root.is_some() {
        // A failed writer start cannot justify synchronous, unbounded file I/O
        // on the authority/AppExit join path.
        metrics.diagnostics.observe_persistence_failure();
    }

    // Publish the authoritative terminal before any best-effort filesystem
    // finalization. Application shutdown may now observe and act on the
    // terminal without waiting on diagnostics I/O.
    let status = make_status(
        &hub,
        roster,
        &metrics,
        config.command_capacity,
        terminal_phase,
        server_ticks,
        failure,
    );
    let terminal = ListenAuthorityTerminal { exit, status };
    publisher.finish(terminal);

    if let Some(writer) = diagnostics_writer.take() {
        let _ = writer.finish_and_join_bounded(
            deferred_replay.take(),
            incident,
            terminal_operational,
            LISTEN_DIAGNOSTICS_FINALIZE_GRACE,
        );
    }
    terminal
}

fn finish_failed_bootstrap(
    publisher: &ListenMailboxPublisher,
    metrics: &SharedListenMetrics,
    command_capacity: usize,
    failure: OnlineFailure,
) -> ListenAuthorityTerminal {
    let terminal = ListenAuthorityTerminal {
        exit: ListenAuthorityExit::Failed,
        status: ListenAuthorityStatus {
            phase: ListenAuthorityPhase::Failed,
            worker: metrics.snapshot(command_capacity),
            diagnostics: metrics.diagnostics.snapshot(),
            failure: Some(failure),
            ..ListenAuthorityStatus::default()
        },
    };
    publisher.finish(terminal);
    terminal
}

fn service_command(
    command: ListenAuthorityCommand,
    hub: &mut LiveListenHub,
    publisher: &ListenMailboxPublisher,
    host_peer_id: PeerId,
) -> CommandService {
    match command {
        ListenAuthorityCommand::AttachInitial {
            peer_id,
            user_id,
            endpoint,
        } => match hub.attach_initial(peer_id, user_id, endpoint) {
            Ok(connection) => publisher.publish_event(ListenAuthorityEvent::InitialAttached {
                peer_id,
                connection,
            }),
            Err(error) => publisher.publish_event(ListenAuthorityEvent::CommandRejected {
                operation: ListenAuthorityOperation::AttachInitial,
                peer_id,
                reason: classify_command_rejection(&error),
            }),
        },
        ListenAuthorityCommand::AttachReconnect {
            user_id,
            claim,
            endpoint,
        } => match hub.attach_reconnect(user_id, claim, endpoint) {
            Ok(connection) => publisher.publish_event(ListenAuthorityEvent::ReconnectAttached {
                peer_id: claim.peer_id,
                connection,
            }),
            Err(error) => publisher.publish_event(ListenAuthorityEvent::CommandRejected {
                operation: ListenAuthorityOperation::AttachReconnect,
                peer_id: claim.peer_id,
                reason: classify_command_rejection(&error),
            }),
        },
        ListenAuthorityCommand::Detach { connection } => {
            let peer_id = hub.peer_for_connection(connection);
            match hub.detach(connection) {
                Ok(peer_id) => publisher.publish_event(ListenAuthorityEvent::Detached {
                    peer_id,
                    connection,
                }),
                // A delayed close for a retired physical generation is an
                // idempotent no-op. It must never resolve by peer id and detach
                // a reconnect replacement.
                Err(AuthorityPeerHubError::StaleConnection(_)) => {}
                Err(error) => {
                    if let Some(peer_id) = peer_id {
                        publisher.publish_event(ListenAuthorityEvent::CommandRejected {
                            operation: ListenAuthorityOperation::Detach,
                            peer_id,
                            reason: classify_command_rejection(&error),
                        });
                    } else {
                        publisher.publish_event(ListenAuthorityEvent::SecurityCommandRejected {
                            operation: ListenAuthorityOperation::Detach,
                            peer_id: None,
                            user_id: None,
                            reason: classify_command_rejection(&error),
                        });
                    }
                }
            }
        }
        ListenAuthorityCommand::RevokeAuthentication { connection } => {
            let identity = hub
                .peer_for_connection(connection)
                .and_then(|peer_id| hub.peer_identity(peer_id));
            match hub.revoke_authentication(connection) {
                Ok(()) => {
                    let identity = identity.expect("exact live generation had an identity");
                    publisher.publish_event(ListenAuthorityEvent::AuthenticationRevoked {
                        peer_id: identity.peer_id,
                        user_id: identity.user_id,
                        connection: identity.connection,
                    })
                }
                // A delayed authentication callback for a retired generation
                // is benign and must not revoke its replacement.
                Err(AuthorityPeerHubError::StaleConnection(_)) => {}
                Err(error) => {
                    publisher.publish_event(ListenAuthorityEvent::SecurityCommandRejected {
                        operation: ListenAuthorityOperation::RevokeAuthentication,
                        peer_id: identity.map(|identity| identity.peer_id),
                        user_id: identity.map(|identity| identity.user_id),
                        reason: classify_command_rejection(&error),
                    })
                }
            }
        }
        ListenAuthorityCommand::EnforcePlatformBan { user_id } => {
            let identity = hub
                .manifest()
                .ownership
                .as_slice()
                .iter()
                .filter_map(|assignment| match assignment.owner {
                    SeatOwner::Peer(peer_id) => hub.peer_identity(peer_id),
                    SeatOwner::AuthorityBot => None,
                })
                .find(|identity| identity.user_id == user_id);
            match hub.enforce_platform_ban(user_id) {
                Ok(()) => publisher.publish_event(ListenAuthorityEvent::PlatformBanEnforced {
                    user_id,
                    peer_id: identity.map(|identity| identity.peer_id),
                    connection: identity.map(|identity| identity.connection),
                }),
                Err(error) => {
                    publisher.publish_event(ListenAuthorityEvent::SecurityCommandRejected {
                        operation: ListenAuthorityOperation::EnforcePlatformBan,
                        peer_id: None,
                        user_id: Some(user_id),
                        reason: classify_command_rejection(&error),
                    })
                }
            }
        }
        ListenAuthorityCommand::BeginShutdown => {
            let starting = hub.shutdown_state() == AuthorityShutdownState::Running;
            match hub.begin_shutdown(host_peer_id) {
                Ok(()) => {
                    if starting {
                        publisher.publish_event(ListenAuthorityEvent::ShutdownStarted);
                    }
                    return CommandService::BeginShutdown;
                }
                Err(error) => {
                    publisher.publish_event(ListenAuthorityEvent::SecurityCommandRejected {
                        operation: ListenAuthorityOperation::BeginShutdown,
                        peer_id: Some(host_peer_id),
                        user_id: None,
                        reason: classify_command_rejection(&error),
                    });
                    return CommandService::Failed;
                }
            }
        }
        #[cfg(test)]
        ListenAuthorityCommand::AdvanceManual(iterations) => {
            return CommandService::Advance(iterations);
        }
        #[cfg(test)]
        ListenAuthorityCommand::FinishCanonicalMatch => {
            use crate::game_state::{MatchPhase, MatchState};
            hub.authority_mut()
                .simulation_mut()
                .world_mut()
                .resource_mut::<MatchState>()
                .phase = MatchPhase::Results;
        }
        #[cfg(test)]
        ListenAuthorityCommand::BlockCommandService(release) => {
            let deadline = Instant::now() + Duration::from_secs(1);
            while !release.load(Ordering::Acquire) && Instant::now() < deadline {
                thread::yield_now();
            }
        }
        ListenAuthorityCommand::Stop => return CommandService::Stop,
    }
    CommandService::Continue
}

fn dequeue_command(metrics: &SharedListenMetrics) {
    let _ =
        metrics
            .command_queue_depth
            .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |depth| {
                Some(depth.saturating_sub(1))
            });
    metrics.commands_processed.fetch_add(1, Ordering::Relaxed);
}

fn publish_hub_drain_events(hub: &mut LiveListenHub, publisher: &ListenMailboxPublisher) {
    while let Some(event) = hub.try_next_drain_event() {
        publisher.publish_event(ListenAuthorityEvent::TerminalDrained {
            peer_id: event.peer_id,
            user_id: event.user_id,
            connection: event.connection,
            disconnect: event.disconnect,
            completion: event.completion,
        });
    }
}

fn classify_command_rejection(
    error: &AuthorityPeerHubError<LiveSimulationError>,
) -> ListenAuthorityCommandRejection {
    match error {
        AuthorityPeerHubError::UnknownPeer(_) => ListenAuthorityCommandRejection::UnknownPeer,
        AuthorityPeerHubError::IdentityMismatch(_) => {
            ListenAuthorityCommandRejection::IdentityMismatch
        }
        AuthorityPeerHubError::DuplicatePeer(_) => {
            ListenAuthorityCommandRejection::AlreadyConnected
        }
        AuthorityPeerHubError::InitialAttachAfterCountdown => {
            ListenAuthorityCommandRejection::StartupClosed
        }
        AuthorityPeerHubError::ReconnectBeforeDisconnect => {
            ListenAuthorityCommandRejection::ReconnectNotAvailable
        }
        AuthorityPeerHubError::Reconnect(_) => ListenAuthorityCommandRejection::ReconnectDenied,
        AuthorityPeerHubError::Capacity => ListenAuthorityCommandRejection::Capacity,
        AuthorityPeerHubError::Protocol(_)
        | AuthorityPeerHubError::RuntimeConfig(_)
        | AuthorityPeerHubError::RuntimeQueue(_) => ListenAuthorityCommandRejection::Protocol,
        AuthorityPeerHubError::StaleConnection(_) => ListenAuthorityCommandRejection::NotConnected,
        _ => ListenAuthorityCommandRejection::Internal,
    }
}

fn classify_phase(hub: &LiveListenHub, roster: ListenAuthenticatedRoster) -> ListenAuthorityPhase {
    match hub.shutdown_state() {
        AuthorityShutdownState::Draining => return ListenAuthorityPhase::Draining,
        AuthorityShutdownState::Drained => return ListenAuthorityPhase::Drained,
        AuthorityShutdownState::Running => {}
    }
    if hub.confirmed_result().is_some() {
        return ListenAuthorityPhase::Results;
    }
    let connected = roster
        .iter()
        .filter(|peer| hub.connection_for_peer(peer.peer_id).is_some())
        .count();
    if connected < roster.len() {
        return ListenAuthorityPhase::WaitingForPeers;
    }
    let Some(start) = hub.countdown_start_tick() else {
        return ListenAuthorityPhase::Synchronizing;
    };
    if hub.network_tick() < start {
        ListenAuthorityPhase::Countdown
    } else {
        ListenAuthorityPhase::Fighting
    }
}

fn make_status(
    hub: &LiveListenHub,
    roster: ListenAuthenticatedRoster,
    metrics: &SharedListenMetrics,
    command_capacity: usize,
    phase: ListenAuthorityPhase,
    server_ticks: ServerTickDistribution,
    failure: Option<OnlineFailure>,
) -> ListenAuthorityStatus {
    let mut peers = [ListenAuthorityPeerStatus::default(); MAX_AUTHORITY_PEERS];
    for (index, authenticated) in roster.iter().enumerate() {
        peers[index] = ListenAuthorityPeerStatus {
            expected: true,
            peer_id: authenticated.peer_id,
            user_id: authenticated.user_id,
            connection: hub.connection_for_peer(authenticated.peer_id),
            phase: hub.peer_phase(authenticated.peer_id),
        };
    }
    ListenAuthorityStatus {
        phase,
        network_tick: hub.network_tick(),
        authority_tick: hub.authority().simulation().current_tick(),
        countdown_start_tick: hub.countdown_start_tick(),
        result: hub.confirmed_result(),
        peers,
        peer_count: roster.len() as u8,
        hub: hub.metrics(),
        observability: hub.observability().counters(),
        server_ticks,
        worker: metrics.snapshot(command_capacity),
        diagnostics: metrics.diagnostics.snapshot(),
        failure,
    }
}

fn make_operational_snapshot(
    hub: &LiveListenHub,
    roster: ListenAuthenticatedRoster,
    metrics: &SharedListenMetrics,
    command_capacity: usize,
    server_ticks: ServerTickDistribution,
    terminal: bool,
) -> Result<AuthorityOperationalSnapshot, crate::multiplayer_diagnostics::DiagnosticsError> {
    let mut security = Vec::with_capacity(roster.len());
    for authenticated in roster.iter() {
        if let Some(peer_metrics) = hub.peer_security_metrics(authenticated.peer_id) {
            security.push(SecurityDiagnosticSnapshot::new(
                authenticated.peer_id.get(),
                peer_metrics,
            ));
        }
    }
    AuthorityOperationalSnapshot::new(
        hub.manifest(),
        terminal,
        hub.network_tick(),
        hub.authority().simulation().current_tick(),
        hub.confirmed_result()
            .map(|result| (result.result_id.get(), result.final_state_hash.0)),
        metrics.snapshot(command_capacity).into(),
        hub.metrics(),
        hub.observability().counters(),
        server_ticks,
        hub.observability().audit().metrics(),
        metrics.diagnostics.snapshot(),
        security,
    )
}

/// Stateless canonical substitute used only after `ReconnectRegistry` advances
/// a disconnected seat past its neutral-input interval.  Every decision is a
/// pure function of match seed, authenticated peer, seat, and simulation tick,
/// so it is replayable and cannot depend on render cadence or process entropy.
pub fn deterministic_disconnected_bot_frame(
    match_seed: u64,
    peer_id: PeerId,
    seat: SeatId,
    tick: SimTick,
) -> InputFrame {
    let key = mix64(
        match_seed
            ^ peer_id.get().rotate_left(17)
            ^ u64::from(seat.get()).rotate_left(41)
            ^ tick.get().wrapping_mul(0x9e37_79b9_7f4a_7c15),
    );
    let direction = if key & 1 == 0 { -72 } else { 72 };
    let attack = tick.get().wrapping_add(key >> 32).is_multiple_of(43);
    let guard = !attack && tick.get().wrapping_add(key >> 16).is_multiple_of(97);
    let mut held = 0;
    let mut pressed = 0;
    if attack {
        held |= InputButtons::LIGHT;
        pressed |= InputButtons::LIGHT;
    }
    if guard {
        held |= InputButtons::GUARD;
        pressed |= InputButtons::GUARD;
    }
    InputFrame {
        tick,
        seat,
        movement_x: QuantizedAxis::new(direction).expect("fixed bot axis is valid"),
        movement_y: QuantizedAxis::default(),
        held_buttons: InputButtons::new(held).expect("fixed bot buttons are supported"),
        pressed_buttons: InputButtons::new(pressed).expect("fixed bot buttons are supported"),
        released_buttons: InputButtons::default(),
        sequence: InputSequence(tick.get() as u16),
    }
}

fn mix64(mut value: u64) -> u64 {
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn duration_ns(duration: Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}

fn listen_internal_failure(detail_code: u16) -> OnlineFailure {
    OnlineFailure {
        code: OnlineFailureCode::InternalFailure,
        severity: OnlineFailureSeverity::Fatal,
        recovery: OnlineRecoveryAction::ReturnToLobby,
        detail_code,
    }
}

#[cfg(test)]
#[path = "../tests/support/live_network_acceptance.rs"]
mod live_network_acceptance;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use crate::components::ParticipantKind;
    use crate::game_state::LocalSetup;
    use crate::match_config::{MatchBuildOptions, build_headless_match_config};
    use crate::multiplayer_security::SecurityViolation;
    use crate::network_protocol::{AuthorityKind, MatchId};
    use crate::remote_online_client::{
        RemoteCommandSubmitOutcome, RemoteLocalInputBatch, RemoteLocalInputSample,
        RemoteOnlineClientPhase, RemoteOnlineTerminal,
    };
    use crate::replay::{HeadlessReplayRunner, Replay};

    static NEXT_DIAGNOSTICS_TEST_ROOT: AtomicU64 = AtomicU64::new(1);

    fn diagnostics_test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "afc-listen-diagnostics-{label}-{}-{}",
            std::process::id(),
            NEXT_DIAGNOSTICS_TEST_ROOT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn moving_input(seat: u8, movement_x: i8) -> RemoteLocalInputBatch {
        RemoteLocalInputBatch::new(&[RemoteLocalInputSample {
            seat: SeatId::new(seat).unwrap(),
            movement_x: QuantizedAxis::new(movement_x).unwrap(),
            ..RemoteLocalInputSample::default()
        }])
        .unwrap()
    }

    fn peer(value: u64) -> PeerId {
        PeerId::new(value).unwrap()
    }

    fn user(value: u64) -> AuthenticatedUserId {
        AuthenticatedUserId::new(value).unwrap()
    }

    fn authenticated(peer_id: u64, user_id: u64) -> AuthenticatedPeer {
        AuthenticatedPeer {
            peer_id: peer(peer_id),
            user_id: user(user_id),
        }
    }

    fn match_config(two_peers: bool, id: [u8; 16]) -> HeadlessMatchConfig {
        let mut setup = LocalSetup::default();
        if two_peers {
            setup.slots[1].participant = ParticipantKind::Human;
        }
        let host = peer(11);
        let mut options = MatchBuildOptions::single_peer(
            MatchId::new(id).unwrap(),
            AuthorityKind::Listen,
            false,
            host,
            &setup,
            SimTick(2),
        );
        if two_peers {
            options.human_owners[1] = Some(peer(22));
        }
        build_headless_match_config(&setup, options).unwrap()
    }

    fn roster(config: &HeadlessMatchConfig, two_peers: bool) -> ListenAuthenticatedRoster {
        let host = authenticated(11, 1_011);
        let peers = if two_peers {
            vec![host, authenticated(22, 2_022)]
        } else {
            vec![host]
        };
        ListenAuthenticatedRoster::new(config, host, peers).unwrap()
    }

    fn manual_config() -> ListenAuthorityConfig {
        let mut config = ListenAuthorityConfig::default();
        config.hub.countdown_lead_ticks = 2;
        config.hub.runtime.inbound_capacity = 64;
        config.hub.runtime.outbound_capacity = 64;
        config.hub.reconnect.neutral_input_ticks = 2;
        config.hub.reconnect.grace_ticks = 64;
        config.host_endpoint_queue_packets = 512;
        config
    }

    #[test]
    fn operational_export_cadence_is_disabled_or_bounded_away_from_hot_path() {
        let mut config = ListenAuthorityConfig::default();
        config.operational_export_interval_ticks = 0;
        assert_eq!(config.validate(), Ok(()));
        config.operational_export_interval_ticks = MIN_OPERATIONAL_EXPORT_INTERVAL_TICKS - 1;
        assert_eq!(
            config.validate(),
            Err(ListenAuthorityConfigError::OperationalExportCadence)
        );
        config.operational_export_interval_ticks = MIN_OPERATIONAL_EXPORT_INTERVAL_TICKS;
        assert_eq!(config.validate(), Ok(()));
    }

    struct ManualHarness {
        config: HeadlessMatchConfig,
        listen: ListenOnlineMatch,
        remote: RemoteOnlineClient,
    }

    impl ManualHarness {
        fn new() -> Self {
            Self::new_with_diagnostics(None)
        }

        fn new_with_diagnostics(diagnostics_root: Option<PathBuf>) -> Self {
            Self::new_with_options(diagnostics_root, manual_config())
        }

        fn new_with_authority_config(authority_config: ListenAuthorityConfig) -> Self {
            Self::new_with_options(None, authority_config)
        }

        fn new_with_options(
            diagnostics_root: Option<PathBuf>,
            authority_config: ListenAuthorityConfig,
        ) -> Self {
            let config = match_config(true, *b"listen-worker-01");
            let listen = match diagnostics_root {
                Some(root) => ListenOnlineMatch::spawn_manual_with_diagnostics(
                    config.clone(),
                    roster(&config, true),
                    authority_config,
                    RemoteOnlineClientConfig::default(),
                    root,
                ),
                None => ListenOnlineMatch::spawn_manual(
                    config.clone(),
                    roster(&config, true),
                    authority_config,
                    RemoteOnlineClientConfig::default(),
                ),
            }
            .unwrap();
            listen.host_client.mark_content_loaded();
            let (client_endpoint, authority_endpoint) = InProcessEndpoint::pair(512).unwrap();
            assert!(
                listen
                    .authority
                    .try_attach_initial(peer(22), user(2_022), authority_endpoint)
                    .is_queued()
            );
            let remote = RemoteOnlineClient::spawn_manual(
                ListenDatagramEndpoint::InProcess(client_endpoint),
                config.clone(),
                peer(22),
                RemoteOnlineClientConfig::default(),
            )
            .unwrap();
            remote.mark_content_loaded();
            Self {
                config,
                listen,
                remote,
            }
        }

        fn round(&self) {
            if self.listen.host_client.terminal().is_none() {
                let advanced = self.listen.host_client.advance_manual(1);
                assert!(advanced || self.listen.host_client.terminal().is_some());
            }
            if self.remote.terminal().is_none() {
                let advanced = self.remote.advance_manual(1);
                assert!(advanced || self.remote.terminal().is_some());
            }
            assert!(self.listen.authority.advance_manual(1));
        }

        fn drive_until_fighting(&self) {
            for _ in 0..768 {
                self.round();
                if self.listen.authority.status().phase == ListenAuthorityPhase::Fighting
                    && self.listen.host_client.status().phase == RemoteOnlineClientPhase::Fighting
                    && self.remote.status().phase == RemoteOnlineClientPhase::Fighting
                {
                    return;
                }
            }
            panic!(
                "startup stalled: authority={:?}, host={:?}, remote={:?}",
                self.listen.authority.status(),
                self.listen.host_client.status(),
                self.remote.status()
            );
        }
    }

    #[test]
    fn two_protocol_peers_fight_and_confirm_identical_result() {
        let harness = ManualHarness::new();
        harness.drive_until_fighting();
        for _ in 0..8 {
            harness.round();
        }
        assert!(harness.listen.authority.finish_canonical_match());
        for _ in 0..256 {
            harness.round();
            if harness.listen.host_client.confirmed_result().is_some()
                && harness.remote.confirmed_result().is_some()
            {
                break;
            }
        }
        let authority = harness.listen.authority.result().expect("authority result");
        let host = harness
            .listen
            .host_client
            .confirmed_result()
            .expect("host result");
        let remote = harness.remote.confirmed_result().expect("remote result");
        assert_eq!(host, remote);
        assert_eq!(host.result_id, authority.result_id.get());
        assert_eq!(host.final_tick, authority.final_tick);
        assert_eq!(host.final_hash, authority.final_state_hash);
        assert_eq!(
            harness.listen.host_client.terminal(),
            Some(RemoteOnlineTerminal::Completed(host))
        );
    }

    #[test]
    fn production_listener_persists_complete_verifiable_replay_once() {
        let root = diagnostics_test_root("replay");
        let mut harness = ManualHarness::new_with_diagnostics(Some(root.clone()));
        harness.drive_until_fighting();
        assert_eq!(
            harness
                .listen
                .host_client
                .submit_inputs(moving_input(0, -112)),
            RemoteCommandSubmitOutcome::Queued
        );
        assert_eq!(
            harness.remote.submit_inputs(moving_input(1, 112)),
            RemoteCommandSubmitOutcome::Queued
        );
        for _ in 0..4_000 {
            harness.round();
            if harness.listen.authority.result().is_some() {
                break;
            }
        }
        let canonical_result = harness.listen.authority.result().expect("authority result");
        let terminal = harness.listen.authority.shutdown().unwrap();
        assert_eq!(terminal.status.result, Some(canonical_result));

        let replay_path = fs::read_dir(root.join("replays"))
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .find(|path| path.extension().and_then(|value| value.to_str()) == Some("afcr"))
            .expect("persisted replay");
        let replay = Replay::decode(&fs::read(replay_path).unwrap()).unwrap();
        assert_eq!(
            replay.final_result.confirmed_tick,
            canonical_result.final_tick
        );
        assert_eq!(
            replay.final_result.state_hash,
            canonical_result.final_state_hash
        );
        let mut verifier = build_headless_simulation(harness.config.clone()).unwrap();
        let verification = HeadlessReplayRunner::verify(&replay, &mut verifier).unwrap();
        assert_eq!(verification.final_tick, canonical_result.final_tick);
        assert_eq!(verification.final_hash, canonical_result.final_state_hash);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn abnormal_listener_exit_persists_partial_privacy_safe_incident() {
        let root = diagnostics_test_root("incident");
        let config = match_config(false, *b"listen-incident1");
        let mut listen = ListenOnlineMatch::spawn_manual_with_diagnostics(
            config.clone(),
            roster(&config, false),
            manual_config(),
            RemoteOnlineClientConfig::default(),
            root.clone(),
        )
        .unwrap();

        let terminal = listen.authority.join().unwrap();
        assert_eq!(
            terminal.exit,
            ListenAuthorityExit::CommandChannelDisconnected
        );
        let incident_path = fs::read_dir(root.join("incidents"))
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .find(|path| path.extension().and_then(|value| value.to_str()) == Some("afci"))
            .expect("persisted incident");
        let archive = crate::multiplayer_diagnostics::AuthorityDiagnosticsArchive::new(&root);
        let incident = archive.load_incident(&incident_path).unwrap();
        assert_eq!(
            incident.operational.match_id,
            *config.manifest.match_id.as_bytes()
        );
        assert_eq!(incident.operational.authority_tick, 0);
        assert!(incident.accepted_input_tail.is_empty());
        assert_eq!(incident.failure.detail_code, 9);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn diagnostics_io_failure_never_replaces_canonical_result() {
        let invalid_root = diagnostics_test_root("unwritable-root");
        fs::write(&invalid_root, b"this path is deliberately a file").unwrap();
        let mut harness = ManualHarness::new_with_diagnostics(Some(invalid_root.clone()));
        harness.drive_until_fighting();
        assert!(harness.listen.authority.finish_canonical_match());
        for _ in 0..256 {
            harness.round();
            if harness.listen.authority.result().is_some() {
                break;
            }
        }
        let canonical_result = harness.listen.authority.result().expect("authority result");
        let shutdown_started = Instant::now();
        let terminal = harness.listen.authority.shutdown().unwrap();
        assert_eq!(terminal.status.result, Some(canonical_result));
        assert!(
            shutdown_started.elapsed() < Duration::from_millis(500),
            "diagnostics I/O failure extended the authority shutdown"
        );
        // Terminal publication intentionally precedes best-effort diagnostics
        // finalization, so post-publication writer counters are not folded
        // back into this immutable canonical terminal snapshot.
        assert_eq!(terminal.status.diagnostics.replays_persisted, 0);
        assert!(invalid_root.is_file());

        fs::remove_file(invalid_root).unwrap();
    }

    #[test]
    fn conservative_zero_tick_reconnect_recovers_slot_tick_boundary_and_result() {
        let mut harness = ManualHarness::new();
        harness.drive_until_fighting();
        for _ in 0..6 {
            harness.round();
        }
        let before = harness.remote.status();
        let remote_connection = harness
            .listen
            .authority
            .status()
            .peer(peer(22))
            .and_then(|peer| peer.connection)
            .expect("remote generation before detach");
        assert!(
            harness
                .listen
                .authority
                .try_detach(remote_connection)
                .is_queued()
        );
        assert!(harness.listen.authority.advance_manual(1));
        let disconnected_at = harness.listen.authority.status().authority_tick;
        for _ in 0..5 {
            assert!(harness.listen.host_client.advance_manual(1));
            assert!(harness.listen.authority.advance_manual(1));
        }
        assert_eq!(
            harness.listen.authority.status().authority_tick.get(),
            disconnected_at.get() + 5
        );

        let (client_endpoint, authority_endpoint) = InProcessEndpoint::pair(512).unwrap();
        assert!(
            harness
                .listen
                .authority
                .try_attach_reconnect(
                    user(2_022),
                    ReconnectClaim {
                        match_id: harness.config.manifest.match_id,
                        peer_id: peer(22),
                        // The application treats this as an optimization hint;
                        // authority identity/grace checks and its current
                        // retained snapshot remain authoritative.
                        last_confirmed_tick: SimTick::ZERO,
                    },
                    authority_endpoint,
                )
                .is_queued()
        );
        harness
            .remote
            .reconnect_manual(ListenDatagramEndpoint::InProcess(client_endpoint))
            .unwrap();
        for _ in 0..768 {
            harness.round();
            if harness.remote.status().phase == RemoteOnlineClientPhase::Fighting
                && harness
                    .listen
                    .authority
                    .status()
                    .peer(peer(22))
                    .is_some_and(|peer| peer.phase == Some(AuthorityPeerPhase::Fighting))
            {
                break;
            }
        }
        assert_eq!(
            harness.remote.status().phase,
            RemoteOnlineClientPhase::Fighting
        );
        assert_eq!(
            harness.remote.status().countdown_start_tick,
            before.countdown_start_tick
        );
        assert_eq!(
            harness.listen.authority.status().hub.reconnects_completed,
            1
        );
        assert!(
            harness
                .config
                .manifest
                .ownership
                .as_slice()
                .iter()
                .any(|assignment| assignment.owner == SeatOwner::Peer(peer(22)))
        );
        assert!(
            harness
                .remote
                .status()
                .confirmed_tick
                .is_some_and(|tick| tick >= disconnected_at)
        );

        assert!(harness.listen.authority.finish_canonical_match());
        for _ in 0..256 {
            harness.round();
            if harness.remote.confirmed_result().is_some()
                && harness.listen.host_client.confirmed_result().is_some()
            {
                break;
            }
        }
        let authority = harness.listen.authority.result().expect("authority result");
        let host = harness
            .listen
            .host_client
            .confirmed_result()
            .unwrap_or_else(|| {
                panic!(
                    "host result: authority={:?}, host={:?}, host_terminal={:?}, remote={:?}, remote_terminal={:?}",
                    harness.listen.authority.status(),
                    harness.listen.host_client.status(),
                    harness.listen.host_client.terminal(),
                    harness.remote.status(),
                    harness.remote.terminal(),
                )
            });
        let remote = harness.remote.confirmed_result().expect("remote result");
        assert_eq!(remote, host);
        assert_eq!(remote.result_id, authority.result_id.get());
        assert_eq!(remote.final_tick, authority.final_tick);
        assert_eq!(remote.final_hash, authority.final_state_hash);
    }

    #[test]
    fn backpressured_stale_exact_detach_cannot_kill_reconnect_replacement() {
        let mut harness = ManualHarness::new();
        harness.drive_until_fighting();
        let old_connection = harness
            .listen
            .authority
            .status()
            .peer(peer(22))
            .and_then(|peer| peer.connection)
            .expect("old remote generation");

        let held_stale = loop {
            match harness.listen.authority.try_detach(old_connection) {
                ListenAuthoritySubmitOutcome::Queued => {}
                ListenAuthoritySubmitOutcome::Full(command) => break command,
                ListenAuthoritySubmitOutcome::Disconnected(_) => {
                    panic!("authority disconnected while inducing backpressure")
                }
            }
        };

        let deadline = Instant::now() + Duration::from_secs(1);
        while harness
            .listen
            .authority
            .status()
            .peer(peer(22))
            .and_then(|peer| peer.connection)
            .is_some()
            || harness.listen.authority.metrics().command_queue_depth != 0
        {
            let _ = harness.listen.authority.advance_manual(1);
            assert!(Instant::now() < deadline, "old generation did not detach");
            thread::yield_now();
        }

        let (replacement_client, replacement_authority) = InProcessEndpoint::pair(512).unwrap();
        assert!(
            harness
                .listen
                .authority
                .try_attach_reconnect(
                    user(2_022),
                    ReconnectClaim {
                        match_id: harness.config.manifest.match_id,
                        peer_id: peer(22),
                        last_confirmed_tick: SimTick::ZERO,
                    },
                    replacement_authority,
                )
                .is_queued()
        );
        while harness
            .listen
            .authority
            .status()
            .peer(peer(22))
            .and_then(|peer| peer.connection)
            .is_none()
        {
            let _ = harness.listen.authority.advance_manual(1);
            assert!(
                Instant::now() < deadline,
                "replacement generation did not attach"
            );
            thread::yield_now();
        }
        let replacement_connection = harness
            .listen
            .authority
            .status()
            .peer(peer(22))
            .and_then(|peer| peer.connection)
            .unwrap();
        assert_ne!(replacement_connection, old_connection);

        let mut command = held_stale;
        loop {
            match harness.listen.authority.try_submit(command) {
                ListenAuthoritySubmitOutcome::Queued => break,
                ListenAuthoritySubmitOutcome::Full(returned) => {
                    command = returned;
                    thread::yield_now();
                }
                ListenAuthoritySubmitOutcome::Disconnected(_) => {
                    panic!("authority disconnected before stale retry")
                }
            }
        }
        while harness.listen.authority.metrics().command_queue_depth != 0 {
            let _ = harness.listen.authority.advance_manual(1);
            assert!(
                Instant::now() < deadline,
                "stale exact detach did not drain"
            );
            thread::yield_now();
        }
        assert_eq!(
            harness
                .listen
                .authority
                .status()
                .peer(peer(22))
                .and_then(|peer| peer.connection),
            Some(replacement_connection)
        );

        drop(replacement_client);
        harness.listen.host_client.stop();
        harness.remote.stop();
        let _ = harness.listen.authority.shutdown();
    }

    #[test]
    fn backpressured_stale_exact_revocation_cannot_ban_reconnect_replacement() {
        let mut authority_config = manual_config();
        authority_config.command_capacity = 1;
        authority_config.max_commands_per_service = 1;
        let mut harness = ManualHarness::new_with_authority_config(authority_config);
        harness.drive_until_fighting();
        let old_connection = harness
            .listen
            .authority
            .status()
            .peer(peer(22))
            .and_then(|peer| peer.connection)
            .expect("old remote generation");

        let release = Arc::new(AtomicBool::new(false));
        assert!(
            harness
                .listen
                .authority
                .try_submit(ListenAuthorityCommand::BlockCommandService(Arc::clone(
                    &release
                )))
                .is_queued()
        );
        let deadline = Instant::now() + Duration::from_secs(1);
        while harness.listen.authority.metrics().command_queue_depth != 0 {
            assert!(
                Instant::now() < deadline,
                "test service blocker was not dequeued"
            );
            thread::yield_now();
        }
        assert!(
            harness
                .listen
                .authority
                .try_detach(old_connection)
                .is_queued()
        );
        let held_stale = match harness
            .listen
            .authority
            .try_revoke_authentication(old_connection)
        {
            ListenAuthoritySubmitOutcome::Full(command) => command,
            _ => panic!("capacity-one queue did not return the exact revocation"),
        };
        release.store(true, Ordering::Release);

        while harness
            .listen
            .authority
            .status()
            .peer(peer(22))
            .and_then(|peer| peer.connection)
            .is_some()
        {
            let _ = harness.listen.authority.advance_manual(1);
            assert!(Instant::now() < deadline, "old generation did not detach");
            thread::yield_now();
        }

        let (replacement_client, replacement_authority) = InProcessEndpoint::pair(512).unwrap();
        loop {
            match harness.listen.authority.try_attach_reconnect(
                user(2_022),
                ReconnectClaim {
                    match_id: harness.config.manifest.match_id,
                    peer_id: peer(22),
                    last_confirmed_tick: SimTick::ZERO,
                },
                replacement_authority,
            ) {
                ListenAuthoritySubmitOutcome::Queued => break,
                ListenAuthoritySubmitOutcome::Full(_) => {
                    panic!("replacement queue remained full after old detach")
                }
                ListenAuthoritySubmitOutcome::Disconnected(_) => {
                    panic!("authority disconnected before replacement")
                }
            }
        }
        while harness
            .listen
            .authority
            .status()
            .peer(peer(22))
            .and_then(|peer| peer.connection)
            .is_none()
        {
            let _ = harness.listen.authority.advance_manual(1);
            assert!(
                Instant::now() < deadline,
                "replacement generation did not attach"
            );
            thread::yield_now();
        }
        let replacement_connection = harness
            .listen
            .authority
            .status()
            .peer(peer(22))
            .and_then(|peer| peer.connection)
            .unwrap();
        assert_ne!(replacement_connection, old_connection);

        let mut command = held_stale;
        loop {
            match harness.listen.authority.try_submit(command) {
                ListenAuthoritySubmitOutcome::Queued => break,
                ListenAuthoritySubmitOutcome::Full(returned) => {
                    command = returned;
                    thread::yield_now();
                }
                ListenAuthoritySubmitOutcome::Disconnected(_) => {
                    panic!("authority disconnected before stale revoke retry")
                }
            }
        }
        while harness.listen.authority.metrics().command_queue_depth != 0 {
            let _ = harness.listen.authority.advance_manual(1);
            assert!(
                Instant::now() < deadline,
                "stale exact revocation did not drain"
            );
            thread::yield_now();
        }
        assert_eq!(
            harness
                .listen
                .authority
                .status()
                .peer(peer(22))
                .and_then(|peer| peer.connection),
            Some(replacement_connection)
        );
        assert_eq!(harness.listen.authority.status().hub.temporary_bans, 0);

        drop(replacement_client);
        harness.listen.host_client.stop();
        harness.remote.stop();
        let _ = harness.listen.authority.shutdown();
    }

    #[test]
    fn render_stall_does_not_pause_realtime_authority_and_stop_is_clean() {
        let diagnostics_root = diagnostics_test_root("realtime");
        let config = match_config(false, *b"listen-worker-rt");
        let mut authority_config = ListenAuthorityConfig::default();
        authority_config.hub.countdown_lead_ticks = 2;
        let mut listen = ListenOnlineMatch::spawn_with_diagnostics_root(
            config.clone(),
            roster(&config, false),
            authority_config,
            RemoteOnlineClientConfig::default(),
            diagnostics_root.clone(),
        )
        .unwrap();
        listen.host_client.mark_content_loaded();
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline
            && (listen.authority.status().phase != ListenAuthorityPhase::Fighting
                || listen.authority.status().authority_tick < SimTick(3))
        {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            listen.authority.status().phase,
            ListenAuthorityPhase::Fighting
        );
        let before = listen.authority.status().authority_tick;
        thread::sleep(Duration::from_millis(300));
        let advanced = listen
            .authority
            .status()
            .authority_tick
            .get()
            .saturating_sub(before.get());
        assert!(
            (12..=24).contains(&advanced),
            "advanced {advanced} ticks during a 300 ms render stall"
        );
        listen.host_client.stop();
        let terminal = listen.authority.shutdown().unwrap();
        assert_eq!(terminal.exit, ListenAuthorityExit::Stopped);
        assert_eq!(terminal.status.phase, ListenAuthorityPhase::Stopped);
        fs::remove_dir_all(diagnostics_root).unwrap();
    }

    #[test]
    fn commands_and_events_are_bounded() {
        let config = match_config(false, *b"listen-worker-bd");
        let mut worker_config = manual_config();
        worker_config.command_capacity = 1;
        worker_config.max_commands_per_service = 1;
        worker_config.event_capacity = 1;
        let mut listen = ListenOnlineMatch::spawn_manual(
            config.clone(),
            roster(&config, false),
            worker_config,
            RemoteOnlineClientConfig::default(),
        )
        .unwrap();
        let host_connection = listen
            .authority
            .status()
            .peer(peer(11))
            .and_then(|peer| peer.connection)
            .expect("host generation after bootstrap");
        let mut full = false;
        for _ in 0..10_000 {
            match listen.authority.try_detach(host_connection) {
                ListenAuthoritySubmitOutcome::Queued => {}
                ListenAuthoritySubmitOutcome::Full(_) => {
                    full = true;
                    break;
                }
                ListenAuthoritySubmitOutcome::Disconnected(_) => panic!("disconnected"),
            }
        }
        assert!(full);
        assert!(listen.authority.metrics().command_queue_high_water <= 1);
        assert!(listen.authority.metrics().commands_full > 0);
        listen.host_client.stop();
        let terminal = listen.authority.shutdown().unwrap();
        assert_eq!(terminal.exit, ListenAuthorityExit::Stopped);
        assert!(listen.authority.metrics().event_queue_high_water <= 1);
        assert!(matches!(
            listen.authority.try_detach(host_connection),
            ListenAuthoritySubmitOutcome::Disconnected(_)
        ));
    }

    #[test]
    fn lifecycle_mailbox_capacity_one_never_evicts_a_retained_fact() {
        let metrics = Arc::new(SharedListenMetrics::default());
        let state = Arc::new(Mutex::new(ListenMailboxState {
            status: ListenAuthorityStatus::default(),
            lifecycle_events: VecDeque::with_capacity(1),
            telemetry_events: VecDeque::with_capacity(1),
            lifecycle_overflowed: false,
            terminal: None,
        }));
        let (signal_tx, signal_rx) = mpsc::sync_channel(1);
        let publisher = ListenMailboxPublisher {
            state: Arc::clone(&state),
            signal: signal_tx,
            event_capacity: 1,
            metrics: Arc::clone(&metrics),
        };
        let mut inbox = ListenMailboxInbox {
            state,
            signal: signal_rx,
        };

        publisher.publish_event(ListenAuthorityEvent::ShutdownStarted);
        publisher.publish_event(ListenAuthorityEvent::CommandRejected {
            operation: ListenAuthorityOperation::AttachInitial,
            peer_id: peer(99),
            reason: ListenAuthorityCommandRejection::Capacity,
        });
        publisher.publish_event(ListenAuthorityEvent::ShutdownDrained);

        assert!(publisher.lifecycle_overflowed());
        let update = inbox.drain();
        assert_eq!(update.events.len(), 2);
        assert!(matches!(
            update.events[0],
            ListenAuthorityEvent::ShutdownStarted
        ));
        assert!(matches!(
            update.events[1],
            ListenAuthorityEvent::CommandRejected { .. }
        ));
        assert_eq!(metrics.snapshot(1).event_queue_high_water, 1);
        assert_eq!(metrics.snapshot(1).events_dropped, 1);
    }

    #[test]
    fn lifecycle_mailbox_saturation_fails_worker_closed_at_capacity_one() {
        let config = match_config(true, *b"mailbox-overflow");
        let mut worker_config = manual_config();
        worker_config.event_capacity = 1;
        let mut listen = ListenOnlineMatch::spawn_manual(
            config,
            roster(&match_config(true, *b"mailbox-overflow"), true),
            worker_config,
            RemoteOnlineClientConfig::default(),
        )
        .unwrap();
        let (remote_endpoint, authority_endpoint) = InProcessEndpoint::pair(64).unwrap();
        assert!(
            listen
                .authority
                .try_attach_initial(peer(22), user(2_022), authority_endpoint)
                .is_queued()
        );
        assert!(listen.authority.advance_manual(1));
        let remote_connection = listen
            .authority
            .status()
            .peer(peer(22))
            .and_then(|peer| peer.connection)
            .expect("attached remote generation");
        assert!(
            listen
                .authority
                .try_revoke_authentication(remote_connection)
                .is_queued()
        );
        let _ = listen.authority.advance_manual(1);

        let deadline = Instant::now() + Duration::from_secs(1);
        while listen.authority.terminal().is_none() {
            assert!(
                Instant::now() < deadline,
                "lifecycle mailbox overflow did not fail the worker"
            );
            thread::yield_now();
        }
        let update = listen.authority.drain_update();
        assert!(matches!(
            update.terminal,
            Some(ListenAuthorityTerminal {
                exit: ListenAuthorityExit::Failed,
                status: ListenAuthorityStatus {
                    phase: ListenAuthorityPhase::Failed,
                    ..
                },
            })
        ));
        assert!(update.events.iter().any(|event| matches!(
            event,
            ListenAuthorityEvent::InitialAttached {
                peer_id,
                ..
            } if *peer_id == peer(22)
        )));
        assert!(
            !update
                .events
                .iter()
                .any(|event| matches!(event, ListenAuthorityEvent::AuthenticationRevoked { .. }))
        );
        drop(remote_endpoint);
        listen.host_client.stop();
        assert_eq!(
            listen.authority.join().unwrap().exit,
            ListenAuthorityExit::Failed
        );
    }

    #[test]
    fn graceful_shutdown_freezes_canonical_time_and_drains_remote_terminal() {
        let mut harness = ManualHarness::new();
        harness.drive_until_fighting();
        let authority_tick = harness.listen.authority.status().authority_tick;
        let remote_identity = harness
            .listen
            .authority
            .status()
            .peer(peer(22))
            .expect("remote status before shutdown");
        assert!(
            harness
                .listen
                .authority
                .try_begin_graceful_shutdown()
                .is_queued()
        );

        let mut events = Vec::new();
        for _ in 0..256 {
            if harness.listen.host_client.terminal().is_none() {
                let _ = harness.listen.host_client.advance_manual(1);
            }
            if harness.remote.terminal().is_none() {
                let _ = harness.remote.advance_manual(1);
            }
            if harness.listen.authority.terminal().is_none() {
                let _ = harness.listen.authority.advance_manual(1);
            }
            let update = harness.listen.authority.drain_update();
            events.extend(update.events);
            if update.terminal.is_some() {
                break;
            }
        }

        let terminal = harness
            .listen
            .authority
            .terminal()
            .expect("graceful authority terminal");
        assert_eq!(terminal.exit, ListenAuthorityExit::Stopped);
        assert_eq!(terminal.status.phase, ListenAuthorityPhase::Drained);
        assert_eq!(terminal.status.authority_tick, authority_tick);
        let Some(RemoteOnlineTerminal::AuthorityDisconnected(disconnect)) =
            harness.remote.terminal()
        else {
            panic!("remote did not receive the typed shutdown terminal");
        };
        assert_eq!(
            disconnect.message.code,
            crate::network_protocol::DisconnectCode::ServerShutdown
        );
        assert_eq!(
            disconnect.message.retry,
            crate::network_protocol::RetryDisposition::MatchEndedNoContest
        );
        assert!(events.contains(&ListenAuthorityEvent::ShutdownStarted));
        assert!(events.contains(&ListenAuthorityEvent::ShutdownDrained));
        assert!(events.iter().any(|event| {
            matches!(
                event,
                ListenAuthorityEvent::TerminalDrained {
                    peer_id: drained_peer,
                    user_id,
                    connection,
                    disconnect: Some(message),
                    ..
                } if *drained_peer == peer(22)
                    && *user_id == remote_identity.user_id
                    && Some(*connection) == remote_identity.connection
                    && message.code == crate::network_protocol::DisconnectCode::ServerShutdown
            )
        }));
    }

    #[test]
    fn graceful_shutdown_without_remote_ack_reaches_deadline_and_drained_terminal() {
        let config = match_config(true, *b"listen-no-ack-01");
        let mut worker_config = manual_config();
        worker_config.hub.typed_disconnect_timeout_ticks = 3;
        let mut listen = ListenOnlineMatch::spawn_manual(
            config.clone(),
            roster(&config, true),
            worker_config,
            RemoteOnlineClientConfig::default(),
        )
        .unwrap();
        listen.host_client.mark_content_loaded();
        let (_unserviced_client, authority_endpoint) = InProcessEndpoint::pair(64).unwrap();
        assert!(
            listen
                .authority
                .try_attach_initial(peer(22), user(2_022), authority_endpoint)
                .is_queued()
        );
        assert!(listen.authority.advance_manual(1));
        let authority_tick = listen.authority.status().authority_tick;
        assert!(listen.authority.try_begin_graceful_shutdown().is_queued());

        let mut events = Vec::new();
        for _ in 0..16 {
            if listen.authority.terminal().is_none() {
                let _ = listen.authority.advance_manual(1);
            }
            let update = listen.authority.drain_update();
            events.extend(update.events);
            if update.terminal.is_some() {
                break;
            }
        }

        let terminal = listen.authority.terminal().expect("bounded drain terminal");
        assert_eq!(terminal.status.phase, ListenAuthorityPhase::Drained);
        assert_eq!(terminal.status.authority_tick, authority_tick);
        assert!(events.iter().any(|event| {
            matches!(
                event,
                ListenAuthorityEvent::TerminalDrained {
                    peer_id: drained_peer,
                    completion: AuthorityClosingCompletion::TimedOut,
                    ..
                } if *drained_peer == peer(22)
            )
        }));
        listen.host_client.stop();
    }

    #[test]
    fn security_commands_publish_logical_isolation_then_exact_generation_drain() {
        let mut harness = ManualHarness::new();
        harness.drive_until_fighting();
        let identity = harness
            .listen
            .authority
            .status()
            .peer(peer(22))
            .expect("connected remote identity");
        let connection = identity.connection.expect("connected remote generation");
        assert!(
            harness
                .listen
                .authority
                .try_revoke_authentication(connection)
                .is_queued()
        );

        let mut events = Vec::new();
        for _ in 0..32 {
            let _ = harness.listen.authority.advance_manual(1);
            if harness.remote.terminal().is_none() {
                let _ = harness.remote.advance_manual(1);
            }
            let update = harness.listen.authority.drain_update();
            events.extend(update.events);
            if events.iter().any(|event| {
                matches!(event, ListenAuthorityEvent::TerminalDrained { peer_id, .. }
                    if *peer_id == peer(22))
            }) {
                break;
            }
        }

        let revoked_index = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    ListenAuthorityEvent::AuthenticationRevoked {
                        peer_id: revoked_peer,
                        user_id,
                        connection,
                    } if *revoked_peer == peer(22)
                        && *user_id == identity.user_id
                        && Some(*connection) == identity.connection
                )
            })
            .expect("immediate logical-isolation event");
        let drained_index = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    ListenAuthorityEvent::TerminalDrained {
                        peer_id: drained_peer,
                        user_id,
                        connection,
                        disconnect: Some(message),
                        ..
                    } if *drained_peer == peer(22)
                        && *user_id == identity.user_id
                        && Some(*connection) == identity.connection
                        && message.detail_code
                            == SecurityViolation::AuthenticationRevoked.detail_code()
                )
            })
            .expect("exact physical drain event");
        assert!(revoked_index < drained_index);
    }

    #[test]
    fn emergency_stop_remains_a_bounded_fallback() {
        let config = match_config(false, *b"listen-emergency");
        let mut listen = ListenOnlineMatch::spawn_manual(
            config.clone(),
            roster(&config, false),
            manual_config(),
            RemoteOnlineClientConfig::default(),
        )
        .unwrap();
        listen.host_client.stop();
        let started = Instant::now();
        listen.authority.request_stop();
        let terminal = listen.authority.join().unwrap();
        assert_eq!(terminal.exit, ListenAuthorityExit::Stopped);
        assert_eq!(terminal.status.phase, ListenAuthorityPhase::Stopped);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn disconnected_bot_frames_are_stable_valid_and_identity_scoped() {
        let mut differed = false;
        for tick in 500..532 {
            let first = deterministic_disconnected_bot_frame(
                7,
                peer(11),
                SeatId::new(0).unwrap(),
                SimTick(tick),
            );
            let repeat = deterministic_disconnected_bot_frame(
                7,
                peer(11),
                SeatId::new(0).unwrap(),
                SimTick(tick),
            );
            let other = deterministic_disconnected_bot_frame(
                7,
                peer(22),
                SeatId::new(0).unwrap(),
                SimTick(tick),
            );
            assert_eq!(first, repeat);
            first.validate().unwrap();
            differed |= first != other;
        }
        assert!(differed);
    }
}
