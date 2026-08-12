//! Auth-gated Steam Networking Sockets P2P transport.
//!
//! [`SteamTransport`] owns Steam P2P listen/connect handles and bounded AFC
//! datagram queues. It deliberately does not pump Steam callbacks: the one
//! [`crate::steam_platform::SteamPlatform`] owner must be pumped first. Incoming
//! connections stay unaccepted until the caller supplies the exact
//! [`AuthenticatedSteamPeer`] produced by that platform service.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};

#[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
use crate::network_io::MAX_AFC_DATAGRAM_BYTES;
use crate::network_io::{AfcDatagram, NonBlockingDatagramEndpoint, ReceiveOutcome, SendOutcome};
use crate::steam_control::{
    EncodedSteamControlFrame, STEAM_CONTROL_MAGIC, STEAM_LOBBY_SCHEMA_VERSION,
    SteamControlCodecError, SteamControlEnvelope, SteamControlIdentity, SteamControlMessage,
    decode_steam_control, encode_steam_control,
};
use crate::steam_platform::{
    AdmissionPurpose, AuthenticatedSteamPeer, DedicatedSdrSupport, SteamLobbyId, SteamUserId,
};

pub const MAX_STEAM_TRANSPORT_CONNECTIONS: usize = 4;
pub const MAX_STEAM_TRANSPORT_EVENTS: usize = 64;
pub const MAX_STEAM_ENDPOINT_QUEUE_PACKETS: usize = 256;
pub const MAX_STEAM_TRANSPORT_DATAGRAMS_PER_PUMP: usize = 64;
pub const MAX_STEAM_TRANSPORT_CALLBACKS_PER_PUMP: usize = 64;
pub const MAX_STEAM_VIRTUAL_PORT: i32 = 999;
pub const DEFAULT_STEAM_ENDPOINT_QUEUE_PACKETS: usize = 64;
pub const DEFAULT_PENDING_ADMISSION_TIMEOUT_MS: u64 = 2_000;
pub const DEFAULT_CONNECT_TIMEOUT_MS: u64 = 15_000;
pub const DEFAULT_CONTROL_RETRANSMIT_MS: u64 = 250;
pub const MAX_CONTROL_FRAMES_PER_CONNECTION_PER_PUMP: usize = 4;
pub const MAX_CONTROL_FRAMES_PER_PUMP: usize = 16;
pub const CONTROL_RETRY_DELAY_MS: u64 = 500;
pub const CONTROL_RETRY_MINIMUM_REMAINING_MS: u64 = 5_000;
pub const ENDPOINT_DROP_DRAIN_QUIET_MS: u64 = 50;
pub const ENDPOINT_DROP_DRAIN_HARD_TIMEOUT_MS: u64 = 250;
/// Absolute coordinator-owned cap for retiring a complete transport.
///
/// A retirement pump always services its bounded outbound budget before this
/// deadline is evaluated. The extra quiet-window margin lets every per-endpoint
/// 250 ms drain reach its own exact terminal outcome first during normal frame
/// cadence.
pub const STEAM_TRANSPORT_RETIREMENT_HARD_TIMEOUT_MS: u64 =
    ENDPOINT_DROP_DRAIN_HARD_TIMEOUT_MS + ENDPOINT_DROP_DRAIN_QUIET_MS;

const MAX_TIMEOUT_MS: u64 = 60_000;
const FAKE_BACKEND_EVENT_CAPACITY: usize = 64;
const MAX_FAKE_BACKENDS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SteamConnectionId(u32);

impl SteamConnectionId {
    pub(crate) fn new(value: u32) -> Result<Self, SteamTransportError> {
        if value == 0 {
            Err(SteamTransportError::BackendIntegrityFailure)
        } else {
            Ok(Self(value))
        }
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SteamTransportRole {
    ListenAuthority,
    Client,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SteamP2pSession {
    pub lobby: SteamLobbyId,
    pub authority_user: SteamUserId,
    pub role: SteamTransportRole,
    pub virtual_port: i32,
}

impl SteamP2pSession {
    fn validate(self, local_user: SteamUserId) -> Result<(), SteamTransportError> {
        if !(0..=MAX_STEAM_VIRTUAL_PORT).contains(&self.virtual_port) {
            return Err(SteamTransportError::InvalidVirtualPort);
        }
        match self.role {
            SteamTransportRole::ListenAuthority if local_user != self.authority_user => {
                Err(SteamTransportError::AuthorityIdentityMismatch)
            }
            SteamTransportRole::Client if local_user == self.authority_user => {
                Err(SteamTransportError::AuthorityIdentityMismatch)
            }
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SteamTransportConfig {
    pub endpoint_queue_packets: usize,
    pub event_capacity: usize,
    pub max_callbacks_per_pump: usize,
    pub max_send_datagrams_per_connection_per_pump: usize,
    pub max_receive_datagrams_per_connection_per_pump: usize,
    pub pending_admission_timeout_ms: u64,
    pub connect_timeout_ms: u64,
}

impl Default for SteamTransportConfig {
    fn default() -> Self {
        Self {
            endpoint_queue_packets: DEFAULT_STEAM_ENDPOINT_QUEUE_PACKETS,
            event_capacity: 32,
            max_callbacks_per_pump: 32,
            max_send_datagrams_per_connection_per_pump: 32,
            max_receive_datagrams_per_connection_per_pump: 32,
            pending_admission_timeout_ms: DEFAULT_PENDING_ADMISSION_TIMEOUT_MS,
            connect_timeout_ms: DEFAULT_CONNECT_TIMEOUT_MS,
        }
    }
}

impl SteamTransportConfig {
    pub fn validate(self) -> Result<(), SteamTransportError> {
        if self.endpoint_queue_packets == 0
            || self.event_capacity == 0
            || self.max_callbacks_per_pump == 0
            || self.max_send_datagrams_per_connection_per_pump == 0
            || self.max_receive_datagrams_per_connection_per_pump == 0
            || self.pending_admission_timeout_ms == 0
            || self.connect_timeout_ms == 0
        {
            return Err(SteamTransportError::InvalidConfiguration);
        }
        if STEAM_TRANSPORT_RETIREMENT_HARD_TIMEOUT_MS
            < ENDPOINT_DROP_DRAIN_HARD_TIMEOUT_MS + ENDPOINT_DROP_DRAIN_QUIET_MS
        {
            return Err(SteamTransportError::InvalidConfiguration);
        }
        if self.endpoint_queue_packets > MAX_STEAM_ENDPOINT_QUEUE_PACKETS
            || self.event_capacity > MAX_STEAM_TRANSPORT_EVENTS
            || self.max_callbacks_per_pump > MAX_STEAM_TRANSPORT_CALLBACKS_PER_PUMP
            || self.max_send_datagrams_per_connection_per_pump
                > MAX_STEAM_TRANSPORT_DATAGRAMS_PER_PUMP
            || self.max_receive_datagrams_per_connection_per_pump
                > MAX_STEAM_TRANSPORT_DATAGRAMS_PER_PUMP
            || self.pending_admission_timeout_ms > MAX_TIMEOUT_MS
            || self.connect_timeout_ms > MAX_TIMEOUT_MS
        {
            return Err(SteamTransportError::CapacityExceeded);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SteamRelayAvailability {
    #[default]
    Unknown,
    NeverTried,
    Waiting,
    Attempting,
    Current,
    CannotTry,
    Failed,
    PreviouslyAvailable,
    Retrying,
}

impl SteamRelayAvailability {
    pub const fn is_ready(self) -> bool {
        matches!(self, Self::Current)
    }

    pub const fn is_terminal_failure(self) -> bool {
        matches!(self, Self::CannotTry | Self::Failed)
    }

    pub const fn diagnostic_code(self) -> u8 {
        match self {
            Self::Unknown => 0,
            Self::NeverTried => 1,
            Self::Waiting => 2,
            Self::Attempting => 3,
            Self::Current => 4,
            Self::CannotTry => 5,
            Self::Failed => 6,
            Self::PreviouslyAvailable => 7,
            Self::Retrying => 8,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SteamRelayStatus {
    pub availability: SteamRelayAvailability,
    pub network_config: SteamRelayAvailability,
    pub any_relay: SteamRelayAvailability,
    pub ping_measurement_in_progress: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SteamNetworkReadiness {
    pub relay: SteamRelayStatus,
    pub authentication: SteamRelayAvailability,
}

impl SteamNetworkReadiness {
    pub const fn is_ready(self) -> bool {
        self.relay.availability.is_ready() && self.authentication.is_ready()
    }

    pub const fn has_terminal_failure(self) -> bool {
        self.relay.availability.is_terminal_failure()
            || self.relay.network_config.is_terminal_failure()
            || self.authentication.is_terminal_failure()
    }
}

impl Default for SteamRelayStatus {
    fn default() -> Self {
        Self {
            availability: SteamRelayAvailability::Unknown,
            network_config: SteamRelayAvailability::Unknown,
            any_relay: SteamRelayAvailability::Unknown,
            ping_measurement_in_progress: false,
        }
    }
}

/// Reads Steam's process-global relay readiness into the bounded status used by
/// the runtime and guarded development UI. Steam's free-form diagnostic string
/// deliberately does not cross this boundary.
#[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
pub(crate) fn steam_client_relay_status(client: &steamworks::Client) -> SteamRelayStatus {
    let status = client.networking_utils().detailed_relay_network_status();
    SteamRelayStatus {
        availability: map_steam_relay_availability(status.availability()),
        network_config: map_steam_relay_availability(status.network_config()),
        any_relay: map_steam_relay_availability(status.any_relay()),
        ping_measurement_in_progress: status.is_ping_measurement_in_progress(),
    }
}

#[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
fn map_steam_relay_availability(
    availability: Result<
        steamworks::networking_types::NetworkingAvailability,
        steamworks::networking_types::NetworkingAvailabilityError,
    >,
) -> SteamRelayAvailability {
    use steamworks::networking_types::{NetworkingAvailability, NetworkingAvailabilityError};

    match availability {
        Ok(NetworkingAvailability::NeverTried) => SteamRelayAvailability::NeverTried,
        Ok(NetworkingAvailability::Waiting) => SteamRelayAvailability::Waiting,
        Ok(NetworkingAvailability::Attempting) => SteamRelayAvailability::Attempting,
        Ok(NetworkingAvailability::Current) => SteamRelayAvailability::Current,
        Err(NetworkingAvailabilityError::Unknown) => SteamRelayAvailability::Unknown,
        Err(NetworkingAvailabilityError::CannotTry) => SteamRelayAvailability::CannotTry,
        Err(NetworkingAvailabilityError::Failed) => SteamRelayAvailability::Failed,
        Err(NetworkingAvailabilityError::Previously) => SteamRelayAvailability::PreviouslyAvailable,
        Err(NetworkingAvailabilityError::Retrying) => SteamRelayAvailability::Retrying,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SteamConnectionQuality {
    pub ping_ms: Option<u32>,
    pub local_delivery_permyriad: Option<u16>,
    pub remote_delivery_permyriad: Option<u16>,
    pub outbound_packets_per_second: u32,
    pub outbound_bytes_per_second: u32,
    pub inbound_packets_per_second: u32,
    pub inbound_bytes_per_second: u32,
    pub estimated_send_rate_bytes_per_second: u32,
    pub pending_unreliable_bytes: u32,
    pub pending_reliable_bytes: u32,
    pub sent_unacked_reliable_bytes: u32,
    pub estimated_queue_delay_micros: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SteamTransportConnectionState {
    PendingAdmission,
    Accepting,
    Connecting,
    Connected,
}

/// Application setup lifecycle for one exact physical Steam connection.
///
/// `SteamTransportConnectionState` continues to expose native handle state;
/// this phase is the security boundary that prevents quarantined bytes from
/// reaching gameplay.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SteamPeerSetupPhase {
    Connecting,
    ControlReady,
    Authenticating,
    Secure,
    ManifestAgreement,
    /// Gameplay receives are buffered, but no endpoint is exposed and no
    /// gameplay sends are permitted yet.
    GameplayReceiveArmed,
    GameplayReady,
}

impl SteamPeerSetupPhase {
    pub const fn diagnostic_code(self) -> u8 {
        match self {
            Self::Connecting => 1,
            Self::ControlReady => 2,
            Self::Authenticating => 3,
            Self::Secure => 4,
            Self::ManifestAgreement => 5,
            Self::GameplayReceiveArmed => 6,
            Self::GameplayReady => 7,
        }
    }

    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Connecting, Self::ControlReady)
                | (Self::ControlReady, Self::Authenticating)
                | (Self::Authenticating, Self::Secure)
                | (Self::Secure, Self::ManifestAgreement)
                | (Self::ManifestAgreement, Self::GameplayReceiveArmed)
                | (Self::GameplayReceiveArmed, Self::GameplayReady)
        )
    }
}

pub const STEAM_PEER_TRACE_EVENT_CAPACITY: usize = 64;
pub const RETAINED_STEAM_PEER_TRACE_CAPACITY: usize = 16;
pub const STEAM_TRACE_CONNECTION_STARTED: u16 = 501;
pub const STEAM_TRACE_CONNECTION_READY: u16 = 502;
pub const STEAM_TRACE_CONNECTION_RETRY_STARTED: u16 = 503;
pub const STEAM_TRACE_SETUP_PHASE_BASE: u16 = 520;
pub const STEAM_TRACE_MANIFEST_ABORTED: u16 = 529;

/// One privacy-safe setup breadcrumb. It deliberately cannot represent a Steam
/// identity, network address, persona, ticket, payload, or native diagnostic
/// string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SteamPeerTraceEvent {
    pub ordinal: u64,
    pub connection_generation: u32,
    pub phase: SteamPeerSetupPhase,
    pub elapsed_ms: u64,
    pub relay_availability: SteamRelayAvailability,
    pub authentication_availability: SteamRelayAvailability,
    pub result_code: u16,
    pub native_end_reason: i32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SteamPeerTrace {
    pub events: Vec<SteamPeerTraceEvent>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SteamTransportCloseReason {
    Requested,
    QualityPolicyRejected,
    AdmissionRejected,
    AdmissionTimedOut,
    ConnectTimedOut,
    RemoteClosed,
    LocalProblem,
    EndpointDropped,
    InboundQueueOverflow,
    OversizedDatagram,
    BackendFailure,
    TransportFault,
    MalformedControlTraffic,
    ProtocolViolation,
    AuthenticationRejected,
}

impl SteamTransportCloseReason {
    pub const fn diagnostic_code(self) -> u16 {
        match self {
            Self::Requested => 401,
            Self::QualityPolicyRejected => 402,
            Self::AdmissionRejected => 403,
            Self::AdmissionTimedOut => 404,
            Self::ConnectTimedOut => 405,
            Self::RemoteClosed => 406,
            Self::LocalProblem => 407,
            Self::EndpointDropped => 408,
            Self::InboundQueueOverflow => 409,
            Self::OversizedDatagram => 410,
            Self::BackendFailure => 411,
            Self::TransportFault => 412,
            Self::MalformedControlTraffic => 413,
            Self::ProtocolViolation => 414,
            Self::AuthenticationRejected => 415,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SteamIncomingRejection {
    WrongRole,
    NotInAdmissionRoster,
    Capacity,
    DuplicateUser,
    IdentityMismatch,
    UnexpectedConnection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SteamTransportEvent {
    RelayStatusChanged(SteamRelayStatus),
    IncomingPending {
        connection: SteamConnectionId,
        lobby: SteamLobbyId,
        user: SteamUserId,
        expires_at_ms: u64,
    },
    IncomingRejected {
        user: SteamUserId,
        reason: SteamIncomingRejection,
    },
    ControlReady {
        connection: SteamConnectionId,
        lobby: SteamLobbyId,
        user: SteamUserId,
        generation: u32,
    },
    SetupPhaseChanged {
        connection: SteamConnectionId,
        lobby: SteamLobbyId,
        user: SteamUserId,
        phase: SteamPeerSetupPhase,
    },
    ControlRetrying {
        connection: SteamConnectionId,
        lobby: SteamLobbyId,
        user: SteamUserId,
        retry_at_ms: u64,
    },
    ConnectionReady {
        connection: SteamConnectionId,
        lobby: SteamLobbyId,
        user: SteamUserId,
    },
    ConnectionClosed {
        connection: SteamConnectionId,
        lobby: SteamLobbyId,
        user: SteamUserId,
        reason: SteamTransportCloseReason,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SteamTransportError {
    InvalidConfiguration,
    InvalidVirtualPort,
    InvalidState,
    CapacityExceeded,
    AuthorityIdentityMismatch,
    AdmissionLobbyMismatch,
    AdmissionUserMismatch,
    AdmissionAuthorityMismatch,
    AdmissionIdentityMismatch,
    DuplicateRemoteUser,
    UnknownConnection,
    EndpointNotReady,
    EndpointAlreadyTaken,
    TimeRegression,
    EventQueueOverflow,
    CallbackQueueOverflow,
    CallbackOwnerGone,
    BackendUnavailable,
    BackendOperationFailed,
    BackendIntegrityFailure,
    HostedDedicatedSdrUnavailable,
    Faulted,
    ControlCodec(SteamControlCodecError),
    ControlQueueOverflow,
    IllegalSetupTransition,
}

impl fmt::Display for SteamTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Steam transport failure: {self:?}")
    }
}

impl SteamTransportError {
    /// Stable privacy-safe diagnostic mapping. Variants may be reordered
    /// without changing persisted diagnostics.
    pub const fn diagnostic_code(self) -> u16 {
        match self {
            // 3xx belongs to AFCP codec failures, 4xx to connection close
            // reasons, and 5xx to trace lifecycle breadcrumbs. Keep transport
            // faults in their own stable range so a persisted result code is
            // globally unambiguous without relying on an enum ordinal.
            Self::InvalidConfiguration => 601,
            Self::InvalidVirtualPort => 602,
            Self::InvalidState => 603,
            Self::CapacityExceeded => 604,
            Self::AuthorityIdentityMismatch => 605,
            Self::AdmissionLobbyMismatch => 606,
            Self::AdmissionUserMismatch => 607,
            Self::AdmissionAuthorityMismatch => 608,
            Self::AdmissionIdentityMismatch => 609,
            Self::DuplicateRemoteUser => 610,
            Self::UnknownConnection => 611,
            Self::EndpointNotReady => 612,
            Self::EndpointAlreadyTaken => 613,
            Self::TimeRegression => 614,
            Self::EventQueueOverflow => 615,
            Self::CallbackQueueOverflow => 616,
            Self::CallbackOwnerGone => 617,
            Self::BackendUnavailable => 618,
            Self::BackendOperationFailed => 619,
            Self::BackendIntegrityFailure => 620,
            Self::HostedDedicatedSdrUnavailable => 621,
            Self::Faulted => 622,
            Self::ControlCodec(error) => error.diagnostic_code(),
            Self::ControlQueueOverflow => 623,
            Self::IllegalSetupTransition => 624,
        }
    }
}

impl std::error::Error for SteamTransportError {}

/// Observable lifecycle of a transport that no longer owns an active match.
///
/// `Complete`, `TimedOut`, and `Faulted` are sticky. A coordinator may retain a
/// terminal transport until it has retired authentication/binding state, then
/// drop it without losing the exact terminal outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SteamTransportRetirementStatus {
    Draining,
    Complete,
    TimedOut,
    Faulted(SteamTransportError),
}

impl SteamTransportRetirementStatus {
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Draining)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SteamEndpointQueueMetrics {
    pub capacity_packets: usize,
    pub outbound_depth_packets: usize,
    pub outbound_high_water_packets: usize,
    pub inbound_depth_packets: usize,
    pub inbound_high_water_packets: usize,
    pub queued_outbound_packets: u64,
    pub delivered_inbound_packets: u64,
    pub full_outbound_attempts: u64,
}

struct EndpointShared {
    connected: AtomicBool,
    receive_enabled: AtomicBool,
    endpoint_alive: AtomicBool,
    capacity: usize,
    outbound_depth: AtomicUsize,
    outbound_high_water: AtomicUsize,
    inbound_depth: AtomicUsize,
    inbound_high_water: AtomicUsize,
    queued_outbound_packets: AtomicU64,
    delivered_inbound_packets: AtomicU64,
    full_outbound_attempts: AtomicU64,
}

impl EndpointShared {
    fn new(capacity: usize) -> Self {
        Self {
            connected: AtomicBool::new(true),
            receive_enabled: AtomicBool::new(true),
            endpoint_alive: AtomicBool::new(true),
            capacity,
            outbound_depth: AtomicUsize::new(0),
            outbound_high_water: AtomicUsize::new(0),
            inbound_depth: AtomicUsize::new(0),
            inbound_high_water: AtomicUsize::new(0),
            queued_outbound_packets: AtomicU64::new(0),
            delivered_inbound_packets: AtomicU64::new(0),
            full_outbound_attempts: AtomicU64::new(0),
        }
    }

    fn snapshot(&self) -> SteamEndpointQueueMetrics {
        SteamEndpointQueueMetrics {
            capacity_packets: self.capacity,
            outbound_depth_packets: self.outbound_depth.load(Ordering::Relaxed),
            outbound_high_water_packets: self.outbound_high_water.load(Ordering::Relaxed),
            inbound_depth_packets: self.inbound_depth.load(Ordering::Relaxed),
            inbound_high_water_packets: self.inbound_high_water.load(Ordering::Relaxed),
            queued_outbound_packets: self.queued_outbound_packets.load(Ordering::Relaxed),
            delivered_inbound_packets: self.delivered_inbound_packets.load(Ordering::Relaxed),
            full_outbound_attempts: self.full_outbound_attempts.load(Ordering::Relaxed),
        }
    }
}

fn update_high_water(counter: &AtomicUsize, candidate: usize) {
    let mut current = counter.load(Ordering::Relaxed);
    while candidate > current {
        match counter.compare_exchange_weak(
            current,
            candidate,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(actual) => current = actual,
        }
    }
}

fn reserve_depth(depth: &AtomicUsize, capacity: usize) -> Option<usize> {
    let mut current = depth.load(Ordering::Relaxed);
    loop {
        if current >= capacity {
            return None;
        }
        match depth.compare_exchange_weak(current, current + 1, Ordering::AcqRel, Ordering::Relaxed)
        {
            Ok(_) => return Some(current + 1),
            Err(actual) => current = actual,
        }
    }
}

fn release_depth(depth: &AtomicUsize) {
    let _ = depth.fetch_update(Ordering::AcqRel, Ordering::Relaxed, |value| {
        Some(value.saturating_sub(1))
    });
}

/// The gameplay-facing half of one admitted Steam connection.
///
/// Both directions use bounded synchronous queues. Calls never block and the
/// original datagram is returned whenever it was not queued.
pub struct SteamDatagramEndpoint {
    outbound: SyncSender<AfcDatagram>,
    inbound: Receiver<AfcDatagram>,
    shared: Arc<EndpointShared>,
}

impl SteamDatagramEndpoint {
    pub fn metrics(&self) -> SteamEndpointQueueMetrics {
        self.shared.snapshot()
    }
}

impl NonBlockingDatagramEndpoint for SteamDatagramEndpoint {
    fn try_send(&mut self, datagram: AfcDatagram) -> SendOutcome {
        if !self.shared.connected.load(Ordering::Acquire) {
            return SendOutcome::Disconnected(datagram);
        }
        let Some(depth) = reserve_depth(&self.shared.outbound_depth, self.shared.capacity) else {
            self.shared
                .full_outbound_attempts
                .fetch_add(1, Ordering::Relaxed);
            return SendOutcome::Full(datagram);
        };
        match self.outbound.try_send(datagram) {
            Ok(()) => {
                update_high_water(&self.shared.outbound_high_water, depth);
                self.shared
                    .queued_outbound_packets
                    .fetch_add(1, Ordering::Relaxed);
                SendOutcome::Sent
            }
            Err(TrySendError::Full(datagram)) => {
                release_depth(&self.shared.outbound_depth);
                self.shared
                    .full_outbound_attempts
                    .fetch_add(1, Ordering::Relaxed);
                SendOutcome::Full(datagram)
            }
            Err(TrySendError::Disconnected(datagram)) => {
                release_depth(&self.shared.outbound_depth);
                SendOutcome::Disconnected(datagram)
            }
        }
    }

    fn try_receive(&mut self) -> ReceiveOutcome {
        if !self.shared.receive_enabled.load(Ordering::Acquire) {
            return ReceiveOutcome::Disconnected;
        }
        match self.inbound.try_recv() {
            Ok(datagram) => {
                release_depth(&self.shared.inbound_depth);
                self.shared
                    .delivered_inbound_packets
                    .fetch_add(1, Ordering::Relaxed);
                ReceiveOutcome::Received(datagram)
            }
            Err(TryRecvError::Empty) if self.shared.connected.load(Ordering::Acquire) => {
                ReceiveOutcome::Empty
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => ReceiveOutcome::Disconnected,
        }
    }
}

impl Drop for SteamDatagramEndpoint {
    fn drop(&mut self) {
        self.shared.endpoint_alive.store(false, Ordering::Release);
    }
}

pub struct AdmittedSteamEndpoint {
    pub connection: SteamConnectionId,
    pub lobby: SteamLobbyId,
    pub remote_user: SteamUserId,
    pub admission: AuthenticatedSteamPeer,
    pub endpoint: SteamDatagramEndpoint,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SteamTransportMetrics {
    pub pumps: u64,
    pub accepted_connections: u64,
    pub rejected_connections: u64,
    pub connected_endpoints: u64,
    pub closed_connections: u64,
    pub sent_datagrams: u64,
    pub sent_bytes: u64,
    pub received_datagrams: u64,
    pub received_bytes: u64,
    pub send_would_block: u64,
    pub oversized_datagrams: u64,
    pub inbound_queue_overflows: u64,
    pub endpoint_drop_drains_started: u64,
    pub endpoint_drop_drains_quiet_completed: u64,
    pub endpoint_drop_drain_timeouts: u64,
    pub retirements_started: u64,
    pub retirements_completed: u64,
    pub retirement_timeouts: u64,
    pub retirement_faults: u64,
    pub event_high_water: usize,
    pub sent_control_frames: u64,
    pub received_control_frames: u64,
    pub duplicate_control_frames: u64,
    pub acknowledged_control_frames: u64,
    pub malformed_control_frames: u64,
    pub control_outbox_high_water: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(all(feature = "steam-net", not(target_arch = "wasm32"))),
    allow(dead_code)
)]
enum BackendConnectionState {
    Connecting,
    Connected,
    ClosedByPeer,
    ProblemDetectedLocally,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(all(feature = "steam-net", not(target_arch = "wasm32"))),
    allow(dead_code)
)]
enum BackendEvent {
    Incoming {
        connection: SteamConnectionId,
        user: SteamUserId,
    },
    Connected {
        connection: SteamConnectionId,
        user: SteamUserId,
    },
    Closed {
        connection: SteamConnectionId,
        user: SteamUserId,
        local_problem: bool,
        native_end_reason: i32,
    },
    IncomingRejected {
        user: SteamUserId,
        reason: SteamIncomingRejection,
    },
    /// Rejections whose remote Steam identity was malformed or unavailable.
    /// They remain attributable to an inbound connection request, but cannot be
    /// projected as a user-keyed public event.
    IncomingPressure { rejected: u16 },
}

#[derive(Debug, PartialEq, Eq)]
enum BackendSendOutcome {
    Sent,
    WouldBlock,
    Disconnected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackendSendMode {
    Gameplay,
    ReliableControl,
}

#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(
    not(all(feature = "steam-net", not(target_arch = "wasm32"))),
    allow(dead_code)
)]
enum BackendReceiveOutcome {
    Datagram(AfcDatagram),
    Empty,
    Disconnected,
    Oversized(usize),
}

trait SteamTransportBackend {
    fn local_user(&self) -> SteamUserId;
    fn initialize_relay(&mut self) -> Result<(), SteamTransportError>;
    fn initialize_authentication(&mut self) -> Result<SteamRelayAvailability, SteamTransportError>;
    fn authentication_status(&self) -> Result<SteamRelayAvailability, SteamTransportError>;
    fn relay_status(&self) -> Result<SteamRelayStatus, SteamTransportError>;
    fn set_connect_timeout_ms(&mut self, timeout_ms: u64) -> Result<(), SteamTransportError>;
    fn open_p2p_listener(&mut self, virtual_port: i32) -> Result<(), SteamTransportError>;
    fn close_p2p_listener(&mut self);
    fn set_allowed_incoming_users(
        &mut self,
        users: [Option<SteamUserId>; MAX_STEAM_TRANSPORT_CONNECTIONS],
    ) -> Result<(), SteamTransportError>;
    fn connect_p2p(
        &mut self,
        remote: SteamUserId,
        virtual_port: i32,
    ) -> Result<SteamConnectionId, SteamTransportError>;
    fn poll_events(&mut self, maximum: usize) -> Result<Vec<BackendEvent>, SteamTransportError>;
    fn accept(&mut self, connection: SteamConnectionId) -> Result<(), SteamTransportError>;
    fn reject(&mut self, connection: SteamConnectionId, reason: SteamTransportCloseReason);
    fn state(
        &self,
        connection: SteamConnectionId,
    ) -> Result<BackendConnectionState, SteamTransportError>;
    fn native_end_reason(&self, connection: SteamConnectionId) -> Result<i32, SteamTransportError>;
    fn send(
        &mut self,
        connection: SteamConnectionId,
        datagram: &AfcDatagram,
        mode: BackendSendMode,
    ) -> Result<BackendSendOutcome, SteamTransportError>;
    fn receive(
        &mut self,
        connection: SteamConnectionId,
    ) -> Result<BackendReceiveOutcome, SteamTransportError>;
    fn quality(
        &self,
        connection: SteamConnectionId,
    ) -> Result<SteamConnectionQuality, SteamTransportError>;
    fn mark_replacement_eligible(
        &mut self,
        connection: SteamConnectionId,
    ) -> Result<(), SteamTransportError>;
    fn close(&mut self, connection: SteamConnectionId, reason: SteamTransportCloseReason);
}

struct ConnectionIo {
    outbound: Receiver<AfcDatagram>,
    inbound: SyncSender<AfcDatagram>,
    shared: Arc<EndpointShared>,
    pending_send: Option<AfcDatagram>,
    endpoint_drop_drain: Option<EndpointDropDrain>,
    control: ControlIo,
}

struct OutstandingControlFrame {
    sequence: u32,
    datagram: EncodedSteamControlFrame,
    last_sent_at_ms: Option<u64>,
    transaction: Option<crate::steam_control::ManifestTransactionId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingControlAcceptance {
    token: SteamControlIngressToken,
    acknowledgement: u32,
    acknowledgement_generation: u32,
}

struct ControlIo {
    local_generation: u32,
    remote_generation: Option<u32>,
    next_sequence: u32,
    highest_received_sequence: u32,
    highest_accepted_sequence: u32,
    highest_transmitted_sequence: u32,
    highest_acknowledged_sequence: u32,
    hello_transmitted: bool,
    ingress_rejected: bool,
    outstanding: VecDeque<OutstandingControlFrame>,
    acknowledgement: Option<EncodedSteamControlFrame>,
    ingress: VecDeque<SteamControlIngress>,
    pending_acceptance: VecDeque<PendingControlAcceptance>,
}

impl ControlIo {
    fn new(local_generation: u32) -> Self {
        Self {
            local_generation,
            remote_generation: None,
            next_sequence: 1,
            highest_received_sequence: 0,
            highest_accepted_sequence: 0,
            highest_transmitted_sequence: 0,
            highest_acknowledged_sequence: 0,
            hello_transmitted: false,
            ingress_rejected: false,
            outstanding: VecDeque::with_capacity(MAX_STEAM_TRANSPORT_EVENTS),
            acknowledgement: None,
            ingress: VecDeque::with_capacity(MAX_STEAM_TRANSPORT_EVENTS),
            pending_acceptance: VecDeque::with_capacity(MAX_STEAM_TRANSPORT_EVENTS),
        }
    }
}

/// Opaque proof that a particular AFCP frame was delivered to the application.
/// It cannot be forged outside this module and is consumed by the explicit
/// semantic accept/reject APIs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SteamControlIngressToken {
    connection: SteamConnectionId,
    generation: u32,
    sequence: u32,
}

impl SteamControlIngressToken {
    pub const fn connection(self) -> SteamConnectionId {
        self.connection
    }

    pub const fn generation(self) -> u32 {
        self.generation
    }

    pub const fn sequence(self) -> u32 {
        self.sequence
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct SteamControlIngress {
    pub token: SteamControlIngressToken,
    pub connection: SteamConnectionId,
    pub user: SteamUserId,
    pub generation: u32,
    pub sequence: u32,
    pub message: SteamControlMessage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EndpointDropDrain {
    hard_deadline_ms: u64,
    quiet_since_ms: Option<u64>,
    origin: OutboundDrainOrigin,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutboundDrainOrigin {
    EndpointDrop,
    TransportRetirement,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutboundDrainCompletion {
    Quiet,
    TimedOut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TransportRetirement {
    hard_deadline_ms: u64,
    status: SteamTransportRetirementStatus,
}

struct ConnectionRecord {
    id: SteamConnectionId,
    remote_user: SteamUserId,
    replacement_for: Option<SteamConnectionId>,
    replacement_eligible: bool,
    state: SteamTransportConnectionState,
    deadline_ms: u64,
    /// Monotonic instant at which `deadline_ms` was last armed. This lets a
    /// process-global Steam backend outage pause only the portion of a newly
    /// created connection deadline that actually overlapped the outage.
    deadline_started_at_ms: u64,
    admission: Option<AuthenticatedSteamPeer>,
    endpoint: Option<SteamDatagramEndpoint>,
    io: Option<ConnectionIo>,
    setup_phase: SteamPeerSetupPhase,
    trace_started_at_ms: u64,
    trace: VecDeque<SteamPeerTraceEvent>,
    next_trace_ordinal: u64,
    automatic_retry_used: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingControlRetry {
    user: SteamUserId,
    closed_connection: SteamConnectionId,
    retry_at_ms: u64,
    deadline_ms: u64,
    reason: SteamTransportCloseReason,
    initiate: bool,
    automatic_retry_used: bool,
}

/// Owns one Steam P2P session's listen/connect handles and connection pumps.
///
/// Call `SteamPlatform::pump` before this object's [`pump`](Self::pump) method.
/// An endpoint becomes available only after Steam reports `Connected` and, for
/// incoming connections, after [`admit_incoming`](Self::admit_incoming).
pub struct SteamTransport {
    backend: Box<dyn SteamTransportBackend>,
    session: SteamP2pSession,
    config: SteamTransportConfig,
    local_user: SteamUserId,
    listening: bool,
    allowed_incoming_users: [Option<SteamUserId>; MAX_STEAM_TRANSPORT_CONNECTIONS],
    connections: Vec<ConnectionRecord>,
    events: VecDeque<SteamTransportEvent>,
    relay_status: SteamRelayStatus,
    authentication_status: SteamRelayAvailability,
    metrics: SteamTransportMetrics,
    last_now_ms: u64,
    last_fault: Option<SteamTransportError>,
    retirement: Option<TransportRetirement>,
    completed_peer_traces: VecDeque<SteamPeerTrace>,
    pending_control_retries: Vec<PendingControlRetry>,
    control_poll_cursor: usize,
    /// Steam account/backend callbacks can be temporarily unavailable while
    /// an already-open Networking Sockets link remains usable. Physical I/O
    /// keeps pumping, but pre-game expiry and automatic retry timers are
    /// frozen until the coordinator has reconciled the Steam lobby again.
    pregame_deadline_pause_started_at_ms: Option<u64>,
}

impl SteamTransport {
    fn from_backend(
        mut backend: Box<dyn SteamTransportBackend>,
        session: SteamP2pSession,
        config: SteamTransportConfig,
        now_ms: u64,
    ) -> Result<Self, SteamTransportError> {
        config.validate()?;
        let local_user = backend.local_user();
        session.validate(local_user)?;
        backend.initialize_relay()?;
        let mut authentication_status = backend.initialize_authentication()?;
        let mut relay_status = backend.relay_status()?;
        if (SteamNetworkReadiness {
            relay: relay_status,
            authentication: authentication_status,
        })
        .has_terminal_failure()
        {
            // Steam documents both initialization calls as process-global and
            // safe to repeat. A terminal result cached before Online was
            // entered gets one fresh attempt for this new transport.
            backend.initialize_relay()?;
            authentication_status = backend.initialize_authentication()?;
            relay_status = backend.relay_status()?;
        }
        backend.set_connect_timeout_ms(config.connect_timeout_ms)?;
        Ok(Self {
            backend,
            session,
            config,
            local_user,
            listening: false,
            allowed_incoming_users: [None; MAX_STEAM_TRANSPORT_CONNECTIONS],
            connections: Vec::with_capacity(MAX_STEAM_TRANSPORT_CONNECTIONS),
            events: VecDeque::with_capacity(config.event_capacity),
            relay_status,
            authentication_status,
            metrics: SteamTransportMetrics::default(),
            last_now_ms: now_ms,
            last_fault: None,
            retirement: None,
            completed_peer_traces: VecDeque::with_capacity(RETAINED_STEAM_PEER_TRACE_CAPACITY),
            pending_control_retries: Vec::with_capacity(MAX_STEAM_TRANSPORT_CONNECTIONS),
            control_poll_cursor: 0,
            pregame_deadline_pause_started_at_ms: None,
        })
    }

    pub const fn session(&self) -> SteamP2pSession {
        self.session
    }

    pub const fn config(&self) -> SteamTransportConfig {
        self.config
    }

    pub const fn local_user(&self) -> SteamUserId {
        self.local_user
    }

    pub const fn relay_status(&self) -> SteamRelayStatus {
        self.relay_status
    }

    pub const fn network_readiness(&self) -> SteamNetworkReadiness {
        SteamNetworkReadiness {
            relay: self.relay_status,
            authentication: self.authentication_status,
        }
    }

    /// Freezes pre-game connection, authentication, and control-retry expiry
    /// while Steam's process-global account backend is reconnecting.
    ///
    /// Networking Sockets I/O must continue to pump during this interval so a
    /// healthy relay link is not torn down merely because lobby/auth callbacks
    /// are unavailable. Calling this more than once for the same outage is
    /// idempotent and preserves the first observed instant.
    pub fn pause_pregame_deadlines(&mut self, now_ms: u64) -> Result<(), SteamTransportError> {
        self.require_operational()?;
        if now_ms < self.last_now_ms {
            return Err(SteamTransportError::TimeRegression);
        }
        self.pregame_deadline_pause_started_at_ms
            .get_or_insert(now_ms);
        Ok(())
    }

    /// Resumes pre-game expiry after a coherent Steam lobby/account recovery.
    /// Only the time for which each live record actually overlapped the outage
    /// is added, retaining the configured 15-second active setup budget even
    /// for a connection that first appeared while Steam was disconnected.
    pub fn resume_pregame_deadlines(&mut self, now_ms: u64) -> Result<u64, SteamTransportError> {
        self.require_operational()?;
        let Some(paused_at_ms) = self.pregame_deadline_pause_started_at_ms else {
            return Ok(0);
        };
        if now_ms < self.last_now_ms || now_ms < paused_at_ms {
            return Err(SteamTransportError::TimeRegression);
        }
        let paused_ms = now_ms.saturating_sub(paused_at_ms);

        // Validate every checked shift before mutating any record. A clock
        // near `u64::MAX` must fail atomically instead of partially extending
        // the bounded connection set.
        for record in self
            .connections
            .iter()
            .filter(|record| record.setup_phase < SteamPeerSetupPhase::Secure)
        {
            let deadline_overlap_ms =
                now_ms.saturating_sub(paused_at_ms.max(record.deadline_started_at_ms));
            record
                .deadline_ms
                .checked_add(deadline_overlap_ms)
                .ok_or(SteamTransportError::InvalidConfiguration)?;
            record
                .deadline_started_at_ms
                .checked_add(deadline_overlap_ms)
                .ok_or(SteamTransportError::InvalidConfiguration)?;
            let trace_overlap_ms =
                now_ms.saturating_sub(paused_at_ms.max(record.trace_started_at_ms));
            record
                .trace_started_at_ms
                .checked_add(trace_overlap_ms)
                .ok_or(SteamTransportError::InvalidConfiguration)?;
        }
        for retry in &self.pending_control_retries {
            retry
                .retry_at_ms
                .checked_add(paused_ms)
                .ok_or(SteamTransportError::InvalidConfiguration)?;
            retry
                .deadline_ms
                .checked_add(paused_ms)
                .ok_or(SteamTransportError::InvalidConfiguration)?;
        }

        for record in self
            .connections
            .iter_mut()
            .filter(|record| record.setup_phase < SteamPeerSetupPhase::Secure)
        {
            let deadline_overlap_ms =
                now_ms.saturating_sub(paused_at_ms.max(record.deadline_started_at_ms));
            record.deadline_ms += deadline_overlap_ms;
            record.deadline_started_at_ms += deadline_overlap_ms;
            let trace_overlap_ms =
                now_ms.saturating_sub(paused_at_ms.max(record.trace_started_at_ms));
            record.trace_started_at_ms += trace_overlap_ms;
        }
        for retry in &mut self.pending_control_retries {
            retry.retry_at_ms += paused_ms;
            retry.deadline_ms += paused_ms;
        }
        self.pregame_deadline_pause_started_at_ms = None;
        Ok(paused_ms)
    }

    /// Re-attempts process-global relay and certificate initialization after a
    /// terminal setup failure. Existing links are not recreated by this call.
    pub fn retry_network_initialization(&mut self) -> Result<(), SteamTransportError> {
        self.require_operational()?;
        self.backend.initialize_relay()?;
        self.authentication_status = self.backend.initialize_authentication()?;
        self.relay_status = self.backend.relay_status()?;
        Ok(())
    }

    pub const fn metrics(&self) -> SteamTransportMetrics {
        self.metrics
    }

    pub const fn last_fault(&self) -> Option<SteamTransportError> {
        self.last_fault
    }

    pub const fn is_faulted(&self) -> bool {
        self.last_fault.is_some()
    }

    pub const fn is_listening(&self) -> bool {
        self.listening
    }

    #[cfg(test)]
    pub(crate) fn incoming_user_is_allowed(&self, user: SteamUserId) -> bool {
        self.allowed_incoming_users.contains(&Some(user))
    }

    pub const fn retirement_status(&self) -> Option<SteamTransportRetirementStatus> {
        match self.retirement {
            Some(retirement) => Some(retirement.status),
            None => None,
        }
    }

    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }

    pub fn peer_trace(&self, connection: SteamConnectionId) -> Option<SteamPeerTrace> {
        self.connections
            .iter()
            .find(|record| record.id == connection)
            .map(|record| SteamPeerTrace {
                events: record.trace.iter().copied().collect(),
            })
    }

    pub fn take_completed_peer_trace(&mut self) -> Option<SteamPeerTrace> {
        self.completed_peer_traces.pop_front()
    }

    pub fn connection_for_user(&self, user: SteamUserId) -> Option<SteamConnectionId> {
        self.connections
            .iter()
            .find(|record| record.remote_user == user)
            .map(|record| record.id)
    }

    pub fn outstanding_control_frames(&self, connection: SteamConnectionId) -> Option<usize> {
        self.connections
            .iter()
            .find(|record| record.id == connection)
            .and_then(|record| record.io.as_ref())
            .map(|io| io.control.outstanding.len())
    }

    /// Counts retained frames for one manifest transaction. Frames are not
    /// removed because deleting a reliable ordered sequence creates a gap and
    /// rewriting an already transmitted sequence makes retransmissions
    /// ambiguous. Known-stale transactions must instead be ACKed and ignored.
    pub fn outstanding_control_frames_for_transaction(
        &self,
        connection: SteamConnectionId,
        transaction: crate::steam_control::ManifestTransactionId,
    ) -> Option<usize> {
        self.connections
            .iter()
            .find(|record| record.id == connection)
            .and_then(|record| record.io.as_ref())
            .map(|io| {
                io.control
                    .outstanding
                    .iter()
                    .filter(|frame| frame.transaction == Some(transaction))
                    .count()
            })
    }

    pub const fn hosted_dedicated_sdr_support(&self) -> DedicatedSdrSupport {
        DedicatedSdrSupport::UnavailableInPinnedBinding
    }

    pub fn open_hosted_dedicated_listener(&mut self) -> Result<(), SteamTransportError> {
        Err(SteamTransportError::HostedDedicatedSdrUnavailable)
    }

    pub fn poll_event(&mut self) -> Option<SteamTransportEvent> {
        self.events.pop_front()
    }

    /// Monotonically retires this match transport without discarding datagrams
    /// already accepted from gameplay endpoints.
    ///
    /// Listener admission, public events, and endpoint/backend receive stop
    /// immediately. Connected endpoint senders remain usable until their
    /// outbound-only drain reaches 50 ms quiet, its exact 250 ms endpoint cap,
    /// or the fixed 300 ms whole-transport cap. Calling this again is
    /// idempotent and returns the sticky current outcome.
    pub fn begin_retirement(&mut self, now_ms: u64) -> SteamTransportRetirementStatus {
        if let Some(status) = self.retirement_status() {
            return status;
        }
        if STEAM_TRANSPORT_RETIREMENT_HARD_TIMEOUT_MS
            < ENDPOINT_DROP_DRAIN_HARD_TIMEOUT_MS + ENDPOINT_DROP_DRAIN_QUIET_MS
        {
            return self.begin_faulted_retirement(SteamTransportError::InvalidConfiguration);
        }
        if now_ms < self.last_now_ms {
            return self.begin_faulted_retirement(SteamTransportError::TimeRegression);
        }
        let Some(hard_deadline_ms) = now_ms.checked_add(STEAM_TRANSPORT_RETIREMENT_HARD_TIMEOUT_MS)
        else {
            return self.begin_faulted_retirement(SteamTransportError::InvalidConfiguration);
        };
        if let Some(error) = self.last_fault {
            return self.begin_faulted_retirement(error);
        }

        self.last_now_ms = now_ms;
        self.metrics.retirements_started = self.metrics.retirements_started.saturating_add(1);
        self.retirement = Some(TransportRetirement {
            hard_deadline_ms,
            status: SteamTransportRetirementStatus::Draining,
        });
        self.pending_control_retries.clear();

        self.backend.close_p2p_listener();
        self.listening = false;
        self.allowed_incoming_users = [None; MAX_STEAM_TRANSPORT_CONNECTIONS];
        if let Err(error) = self
            .backend
            .set_allowed_incoming_users(self.allowed_incoming_users)
        {
            return self.finish_retirement(SteamTransportRetirementStatus::Faulted(error));
        }
        self.events.clear();

        let non_connected: Vec<_> = self
            .connections
            .iter()
            .filter(|record| record.state != SteamTransportConnectionState::Connected)
            .map(|record| record.id)
            .collect();
        for connection in non_connected {
            if self
                .close_connection_internal(connection, SteamTransportCloseReason::Requested, false)
                .is_err()
            {
                return self.finish_retirement(SteamTransportRetirementStatus::Faulted(
                    SteamTransportError::BackendIntegrityFailure,
                ));
            }
        }

        for record in &mut self.connections {
            let Some(io) = record.io.as_mut() else {
                return self.finish_retirement(SteamTransportRetirementStatus::Faulted(
                    SteamTransportError::BackendIntegrityFailure,
                ));
            };
            io.shared.receive_enabled.store(false, Ordering::Release);
            if let Err(error) = begin_outbound_drain(
                io,
                now_ms,
                OutboundDrainOrigin::TransportRetirement,
                &mut self.metrics,
            ) {
                return self.finish_retirement(SteamTransportRetirementStatus::Faulted(error));
            }
        }

        if self.connections.is_empty() {
            self.finish_retirement(SteamTransportRetirementStatus::Complete)
        } else {
            SteamTransportRetirementStatus::Draining
        }
    }

    /// Pumps only bounded outbound endpoint work for a retiring transport.
    ///
    /// The outbound budget is always attempted before either endpoint or
    /// whole-transport deadlines are evaluated. This ordering is what lets an
    /// ACK queued immediately before a worker drops survive a delayed next
    /// application frame.
    pub fn pump_retirement(&mut self, now_ms: u64) -> SteamTransportRetirementStatus {
        let Some(retirement) = self.retirement else {
            return self.begin_faulted_retirement(SteamTransportError::InvalidState);
        };
        if retirement.status.is_terminal() {
            return retirement.status;
        }
        if now_ms < self.last_now_ms {
            return self.finish_retirement(SteamTransportRetirementStatus::Faulted(
                SteamTransportError::TimeRegression,
            ));
        }
        self.last_now_ms = now_ms;
        self.metrics.pumps = self.metrics.pumps.saturating_add(1);

        let mut closures = Vec::with_capacity(MAX_STEAM_TRANSPORT_CONNECTIONS);
        let mut timed_out = false;
        let mut io_fault = None;
        {
            let backend = self.backend.as_mut();
            let metrics = &mut self.metrics;
            for record in &mut self.connections {
                if record.state != SteamTransportConnectionState::Connected {
                    io_fault = Some(SteamTransportError::BackendIntegrityFailure);
                    break;
                }
                let Some(io) = record.io.as_mut() else {
                    io_fault = Some(SteamTransportError::BackendIntegrityFailure);
                    break;
                };
                match pump_retiring_connection_io(
                    backend,
                    record.id,
                    io,
                    self.config,
                    metrics,
                    now_ms,
                ) {
                    Ok(Some(OutboundDrainCompletion::Quiet)) => closures.push(record.id),
                    Ok(Some(OutboundDrainCompletion::TimedOut)) => {
                        timed_out = true;
                        closures.push(record.id);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        io_fault = Some(error);
                        break;
                    }
                }
            }
        }

        if let Some(error) = io_fault {
            return self.finish_retirement(SteamTransportRetirementStatus::Faulted(error));
        }
        for connection in closures {
            if self
                .close_connection_internal(
                    connection,
                    SteamTransportCloseReason::EndpointDropped,
                    false,
                )
                .is_err()
            {
                return self.finish_retirement(SteamTransportRetirementStatus::Faulted(
                    SteamTransportError::BackendIntegrityFailure,
                ));
            }
        }
        if timed_out {
            return self.finish_retirement(SteamTransportRetirementStatus::TimedOut);
        }
        if self.connections.is_empty() {
            return self.finish_retirement(SteamTransportRetirementStatus::Complete);
        }
        if now_ms >= retirement.hard_deadline_ms {
            return self.finish_retirement(SteamTransportRetirementStatus::TimedOut);
        }
        SteamTransportRetirementStatus::Draining
    }

    /// Replaces the exact bounded set of Steam identities that may consume an
    /// inbound listen-socket connection slot. The online-lobby coordinator
    /// refreshes this after every platform roster update and immediately before
    /// each transport pump.
    pub fn set_allowed_incoming_users(
        &mut self,
        users: &[SteamUserId],
    ) -> Result<(), SteamTransportError> {
        self.require_operational()?;
        if self.session.role != SteamTransportRole::ListenAuthority {
            return if users.is_empty() {
                Ok(())
            } else {
                Err(SteamTransportError::InvalidState)
            };
        }
        if users.len() > MAX_STEAM_TRANSPORT_CONNECTIONS {
            return Err(SteamTransportError::CapacityExceeded);
        }
        let mut allowed = [None; MAX_STEAM_TRANSPORT_CONNECTIONS];
        for (index, user) in users.iter().copied().enumerate() {
            if user == self.local_user || allowed[..index].contains(&Some(user)) {
                return Err(SteamTransportError::AdmissionUserMismatch);
            }
            allowed[index] = Some(user);
        }
        self.backend.set_allowed_incoming_users(allowed)?;
        self.allowed_incoming_users = allowed;
        Ok(())
    }

    pub fn start_listening(&mut self) -> Result<(), SteamTransportError> {
        self.require_operational()?;
        if self.session.role != SteamTransportRole::ListenAuthority || self.listening {
            return Err(SteamTransportError::InvalidState);
        }
        self.backend.open_p2p_listener(self.session.virtual_port)?;
        self.listening = true;
        Ok(())
    }

    pub fn stop_listening(&mut self) -> Result<(), SteamTransportError> {
        self.require_operational()?;
        self.backend.close_p2p_listener();
        self.listening = false;
        let pending: Vec<_> = self
            .connections
            .iter()
            .filter(|record| record.state == SteamTransportConnectionState::PendingAdmission)
            .map(|record| record.id)
            .collect();
        self.metrics.rejected_connections = self
            .metrics
            .rejected_connections
            .saturating_add(pending.len() as u64);
        for connection in pending {
            if let Err(error) = self.close_connection_internal(
                connection,
                SteamTransportCloseReason::AdmissionRejected,
                true,
            ) {
                return self.fail_closed(error);
            }
        }
        Ok(())
    }

    pub fn connect_p2p(
        &mut self,
        admission: AuthenticatedSteamPeer,
        now_ms: u64,
    ) -> Result<SteamConnectionId, SteamTransportError> {
        self.require_operational()?;
        self.advance_time(now_ms)?;
        if self.session.role != SteamTransportRole::Client {
            return Err(SteamTransportError::InvalidState);
        }
        self.validate_admission(admission, self.session.authority_user)?;
        let same_user: Vec<_> = self
            .connections
            .iter()
            .filter(|record| record.remote_user == admission.user)
            .map(|record| record.id)
            .collect();
        let replacement_for = if same_user.is_empty() {
            None
        } else if same_user.len() == 1
            && self
                .connections
                .iter()
                .find(|record| record.id == same_user[0])
                .is_some_and(|record| record.replacement_eligible)
            && admission.purpose == AdmissionPurpose::Reconnect
        {
            Some(same_user[0])
        } else {
            return Err(SteamTransportError::DuplicateRemoteUser);
        };
        if self.connections.len() >= MAX_STEAM_TRANSPORT_CONNECTIONS {
            return Err(SteamTransportError::CapacityExceeded);
        }
        let connect_deadline_ms = deadline(now_ms, self.config.connect_timeout_ms)?;
        let id = self
            .backend
            .connect_p2p(admission.user, self.session.virtual_port)?;
        if self.connections.iter().any(|record| record.id == id) {
            self.backend
                .close(id, SteamTransportCloseReason::TransportFault);
            return self.fail_closed(SteamTransportError::BackendIntegrityFailure);
        }
        if let Some(old) = replacement_for
            && let Some(record) = self.connections.iter_mut().find(|record| record.id == old)
        {
            record.replacement_eligible = false;
        }
        self.connections.push(ConnectionRecord {
            id,
            remote_user: admission.user,
            replacement_for,
            replacement_eligible: false,
            state: SteamTransportConnectionState::Connecting,
            deadline_ms: connect_deadline_ms,
            deadline_started_at_ms: now_ms,
            admission: Some(admission),
            endpoint: None,
            io: None,
            setup_phase: SteamPeerSetupPhase::Connecting,
            trace_started_at_ms: now_ms,
            trace: VecDeque::with_capacity(STEAM_PEER_TRACE_EVENT_CAPACITY),
            next_trace_ordinal: 1,
            automatic_retry_used: false,
        });
        self.record_peer_trace(id, STEAM_TRACE_CONNECTION_STARTED, 0);
        Ok(id)
    }

    /// Opens the one physical client-to-authority link before Steam ticket
    /// authentication. The connection remains quarantined until explicit
    /// promotion after mutual authentication and manifest commit.
    pub fn connect_control(
        &mut self,
        remote: SteamUserId,
        now_ms: u64,
    ) -> Result<SteamConnectionId, SteamTransportError> {
        self.require_operational()?;
        self.advance_time(now_ms)?;
        if self.session.role != SteamTransportRole::Client
            || remote != self.session.authority_user
            || self
                .connections
                .iter()
                .any(|record| record.remote_user == remote)
        {
            return Err(SteamTransportError::InvalidState);
        }
        if self.connections.len() >= MAX_STEAM_TRANSPORT_CONNECTIONS {
            return Err(SteamTransportError::CapacityExceeded);
        }
        let id = self
            .backend
            .connect_p2p(remote, self.session.virtual_port)?;
        if self.connections.iter().any(|record| record.id == id) {
            self.backend
                .close(id, SteamTransportCloseReason::TransportFault);
            return self.fail_closed(SteamTransportError::BackendIntegrityFailure);
        }
        self.connections.push(ConnectionRecord {
            id,
            remote_user: remote,
            replacement_for: None,
            replacement_eligible: false,
            state: SteamTransportConnectionState::Connecting,
            deadline_ms: deadline(now_ms, self.config.connect_timeout_ms)?,
            deadline_started_at_ms: now_ms,
            admission: None,
            endpoint: None,
            io: None,
            setup_phase: SteamPeerSetupPhase::Connecting,
            trace_started_at_ms: now_ms,
            trace: VecDeque::with_capacity(STEAM_PEER_TRACE_EVENT_CAPACITY),
            next_trace_ordinal: 1,
            automatic_retry_used: false,
        });
        self.record_peer_trace(id, STEAM_TRACE_CONNECTION_STARTED, 0);
        Ok(id)
    }

    /// Grants one exact live connection a single overlapping reconnect
    /// generation.
    ///
    /// The caller must already have authenticated an authority-authored
    /// `ReconnectAllowed` terminal for this physical generation. The grant is
    /// consumed by the first same-user connection attempt and cannot be
    /// inferred from identity alone.
    pub fn mark_connection_replacement_eligible(
        &mut self,
        connection: SteamConnectionId,
    ) -> Result<(), SteamTransportError> {
        self.require_operational()?;
        let Some(index) = self
            .connections
            .iter()
            .position(|record| record.id == connection)
        else {
            return Err(SteamTransportError::UnknownConnection);
        };
        if self.connections[index].state != SteamTransportConnectionState::Connected
            || self.connections[index].replacement_eligible
            || self.connections.iter().any(|record| {
                record.replacement_for == Some(connection)
                    || (record.remote_user == self.connections[index].remote_user
                        && record.id != connection)
            })
        {
            return Err(SteamTransportError::InvalidState);
        }
        self.backend.mark_replacement_eligible(connection)?;
        self.connections[index].replacement_eligible = true;
        Ok(())
    }

    pub fn admit_incoming(
        &mut self,
        connection: SteamConnectionId,
        admission: AuthenticatedSteamPeer,
        now_ms: u64,
    ) -> Result<(), SteamTransportError> {
        self.require_operational()?;
        self.advance_time(now_ms)?;
        if self.session.role != SteamTransportRole::ListenAuthority {
            return Err(SteamTransportError::InvalidState);
        }
        let Some(index) = self
            .connections
            .iter()
            .position(|record| record.id == connection)
        else {
            return Err(SteamTransportError::UnknownConnection);
        };
        let remote_user = self.connections[index].remote_user;
        if self.connections[index].state != SteamTransportConnectionState::PendingAdmission {
            return Err(SteamTransportError::InvalidState);
        }
        if now_ms >= self.connections[index].deadline_ms {
            if let Err(error) = self.close_connection_internal(
                connection,
                SteamTransportCloseReason::AdmissionTimedOut,
                true,
            ) {
                return self.fail_closed(error);
            }
            return Err(SteamTransportError::InvalidState);
        }
        self.validate_admission(admission, remote_user)?;
        if self.connections[index].replacement_for.is_some()
            && admission.purpose != AdmissionPurpose::Reconnect
        {
            self.metrics.rejected_connections = self.metrics.rejected_connections.saturating_add(1);
            self.close_connection_internal(
                connection,
                SteamTransportCloseReason::AdmissionRejected,
                true,
            )?;
            return Err(SteamTransportError::InvalidState);
        }
        let connect_deadline_ms = deadline(now_ms, self.config.connect_timeout_ms)?;
        self.backend.accept(connection)?;
        let record = &mut self.connections[index];
        record.state = SteamTransportConnectionState::Accepting;
        record.admission = Some(admission);
        record.deadline_ms = connect_deadline_ms;
        record.deadline_started_at_ms = now_ms;
        self.metrics.accepted_connections = self.metrics.accepted_connections.saturating_add(1);
        Ok(())
    }

    /// Promptly accepts a current-lobby identity into the control quarantine.
    /// This grants neither a Steam-authenticated admission nor a gameplay
    /// endpoint.
    pub fn accept_control(
        &mut self,
        connection: SteamConnectionId,
        now_ms: u64,
    ) -> Result<(), SteamTransportError> {
        self.require_operational()?;
        self.advance_time(now_ms)?;
        if self.session.role != SteamTransportRole::ListenAuthority {
            return Err(SteamTransportError::InvalidState);
        }
        let record = self
            .connections
            .iter_mut()
            .find(|record| record.id == connection)
            .ok_or(SteamTransportError::UnknownConnection)?;
        if record.state != SteamTransportConnectionState::PendingAdmission
            || now_ms >= record.deadline_ms
        {
            return Err(SteamTransportError::InvalidState);
        }
        self.backend.accept(connection)?;
        record.state = SteamTransportConnectionState::Accepting;
        record.deadline_ms = deadline(now_ms, self.config.connect_timeout_ms)?;
        record.deadline_started_at_ms = now_ms;
        self.metrics.accepted_connections = self.metrics.accepted_connections.saturating_add(1);
        Ok(())
    }

    pub fn reject_incoming(
        &mut self,
        connection: SteamConnectionId,
    ) -> Result<(), SteamTransportError> {
        self.require_operational()?;
        let Some(record) = self
            .connections
            .iter()
            .find(|record| record.id == connection)
        else {
            return Err(SteamTransportError::UnknownConnection);
        };
        if record.state != SteamTransportConnectionState::PendingAdmission {
            return Err(SteamTransportError::InvalidState);
        }
        self.metrics.rejected_connections = self.metrics.rejected_connections.saturating_add(1);
        let result = self.close_connection_internal(
            connection,
            SteamTransportCloseReason::AdmissionRejected,
            true,
        );
        match result {
            Err(SteamTransportError::EventQueueOverflow) => {
                self.fail_closed(SteamTransportError::EventQueueOverflow)
            }
            result => result,
        }
    }

    pub fn connection_state(
        &self,
        connection: SteamConnectionId,
    ) -> Option<SteamTransportConnectionState> {
        self.connections
            .iter()
            .find(|record| record.id == connection)
            .map(|record| record.state)
    }

    pub fn setup_phase(&self, connection: SteamConnectionId) -> Option<SteamPeerSetupPhase> {
        self.connections
            .iter()
            .find(|record| record.id == connection)
            .map(|record| record.setup_phase)
    }

    pub fn mark_authenticating(
        &mut self,
        connection: SteamConnectionId,
    ) -> Result<(), SteamTransportError> {
        self.transition_setup(connection, SteamPeerSetupPhase::Authenticating)
    }

    /// Installs the exact platform-authenticated lease while retaining the
    /// physical connection in quarantine.
    pub fn mark_secure(
        &mut self,
        connection: SteamConnectionId,
        admission: AuthenticatedSteamPeer,
    ) -> Result<(), SteamTransportError> {
        let remote = self
            .connections
            .iter()
            .find(|record| record.id == connection)
            .map(|record| record.remote_user)
            .ok_or(SteamTransportError::UnknownConnection)?;
        self.validate_admission(admission, remote)?;
        let record = self
            .connections
            .iter_mut()
            .find(|record| record.id == connection)
            .expect("connection was just located");
        if record.admission.is_some() {
            return Err(SteamTransportError::InvalidState);
        }
        record.admission = Some(admission);
        self.transition_setup(connection, SteamPeerSetupPhase::Secure)?;
        self.connections
            .iter_mut()
            .find(|record| record.id == connection)
            .expect("connection was just located")
            .deadline_ms = u64::MAX;
        Ok(())
    }

    pub fn begin_manifest_agreement(
        &mut self,
        connection: SteamConnectionId,
    ) -> Result<(), SteamTransportError> {
        self.transition_setup(connection, SteamPeerSetupPhase::ManifestAgreement)
    }

    pub fn abort_manifest_agreement(
        &mut self,
        connection: SteamConnectionId,
    ) -> Result<(), SteamTransportError> {
        self.require_operational()?;
        let (user, phase) = self
            .connections
            .iter()
            .find(|record| record.id == connection)
            .map(|record| (record.remote_user, record.setup_phase))
            .ok_or(SteamTransportError::UnknownConnection)?;
        if phase == SteamPeerSetupPhase::Secure {
            return Ok(());
        }
        if !matches!(
            phase,
            SteamPeerSetupPhase::ManifestAgreement | SteamPeerSetupPhase::GameplayReceiveArmed
        ) {
            return Err(SteamTransportError::IllegalSetupTransition);
        }
        let record = self
            .connections
            .iter_mut()
            .find(|record| record.id == connection)
            .expect("connection was just located");
        record.setup_phase = SteamPeerSetupPhase::Secure;
        if let Some(endpoint) = record.endpoint.as_mut() {
            while endpoint.inbound.try_recv().is_ok() {
                release_depth(&endpoint.shared.inbound_depth);
            }
        }
        self.record_peer_trace(connection, STEAM_TRACE_MANIFEST_ABORTED, 0);
        self.push_event(SteamTransportEvent::SetupPhaseChanged {
            connection,
            lobby: self.session.lobby,
            user,
            phase: SteamPeerSetupPhase::Secure,
        })
    }

    /// Starts accepting and buffering AFCN gameplay datagrams without exposing
    /// the endpoint or permitting local gameplay sends. This closes the race in
    /// which one participant activates a worker before the authority has
    /// received every activation receipt.
    pub fn arm_gameplay_receive(
        &mut self,
        connection: SteamConnectionId,
    ) -> Result<(), SteamTransportError> {
        self.transition_setup(connection, SteamPeerSetupPhase::GameplayReceiveArmed)
    }

    pub fn promote_to_gameplay(
        &mut self,
        connection: SteamConnectionId,
    ) -> Result<(), SteamTransportError> {
        self.transition_setup(connection, SteamPeerSetupPhase::GameplayReady)?;
        let (user, has_endpoint, has_admission) = self
            .connections
            .iter()
            .find(|record| record.id == connection)
            .map(|record| {
                (
                    record.remote_user,
                    record.endpoint.is_some(),
                    record.admission.is_some(),
                )
            })
            .ok_or(SteamTransportError::UnknownConnection)?;
        if !has_endpoint || !has_admission {
            return Err(SteamTransportError::BackendIntegrityFailure);
        }
        self.metrics.connected_endpoints = self.metrics.connected_endpoints.saturating_add(1);
        self.push_event(SteamTransportEvent::ConnectionReady {
            connection,
            lobby: self.session.lobby,
            user,
        })
    }

    pub fn queue_control(
        &mut self,
        connection: SteamConnectionId,
        message: SteamControlMessage,
    ) -> Result<u32, SteamTransportError> {
        self.require_operational()?;
        let record = self
            .connections
            .iter_mut()
            .find(|record| record.id == connection)
            .ok_or(SteamTransportError::UnknownConnection)?;
        if record.state != SteamTransportConnectionState::Connected
            || record.setup_phase == SteamPeerSetupPhase::Connecting
        {
            return Err(SteamTransportError::InvalidState);
        }
        let identity = message.identity();
        if identity.lobby != self.session.lobby
            || identity.sender != self.local_user
            || identity.recipient != record.remote_user
        {
            return Err(SteamTransportError::AdmissionIdentityMismatch);
        }
        let io = record
            .io
            .as_mut()
            .ok_or(SteamTransportError::BackendIntegrityFailure)?;
        if io.control.outstanding.len() >= self.config.event_capacity {
            return Err(SteamTransportError::ControlQueueOverflow);
        }
        let sequence = io.control.next_sequence;
        let is_hello = matches!(&message, SteamControlMessage::LinkHello { .. });
        if (sequence == 1) != is_hello {
            return Err(SteamTransportError::IllegalSetupTransition);
        }
        io.control.next_sequence = sequence
            .checked_add(1)
            .ok_or(SteamTransportError::CapacityExceeded)?;
        let acknowledgement = io.control.highest_accepted_sequence;
        let acknowledgement_generation = if acknowledgement == 0 {
            None
        } else {
            Some(
                io.control
                    .remote_generation
                    .ok_or(SteamTransportError::IllegalSetupTransition)?,
            )
        };
        let envelope = SteamControlEnvelope::message_with_ack_generation(
            io.control.local_generation,
            sequence,
            acknowledgement_generation,
            acknowledgement,
            message,
        )
        .map_err(SteamTransportError::ControlCodec)?;
        let transaction = envelope
            .message
            .as_ref()
            .and_then(SteamControlMessage::manifest_transaction);
        let datagram =
            encode_steam_control(&envelope).map_err(SteamTransportError::ControlCodec)?;
        io.control.outstanding.push_back(OutstandingControlFrame {
            sequence,
            datagram,
            last_sent_at_ms: None,
            transaction,
        });
        self.metrics.control_outbox_high_water = self
            .metrics
            .control_outbox_high_water
            .max(io.control.outstanding.len());
        Ok(sequence)
    }

    pub fn poll_control(&mut self) -> Option<SteamControlIngress> {
        let count = self.connections.len();
        if count == 0 {
            return None;
        }
        for offset in 0..count {
            let index = (self.control_poll_cursor + offset) % count;
            let Some(io) = self.connections[index].io.as_mut() else {
                continue;
            };
            if let Some(ingress) = io.control.ingress.pop_front() {
                self.control_poll_cursor = (index + 1) % count;
                return Some(ingress);
            }
        }
        self.control_poll_cursor %= count;
        None
    }

    /// Confirms that the application semantically accepted a delivered AFCP
    /// message. Only now are its piggyback ACK and the ACK for the inbound
    /// sequence allowed to mutate control state.
    pub fn accept_control_ingress(
        &mut self,
        token: SteamControlIngressToken,
    ) -> Result<(), SteamTransportError> {
        self.require_operational()?;
        let record = self
            .connections
            .iter_mut()
            .find(|record| record.id == token.connection)
            .ok_or(SteamTransportError::UnknownConnection)?;
        let io = record
            .io
            .as_mut()
            .ok_or(SteamTransportError::BackendIntegrityFailure)?;
        if io.control.ingress_rejected || io.control.remote_generation != Some(token.generation) {
            return Err(SteamTransportError::IllegalSetupTransition);
        }
        if token.sequence <= io.control.highest_accepted_sequence {
            return Ok(());
        }
        let expected = io
            .control
            .highest_accepted_sequence
            .checked_add(1)
            .ok_or(SteamTransportError::CapacityExceeded)?;
        let pending = io
            .control
            .pending_acceptance
            .front()
            .copied()
            .ok_or(SteamTransportError::IllegalSetupTransition)?;
        if pending.token != token || token.sequence != expected {
            return Err(SteamTransportError::IllegalSetupTransition);
        }
        if pending.acknowledgement != 0
            && pending.acknowledgement_generation != io.control.local_generation
        {
            return Err(SteamTransportError::IllegalSetupTransition);
        }
        io.control.pending_acceptance.pop_front();
        apply_control_ack(
            &mut io.control,
            pending.acknowledgement_generation,
            pending.acknowledgement,
            &mut self.metrics,
        )?;
        io.control.highest_accepted_sequence = token.sequence;
        queue_control_ack(
            &mut io.control,
            SteamControlIdentity::new(self.session.lobby, self.local_user, record.remote_user)
                .map_err(SteamTransportError::ControlCodec)?,
        )?;
        Ok(())
    }

    /// Records semantic rejection without acknowledging the rejected frame.
    /// The caller should send any applicable Abort and then close or replace the
    /// peer; this generation will not deliver or acknowledge later ingress.
    pub fn reject_control_ingress(
        &mut self,
        token: SteamControlIngressToken,
    ) -> Result<(), SteamTransportError> {
        self.require_operational()?;
        let record = self
            .connections
            .iter_mut()
            .find(|record| record.id == token.connection)
            .ok_or(SteamTransportError::UnknownConnection)?;
        let io = record
            .io
            .as_mut()
            .ok_or(SteamTransportError::BackendIntegrityFailure)?;
        if io.control.remote_generation != Some(token.generation)
            || !io
                .control
                .pending_acceptance
                .iter()
                .any(|pending| pending.token == token)
        {
            return Err(SteamTransportError::IllegalSetupTransition);
        }
        io.control.ingress_rejected = true;
        io.control.ingress.clear();
        io.control.pending_acceptance.clear();
        Ok(())
    }

    pub fn connection_quality(
        &self,
        connection: SteamConnectionId,
    ) -> Result<SteamConnectionQuality, SteamTransportError> {
        self.require_operational()?;
        let Some(record) = self
            .connections
            .iter()
            .find(|record| record.id == connection)
        else {
            return Err(SteamTransportError::UnknownConnection);
        };
        if record.state != SteamTransportConnectionState::Connected {
            return Err(SteamTransportError::EndpointNotReady);
        }
        self.backend.quality(connection)
    }

    pub fn take_endpoint(
        &mut self,
        connection: SteamConnectionId,
    ) -> Result<AdmittedSteamEndpoint, SteamTransportError> {
        self.require_operational()?;
        let Some(record) = self
            .connections
            .iter_mut()
            .find(|record| record.id == connection)
        else {
            return Err(SteamTransportError::UnknownConnection);
        };
        if record.state != SteamTransportConnectionState::Connected
            || record.setup_phase != SteamPeerSetupPhase::GameplayReady
        {
            return Err(SteamTransportError::EndpointNotReady);
        }
        let endpoint = record
            .endpoint
            .take()
            .ok_or(SteamTransportError::EndpointAlreadyTaken)?;
        let admission = record
            .admission
            .ok_or(SteamTransportError::BackendIntegrityFailure)?;
        Ok(AdmittedSteamEndpoint {
            connection,
            lobby: self.session.lobby,
            remote_user: record.remote_user,
            admission,
            endpoint,
        })
    }

    pub fn close_connection(
        &mut self,
        connection: SteamConnectionId,
    ) -> Result<(), SteamTransportError> {
        self.require_operational()?;
        let result =
            self.close_connection_internal(connection, SteamTransportCloseReason::Requested, true);
        match result {
            Err(SteamTransportError::EventQueueOverflow) => {
                self.fail_closed(SteamTransportError::EventQueueOverflow)
            }
            result => result,
        }
    }

    /// Closes one attributable link because the bounded quality monitor entered
    /// its sustained reject state. This stays distinct from a user-requested
    /// close so the coordinator cannot accidentally enter a reconnect loop.
    pub fn close_connection_for_quality_policy(
        &mut self,
        connection: SteamConnectionId,
    ) -> Result<(), SteamTransportError> {
        self.require_operational()?;
        let result = self.close_connection_internal(
            connection,
            SteamTransportCloseReason::QualityPolicyRejected,
            true,
        );
        match result {
            Err(SteamTransportError::EventQueueOverflow) => {
                self.fail_closed(SteamTransportError::EventQueueOverflow)
            }
            result => result,
        }
    }

    /// Retires one quarantined control generation and schedules one fresh
    /// client-originated generation after the normal bounded retry delay.
    pub fn retry_control_generation(
        &mut self,
        user: SteamUserId,
        now_ms: u64,
    ) -> Result<(), SteamTransportError> {
        self.require_operational()?;
        self.advance_time(now_ms)?;
        if self.pregame_deadline_pause_started_at_ms.is_some() {
            return Err(SteamTransportError::InvalidState);
        }
        if self
            .pending_control_retries
            .iter()
            .any(|retry| retry.user == user)
        {
            return Ok(());
        }
        if self.pending_control_retries.len() >= MAX_STEAM_TRANSPORT_CONNECTIONS {
            return Err(SteamTransportError::CapacityExceeded);
        }
        let connection = self
            .connections
            .iter()
            .find(|record| record.remote_user == user)
            .map(|record| record.id)
            .ok_or(SteamTransportError::UnknownConnection)?;
        let retry_at_ms = deadline(now_ms, CONTROL_RETRY_DELAY_MS)?;
        let deadline_ms = deadline(now_ms, self.config.connect_timeout_ms)?;
        self.close_connection_internal(connection, SteamTransportCloseReason::LocalProblem, false)?;
        self.pending_control_retries.push(PendingControlRetry {
            user,
            closed_connection: connection,
            retry_at_ms,
            deadline_ms,
            reason: SteamTransportCloseReason::LocalProblem,
            initiate: self.session.role == SteamTransportRole::Client,
            automatic_retry_used: false,
        });
        self.push_event(SteamTransportEvent::ControlRetrying {
            connection,
            lobby: self.session.lobby,
            user,
            retry_at_ms,
        })
    }

    /// Schedules a fresh control generation after an exhausted pre-game link
    /// has already been finalized and removed from the transport.
    ///
    /// The client is the only side that opens a replacement connection. A
    /// listen authority records the same bounded deadline but waits for the
    /// admitted client to reconnect, preserving the authority-star topology.
    pub fn retry_closed_control_generation(
        &mut self,
        user: SteamUserId,
        closed_connection: SteamConnectionId,
        now_ms: u64,
    ) -> Result<(), SteamTransportError> {
        self.require_operational()?;
        self.advance_time(now_ms)?;
        if self.pregame_deadline_pause_started_at_ms.is_some() {
            return Err(SteamTransportError::InvalidState);
        }
        if user == self.local_user
            || match self.session.role {
                SteamTransportRole::Client => user != self.session.authority_user,
                SteamTransportRole::ListenAuthority => {
                    !self.allowed_incoming_users.contains(&Some(user))
                }
            }
        {
            return Err(SteamTransportError::AdmissionUserMismatch);
        }
        if self
            .connections
            .iter()
            .any(|record| record.remote_user == user)
        {
            return Err(SteamTransportError::DuplicateRemoteUser);
        }
        if let Some(retry) = self
            .pending_control_retries
            .iter()
            .find(|retry| retry.user == user)
        {
            return if retry.closed_connection == closed_connection {
                Ok(())
            } else {
                Err(SteamTransportError::InvalidState)
            };
        }
        if self.pending_control_retries.len() >= MAX_STEAM_TRANSPORT_CONNECTIONS {
            return Err(SteamTransportError::CapacityExceeded);
        }
        let retry_at_ms = deadline(now_ms, CONTROL_RETRY_DELAY_MS)?;
        let deadline_ms = deadline(now_ms, self.config.connect_timeout_ms)?;
        self.pending_control_retries.push(PendingControlRetry {
            user,
            closed_connection,
            retry_at_ms,
            deadline_ms,
            reason: SteamTransportCloseReason::ConnectTimedOut,
            initiate: self.session.role == SteamTransportRole::Client,
            automatic_retry_used: false,
        });
        self.push_event(SteamTransportEvent::ControlRetrying {
            connection: closed_connection,
            lobby: self.session.lobby,
            user,
            retry_at_ms,
        })
    }

    /// Peer-scoped fail-closed variant used when authentication or AFCP
    /// semantics prove the attributed link hostile. Keeping the reason here
    /// makes the completed pre-game trace persistable instead of disguising
    /// isolation as a user-requested close.
    pub fn close_connections_for_user_with_reason(
        &mut self,
        user: SteamUserId,
        reason: SteamTransportCloseReason,
    ) -> Result<u8, SteamTransportError> {
        self.require_operational()?;
        let mut connections = [None; MAX_STEAM_TRANSPORT_CONNECTIONS];
        let mut count = 0_usize;
        for record in &self.connections {
            if record.remote_user == user {
                connections[count] = Some(record.id);
                count += 1;
            }
        }
        for connection in connections[..count].iter().flatten().copied() {
            self.close_connection_internal(connection, reason, true)?;
        }
        Ok(count as u8)
    }

    pub fn pump(&mut self, now_ms: u64) -> Result<(), SteamTransportError> {
        self.require_operational()?;
        self.advance_time(now_ms)?;
        self.metrics.pumps = self.metrics.pumps.saturating_add(1);

        let relay_status = match self.backend.relay_status() {
            Ok(status) => status,
            Err(error) => return self.fail_closed(error),
        };
        self.authentication_status = match self.backend.authentication_status() {
            Ok(status) => status,
            Err(error) => return self.fail_closed(error),
        };
        if relay_status != self.relay_status {
            self.relay_status = relay_status;
            if let Err(error) =
                self.push_event(SteamTransportEvent::RelayStatusChanged(relay_status))
            {
                return self.fail_closed(error);
            }
        }
        if self.pregame_deadline_pause_started_at_ms.is_none()
            && let Err(error) = self.process_control_retries(now_ms)
        {
            return self.fail_closed(error);
        }

        let backend_events = match self.backend.poll_events(self.config.max_callbacks_per_pump) {
            Ok(events) => events,
            Err(error) => return self.fail_closed(error),
        };
        for event in backend_events {
            if let Err(error) = self.handle_backend_event(event, now_ms) {
                return self.fail_closed(error);
            }
        }

        let expired: Vec<_> = if self.pregame_deadline_pause_started_at_ms.is_some() {
            Vec::new()
        } else {
            self.connections
                .iter()
                .filter_map(|record| {
                    if now_ms < record.deadline_ms {
                        return None;
                    }
                    match record.state {
                        SteamTransportConnectionState::PendingAdmission => {
                            Some((record.id, SteamTransportCloseReason::AdmissionTimedOut))
                        }
                        SteamTransportConnectionState::Accepting
                        | SteamTransportConnectionState::Connecting => {
                            Some((record.id, SteamTransportCloseReason::ConnectTimedOut))
                        }
                        SteamTransportConnectionState::Connected
                            if record.setup_phase < SteamPeerSetupPhase::Secure =>
                        {
                            Some((record.id, SteamTransportCloseReason::ConnectTimedOut))
                        }
                        SteamTransportConnectionState::Connected => None,
                    }
                })
                .collect()
        };
        for (connection, reason) in expired {
            let result = self.try_schedule_control_retry(connection, reason, 0, now_ms);
            if let Err(error) = result.and_then(|scheduled| {
                if scheduled {
                    Ok(())
                } else {
                    self.close_connection_internal(connection, reason, true)
                }
            }) {
                return self.fail_closed(error);
            }
        }

        let polled: Vec<_> = self
            .connections
            .iter()
            .filter(|record| {
                matches!(
                    record.state,
                    SteamTransportConnectionState::Connecting
                        | SteamTransportConnectionState::Connected
                )
            })
            .map(|record| record.id)
            .collect();
        for connection in polled {
            let state = match self.backend.state(connection) {
                Ok(state) => state,
                Err(error) => return self.fail_closed(error),
            };
            match state {
                BackendConnectionState::Connected
                    if self.connection_state(connection)
                        == Some(SteamTransportConnectionState::Connecting) =>
                {
                    if let Err(error) = self.mark_connected(connection) {
                        return self.fail_closed(error);
                    }
                }
                BackendConnectionState::ClosedByPeer => {
                    let native_end_reason = self
                        .backend
                        .native_end_reason(connection)
                        .unwrap_or_default();
                    let result = self.try_schedule_control_retry(
                        connection,
                        SteamTransportCloseReason::RemoteClosed,
                        native_end_reason,
                        now_ms,
                    );
                    if let Err(error) = result.and_then(|scheduled| {
                        if scheduled {
                            Ok(())
                        } else {
                            self.close_connection_internal_with_native_reason(
                                connection,
                                SteamTransportCloseReason::RemoteClosed,
                                true,
                                native_end_reason,
                            )
                        }
                    }) {
                        return self.fail_closed(error);
                    }
                }
                BackendConnectionState::ProblemDetectedLocally => {
                    let native_end_reason = self
                        .backend
                        .native_end_reason(connection)
                        .unwrap_or_default();
                    let result = self.try_schedule_control_retry(
                        connection,
                        SteamTransportCloseReason::LocalProblem,
                        native_end_reason,
                        now_ms,
                    );
                    if let Err(error) = result.and_then(|scheduled| {
                        if scheduled {
                            Ok(())
                        } else {
                            self.close_connection_internal_with_native_reason(
                                connection,
                                SteamTransportCloseReason::LocalProblem,
                                true,
                                native_end_reason,
                            )
                        }
                    }) {
                        return self.fail_closed(error);
                    }
                }
                BackendConnectionState::Connecting | BackendConnectionState::Connected => {}
            }
        }

        let mut closures = Vec::with_capacity(MAX_STEAM_TRANSPORT_CONNECTIONS);
        let mut io_error = None;
        {
            let backend = self.backend.as_mut();
            let metrics = &mut self.metrics;
            let mut remaining_control_frames = MAX_CONTROL_FRAMES_PER_PUMP;
            for record in &mut self.connections {
                if record.state != SteamTransportConnectionState::Connected {
                    continue;
                }
                let Some(io) = record.io.as_mut() else {
                    return self.fail_closed(SteamTransportError::BackendIntegrityFailure);
                };
                match pump_connection_io(
                    backend,
                    record.id,
                    record.remote_user,
                    self.local_user,
                    self.session.lobby,
                    record.setup_phase,
                    io,
                    self.config,
                    metrics,
                    now_ms,
                    &mut remaining_control_frames,
                ) {
                    Ok(Some(reason)) => closures.push((record.id, reason)),
                    Ok(None) => {}
                    Err(error) => {
                        io_error = Some(error);
                        break;
                    }
                }
            }
        }
        if let Some(error) = io_error {
            return self.fail_closed(error);
        }
        for (connection, reason) in closures {
            let result = self.try_schedule_control_retry(connection, reason, 0, now_ms);
            if let Err(error) = result.and_then(|scheduled| {
                if scheduled {
                    Ok(())
                } else {
                    self.close_connection_internal(connection, reason, true)
                }
            }) {
                return self.fail_closed(error);
            }
        }
        Ok(())
    }

    fn handle_backend_event(
        &mut self,
        event: BackendEvent,
        now_ms: u64,
    ) -> Result<(), SteamTransportError> {
        match event {
            BackendEvent::Incoming { connection, user } => {
                let same_user: Vec<_> = self
                    .connections
                    .iter()
                    .filter(|record| record.remote_user == user)
                    .map(|record| record.id)
                    .collect();
                let replacement_for = if same_user.is_empty() {
                    None
                } else if same_user.len() == 1
                    && self
                        .connections
                        .iter()
                        .find(|record| record.id == same_user[0])
                        .is_some_and(|record| record.replacement_eligible)
                {
                    Some(same_user[0])
                } else {
                    None
                };
                let rejection = if self.session.role != SteamTransportRole::ListenAuthority
                    || !self.listening
                {
                    Some(SteamIncomingRejection::WrongRole)
                } else if !self.allowed_incoming_users.contains(&Some(user)) {
                    Some(SteamIncomingRejection::NotInAdmissionRoster)
                } else if self.connections.len() >= MAX_STEAM_TRANSPORT_CONNECTIONS {
                    Some(SteamIncomingRejection::Capacity)
                } else if !same_user.is_empty() && replacement_for.is_none() {
                    Some(SteamIncomingRejection::DuplicateUser)
                } else if self
                    .connections
                    .iter()
                    .any(|record| record.id == connection)
                {
                    Some(SteamIncomingRejection::UnexpectedConnection)
                } else {
                    None
                };
                if let Some(reason) = rejection {
                    self.backend
                        .reject(connection, SteamTransportCloseReason::AdmissionRejected);
                    self.metrics.rejected_connections =
                        self.metrics.rejected_connections.saturating_add(1);
                    let _ = (user, reason);
                    return Ok(());
                }
                let pending_retry = self
                    .pending_control_retries
                    .iter()
                    .position(|retry| !retry.initiate && retry.user == user)
                    .map(|index| self.pending_control_retries.remove(index));
                let expires_at_ms = pending_retry.map_or_else(
                    || deadline(now_ms, self.config.pending_admission_timeout_ms),
                    |retry| Ok(retry.deadline_ms),
                )?;
                if let Some(old) = replacement_for
                    && let Some(record) =
                        self.connections.iter_mut().find(|record| record.id == old)
                {
                    record.replacement_eligible = false;
                }
                self.connections.push(ConnectionRecord {
                    id: connection,
                    remote_user: user,
                    replacement_for,
                    replacement_eligible: false,
                    state: SteamTransportConnectionState::PendingAdmission,
                    deadline_ms: expires_at_ms,
                    deadline_started_at_ms: now_ms,
                    admission: None,
                    endpoint: None,
                    io: None,
                    setup_phase: SteamPeerSetupPhase::Connecting,
                    trace_started_at_ms: now_ms,
                    trace: VecDeque::with_capacity(STEAM_PEER_TRACE_EVENT_CAPACITY),
                    next_trace_ordinal: 1,
                    automatic_retry_used: pending_retry
                        .is_some_and(|retry| retry.automatic_retry_used),
                });
                self.record_peer_trace(
                    connection,
                    if pending_retry.is_some() {
                        STEAM_TRACE_CONNECTION_RETRY_STARTED
                    } else {
                        STEAM_TRACE_CONNECTION_STARTED
                    },
                    0,
                );
                self.push_event(SteamTransportEvent::IncomingPending {
                    connection,
                    lobby: self.session.lobby,
                    user,
                    expires_at_ms,
                })
            }
            BackendEvent::Connected { connection, user } => {
                let Some(record) = self
                    .connections
                    .iter()
                    .find(|record| record.id == connection)
                else {
                    self.backend
                        .close(connection, SteamTransportCloseReason::TransportFault);
                    return Ok(());
                };
                if record.remote_user != user
                    || !matches!(
                        record.state,
                        SteamTransportConnectionState::Accepting
                            | SteamTransportConnectionState::Connecting
                    )
                {
                    return Err(SteamTransportError::BackendIntegrityFailure);
                }
                self.mark_connected(connection)
            }
            BackendEvent::Closed {
                connection,
                user,
                local_problem,
                native_end_reason,
            } => {
                let Some(record) = self
                    .connections
                    .iter()
                    .find(|record| record.id == connection)
                else {
                    return Ok(());
                };
                if record.remote_user != user {
                    return Err(SteamTransportError::BackendIntegrityFailure);
                }
                let reason = if local_problem {
                    SteamTransportCloseReason::LocalProblem
                } else {
                    SteamTransportCloseReason::RemoteClosed
                };
                if self.try_schedule_control_retry(connection, reason, native_end_reason, now_ms)? {
                    Ok(())
                } else {
                    self.close_connection_internal_with_native_reason(
                        connection,
                        reason,
                        true,
                        native_end_reason,
                    )
                }
            }
            BackendEvent::IncomingRejected { user, reason } => {
                self.metrics.rejected_connections =
                    self.metrics.rejected_connections.saturating_add(1);
                let _ = (user, reason);
                Ok(())
            }
            BackendEvent::IncomingPressure { rejected } => {
                self.metrics.rejected_connections = self
                    .metrics
                    .rejected_connections
                    .saturating_add(u64::from(rejected));
                Ok(())
            }
        }
    }

    fn mark_connected(&mut self, connection: SteamConnectionId) -> Result<(), SteamTransportError> {
        let (remote_user, legacy_admitted) = {
            let Some(record) = self
                .connections
                .iter_mut()
                .find(|record| record.id == connection)
            else {
                return Err(SteamTransportError::UnknownConnection);
            };
            if record.state == SteamTransportConnectionState::Connected {
                return Ok(());
            }
            if !matches!(
                record.state,
                SteamTransportConnectionState::Accepting
                    | SteamTransportConnectionState::Connecting
            ) {
                return Err(SteamTransportError::BackendIntegrityFailure);
            }
            let (to_transport, from_endpoint) = sync_channel(self.config.endpoint_queue_packets);
            let (to_endpoint, from_transport) = sync_channel(self.config.endpoint_queue_packets);
            let shared = Arc::new(EndpointShared::new(self.config.endpoint_queue_packets));
            record.endpoint = Some(SteamDatagramEndpoint {
                outbound: to_transport,
                inbound: from_transport,
                shared: shared.clone(),
            });
            record.io = Some(ConnectionIo {
                outbound: from_endpoint,
                inbound: to_endpoint,
                shared,
                pending_send: None,
                endpoint_drop_drain: None,
                control: ControlIo::new(connection.get()),
            });
            record.state = SteamTransportConnectionState::Connected;
            record.deadline_ms = deadline(self.last_now_ms, self.config.connect_timeout_ms)?;
            record.deadline_started_at_ms = self.last_now_ms;
            let legacy_admitted = record.admission.is_some();
            record.setup_phase = if legacy_admitted {
                SteamPeerSetupPhase::GameplayReady
            } else {
                SteamPeerSetupPhase::ControlReady
            };
            (record.remote_user, legacy_admitted)
        };
        self.record_peer_trace(connection, STEAM_TRACE_CONNECTION_READY, 0);
        if legacy_admitted {
            self.metrics.connected_endpoints = self.metrics.connected_endpoints.saturating_add(1);
            self.push_event(SteamTransportEvent::ConnectionReady {
                connection,
                lobby: self.session.lobby,
                user: remote_user,
            })
        } else {
            let identity =
                SteamControlIdentity::new(self.session.lobby, self.local_user, remote_user)
                    .map_err(SteamTransportError::ControlCodec)?;
            self.queue_control(
                connection,
                SteamControlMessage::LinkHello {
                    identity,
                    lobby_schema: STEAM_LOBBY_SCHEMA_VERSION,
                },
            )?;
            self.push_event(SteamTransportEvent::ControlReady {
                connection,
                lobby: self.session.lobby,
                user: remote_user,
                generation: connection.get(),
            })
        }
    }

    fn transition_setup(
        &mut self,
        connection: SteamConnectionId,
        next: SteamPeerSetupPhase,
    ) -> Result<(), SteamTransportError> {
        self.require_operational()?;
        let (user, current) = self
            .connections
            .iter()
            .find(|record| record.id == connection)
            .map(|record| (record.remote_user, record.setup_phase))
            .ok_or(SteamTransportError::UnknownConnection)?;
        if current == next {
            return Ok(());
        }
        if !current.can_transition_to(next) {
            return Err(SteamTransportError::IllegalSetupTransition);
        }
        self.connections
            .iter_mut()
            .find(|record| record.id == connection)
            .expect("connection was just located")
            .setup_phase = next;
        self.record_peer_trace(
            connection,
            STEAM_TRACE_SETUP_PHASE_BASE + u16::from(next.diagnostic_code()),
            0,
        );
        self.push_event(SteamTransportEvent::SetupPhaseChanged {
            connection,
            lobby: self.session.lobby,
            user,
            phase: next,
        })
    }

    fn record_peer_trace(
        &mut self,
        connection: SteamConnectionId,
        result_code: u16,
        native_end_reason: i32,
    ) {
        let relay_availability = self.relay_status.availability;
        let authentication_availability = self.authentication_status;
        let now_ms = self.last_now_ms;
        let Some(record) = self
            .connections
            .iter_mut()
            .find(|record| record.id == connection)
        else {
            return;
        };
        if record.trace.len() == STEAM_PEER_TRACE_EVENT_CAPACITY {
            record.trace.pop_front();
        }
        record.trace.push_back(SteamPeerTraceEvent {
            ordinal: record.next_trace_ordinal,
            connection_generation: record.id.get(),
            phase: record.setup_phase,
            elapsed_ms: now_ms.saturating_sub(record.trace_started_at_ms),
            relay_availability,
            authentication_availability,
            result_code,
            native_end_reason,
        });
        record.next_trace_ordinal = record.next_trace_ordinal.saturating_add(1);
    }

    fn try_schedule_control_retry(
        &mut self,
        connection: SteamConnectionId,
        reason: SteamTransportCloseReason,
        native_end_reason: i32,
        now_ms: u64,
    ) -> Result<bool, SteamTransportError> {
        if !matches!(
            reason,
            SteamTransportCloseReason::ConnectTimedOut
                | SteamTransportCloseReason::RemoteClosed
                | SteamTransportCloseReason::LocalProblem
                | SteamTransportCloseReason::BackendFailure
        ) {
            return Ok(false);
        }
        let Some(record) = self
            .connections
            .iter()
            .find(|record| record.id == connection)
        else {
            return Ok(false);
        };
        if record.setup_phase >= SteamPeerSetupPhase::GameplayReceiveArmed
            || record.automatic_retry_used
            || self
                .pending_control_retries
                .iter()
                .any(|retry| retry.user == record.remote_user)
        {
            return Ok(false);
        }
        let retry_deadline_ms =
            deadline(record.trace_started_at_ms, self.config.connect_timeout_ms)?;
        let retry_clock_ms = self.pregame_deadline_pause_started_at_ms.unwrap_or(now_ms);
        if retry_deadline_ms.saturating_sub(retry_clock_ms) < CONTROL_RETRY_MINIMUM_REMAINING_MS {
            return Ok(false);
        }
        let retry_at_ms = deadline(retry_clock_ms, CONTROL_RETRY_DELAY_MS)?;
        let user = record.remote_user;
        self.close_connection_internal_with_native_reason(
            connection,
            reason,
            false,
            native_end_reason,
        )?;
        if self.pending_control_retries.len() >= MAX_STEAM_TRANSPORT_CONNECTIONS {
            return Err(SteamTransportError::CapacityExceeded);
        }
        self.pending_control_retries.push(PendingControlRetry {
            user,
            closed_connection: connection,
            retry_at_ms,
            deadline_ms: retry_deadline_ms,
            reason,
            initiate: self.session.role == SteamTransportRole::Client,
            automatic_retry_used: true,
        });
        self.push_event(SteamTransportEvent::ControlRetrying {
            connection,
            lobby: self.session.lobby,
            user,
            retry_at_ms,
        })?;
        Ok(true)
    }

    fn process_control_retries(&mut self, now_ms: u64) -> Result<(), SteamTransportError> {
        let mut index = 0;
        while index < self.pending_control_retries.len() {
            let retry = self.pending_control_retries[index];
            if now_ms >= retry.deadline_ms {
                self.pending_control_retries.remove(index);
                self.push_event(SteamTransportEvent::ConnectionClosed {
                    connection: retry.closed_connection,
                    lobby: self.session.lobby,
                    user: retry.user,
                    reason: retry.reason,
                })?;
                continue;
            }
            if !retry.initiate || now_ms < retry.retry_at_ms {
                index += 1;
                continue;
            }
            self.pending_control_retries.remove(index);
            if self.connections.len() >= MAX_STEAM_TRANSPORT_CONNECTIONS {
                return Err(SteamTransportError::CapacityExceeded);
            }
            let connection = self
                .backend
                .connect_p2p(retry.user, self.session.virtual_port)?;
            if self
                .connections
                .iter()
                .any(|record| record.id == connection)
            {
                self.backend
                    .close(connection, SteamTransportCloseReason::TransportFault);
                return Err(SteamTransportError::BackendIntegrityFailure);
            }
            self.connections.push(ConnectionRecord {
                id: connection,
                remote_user: retry.user,
                replacement_for: None,
                replacement_eligible: false,
                state: SteamTransportConnectionState::Connecting,
                deadline_ms: retry.deadline_ms,
                deadline_started_at_ms: now_ms,
                admission: None,
                endpoint: None,
                io: None,
                setup_phase: SteamPeerSetupPhase::Connecting,
                trace_started_at_ms: now_ms,
                trace: VecDeque::with_capacity(STEAM_PEER_TRACE_EVENT_CAPACITY),
                next_trace_ordinal: 1,
                automatic_retry_used: retry.automatic_retry_used,
            });
            self.record_peer_trace(connection, STEAM_TRACE_CONNECTION_RETRY_STARTED, 0);
        }
        Ok(())
    }

    fn close_connection_internal(
        &mut self,
        connection: SteamConnectionId,
        reason: SteamTransportCloseReason,
        emit: bool,
    ) -> Result<(), SteamTransportError> {
        self.close_connection_internal_with_native_reason(connection, reason, emit, 0)
    }

    fn close_connection_internal_with_native_reason(
        &mut self,
        connection: SteamConnectionId,
        reason: SteamTransportCloseReason,
        emit: bool,
        native_end_reason: i32,
    ) -> Result<(), SteamTransportError> {
        self.record_peer_trace(connection, reason.diagnostic_code(), native_end_reason);
        self.finalize_connection_record(connection, reason, emit)
    }

    /// Retires one already-traced connection while preserving its bounded
    /// privacy-safe setup history for the coordinator. Fault paths call this
    /// after recording the exact transport error, rather than losing every
    /// peer trace by draining the record collection directly.
    fn finalize_connection_record(
        &mut self,
        connection: SteamConnectionId,
        reason: SteamTransportCloseReason,
        emit: bool,
    ) -> Result<(), SteamTransportError> {
        let Some(index) = self
            .connections
            .iter()
            .position(|record| record.id == connection)
        else {
            return Err(SteamTransportError::UnknownConnection);
        };
        let record = self.connections.swap_remove(index);
        if self.completed_peer_traces.len() == RETAINED_STEAM_PEER_TRACE_CAPACITY {
            self.completed_peer_traces.pop_front();
        }
        self.completed_peer_traces.push_back(SteamPeerTrace {
            events: record.trace.iter().copied().collect(),
        });
        if let Some(io) = &record.io {
            io.shared.connected.store(false, Ordering::Release);
            io.shared.outbound_depth.store(0, Ordering::Release);
            io.shared.inbound_depth.store(0, Ordering::Release);
        }
        self.backend.close(connection, reason);
        self.metrics.closed_connections = self.metrics.closed_connections.saturating_add(1);
        if emit {
            self.push_event(SteamTransportEvent::ConnectionClosed {
                connection,
                lobby: self.session.lobby,
                user: record.remote_user,
                reason,
            })?;
        }
        Ok(())
    }

    fn validate_admission(
        &self,
        admission: AuthenticatedSteamPeer,
        expected_user: SteamUserId,
    ) -> Result<(), SteamTransportError> {
        if admission.lobby != self.session.lobby {
            return Err(SteamTransportError::AdmissionLobbyMismatch);
        }
        if admission.user != expected_user {
            return Err(SteamTransportError::AdmissionUserMismatch);
        }
        if admission.authenticated_user.get() != admission.user.get() {
            return Err(SteamTransportError::AdmissionIdentityMismatch);
        }
        match self.session.role {
            SteamTransportRole::ListenAuthority if admission.user == self.local_user => {
                Err(SteamTransportError::AdmissionUserMismatch)
            }
            SteamTransportRole::Client if admission.user != self.session.authority_user => {
                Err(SteamTransportError::AdmissionAuthorityMismatch)
            }
            _ => Ok(()),
        }
    }

    fn advance_time(&mut self, now_ms: u64) -> Result<(), SteamTransportError> {
        if now_ms < self.last_now_ms {
            return self.fail_closed(SteamTransportError::TimeRegression);
        }
        self.last_now_ms = now_ms;
        Ok(())
    }

    fn require_operational(&self) -> Result<(), SteamTransportError> {
        if self.last_fault.is_some() {
            Err(SteamTransportError::Faulted)
        } else if self.retirement.is_some() {
            Err(SteamTransportError::InvalidState)
        } else {
            Ok(())
        }
    }

    fn push_event(&mut self, event: SteamTransportEvent) -> Result<(), SteamTransportError> {
        if self.events.len() >= self.config.event_capacity {
            return Err(SteamTransportError::EventQueueOverflow);
        }
        self.events.push_back(event);
        self.metrics.event_high_water = self.metrics.event_high_water.max(self.events.len());
        Ok(())
    }

    fn fail_closed<T>(&mut self, error: SteamTransportError) -> Result<T, SteamTransportError> {
        if self.last_fault.is_none() {
            self.last_fault = Some(error);
        }
        self.backend.close_p2p_listener();
        self.listening = false;
        let connections: Vec<_> = self.connections.iter().map(|record| record.id).collect();
        for connection in connections {
            self.record_peer_trace(connection, error.diagnostic_code(), 0);
            if let Some(io) = self
                .connections
                .iter()
                .find(|record| record.id == connection)
                .and_then(|record| record.io.as_ref())
            {
                io.shared.receive_enabled.store(false, Ordering::Release);
            }
            let result = self.finalize_connection_record(
                connection,
                SteamTransportCloseReason::TransportFault,
                false,
            );
            debug_assert!(result.is_ok(), "collected connection must still exist");
        }
        self.events.clear();
        Err(error)
    }

    fn begin_faulted_retirement(
        &mut self,
        error: SteamTransportError,
    ) -> SteamTransportRetirementStatus {
        if let Some(status) = self.retirement_status() {
            return status;
        }
        self.retirement = Some(TransportRetirement {
            hard_deadline_ms: self.last_now_ms,
            status: SteamTransportRetirementStatus::Draining,
        });
        self.finish_retirement(SteamTransportRetirementStatus::Faulted(error))
    }

    fn finish_retirement(
        &mut self,
        status: SteamTransportRetirementStatus,
    ) -> SteamTransportRetirementStatus {
        if let Some(existing) = self.retirement_status()
            && existing.is_terminal()
        {
            return existing;
        }
        debug_assert!(status.is_terminal());
        if let SteamTransportRetirementStatus::Faulted(error) = status {
            if self.last_fault.is_none() {
                self.last_fault = Some(error);
            }
        }
        self.backend.close_p2p_listener();
        self.listening = false;
        self.allowed_incoming_users = [None; MAX_STEAM_TRANSPORT_CONNECTIONS];
        let connections: Vec<_> = self.connections.iter().map(|record| record.id).collect();
        for connection in connections {
            let (result_code, reason) = match status {
                SteamTransportRetirementStatus::Faulted(error) => (
                    error.diagnostic_code(),
                    SteamTransportCloseReason::TransportFault,
                ),
                SteamTransportRetirementStatus::Complete
                | SteamTransportRetirementStatus::TimedOut => (
                    SteamTransportCloseReason::Requested.diagnostic_code(),
                    SteamTransportCloseReason::Requested,
                ),
                SteamTransportRetirementStatus::Draining => unreachable!(),
            };
            self.record_peer_trace(connection, result_code, 0);
            if let Some(io) = self
                .connections
                .iter()
                .find(|record| record.id == connection)
                .and_then(|record| record.io.as_ref())
            {
                io.shared.receive_enabled.store(false, Ordering::Release);
            }
            let result = self.finalize_connection_record(connection, reason, false);
            debug_assert!(result.is_ok(), "collected connection must still exist");
        }
        self.events.clear();
        match status {
            SteamTransportRetirementStatus::Complete => {
                self.metrics.retirements_completed =
                    self.metrics.retirements_completed.saturating_add(1);
            }
            SteamTransportRetirementStatus::TimedOut => {
                self.metrics.retirement_timeouts =
                    self.metrics.retirement_timeouts.saturating_add(1);
            }
            SteamTransportRetirementStatus::Faulted(_) => {
                self.metrics.retirement_faults = self.metrics.retirement_faults.saturating_add(1);
            }
            SteamTransportRetirementStatus::Draining => unreachable!(),
        }
        if let Some(retirement) = self.retirement.as_mut() {
            retirement.status = status;
        }
        status
    }
}

impl Drop for SteamTransport {
    fn drop(&mut self) {
        self.backend.close_p2p_listener();
        for record in self.connections.drain(..) {
            if let Some(io) = record.io {
                io.shared.connected.store(false, Ordering::Release);
                io.shared.receive_enabled.store(false, Ordering::Release);
            }
            self.backend
                .close(record.id, SteamTransportCloseReason::Requested);
        }
    }
}

fn deadline(now_ms: u64, duration_ms: u64) -> Result<u64, SteamTransportError> {
    now_ms
        .checked_add(duration_ms)
        .ok_or(SteamTransportError::InvalidConfiguration)
}

fn pump_connection_io(
    backend: &mut dyn SteamTransportBackend,
    connection: SteamConnectionId,
    remote_user: SteamUserId,
    local_user: SteamUserId,
    lobby: SteamLobbyId,
    setup_phase: SteamPeerSetupPhase,
    io: &mut ConnectionIo,
    config: SteamTransportConfig,
    metrics: &mut SteamTransportMetrics,
    now_ms: u64,
    remaining_control_frames: &mut usize,
) -> Result<Option<SteamTransportCloseReason>, SteamTransportError> {
    if let Some(reason) = pump_control_outbound(backend, connection, io, config, metrics, now_ms)? {
        return Ok(Some(reason));
    }

    let gameplay_ready = setup_phase == SteamPeerSetupPhase::GameplayReady;
    let gameplay_receive_armed = setup_phase >= SteamPeerSetupPhase::GameplayReceiveArmed;
    if gameplay_ready && !io.shared.endpoint_alive.load(Ordering::Acquire) {
        begin_endpoint_drop_drain(io, now_ms, metrics)?;
    }
    if let Some(reason) = endpoint_drop_drain_completion(io, now_ms, metrics) {
        return Ok(Some(reason));
    }

    for _ in 0..if gameplay_ready {
        config.max_send_datagrams_per_connection_per_pump
    } else {
        0
    } {
        if io.pending_send.is_none() {
            match io.outbound.try_recv() {
                Ok(datagram) => io.pending_send = Some(datagram),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    begin_endpoint_drop_drain(io, now_ms, metrics)?;
                    break;
                }
            }
        }
        let Some(datagram) = io.pending_send.as_ref() else {
            break;
        };
        match backend.send(connection, datagram, BackendSendMode::Gameplay) {
            Ok(BackendSendOutcome::Sent) => {
                metrics.sent_datagrams = metrics.sent_datagrams.saturating_add(1);
                metrics.sent_bytes = metrics.sent_bytes.saturating_add(datagram.len() as u64);
                io.pending_send = None;
                release_depth(&io.shared.outbound_depth);
                if let Some(drain) = io.endpoint_drop_drain.as_mut() {
                    drain.quiet_since_ms = None;
                }
            }
            Ok(BackendSendOutcome::WouldBlock) => {
                metrics.send_would_block = metrics.send_would_block.saturating_add(1);
                break;
            }
            Ok(BackendSendOutcome::Disconnected) => {
                return Ok(Some(SteamTransportCloseReason::RemoteClosed));
            }
            Err(_) => return Ok(Some(SteamTransportCloseReason::BackendFailure)),
        }
    }

    if gameplay_ready && !io.shared.endpoint_alive.load(Ordering::Acquire) {
        begin_endpoint_drop_drain(io, now_ms, metrics)?;
    }
    if io.endpoint_drop_drain.is_some() {
        return Ok(endpoint_drop_drain_completion(io, now_ms, metrics));
    }

    let mut received_control_frames = 0_usize;
    for _ in 0..config.max_receive_datagrams_per_connection_per_pump {
        if *remaining_control_frames == 0
            || received_control_frames >= MAX_CONTROL_FRAMES_PER_CONNECTION_PER_PUMP
        {
            break;
        }
        if gameplay_ready && !io.shared.endpoint_alive.load(Ordering::Acquire) {
            begin_endpoint_drop_drain(io, now_ms, metrics)?;
            return Ok(endpoint_drop_drain_completion(io, now_ms, metrics));
        }
        match backend.receive(connection) {
            Ok(BackendReceiveOutcome::Datagram(datagram)) => {
                if datagram.as_slice().starts_with(&STEAM_CONTROL_MAGIC) {
                    *remaining_control_frames = (*remaining_control_frames).saturating_sub(1);
                    received_control_frames += 1;
                    if let Err(error) = handle_control_datagram(
                        connection,
                        remote_user,
                        local_user,
                        lobby,
                        io,
                        datagram,
                        metrics,
                    ) {
                        metrics.malformed_control_frames =
                            metrics.malformed_control_frames.saturating_add(1);
                        return Ok(Some(match error {
                            SteamTransportError::ControlQueueOverflow => {
                                SteamTransportCloseReason::InboundQueueOverflow
                            }
                            _ => SteamTransportCloseReason::MalformedControlTraffic,
                        }));
                    }
                    continue;
                }
                if !gameplay_receive_armed {
                    metrics.malformed_control_frames =
                        metrics.malformed_control_frames.saturating_add(1);
                    return Ok(Some(SteamTransportCloseReason::MalformedControlTraffic));
                }
                let len = datagram.len();
                let Some(depth) = reserve_depth(&io.shared.inbound_depth, io.shared.capacity)
                else {
                    metrics.inbound_queue_overflows =
                        metrics.inbound_queue_overflows.saturating_add(1);
                    return Ok(Some(SteamTransportCloseReason::InboundQueueOverflow));
                };
                match io.inbound.try_send(datagram) {
                    Ok(()) => {
                        update_high_water(&io.shared.inbound_high_water, depth);
                        metrics.received_datagrams = metrics.received_datagrams.saturating_add(1);
                        metrics.received_bytes = metrics.received_bytes.saturating_add(len as u64);
                    }
                    Err(TrySendError::Full(_)) => {
                        release_depth(&io.shared.inbound_depth);
                        metrics.inbound_queue_overflows =
                            metrics.inbound_queue_overflows.saturating_add(1);
                        return Ok(Some(SteamTransportCloseReason::InboundQueueOverflow));
                    }
                    Err(TrySendError::Disconnected(_)) => {
                        release_depth(&io.shared.inbound_depth);
                        begin_endpoint_drop_drain(io, now_ms, metrics)?;
                        return Ok(endpoint_drop_drain_completion(io, now_ms, metrics));
                    }
                }
            }
            Ok(BackendReceiveOutcome::Empty) => break,
            Ok(BackendReceiveOutcome::Disconnected) => {
                return Ok(Some(SteamTransportCloseReason::RemoteClosed));
            }
            Ok(BackendReceiveOutcome::Oversized(_)) => {
                metrics.oversized_datagrams = metrics.oversized_datagrams.saturating_add(1);
                return Ok(Some(SteamTransportCloseReason::OversizedDatagram));
            }
            Err(_) => return Ok(Some(SteamTransportCloseReason::BackendFailure)),
        }
    }
    if gameplay_ready && !io.shared.endpoint_alive.load(Ordering::Acquire) {
        begin_endpoint_drop_drain(io, now_ms, metrics)?;
        return Ok(endpoint_drop_drain_completion(io, now_ms, metrics));
    }
    Ok(None)
}

fn pump_control_outbound(
    backend: &mut dyn SteamTransportBackend,
    connection: SteamConnectionId,
    io: &mut ConnectionIo,
    config: SteamTransportConfig,
    metrics: &mut SteamTransportMetrics,
    now_ms: u64,
) -> Result<Option<SteamTransportCloseReason>, SteamTransportError> {
    let mut budget = config.max_send_datagrams_per_connection_per_pump;
    if !io.control.hello_transmitted && !io.control.outstanding.is_empty() {
        let Some(hello) = io
            .control
            .outstanding
            .iter_mut()
            .find(|frame| frame.sequence == 1)
        else {
            return Err(SteamTransportError::BackendIntegrityFailure);
        };
        match backend.send(
            connection,
            hello.datagram.datagram(),
            BackendSendMode::ReliableControl,
        ) {
            Ok(BackendSendOutcome::Sent) => {
                hello.last_sent_at_ms = Some(now_ms);
                io.control.hello_transmitted = true;
                io.control.highest_transmitted_sequence = 1;
                metrics.sent_control_frames = metrics.sent_control_frames.saturating_add(1);
                budget = budget.saturating_sub(1);
            }
            Ok(BackendSendOutcome::WouldBlock) => {
                metrics.send_would_block = metrics.send_would_block.saturating_add(1);
                return Ok(None);
            }
            Ok(BackendSendOutcome::Disconnected) => {
                return Ok(Some(SteamTransportCloseReason::RemoteClosed));
            }
            Err(_) => return Ok(Some(SteamTransportCloseReason::BackendFailure)),
        }
    }
    if let Some(acknowledgement) = io.control.acknowledgement.as_ref() {
        if !io.control.hello_transmitted {
            return Err(SteamTransportError::IllegalSetupTransition);
        }
        if budget == 0 {
            return Ok(None);
        }
        match backend.send(
            connection,
            acknowledgement.datagram(),
            BackendSendMode::ReliableControl,
        ) {
            Ok(BackendSendOutcome::Sent) => {
                metrics.sent_control_frames = metrics.sent_control_frames.saturating_add(1);
                io.control.acknowledgement = None;
                budget = budget.saturating_sub(1);
            }
            Ok(BackendSendOutcome::WouldBlock) => {
                metrics.send_would_block = metrics.send_would_block.saturating_add(1);
                return Ok(None);
            }
            Ok(BackendSendOutcome::Disconnected) => {
                return Ok(Some(SteamTransportCloseReason::RemoteClosed));
            }
            Err(_) => return Ok(Some(SteamTransportCloseReason::BackendFailure)),
        }
    }
    for frame in io.control.outstanding.iter_mut() {
        if budget == 0 {
            break;
        }
        if frame
            .last_sent_at_ms
            .is_some_and(|sent| now_ms.saturating_sub(sent) < DEFAULT_CONTROL_RETRANSMIT_MS)
        {
            continue;
        }
        match backend.send(
            connection,
            frame.datagram.datagram(),
            BackendSendMode::ReliableControl,
        ) {
            Ok(BackendSendOutcome::Sent) => {
                frame.last_sent_at_ms = Some(now_ms);
                io.control.highest_transmitted_sequence =
                    io.control.highest_transmitted_sequence.max(frame.sequence);
                metrics.sent_control_frames = metrics.sent_control_frames.saturating_add(1);
                budget -= 1;
            }
            Ok(BackendSendOutcome::WouldBlock) => {
                metrics.send_would_block = metrics.send_would_block.saturating_add(1);
                break;
            }
            Ok(BackendSendOutcome::Disconnected) => {
                return Ok(Some(SteamTransportCloseReason::RemoteClosed));
            }
            Err(_) => return Ok(Some(SteamTransportCloseReason::BackendFailure)),
        }
    }
    Ok(None)
}

fn handle_control_datagram(
    connection: SteamConnectionId,
    remote_user: SteamUserId,
    local_user: SteamUserId,
    lobby: SteamLobbyId,
    io: &mut ConnectionIo,
    mut datagram: AfcDatagram,
    metrics: &mut SteamTransportMetrics,
) -> Result<(), SteamTransportError> {
    let decoded = decode_steam_control(datagram.as_slice());
    // The raw inbound copy may contain a routed ticket, including on malformed
    // input paths. The decoded ticket owns its own independently zeroized copy.
    datagram.zeroize();
    let envelope = decoded.map_err(SteamTransportError::ControlCodec)?;
    let identity = envelope
        .message
        .as_ref()
        .map(SteamControlMessage::identity)
        .or(envelope.acknowledgement_identity)
        .ok_or(SteamTransportError::IllegalSetupTransition)?;
    if identity.lobby != lobby || identity.sender != remote_user || identity.recipient != local_user
    {
        return Err(SteamTransportError::AdmissionIdentityMismatch);
    }
    match io.control.remote_generation {
        None => {
            if !matches!(
                envelope.message.as_ref(),
                Some(SteamControlMessage::LinkHello {
                    lobby_schema: STEAM_LOBBY_SCHEMA_VERSION,
                    ..
                })
            ) || envelope.sequence != 1
            {
                return Err(SteamTransportError::IllegalSetupTransition);
            }
            io.control.remote_generation = Some(envelope.generation);
        }
        Some(generation) if generation != envelope.generation => {
            return Err(SteamTransportError::IllegalSetupTransition);
        }
        Some(_)
            if matches!(
                envelope.message.as_ref(),
                Some(SteamControlMessage::LinkHello { .. })
            ) && envelope.sequence != 1 =>
        {
            // LinkHello is the generation-binding sequence-1 frame. Accepting
            // a distinct later one would let syntactically valid bootstrap
            // traffic bypass the application phase machine indefinitely. A
            // reliable retransmission of the original sequence remains
            // idempotent until semantic acceptance produces its ACK.
            return Err(SteamTransportError::IllegalSetupTransition);
        }
        Some(_) => {}
    }
    validate_control_ack(
        &io.control,
        envelope.acknowledgement_generation,
        envelope.acknowledgement,
    )?;
    let Some(message) = envelope.message else {
        apply_control_ack(
            &mut io.control,
            envelope.acknowledgement_generation,
            envelope.acknowledgement,
            metrics,
        )?;
        return Ok(());
    };
    if io.control.ingress_rejected {
        return Err(SteamTransportError::IllegalSetupTransition);
    }
    if envelope.sequence <= io.control.highest_accepted_sequence {
        metrics.duplicate_control_frames = metrics.duplicate_control_frames.saturating_add(1);
        queue_control_ack(&mut io.control, identity.reverse())?;
    } else if envelope.sequence <= io.control.highest_received_sequence {
        // The application has not accepted this sequence yet. Reliable
        // retransmission is harmless, but acknowledging it here would recreate
        // the original local-submit-is-delivery bug.
        metrics.duplicate_control_frames = metrics.duplicate_control_frames.saturating_add(1);
    } else {
        let expected = io
            .control
            .highest_received_sequence
            .checked_add(1)
            .ok_or(SteamTransportError::CapacityExceeded)?;
        if envelope.sequence != expected {
            return Err(SteamTransportError::IllegalSetupTransition);
        }
        if io.control.ingress.len() >= MAX_STEAM_TRANSPORT_EVENTS
            || io.control.pending_acceptance.len() >= MAX_STEAM_TRANSPORT_EVENTS
        {
            return Err(SteamTransportError::ControlQueueOverflow);
        }
        io.control.highest_received_sequence = envelope.sequence;
        let token = SteamControlIngressToken {
            connection,
            generation: envelope.generation,
            sequence: envelope.sequence,
        };
        io.control
            .pending_acceptance
            .push_back(PendingControlAcceptance {
                token,
                acknowledgement: envelope.acknowledgement,
                acknowledgement_generation: envelope.acknowledgement_generation,
            });
        io.control.ingress.push_back(SteamControlIngress {
            token,
            connection,
            user: remote_user,
            generation: envelope.generation,
            sequence: envelope.sequence,
            message,
        });
        metrics.received_control_frames = metrics.received_control_frames.saturating_add(1);
    }
    Ok(())
}

fn validate_control_ack(
    control: &ControlIo,
    acknowledgement_generation: u32,
    acknowledgement: u32,
) -> Result<(), SteamTransportError> {
    if (acknowledgement != 0 && acknowledgement_generation != control.local_generation)
        || (acknowledgement == 0 && acknowledgement_generation != 0)
        || acknowledgement > control.highest_transmitted_sequence
    {
        Err(SteamTransportError::IllegalSetupTransition)
    } else {
        Ok(())
    }
}

fn apply_control_ack(
    control: &mut ControlIo,
    acknowledgement_generation: u32,
    acknowledgement: u32,
    metrics: &mut SteamTransportMetrics,
) -> Result<(), SteamTransportError> {
    validate_control_ack(control, acknowledgement_generation, acknowledgement)?;
    if acknowledgement <= control.highest_acknowledged_sequence {
        return Ok(());
    }
    let mut acknowledged = 0_u64;
    while control
        .outstanding
        .front()
        .is_some_and(|frame| frame.sequence <= acknowledgement)
    {
        control.outstanding.pop_front();
        acknowledged = acknowledged.saturating_add(1);
    }
    control.highest_acknowledged_sequence = acknowledgement;
    metrics.acknowledged_control_frames = metrics
        .acknowledged_control_frames
        .saturating_add(acknowledged);
    Ok(())
}

fn queue_control_ack(
    control: &mut ControlIo,
    identity: SteamControlIdentity,
) -> Result<(), SteamTransportError> {
    let acknowledgement = SteamControlEnvelope::acknowledgement(
        control.local_generation,
        control
            .remote_generation
            .ok_or(SteamTransportError::IllegalSetupTransition)?,
        control.highest_accepted_sequence,
        identity,
    )
    .map_err(SteamTransportError::ControlCodec)?;
    control.acknowledgement =
        Some(encode_steam_control(&acknowledgement).map_err(SteamTransportError::ControlCodec)?);
    Ok(())
}

fn begin_endpoint_drop_drain(
    io: &mut ConnectionIo,
    now_ms: u64,
    metrics: &mut SteamTransportMetrics,
) -> Result<(), SteamTransportError> {
    begin_outbound_drain(io, now_ms, OutboundDrainOrigin::EndpointDrop, metrics)
}

fn begin_outbound_drain(
    io: &mut ConnectionIo,
    now_ms: u64,
    origin: OutboundDrainOrigin,
    metrics: &mut SteamTransportMetrics,
) -> Result<(), SteamTransportError> {
    if io.endpoint_drop_drain.is_none() {
        io.endpoint_drop_drain = Some(EndpointDropDrain {
            hard_deadline_ms: deadline(now_ms, ENDPOINT_DROP_DRAIN_HARD_TIMEOUT_MS)?,
            quiet_since_ms: None,
            origin,
        });
        if origin == OutboundDrainOrigin::EndpointDrop {
            metrics.endpoint_drop_drains_started =
                metrics.endpoint_drop_drains_started.saturating_add(1);
        }
    }
    Ok(())
}

fn endpoint_drop_drain_completion(
    io: &mut ConnectionIo,
    now_ms: u64,
    metrics: &mut SteamTransportMetrics,
) -> Option<SteamTransportCloseReason> {
    outbound_drain_completion(io, now_ms, metrics)
        .map(|_| SteamTransportCloseReason::EndpointDropped)
}

fn outbound_drain_completion(
    io: &mut ConnectionIo,
    now_ms: u64,
    metrics: &mut SteamTransportMetrics,
) -> Option<OutboundDrainCompletion> {
    let drain = io.endpoint_drop_drain.as_mut()?;
    if now_ms >= drain.hard_deadline_ms {
        if drain.origin == OutboundDrainOrigin::EndpointDrop {
            metrics.endpoint_drop_drain_timeouts =
                metrics.endpoint_drop_drain_timeouts.saturating_add(1);
        }
        return Some(OutboundDrainCompletion::TimedOut);
    }
    if io.pending_send.is_none() && io.shared.outbound_depth.load(Ordering::Acquire) == 0 {
        let quiet_since = drain.quiet_since_ms.get_or_insert(now_ms);
        if now_ms.saturating_sub(*quiet_since) >= ENDPOINT_DROP_DRAIN_QUIET_MS {
            if drain.origin == OutboundDrainOrigin::EndpointDrop {
                metrics.endpoint_drop_drains_quiet_completed = metrics
                    .endpoint_drop_drains_quiet_completed
                    .saturating_add(1);
            }
            return Some(OutboundDrainCompletion::Quiet);
        }
    } else {
        drain.quiet_since_ms = None;
    }
    None
}

fn pump_retiring_connection_io(
    backend: &mut dyn SteamTransportBackend,
    connection: SteamConnectionId,
    io: &mut ConnectionIo,
    config: SteamTransportConfig,
    metrics: &mut SteamTransportMetrics,
    now_ms: u64,
) -> Result<Option<OutboundDrainCompletion>, SteamTransportError> {
    io.shared.receive_enabled.store(false, Ordering::Release);
    begin_outbound_drain(
        io,
        now_ms,
        OutboundDrainOrigin::TransportRetirement,
        metrics,
    )?;

    // Retirement deliberately services the exact bounded send budget before
    // checking either deadline. A late application frame can therefore still
    // submit an ACK that the endpoint accepted before its worker was cleared.
    for _ in 0..config.max_send_datagrams_per_connection_per_pump {
        if io.pending_send.is_none() {
            match io.outbound.try_recv() {
                Ok(datagram) => io.pending_send = Some(datagram),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        let Some(datagram) = io.pending_send.as_ref() else {
            break;
        };
        match backend.send(connection, datagram, BackendSendMode::Gameplay) {
            Ok(BackendSendOutcome::Sent) => {
                metrics.sent_datagrams = metrics.sent_datagrams.saturating_add(1);
                metrics.sent_bytes = metrics.sent_bytes.saturating_add(datagram.len() as u64);
                io.pending_send = None;
                release_depth(&io.shared.outbound_depth);
                if let Some(drain) = io.endpoint_drop_drain.as_mut() {
                    drain.quiet_since_ms = None;
                }
            }
            Ok(BackendSendOutcome::WouldBlock) => {
                metrics.send_would_block = metrics.send_would_block.saturating_add(1);
                break;
            }
            Ok(BackendSendOutcome::Disconnected) => {
                return Ok(Some(OutboundDrainCompletion::Quiet));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(outbound_drain_completion(io, now_ms, metrics))
}

#[derive(Clone)]
pub struct FakeSteamTransportNetwork {
    shared: Arc<Mutex<FakeNetworkState>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FakeSteamTransportResourceCounts {
    pub backends: usize,
    pub listeners: usize,
    pub inboxes: usize,
    pub links: usize,
}

impl FakeSteamTransportNetwork {
    pub fn new(wire_queue_packets: usize) -> Result<Self, SteamTransportError> {
        if wire_queue_packets == 0 {
            return Err(SteamTransportError::InvalidConfiguration);
        }
        if wire_queue_packets > MAX_STEAM_ENDPOINT_QUEUE_PACKETS {
            return Err(SteamTransportError::CapacityExceeded);
        }
        Ok(Self {
            shared: Arc::new(Mutex::new(FakeNetworkState {
                wire_queue_packets,
                next_connection: 1,
                next_backend: 1,
                backends: BTreeMap::new(),
                listeners: BTreeMap::new(),
                inboxes: BTreeMap::new(),
                allowed_incoming_users: BTreeMap::new(),
                relays: BTreeMap::new(),
                links: BTreeMap::new(),
                send_failures: BTreeMap::new(),
            })),
        })
    }

    pub fn create_transport(
        &self,
        local_user: SteamUserId,
        session: SteamP2pSession,
        config: SteamTransportConfig,
        now_ms: u64,
    ) -> Result<SteamTransport, SteamTransportError> {
        let backend = FakeSteamTransportBackend::register(self.shared.clone(), local_user)?;
        SteamTransport::from_backend(Box::new(backend), session, config, now_ms)
    }

    pub fn resource_counts(&self) -> FakeSteamTransportResourceCounts {
        lock_fake(&self.shared).map_or_else(
            |_| FakeSteamTransportResourceCounts::default(),
            |state| FakeSteamTransportResourceCounts {
                backends: state.backends.len(),
                listeners: state.listeners.len(),
                inboxes: state.inboxes.len(),
                links: state.links.len(),
            },
        )
    }

    pub fn set_relay_status(
        &self,
        user: SteamUserId,
        status: SteamRelayStatus,
    ) -> Result<(), SteamTransportError> {
        let mut state = lock_fake(&self.shared)?;
        let backend_ids: Vec<_> = state
            .backends
            .iter()
            .filter_map(|(backend, candidate)| (*candidate == user).then_some(*backend))
            .collect();
        if backend_ids.is_empty() {
            return Err(SteamTransportError::BackendUnavailable);
        }
        for backend in backend_ids {
            state.relays.insert(backend, status);
        }
        Ok(())
    }

    pub fn set_connection_quality(
        &self,
        connection: SteamConnectionId,
        user: SteamUserId,
        quality: SteamConnectionQuality,
    ) -> Result<(), SteamTransportError> {
        let mut state = lock_fake(&self.shared)?;
        let link = state
            .links
            .get_mut(&connection)
            .ok_or(SteamTransportError::UnknownConnection)?;
        if user == link.client_user {
            link.client_quality = quality;
        } else if user == link.host_user {
            link.host_quality = quality;
        } else {
            return Err(SteamTransportError::AdmissionUserMismatch);
        }
        Ok(())
    }

    pub fn disconnect_locally(
        &self,
        connection: SteamConnectionId,
        user: SteamUserId,
    ) -> Result<(), SteamTransportError> {
        let mut state = lock_fake(&self.shared)?;
        let (client, host, local_backend, remote_backend, was_connected) = {
            let link = state
                .links
                .get_mut(&connection)
                .ok_or(SteamTransportError::UnknownConnection)?;
            if user != link.client_user && user != link.host_user {
                return Err(SteamTransportError::AdmissionUserMismatch);
            }
            let was_connected = link.connected;
            link.connected = false;
            if user == link.client_user {
                (
                    link.client_user,
                    link.host_user,
                    link.client_backend,
                    link.host_backend,
                    was_connected,
                )
            } else {
                (
                    link.client_user,
                    link.host_user,
                    link.host_backend,
                    link.client_backend,
                    was_connected,
                )
            }
        };
        if was_connected {
            enqueue_fake_event(
                &mut state,
                local_backend,
                FakeBackendEvent::Closed {
                    connection,
                    remote: if user == client { host } else { client },
                    local_problem: true,
                },
            );
            enqueue_fake_event(
                &mut state,
                remote_backend,
                FakeBackendEvent::Closed {
                    connection,
                    remote: user,
                    local_problem: false,
                },
            );
        }
        Ok(())
    }

    pub fn connection_was_accepted(
        &self,
        connection: SteamConnectionId,
    ) -> Result<bool, SteamTransportError> {
        Ok(lock_fake(&self.shared)?
            .links
            .get(&connection)
            .ok_or(SteamTransportError::UnknownConnection)?
            .accepted)
    }

    pub fn inject_callback_overflow(&self, user: SteamUserId) -> Result<(), SteamTransportError> {
        let mut state = lock_fake(&self.shared)?;
        let backend_ids: Vec<_> = state
            .backends
            .iter()
            .filter_map(|(backend, candidate)| (*candidate == user).then_some(*backend))
            .collect();
        if backend_ids.is_empty() {
            return Err(SteamTransportError::BackendUnavailable);
        }
        for backend in backend_ids {
            state
                .inboxes
                .get_mut(&backend)
                .ok_or(SteamTransportError::BackendIntegrityFailure)?
                .overflowed = true;
        }
        Ok(())
    }

    /// Injects one exact backend send failure for deterministic retirement and
    /// fail-closed tests.
    pub fn inject_send_failure(
        &self,
        connection: SteamConnectionId,
        user: SteamUserId,
        error: SteamTransportError,
    ) -> Result<(), SteamTransportError> {
        let mut state = lock_fake(&self.shared)?;
        let link = state
            .links
            .get(&connection)
            .ok_or(SteamTransportError::UnknownConnection)?;
        if user != link.client_user && user != link.host_user {
            return Err(SteamTransportError::AdmissionUserMismatch);
        }
        let backend = if user == link.client_user {
            link.client_backend
        } else {
            link.host_backend
        };
        state.send_failures.insert((connection, backend), error);
        Ok(())
    }
}

struct FakeNetworkState {
    wire_queue_packets: usize,
    next_connection: u32,
    next_backend: u64,
    backends: BTreeMap<FakeBackendId, SteamUserId>,
    listeners: BTreeMap<(SteamUserId, i32), FakeBackendId>,
    inboxes: BTreeMap<FakeBackendId, FakeInbox>,
    allowed_incoming_users:
        BTreeMap<FakeBackendId, [Option<SteamUserId>; MAX_STEAM_TRANSPORT_CONNECTIONS]>,
    relays: BTreeMap<FakeBackendId, SteamRelayStatus>,
    links: BTreeMap<SteamConnectionId, FakeLink>,
    send_failures: BTreeMap<(SteamConnectionId, FakeBackendId), SteamTransportError>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct FakeBackendId(u64);

struct FakeInbox {
    events: VecDeque<FakeBackendEvent>,
    overflowed: bool,
}

#[derive(Clone, Copy)]
enum FakeBackendEvent {
    Incoming {
        connection: SteamConnectionId,
        remote: SteamUserId,
    },
    Connected {
        connection: SteamConnectionId,
        remote: SteamUserId,
    },
    Closed {
        connection: SteamConnectionId,
        remote: SteamUserId,
        local_problem: bool,
    },
}

struct FakeLink {
    client_user: SteamUserId,
    host_user: SteamUserId,
    client_backend: FakeBackendId,
    host_backend: FakeBackendId,
    client_open: bool,
    host_open: bool,
    client_replacement_eligible: bool,
    host_replacement_eligible: bool,
    accepted: bool,
    connected: bool,
    to_client: VecDeque<AfcDatagram>,
    to_host: VecDeque<AfcDatagram>,
    client_quality: SteamConnectionQuality,
    host_quality: SteamConnectionQuality,
}

impl Drop for FakeLink {
    fn drop(&mut self) {
        for datagram in self.to_client.iter_mut().chain(self.to_host.iter_mut()) {
            if datagram.as_slice().starts_with(&STEAM_CONTROL_MAGIC) {
                datagram.zeroize();
            }
        }
    }
}

struct FakeSteamTransportBackend {
    shared: Arc<Mutex<FakeNetworkState>>,
    id: FakeBackendId,
    local_user: SteamUserId,
    listener_port: Option<i32>,
    connect_timeout_ms: u64,
}

impl FakeSteamTransportBackend {
    fn register(
        shared: Arc<Mutex<FakeNetworkState>>,
        local_user: SteamUserId,
    ) -> Result<Self, SteamTransportError> {
        let id = {
            let mut state = lock_fake(&shared)?;
            if state.backends.len() >= MAX_FAKE_BACKENDS {
                return Err(SteamTransportError::CapacityExceeded);
            }
            let id = FakeBackendId(state.next_backend);
            state.next_backend = state
                .next_backend
                .checked_add(1)
                .ok_or(SteamTransportError::CapacityExceeded)?;
            state.backends.insert(id, local_user);
            state.inboxes.insert(
                id,
                FakeInbox {
                    events: VecDeque::with_capacity(FAKE_BACKEND_EVENT_CAPACITY),
                    overflowed: false,
                },
            );
            state
                .allowed_incoming_users
                .insert(id, [None; MAX_STEAM_TRANSPORT_CONNECTIONS]);
            state.relays.insert(id, current_fake_relay());
            id
        };
        Ok(Self {
            shared,
            id,
            local_user,
            listener_port: None,
            connect_timeout_ms: DEFAULT_CONNECT_TIMEOUT_MS,
        })
    }
}

impl SteamTransportBackend for FakeSteamTransportBackend {
    fn local_user(&self) -> SteamUserId {
        self.local_user
    }

    fn initialize_relay(&mut self) -> Result<(), SteamTransportError> {
        Ok(())
    }

    fn initialize_authentication(&mut self) -> Result<SteamRelayAvailability, SteamTransportError> {
        Ok(SteamRelayAvailability::Current)
    }

    fn authentication_status(&self) -> Result<SteamRelayAvailability, SteamTransportError> {
        Ok(SteamRelayAvailability::Current)
    }

    fn relay_status(&self) -> Result<SteamRelayStatus, SteamTransportError> {
        lock_fake(&self.shared)?
            .relays
            .get(&self.id)
            .copied()
            .ok_or(SteamTransportError::BackendUnavailable)
    }

    fn set_connect_timeout_ms(&mut self, timeout_ms: u64) -> Result<(), SteamTransportError> {
        self.connect_timeout_ms = timeout_ms;
        Ok(())
    }

    fn open_p2p_listener(&mut self, virtual_port: i32) -> Result<(), SteamTransportError> {
        if self.listener_port.is_some() {
            return Err(SteamTransportError::InvalidState);
        }
        let mut state = lock_fake(&self.shared)?;
        if state
            .listeners
            .insert((self.local_user, virtual_port), self.id)
            .is_some()
        {
            return Err(SteamTransportError::BackendOperationFailed);
        }
        self.listener_port = Some(virtual_port);
        Ok(())
    }

    fn close_p2p_listener(&mut self) {
        if let Some(port) = self.listener_port.take()
            && let Ok(mut state) = self.shared.lock()
            && state.listeners.get(&(self.local_user, port)) == Some(&self.id)
        {
            state.listeners.remove(&(self.local_user, port));
        }
    }

    fn set_allowed_incoming_users(
        &mut self,
        users: [Option<SteamUserId>; MAX_STEAM_TRANSPORT_CONNECTIONS],
    ) -> Result<(), SteamTransportError> {
        let mut state = lock_fake(&self.shared)?;
        let allowed = state
            .allowed_incoming_users
            .get_mut(&self.id)
            .ok_or(SteamTransportError::BackendUnavailable)?;
        *allowed = users;
        Ok(())
    }

    fn connect_p2p(
        &mut self,
        remote: SteamUserId,
        virtual_port: i32,
    ) -> Result<SteamConnectionId, SteamTransportError> {
        let mut state = lock_fake(&self.shared)?;
        let host_backend = state
            .listeners
            .get(&(remote, virtual_port))
            .copied()
            .ok_or(SteamTransportError::BackendUnavailable)?;
        let local_links = state
            .links
            .values()
            .filter(|link| {
                (link.client_backend == self.id && link.client_open)
                    || (link.host_backend == self.id && link.host_open)
            })
            .count();
        if local_links >= MAX_STEAM_TRANSPORT_CONNECTIONS {
            return Err(SteamTransportError::CapacityExceeded);
        }
        let duplicate_connections: Vec<_> = state
            .links
            .iter()
            .filter_map(|(connection, link)| {
                (link.client_backend == self.id && link.host_user == remote && link.client_open)
                    .then_some(*connection)
            })
            .collect();
        if duplicate_connections.len() > 1 {
            return Err(SteamTransportError::BackendIntegrityFailure);
        }
        let replacement_for = duplicate_connections.first().copied();
        if let Some(old_connection) = replacement_for
            && !state
                .links
                .get(&old_connection)
                .is_some_and(|old| old.client_replacement_eligible)
        {
            return Err(SteamTransportError::DuplicateRemoteUser);
        }
        let id = SteamConnectionId::new(state.next_connection)?;
        state.next_connection = state
            .next_connection
            .checked_add(1)
            .ok_or(SteamTransportError::CapacityExceeded)?;
        let wire_queue_packets = state.wire_queue_packets;
        state.links.insert(
            id,
            FakeLink {
                client_user: self.local_user,
                host_user: remote,
                client_backend: self.id,
                host_backend,
                client_open: true,
                host_open: true,
                client_replacement_eligible: false,
                host_replacement_eligible: false,
                accepted: false,
                connected: false,
                to_client: VecDeque::with_capacity(wire_queue_packets),
                to_host: VecDeque::with_capacity(wire_queue_packets),
                client_quality: default_fake_quality(),
                host_quality: default_fake_quality(),
            },
        );
        let queued = enqueue_fake_event(
            &mut state,
            host_backend,
            FakeBackendEvent::Incoming {
                connection: id,
                remote: self.local_user,
            },
        );
        if !queued {
            state.links.remove(&id);
            return Err(SteamTransportError::CallbackQueueOverflow);
        }
        if let Some(old_connection) = replacement_for {
            state
                .links
                .get_mut(&old_connection)
                .ok_or(SteamTransportError::BackendIntegrityFailure)?
                .client_replacement_eligible = false;
        }
        Ok(id)
    }

    fn poll_events(&mut self, maximum: usize) -> Result<Vec<BackendEvent>, SteamTransportError> {
        let mut state = lock_fake(&self.shared)?;
        let raw = {
            let inbox = state
                .inboxes
                .get_mut(&self.id)
                .ok_or(SteamTransportError::BackendUnavailable)?;
            if inbox.overflowed {
                inbox.overflowed = false;
                inbox.events.clear();
                return Err(SteamTransportError::CallbackQueueOverflow);
            }
            let count = inbox.events.len().min(maximum);
            inbox.events.drain(..count).collect::<Vec<_>>()
        };
        let mut events = Vec::with_capacity(raw.len());
        for event in raw {
            match event {
                FakeBackendEvent::Incoming { connection, remote } => {
                    let allowed = state
                        .allowed_incoming_users
                        .get(&self.id)
                        .is_some_and(|users| users.contains(&Some(remote)));
                    let duplicates: Vec<_> = state
                        .links
                        .iter()
                        .filter_map(|(candidate, link)| {
                            (*candidate != connection
                                && link.host_backend == self.id
                                && link.client_user == remote
                                && link.host_open)
                                .then_some(*candidate)
                        })
                        .collect();
                    let replacement_for =
                        (duplicates.len() == 1)
                            .then(|| duplicates[0])
                            .filter(|old| {
                                state
                                    .links
                                    .get(old)
                                    .is_some_and(|link| link.host_replacement_eligible)
                            });
                    let duplicate_allowed = duplicates.is_empty() || replacement_for.is_some();
                    if allowed && duplicate_allowed {
                        if let Some(old) = replacement_for {
                            state
                                .links
                                .get_mut(&old)
                                .ok_or(SteamTransportError::BackendIntegrityFailure)?
                                .host_replacement_eligible = false;
                        }
                        events.push(BackendEvent::Incoming {
                            connection,
                            user: remote,
                        });
                    } else {
                        close_fake_link(&mut state, connection, self.id);
                        events.push(BackendEvent::IncomingRejected {
                            user: remote,
                            reason: if allowed {
                                SteamIncomingRejection::DuplicateUser
                            } else {
                                SteamIncomingRejection::NotInAdmissionRoster
                            },
                        });
                    }
                }
                FakeBackendEvent::Connected { connection, remote } => {
                    events.push(BackendEvent::Connected {
                        connection,
                        user: remote,
                    });
                }
                FakeBackendEvent::Closed {
                    connection,
                    remote,
                    local_problem,
                } => events.push(BackendEvent::Closed {
                    connection,
                    user: remote,
                    local_problem,
                    native_end_reason: 0,
                }),
            }
        }
        Ok(events)
    }

    fn accept(&mut self, connection: SteamConnectionId) -> Result<(), SteamTransportError> {
        let mut state = lock_fake(&self.shared)?;
        let (client, host, client_backend) = {
            let link = state
                .links
                .get_mut(&connection)
                .ok_or(SteamTransportError::UnknownConnection)?;
            if link.host_backend != self.id || !link.host_open || !link.client_open || link.accepted
            {
                return Err(SteamTransportError::InvalidState);
            }
            link.accepted = true;
            link.connected = true;
            (link.client_user, link.host_user, link.client_backend)
        };
        let host_sent = enqueue_fake_event(
            &mut state,
            self.id,
            FakeBackendEvent::Connected {
                connection,
                remote: client,
            },
        );
        let client_sent = enqueue_fake_event(
            &mut state,
            client_backend,
            FakeBackendEvent::Connected {
                connection,
                remote: host,
            },
        );
        if !host_sent || !client_sent {
            return Err(SteamTransportError::CallbackQueueOverflow);
        }
        Ok(())
    }

    fn reject(&mut self, connection: SteamConnectionId, reason: SteamTransportCloseReason) {
        self.close(connection, reason);
    }

    fn state(
        &self,
        connection: SteamConnectionId,
    ) -> Result<BackendConnectionState, SteamTransportError> {
        let state = lock_fake(&self.shared)?;
        let link = state
            .links
            .get(&connection)
            .ok_or(SteamTransportError::UnknownConnection)?;
        if self.id != link.client_backend && self.id != link.host_backend {
            return Err(SteamTransportError::BackendIntegrityFailure);
        }
        if link.connected {
            Ok(BackendConnectionState::Connected)
        } else if link.client_open && link.host_open {
            Ok(BackendConnectionState::Connecting)
        } else {
            Ok(BackendConnectionState::ClosedByPeer)
        }
    }

    fn native_end_reason(
        &self,
        _connection: SteamConnectionId,
    ) -> Result<i32, SteamTransportError> {
        Ok(0)
    }

    fn send(
        &mut self,
        connection: SteamConnectionId,
        datagram: &AfcDatagram,
        _mode: BackendSendMode,
    ) -> Result<BackendSendOutcome, SteamTransportError> {
        let mut state = lock_fake(&self.shared)?;
        if let Some(error) = state.send_failures.remove(&(connection, self.id)) {
            return Err(error);
        }
        let capacity = state.wire_queue_packets;
        let Some(link) = state.links.get_mut(&connection) else {
            // The opposite endpoint may have completed its bounded retirement
            // first and removed the shared fake link. From the surviving
            // endpoint's send perspective that exact generation is closed,
            // not a backend-integrity fault.
            return Ok(BackendSendOutcome::Disconnected);
        };
        if !link.connected || !link.client_open || !link.host_open {
            return Ok(BackendSendOutcome::Disconnected);
        }
        let queue = if self.id == link.client_backend {
            &mut link.to_host
        } else if self.id == link.host_backend {
            &mut link.to_client
        } else {
            return Err(SteamTransportError::BackendIntegrityFailure);
        };
        if queue.len() >= capacity {
            return Ok(BackendSendOutcome::WouldBlock);
        }
        queue.push_back(datagram.clone());
        Ok(BackendSendOutcome::Sent)
    }

    fn receive(
        &mut self,
        connection: SteamConnectionId,
    ) -> Result<BackendReceiveOutcome, SteamTransportError> {
        let mut state = lock_fake(&self.shared)?;
        let link = state
            .links
            .get_mut(&connection)
            .ok_or(SteamTransportError::UnknownConnection)?;
        let (open, queue) = if self.id == link.client_backend {
            (link.connected && link.host_open, &mut link.to_client)
        } else if self.id == link.host_backend {
            (link.connected && link.client_open, &mut link.to_host)
        } else {
            return Err(SteamTransportError::BackendIntegrityFailure);
        };
        if let Some(datagram) = queue.pop_front() {
            return Ok(BackendReceiveOutcome::Datagram(datagram));
        }
        if open {
            Ok(BackendReceiveOutcome::Empty)
        } else {
            Ok(BackendReceiveOutcome::Disconnected)
        }
    }

    fn quality(
        &self,
        connection: SteamConnectionId,
    ) -> Result<SteamConnectionQuality, SteamTransportError> {
        let state = lock_fake(&self.shared)?;
        let link = state
            .links
            .get(&connection)
            .ok_or(SteamTransportError::UnknownConnection)?;
        if self.id == link.client_backend {
            Ok(link.client_quality)
        } else if self.id == link.host_backend {
            Ok(link.host_quality)
        } else {
            Err(SteamTransportError::BackendIntegrityFailure)
        }
    }

    fn mark_replacement_eligible(
        &mut self,
        connection: SteamConnectionId,
    ) -> Result<(), SteamTransportError> {
        let mut state = lock_fake(&self.shared)?;
        let link = state
            .links
            .get_mut(&connection)
            .ok_or(SteamTransportError::UnknownConnection)?;
        if !link.connected {
            return Err(SteamTransportError::InvalidState);
        }
        if self.id == link.client_backend && link.client_open {
            link.client_replacement_eligible = true;
        } else if self.id == link.host_backend && link.host_open {
            link.host_replacement_eligible = true;
        } else {
            return Err(SteamTransportError::BackendIntegrityFailure);
        }
        Ok(())
    }

    fn close(&mut self, connection: SteamConnectionId, _reason: SteamTransportCloseReason) {
        let Ok(mut state) = self.shared.lock() else {
            return;
        };
        close_fake_link(&mut state, connection, self.id);
    }
}

impl Drop for FakeSteamTransportBackend {
    fn drop(&mut self) {
        self.close_p2p_listener();
        let Ok(mut state) = self.shared.lock() else {
            return;
        };
        let connections: Vec<_> = state
            .links
            .iter()
            .filter_map(|(id, link)| {
                (link.client_backend == self.id || link.host_backend == self.id).then_some(*id)
            })
            .collect();
        for connection in connections {
            close_fake_link(&mut state, connection, self.id);
        }
        state.backends.remove(&self.id);
        state.inboxes.remove(&self.id);
        state.allowed_incoming_users.remove(&self.id);
        state.relays.remove(&self.id);
    }
}

fn lock_fake(
    shared: &Arc<Mutex<FakeNetworkState>>,
) -> Result<std::sync::MutexGuard<'_, FakeNetworkState>, SteamTransportError> {
    shared
        .lock()
        .map_err(|_| SteamTransportError::BackendIntegrityFailure)
}

fn enqueue_fake_event(
    state: &mut FakeNetworkState,
    backend: FakeBackendId,
    event: FakeBackendEvent,
) -> bool {
    let Some(inbox) = state.inboxes.get_mut(&backend) else {
        return false;
    };
    if inbox.events.len() >= FAKE_BACKEND_EVENT_CAPACITY {
        inbox.overflowed = true;
        return false;
    }
    inbox.events.push_back(event);
    true
}

fn close_fake_link(
    state: &mut FakeNetworkState,
    connection: SteamConnectionId,
    backend: FakeBackendId,
) {
    let Some(link) = state.links.get_mut(&connection) else {
        return;
    };
    let (remote, remote_backend, should_notify, remove) = if backend == link.client_backend {
        let should_notify = link.host_open;
        link.client_open = false;
        link.connected = false;
        (
            link.client_user,
            link.host_backend,
            should_notify,
            !link.host_open,
        )
    } else if backend == link.host_backend {
        let should_notify = link.client_open;
        link.host_open = false;
        link.connected = false;
        (
            link.host_user,
            link.client_backend,
            should_notify,
            !link.client_open,
        )
    } else {
        return;
    };
    if should_notify {
        enqueue_fake_event(
            state,
            remote_backend,
            FakeBackendEvent::Closed {
                connection,
                remote,
                local_problem: false,
            },
        );
    }
    if remove {
        state.links.remove(&connection);
    }
}

fn current_fake_relay() -> SteamRelayStatus {
    SteamRelayStatus {
        availability: SteamRelayAvailability::Current,
        network_config: SteamRelayAvailability::Current,
        any_relay: SteamRelayAvailability::Current,
        ping_measurement_in_progress: false,
    }
}

fn default_fake_quality() -> SteamConnectionQuality {
    SteamConnectionQuality {
        ping_ms: Some(42),
        local_delivery_permyriad: Some(10_000),
        remote_delivery_permyriad: Some(10_000),
        outbound_packets_per_second: 60,
        outbound_bytes_per_second: 7_200,
        inbound_packets_per_second: 60,
        inbound_bytes_per_second: 7_200,
        estimated_send_rate_bytes_per_second: 128_000,
        ..SteamConnectionQuality::default()
    }
}

#[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
mod real {
    use super::*;
    use crate::steam_platform::{RealClientOwnershipGuard, RealSteamBackend, SteamPlatform};
    use steamworks::networking_sockets::{ListenSocket, NetConnection};
    use steamworks::networking_types::{
        AppNetConnectionEnd, ConnectionRequest, ListenSocketEvent, NetConnectionEnd,
        NetworkingConfigEntry, NetworkingConfigValue, NetworkingConnectionState,
        NetworkingIdentity, SendFlags,
    };

    struct RealConnection {
        id: SteamConnectionId,
        remote: SteamUserId,
        replacement_eligible: bool,
        kind: RealConnectionKind,
    }

    enum RealConnectionKind {
        Pending(Option<ConnectionRequest>),
        AcceptedWaitingForCallback,
        Active(NetConnection),
    }

    pub(super) struct RealSteamTransportBackend {
        client: steamworks::Client,
        callback_owner_alive: Arc<AtomicBool>,
        ownership: Arc<RealClientOwnershipGuard>,
        local_user: SteamUserId,
        listener: Option<ListenSocket>,
        allowed_incoming_users: [Option<SteamUserId>; MAX_STEAM_TRANSPORT_CONNECTIONS],
        connections: Vec<RealConnection>,
        connect_timeout_ms: i32,
    }

    impl RealSteamTransportBackend {
        fn new(
            client: steamworks::Client,
            callback_owner_alive: Arc<AtomicBool>,
            ownership: Arc<RealClientOwnershipGuard>,
            local_user: SteamUserId,
        ) -> Self {
            Self {
                client,
                callback_owner_alive,
                ownership,
                local_user,
                listener: None,
                allowed_incoming_users: [None; MAX_STEAM_TRANSPORT_CONNECTIONS],
                connections: Vec::with_capacity(MAX_STEAM_TRANSPORT_CONNECTIONS),
                connect_timeout_ms: DEFAULT_CONNECT_TIMEOUT_MS as i32,
            }
        }

        fn ensure_callback_owner(&self) -> Result<(), SteamTransportError> {
            if self.callback_owner_alive.load(Ordering::Acquire) {
                Ok(())
            } else {
                Err(SteamTransportError::CallbackOwnerGone)
            }
        }

        fn allocate_connection_id(&self) -> Result<SteamConnectionId, SteamTransportError> {
            SteamConnectionId::new(
                self.ownership
                    .allocate_transport_connection_id()
                    .ok_or(SteamTransportError::CapacityExceeded)?,
            )
        }

        fn translate_listen_event(
            &mut self,
            event: ListenSocketEvent,
        ) -> Result<BackendEvent, SteamTransportError> {
            match event {
                ListenSocketEvent::Connecting(request) => {
                    let remote = match real_steam_user(request.remote()) {
                        Ok(remote) => remote,
                        Err(_) => {
                            request.reject(
                                exceptional_end(SteamTransportCloseReason::AdmissionRejected),
                                Some("AFC requires a Steam admission identity"),
                            );
                            return Ok(BackendEvent::IncomingPressure { rejected: 1 });
                        }
                    };
                    if !self.allowed_incoming_users.contains(&Some(remote)) {
                        request.reject(
                            exceptional_end(SteamTransportCloseReason::AdmissionRejected),
                            Some("AFC Steam identity is not in the admission roster"),
                        );
                        return Ok(BackendEvent::IncomingRejected {
                            user: remote,
                            reason: SteamIncomingRejection::NotInAdmissionRoster,
                        });
                    }
                    if self.connections.len() >= MAX_STEAM_TRANSPORT_CONNECTIONS {
                        request.reject(
                            exceptional_end(SteamTransportCloseReason::AdmissionRejected),
                            Some("AFC connection capacity reached"),
                        );
                        return Ok(BackendEvent::IncomingRejected {
                            user: remote,
                            reason: SteamIncomingRejection::Capacity,
                        });
                    }
                    let duplicate_indices: Vec<_> = self
                        .connections
                        .iter()
                        .enumerate()
                        .filter_map(|(index, connection)| {
                            (connection.remote == remote).then_some(index)
                        })
                        .collect();
                    let replacement_index = (duplicate_indices.len() == 1)
                        .then(|| duplicate_indices[0])
                        .filter(|index| self.connections[*index].replacement_eligible);
                    if !duplicate_indices.is_empty() && replacement_index.is_none() {
                        request.reject(
                            exceptional_end(SteamTransportCloseReason::AdmissionRejected),
                            Some("AFC duplicate Steam identity"),
                        );
                        return Ok(BackendEvent::IncomingRejected {
                            user: remote,
                            reason: SteamIncomingRejection::DuplicateUser,
                        });
                    }
                    let id = self.allocate_connection_id()?;
                    if request
                        .set_connection_user_data(i64::from(id.get()))
                        .is_err()
                    {
                        request.reject(
                            exceptional_end(SteamTransportCloseReason::TransportFault),
                            Some("AFC connection generation tagging failed"),
                        );
                        return Err(SteamTransportError::BackendOperationFailed);
                    }
                    if let Some(index) = replacement_index {
                        self.connections[index].replacement_eligible = false;
                    }
                    self.connections.push(RealConnection {
                        id,
                        remote,
                        replacement_eligible: false,
                        kind: RealConnectionKind::Pending(Some(request)),
                    });
                    Ok(BackendEvent::Incoming {
                        connection: id,
                        user: remote,
                    })
                }
                ListenSocketEvent::Connected(event) => {
                    let remote = match real_steam_user(event.remote()) {
                        Ok(remote) => remote,
                        Err(_) => {
                            event.take_connection().close(
                                exceptional_end(SteamTransportCloseReason::AdmissionRejected),
                                Some("AFC requires a Steam connection identity"),
                                false,
                            );
                            return Ok(BackendEvent::IncomingPressure { rejected: 1 });
                        }
                    };
                    let info = self
                        .client
                        .networking_sockets()
                        .get_connection_info(event.connection())
                        .map_err(|_| SteamTransportError::BackendOperationFailed)?;
                    let current_remote = info.identity_remote().and_then(|identity| {
                        // A callback snapshot alone is insufficient admission
                        // evidence. Bind the accepted handle to its live remote
                        // identity before selecting the AFC generation.
                        real_steam_user(identity).ok()
                    });
                    if current_remote != Some(remote) {
                        event.take_connection().close(
                            exceptional_end(SteamTransportCloseReason::AdmissionRejected),
                            Some("AFC Steam connection identity changed"),
                            false,
                        );
                        return Ok(BackendEvent::IncomingRejected {
                            user: remote,
                            reason: SteamIncomingRejection::IdentityMismatch,
                        });
                    }
                    let tagged = event
                        .connection()
                        .connection_user_data()
                        .ok()
                        .and_then(|raw| u32::try_from(raw).ok())
                        .and_then(|raw| SteamConnectionId::new(raw).ok());
                    let Some(connection) = tagged.and_then(|tagged| {
                        self.connections.iter_mut().find(|connection| {
                            connection.id == tagged
                                && connection.remote == remote
                                && matches!(
                                    connection.kind,
                                    RealConnectionKind::AcceptedWaitingForCallback
                                )
                        })
                    }) else {
                        event.take_connection().close(
                            exceptional_end(SteamTransportCloseReason::AdmissionRejected),
                            Some("AFC stale connection callback"),
                            false,
                        );
                        return Ok(BackendEvent::IncomingRejected {
                            user: remote,
                            reason: SteamIncomingRejection::UnexpectedConnection,
                        });
                    };
                    connection.kind = RealConnectionKind::Active(event.take_connection());
                    Ok(BackendEvent::Connected {
                        connection: connection.id,
                        user: remote,
                    })
                }
                ListenSocketEvent::Disconnected(event) => {
                    let remote = real_steam_user(event.remote())?;
                    let tagged = u32::try_from(event.user_data())
                        .ok()
                        .and_then(|raw| SteamConnectionId::new(raw).ok());
                    let index = tagged.and_then(|tagged| {
                        self.connections.iter().position(|connection| {
                            connection.id == tagged && connection.remote == remote
                        })
                    });
                    let Some(index) = index else {
                        // A delayed callback from a locally retired exact
                        // generation is benign. Its user-data tag must never
                        // select a same-identity replacement.
                        return Ok(BackendEvent::IncomingPressure { rejected: 0 });
                    };
                    let connection = self.connections.swap_remove(index);
                    close_real_kind(connection.kind, SteamTransportCloseReason::RemoteClosed);
                    Ok(BackendEvent::Closed {
                        connection: connection.id,
                        user: remote,
                        local_problem: false,
                        native_end_reason: i32::from(event.end_reason()),
                    })
                }
            }
        }

        fn close_all(&mut self, reason: SteamTransportCloseReason) {
            for connection in self.connections.drain(..) {
                close_real_kind(connection.kind, reason);
            }
        }
    }

    impl SteamTransportBackend for RealSteamTransportBackend {
        fn local_user(&self) -> SteamUserId {
            self.local_user
        }

        fn initialize_relay(&mut self) -> Result<(), SteamTransportError> {
            self.ensure_callback_owner()?;
            self.client.networking_utils().init_relay_network_access();
            Ok(())
        }

        fn initialize_authentication(
            &mut self,
        ) -> Result<SteamRelayAvailability, SteamTransportError> {
            self.ensure_callback_owner()?;
            Ok(map_steam_relay_availability(
                self.client.networking_sockets().init_authentication(),
            ))
        }

        fn authentication_status(&self) -> Result<SteamRelayAvailability, SteamTransportError> {
            self.ensure_callback_owner()?;
            Ok(map_steam_relay_availability(
                self.client.networking_sockets().get_authentication_status(),
            ))
        }

        fn relay_status(&self) -> Result<SteamRelayStatus, SteamTransportError> {
            self.ensure_callback_owner()?;
            Ok(steam_client_relay_status(&self.client))
        }

        fn set_connect_timeout_ms(&mut self, timeout_ms: u64) -> Result<(), SteamTransportError> {
            self.connect_timeout_ms =
                i32::try_from(timeout_ms).map_err(|_| SteamTransportError::InvalidConfiguration)?;
            Ok(())
        }

        fn open_p2p_listener(&mut self, virtual_port: i32) -> Result<(), SteamTransportError> {
            self.ensure_callback_owner()?;
            if self.listener.is_some() {
                return Err(SteamTransportError::InvalidState);
            }
            let listener = self
                .client
                .networking_sockets()
                .create_listen_socket_p2p(
                    virtual_port,
                    [
                        NetworkingConfigEntry::new_int32(
                            NetworkingConfigValue::TimeoutInitial,
                            self.connect_timeout_ms,
                        ),
                        // Zero is never a valid AFC generation. Incoming
                        // requests replace this sentinel before AcceptConnection;
                        // queued pre-tag callbacks therefore cannot alias a peer.
                        NetworkingConfigEntry::new_int64(
                            NetworkingConfigValue::ConnectionUserData,
                            0,
                        ),
                    ],
                )
                .map_err(|_| SteamTransportError::BackendOperationFailed)?;
            self.listener = Some(listener);
            Ok(())
        }

        fn close_p2p_listener(&mut self) {
            self.listener = None;
        }

        fn set_allowed_incoming_users(
            &mut self,
            users: [Option<SteamUserId>; MAX_STEAM_TRANSPORT_CONNECTIONS],
        ) -> Result<(), SteamTransportError> {
            self.ensure_callback_owner()?;
            self.allowed_incoming_users = users;
            Ok(())
        }

        fn connect_p2p(
            &mut self,
            remote: SteamUserId,
            virtual_port: i32,
        ) -> Result<SteamConnectionId, SteamTransportError> {
            self.ensure_callback_owner()?;
            if self.connections.len() >= MAX_STEAM_TRANSPORT_CONNECTIONS {
                return Err(SteamTransportError::CapacityExceeded);
            }
            let duplicate_indices: Vec<_> = self
                .connections
                .iter()
                .enumerate()
                .filter_map(|(index, connection)| (connection.remote == remote).then_some(index))
                .collect();
            let replacement_index = if duplicate_indices.is_empty() {
                None
            } else if duplicate_indices.len() == 1
                && self.connections[duplicate_indices[0]].replacement_eligible
            {
                Some(duplicate_indices[0])
            } else {
                return Err(SteamTransportError::DuplicateRemoteUser);
            };
            let id = self.allocate_connection_id()?;
            let connection = self
                .client
                .networking_sockets()
                .connect_p2p(
                    NetworkingIdentity::new_steam_id(steamworks::SteamId::from_raw(remote.get())),
                    virtual_port,
                    [
                        NetworkingConfigEntry::new_int32(
                            NetworkingConfigValue::TimeoutInitial,
                            self.connect_timeout_ms,
                        ),
                        NetworkingConfigEntry::new_int64(
                            NetworkingConfigValue::ConnectionUserData,
                            i64::from(id.get()),
                        ),
                    ],
                )
                .map_err(|_| SteamTransportError::BackendOperationFailed)?;
            if !matches!(
                connection.connection_user_data(),
                Ok(tag) if tag == i64::from(id.get())
            ) {
                connection.close(
                    exceptional_end(SteamTransportCloseReason::TransportFault),
                    Some("AFC connection generation tag was not installed atomically"),
                    false,
                );
                return Err(SteamTransportError::BackendOperationFailed);
            }
            if let Some(index) = replacement_index {
                self.connections[index].replacement_eligible = false;
            }
            self.connections.push(RealConnection {
                id,
                remote,
                replacement_eligible: false,
                kind: RealConnectionKind::Active(connection),
            });
            Ok(id)
        }

        fn poll_events(
            &mut self,
            maximum: usize,
        ) -> Result<Vec<BackendEvent>, SteamTransportError> {
            self.ensure_callback_owner()?;
            let mut raw = Vec::with_capacity(maximum);
            if let Some(listener) = &self.listener {
                for _ in 0..maximum {
                    let Some(event) = listener.try_receive_event() else {
                        break;
                    };
                    raw.push(event);
                }
            }
            let mut translated = Vec::with_capacity(raw.len());
            for event in raw {
                translated.push(self.translate_listen_event(event)?);
            }
            Ok(translated)
        }

        fn accept(&mut self, connection: SteamConnectionId) -> Result<(), SteamTransportError> {
            self.ensure_callback_owner()?;
            let Some(index) = self
                .connections
                .iter()
                .position(|entry| entry.id == connection)
            else {
                return Err(SteamTransportError::UnknownConnection);
            };
            let request = match &mut self.connections[index].kind {
                RealConnectionKind::Pending(request) => request
                    .take()
                    .ok_or(SteamTransportError::BackendIntegrityFailure)?,
                _ => return Err(SteamTransportError::InvalidState),
            };
            request
                .accept()
                .map_err(|_| SteamTransportError::BackendOperationFailed)?;
            self.connections[index].kind = RealConnectionKind::AcceptedWaitingForCallback;
            Ok(())
        }

        fn reject(&mut self, connection: SteamConnectionId, reason: SteamTransportCloseReason) {
            self.close(connection, reason);
        }

        fn state(
            &self,
            connection: SteamConnectionId,
        ) -> Result<BackendConnectionState, SteamTransportError> {
            self.ensure_callback_owner()?;
            let entry = self
                .connections
                .iter()
                .find(|entry| entry.id == connection)
                .ok_or(SteamTransportError::UnknownConnection)?;
            match &entry.kind {
                RealConnectionKind::Pending(_) | RealConnectionKind::AcceptedWaitingForCallback => {
                    Ok(BackendConnectionState::Connecting)
                }
                RealConnectionKind::Active(connection) => {
                    let info = self
                        .client
                        .networking_sockets()
                        .get_connection_info(connection)
                        .map_err(|_| SteamTransportError::BackendOperationFailed)?;
                    let current_remote = info
                        .identity_remote()
                        .ok_or(SteamTransportError::BackendIntegrityFailure)
                        .and_then(real_steam_user)?;
                    if current_remote != entry.remote {
                        return Err(SteamTransportError::AdmissionIdentityMismatch);
                    }
                    match info
                        .state()
                        .map_err(|_| SteamTransportError::BackendIntegrityFailure)?
                    {
                        NetworkingConnectionState::Connecting
                        | NetworkingConnectionState::FindingRoute => {
                            Ok(BackendConnectionState::Connecting)
                        }
                        NetworkingConnectionState::Connected => {
                            Ok(BackendConnectionState::Connected)
                        }
                        NetworkingConnectionState::ClosedByPeer => {
                            Ok(BackendConnectionState::ClosedByPeer)
                        }
                        NetworkingConnectionState::ProblemDetectedLocally
                        | NetworkingConnectionState::None => {
                            Ok(BackendConnectionState::ProblemDetectedLocally)
                        }
                    }
                }
            }
        }

        fn native_end_reason(
            &self,
            connection: SteamConnectionId,
        ) -> Result<i32, SteamTransportError> {
            self.ensure_callback_owner()?;
            let entry = self
                .connections
                .iter()
                .find(|entry| entry.id == connection)
                .ok_or(SteamTransportError::UnknownConnection)?;
            let RealConnectionKind::Active(connection) = &entry.kind else {
                return Ok(0);
            };
            let info = self
                .client
                .networking_sockets()
                .get_connection_info(connection)
                .map_err(|_| SteamTransportError::BackendOperationFailed)?;
            Ok(info.end_reason().map(i32::from).unwrap_or_default())
        }

        fn send(
            &mut self,
            connection: SteamConnectionId,
            datagram: &AfcDatagram,
            mode: BackendSendMode,
        ) -> Result<BackendSendOutcome, SteamTransportError> {
            self.ensure_callback_owner()?;
            let entry = self
                .connections
                .iter()
                .find(|entry| entry.id == connection)
                .ok_or(SteamTransportError::UnknownConnection)?;
            let RealConnectionKind::Active(connection) = &entry.kind else {
                return Ok(BackendSendOutcome::Disconnected);
            };
            let flags = match mode {
                BackendSendMode::Gameplay => SendFlags::UNRELIABLE_NO_DELAY,
                BackendSendMode::ReliableControl => SendFlags::RELIABLE_NO_NAGLE,
            };
            match connection.send_message(datagram.as_slice(), flags) {
                Ok(_) => Ok(BackendSendOutcome::Sent),
                Err(
                    steamworks::SteamError::Busy
                    | steamworks::SteamError::LimitExceeded
                    | steamworks::SteamError::Ignored
                    | steamworks::SteamError::Pending,
                ) => Ok(BackendSendOutcome::WouldBlock),
                Err(
                    steamworks::SteamError::NoConnection
                    | steamworks::SteamError::InvalidState
                    | steamworks::SteamError::RemoteDisconnect,
                ) => Ok(BackendSendOutcome::Disconnected),
                Err(_) => Err(SteamTransportError::BackendOperationFailed),
            }
        }

        fn receive(
            &mut self,
            connection: SteamConnectionId,
        ) -> Result<BackendReceiveOutcome, SteamTransportError> {
            self.ensure_callback_owner()?;
            let entry = self
                .connections
                .iter_mut()
                .find(|entry| entry.id == connection)
                .ok_or(SteamTransportError::UnknownConnection)?;
            let RealConnectionKind::Active(connection) = &mut entry.kind else {
                return Ok(BackendReceiveOutcome::Disconnected);
            };
            let mut messages = connection
                .receive_messages(1)
                .map_err(|_| SteamTransportError::BackendOperationFailed)?;
            let Some(message) = messages.pop() else {
                return Ok(BackendReceiveOutcome::Empty);
            };
            let data = message.data();
            if data.len() > MAX_AFC_DATAGRAM_BYTES {
                return Ok(BackendReceiveOutcome::Oversized(data.len()));
            }
            let datagram = AfcDatagram::try_from_slice(data)
                .map_err(|_| SteamTransportError::BackendIntegrityFailure)?;
            Ok(BackendReceiveOutcome::Datagram(datagram))
        }

        fn quality(
            &self,
            connection: SteamConnectionId,
        ) -> Result<SteamConnectionQuality, SteamTransportError> {
            self.ensure_callback_owner()?;
            let entry = self
                .connections
                .iter()
                .find(|entry| entry.id == connection)
                .ok_or(SteamTransportError::UnknownConnection)?;
            let RealConnectionKind::Active(connection) = &entry.kind else {
                return Err(SteamTransportError::EndpointNotReady);
            };
            let (quality, _) = self
                .client
                .networking_sockets()
                .get_realtime_connection_status(connection, 0)
                .map_err(|_| SteamTransportError::BackendOperationFailed)?;
            Ok(SteamConnectionQuality {
                ping_ms: nonnegative_i32(quality.ping()),
                local_delivery_permyriad: quality_permyriad(quality.connection_quality_local()),
                remote_delivery_permyriad: quality_permyriad(quality.connection_quality_remote()),
                outbound_packets_per_second: nonnegative_f32(quality.out_packets_per_sec()),
                outbound_bytes_per_second: nonnegative_f32(quality.out_bytes_per_sec()),
                inbound_packets_per_second: nonnegative_f32(quality.in_packets_per_sec()),
                inbound_bytes_per_second: nonnegative_f32(quality.in_bytes_per_sec()),
                estimated_send_rate_bytes_per_second: nonnegative_i32(
                    quality.send_rate_bytes_per_sec(),
                )
                .unwrap_or(0),
                pending_unreliable_bytes: nonnegative_i32(quality.pending_unreliable())
                    .unwrap_or(0),
                pending_reliable_bytes: nonnegative_i32(quality.pending_reliable()).unwrap_or(0),
                sent_unacked_reliable_bytes: nonnegative_i32(quality.sent_unacked_reliable())
                    .unwrap_or(0),
                estimated_queue_delay_micros: u64::try_from(quality.queued_send_bytes())
                    .unwrap_or(0),
            })
        }

        fn mark_replacement_eligible(
            &mut self,
            connection: SteamConnectionId,
        ) -> Result<(), SteamTransportError> {
            self.ensure_callback_owner()?;
            let entry = self
                .connections
                .iter_mut()
                .find(|entry| entry.id == connection)
                .ok_or(SteamTransportError::UnknownConnection)?;
            if !matches!(entry.kind, RealConnectionKind::Active(_)) || entry.replacement_eligible {
                return Err(SteamTransportError::InvalidState);
            }
            entry.replacement_eligible = true;
            Ok(())
        }

        fn close(&mut self, connection: SteamConnectionId, reason: SteamTransportCloseReason) {
            let Some(index) = self
                .connections
                .iter()
                .position(|entry| entry.id == connection)
            else {
                return;
            };
            let entry = self.connections.swap_remove(index);
            close_real_kind(entry.kind, reason);
        }
    }

    impl Drop for RealSteamTransportBackend {
        fn drop(&mut self) {
            self.listener = None;
            self.close_all(SteamTransportCloseReason::Requested);
        }
    }

    impl SteamTransport {
        pub fn from_steam_platform(
            platform: &SteamPlatform<RealSteamBackend>,
            session: SteamP2pSession,
            config: SteamTransportConfig,
            now_ms: u64,
        ) -> Result<Self, SteamTransportError> {
            let (client, callback_owner_alive, ownership) =
                platform.steam_transport_client_access();
            let backend = RealSteamTransportBackend::new(
                client,
                callback_owner_alive,
                ownership,
                platform.local_user(),
            );
            Self::from_backend(Box::new(backend), session, config, now_ms)
        }
    }

    fn real_steam_user(identity: NetworkingIdentity) -> Result<SteamUserId, SteamTransportError> {
        let id = identity
            .steam_id()
            .ok_or(SteamTransportError::AdmissionIdentityMismatch)?;
        SteamUserId::new(id.raw()).map_err(|_| SteamTransportError::AdmissionIdentityMismatch)
    }

    fn normal_end(reason: SteamTransportCloseReason) -> NetConnectionEnd {
        let code = match reason {
            SteamTransportCloseReason::Requested => 1001,
            SteamTransportCloseReason::EndpointDropped => 1002,
            _ => return exceptional_end(reason),
        };
        NetConnectionEnd::App(AppNetConnectionEnd::normal(code))
    }

    fn exceptional_end(reason: SteamTransportCloseReason) -> NetConnectionEnd {
        let code = match reason {
            SteamTransportCloseReason::AdmissionRejected => 2001,
            SteamTransportCloseReason::AdmissionTimedOut => 2002,
            SteamTransportCloseReason::ConnectTimedOut => 2003,
            SteamTransportCloseReason::InboundQueueOverflow => 2004,
            SteamTransportCloseReason::OversizedDatagram => 2005,
            SteamTransportCloseReason::BackendFailure => 2006,
            SteamTransportCloseReason::TransportFault => 2007,
            SteamTransportCloseReason::LocalProblem => 2008,
            SteamTransportCloseReason::RemoteClosed => 2009,
            SteamTransportCloseReason::QualityPolicyRejected => 2010,
            SteamTransportCloseReason::MalformedControlTraffic => 2011,
            SteamTransportCloseReason::ProtocolViolation => 2012,
            SteamTransportCloseReason::AuthenticationRejected => 2013,
            SteamTransportCloseReason::Requested | SteamTransportCloseReason::EndpointDropped => {
                return normal_end(reason);
            }
        };
        NetConnectionEnd::App(AppNetConnectionEnd::exception(code))
    }

    fn close_real_kind(kind: RealConnectionKind, reason: SteamTransportCloseReason) {
        match kind {
            RealConnectionKind::Pending(Some(request)) => {
                request.reject(
                    normal_or_exception_end(reason),
                    Some("AFC connection closed"),
                );
            }
            RealConnectionKind::Active(connection) => {
                connection.close(
                    normal_or_exception_end(reason),
                    Some("AFC connection closed"),
                    matches!(reason, SteamTransportCloseReason::Requested),
                );
            }
            RealConnectionKind::Pending(None) | RealConnectionKind::AcceptedWaitingForCallback => {
                // steamworks 0.12.2 consumes ConnectionRequest on accept and does
                // not expose the handle again until Connected; the next callback
                // is still drained and rejected by the live listener owner.
            }
        }
    }

    fn normal_or_exception_end(reason: SteamTransportCloseReason) -> NetConnectionEnd {
        match reason {
            SteamTransportCloseReason::Requested | SteamTransportCloseReason::EndpointDropped => {
                normal_end(reason)
            }
            _ => exceptional_end(reason),
        }
    }

    fn quality_permyriad(value: f32) -> Option<u16> {
        if !value.is_finite() || value < 0.0 {
            None
        } else {
            Some((value.clamp(0.0, 1.0) * 10_000.0).round() as u16)
        }
    }

    fn nonnegative_f32(value: f32) -> u32 {
        if value.is_finite() && value > 0.0 {
            value.min(u32::MAX as f32).round() as u32
        } else {
            0
        }
    }

    fn nonnegative_i32(value: i32) -> Option<u32> {
        u32::try_from(value).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::match_config::current_compatibility;
    use crate::network_codec::{Handshake, WireMessage};
    use crate::network_protocol::{
        DisconnectCode, DisconnectMessage, MatchId, RetryDisposition, SimTick,
    };
    use crate::network_runtime::{
        NetworkRuntime, PeerRole, ReliableSendStatus, RuntimeConfig, RuntimeEvent,
    };
    use crate::reconnect::AuthenticatedUserId;
    use crate::steam_platform::AdmissionPurpose;

    const NOW_MS: u64 = 10_000;

    fn user(value: u64) -> SteamUserId {
        SteamUserId::new(value).unwrap()
    }

    fn lobby(value: u64) -> SteamLobbyId {
        SteamLobbyId::new(value).unwrap()
    }

    fn admission(
        lobby: SteamLobbyId,
        remote: SteamUserId,
        license_owner: SteamUserId,
    ) -> AuthenticatedSteamPeer {
        AuthenticatedSteamPeer {
            lobby,
            user: remote,
            license_owner_user: license_owner,
            authenticated_user: AuthenticatedUserId::new(remote.get()).unwrap(),
            local_seats: 1,
            purpose: AdmissionPurpose::Initial,
        }
    }

    fn reconnect_admission(
        lobby: SteamLobbyId,
        remote: SteamUserId,
        license_owner: SteamUserId,
    ) -> AuthenticatedSteamPeer {
        AuthenticatedSteamPeer {
            purpose: AdmissionPurpose::Reconnect,
            ..admission(lobby, remote, license_owner)
        }
    }

    fn sessions(lobby: SteamLobbyId, authority: SteamUserId) -> (SteamP2pSession, SteamP2pSession) {
        (
            SteamP2pSession {
                lobby,
                authority_user: authority,
                role: SteamTransportRole::ListenAuthority,
                virtual_port: 0,
            },
            SteamP2pSession {
                lobby,
                authority_user: authority,
                role: SteamTransportRole::Client,
                virtual_port: 0,
            },
        )
    }

    fn connected_pair(
        config: SteamTransportConfig,
    ) -> (
        FakeSteamTransportNetwork,
        SteamTransport,
        SteamTransport,
        SteamConnectionId,
        SteamDatagramEndpoint,
        SteamDatagramEndpoint,
    ) {
        connected_pair_with_wire_capacity(config, 32)
    }

    fn connected_pair_with_wire_capacity(
        config: SteamTransportConfig,
        wire_queue_packets: usize,
    ) -> (
        FakeSteamTransportNetwork,
        SteamTransport,
        SteamTransport,
        SteamConnectionId,
        SteamDatagramEndpoint,
        SteamDatagramEndpoint,
    ) {
        let authority = user(1001);
        let client = user(1002);
        let lobby = lobby(9001);
        let network = FakeSteamTransportNetwork::new(wire_queue_packets).unwrap();
        let (host_session, client_session) = sessions(lobby, authority);
        let mut host = network
            .create_transport(authority, host_session, config, NOW_MS)
            .unwrap();
        let mut client_transport = network
            .create_transport(client, client_session, config, NOW_MS)
            .unwrap();
        host.start_listening().unwrap();
        host.set_allowed_incoming_users(&[client]).unwrap();
        let connection = client_transport
            .connect_p2p(admission(lobby, authority, authority), NOW_MS)
            .unwrap();
        host.pump(NOW_MS + 1).unwrap();
        assert_eq!(
            host.poll_event(),
            Some(SteamTransportEvent::IncomingPending {
                connection,
                lobby,
                user: client,
                expires_at_ms: NOW_MS + 1 + config.pending_admission_timeout_ms,
            })
        );
        host.admit_incoming(connection, admission(lobby, client, client), NOW_MS + 1)
            .unwrap();
        host.pump(NOW_MS + 2).unwrap();
        client_transport.pump(NOW_MS + 2).unwrap();
        assert_eq!(
            host.poll_event(),
            Some(SteamTransportEvent::ConnectionReady {
                connection,
                lobby,
                user: client,
            })
        );
        assert_eq!(
            client_transport.poll_event(),
            Some(SteamTransportEvent::ConnectionReady {
                connection,
                lobby,
                user: authority,
            })
        );
        let host_endpoint = host.take_endpoint(connection).unwrap().endpoint;
        let client_endpoint = client_transport.take_endpoint(connection).unwrap().endpoint;
        (
            network,
            host,
            client_transport,
            connection,
            host_endpoint,
            client_endpoint,
        )
    }

    fn quarantined_control_pair(
        config: SteamTransportConfig,
    ) -> (
        FakeSteamTransportNetwork,
        SteamTransport,
        SteamTransport,
        SteamConnectionId,
        SteamLobbyId,
        SteamUserId,
        SteamUserId,
    ) {
        let authority = user(1101);
        let client = user(1102);
        let lobby = lobby(9101);
        let network = FakeSteamTransportNetwork::new(32).unwrap();
        let (host_session, client_session) = sessions(lobby, authority);
        let mut host = network
            .create_transport(authority, host_session, config, NOW_MS)
            .unwrap();
        let mut client_transport = network
            .create_transport(client, client_session, config, NOW_MS)
            .unwrap();
        host.start_listening().unwrap();
        host.set_allowed_incoming_users(&[client]).unwrap();
        let connection = client_transport.connect_control(authority, NOW_MS).unwrap();
        host.pump(NOW_MS + 1).unwrap();
        assert!(matches!(
            host.poll_event(),
            Some(SteamTransportEvent::IncomingPending { connection: observed, .. })
                if observed == connection
        ));
        host.accept_control(connection, NOW_MS + 1).unwrap();
        host.pump(NOW_MS + 2).unwrap();
        client_transport.pump(NOW_MS + 2).unwrap();
        assert!(matches!(
            host.poll_event(),
            Some(SteamTransportEvent::ControlReady { connection: observed, .. })
                if observed == connection
        ));
        assert!(matches!(
            client_transport.poll_event(),
            Some(SteamTransportEvent::ControlReady { connection: observed, .. })
                if observed == connection
        ));
        (
            network,
            host,
            client_transport,
            connection,
            lobby,
            authority,
            client,
        )
    }

    #[test]
    fn quarantined_control_is_reliable_idempotent_and_requires_explicit_promotion() {
        let config = SteamTransportConfig::default();
        let (_, mut host, mut client_transport, connection, lobby, authority, client) =
            quarantined_control_pair(config);

        assert!(matches!(
            host.take_endpoint(connection),
            Err(SteamTransportError::EndpointNotReady)
        ));
        let client_hello = client_transport
            .poll_control()
            .expect("authority hello reaches the client");
        assert!(matches!(
            client_hello.message,
            SteamControlMessage::LinkHello { .. }
        ));
        client_transport
            .accept_control_ingress(client_hello.token)
            .unwrap();
        // Suppress the standalone ACK in this test so the same cumulative ACK
        // is observed exclusively on the following application message.
        client_transport
            .connections
            .iter_mut()
            .find(|record| record.id == connection)
            .unwrap()
            .io
            .as_mut()
            .unwrap()
            .control
            .acknowledgement = None;
        let identity = SteamControlIdentity::new(lobby, client, authority).unwrap();
        let sequence = client_transport
            .queue_control(
                connection,
                SteamControlMessage::AuthAccepted {
                    identity,
                    ticket_sequence: 77,
                },
            )
            .unwrap();

        client_transport.pump(NOW_MS + 3).unwrap();
        client_transport
            .pump(NOW_MS + 3 + DEFAULT_CONTROL_RETRANSMIT_MS)
            .unwrap();
        host.pump(NOW_MS + 4 + DEFAULT_CONTROL_RETRANSMIT_MS)
            .unwrap();
        assert_eq!(
            host.outstanding_control_frames(connection),
            Some(1),
            "a piggyback ACK must not retire the local hello before semantic acceptance"
        );
        let mut received = None;
        while let Some(ingress) = host.poll_control() {
            let token = ingress.token;
            if ingress.sequence == sequence {
                received = Some(ingress);
            }
            host.accept_control_ingress(token).unwrap();
        }
        let received = received.expect("application receives the reliable control message");
        assert!(matches!(
            received.message,
            SteamControlMessage::AuthAccepted {
                ticket_sequence: 77,
                ..
            }
        ));
        assert!(
            std::iter::from_fn(|| host.poll_control()).all(|ingress| ingress.sequence != sequence)
        );
        assert!(host.metrics().duplicate_control_frames >= 1);

        host.pump(NOW_MS + 5 + DEFAULT_CONTROL_RETRANSMIT_MS)
            .unwrap();
        client_transport
            .pump(NOW_MS + 6 + DEFAULT_CONTROL_RETRANSMIT_MS)
            .unwrap();
        assert_eq!(
            client_transport.outstanding_control_frames(connection),
            Some(0)
        );

        host.mark_authenticating(connection).unwrap();
        client_transport.mark_authenticating(connection).unwrap();
        host.mark_secure(connection, admission(lobby, client, client))
            .unwrap();
        client_transport
            .mark_secure(connection, admission(lobby, authority, authority))
            .unwrap();
        host.begin_manifest_agreement(connection).unwrap();
        client_transport
            .begin_manifest_agreement(connection)
            .unwrap();
        host.arm_gameplay_receive(connection).unwrap();
        client_transport.arm_gameplay_receive(connection).unwrap();
        host.promote_to_gameplay(connection).unwrap();
        client_transport.promote_to_gameplay(connection).unwrap();
        assert!(host.take_endpoint(connection).is_ok());
        assert!(client_transport.take_endpoint(connection).is_ok());
    }

    #[test]
    fn setup_phase_rejects_skips_and_keeps_duplicate_transitions_idempotent() {
        let (_, mut host, _, connection, _, _, _) =
            quarantined_control_pair(SteamTransportConfig::default());
        assert_eq!(
            host.promote_to_gameplay(connection),
            Err(SteamTransportError::IllegalSetupTransition)
        );
        host.mark_authenticating(connection).unwrap();
        host.mark_authenticating(connection).unwrap();
        assert_eq!(
            host.begin_manifest_agreement(connection),
            Err(SteamTransportError::IllegalSetupTransition)
        );

        for result_code in 0..80_u16 {
            host.record_peer_trace(connection, 600 + result_code, 0);
        }
        let active = host.peer_trace(connection).unwrap();
        assert_eq!(active.events.len(), STEAM_PEER_TRACE_EVENT_CAPACITY);
        assert!(
            active
                .events
                .windows(2)
                .all(|events| events[0].ordinal < events[1].ordinal)
        );
        assert!(active.events.iter().all(|event| {
            event.connection_generation == connection.get() && event.native_end_reason == 0
        }));

        host.close_connection(connection).unwrap();
        let completed = host.take_completed_peer_trace().unwrap();
        assert_eq!(completed.events.len(), STEAM_PEER_TRACE_EVENT_CAPACITY);
        assert_eq!(
            completed.events.last().unwrap().result_code,
            SteamTransportCloseReason::Requested.diagnostic_code()
        );
        assert!(host.take_completed_peer_trace().is_none());
    }

    #[test]
    fn identityless_transport_fault_finalizes_active_pregame_traces() {
        let (_, mut host, _, connection, _, _, _) =
            quarantined_control_pair(SteamTransportConfig::default());
        assert!(host.peer_trace(connection).is_some());

        assert_eq!(
            host.fail_closed::<()>(SteamTransportError::BackendIntegrityFailure),
            Err(SteamTransportError::BackendIntegrityFailure)
        );
        assert!(host.peer_trace(connection).is_none());
        let completed = host
            .take_completed_peer_trace()
            .expect("faulted active connection retains its bounded trace");
        let terminal = completed.events.last().unwrap();
        assert_eq!(terminal.connection_generation, connection.get());
        assert_eq!(
            terminal.result_code,
            SteamTransportError::BackendIntegrityFailure.diagnostic_code()
        );
        assert!(terminal.phase < SteamPeerSetupPhase::GameplayReady);
        assert!(host.take_completed_peer_trace().is_none());
    }

    #[test]
    fn attributed_malformed_isolation_retains_a_failure_trace() {
        let (_, mut host, _, connection, _, _, client) =
            quarantined_control_pair(SteamTransportConfig::default());
        assert_eq!(
            host.close_connections_for_user_with_reason(
                client,
                SteamTransportCloseReason::MalformedControlTraffic,
            ),
            Ok(1)
        );
        let completed = host
            .take_completed_peer_trace()
            .expect("attributed malformed close retains its setup history");
        assert_eq!(
            completed.events.last().unwrap().result_code,
            SteamTransportCloseReason::MalformedControlTraffic.diagnostic_code()
        );
        assert!(completed.events.last().unwrap().phase < SteamPeerSetupPhase::GameplayReady);
        assert!(host.peer_trace(connection).is_none());
    }

    #[test]
    fn persisted_trace_result_domains_are_globally_unambiguous() {
        let codec_codes = [
            SteamControlCodecError::Empty,
            SteamControlCodecError::Oversized,
            SteamControlCodecError::Truncated,
            SteamControlCodecError::InvalidMagic,
            SteamControlCodecError::UnsupportedVersion,
            SteamControlCodecError::UnknownKind,
            SteamControlCodecError::ReservedBits,
            SteamControlCodecError::InvalidEnvelope,
            SteamControlCodecError::InvalidIdentity,
            SteamControlCodecError::InvalidPayload,
            SteamControlCodecError::GameplayCodec,
        ]
        .map(SteamControlCodecError::diagnostic_code);
        let close_codes = [
            SteamTransportCloseReason::Requested,
            SteamTransportCloseReason::QualityPolicyRejected,
            SteamTransportCloseReason::AdmissionRejected,
            SteamTransportCloseReason::AdmissionTimedOut,
            SteamTransportCloseReason::ConnectTimedOut,
            SteamTransportCloseReason::RemoteClosed,
            SteamTransportCloseReason::LocalProblem,
            SteamTransportCloseReason::EndpointDropped,
            SteamTransportCloseReason::InboundQueueOverflow,
            SteamTransportCloseReason::OversizedDatagram,
            SteamTransportCloseReason::BackendFailure,
            SteamTransportCloseReason::TransportFault,
            SteamTransportCloseReason::MalformedControlTraffic,
            SteamTransportCloseReason::ProtocolViolation,
            SteamTransportCloseReason::AuthenticationRejected,
        ]
        .map(SteamTransportCloseReason::diagnostic_code);
        let transport_codes = [
            SteamTransportError::InvalidConfiguration,
            SteamTransportError::InvalidVirtualPort,
            SteamTransportError::InvalidState,
            SteamTransportError::CapacityExceeded,
            SteamTransportError::AuthorityIdentityMismatch,
            SteamTransportError::AdmissionLobbyMismatch,
            SteamTransportError::AdmissionUserMismatch,
            SteamTransportError::AdmissionAuthorityMismatch,
            SteamTransportError::AdmissionIdentityMismatch,
            SteamTransportError::DuplicateRemoteUser,
            SteamTransportError::UnknownConnection,
            SteamTransportError::EndpointNotReady,
            SteamTransportError::EndpointAlreadyTaken,
            SteamTransportError::TimeRegression,
            SteamTransportError::EventQueueOverflow,
            SteamTransportError::CallbackQueueOverflow,
            SteamTransportError::CallbackOwnerGone,
            SteamTransportError::BackendUnavailable,
            SteamTransportError::BackendOperationFailed,
            SteamTransportError::BackendIntegrityFailure,
            SteamTransportError::HostedDedicatedSdrUnavailable,
            SteamTransportError::Faulted,
            SteamTransportError::ControlQueueOverflow,
            SteamTransportError::IllegalSetupTransition,
        ]
        .map(SteamTransportError::diagnostic_code);
        let lifecycle_codes = [
            STEAM_TRACE_CONNECTION_STARTED,
            STEAM_TRACE_CONNECTION_READY,
            STEAM_TRACE_CONNECTION_RETRY_STARTED,
            STEAM_TRACE_SETUP_PHASE_BASE + 1,
            STEAM_TRACE_SETUP_PHASE_BASE + 2,
            STEAM_TRACE_SETUP_PHASE_BASE + 3,
            STEAM_TRACE_SETUP_PHASE_BASE + 4,
            STEAM_TRACE_SETUP_PHASE_BASE + 5,
            STEAM_TRACE_SETUP_PHASE_BASE + 6,
            STEAM_TRACE_SETUP_PHASE_BASE + 7,
            STEAM_TRACE_MANIFEST_ABORTED,
        ];

        let mut all_codes = Vec::new();
        all_codes.extend(codec_codes);
        all_codes.extend(close_codes);
        all_codes.extend(lifecycle_codes);
        all_codes.extend(transport_codes);
        all_codes.sort_unstable();
        assert!(all_codes.windows(2).all(|pair| pair[0] != pair[1]));
    }

    #[test]
    fn future_piggyback_ack_is_rejected_without_retiring_the_outbox() {
        let (_, mut host, _, connection, lobby, authority, client) =
            quarantined_control_pair(SteamTransportConfig::default());
        assert_eq!(host.outstanding_control_frames(connection), Some(1));
        let malicious = SteamControlEnvelope::message(
            connection.get(),
            1,
            2,
            SteamControlMessage::LinkHello {
                identity: SteamControlIdentity::new(lobby, client, authority).unwrap(),
                lobby_schema: STEAM_LOBBY_SCHEMA_VERSION,
            },
        )
        .unwrap();
        let datagram = encode_steam_control(&malicious).unwrap().into_datagram();
        let record = host
            .connections
            .iter_mut()
            .find(|record| record.id == connection)
            .unwrap();
        let io = record.io.as_mut().unwrap();
        assert_eq!(io.control.highest_transmitted_sequence, 1);
        assert_eq!(
            handle_control_datagram(
                connection,
                client,
                authority,
                lobby,
                io,
                datagram,
                &mut host.metrics,
            ),
            Err(SteamTransportError::IllegalSetupTransition)
        );
        assert_eq!(io.control.outstanding.len(), 1);
        assert_eq!(io.control.highest_acknowledged_sequence, 0);
    }

    #[test]
    fn link_hello_is_legal_only_as_the_generation_binding_sequence_one_frame() {
        let (_, mut host, _, connection, lobby, authority, client) =
            quarantined_control_pair(SteamTransportConfig::default());
        let identity = SteamControlIdentity::new(lobby, client, authority).unwrap();
        let hello = SteamControlEnvelope::message(
            connection.get(),
            1,
            0,
            SteamControlMessage::LinkHello {
                identity,
                lobby_schema: STEAM_LOBBY_SCHEMA_VERSION,
            },
        )
        .unwrap();
        let repeated_hello = SteamControlEnvelope::message(
            connection.get(),
            2,
            0,
            SteamControlMessage::LinkHello {
                identity,
                lobby_schema: STEAM_LOBBY_SCHEMA_VERSION,
            },
        )
        .unwrap();
        let record = host
            .connections
            .iter_mut()
            .find(|record| record.id == connection)
            .unwrap();
        let io = record.io.as_mut().unwrap();
        handle_control_datagram(
            connection,
            client,
            authority,
            lobby,
            io,
            encode_steam_control(&hello).unwrap().into_datagram(),
            &mut host.metrics,
        )
        .unwrap();
        assert_eq!(io.control.remote_generation, Some(connection.get()));
        assert_eq!(io.control.outstanding.len(), 1);

        assert_eq!(
            handle_control_datagram(
                connection,
                client,
                authority,
                lobby,
                io,
                encode_steam_control(&repeated_hello)
                    .unwrap()
                    .into_datagram(),
                &mut host.metrics,
            ),
            Err(SteamTransportError::IllegalSetupTransition)
        );
        assert_eq!(io.control.outstanding.len(), 1);
        assert_eq!(io.control.highest_acknowledged_sequence, 0);
        assert_eq!(io.control.highest_received_sequence, 1);
    }

    #[test]
    fn wrong_generation_identity_or_ack_generation_cannot_mutate_the_outbox() {
        for fault in [0_u8, 1, 2] {
            let (_, mut host, _, connection, lobby, authority, client) =
                quarantined_control_pair(SteamTransportConfig::default());
            let remote_generation = connection.get();
            let hello = SteamControlEnvelope::message(
                remote_generation,
                1,
                0,
                SteamControlMessage::LinkHello {
                    identity: SteamControlIdentity::new(lobby, client, authority).unwrap(),
                    lobby_schema: STEAM_LOBBY_SCHEMA_VERSION,
                },
            )
            .unwrap();
            {
                let record = host
                    .connections
                    .iter_mut()
                    .find(|record| record.id == connection)
                    .unwrap();
                handle_control_datagram(
                    connection,
                    client,
                    authority,
                    lobby,
                    record.io.as_mut().unwrap(),
                    encode_steam_control(&hello).unwrap().into_datagram(),
                    &mut host.metrics,
                )
                .unwrap();
            }
            let ack_identity = if fault == 0 {
                SteamControlIdentity::new(lobby, user(9999), authority).unwrap()
            } else {
                SteamControlIdentity::new(lobby, client, authority).unwrap()
            };
            let ack = SteamControlEnvelope::acknowledgement(
                if fault == 1 {
                    remote_generation + 1
                } else {
                    remote_generation
                },
                if fault == 2 {
                    connection.get() + 1
                } else {
                    connection.get()
                },
                1,
                ack_identity,
            )
            .unwrap();
            let record = host
                .connections
                .iter_mut()
                .find(|record| record.id == connection)
                .unwrap();
            let result = handle_control_datagram(
                connection,
                client,
                authority,
                lobby,
                record.io.as_mut().unwrap(),
                encode_steam_control(&ack).unwrap().into_datagram(),
                &mut host.metrics,
            );
            assert!(matches!(
                result,
                Err(SteamTransportError::IllegalSetupTransition
                    | SteamTransportError::AdmissionIdentityMismatch)
            ));
            assert_eq!(record.io.as_ref().unwrap().control.outstanding.len(), 1);
        }
    }

    #[test]
    fn armed_link_buffers_first_gameplay_until_endpoint_promotion() {
        let (_, mut host, mut client_transport, connection, lobby, authority, client) =
            quarantined_control_pair(SteamTransportConfig::default());
        host.mark_authenticating(connection).unwrap();
        client_transport.mark_authenticating(connection).unwrap();
        host.mark_secure(connection, admission(lobby, client, client))
            .unwrap();
        client_transport
            .mark_secure(connection, admission(lobby, authority, authority))
            .unwrap();
        host.begin_manifest_agreement(connection).unwrap();
        client_transport
            .begin_manifest_agreement(connection)
            .unwrap();
        host.arm_gameplay_receive(connection).unwrap();
        client_transport.arm_gameplay_receive(connection).unwrap();
        client_transport.promote_to_gameplay(connection).unwrap();
        let mut client_endpoint = client_transport.take_endpoint(connection).unwrap().endpoint;
        let first_gameplay = AfcDatagram::try_from_slice(&[0x41, 0x46, 0x43, 0x4e, 7]).unwrap();
        assert_eq!(
            client_endpoint.try_send(first_gameplay.clone()),
            SendOutcome::Sent
        );
        client_transport.pump(NOW_MS + 3).unwrap();
        host.pump(NOW_MS + 3).unwrap();
        assert_eq!(
            host.setup_phase(connection),
            Some(SteamPeerSetupPhase::GameplayReceiveArmed)
        );
        assert!(matches!(
            host.take_endpoint(connection),
            Err(SteamTransportError::EndpointNotReady)
        ));
        host.promote_to_gameplay(connection).unwrap();
        let mut host_endpoint = host.take_endpoint(connection).unwrap().endpoint;
        assert_eq!(
            host_endpoint.try_receive(),
            ReceiveOutcome::Received(first_gameplay)
        );
    }

    #[test]
    fn pre_activation_abort_rolls_armed_receive_back_but_not_gameplay_ready() {
        let (_, mut host, _, connection, lobby, _, client) =
            quarantined_control_pair(SteamTransportConfig::default());
        host.mark_authenticating(connection).unwrap();
        host.mark_secure(connection, admission(lobby, client, client))
            .unwrap();
        host.begin_manifest_agreement(connection).unwrap();
        host.arm_gameplay_receive(connection).unwrap();
        host.abort_manifest_agreement(connection).unwrap();
        assert_eq!(
            host.setup_phase(connection),
            Some(SteamPeerSetupPhase::Secure)
        );
        host.abort_manifest_agreement(connection).unwrap();
        host.begin_manifest_agreement(connection).unwrap();
        host.arm_gameplay_receive(connection).unwrap();
        host.promote_to_gameplay(connection).unwrap();
        assert_eq!(
            host.abort_manifest_agreement(connection),
            Err(SteamTransportError::IllegalSetupTransition)
        );
    }

    #[test]
    fn control_ready_link_has_a_finite_authentication_deadline() {
        let mut config = SteamTransportConfig::default();
        config.connect_timeout_ms = CONTROL_RETRY_MINIMUM_REMAINING_MS;
        let (_network, mut host, _client_transport, connection, _, _, _) =
            quarantined_control_pair(config);
        assert_eq!(
            host.setup_phase(connection),
            Some(SteamPeerSetupPhase::ControlReady)
        );
        host.pump(NOW_MS + 2 + config.connect_timeout_ms).unwrap();
        assert_eq!(host.connection_state(connection), None);
        assert!(matches!(
            host.poll_event(),
            Some(SteamTransportEvent::ConnectionClosed {
                connection: observed,
                reason: SteamTransportCloseReason::ConnectTimedOut,
                ..
            }) if observed == connection
        ));
    }

    #[test]
    fn steam_backend_pause_preserves_the_same_presecure_generation_past_its_wall_clock_deadline() {
        let mut config = SteamTransportConfig::default();
        config.connect_timeout_ms = CONTROL_RETRY_MINIMUM_REMAINING_MS;
        let (_network, mut host, _client_transport, connection, lobby, _, client) =
            quarantined_control_pair(config);
        host.mark_authenticating(connection).unwrap();
        let original_deadline_ms = NOW_MS + 2 + config.connect_timeout_ms;
        let pause_started_at_ms = original_deadline_ms - 1;

        host.pause_pregame_deadlines(pause_started_at_ms).unwrap();
        host.pump(original_deadline_ms + 3_000).unwrap();
        assert_eq!(host.connection_for_user(client), Some(connection));
        assert_eq!(
            host.setup_phase(connection),
            Some(SteamPeerSetupPhase::Authenticating)
        );
        assert!(host.allowed_incoming_users.contains(&Some(client)));

        let recovered_at_ms = original_deadline_ms + 3_999;
        assert_eq!(
            host.resume_pregame_deadlines(recovered_at_ms).unwrap(),
            recovered_at_ms - pause_started_at_ms
        );
        host.pump(recovered_at_ms).unwrap();
        host.mark_secure(connection, admission(lobby, client, client))
            .unwrap();
        assert_eq!(host.connection_for_user(client), Some(connection));
        assert_eq!(
            host.setup_phase(connection),
            Some(SteamPeerSetupPhase::Secure)
        );
    }

    #[test]
    fn early_transient_control_failure_retries_once_after_exact_backoff() {
        let (network, mut host, mut client_transport, first, _, authority, client) =
            quarantined_control_pair(SteamTransportConfig::default());
        network.disconnect_locally(first, client).unwrap();
        host.pump(NOW_MS + 100).unwrap();
        client_transport.pump(NOW_MS + 100).unwrap();
        assert!(matches!(
            host.poll_event(),
            Some(SteamTransportEvent::ControlRetrying {
                connection,
                retry_at_ms,
                ..
            }) if connection == first && retry_at_ms == NOW_MS + 600
        ));
        assert!(matches!(
            client_transport.poll_event(),
            Some(SteamTransportEvent::ControlRetrying {
                connection,
                retry_at_ms,
                ..
            }) if connection == first && retry_at_ms == NOW_MS + 600
        ));

        client_transport.pump(NOW_MS + 599).unwrap();
        assert_eq!(client_transport.connection_count(), 0);
        client_transport.pump(NOW_MS + 600).unwrap();
        host.pump(NOW_MS + 600).unwrap();
        let replacement = match host.poll_event() {
            Some(SteamTransportEvent::IncomingPending {
                connection, user, ..
            }) => {
                assert_eq!(user, client);
                connection
            }
            event => panic!("expected replacement connection, got {event:?}"),
        };
        assert_ne!(replacement, first);
        host.accept_control(replacement, NOW_MS + 600).unwrap();
        host.pump(NOW_MS + 601).unwrap();
        client_transport.pump(NOW_MS + 601).unwrap();
        assert!(matches!(
            host.poll_event(),
            Some(SteamTransportEvent::ControlReady { connection, .. })
                if connection == replacement
        ));
        assert!(matches!(
            client_transport.poll_event(),
            Some(SteamTransportEvent::ControlReady { connection, user, .. })
                if connection == replacement && user == authority
        ));
        assert_eq!(
            client_transport
                .peer_trace(replacement)
                .unwrap()
                .events
                .first()
                .unwrap()
                .result_code,
            503
        );

        network.disconnect_locally(replacement, client).unwrap();
        host.pump(NOW_MS + 700).unwrap();
        client_transport.pump(NOW_MS + 700).unwrap();
        assert!(matches!(
            client_transport.poll_event(),
            Some(SteamTransportEvent::ConnectionClosed {
                connection,
                reason: SteamTransportCloseReason::LocalProblem,
                ..
            }) if connection == replacement
        ));
        assert!(!matches!(
            host.poll_event(),
            Some(SteamTransportEvent::ControlRetrying { .. })
        ));
    }

    #[test]
    fn only_an_exact_typed_reconnect_can_overlap_and_old_drain_cannot_close_replacement() {
        let config = SteamTransportConfig::default();
        let (
            _network,
            mut host,
            mut client,
            old_connection,
            mut old_host_endpoint,
            mut old_client_endpoint,
        ) = connected_pair(config);
        let expected_lobby = lobby(9001);
        let authority = user(1001);
        let remote = user(1002);

        assert_eq!(
            client.connect_p2p(
                reconnect_admission(expected_lobby, authority, authority),
                NOW_MS + 3,
            ),
            Err(SteamTransportError::DuplicateRemoteUser),
            "identity alone must never authorize an overlapping generation"
        );

        host.mark_connection_replacement_eligible(old_connection)
            .unwrap();
        client
            .mark_connection_replacement_eligible(old_connection)
            .unwrap();
        assert_eq!(
            client.connect_p2p(admission(expected_lobby, authority, authority), NOW_MS + 3,),
            Err(SteamTransportError::DuplicateRemoteUser),
            "the exact grant is usable only by Reconnect admission"
        );

        let replacement = client
            .connect_p2p(
                reconnect_admission(expected_lobby, authority, authority),
                NOW_MS + 3,
            )
            .unwrap();
        host.pump(NOW_MS + 4).unwrap();
        assert_eq!(
            host.poll_event(),
            Some(SteamTransportEvent::IncomingPending {
                connection: replacement,
                lobby: expected_lobby,
                user: remote,
                expires_at_ms: NOW_MS + 4 + config.pending_admission_timeout_ms,
            })
        );
        host.admit_incoming(
            replacement,
            reconnect_admission(expected_lobby, remote, remote),
            NOW_MS + 4,
        )
        .unwrap();
        host.pump(NOW_MS + 5).unwrap();
        client.pump(NOW_MS + 5).unwrap();
        assert!(matches!(
            host.poll_event(),
            Some(SteamTransportEvent::ConnectionReady {
                connection,
                ..
            }) if connection == replacement
        ));
        assert!(matches!(
            client.poll_event(),
            Some(SteamTransportEvent::ConnectionReady {
                connection,
                ..
            }) if connection == replacement
        ));
        let mut replacement_host_endpoint = host.take_endpoint(replacement).unwrap().endpoint;
        let mut replacement_client_endpoint = client.take_endpoint(replacement).unwrap().endpoint;

        let final_old_ack = AfcDatagram::try_from_slice(&[0xA1]).unwrap();
        assert_eq!(
            old_host_endpoint.try_send(final_old_ack.clone()),
            SendOutcome::Sent
        );
        drop(old_host_endpoint);
        host.pump(NOW_MS + 6).unwrap();
        client.pump(NOW_MS + 6).unwrap();
        assert_eq!(
            old_client_endpoint.try_receive(),
            ReceiveOutcome::Received(final_old_ack)
        );

        host.pump(NOW_MS + 56).unwrap();
        client.pump(NOW_MS + 56).unwrap();
        assert!(host.connection_state(old_connection).is_none());
        assert!(client.connection_state(old_connection).is_none());
        assert_eq!(
            host.connection_state(replacement),
            Some(SteamTransportConnectionState::Connected)
        );
        assert_eq!(
            client.connection_state(replacement),
            Some(SteamTransportConnectionState::Connected)
        );

        let replacement_payload = AfcDatagram::try_from_slice(&[0xB2]).unwrap();
        assert_eq!(
            replacement_client_endpoint.try_send(replacement_payload.clone()),
            SendOutcome::Sent
        );
        client.pump(NOW_MS + 57).unwrap();
        host.pump(NOW_MS + 57).unwrap();
        assert_eq!(
            replacement_host_endpoint.try_receive(),
            ReceiveOutcome::Received(replacement_payload)
        );
    }

    #[test]
    fn dropping_old_same_identity_backends_cannot_erase_new_listener_or_links() {
        let config = SteamTransportConfig::default();
        let authority = user(1101);
        let remote = user(1102);
        let expected_lobby = lobby(9101);
        let network = FakeSteamTransportNetwork::new(32).unwrap();
        let (host_session, client_session) = sessions(expected_lobby, authority);

        let mut old_host = network
            .create_transport(authority, host_session, config, NOW_MS)
            .unwrap();
        let mut old_client = network
            .create_transport(remote, client_session, config, NOW_MS)
            .unwrap();
        old_host.start_listening().unwrap();
        old_host.set_allowed_incoming_users(&[remote]).unwrap();
        let old_connection = old_client
            .connect_p2p(admission(expected_lobby, authority, authority), NOW_MS)
            .unwrap();
        old_host.pump(NOW_MS + 1).unwrap();
        let _ = old_host.poll_event();
        old_host
            .admit_incoming(
                old_connection,
                admission(expected_lobby, remote, remote),
                NOW_MS + 1,
            )
            .unwrap();
        old_host.pump(NOW_MS + 2).unwrap();
        old_client.pump(NOW_MS + 2).unwrap();
        let _ = old_host.poll_event();
        let _ = old_client.poll_event();
        let old_host_endpoint = old_host.take_endpoint(old_connection).unwrap().endpoint;
        let old_client_endpoint = old_client.take_endpoint(old_connection).unwrap().endpoint;

        assert_eq!(
            old_host.begin_retirement(NOW_MS + 3),
            SteamTransportRetirementStatus::Draining
        );
        assert_eq!(
            old_client.begin_retirement(NOW_MS + 3),
            SteamTransportRetirementStatus::Draining
        );

        let mut new_host = network
            .create_transport(authority, host_session, config, NOW_MS + 3)
            .unwrap();
        let mut new_client = network
            .create_transport(remote, client_session, config, NOW_MS + 3)
            .unwrap();
        new_host.start_listening().unwrap();
        new_host.set_allowed_incoming_users(&[remote]).unwrap();
        let new_connection = new_client
            .connect_p2p(admission(expected_lobby, authority, authority), NOW_MS + 3)
            .unwrap();
        new_host.pump(NOW_MS + 4).unwrap();
        let _ = new_host.poll_event();
        new_host
            .admit_incoming(
                new_connection,
                admission(expected_lobby, remote, remote),
                NOW_MS + 4,
            )
            .unwrap();
        new_host.pump(NOW_MS + 5).unwrap();
        new_client.pump(NOW_MS + 5).unwrap();
        let _ = new_host.poll_event();
        let _ = new_client.poll_event();
        let mut new_host_endpoint = new_host.take_endpoint(new_connection).unwrap().endpoint;
        let mut new_client_endpoint = new_client.take_endpoint(new_connection).unwrap().endpoint;

        drop(old_host_endpoint);
        drop(old_client_endpoint);
        drop(old_host);
        drop(old_client);

        let payload = AfcDatagram::try_from_slice(&[0xC3]).unwrap();
        assert_eq!(
            new_client_endpoint.try_send(payload.clone()),
            SendOutcome::Sent
        );
        new_client.pump(NOW_MS + 6).unwrap();
        new_host.pump(NOW_MS + 6).unwrap();
        assert_eq!(
            new_host_endpoint.try_receive(),
            ReceiveOutcome::Received(payload)
        );
        assert_eq!(
            new_host.connection_state(new_connection),
            Some(SteamTransportConnectionState::Connected)
        );
    }

    #[test]
    fn configuration_and_hosted_dedicated_boundary_are_explicit() {
        let authority = user(1);
        let lobby = lobby(2);
        let network = FakeSteamTransportNetwork::new(8).unwrap();
        let (host_session, _) = sessions(lobby, authority);
        let mut transport = network
            .create_transport(
                authority,
                host_session,
                SteamTransportConfig::default(),
                NOW_MS,
            )
            .unwrap();
        assert_eq!(
            transport.hosted_dedicated_sdr_support(),
            DedicatedSdrSupport::UnavailableInPinnedBinding
        );
        assert_eq!(
            transport.open_hosted_dedicated_listener(),
            Err(SteamTransportError::HostedDedicatedSdrUnavailable)
        );

        let invalid = SteamP2pSession {
            virtual_port: MAX_STEAM_VIRTUAL_PORT + 1,
            ..host_session
        };
        assert!(matches!(
            network.create_transport(user(3), invalid, SteamTransportConfig::default(), NOW_MS),
            Err(SteamTransportError::InvalidVirtualPort)
                | Err(SteamTransportError::AuthorityIdentityMismatch)
        ));
    }

    #[test]
    fn incoming_connection_is_not_accepted_before_exact_steam_admission() {
        let authority = user(11);
        let client = user(12);
        let expected_lobby = lobby(13);
        let network = FakeSteamTransportNetwork::new(8).unwrap();
        let (host_session, client_session) = sessions(expected_lobby, authority);
        let mut host = network
            .create_transport(
                authority,
                host_session,
                SteamTransportConfig::default(),
                NOW_MS,
            )
            .unwrap();
        let mut client_transport = network
            .create_transport(
                client,
                client_session,
                SteamTransportConfig::default(),
                NOW_MS,
            )
            .unwrap();
        host.start_listening().unwrap();
        host.set_allowed_incoming_users(&[client]).unwrap();
        let connection = client_transport
            .connect_p2p(admission(expected_lobby, authority, authority), NOW_MS)
            .unwrap();
        host.pump(NOW_MS + 1).unwrap();
        assert!(!network.connection_was_accepted(connection).unwrap());
        assert_eq!(
            host.connection_state(connection),
            Some(SteamTransportConnectionState::PendingAdmission)
        );

        let wrong_lobby = admission(lobby(14), client, client);
        assert_eq!(
            host.admit_incoming(connection, wrong_lobby, NOW_MS + 1),
            Err(SteamTransportError::AdmissionLobbyMismatch)
        );
        assert!(!network.connection_was_accepted(connection).unwrap());

        let forged_identity = AuthenticatedSteamPeer {
            authenticated_user: AuthenticatedUserId::new(999).unwrap(),
            ..admission(expected_lobby, client, client)
        };
        assert_eq!(
            host.admit_incoming(connection, forged_identity, NOW_MS + 1),
            Err(SteamTransportError::AdmissionIdentityMismatch)
        );
        assert!(!network.connection_was_accepted(connection).unwrap());

        host.admit_incoming(
            connection,
            admission(expected_lobby, client, client),
            NOW_MS + 1,
        )
        .unwrap();
        assert!(network.connection_was_accepted(connection).unwrap());
    }

    #[test]
    fn transport_validates_authority_independently_of_steam_license_owner() {
        let authority = user(15);
        let client = user(16);
        let family_license_owner = user(17);
        let expected_lobby = lobby(18);
        let network = FakeSteamTransportNetwork::new(8).unwrap();
        let (host_session, client_session) = sessions(expected_lobby, authority);
        let mut host = network
            .create_transport(
                authority,
                host_session,
                SteamTransportConfig::default(),
                NOW_MS,
            )
            .unwrap();
        let mut client_transport = network
            .create_transport(
                client,
                client_session,
                SteamTransportConfig::default(),
                NOW_MS,
            )
            .unwrap();
        host.start_listening().unwrap();
        host.set_allowed_incoming_users(&[client]).unwrap();

        // The client's transport admission names the lobby authority as its
        // remote ticket provider. A distinct Steam Families license owner is
        // valid and has no authority semantics.
        let connection = client_transport
            .connect_p2p(
                admission(expected_lobby, authority, family_license_owner),
                NOW_MS,
            )
            .unwrap();
        host.pump(NOW_MS + 1).unwrap();
        host.admit_incoming(
            connection,
            admission(expected_lobby, client, family_license_owner),
            NOW_MS + 1,
        )
        .unwrap();
        assert!(network.connection_was_accepted(connection).unwrap());

        let wrong_client_remote =
            admission(expected_lobby, family_license_owner, family_license_owner);
        assert_eq!(
            client_transport.validate_admission(wrong_client_remote, family_license_owner),
            Err(SteamTransportError::AdmissionAuthorityMismatch)
        );
    }

    #[test]
    fn pending_admission_times_out_without_auto_accepting() {
        let authority = user(21);
        let client = user(22);
        let expected_lobby = lobby(23);
        let network = FakeSteamTransportNetwork::new(8).unwrap();
        let config = SteamTransportConfig {
            pending_admission_timeout_ms: 5,
            ..SteamTransportConfig::default()
        };
        let (host_session, client_session) = sessions(expected_lobby, authority);
        let mut host = network
            .create_transport(authority, host_session, config, NOW_MS)
            .unwrap();
        let mut client_transport = network
            .create_transport(client, client_session, config, NOW_MS)
            .unwrap();
        host.start_listening().unwrap();
        host.set_allowed_incoming_users(&[client]).unwrap();
        let connection = client_transport
            .connect_p2p(admission(expected_lobby, authority, authority), NOW_MS)
            .unwrap();
        host.pump(NOW_MS + 1).unwrap();
        let _ = host.poll_event();
        host.pump(NOW_MS + 6).unwrap();
        assert!(host.connection_state(connection).is_none());
        assert_eq!(
            host.poll_event(),
            Some(SteamTransportEvent::ConnectionClosed {
                connection,
                lobby: expected_lobby,
                user: client,
                reason: SteamTransportCloseReason::AdmissionTimedOut,
            })
        );
        assert!(!network.connection_was_accepted(connection).unwrap());
    }

    #[test]
    fn endpoint_queues_are_bounded_and_inbound_overflow_closes_connection() {
        let config = SteamTransportConfig {
            endpoint_queue_packets: 1,
            ..SteamTransportConfig::default()
        };
        let (_network, mut host, mut client, connection, mut host_endpoint, mut client_endpoint) =
            connected_pair(config);
        let first = AfcDatagram::try_from_slice(&[1]).unwrap();
        let second = AfcDatagram::try_from_slice(&[2]).unwrap();
        assert_eq!(client_endpoint.try_send(first), SendOutcome::Sent);
        assert_eq!(
            client_endpoint.try_send(second.clone()),
            SendOutcome::Full(second.clone())
        );
        client.pump(NOW_MS + 3).unwrap();
        assert_eq!(client_endpoint.try_send(second), SendOutcome::Sent);
        client.pump(NOW_MS + 4).unwrap();

        host.pump(NOW_MS + 4).unwrap();
        assert!(host.connection_state(connection).is_none());
        assert_eq!(
            host.poll_event(),
            Some(SteamTransportEvent::ConnectionClosed {
                connection,
                lobby: lobby(9001),
                user: user(1002),
                reason: SteamTransportCloseReason::InboundQueueOverflow,
            })
        );
        assert!(matches!(
            host_endpoint.try_receive(),
            ReceiveOutcome::Received(_)
        ));
        assert_eq!(host_endpoint.try_receive(), ReceiveOutcome::Disconnected);
    }

    #[test]
    fn endpoint_drop_closes_only_after_the_fixed_quiet_window() {
        let (_network, mut host, mut client, connection, host_endpoint, mut client_endpoint) =
            connected_pair(SteamTransportConfig::default());
        drop(host_endpoint);
        host.pump(NOW_MS + 3).unwrap();
        assert_eq!(
            host.connection_state(connection),
            Some(SteamTransportConnectionState::Connected)
        );
        assert!(host.poll_event().is_none());
        host.pump(NOW_MS + 52).unwrap();
        assert_eq!(
            host.connection_state(connection),
            Some(SteamTransportConnectionState::Connected)
        );
        assert!(host.poll_event().is_none());
        host.pump(NOW_MS + 53).unwrap();
        assert_eq!(
            host.poll_event(),
            Some(SteamTransportEvent::ConnectionClosed {
                connection,
                lobby: lobby(9001),
                user: user(1002),
                reason: SteamTransportCloseReason::EndpointDropped,
            })
        );
        assert_eq!(host.metrics().endpoint_drop_drains_started, 1);
        assert_eq!(host.metrics().endpoint_drop_drains_quiet_completed, 1);
        assert_eq!(host.metrics().endpoint_drop_drain_timeouts, 0);
        client.pump(NOW_MS + 53).unwrap();
        assert!(client.connection_state(connection).is_none());
        assert_eq!(client_endpoint.try_receive(), ReceiveOutcome::Disconnected);
    }

    #[test]
    fn queued_ack_like_datagram_is_submitted_after_endpoint_drop() {
        let (_network, mut host, mut client, connection, mut host_endpoint, mut client_endpoint) =
            connected_pair(SteamTransportConfig::default());
        let ack = AfcDatagram::try_from_slice(&[0xAC, 0x4B]).unwrap();
        assert_eq!(host_endpoint.try_send(ack.clone()), SendOutcome::Sent);
        drop(host_endpoint);

        host.pump(NOW_MS + 3).unwrap();
        assert_eq!(
            host.connection_state(connection),
            Some(SteamTransportConnectionState::Connected)
        );
        client.pump(NOW_MS + 3).unwrap();
        assert_eq!(client_endpoint.try_receive(), ReceiveOutcome::Received(ack));
        assert_eq!(host.metrics().sent_datagrams, 1);
        assert_eq!(host.metrics().endpoint_drop_drains_started, 1);

        host.pump(NOW_MS + 53).unwrap();
        assert!(host.connection_state(connection).is_none());
        assert_eq!(host.metrics().endpoint_drop_drains_quiet_completed, 1);
    }

    #[test]
    fn transport_retirement_delivers_final_ack_without_receiving_gameplay() {
        assert_eq!(
            STEAM_TRANSPORT_RETIREMENT_HARD_TIMEOUT_MS,
            ENDPOINT_DROP_DRAIN_HARD_TIMEOUT_MS + ENDPOINT_DROP_DRAIN_QUIET_MS
        );
        let (network, mut host, mut client, connection, mut host_endpoint, mut client_endpoint) =
            connected_pair(SteamTransportConfig::default());

        let stale_gameplay = AfcDatagram::try_from_slice(&[0x55]).unwrap();
        assert_eq!(host_endpoint.try_send(stale_gameplay), SendOutcome::Sent);
        host.pump(NOW_MS + 3).unwrap();
        assert_eq!(
            network
                .shared
                .lock()
                .unwrap()
                .links
                .get(&connection)
                .unwrap()
                .to_client
                .len(),
            1
        );

        // This models the client protocol pump accepting a typed Disconnect,
        // synchronously queueing its exact ACK, and only then the application
        // transitioning away and dropping the worker endpoint.
        let ack = AfcDatagram::try_from_slice(&[0xAC, 0x4B]).unwrap();
        assert_eq!(client_endpoint.try_send(ack.clone()), SendOutcome::Sent);
        assert_eq!(
            client.begin_retirement(NOW_MS + 3),
            SteamTransportRetirementStatus::Draining
        );
        assert!(!client.is_listening());
        assert_eq!(client.poll_event(), None);
        assert_eq!(client_endpoint.try_receive(), ReceiveOutcome::Disconnected);
        drop(client_endpoint);

        assert_eq!(
            client.pump_retirement(NOW_MS + 4),
            SteamTransportRetirementStatus::Draining
        );
        assert_eq!(client.metrics().received_datagrams, 0);
        assert_eq!(
            network
                .shared
                .lock()
                .unwrap()
                .links
                .get(&connection)
                .unwrap()
                .to_client
                .len(),
            1,
            "retirement must not consume receive-side gameplay"
        );
        host.pump(NOW_MS + 4).unwrap();
        assert_eq!(host_endpoint.try_receive(), ReceiveOutcome::Received(ack));

        assert_eq!(
            client.pump_retirement(NOW_MS + 53),
            SteamTransportRetirementStatus::Draining
        );
        assert_eq!(
            client.pump_retirement(NOW_MS + 54),
            SteamTransportRetirementStatus::Complete
        );
        assert_eq!(
            client.retirement_status(),
            Some(SteamTransportRetirementStatus::Complete)
        );
        assert_eq!(client.connection_count(), 0);
        assert_eq!(client.metrics().retirements_started, 1);
        assert_eq!(client.metrics().retirements_completed, 1);
        assert_eq!(client.metrics().retirement_timeouts, 0);
        assert_eq!(client.metrics().retirement_faults, 0);

        // Terminal status and counters are sticky; normal gameplay/admission
        // APIs cannot reactivate this generation.
        assert_eq!(
            client.pump_retirement(NOW_MS + 100),
            SteamTransportRetirementStatus::Complete
        );
        assert_eq!(client.metrics().retirements_completed, 1);
        assert_eq!(
            client.set_allowed_incoming_users(&[]),
            Err(SteamTransportError::InvalidState)
        );
    }

    #[test]
    fn retirement_services_outbound_budget_before_exact_hard_timeout() {
        let (network, mut host, mut client, connection, mut host_endpoint, mut client_endpoint) =
            connected_pair_with_wire_capacity(SteamTransportConfig::default(), 1);
        let blocker = AfcDatagram::try_from_slice(&[1]).unwrap();
        let final_ack = AfcDatagram::try_from_slice(&[2]).unwrap();
        assert_eq!(client_endpoint.try_send(blocker.clone()), SendOutcome::Sent);
        client.pump(NOW_MS + 3).unwrap();
        assert_eq!(
            client_endpoint.try_send(final_ack.clone()),
            SendOutcome::Sent
        );
        let retirement_started = NOW_MS + 4;
        assert_eq!(
            client.begin_retirement(retirement_started),
            SteamTransportRetirementStatus::Draining
        );
        drop(client_endpoint);

        // Free the one-packet fake wire only on the exact endpoint hard
        // deadline. The retirement pump must send before classifying timeout.
        host.pump(retirement_started + ENDPOINT_DROP_DRAIN_HARD_TIMEOUT_MS)
            .unwrap();
        assert_eq!(
            host_endpoint.try_receive(),
            ReceiveOutcome::Received(blocker)
        );
        assert_eq!(
            client.pump_retirement(retirement_started + ENDPOINT_DROP_DRAIN_HARD_TIMEOUT_MS),
            SteamTransportRetirementStatus::TimedOut
        );
        assert_eq!(
            network
                .shared
                .lock()
                .unwrap()
                .links
                .get(&connection)
                .unwrap()
                .to_host
                .front(),
            Some(&final_ack),
            "the exact deadline classifies timeout only after backend submission"
        );
        assert_eq!(client.metrics().retirements_completed, 0);
        assert_eq!(client.metrics().retirement_timeouts, 1);
        assert_eq!(client.metrics().retirement_faults, 0);
    }

    #[test]
    fn retirement_fault_is_exact_sticky_and_closes_once() {
        let (network, _host, mut client, connection, _host_endpoint, mut client_endpoint) =
            connected_pair(SteamTransportConfig::default());
        assert_eq!(
            client_endpoint.try_send(AfcDatagram::try_from_slice(&[9]).unwrap()),
            SendOutcome::Sent
        );
        assert_eq!(
            client.begin_retirement(NOW_MS + 3),
            SteamTransportRetirementStatus::Draining
        );
        network
            .inject_send_failure(
                connection,
                user(1002),
                SteamTransportError::BackendOperationFailed,
            )
            .unwrap();
        assert_eq!(
            client.pump_retirement(NOW_MS + 4),
            SteamTransportRetirementStatus::Faulted(SteamTransportError::BackendOperationFailed)
        );
        assert_eq!(
            client.last_fault(),
            Some(SteamTransportError::BackendOperationFailed)
        );
        assert_eq!(client.connection_count(), 0);
        assert_eq!(client_endpoint.try_receive(), ReceiveOutcome::Disconnected);
        assert_eq!(client.metrics().retirements_started, 1);
        assert_eq!(client.metrics().retirements_completed, 0);
        assert_eq!(client.metrics().retirement_timeouts, 0);
        assert_eq!(client.metrics().retirement_faults, 1);
        assert_eq!(
            client.pump_retirement(NOW_MS + 5),
            SteamTransportRetirementStatus::Faulted(SteamTransportError::BackendOperationFailed)
        );
        assert_eq!(client.metrics().retirement_faults, 1);
    }

    #[test]
    fn endpoint_drop_drain_never_receives_backend_datagrams() {
        let (network, mut host, mut client, connection, host_endpoint, mut client_endpoint) =
            connected_pair(SteamTransportConfig::default());
        drop(host_endpoint);
        host.pump(NOW_MS + 3).unwrap();

        let inbound = AfcDatagram::try_from_slice(&[7, 8, 9]).unwrap();
        assert_eq!(client_endpoint.try_send(inbound), SendOutcome::Sent);
        client.pump(NOW_MS + 4).unwrap();
        let queued_before = network
            .shared
            .lock()
            .unwrap()
            .links
            .get(&connection)
            .unwrap()
            .to_host
            .len();
        assert_eq!(queued_before, 1);

        host.pump(NOW_MS + 4).unwrap();
        let queued_after = network
            .shared
            .lock()
            .unwrap()
            .links
            .get(&connection)
            .unwrap()
            .to_host
            .len();
        assert_eq!(queued_after, queued_before);
        assert_eq!(host.metrics().received_datagrams, 0);
    }

    #[test]
    fn permanent_send_backpressure_closes_at_the_endpoint_drop_hard_cap() {
        let (_network, mut host, _client, connection, mut host_endpoint, _client_endpoint) =
            connected_pair_with_wire_capacity(SteamTransportConfig::default(), 1);
        let blocker = AfcDatagram::try_from_slice(&[1]).unwrap();
        let pending = AfcDatagram::try_from_slice(&[2]).unwrap();
        assert_eq!(host_endpoint.try_send(blocker), SendOutcome::Sent);
        host.pump(NOW_MS + 3).unwrap();
        assert_eq!(host_endpoint.try_send(pending), SendOutcome::Sent);
        drop(host_endpoint);

        let drain_started = NOW_MS + 4;
        host.pump(drain_started).unwrap();
        assert_eq!(
            host.connection_state(connection),
            Some(SteamTransportConnectionState::Connected)
        );
        host.pump(drain_started + ENDPOINT_DROP_DRAIN_HARD_TIMEOUT_MS - 1)
            .unwrap();
        assert_eq!(
            host.connection_state(connection),
            Some(SteamTransportConnectionState::Connected)
        );
        assert_eq!(host.metrics().endpoint_drop_drain_timeouts, 0);

        host.pump(drain_started + ENDPOINT_DROP_DRAIN_HARD_TIMEOUT_MS)
            .unwrap();
        assert!(host.connection_state(connection).is_none());
        assert_eq!(
            host.poll_event(),
            Some(SteamTransportEvent::ConnectionClosed {
                connection,
                lobby: lobby(9001),
                user: user(1002),
                reason: SteamTransportCloseReason::EndpointDropped,
            })
        );
        assert_eq!(host.metrics().endpoint_drop_drains_started, 1);
        assert_eq!(host.metrics().endpoint_drop_drain_timeouts, 1);
        assert_eq!(host.metrics().endpoint_drop_drains_quiet_completed, 0);
        assert!(host.metrics().send_would_block >= 1);
    }

    #[test]
    fn backend_disconnect_during_endpoint_drain_closes_immediately() {
        let (network, mut host, _client, connection, host_endpoint, _client_endpoint) =
            connected_pair(SteamTransportConfig::default());
        drop(host_endpoint);
        host.pump(NOW_MS + 3).unwrap();
        network.disconnect_locally(connection, user(1001)).unwrap();

        host.pump(NOW_MS + 4).unwrap();

        assert!(host.connection_state(connection).is_none());
        assert_eq!(
            host.poll_event(),
            Some(SteamTransportEvent::ConnectionClosed {
                connection,
                lobby: lobby(9001),
                user: user(1002),
                reason: SteamTransportCloseReason::LocalProblem,
            })
        );
        assert_eq!(host.metrics().endpoint_drop_drains_started, 1);
        assert_eq!(host.metrics().endpoint_drop_drains_quiet_completed, 0);
        assert_eq!(host.metrics().endpoint_drop_drain_timeouts, 0);
    }

    #[test]
    fn attributed_user_close_removes_native_link_and_disconnects_only_that_endpoint() {
        let (_network, mut host, mut client, connection, mut host_endpoint, mut client_endpoint) =
            connected_pair(SteamTransportConfig::default());

        assert_eq!(
            host.close_connections_for_user_with_reason(
                user(1002),
                SteamTransportCloseReason::Requested,
            )
            .unwrap(),
            1
        );
        assert!(host.connection_state(connection).is_none());
        assert_eq!(
            host.poll_event(),
            Some(SteamTransportEvent::ConnectionClosed {
                connection,
                lobby: lobby(9001),
                user: user(1002),
                reason: SteamTransportCloseReason::Requested,
            })
        );
        assert_eq!(host_endpoint.try_receive(), ReceiveOutcome::Disconnected);

        client.pump(NOW_MS + 3).unwrap();
        assert!(client.connection_state(connection).is_none());
        assert_eq!(client_endpoint.try_receive(), ReceiveOutcome::Disconnected);
        assert_eq!(
            host.close_connections_for_user_with_reason(
                user(1999),
                SteamTransportCloseReason::Requested,
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn quality_policy_close_has_an_explicit_attributable_reason() {
        let (_network, mut host, mut client, connection, mut host_endpoint, mut client_endpoint) =
            connected_pair(SteamTransportConfig::default());

        host.close_connection_for_quality_policy(connection)
            .unwrap();
        assert!(host.connection_state(connection).is_none());
        assert_eq!(
            host.poll_event(),
            Some(SteamTransportEvent::ConnectionClosed {
                connection,
                lobby: lobby(9001),
                user: user(1002),
                reason: SteamTransportCloseReason::QualityPolicyRejected,
            })
        );
        assert_eq!(host_endpoint.try_receive(), ReceiveOutcome::Disconnected);

        client.pump(NOW_MS + 3).unwrap();
        assert!(client.connection_state(connection).is_none());
        assert_eq!(client_endpoint.try_receive(), ReceiveOutcome::Disconnected);
    }

    #[test]
    fn relay_and_quality_metrics_are_observable_without_owning_callbacks() {
        let (network, mut host, _client, connection, _host_endpoint, _client_endpoint) =
            connected_pair(SteamTransportConfig::default());
        let degraded = SteamRelayStatus {
            availability: SteamRelayAvailability::Retrying,
            network_config: SteamRelayAvailability::Current,
            any_relay: SteamRelayAvailability::PreviouslyAvailable,
            ping_measurement_in_progress: true,
        };
        network.set_relay_status(user(1001), degraded).unwrap();
        let quality = SteamConnectionQuality {
            ping_ms: Some(87),
            local_delivery_permyriad: Some(9_700),
            remote_delivery_permyriad: Some(9_500),
            ..SteamConnectionQuality::default()
        };
        network
            .set_connection_quality(connection, user(1001), quality)
            .unwrap();
        host.pump(NOW_MS + 3).unwrap();
        assert_eq!(host.relay_status(), degraded);
        assert_eq!(
            host.poll_event(),
            Some(SteamTransportEvent::RelayStatusChanged(degraded))
        );
        assert_eq!(host.connection_quality(connection).unwrap(), quality);
    }

    #[test]
    fn callback_overflow_faults_closed_and_disconnects_endpoint() {
        let (network, mut host, _client, connection, mut host_endpoint, _client_endpoint) =
            connected_pair(SteamTransportConfig::default());
        network.inject_callback_overflow(user(1001)).unwrap();
        assert_eq!(
            host.pump(NOW_MS + 3),
            Err(SteamTransportError::CallbackQueueOverflow)
        );
        assert_eq!(
            host.last_fault(),
            Some(SteamTransportError::CallbackQueueOverflow)
        );
        assert!(host.connection_state(connection).is_none());
        assert_eq!(host_endpoint.try_receive(), ReceiveOutcome::Disconnected);
    }

    #[test]
    fn outsider_callback_pressure_is_bounded_without_starving_an_allowed_peer() {
        let authority = user(2_001);
        let allowed_user = user(2_002);
        let outsiders = [user(2_003), user(2_004), user(2_005)];
        let expected_lobby = lobby(9_002);
        let config = SteamTransportConfig {
            event_capacity: 4,
            max_callbacks_per_pump: 2,
            ..SteamTransportConfig::default()
        };
        let network = FakeSteamTransportNetwork::new(16).unwrap();
        let (host_session, client_session) = sessions(expected_lobby, authority);
        let mut host = network
            .create_transport(authority, host_session, config, NOW_MS)
            .unwrap();
        host.start_listening().unwrap();
        host.set_allowed_incoming_users(&[allowed_user]).unwrap();

        let mut outsider_transports = Vec::new();
        for outsider in outsiders {
            let mut transport = network
                .create_transport(outsider, client_session, config, NOW_MS)
                .unwrap();
            transport
                .connect_p2p(admission(expected_lobby, authority, authority), NOW_MS)
                .unwrap();
            outsider_transports.push(transport);
        }
        let mut allowed_transport = network
            .create_transport(allowed_user, client_session, config, NOW_MS)
            .unwrap();
        let allowed_connection = allowed_transport
            .connect_p2p(admission(expected_lobby, authority, authority), NOW_MS)
            .unwrap();

        host.pump(NOW_MS + 1).unwrap();
        assert!(host.is_listening());
        assert!(!host.is_faulted());
        assert_eq!(host.connection_count(), 0);
        assert_eq!(host.metrics().rejected_connections, 2);
        assert_eq!(host.poll_event(), None);

        host.pump(NOW_MS + 2).unwrap();
        assert!(host.is_listening());
        assert!(!host.is_faulted());
        assert_eq!(host.connection_count(), 1);
        assert_eq!(host.metrics().rejected_connections, 3);
        assert_eq!(
            host.poll_event(),
            Some(SteamTransportEvent::IncomingPending {
                connection: allowed_connection,
                lobby: expected_lobby,
                user: allowed_user,
                expires_at_ms: NOW_MS + 2 + config.pending_admission_timeout_ms,
            })
        );
        assert_eq!(host.poll_event(), None);

        host.admit_incoming(
            allowed_connection,
            admission(expected_lobby, allowed_user, allowed_user),
            NOW_MS + 2,
        )
        .unwrap();
        host.pump(NOW_MS + 3).unwrap();
        allowed_transport.pump(NOW_MS + 3).unwrap();
        assert_eq!(
            host.poll_event(),
            Some(SteamTransportEvent::ConnectionReady {
                connection: allowed_connection,
                lobby: expected_lobby,
                user: allowed_user,
            })
        );
        assert!(!host.is_faulted());
        drop(outsider_transports);
    }

    #[test]
    fn fake_steam_endpoint_drives_network_runtime_end_to_end() {
        let (_network, mut host, mut client, _connection, host_endpoint, client_endpoint) =
            connected_pair(SteamTransportConfig::default());
        let compatibility = current_compatibility();
        let mut client_runtime = NetworkRuntime::new(
            client_endpoint,
            PeerRole::Client,
            compatibility,
            RuntimeConfig::default(),
        )
        .unwrap();
        let mut authority_runtime = NetworkRuntime::new(
            host_endpoint,
            PeerRole::Authority,
            compatibility,
            RuntimeConfig::default(),
        )
        .unwrap();
        let handshake = WireMessage::Handshake(Handshake { compatibility });
        client_runtime.queue_message(handshake.clone()).unwrap();
        client_runtime.pump(SimTick(1));
        client.pump(NOW_MS + 3).unwrap();
        host.pump(NOW_MS + 3).unwrap();
        authority_runtime.pump(SimTick(1));
        assert_eq!(
            authority_runtime.try_next_event(),
            Some(RuntimeEvent::Message(handshake))
        );
    }

    #[test]
    fn retired_transport_delivers_exact_disconnect_ack_after_client_worker_drop() {
        let (_network, mut host, mut client, _connection, host_endpoint, client_endpoint) =
            connected_pair(SteamTransportConfig::default());
        let compatibility = current_compatibility();
        let mut authority_runtime = NetworkRuntime::new(
            host_endpoint,
            PeerRole::Authority,
            compatibility,
            RuntimeConfig::default(),
        )
        .unwrap();
        let mut client_runtime = NetworkRuntime::new(
            client_endpoint,
            PeerRole::Client,
            compatibility,
            RuntimeConfig::default(),
        )
        .unwrap();
        let disconnect = DisconnectMessage {
            match_id: Some(MatchId::new(*b"retire-ack-race1").unwrap()),
            code: DisconnectCode::Kicked,
            retry: RetryDisposition::Fatal,
            detail_code: 77,
            last_confirmed_tick: Some(SimTick(40)),
        };
        let tracked = authority_runtime
            .queue_tracked_disconnect(disconnect)
            .unwrap();

        // Authority transport pump delivers the typed terminal. The client
        // protocol pump validates it and queues its reliable ACK before
        // publishing the event to the application.
        authority_runtime.pump(SimTick(41));
        host.pump(NOW_MS + 3).unwrap();
        client.pump(NOW_MS + 3).unwrap();
        client_runtime.pump(SimTick(41));
        assert_eq!(
            client_runtime.try_next_event(),
            Some(RuntimeEvent::Message(WireMessage::Disconnect(disconnect)))
        );
        assert_eq!(
            authority_runtime.reliable_send_status(tracked),
            ReliableSendStatus::Pending
        );

        // The application applies the terminal, moves the active transport to
        // retirement, and synchronously clears the client worker before the
        // next native transport frame.
        assert_eq!(
            client.begin_retirement(NOW_MS + 3),
            SteamTransportRetirementStatus::Draining
        );
        drop(client_runtime);

        // Only the retired transport remains. Its next outbound-only pump
        // submits the already-queued exact ACK to the still-live authority.
        assert_eq!(
            client.pump_retirement(NOW_MS + 4),
            SteamTransportRetirementStatus::Draining
        );
        host.pump(NOW_MS + 4).unwrap();
        authority_runtime.pump(SimTick(42));
        assert_eq!(
            authority_runtime.reliable_send_status(tracked),
            ReliableSendStatus::Acknowledged
        );
    }
}
