//! Production Steam lobby/application orchestration.
//!
//! This module owns no rendering and no gameplay simulation. It is the bounded
//! application seam between [`crate::steam_platform`], authenticated
//! [`crate::online_roster`] declarations, [`crate::steam_transport`], and the
//! session/bootstrap code that consumes admitted datagram endpoints.
//!
//! The coordinator deliberately keeps Steam callback pumping in one place:
//! [`OnlineLobbyCoordinator::pump`] always pumps the platform first, consumes
//! authentication/lobby events, then pumps the gameplay transport. UI code sends
//! commands through the methods on the coordinator and reads [`OnlineLobbyStatus`]
//! plus a bounded [`OnlineLobbyEvent`] stream.

use core::fmt;
use std::collections::VecDeque;

use crate::headless::HeadlessMatchConfig;
use crate::network_protocol::{
    AuthorityKind, DefinitionId, DisconnectMessage, MAX_FIGHTERS, ManifestHash, MatchId,
    MatchManifest, PeerId, ProtocolValidationError, RetryDisposition, SeatOwner,
};
use crate::network_quality::{
    InputDelayCalibrationSnapshot, InputDelayCalibrationState, MAX_CALIBRATED_ROLLBACK_TICKS,
    NetworkQuality, NetworkQualityError, NetworkQualityMonitor, NetworkQualityPolicy,
    NetworkQualitySample, NetworkQualitySnapshot, PrecommitRttCalibrator, calibrated_input_delay,
};
use crate::online_failure::{
    OnlineFailure, OnlineFailureCode, OnlineFailureSeverity, OnlineRecoveryAction,
};
use crate::online_roster::{
    FirstReleaseOnlinePolicy, OnlineManifestOptions, OnlineRoster, OnlineRosterError,
    OnlineRosterMember, OnlineSeatSelection, decode_member_declaration, encode_member_declaration,
};
use crate::simulation::SimTick;
use crate::steam_platform::{
    AdmissionPurpose, AuthTicketHandle, AuthenticatedSteamPeer, IssuedAuthTicket, LobbyExitReason,
    LobbyJoinIntent, LobbyMetadata, LobbyVisibility, MAX_STEAM_LOBBY_MEMBERS, MemberDataOutcome,
    MemberDeclarationRejection, MemberLoadoutDeclaration, MemberReadiness, SteamBackend,
    SteamLobbyId, SteamOverlayRequestStatus, SteamPlatform, SteamPlatformError, SteamPlatformEvent,
    SteamPlatformState, SteamUserId,
};
use crate::steam_transport::{
    AdmittedSteamEndpoint, SteamConnectionId, SteamConnectionQuality, SteamP2pSession,
    SteamRelayStatus, SteamTransport, SteamTransportCloseReason, SteamTransportConfig,
    SteamTransportError, SteamTransportEvent, SteamTransportRetirementStatus, SteamTransportRole,
};

pub const MAX_ONLINE_LOBBY_EVENTS: usize = 128;
pub const DEFAULT_ONLINE_LOBBY_EVENT_CAPACITY: usize = 64;
pub const DEFAULT_QUALITY_SAMPLE_INTERVAL_MS: u64 = 500;
/// One active generation plus bounded overlap from rapid failure/retry or
/// between-match transitions. Every entry has a 300 ms transport hard cap.
pub const MAX_RETIRING_STEAM_TRANSPORTS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OnlineLobbyTimeouts {
    pub platform_operation_ms: u64,
    pub authentication_ms: u64,
    pub connection_ms: u64,
    pub manifest_agreement_ms: u64,
    pub loading_ms: u64,
    pub initial_sync_ms: u64,
    pub ready_ms: u64,
    pub countdown_ms: u64,
    pub reconnect_ms: u64,
    pub result_confirmation_ms: u64,
}

impl Default for OnlineLobbyTimeouts {
    fn default() -> Self {
        Self {
            platform_operation_ms: 15_000,
            authentication_ms: 15_000,
            connection_ms: 15_000,
            manifest_agreement_ms: 10_000,
            loading_ms: 30_000,
            initial_sync_ms: 10_000,
            ready_ms: 30_000,
            countdown_ms: 10_000,
            reconnect_ms: 15_000,
            result_confirmation_ms: 10_000,
        }
    }
}

impl OnlineLobbyTimeouts {
    pub const fn validate(self) -> bool {
        self.platform_operation_ms > 0
            && self.authentication_ms > 0
            && self.connection_ms > 0
            && self.manifest_agreement_ms > 0
            && self.loading_ms > 0
            && self.initial_sync_ms > 0
            && self.ready_ms > 0
            && self.countdown_ms > 0
            && self.reconnect_ms > 0
            && self.result_confirmation_ms > 0
    }

    const fn for_phase(self, phase: OnlineLobbyPhase) -> Option<u64> {
        match phase {
            OnlineLobbyPhase::CreatingLobby | OnlineLobbyPhase::JoiningLobby => {
                Some(self.platform_operation_ms)
            }
            OnlineLobbyPhase::Authenticating => Some(self.authentication_ms),
            OnlineLobbyPhase::Connecting => Some(self.connection_ms),
            OnlineLobbyPhase::ManifestAgreement => Some(self.manifest_agreement_ms),
            OnlineLobbyPhase::Loading => Some(self.loading_ms),
            OnlineLobbyPhase::InitialSync => Some(self.initial_sync_ms),
            OnlineLobbyPhase::Ready => Some(self.ready_ms),
            OnlineLobbyPhase::Countdown => Some(self.countdown_ms),
            OnlineLobbyPhase::Reconnecting => Some(self.reconnect_ms),
            OnlineLobbyPhase::ConfirmingResult => Some(self.result_confirmation_ms),
            OnlineLobbyPhase::OfflineMenu
            | OnlineLobbyPhase::InvitePending
            | OnlineLobbyPhase::Lobby
            | OnlineLobbyPhase::Fighting
            | OnlineLobbyPhase::Results
            | OnlineLobbyPhase::ReturningToLobby
            | OnlineLobbyPhase::Failed => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OnlineLobbyConfig {
    pub timeouts: OnlineLobbyTimeouts,
    pub transport: SteamTransportConfig,
    pub quality: NetworkQualityPolicy,
    pub quality_sample_interval_ms: u64,
    pub virtual_port: i32,
    pub event_capacity: usize,
}

impl Default for OnlineLobbyConfig {
    fn default() -> Self {
        Self {
            timeouts: OnlineLobbyTimeouts::default(),
            transport: SteamTransportConfig::default(),
            quality: NetworkQualityPolicy::default(),
            quality_sample_interval_ms: DEFAULT_QUALITY_SAMPLE_INTERVAL_MS,
            virtual_port: 0,
            event_capacity: DEFAULT_ONLINE_LOBBY_EVENT_CAPACITY,
        }
    }
}

impl OnlineLobbyConfig {
    pub fn validate(self) -> Result<(), OnlineLobbyError> {
        if !self.timeouts.validate()
            || self.quality_sample_interval_ms == 0
            || self.event_capacity == 0
            || self.event_capacity > MAX_ONLINE_LOBBY_EVENTS
        {
            return Err(OnlineLobbyError::InvalidConfiguration);
        }
        self.transport
            .validate()
            .map_err(OnlineLobbyError::Transport)?;
        self.quality.validate().map_err(OnlineLobbyError::Quality)
    }
}

/// UI/application states. Match phases mirror the protocol lifecycle, while
/// platform operations and reconnect are made explicit instead of being hidden
/// in a loading spinner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnlineLobbyPhase {
    OfflineMenu,
    InvitePending,
    CreatingLobby,
    JoiningLobby,
    Lobby,
    Connecting,
    Authenticating,
    ManifestAgreement,
    Loading,
    InitialSync,
    Ready,
    Countdown,
    Fighting,
    Reconnecting,
    ConfirmingResult,
    Results,
    ReturningToLobby,
    Failed,
}

impl OnlineLobbyPhase {
    pub fn can_transition_to(self, next: Self) -> bool {
        if next == Self::Failed && self != Self::Failed {
            return true;
        }
        matches!(
            (self, next),
            (
                Self::OfflineMenu,
                Self::InvitePending | Self::CreatingLobby | Self::JoiningLobby
            ) | (Self::InvitePending, Self::OfflineMenu | Self::JoiningLobby)
                | (Self::CreatingLobby | Self::JoiningLobby, Self::Lobby)
                | (
                    Self::Lobby,
                    Self::OfflineMenu
                        | Self::Connecting
                        | Self::Authenticating
                        | Self::ManifestAgreement
                        | Self::ReturningToLobby
                )
                | (
                    Self::Connecting,
                    Self::Authenticating
                        | Self::ManifestAgreement
                        | Self::InitialSync
                        | Self::Reconnecting
                )
                | (
                    Self::Authenticating,
                    Self::Connecting
                        | Self::ManifestAgreement
                        | Self::InitialSync
                        | Self::Reconnecting
                )
                | (Self::ManifestAgreement, Self::Loading | Self::Lobby)
                | (Self::Loading, Self::InitialSync | Self::Lobby)
                | (
                    Self::InitialSync,
                    Self::Ready | Self::Countdown | Self::Fighting | Self::Lobby
                )
                | (Self::Ready, Self::Countdown | Self::Lobby)
                | (
                    Self::Countdown,
                    Self::Fighting | Self::Reconnecting | Self::Lobby
                )
                | (
                    Self::Fighting,
                    Self::Reconnecting | Self::ConfirmingResult | Self::Results
                )
                | (
                    Self::Reconnecting,
                    Self::Authenticating
                        | Self::Connecting
                        | Self::InitialSync
                        | Self::Results
                        | Self::Lobby
                )
                | (Self::ConfirmingResult, Self::Results | Self::Lobby)
                | (
                    Self::Results,
                    Self::Lobby | Self::OfflineMenu | Self::ReturningToLobby
                )
                | (Self::ReturningToLobby, Self::Lobby | Self::OfflineMenu)
                | (
                    Self::Failed,
                    Self::OfflineMenu | Self::Lobby | Self::ReturningToLobby
                )
        )
    }
}

/// Deterministic pure transition core. External callbacks are normalized before
/// they reach this machine, which makes callback order and UI frame rate unable
/// to skip lifecycle gates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OnlineFlowMachine {
    phase: OnlineLobbyPhase,
    entered_at_ms: u64,
    deadline_at_ms: Option<u64>,
}

impl OnlineFlowMachine {
    pub const fn new(now_ms: u64) -> Self {
        Self {
            phase: OnlineLobbyPhase::OfflineMenu,
            entered_at_ms: now_ms,
            deadline_at_ms: None,
        }
    }

    pub const fn phase(self) -> OnlineLobbyPhase {
        self.phase
    }

    pub const fn entered_at_ms(self) -> u64 {
        self.entered_at_ms
    }

    pub const fn deadline_at_ms(self) -> Option<u64> {
        self.deadline_at_ms
    }

    pub fn transition(
        &mut self,
        next: OnlineLobbyPhase,
        now_ms: u64,
        timeouts: OnlineLobbyTimeouts,
    ) -> Result<(), OnlineLobbyError> {
        if now_ms < self.entered_at_ms {
            return Err(OnlineLobbyError::TimeRegression);
        }
        if self.phase == next {
            return Ok(());
        }
        if !self.phase.can_transition_to(next) {
            return Err(OnlineLobbyError::InvalidTransition {
                from: self.phase,
                to: next,
            });
        }
        let deadline_at_ms = timeouts
            .for_phase(next)
            .map(|duration| {
                now_ms
                    .checked_add(duration)
                    .ok_or(OnlineLobbyError::InvalidConfiguration)
            })
            .transpose()?;
        self.phase = next;
        self.entered_at_ms = now_ms;
        self.deadline_at_ms = deadline_at_ms;
        Ok(())
    }

    pub const fn is_expired(self, now_ms: u64) -> bool {
        matches!(self.deadline_at_ms, Some(deadline) if now_ms >= deadline)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnlineLobbyRole {
    ListenAuthority,
    Client,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnlineMatchOutcome {
    Confirmed,
    NoContestHostLost,
}

/// One coordinator-owned Steam/protocol identity reservation.
///
/// The complete bounded snapshot carried by [`OnlineLobbyEvent::RosterChanged`]
/// is the cleanup-before-reallocation barrier for the runtime and application
/// layers. It deliberately includes unauthenticated in-progress reservations;
/// only removal from the snapshot releases a tuple for reuse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OnlinePeerIdentity {
    pub user: SteamUserId,
    pub peer_id: PeerId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnlineLobbyEvent {
    StateChanged {
        from: OnlineLobbyPhase,
        to: OnlineLobbyPhase,
    },
    JoinRequested(LobbyJoinIntent),
    LobbyEntered {
        lobby: SteamLobbyId,
        owner: SteamUserId,
        role: OnlineLobbyRole,
    },
    RosterChanged {
        members: u8,
        seats: u8,
        all_ready: bool,
        live_bindings: [Option<OnlinePeerIdentity>; MAX_STEAM_LOBBY_MEMBERS],
    },
    TransportRequested(SteamP2pSession),
    AuthenticationRequired {
        user: SteamUserId,
        reconnect: bool,
    },
    AuthTicketReady {
        handle: AuthTicketHandle,
        remote_user: SteamUserId,
    },
    PeerAuthenticated {
        user: SteamUserId,
        peer_id: PeerId,
        reconnect: bool,
    },
    PeerAuthenticationRejected {
        user: SteamUserId,
        connection: Option<SteamConnectionId>,
        failure: OnlineFailure,
    },
    EndpointReady {
        connection: SteamConnectionId,
        user: SteamUserId,
        peer_id: PeerId,
        reconnect: bool,
    },
    PeerDisconnected {
        connection: SteamConnectionId,
        user: SteamUserId,
        peer_id: PeerId,
        reconnect_allowed: bool,
    },
    QualityChanged {
        user: SteamUserId,
        quality: NetworkQualitySnapshot,
    },
    ManifestCommitted(ManifestHash),
    DropGameplayEndpoints,
    MatchEnded(OnlineMatchOutcome),
    ReturnedToLobby {
        rematch: bool,
    },
    RichPresenceUnavailable,
    Failure(OnlineFailure),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OnlineLobbyStatus {
    pub phase: OnlineLobbyPhase,
    pub deadline_at_ms: Option<u64>,
    pub lobby: Option<SteamLobbyId>,
    pub owner: Option<SteamUserId>,
    pub role: Option<OnlineLobbyRole>,
    pub pending_join: Option<LobbyJoinIntent>,
    pub lobby_members: u8,
    pub roster_members: u8,
    pub total_seats: u8,
    pub seat_capacity: u8,
    pub effective_joinable: bool,
    pub all_members_ready: bool,
    pub connected_remote_peers: u8,
    pub transport_installed: bool,
    pub relay_status: SteamRelayStatus,
    pub manifest_hash: Option<ManifestHash>,
    pub countdown_start_tick: Option<SimTick>,
    pub network_quality: NetworkQualitySnapshot,
    pub input_delay_calibration: InputDelayCalibrationSnapshot,
    pub outcome: Option<OnlineMatchOutcome>,
    pub failure: Option<OnlineFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OnlineLobbyError {
    InvalidConfiguration,
    InvalidTransition {
        from: OnlineLobbyPhase,
        to: OnlineLobbyPhase,
    },
    InvalidState,
    TimeRegression,
    EventQueueOverflow,
    EndpointQueueOverflow,
    MissingPeerBinding(SteamUserId),
    DuplicatePeerBinding,
    PeerIdentityMismatch,
    LocalDeclarationMismatch,
    DeclarationRevisionMustIncrease,
    DeclarationRevisionExhausted,
    DeclarationChangeMustClearReady,
    TransportSessionMismatch,
    TransportAlreadyInstalled,
    TransportNotInstalled,
    AdmissionQuiesced,
    RetiringTransportCapacity,
    MissingAuthenticatedAdmission,
    ManifestMismatch,
    ManifestDeclarationsPending,
    PeersNotReady,
    InputDelayCalibrationIncomplete,
    InputDelayCalibrationMismatch,
    InputDelayCalibrationUnplayable,
    RollbackBudgetExceeded,
    QualityPolicyRejected,
    Steam(SteamPlatformError),
    Transport(SteamTransportError),
    Roster(OnlineRosterError),
    Protocol(ProtocolValidationError),
    Quality(NetworkQualityError),
}

impl fmt::Display for OnlineLobbyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "online lobby operation failed: {self:?}")
    }
}

impl std::error::Error for OnlineLobbyError {}

impl From<SteamPlatformError> for OnlineLobbyError {
    fn from(value: SteamPlatformError) -> Self {
        Self::Steam(value)
    }
}

impl From<SteamTransportError> for OnlineLobbyError {
    fn from(value: SteamTransportError) -> Self {
        Self::Transport(value)
    }
}

impl From<OnlineRosterError> for OnlineLobbyError {
    fn from(value: OnlineRosterError) -> Self {
        Self::Roster(value)
    }
}

impl From<ProtocolValidationError> for OnlineLobbyError {
    fn from(value: ProtocolValidationError) -> Self {
        Self::Protocol(value)
    }
}

impl From<NetworkQualityError> for OnlineLobbyError {
    fn from(value: NetworkQualityError) -> Self {
        Self::Quality(value)
    }
}

struct PeerBinding {
    user: SteamUserId,
    peer_id: PeerId,
    authenticated: bool,
    admission: Option<AuthenticatedSteamPeer>,
    pending_connection: Option<SteamConnectionId>,
    connection: Option<SteamConnectionId>,
    /// Exact prior generation allowed to drain while a reconnect generation
    /// authenticates and attaches under the same Steam identity.
    retiring_connection: Option<SteamConnectionId>,
    quality: NetworkQualityMonitor,
    last_reported_quality: NetworkQuality,
    precommit_rtt: PrecommitRttCalibrator,
    /// The old match generation remains identity-reserved until its transport
    /// reaches an exact retirement outcome and platform auth is ended.
    retiring: bool,
    /// Exact Steam generation whose authority-side typed terminal has drained.
    /// Its later native close is cleanup-only and must not start reconnect.
    authority_terminal_cleanup: Option<SteamConnectionId>,
    /// A committed listen-owner close waits one coordinator turn so an
    /// authority `TerminalDrained` publication racing the native callback can
    /// mark the exact generation first.
    deferred_authority_close: Option<DeferredAuthorityClose>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DeferredAuthorityClose {
    connection: SteamConnectionId,
    reason: SteamTransportCloseReason,
}

#[derive(Clone, Copy)]
struct PendingIncoming {
    connection: SteamConnectionId,
    user: SteamUserId,
}

struct PendingIssuedTicket {
    lease: AuthTicketLease,
    ticket: Option<IssuedAuthTicket>,
    ready: bool,
}

struct RetiringSteamTransport {
    transport: SteamTransport,
    authenticated_users: [Option<SteamUserId>; MAX_STEAM_LOBBY_MEMBERS],
    issued_tickets: Vec<PendingIssuedTicket>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OnlineTransportRetirementMetrics {
    pub started: u64,
    pub completed: u64,
    pub timed_out: u64,
    pub faulted: u64,
    pub high_water: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthPeerLease {
    pub user: SteamUserId,
    pub peer_id: PeerId,
    pub revision: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthSignalScope {
    pub lobby: SteamLobbyId,
    pub purpose: AdmissionPurpose,
    pub owner_revision: u16,
    pub match_id: Option<MatchId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthTicketLease {
    pub handle: AuthTicketHandle,
    pub remote_user: SteamUserId,
    pub remote_revision: u16,
    pub sender: AuthPeerLease,
    pub scope: AuthSignalScope,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthSignalLeaseStatus {
    Current,
    Stale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CommittedMemberRevision {
    user: SteamUserId,
    revision: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReconnectResumePhase {
    Countdown,
    Fighting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ManifestPeerGroup {
    peer_id: PeerId,
    seat_count: u8,
    seats: [OnlineSeatSelection; MAX_FIGHTERS],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LobbyContract {
    authority: AuthorityKind,
    visibility: LobbyVisibility,
    rules: DefinitionId,
    arena: DefinitionId,
    seat_capacity: u8,
}

impl LobbyContract {
    fn first_release(metadata: &LobbyMetadata) -> Result<Self, OnlineLobbyError> {
        metadata.validate_first_release_player_scope()?;
        Ok(Self {
            authority: metadata.authority,
            visibility: metadata.visibility,
            rules: metadata.rules,
            arena: metadata.arena,
            seat_capacity: metadata.seat_capacity,
        })
    }

    fn accepts_manifest(self, config: &HeadlessMatchConfig) -> bool {
        FirstReleaseOnlinePolicy::accepts_manifest(&config.manifest)
            && self.authority == config.manifest.authority
            && matches!(
                self.visibility,
                LobbyVisibility::Private | LobbyVisibility::FriendsOnly
            )
            && self.rules == config.manifest.rules
            && self.arena == config.manifest.arena
            && config.manifest.ownership.len() <= usize::from(self.seat_capacity)
    }
}

/// One native online-flow owner. The gameplay endpoint is moved out through
/// [`take_endpoint`](Self::take_endpoint), while this object retains and pumps
/// the corresponding Steam transport connection.
pub struct OnlineLobbyCoordinator {
    config: OnlineLobbyConfig,
    local_user: SteamUserId,
    flow: OnlineFlowMachine,
    last_now_ms: u64,
    lobby: Option<SteamLobbyId>,
    owner: Option<SteamUserId>,
    role: Option<OnlineLobbyRole>,
    lobby_contract: Option<LobbyContract>,
    pending_join: Option<LobbyJoinIntent>,
    local_declaration: Option<OnlineRosterMember>,
    bindings: [Option<PeerBinding>; MAX_STEAM_LOBBY_MEMBERS],
    pending_incoming: [Option<PendingIncoming>; MAX_STEAM_LOBBY_MEMBERS],
    quality_rejected_users: [Option<SteamUserId>; MAX_STEAM_LOBBY_MEMBERS],
    roster: OnlineRoster,
    lobby_member_count: usize,
    platform_total_seats: usize,
    seat_capacity: u8,
    effective_joinable: bool,
    roster_all_ready: bool,
    transport_request: Option<SteamP2pSession>,
    transport: Option<SteamTransport>,
    retiring_transports: VecDeque<RetiringSteamTransport>,
    retirement_metrics: OnlineTransportRetirementMetrics,
    /// A user-visible leave may transition UI immediately, but the Steam lobby
    /// and its auth sessions remain owned until all retired transports finish.
    pending_platform_leave: bool,
    /// Monotonic within one active match teardown. No new native or Steam-auth
    /// capability may cross the application boundary after this is raised.
    admission_quiesced: bool,
    relay_status: SteamRelayStatus,
    endpoints: VecDeque<AdmittedSteamEndpoint>,
    issued_tickets: Vec<PendingIssuedTicket>,
    events: VecDeque<OnlineLobbyEvent>,
    match_config: Option<HeadlessMatchConfig>,
    countdown_start_tick: Option<SimTick>,
    reconnect_resume: Option<ReconnectResumePhase>,
    last_quality_sample_ms: u64,
    outcome: Option<OnlineMatchOutcome>,
    failure: Option<OnlineFailure>,
    committed_input_delay_calibration: Option<InputDelayCalibrationSnapshot>,
    /// First authenticated authority-authored terminal for the current match.
    /// It protects the exact failure from later generic connection callbacks.
    authority_disconnect: Option<DisconnectMessage>,
    /// Steam-member declaration revisions frozen with the current/previous
    /// manifest. A newer unready owner declaration is the durable between-match
    /// epoch, and the owner waits for each client's matching acknowledgement
    /// before starting fresh Initial authentication.
    committed_peer_leases: [Option<AuthPeerLease>; MAX_STEAM_LOBBY_MEMBERS],
    committed_member_revisions: [Option<CommittedMemberRevision>; MAX_STEAM_LOBBY_MEMBERS],
    /// A non-owner may express either Results action early, but cannot retire
    /// the old match or originate Initial authentication before the owner epoch.
    pending_results_return: Option<bool>,
}

impl OnlineLobbyCoordinator {
    pub fn new(
        local_user: SteamUserId,
        config: OnlineLobbyConfig,
        now_ms: u64,
    ) -> Result<Self, OnlineLobbyError> {
        config.validate()?;
        Ok(Self {
            config,
            local_user,
            flow: OnlineFlowMachine::new(now_ms),
            last_now_ms: now_ms,
            lobby: None,
            owner: None,
            role: None,
            lobby_contract: None,
            pending_join: None,
            local_declaration: None,
            bindings: std::array::from_fn(|_| None),
            pending_incoming: [None; MAX_STEAM_LOBBY_MEMBERS],
            quality_rejected_users: [None; MAX_STEAM_LOBBY_MEMBERS],
            roster: OnlineRoster::default(),
            lobby_member_count: 0,
            platform_total_seats: 0,
            seat_capacity: 0,
            effective_joinable: false,
            roster_all_ready: false,
            transport_request: None,
            transport: None,
            retiring_transports: VecDeque::with_capacity(MAX_RETIRING_STEAM_TRANSPORTS),
            retirement_metrics: OnlineTransportRetirementMetrics::default(),
            pending_platform_leave: false,
            admission_quiesced: false,
            relay_status: SteamRelayStatus::default(),
            endpoints: VecDeque::with_capacity(MAX_STEAM_LOBBY_MEMBERS),
            issued_tickets: Vec::with_capacity(MAX_STEAM_LOBBY_MEMBERS),
            events: VecDeque::with_capacity(config.event_capacity),
            match_config: None,
            countdown_start_tick: None,
            reconnect_resume: None,
            last_quality_sample_ms: now_ms,
            outcome: None,
            failure: None,
            committed_input_delay_calibration: None,
            authority_disconnect: None,
            committed_peer_leases: [None; MAX_STEAM_LOBBY_MEMBERS],
            committed_member_revisions: [None; MAX_STEAM_LOBBY_MEMBERS],
            pending_results_return: None,
        })
    }

    pub const fn config(&self) -> OnlineLobbyConfig {
        self.config
    }

    pub const fn local_user(&self) -> SteamUserId {
        self.local_user
    }

    pub const fn local_declaration(&self) -> Option<OnlineRosterMember> {
        self.local_declaration
    }

    pub fn retiring_transport_count(&self) -> usize {
        self.retiring_transports.len()
    }

    pub const fn transport_retirement_metrics(&self) -> OnlineTransportRetirementMetrics {
        self.retirement_metrics
    }

    /// True when this Steam member has acknowledged the latest completed-match
    /// epoch and may participate in a fresh Initial-authentication exchange.
    /// A lobby that has not completed a match has no revision floor.
    pub fn initial_authentication_allowed(
        &self,
        user: SteamUserId,
        observed_revision: u16,
    ) -> bool {
        !self.user_is_retiring(user)
            && self
                .committed_peer_leases
                .iter()
                .flatten()
                .find(|record| record.user == user)
                .map(|record| record.revision)
                .or_else(|| {
                    self.committed_member_revisions
                        .iter()
                        .flatten()
                        .find(|record| record.user == user)
                        .map(|record| record.revision)
                })
                .is_none_or(|revision| observed_revision > revision)
    }

    pub fn committed_auth_peer_lease(&self, user: SteamUserId) -> Option<AuthPeerLease> {
        self.committed_peer_leases
            .iter()
            .flatten()
            .find(|lease| lease.user == user)
            .copied()
    }

    fn committed_auth_revision(&self, user: SteamUserId) -> Option<u16> {
        self.committed_auth_peer_lease(user)
            .map(|lease| lease.revision)
            .or_else(|| {
                self.committed_member_revisions
                    .iter()
                    .flatten()
                    .find(|record| record.user == user)
                    .map(|record| record.revision)
            })
    }

    pub const fn authorized_auth_signal_leases(
        &self,
    ) -> [Option<AuthPeerLease>; MAX_STEAM_LOBBY_MEMBERS] {
        self.committed_peer_leases
    }

    pub fn status(&self) -> OnlineLobbyStatus {
        let connected_remote_peers = self
            .bindings
            .iter()
            .flatten()
            .filter(|binding| {
                binding.user != self.local_user && !binding.retiring && binding.connection.is_some()
            })
            .count();
        let network_quality = self
            .bindings
            .iter()
            .flatten()
            .filter(|binding| !binding.retiring && binding.connection.is_some())
            .map(|binding| binding.quality.snapshot())
            .max_by_key(|snapshot| snapshot.quality)
            .unwrap_or_default();
        OnlineLobbyStatus {
            phase: self.flow.phase(),
            deadline_at_ms: self.flow.deadline_at_ms(),
            lobby: self.lobby,
            owner: self.owner,
            role: self.role,
            pending_join: self.pending_join,
            lobby_members: self.lobby_member_count.min(u8::MAX as usize) as u8,
            roster_members: self.roster.len().min(u8::MAX as usize) as u8,
            total_seats: self.platform_total_seats.min(u8::MAX as usize) as u8,
            seat_capacity: self.seat_capacity,
            effective_joinable: self.effective_joinable,
            all_members_ready: self.roster_all_ready,
            connected_remote_peers: connected_remote_peers.min(u8::MAX as usize) as u8,
            transport_installed: self.transport.is_some(),
            relay_status: self.relay_status,
            manifest_hash: self
                .match_config
                .as_ref()
                .map(|config| config.manifest.manifest_hash),
            countdown_start_tick: self.countdown_start_tick,
            network_quality,
            input_delay_calibration: self.input_delay_calibration(),
            outcome: self.outcome,
            failure: self.failure,
        }
    }

    fn input_delay_calibration(&self) -> InputDelayCalibrationSnapshot {
        if let Some(committed) = self.committed_input_delay_calibration {
            return committed;
        }
        if self.role != Some(OnlineLobbyRole::ListenAuthority) {
            return InputDelayCalibrationSnapshot {
                state: InputDelayCalibrationState::NotAuthority,
                ..Default::default()
            };
        }

        let mut peers = [None; MAX_STEAM_LOBBY_MEMBERS];
        let mut peer_count = 0_usize;
        for binding in self.bindings.iter().flatten().filter(|binding| {
            binding.user != self.local_user && binding.authenticated && binding.connection.is_some()
        }) {
            peers[peer_count] = Some((binding.peer_id.get(), binding.precommit_rtt.p95_rtt_ms()));
            peer_count += 1;
        }
        peers[..peer_count]
            .sort_unstable_by_key(|peer| peer.expect("calibration peers have a dense prefix").0);

        if peer_count == 0 {
            return InputDelayCalibrationSnapshot {
                state: InputDelayCalibrationState::Ready,
                remote_peer_count: 0,
                calibrated_peer_count: 0,
                worst_p95_rtt_ms: Some(0),
                selected_input_delay_ticks: Some(2),
                required_rollback_ticks: Some(2),
            };
        }

        let mut calibrated_peer_count = 0_usize;
        let mut worst_p95_rtt_ms = None;
        for (_, p95_rtt_ms) in peers[..peer_count].iter().flatten().copied() {
            if let Some(p95_rtt_ms) = p95_rtt_ms {
                calibrated_peer_count += 1;
                worst_p95_rtt_ms = Some(worst_p95_rtt_ms.unwrap_or(0_u16).max(p95_rtt_ms));
            }
        }
        if calibrated_peer_count != peer_count {
            return InputDelayCalibrationSnapshot {
                state: InputDelayCalibrationState::Calibrating,
                remote_peer_count: peer_count.min(u8::MAX as usize) as u8,
                calibrated_peer_count: calibrated_peer_count.min(u8::MAX as usize) as u8,
                worst_p95_rtt_ms,
                selected_input_delay_ticks: None,
                required_rollback_ticks: None,
            };
        }

        let worst_p95_rtt_ms = worst_p95_rtt_ms.expect("every calibration peer has a p95");
        let (selected_input_delay_ticks, required_rollback_ticks) =
            calibrated_input_delay(worst_p95_rtt_ms);
        InputDelayCalibrationSnapshot {
            state: if required_rollback_ticks > MAX_CALIBRATED_ROLLBACK_TICKS {
                InputDelayCalibrationState::Unplayable
            } else {
                InputDelayCalibrationState::Ready
            },
            remote_peer_count: peer_count.min(u8::MAX as usize) as u8,
            calibrated_peer_count: calibrated_peer_count.min(u8::MAX as usize) as u8,
            worst_p95_rtt_ms: Some(worst_p95_rtt_ms),
            selected_input_delay_ticks: Some(selected_input_delay_ticks),
            required_rollback_ticks: Some(required_rollback_ticks),
        }
    }

    pub fn poll_event(&mut self) -> Option<OnlineLobbyEvent> {
        self.events.pop_front()
    }

    pub fn take_transport_request(&mut self) -> Option<SteamP2pSession> {
        if self.admission_quiesced {
            self.transport_request = None;
            return None;
        }
        self.transport_request.take()
    }

    pub fn match_config(&self) -> Option<&HeadlessMatchConfig> {
        self.match_config.as_ref()
    }

    pub fn take_endpoint(&mut self) -> Option<AdmittedSteamEndpoint> {
        if self.admission_quiesced {
            self.endpoints.clear();
            return None;
        }
        self.endpoints.pop_front()
    }

    pub fn active_connection_for_user(&self, user: SteamUserId) -> Option<SteamConnectionId> {
        self.binding(user).and_then(|binding| binding.connection)
    }

    pub const fn admission_is_quiesced(&self) -> bool {
        self.admission_quiesced
    }

    /// Atomically fences every new online capability while preserving already
    /// established connections for bounded terminal/ACK drain.
    pub fn quiesce_admission<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
    ) -> Result<(), OnlineLobbyError> {
        if self.admission_quiesced {
            return Ok(());
        }
        self.admission_quiesced = true;
        self.transport_request = None;
        self.endpoints.clear();
        self.events.retain(|event| {
            !matches!(
                event,
                OnlineLobbyEvent::TransportRequested(_)
                    | OnlineLobbyEvent::AuthTicketReady { .. }
                    | OnlineLobbyEvent::AuthenticationRequired { .. }
                    | OnlineLobbyEvent::PeerAuthenticated { .. }
                    | OnlineLobbyEvent::EndpointReady { .. }
            )
        });

        let pending: Vec<_> = self.pending_incoming.iter().flatten().copied().collect();
        self.pending_incoming = [None; MAX_STEAM_LOBBY_MEMBERS];
        if let Some(transport) = &mut self.transport {
            for incoming in pending {
                if transport
                    .connection_state(incoming.connection)
                    .is_some_and(|state| {
                        state == crate::steam_transport::SteamTransportConnectionState::PendingAdmission
                    })
                {
                    transport.reject_incoming(incoming.connection)?;
                }
            }
            if self.role == Some(OnlineLobbyRole::ListenAuthority) {
                transport.stop_listening()?;
            }
        }

        for record in std::mem::take(&mut self.issued_tickets) {
            let _ = platform.cancel_auth_ticket(record.lease.handle);
        }
        let mut end_auth = [None; MAX_STEAM_LOBBY_MEMBERS];
        let mut end_count = 0_usize;
        for binding in self.bindings.iter_mut().flatten() {
            if binding.user == self.local_user || binding.connection.is_some() {
                continue;
            }
            if binding.authenticated
                || binding.admission.is_some()
                || binding.pending_connection.is_some()
            {
                end_auth[end_count] = Some(binding.user);
                end_count += 1;
            }
            binding.authenticated = false;
            binding.admission = None;
            binding.pending_connection = None;
        }
        for user in end_auth[..end_count].iter().flatten().copied() {
            let _ = platform.end_peer_authentication(user);
        }
        Ok(())
    }

    /// Marks one exact listen-authority physical generation as terminal.
    ///
    /// The authority worker and Steam adapter use independent connection IDs;
    /// the application resolves that mapping before calling here. A stale
    /// generation is a benign `Ok(false)`. For a live generation, native
    /// endpoint drop/quiet drain still owns physical close. If the native
    /// callback won the race, its bounded deferred fact is consumed here.
    pub fn mark_authority_terminal_drained<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        user: SteamUserId,
        peer_id: PeerId,
        connection: SteamConnectionId,
        retry: Option<RetryDisposition>,
    ) -> Result<bool, OnlineLobbyError> {
        if self.role != Some(OnlineLobbyRole::ListenAuthority) || user == self.local_user {
            return Ok(false);
        }
        let Some(binding) = self.binding(user) else {
            return Ok(false);
        };
        if binding.peer_id != peer_id {
            return Ok(false);
        }
        if binding.connection == Some(connection) {
            if retry == Some(RetryDisposition::ReconnectAllowed) {
                self.prepare_connection_replacement(platform, user, connection)?;
            } else {
                self.binding_mut(user)
                    .expect("validated binding remains installed")
                    .authority_terminal_cleanup = Some(connection);
            }
            return Ok(true);
        }
        if self
            .binding(user)
            .expect("validated binding remains installed")
            .deferred_authority_close
            .is_some_and(|closed| closed.connection == connection)
        {
            let binding = self
                .binding_mut(user)
                .expect("validated binding remains installed");
            binding.deferred_authority_close = None;
            binding.authority_terminal_cleanup = None;
            return Ok(true);
        }
        Ok(false)
    }

    pub fn take_ready_auth_ticket(&mut self, lease: AuthTicketLease) -> Option<IssuedAuthTicket> {
        let record = self
            .issued_tickets
            .iter_mut()
            .find(|record| record.lease == lease && record.ready)?;
        record.ticket.take()
    }

    pub fn begin_create<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        request: crate::steam_platform::LobbyCreateRequest,
        metadata: crate::steam_platform::LobbyMetadata,
        declaration: OnlineRosterMember,
        now_ms: u64,
    ) -> Result<(), OnlineLobbyError> {
        self.require_local_platform(platform)?;
        self.require_phase(OnlineLobbyPhase::OfflineMenu)?;
        self.admission_quiesced = false;
        self.validate_initial_local_declaration(declaration)?;
        LobbyContract::first_release(&metadata)?;
        platform.create_lobby(request, metadata)?;
        self.local_declaration = Some(declaration);
        let result = self
            .install_local_binding(declaration.peer_id)
            .and_then(|()| self.transition(OnlineLobbyPhase::CreatingLobby, now_ms));
        if let Err(error) = result {
            platform.cancel_pending_lobby_operation()?;
            self.local_declaration = None;
            self.bindings = std::array::from_fn(|_| None);
            self.flow = OnlineFlowMachine::new(now_ms);
            return Err(error);
        }
        Ok(())
    }

    pub fn begin_join<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        intent: LobbyJoinIntent,
        declaration: OnlineRosterMember,
        now_ms: u64,
    ) -> Result<(), OnlineLobbyError> {
        self.require_local_platform(platform)?;
        if !matches!(
            self.flow.phase(),
            OnlineLobbyPhase::OfflineMenu | OnlineLobbyPhase::InvitePending
        ) {
            return Err(OnlineLobbyError::InvalidState);
        }
        self.admission_quiesced = false;
        self.validate_initial_local_declaration(declaration)?;
        let prior_flow = self.flow;
        let prior_pending_join = self.pending_join;
        platform.join_lobby(intent, declaration.seat_count() as u8, now_ms)?;
        self.pending_join = None;
        self.local_declaration = Some(declaration);
        let result = self
            .install_local_binding(declaration.peer_id)
            .and_then(|()| self.transition(OnlineLobbyPhase::JoiningLobby, now_ms));
        if let Err(error) = result {
            platform.cancel_pending_lobby_operation()?;
            self.pending_join = prior_pending_join;
            self.local_declaration = None;
            self.bindings = std::array::from_fn(|_| None);
            self.flow = prior_flow;
            return Err(error);
        }
        Ok(())
    }

    pub fn decline_join_request(&mut self, now_ms: u64) -> Result<(), OnlineLobbyError> {
        self.require_phase(OnlineLobbyPhase::InvitePending)?;
        self.pending_join = None;
        self.transition(OnlineLobbyPhase::OfflineMenu, now_ms)
    }

    pub fn set_local_declaration<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        declaration: OnlineRosterMember,
    ) -> Result<(), OnlineLobbyError> {
        self.require_phase(OnlineLobbyPhase::Lobby)?;
        self.validate_local_declaration_identity(declaration)?;
        if let Some(prior) = self.local_declaration {
            if declaration.revision <= prior.revision {
                return Err(OnlineLobbyError::DeclarationRevisionMustIncrease);
            }
            if declaration.ready {
                return Err(OnlineLobbyError::DeclarationChangeMustClearReady);
            }
        }
        self.publish_declaration(platform, declaration)?;
        self.local_declaration = Some(declaration);
        self.rebuild_roster(platform)
    }

    pub fn set_ready<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        ready: bool,
    ) -> Result<(), OnlineLobbyError> {
        self.require_phase(OnlineLobbyPhase::Lobby)?;
        let current = self
            .local_declaration
            .ok_or(OnlineLobbyError::LocalDeclarationMismatch)?;
        let declaration = OnlineRosterMember::new(
            current.peer_id,
            current.authenticated_user,
            current.revision,
            ready,
            current.seats(),
        )?;
        self.publish_declaration(platform, declaration)?;
        self.local_declaration = Some(declaration);
        self.rebuild_roster(platform)
    }

    pub fn open_invite_overlay<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
    ) -> Result<SteamOverlayRequestStatus, OnlineLobbyError> {
        self.require_phase(OnlineLobbyPhase::Lobby)?;
        platform.open_invite_overlay().map_err(Into::into)
    }

    /// Issues a ticket but does not expose it until Steam emits
    /// `AuthTicketReady`. The caller transports the ready ticket through the
    /// product's bounded pre-game signaling channel; lobby chat is not used.
    pub fn issue_auth_ticket<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        remote_user: SteamUserId,
        purpose: AdmissionPurpose,
    ) -> Result<AuthTicketLease, OnlineLobbyError> {
        self.require_lobby()?;
        if self.admission_quiesced {
            return Err(OnlineLobbyError::AdmissionQuiesced);
        }
        if self.is_quality_rejected(remote_user) || self.user_is_retiring(remote_user) {
            return Err(OnlineLobbyError::QualityPolicyRejected);
        }
        if self.issued_tickets.len() >= MAX_STEAM_LOBBY_MEMBERS {
            return Err(OnlineLobbyError::InvalidState);
        }
        let (sender, scope, remote_revision) =
            self.auth_ticket_context(platform, remote_user, purpose)?;
        let ticket = platform.issue_auth_ticket(remote_user)?;
        let handle = ticket.handle;
        let lease = AuthTicketLease {
            handle,
            remote_user,
            remote_revision,
            sender,
            scope,
        };
        self.issued_tickets.push(PendingIssuedTicket {
            lease,
            ticket: Some(ticket),
            ready: false,
        });
        Ok(lease)
    }

    pub fn cancel_auth_ticket<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        handle: AuthTicketHandle,
    ) -> Result<(), OnlineLobbyError> {
        let index = self
            .issued_tickets
            .iter()
            .position(|record| record.lease.handle == handle)
            .ok_or(OnlineLobbyError::InvalidState)?;
        platform.cancel_auth_ticket(handle)?;
        self.issued_tickets.swap_remove(index);
        Ok(())
    }

    pub fn auth_signal_scope<B: SteamBackend>(
        &self,
        platform: &SteamPlatform<B>,
        purpose: AdmissionPurpose,
    ) -> Result<AuthSignalScope, OnlineLobbyError> {
        let lobby = self.require_lobby()?;
        let owner = self.owner.ok_or(OnlineLobbyError::InvalidState)?;
        let owner_revision = match purpose {
            AdmissionPurpose::Initial => self
                .live_auth_revision(platform, owner)
                .ok_or(OnlineLobbyError::ManifestDeclarationsPending)?,
            AdmissionPurpose::Reconnect => self
                .committed_auth_peer_lease(owner)
                .map(|lease| lease.revision)
                .ok_or(OnlineLobbyError::MissingPeerBinding(owner))?,
        };
        if owner_revision == 0 {
            return Err(OnlineLobbyError::DeclarationRevisionMustIncrease);
        }
        let match_id = match purpose {
            AdmissionPurpose::Initial => None,
            AdmissionPurpose::Reconnect => Some(
                self.match_config
                    .as_ref()
                    .ok_or(OnlineLobbyError::InvalidState)?
                    .manifest
                    .match_id,
            ),
        };
        Ok(AuthSignalScope {
            lobby,
            purpose,
            owner_revision,
            match_id,
        })
    }

    pub fn auth_ticket_lease_is_current<B: SteamBackend>(
        &self,
        platform: &SteamPlatform<B>,
        lease: AuthTicketLease,
    ) -> bool {
        self.auth_ticket_context(platform, lease.remote_user, lease.scope.purpose)
            .is_ok_and(|(sender, scope, remote_revision)| {
                sender == lease.sender
                    && scope == lease.scope
                    && remote_revision == lease.remote_revision
            })
    }

    pub fn classify_auth_signal_lease<B: SteamBackend>(
        &self,
        platform: &SteamPlatform<B>,
        sender: AuthPeerLease,
        scope: AuthSignalScope,
    ) -> Result<AuthSignalLeaseStatus, OnlineLobbyError> {
        if sender.user == self.local_user
            || sender.revision == 0
            || scope.owner_revision == 0
            || scope.lobby != self.require_lobby()?
            || matches!(
                (scope.purpose, scope.match_id),
                (AdmissionPurpose::Initial, Some(_)) | (AdmissionPurpose::Reconnect, None)
            )
        {
            return Err(OnlineLobbyError::PeerIdentityMismatch);
        }

        if scope.purpose == AdmissionPurpose::Reconnect {
            let Some(current_match_id) = self
                .match_config
                .as_ref()
                .map(|config| config.manifest.match_id)
            else {
                return Ok(AuthSignalLeaseStatus::Stale);
            };
            if scope.match_id != Some(current_match_id) {
                return Ok(AuthSignalLeaseStatus::Stale);
            }
        }
        let owner = self.owner.ok_or(OnlineLobbyError::InvalidState)?;
        match scope.purpose {
            AdmissionPurpose::Reconnect => {
                let owner_revision = self
                    .committed_auth_revision(owner)
                    .ok_or(OnlineLobbyError::MissingPeerBinding(owner))?;
                if scope.owner_revision < owner_revision {
                    return Ok(AuthSignalLeaseStatus::Stale);
                }
                if scope.owner_revision > owner_revision {
                    return Err(OnlineLobbyError::DeclarationRevisionMustIncrease);
                }

                let committed = self
                    .committed_auth_peer_lease(sender.user)
                    .ok_or(OnlineLobbyError::MissingPeerBinding(sender.user))?;
                if sender.revision < committed.revision {
                    return Ok(AuthSignalLeaseStatus::Stale);
                }
                if sender.revision > committed.revision {
                    return Err(OnlineLobbyError::DeclarationRevisionMustIncrease);
                }
                if sender.peer_id != committed.peer_id {
                    return Err(OnlineLobbyError::PeerIdentityMismatch);
                }
            }
            AdmissionPurpose::Initial => {
                let committed_owner_revision = self.committed_auth_revision(owner);
                let live_owner_revision = self.live_auth_revision(platform, owner);
                if let Some(committed_revision) = committed_owner_revision {
                    if scope.owner_revision <= committed_revision {
                        return Ok(AuthSignalLeaseStatus::Stale);
                    }
                    if let Some(live_revision) = live_owner_revision {
                        if scope.owner_revision < live_revision {
                            return Ok(AuthSignalLeaseStatus::Stale);
                        }
                        if scope.owner_revision > live_revision {
                            return Err(OnlineLobbyError::DeclarationRevisionMustIncrease);
                        }
                    } else if sender.user != owner || scope.owner_revision != sender.revision {
                        return Err(OnlineLobbyError::ManifestDeclarationsPending);
                    }
                } else {
                    let live_revision =
                        live_owner_revision.ok_or(OnlineLobbyError::ManifestDeclarationsPending)?;
                    if scope.owner_revision < live_revision {
                        return Ok(AuthSignalLeaseStatus::Stale);
                    }
                    if scope.owner_revision > live_revision {
                        return Err(OnlineLobbyError::DeclarationRevisionMustIncrease);
                    }
                }

                if let Some(committed) = self.committed_auth_peer_lease(sender.user) {
                    if sender.revision <= committed.revision {
                        return Ok(AuthSignalLeaseStatus::Stale);
                    }
                    if sender.peer_id != committed.peer_id {
                        return Err(OnlineLobbyError::PeerIdentityMismatch);
                    }
                    if let Some(live_revision) = self.live_auth_revision(platform, sender.user) {
                        if sender.revision < live_revision {
                            return Ok(AuthSignalLeaseStatus::Stale);
                        }
                        if sender.revision > live_revision {
                            return Err(OnlineLobbyError::DeclarationRevisionMustIncrease);
                        }
                    }
                } else {
                    let live_revision = self
                        .live_auth_revision(platform, sender.user)
                        .ok_or(OnlineLobbyError::MissingPeerBinding(sender.user))?;
                    if sender.revision < live_revision {
                        return Ok(AuthSignalLeaseStatus::Stale);
                    }
                    if sender.revision > live_revision {
                        return Err(OnlineLobbyError::DeclarationRevisionMustIncrease);
                    }
                    if self
                        .binding(sender.user)
                        .is_some_and(|binding| binding.peer_id != sender.peer_id)
                    {
                        return Err(OnlineLobbyError::PeerIdentityMismatch);
                    }
                }
            }
        }
        Ok(AuthSignalLeaseStatus::Current)
    }

    fn auth_ticket_context<B: SteamBackend>(
        &self,
        platform: &SteamPlatform<B>,
        remote_user: SteamUserId,
        purpose: AdmissionPurpose,
    ) -> Result<(AuthPeerLease, AuthSignalScope, u16), OnlineLobbyError> {
        let declaration = self
            .local_declaration
            .ok_or(OnlineLobbyError::LocalDeclarationMismatch)?;
        let sender = match purpose {
            AdmissionPurpose::Initial => {
                if !self.initial_authentication_allowed(self.local_user, declaration.revision) {
                    return Err(OnlineLobbyError::InvalidState);
                }
                AuthPeerLease {
                    user: self.local_user,
                    peer_id: declaration.peer_id,
                    revision: declaration.revision,
                }
            }
            AdmissionPurpose::Reconnect => self
                .committed_auth_peer_lease(self.local_user)
                .ok_or(OnlineLobbyError::MissingPeerBinding(self.local_user))?,
        };
        let remote_revision = match purpose {
            AdmissionPurpose::Initial => self
                .live_auth_revision(platform, remote_user)
                .ok_or(OnlineLobbyError::ManifestDeclarationsPending)?,
            AdmissionPurpose::Reconnect => self
                .committed_auth_peer_lease(remote_user)
                .map(|lease| lease.revision)
                .ok_or(OnlineLobbyError::MissingPeerBinding(remote_user))?,
        };
        if sender.revision == 0 || remote_revision == 0 {
            return Err(OnlineLobbyError::DeclarationRevisionMustIncrease);
        }
        Ok((
            sender,
            self.auth_signal_scope(platform, purpose)?,
            remote_revision,
        ))
    }

    fn live_auth_revision<B: SteamBackend>(
        &self,
        platform: &SteamPlatform<B>,
        user: SteamUserId,
    ) -> Option<u16> {
        if user == self.local_user {
            return self
                .local_declaration
                .map(|declaration| declaration.revision);
        }
        platform
            .roster()
            .iter()
            .flatten()
            .find(|member| member.user == user)
            .and_then(|member| member.loadout)
            .map(|loadout| loadout.revision())
    }

    /// Ends every platform and transport capability attributable to one
    /// remote user, then removes its coordinator binding. This is the
    /// peer-scoped fail-closed path for malformed pre-game signaling: the
    /// lobby owner and unrelated peers remain operational.
    pub fn isolate_peer_authentication<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        user: SteamUserId,
    ) -> Result<Option<PeerId>, OnlineLobbyError> {
        if user == self.local_user {
            return Err(OnlineLobbyError::PeerIdentityMismatch);
        }
        let peer_id = self.binding(user).map(|binding| binding.peer_id);
        let close_result = if let Some(transport) = self.transport.as_mut() {
            transport.close_connections_for_user(user).map(|_| ())
        } else {
            Ok(())
        };

        for slot in &mut self.pending_incoming {
            if slot.is_some_and(|pending| pending.user == user) {
                *slot = None;
            }
        }
        self.endpoints
            .retain(|endpoint| endpoint.remote_user != user);
        self.cancel_issued_ticket_for_user(platform, user);
        let _ = platform.end_peer_authentication(user);
        if let Some(slot) = self
            .bindings
            .iter_mut()
            .find(|slot| slot.as_ref().is_some_and(|binding| binding.user == user))
        {
            *slot = None;
        }

        // Cleanup above is unconditional even if closing the native link
        // itself faults; the transport error then escalates as infrastructure
        // failure rather than leaving an authenticated binding alive.
        close_result?;
        Ok(peer_id)
    }

    /// Starts Steam session-ticket authentication and pre-binds the resulting
    /// platform identity to the authority-assigned protocol peer ID.
    pub fn begin_peer_authentication<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        user: SteamUserId,
        peer_id: PeerId,
        ticket: &[u8],
        purpose: AdmissionPurpose,
        now_ms: u64,
    ) -> Result<(), OnlineLobbyError> {
        let lobby = self.require_lobby()?;
        if self.admission_quiesced {
            return Err(OnlineLobbyError::AdmissionQuiesced);
        }
        peer_id.validate()?;
        if user == self.local_user {
            return Err(OnlineLobbyError::PeerIdentityMismatch);
        }
        if self.is_quality_rejected(user) || self.user_is_retiring(user) {
            return Err(OnlineLobbyError::QualityPolicyRejected);
        }
        if self
            .binding(user)
            .is_some_and(|binding| binding.retiring_connection.is_some())
            && purpose != AdmissionPurpose::Reconnect
        {
            return Err(OnlineLobbyError::DuplicatePeerBinding);
        }
        let binding_preexisted = self.binding(user).is_some();
        if let Some(binding) = self.binding(user)
            && binding.authenticated
        {
            return if binding.peer_id == peer_id
                && binding.admission.is_some_and(|admission| {
                    admission.lobby == lobby
                        && admission.user == user
                        && admission.purpose == purpose
                }) {
                // Reliable Steam pre-game signaling may replay a ticket after
                // this exact identity/purpose already produced a consumed
                // admission. It grants no new capability and is idempotent.
                Ok(())
            } else {
                Err(OnlineLobbyError::DuplicatePeerBinding)
            };
        }
        self.reserve_peer_binding(user, peer_id)?;
        if let Err(error) = platform.begin_peer_authentication(lobby, user, ticket, purpose, now_ms)
        {
            if !binding_preexisted {
                self.remove_unauthenticated_binding(user);
            }
            return Err(error.into());
        }
        if self.role == Some(OnlineLobbyRole::Client) {
            if purpose == AdmissionPurpose::Reconnect && self.reconnect_resume.is_none() {
                self.reconnect_resume = Some(match self.flow.phase() {
                    OnlineLobbyPhase::Countdown => ReconnectResumePhase::Countdown,
                    OnlineLobbyPhase::Fighting => ReconnectResumePhase::Fighting,
                    // A reconnect ticket normally follows the typed disconnect
                    // transition below. Retain a fail-safe Fighting target for
                    // reordered signaling that arrives after Reconnecting.
                    _ => ReconnectResumePhase::Fighting,
                });
            }
            if self.flow.phase() == OnlineLobbyPhase::Lobby {
                self.transition(OnlineLobbyPhase::Connecting, now_ms)?;
            }
            self.transition(OnlineLobbyPhase::Authenticating, now_ms)?;
        }
        Ok(())
    }

    pub fn install_transport(
        &mut self,
        mut transport: SteamTransport,
        now_ms: u64,
    ) -> Result<(), OnlineLobbyError> {
        if self.admission_quiesced {
            return Err(OnlineLobbyError::AdmissionQuiesced);
        }
        if self.transport.is_some() {
            return Err(OnlineLobbyError::TransportAlreadyInstalled);
        }
        let expected = self
            .transport_request
            .or_else(|| self.expected_transport_session())
            .ok_or(OnlineLobbyError::InvalidState)?;
        if transport.session() != expected || transport.local_user() != self.local_user {
            return Err(OnlineLobbyError::TransportSessionMismatch);
        }
        self.transport_request = None;
        if expected.role == SteamTransportRole::ListenAuthority {
            transport.start_listening()?;
        }
        self.relay_status = transport.relay_status();
        self.transport = Some(transport);
        self.try_connect_client(now_ms)
    }

    /// Authority-only immutable match commit. Every Steam member must have a
    /// valid declaration, an authenticated peer binding, and a connected remote
    /// transport before admission is closed.
    pub fn commit_manifest<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        options: OnlineManifestOptions,
        current_tick: SimTick,
        now_ms: u64,
    ) -> Result<(), OnlineLobbyError> {
        self.require_phase(OnlineLobbyPhase::Lobby)?;
        if self.role != Some(OnlineLobbyRole::ListenAuthority) {
            return Err(OnlineLobbyError::InvalidState);
        }
        platform.revalidate_active_lobby_metadata()?;
        self.rebuild_roster(platform)?;
        if !self.roster_all_ready
            || !self.all_remote_members_connected(platform)
            || self.bindings.iter().flatten().any(|binding| {
                binding.connection.is_some() && binding.quality.quality() == NetworkQuality::Reject
            })
        {
            return Err(OnlineLobbyError::PeersNotReady);
        }
        let calibration = self.input_delay_calibration();
        match calibration.state {
            InputDelayCalibrationState::Calibrating => {
                return Err(OnlineLobbyError::InputDelayCalibrationIncomplete);
            }
            InputDelayCalibrationState::Unplayable => {
                return Err(OnlineLobbyError::InputDelayCalibrationUnplayable);
            }
            InputDelayCalibrationState::Ready => {}
            InputDelayCalibrationState::NotAuthority | InputDelayCalibrationState::Committed => {
                return Err(OnlineLobbyError::InvalidState);
            }
        }
        if calibration.selected_input_delay_ticks != Some(options.input_delay_ticks) {
            return Err(OnlineLobbyError::InputDelayCalibrationMismatch);
        }
        if calibration
            .required_rollback_ticks
            .is_none_or(|required| u16::from(options.rollback_limit_ticks) < required)
        {
            return Err(OnlineLobbyError::RollbackBudgetExceeded);
        }
        let metadata = platform
            .lobby_metadata()
            .ok_or(OnlineLobbyError::InvalidState)?;
        let observed_contract = LobbyContract::first_release(metadata)?;
        if self.lobby_contract != Some(observed_contract) {
            return Err(OnlineLobbyError::ManifestMismatch);
        }
        let local_peer = self
            .local_declaration
            .ok_or(OnlineLobbyError::LocalDeclarationMismatch)?
            .peer_id;
        if !FirstReleaseOnlinePolicy::accepts_options(&options)
            || options.authority_peer != Some(local_peer)
            || options.arena != metadata.arena
            || options.rules != metadata.rules
        {
            return Err(OnlineLobbyError::ManifestMismatch);
        }
        let config = self.roster.build_headless_config(options, current_tick)?;
        let manifest_hash = config.manifest.manifest_hash;
        platform.set_accepting_peers(false)?;
        // Keep the auth-gated P2P listen socket alive for same-identity
        // reconnects. Lobby joinability is the new-peer admission gate;
        // stopping this socket would also disable the documented reclaim path.
        self.match_config = Some(config);
        self.capture_committed_peer_leases(platform)?;
        self.committed_input_delay_calibration = Some(InputDelayCalibrationSnapshot {
            state: InputDelayCalibrationState::Committed,
            ..calibration
        });
        self.transition(OnlineLobbyPhase::ManifestAgreement, now_ms)?;
        self.push_event(OnlineLobbyEvent::ManifestCommitted(manifest_hash))
    }

    /// Accepts the exact authority manifest after the AFC handshake. This is
    /// used by clients and by the listen authority's local session.
    pub fn accept_manifest<B: SteamBackend>(
        &mut self,
        platform: &SteamPlatform<B>,
        config: HeadlessMatchConfig,
        now_ms: u64,
    ) -> Result<(), OnlineLobbyError> {
        self.require_phase(OnlineLobbyPhase::ManifestAgreement)?;
        config
            .validate()
            .map_err(|_| OnlineLobbyError::ManifestMismatch)?;
        let metadata_mismatch = self
            .match_config
            .as_ref()
            .is_some_and(|committed| committed.manifest != config.manifest);
        if metadata_mismatch || !FirstReleaseOnlinePolicy::accepts_manifest(&config.manifest) {
            return Err(OnlineLobbyError::ManifestMismatch);
        }
        let contract = self
            .lobby_contract
            .ok_or(OnlineLobbyError::ManifestMismatch)?;
        if !contract.accepts_manifest(&config) {
            return Err(OnlineLobbyError::ManifestMismatch);
        }
        let local_peer = self
            .local_declaration
            .ok_or(OnlineLobbyError::LocalDeclarationMismatch)?
            .peer_id;
        if !config.manifest.ownership.peer_owns_any_seat(local_peer) {
            return Err(OnlineLobbyError::ManifestMismatch);
        }
        let authority_user = self.owner.ok_or(OnlineLobbyError::ManifestMismatch)?;
        let authority_peer = self
            .binding(authority_user)
            .map(|binding| binding.peer_id)
            .ok_or(OnlineLobbyError::ManifestMismatch)?;
        if !config.manifest.ownership.peer_owns_any_seat(authority_peer) {
            return Err(OnlineLobbyError::ManifestMismatch);
        }
        self.validate_exact_manifest_from_platform(platform, &config, authority_peer)?;
        self.match_config = Some(config);
        self.capture_committed_peer_leases(platform)?;
        self.transition(OnlineLobbyPhase::Loading, now_ms)
    }

    /// Reconstructs the complete canonical roster from coherent Steam member
    /// declarations, including members this client never authenticates
    /// directly. Known transport identities are pinned. Remaining manifest
    /// peer groups are matched by their exact ordered couch signature; groups
    /// with identical signatures are intentionally interchangeable.
    fn validate_exact_manifest_from_platform<B: SteamBackend>(
        &self,
        platform: &SteamPlatform<B>,
        config: &HeadlessMatchConfig,
        authority_peer: PeerId,
    ) -> Result<(), OnlineLobbyError> {
        if platform.state() != SteamPlatformState::InLobby(self.require_lobby()?)
            || platform.roster_len() == 0
        {
            return Err(OnlineLobbyError::ManifestMismatch);
        }

        let (groups, group_count) = manifest_peer_groups(&config.manifest)?;
        if group_count != platform.roster_len() {
            return Err(OnlineLobbyError::ManifestMismatch);
        }

        let mut used_groups = [false; MAX_STEAM_LOBBY_MEMBERS];
        let mut assigned_groups = [None; MAX_STEAM_LOBBY_MEMBERS];
        let mut member_count = 0_usize;
        // Reserve every authenticated/retained identity before signature
        // matching. Otherwise an earlier unbound member with an identical
        // couch signature could consume a known member's peer group.
        for (member_index, member) in platform.roster().iter().flatten().enumerate() {
            member_count += 1;
            let (ready, local_seats) = match member.readiness {
                MemberReadiness::Pending => {
                    return Err(OnlineLobbyError::ManifestDeclarationsPending);
                }
                MemberReadiness::Declared { ready, local_seats } => (ready, local_seats),
            };
            let Some(loadout) = member.loadout else {
                return Err(OnlineLobbyError::ManifestDeclarationsPending);
            };
            if !ready || local_seats != loadout.seat_count() {
                return Err(OnlineLobbyError::ManifestMismatch);
            }
            if let Some(peer_id) = self.binding(member.user).map(|binding| binding.peer_id) {
                let group_index = groups[..group_count]
                    .iter()
                    .position(|group| group.is_some_and(|group| group.peer_id == peer_id))
                    .filter(|index| !used_groups[*index])
                    .ok_or(OnlineLobbyError::ManifestMismatch)?;
                let group = groups[group_index].expect("selected manifest peer group exists");
                if !manifest_declaration_matches_group(member.user, loadout, group)? {
                    return Err(OnlineLobbyError::ManifestMismatch);
                }
                used_groups[group_index] = true;
                assigned_groups[member_index] = Some(group_index);
            }
        }
        if member_count != platform.roster_len() {
            return Err(OnlineLobbyError::ManifestMismatch);
        }

        for (member_index, member) in platform.roster().iter().flatten().enumerate() {
            if assigned_groups[member_index].is_some() {
                continue;
            }
            let loadout = member
                .loadout
                .ok_or(OnlineLobbyError::ManifestDeclarationsPending)?;
            let mut matched = None;
            for (index, group) in groups[..group_count].iter().enumerate() {
                let group = group.expect("manifest peer groups have a dense prefix");
                if !used_groups[index]
                    && manifest_declaration_matches_group(member.user, loadout, group)?
                {
                    matched = Some(index);
                    break;
                }
            }
            let group_index = matched.ok_or(OnlineLobbyError::ManifestMismatch)?;
            used_groups[group_index] = true;
            assigned_groups[member_index] = Some(group_index);
        }

        let mut rebuilt = OnlineRoster::default();
        for (member_index, member) in platform.roster().iter().flatten().enumerate() {
            let MemberReadiness::Declared { ready, .. } = member.readiness else {
                return Err(OnlineLobbyError::ManifestDeclarationsPending);
            };
            let loadout = member
                .loadout
                .ok_or(OnlineLobbyError::ManifestDeclarationsPending)?;
            let group_index =
                assigned_groups[member_index].ok_or(OnlineLobbyError::ManifestMismatch)?;
            let group = groups[group_index].expect("assigned manifest peer group exists");
            rebuilt.upsert(decode_member_declaration(
                group.peer_id,
                member.user.authenticated(),
                ready,
                loadout.as_str(),
            )?)?;
        }
        if used_groups[..group_count].iter().any(|used| !used) {
            return Err(OnlineLobbyError::ManifestMismatch);
        }

        let manifest = &config.manifest;
        let options = OnlineManifestOptions {
            match_id: manifest.match_id,
            authority: manifest.authority,
            authority_peer: Some(authority_peer),
            trusted_results: manifest.trusted_results,
            arena: manifest.arena,
            rules: manifest.rules,
            master_gameplay_seed: manifest.master_gameplay_seed,
            agreed_start_tick: manifest.agreed_start_tick,
            input_delay_ticks: manifest.input_delay_ticks,
            rollback_limit_ticks: manifest.rollback_limit_ticks,
            snapshot_history_ticks: manifest.snapshot_history_ticks,
        };
        let canonical = rebuilt
            .build_headless_config(options, SimTick::ZERO)
            .map_err(|_| OnlineLobbyError::ManifestMismatch)?;
        if canonical.manifest != *manifest {
            return Err(OnlineLobbyError::ManifestMismatch);
        }
        Ok(())
    }

    pub fn mark_content_loaded(&mut self, now_ms: u64) -> Result<(), OnlineLobbyError> {
        self.require_phase(OnlineLobbyPhase::Loading)?;
        self.transition(OnlineLobbyPhase::InitialSync, now_ms)
    }

    pub fn mark_initial_sync_complete(&mut self, now_ms: u64) -> Result<(), OnlineLobbyError> {
        self.require_phase(OnlineLobbyPhase::InitialSync)?;
        match self.reconnect_resume.take() {
            Some(ReconnectResumePhase::Countdown) => {
                if self.countdown_start_tick.is_none() {
                    return Err(OnlineLobbyError::ManifestMismatch);
                }
                self.transition(OnlineLobbyPhase::Countdown, now_ms)
            }
            Some(ReconnectResumePhase::Fighting) => {
                self.transition(OnlineLobbyPhase::Fighting, now_ms)
            }
            None => self.transition(OnlineLobbyPhase::Ready, now_ms),
        }
    }

    pub fn begin_countdown(
        &mut self,
        start_tick: SimTick,
        now_ms: u64,
    ) -> Result<(), OnlineLobbyError> {
        self.require_phase(OnlineLobbyPhase::Ready)?;
        let config = self
            .match_config
            .as_ref()
            .ok_or(OnlineLobbyError::ManifestMismatch)?;
        config
            .validate()
            .map_err(|_| OnlineLobbyError::ManifestMismatch)?;
        let contract = self
            .lobby_contract
            .ok_or(OnlineLobbyError::ManifestMismatch)?;
        if !contract.accepts_manifest(config) {
            return Err(OnlineLobbyError::ManifestMismatch);
        }
        let proposed = config.manifest.agreed_start_tick;
        // The manifest boundary is an earliest proposal. The authority chooses
        // the actual countdown only after every client is ready.
        if start_tick.0 < proposed.0 {
            return Err(OnlineLobbyError::ManifestMismatch);
        }
        self.countdown_start_tick = Some(start_tick);
        self.transition(OnlineLobbyPhase::Countdown, now_ms)
    }

    pub fn mark_fighting(
        &mut self,
        current_tick: SimTick,
        now_ms: u64,
    ) -> Result<(), OnlineLobbyError> {
        self.require_phase(OnlineLobbyPhase::Countdown)?;
        let start = self
            .countdown_start_tick
            .ok_or(OnlineLobbyError::ManifestMismatch)?;
        if current_tick.0 < start.0 {
            return Err(OnlineLobbyError::InvalidState);
        }
        self.transition(OnlineLobbyPhase::Fighting, now_ms)
    }

    pub fn begin_result_confirmation(&mut self, now_ms: u64) -> Result<(), OnlineLobbyError> {
        self.require_phase(OnlineLobbyPhase::Fighting)?;
        self.transition(OnlineLobbyPhase::ConfirmingResult, now_ms)
    }

    pub fn confirm_result(&mut self, now_ms: u64) -> Result<(), OnlineLobbyError> {
        self.require_phase(OnlineLobbyPhase::ConfirmingResult)?;
        self.outcome = Some(OnlineMatchOutcome::Confirmed);
        self.transition(OnlineLobbyPhase::Results, now_ms)?;
        self.push_event(OnlineLobbyEvent::MatchEnded(OnlineMatchOutcome::Confirmed))
    }

    /// Applies one authenticated authority-authored terminal for the active
    /// client match. The first valid payload is authoritative for this match;
    /// later native close callbacks may advance cleanup but cannot replace its
    /// failure identity or retry policy.
    pub fn apply_authority_disconnect<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        message: DisconnectMessage,
        now_ms: u64,
    ) -> Result<(), OnlineLobbyError> {
        message.validate()?;
        if self.role != Some(OnlineLobbyRole::Client)
            || self
                .match_config
                .as_ref()
                .map(|config| config.manifest.match_id)
                != message.match_id
        {
            return Err(OnlineLobbyError::Protocol(
                ProtocolValidationError::MatchMismatch,
            ));
        }
        if self.authority_disconnect.is_some() {
            // Reliable retransmission or reordered terminal observation is
            // idempotent. A conflicting later payload never wins.
            return Ok(());
        }

        self.authority_disconnect = Some(message);
        let failure = OnlineFailure::from_disconnect(message);
        match message.retry {
            RetryDisposition::ReconnectAllowed => {
                if let Some(owner) = self.owner
                    && let Some(connection) =
                        self.binding(owner).and_then(|binding| binding.connection)
                {
                    self.prepare_connection_replacement(platform, owner, connection)?;
                }
                self.failure = Some(failure);
                self.reconnect_resume = Some(
                    self.disconnect_resume_phase()
                        .unwrap_or(ReconnectResumePhase::Fighting),
                );
                if self.flow.phase() != OnlineLobbyPhase::Reconnecting {
                    if self
                        .flow
                        .phase()
                        .can_transition_to(OnlineLobbyPhase::Reconnecting)
                    {
                        self.transition(OnlineLobbyPhase::Reconnecting, now_ms)?;
                    } else {
                        self.force_phase(OnlineLobbyPhase::Reconnecting, now_ms)?;
                    }
                }
                Ok(())
            }
            RetryDisposition::ReturnToLobby | RetryDisposition::Fatal => {
                self.fail(platform, failure, now_ms);
                Ok(())
            }
            RetryDisposition::MatchEndedNoContest => {
                self.outcome = Some(OnlineMatchOutcome::NoContestHostLost);
                self.failure = Some(failure);
                self.teardown_match_transport(platform, now_ms)?;
                self.reconnect_resume = None;
                if self.flow.phase() != OnlineLobbyPhase::Results {
                    if self
                        .flow
                        .phase()
                        .can_transition_to(OnlineLobbyPhase::Results)
                    {
                        self.transition(OnlineLobbyPhase::Results, now_ms)?;
                    } else {
                        self.force_phase(OnlineLobbyPhase::Results, now_ms)?;
                    }
                }
                self.push_event(OnlineLobbyEvent::DropGameplayEndpoints)?;
                self.push_event(OnlineLobbyEvent::MatchEnded(
                    OnlineMatchOutcome::NoContestHostLost,
                ))
            }
        }
    }

    pub fn return_to_lobby<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        rematch: bool,
        now_ms: u64,
    ) -> Result<(), OnlineLobbyError> {
        if !matches!(
            self.flow.phase(),
            OnlineLobbyPhase::Results | OnlineLobbyPhase::Failed
        ) {
            return Err(OnlineLobbyError::InvalidState);
        }
        if self.confirmed_result_is_final() && self.role == Some(OnlineLobbyRole::Client) {
            // Only the listen owner authors the between-match epoch. Keep the
            // endpoint alive and retain one bounded UI intent; entering Lobby
            // here would let this client race an Initial ticket into an owner
            // that is still in Results.
            self.pending_results_return.get_or_insert(rematch);
            return Ok(());
        }
        self.complete_return_to_lobby(platform, rematch, now_ms)
    }

    fn complete_return_to_lobby<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        rematch: bool,
        now_ms: u64,
    ) -> Result<(), OnlineLobbyError> {
        let next_declaration = if matches!(platform.state(), SteamPlatformState::InLobby(_)) {
            let current = self
                .local_declaration
                .ok_or(OnlineLobbyError::LocalDeclarationMismatch)?;
            let revision = current
                .revision
                .checked_add(1)
                .ok_or(OnlineLobbyError::DeclarationRevisionExhausted)?;
            Some(OnlineRosterMember::new(
                current.peer_id,
                current.authenticated_user,
                revision,
                false,
                current.seats(),
            )?)
        } else {
            None
        };

        self.transition(OnlineLobbyPhase::ReturningToLobby, now_ms)?;
        self.push_event(OnlineLobbyEvent::DropGameplayEndpoints)?;
        self.teardown_match_transport(platform, now_ms)?;
        self.match_config = None;
        self.committed_input_delay_calibration = None;
        self.countdown_start_tick = None;
        self.outcome = None;
        self.failure = None;
        self.authority_disconnect = None;
        self.reconnect_resume = None;
        self.pending_results_return = None;
        self.quality_rejected_users = [None; MAX_STEAM_LOBBY_MEMBERS];

        if matches!(
            platform.state(),
            SteamPlatformState::Creating | SteamPlatformState::Joining
        ) {
            platform.cancel_pending_lobby_operation()?;
        }
        let SteamPlatformState::InLobby(lobby) = platform.state() else {
            self.clear_lobby_state();
            self.transition(OnlineLobbyPhase::OfflineMenu, now_ms)?;
            return self.push_event(OnlineLobbyEvent::ReturnedToLobby { rematch });
        };
        self.lobby = Some(lobby);
        self.admission_quiesced = false;
        let unready = next_declaration.expect("InLobby prepared an epoch declaration");
        self.publish_declaration(platform, unready)?;
        self.local_declaration = Some(unready);
        if self.role == Some(OnlineLobbyRole::ListenAuthority) {
            platform.set_accepting_peers(true)?;
        }
        self.request_transport()?;
        self.rebuild_roster(platform)?;
        self.transition(OnlineLobbyPhase::Lobby, now_ms)?;
        self.push_event(OnlineLobbyEvent::ReturnedToLobby { rematch })
    }

    pub fn leave_online<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        now_ms: u64,
    ) -> Result<(), OnlineLobbyError> {
        if self.flow.phase() == OnlineLobbyPhase::OfflineMenu {
            if matches!(
                platform.state(),
                SteamPlatformState::Creating | SteamPlatformState::Joining
            ) {
                platform.cancel_pending_lobby_operation()?;
                self.clear_lobby_state();
            }
            return Ok(());
        }
        self.push_event(OnlineLobbyEvent::DropGameplayEndpoints)?;
        self.teardown_match_transport(platform, now_ms)?;
        match platform.state() {
            SteamPlatformState::Creating | SteamPlatformState::Joining => {
                platform.cancel_pending_lobby_operation()?;
            }
            SteamPlatformState::InLobby(_) => {
                if self.retiring_transports.is_empty() {
                    platform.leave_lobby()?;
                } else {
                    self.pending_platform_leave = true;
                }
            }
            SteamPlatformState::Idle | SteamPlatformState::Faulted => {}
        }
        self.clear_lobby_state();
        // Failed may transition directly to OfflineMenu; all active phases that
        // expose this command first normalize through Results/Lobby semantics.
        if self
            .flow
            .phase()
            .can_transition_to(OnlineLobbyPhase::OfflineMenu)
        {
            self.transition(OnlineLobbyPhase::OfflineMenu, now_ms)
        } else {
            self.force_phase(OnlineLobbyPhase::OfflineMenu, now_ms)
        }
    }

    pub fn pump<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        now_ms: u64,
    ) -> Result<(), OnlineLobbyError> {
        self.require_local_platform(platform)?;
        self.advance_time(now_ms)?;
        self.flush_deferred_authority_closes(now_ms)?;
        if let Err(error) = platform.pump(now_ms) {
            let failure = OnlineFailure::from_steam(error);
            self.fail(platform, failure, now_ms);
            return Err(error.into());
        }
        while let Some(event) = platform.poll_event() {
            if let Err(error) = self.handle_platform_event(platform, event, now_ms) {
                let failure = self.failure_for_error(&error);
                self.fail(platform, failure, now_ms);
                return Err(error);
            }
        }

        self.pump_retiring_transports(platform, now_ms)?;
        self.try_complete_pending_platform_leave(platform)?;
        self.refresh_transport_incoming_policy(platform)?;
        if let Some(transport) = &mut self.transport
            && let Err(error) = transport.pump(now_ms)
        {
            let failure = failure_from_transport_error(error);
            self.fail(platform, failure, now_ms);
            return Err(error.into());
        }
        let mut transport_events = Vec::with_capacity(self.config.transport.event_capacity);
        if let Some(transport) = &mut self.transport {
            while let Some(event) = transport.poll_event() {
                transport_events.push(event);
            }
        }
        for event in transport_events {
            if let Err(error) = self.handle_transport_event(platform, event, now_ms) {
                let failure = self.failure_for_error(&error);
                self.fail(platform, failure, now_ms);
                return Err(error);
            }
        }
        self.sample_connection_quality(platform, now_ms)?;

        if let Some(intent) = self.pending_join
            && now_ms >= intent.expires_at_ms
            && self.flow.phase() == OnlineLobbyPhase::InvitePending
        {
            self.pending_join = None;
            let failure = online_failure(
                OnlineFailureCode::LobbyUnavailable,
                OnlineFailureSeverity::Recoverable,
                OnlineRecoveryAction::Retry,
                1,
            );
            self.fail(platform, failure, now_ms);
        } else if self.flow.is_expired(now_ms) {
            let failure = timeout_failure(self.flow.phase());
            if matches!(
                self.flow.phase(),
                OnlineLobbyPhase::CreatingLobby | OnlineLobbyPhase::JoiningLobby
            ) {
                platform.cancel_pending_lobby_operation()?;
            }
            self.fail(platform, failure, now_ms);
        }
        Ok(())
    }

    fn refresh_transport_incoming_policy<B: SteamBackend>(
        &mut self,
        platform: &SteamPlatform<B>,
    ) -> Result<(), OnlineLobbyError> {
        if self.role != Some(OnlineLobbyRole::ListenAuthority) {
            return Ok(());
        }
        if self.admission_quiesced {
            if let Some(transport) = &mut self.transport {
                transport.set_allowed_incoming_users(&[])?;
            }
            return Ok(());
        }
        let mut allowed = [self.local_user; MAX_STEAM_LOBBY_MEMBERS];
        let mut count = 0_usize;
        if self.match_config.is_some() {
            // Once the manifest is committed, bindings are immutable leases.
            // Do not let a delayed/temporarily incomplete lobby roster callback
            // prevent the same Steam identity from reclaiming its connection.
            for binding in self.bindings.iter().flatten() {
                if binding.user != self.local_user
                    && !binding.retiring
                    && !self.is_quality_rejected(binding.user)
                {
                    allowed[count] = binding.user;
                    count += 1;
                }
            }
        } else {
            // Before commit, only the current coherent Steam lobby roster may
            // consume scarce native listen-socket connection state.
            for member in platform.roster().iter().flatten() {
                if member.user != self.local_user
                    && !self.user_is_retiring(member.user)
                    && !self.is_quality_rejected(member.user)
                {
                    allowed[count] = member.user;
                    count += 1;
                }
            }
        }
        if let Some(transport) = &mut self.transport {
            transport.set_allowed_incoming_users(&allowed[..count])?;
        }
        Ok(())
    }

    fn handle_platform_event<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        event: SteamPlatformEvent,
        now_ms: u64,
    ) -> Result<(), OnlineLobbyError> {
        match event {
            SteamPlatformEvent::LobbyJoinRequested(intent) => {
                self.pending_join = Some(intent);
                if self.flow.phase() == OnlineLobbyPhase::OfflineMenu {
                    self.transition(OnlineLobbyPhase::InvitePending, now_ms)?;
                }
                self.push_event(OnlineLobbyEvent::JoinRequested(intent))
            }
            SteamPlatformEvent::LobbyEntered { lobby, owner } => {
                if !matches!(
                    self.flow.phase(),
                    OnlineLobbyPhase::CreatingLobby | OnlineLobbyPhase::JoiningLobby
                ) {
                    return Err(OnlineLobbyError::InvalidState);
                }
                self.lobby = Some(lobby);
                self.owner = Some(owner);
                self.role = Some(if owner == self.local_user {
                    OnlineLobbyRole::ListenAuthority
                } else {
                    OnlineLobbyRole::Client
                });
                let metadata = platform
                    .lobby_metadata()
                    .ok_or(OnlineLobbyError::InvalidState)?;
                self.lobby_contract = Some(LobbyContract::first_release(metadata)?);
                let declaration = self
                    .local_declaration
                    .ok_or(OnlineLobbyError::LocalDeclarationMismatch)?;
                self.publish_declaration(platform, declaration)?;
                self.rebuild_roster(platform)?;
                self.transition(OnlineLobbyPhase::Lobby, now_ms)?;
                self.request_transport()?;
                self.push_event(OnlineLobbyEvent::LobbyEntered {
                    lobby,
                    owner,
                    role: self.role.expect("role was just installed"),
                })
            }
            SteamPlatformEvent::LobbyCreateFailed(error) => Err(error.into()),
            SteamPlatformEvent::LobbyJoinRejected { reason, .. } => Err(reason.into()),
            SteamPlatformEvent::LobbyRosterChanged { lobby } => {
                if Some(lobby) != self.lobby {
                    return Err(OnlineLobbyError::InvalidState);
                }
                self.reconcile_departed_lobby_bindings(platform, now_ms)?;
                self.rebuild_roster(platform)
            }
            SteamPlatformEvent::LobbyMetadataChanged { lobby } => {
                if Some(lobby) != self.lobby {
                    return Err(OnlineLobbyError::InvalidState);
                }
                let metadata = platform
                    .lobby_metadata()
                    .ok_or(OnlineLobbyError::InvalidState)?;
                let observed_contract = LobbyContract::first_release(metadata)?;
                if self.lobby_contract != Some(observed_contract) {
                    return Err(OnlineLobbyError::ManifestMismatch);
                }
                self.rebuild_roster(platform)
            }
            SteamPlatformEvent::LobbyMemberDataChanged {
                lobby,
                user,
                outcome,
            } => {
                if Some(lobby) != self.lobby {
                    return Err(OnlineLobbyError::InvalidState);
                }
                match outcome {
                    MemberDataOutcome::Staging | MemberDataOutcome::Accepted => {
                        self.rebuild_roster(platform)?;
                        self.follow_owner_between_match_epoch(platform, user, now_ms)
                    }
                    MemberDataOutcome::Rejected(reason) => {
                        let failure = failure_for_member_declaration(reason);
                        self.handle_member_declaration_rejection(platform, user, failure, now_ms)
                    }
                }
            }
            SteamPlatformEvent::LobbyLeft { reason, .. } => {
                if reason == LobbyExitReason::AuthorityLost {
                    // Current SteamPlatform owner transfer emits only the
                    // atomic AuthorityLost event while remaining InLobby. A
                    // legacy/defensive LobbyLeft cannot retain that identity.
                    if self.confirmed_result_is_final() {
                        self.teardown_confirmed_result_transport(platform, now_ms)?;
                        self.clear_lobby_identity_only();
                        return Ok(());
                    }
                    if self.host_loss_ends_match() {
                        self.end_no_contest_host_lost(platform, now_ms)?;
                        self.clear_lobby_identity_only();
                        return Ok(());
                    }
                    self.teardown_match_transport(platform, now_ms)?;
                    self.clear_lobby_identity_only();
                    self.fail(
                        platform,
                        online_failure(
                            OnlineFailureCode::AuthorityLost,
                            OnlineFailureSeverity::Recoverable,
                            OnlineRecoveryAction::ReturnToMenu,
                            4,
                        ),
                        now_ms,
                    );
                    Ok(())
                } else if let Some(failure) = OnlineFailure::from_lobby_exit(reason) {
                    self.fail(platform, failure, now_ms);
                    Ok(())
                } else {
                    self.clear_lobby_state();
                    self.force_phase(OnlineLobbyPhase::OfflineMenu, now_ms)
                }
            }
            SteamPlatformEvent::AuthorityLost {
                lobby,
                previous_authority,
                successor,
            } => {
                if self.lobby != Some(lobby)
                    || self.owner != Some(previous_authority)
                    || successor == previous_authority
                    || platform.state() != SteamPlatformState::InLobby(lobby)
                    || platform.lobby_owner() != Some(successor)
                    || !platform
                        .roster()
                        .iter()
                        .flatten()
                        .any(|member| member.user == successor)
                {
                    return Err(OnlineLobbyError::InvalidState);
                }
                let metadata = platform
                    .lobby_metadata()
                    .ok_or(OnlineLobbyError::InvalidState)?;
                let observed_contract = LobbyContract::first_release(metadata)?;
                if self.lobby_contract != Some(observed_contract) {
                    return Err(OnlineLobbyError::ManifestMismatch);
                }

                self.owner = Some(successor);
                self.role = Some(if successor == self.local_user {
                    OnlineLobbyRole::ListenAuthority
                } else {
                    OnlineLobbyRole::Client
                });
                self.reconnect_resume = None;

                if self.confirmed_result_is_final() {
                    self.teardown_confirmed_result_transport(platform, now_ms)?;
                } else if self.host_loss_ends_match() {
                    self.end_no_contest_host_lost(platform, now_ms)?;
                } else {
                    self.teardown_match_transport(platform, now_ms)?;
                    self.fail(
                        platform,
                        online_failure(
                            OnlineFailureCode::AuthorityLost,
                            OnlineFailureSeverity::Recoverable,
                            OnlineRecoveryAction::ReturnToLobby,
                            4,
                        ),
                        now_ms,
                    );
                }
                self.rebuild_roster(platform)
            }
            SteamPlatformEvent::RichPresenceUnavailable => {
                self.push_event(OnlineLobbyEvent::RichPresenceUnavailable)
            }
            SteamPlatformEvent::AuthTicketReady { handle } => {
                let Some(index) = self
                    .issued_tickets
                    .iter()
                    .position(|record| record.lease.handle == handle)
                else {
                    let _ = platform.cancel_auth_ticket(handle);
                    return Ok(());
                };
                let lease = self.issued_tickets[index].lease;
                if self.admission_quiesced || !self.auth_ticket_lease_is_current(platform, lease) {
                    let _ = platform.cancel_auth_ticket(handle);
                    self.issued_tickets.swap_remove(index);
                    return Ok(());
                }
                self.issued_tickets[index].ready = true;
                self.push_event(OnlineLobbyEvent::AuthTicketReady {
                    handle,
                    remote_user: lease.remote_user,
                })
            }
            SteamPlatformEvent::AuthTicketRejected {
                handle,
                remote_user,
            } => {
                let Some(index) = self.issued_tickets.iter().position(|record| {
                    record.lease.handle == handle && record.lease.remote_user == remote_user
                }) else {
                    return Ok(());
                };
                let lease = self.issued_tickets[index].lease;
                if self.admission_quiesced || !self.auth_ticket_lease_is_current(platform, lease) {
                    self.issued_tickets.swap_remove(index);
                    return Ok(());
                }
                self.issued_tickets.swap_remove(index);
                let failure = online_failure(
                    OnlineFailureCode::AuthenticationFailed,
                    OnlineFailureSeverity::Fatal,
                    OnlineRecoveryAction::ReturnToMenu,
                    0,
                );
                self.handle_attributed_authentication_rejection(
                    platform,
                    remote_user,
                    failure,
                    now_ms,
                )
            }
            SteamPlatformEvent::PeerAuthenticated { lobby, user } => {
                if Some(lobby) != self.lobby {
                    return Err(OnlineLobbyError::InvalidState);
                }
                if self.admission_quiesced {
                    let _ = platform.end_peer_authentication(user);
                    if let Some(binding) = self.binding_mut(user)
                        && binding.connection.is_none()
                    {
                        binding.authenticated = false;
                        binding.admission = None;
                        binding.pending_connection = None;
                    }
                    return Ok(());
                }
                let admission = platform.consume_authenticated_admission(lobby, user, now_ms)?;
                let binding = self
                    .binding_mut(user)
                    .ok_or(OnlineLobbyError::MissingPeerBinding(user))?;
                if admission.authenticated_user.get() != user.get() {
                    return Err(OnlineLobbyError::PeerIdentityMismatch);
                }
                binding.authenticated = true;
                binding.admission = Some(admission);
                let peer_id = binding.peer_id;
                self.rebuild_roster(platform)?;
                self.push_event(OnlineLobbyEvent::PeerAuthenticated {
                    user,
                    peer_id,
                    reconnect: admission.purpose == AdmissionPurpose::Reconnect,
                })?;
                if self.role == Some(OnlineLobbyRole::Client) {
                    self.try_connect_client(now_ms)
                } else {
                    self.try_admit_incoming(user, now_ms)
                }
            }
            SteamPlatformEvent::PeerAuthenticationRejected {
                lobby,
                user,
                reason,
            } => {
                if Some(lobby) != self.lobby {
                    return Err(OnlineLobbyError::InvalidState);
                }
                if self.admission_quiesced {
                    if let Some(binding) = self.binding_mut(user)
                        && binding.connection.is_none()
                    {
                        binding.authenticated = false;
                        binding.admission = None;
                        binding.pending_connection = None;
                    }
                    return Ok(());
                }
                let failure = OnlineFailure::from_auth_rejection(reason);
                self.handle_attributed_authentication_rejection(platform, user, failure, now_ms)
            }
        }
    }

    fn handle_member_declaration_rejection<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        user: SteamUserId,
        failure: OnlineFailure,
        now_ms: u64,
    ) -> Result<(), OnlineLobbyError> {
        if user == self.local_user {
            let local_failure = OnlineFailure {
                recovery: OnlineRecoveryAction::ReturnToMenu,
                ..failure
            };
            self.push_event(OnlineLobbyEvent::PeerAuthenticationRejected {
                user,
                connection: None,
                failure: local_failure,
            })?;
            self.rebuild_roster(platform)?;
            self.fail(platform, local_failure, now_ms);
            return Ok(());
        }

        match self.role {
            Some(OnlineLobbyRole::ListenAuthority) => {
                self.handle_attributed_authentication_rejection(platform, user, failure, now_ms)
            }
            Some(OnlineLobbyRole::Client) if Some(user) == self.owner => {
                self.handle_attributed_authentication_rejection(platform, user, failure, now_ms)
            }
            Some(OnlineLobbyRole::Client) => {
                // A client has no authority over an unrelated member's Steam
                // authentication capability. The platform projection already
                // marks that declaration pending; only rebuild the local
                // roster and let the listen authority perform isolation.
                self.rebuild_roster(platform)
            }
            None => Err(OnlineLobbyError::InvalidState),
        }
    }

    fn handle_attributed_authentication_rejection<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        user: SteamUserId,
        failure: OnlineFailure,
        now_ms: u64,
    ) -> Result<(), OnlineLobbyError> {
        let client_owner_rejected =
            self.role == Some(OnlineLobbyRole::Client) && Some(user) == self.owner;
        let local_rejected = user == self.local_user;
        let remote_rejected = user != self.local_user
            && platform
                .roster()
                .iter()
                .flatten()
                .any(|member| member.user == user);
        if !local_rejected && !remote_rejected {
            return Err(OnlineLobbyError::InvalidState);
        }
        let connection = self.active_connection_for_user(user);
        let isolate_result = if local_rejected {
            Ok(None)
        } else {
            self.isolate_peer_authentication(platform, user)
        };

        // The application must observe this while it still owns its
        // user-to-peer binding so an active listen authority can revoke the
        // canonical identity. The following roster snapshot is the
        // cleanup-before-reallocation barrier for every upper layer.
        self.push_event(OnlineLobbyEvent::PeerAuthenticationRejected {
            user,
            connection,
            failure,
        })?;
        self.rebuild_roster(platform)?;
        isolate_result?;

        if client_owner_rejected || local_rejected {
            self.fail(platform, failure, now_ms);
        }
        Ok(())
    }

    fn handle_transport_event<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        event: SteamTransportEvent,
        now_ms: u64,
    ) -> Result<(), OnlineLobbyError> {
        match event {
            SteamTransportEvent::RelayStatusChanged(status) => {
                self.relay_status = status;
                Ok(())
            }
            SteamTransportEvent::IncomingPending {
                connection,
                lobby,
                user,
                ..
            } => {
                if Some(lobby) != self.lobby || self.role != Some(OnlineLobbyRole::ListenAuthority)
                {
                    return Err(OnlineLobbyError::TransportSessionMismatch);
                }
                if self.admission_quiesced || self.user_is_retiring(user) {
                    self.transport
                        .as_mut()
                        .ok_or(OnlineLobbyError::TransportNotInstalled)?
                        .reject_incoming(connection)?;
                    return Ok(());
                }
                let admission_identity_is_allowed = if self.match_config.is_some() {
                    self.binding(user).is_some_and(|binding| !binding.retiring)
                } else {
                    platform
                        .roster()
                        .iter()
                        .flatten()
                        .any(|member| member.user == user)
                };
                if !admission_identity_is_allowed {
                    self.transport
                        .as_mut()
                        .ok_or(OnlineLobbyError::TransportNotInstalled)?
                        .reject_incoming(connection)?;
                    return Ok(());
                }
                if self.is_quality_rejected(user) {
                    self.transport
                        .as_mut()
                        .ok_or(OnlineLobbyError::TransportNotInstalled)?
                        .close_connection_for_quality_policy(connection)?;
                    return Ok(());
                }
                let slot = self
                    .pending_incoming
                    .iter_mut()
                    .find(|slot| slot.is_none())
                    .ok_or(OnlineLobbyError::EndpointQueueOverflow)?;
                *slot = Some(PendingIncoming { connection, user });
                if self
                    .binding(user)
                    .is_some_and(|binding| binding.admission.is_some())
                {
                    self.try_admit_incoming(user, now_ms)
                } else {
                    self.push_event(OnlineLobbyEvent::AuthenticationRequired {
                        user,
                        reconnect: self.disconnect_resume_phase().is_some(),
                    })
                }
            }
            SteamTransportEvent::IncomingRejected { .. } => Ok(()),
            SteamTransportEvent::ConnectionReady {
                connection,
                lobby,
                user,
            } => {
                if Some(lobby) != self.lobby {
                    return Err(OnlineLobbyError::TransportSessionMismatch);
                }
                if self.admission_quiesced {
                    self.transport
                        .as_mut()
                        .ok_or(OnlineLobbyError::TransportNotInstalled)?
                        .close_connection(connection)?;
                    return Ok(());
                }
                let endpoint = self
                    .transport
                    .as_mut()
                    .ok_or(OnlineLobbyError::TransportNotInstalled)?
                    .take_endpoint(connection)?;
                if self.endpoints.len() >= MAX_STEAM_LOBBY_MEMBERS {
                    return Err(OnlineLobbyError::EndpointQueueOverflow);
                }
                let precommit = self.match_config.is_none();
                let binding = self
                    .binding_mut(user)
                    .ok_or(OnlineLobbyError::MissingPeerBinding(user))?;
                if !binding.authenticated || binding.admission.is_none() {
                    return Err(OnlineLobbyError::MissingAuthenticatedAdmission);
                }
                binding.connection = Some(connection);
                binding.pending_connection = None;
                if precommit {
                    // A native connection becoming ready defines a fresh
                    // precommit sampling generation. No RTT from a replaced
                    // connection may influence the immutable match manifest.
                    binding.precommit_rtt.reset();
                }
                binding.authority_terminal_cleanup = None;
                binding.deferred_authority_close = None;
                let peer_id = binding.peer_id;
                let reconnect = binding
                    .admission
                    .is_some_and(|admission| admission.purpose == AdmissionPurpose::Reconnect);
                self.endpoints.push_back(endpoint);
                if self.role == Some(OnlineLobbyRole::Client) {
                    if reconnect || self.reconnect_resume.is_some() {
                        if self.reconnect_resume.is_none() {
                            self.reconnect_resume = Some(ReconnectResumePhase::Fighting);
                        }
                        self.failure = None;
                        self.authority_disconnect = None;
                        self.transition(OnlineLobbyPhase::InitialSync, now_ms)?;
                    } else {
                        self.transition(OnlineLobbyPhase::ManifestAgreement, now_ms)?;
                    }
                }
                self.push_event(OnlineLobbyEvent::EndpointReady {
                    connection,
                    user,
                    peer_id,
                    reconnect,
                })
            }
            SteamTransportEvent::ConnectionClosed {
                connection,
                lobby,
                user,
                reason,
            } => {
                if Some(lobby) != self.lobby {
                    return Ok(());
                }
                if let Some(slot) = self.pending_incoming.iter_mut().find(|slot| {
                    slot.is_some_and(|pending| {
                        pending.user == user && pending.connection == connection
                    })
                }) {
                    *slot = None;
                }
                if let Some(binding) = self.binding_mut(user)
                    && binding.retiring_connection == Some(connection)
                {
                    binding.retiring_connection = None;
                    if binding.authority_terminal_cleanup == Some(connection) {
                        binding.authority_terminal_cleanup = None;
                    }
                    if binding
                        .deferred_authority_close
                        .is_some_and(|closed| closed.connection == connection)
                    {
                        binding.deferred_authority_close = None;
                    }
                    // This exact generation was superseded only after an
                    // authenticated ReconnectAllowed terminal. Its delayed
                    // close cannot end replacement auth or clear the new link.
                    return Ok(());
                }
                let defer_authority_close = self.role == Some(OnlineLobbyRole::ListenAuthority)
                    && self.match_config.is_some();
                let (peer_id, was_connected, still_has_link, terminal_cleanup, deferred) =
                    match self.binding_mut(user) {
                        Some(binding) => {
                            let was_connected = binding.connection == Some(connection);
                            let was_pending = binding.pending_connection == Some(connection);
                            if !was_connected && !was_pending {
                                return Ok(());
                            }
                            if was_connected {
                                binding.connection = None;
                            }
                            if was_pending {
                                binding.pending_connection = None;
                            }
                            let still_has_link = binding.connection.is_some()
                                || binding.pending_connection.is_some();
                            if !still_has_link {
                                binding.admission = None;
                                binding.authenticated = false;
                            }
                            let terminal_cleanup =
                                binding.authority_terminal_cleanup == Some(connection);
                            if terminal_cleanup {
                                binding.authority_terminal_cleanup = None;
                            }
                            let deferred =
                                defer_authority_close && was_connected && !terminal_cleanup;
                            if deferred {
                                binding.deferred_authority_close =
                                    Some(DeferredAuthorityClose { connection, reason });
                            }
                            (
                                binding.peer_id,
                                was_connected,
                                still_has_link,
                                terminal_cleanup,
                                deferred,
                            )
                        }
                        None => return Ok(()),
                    };
                if still_has_link {
                    return Ok(());
                }
                let _ = platform.end_peer_authentication(user);
                self.cancel_issued_ticket_for_user(platform, user);
                if terminal_cleanup || deferred {
                    return Ok(());
                }
                if !was_connected
                    && matches!(
                        reason,
                        SteamTransportCloseReason::AdmissionRejected
                            | SteamTransportCloseReason::AdmissionTimedOut
                            | SteamTransportCloseReason::QualityPolicyRejected
                    )
                {
                    return Ok(());
                }
                self.finish_peer_disconnect(connection, user, peer_id, reason, now_ms)
            }
        }
    }

    fn publish_declaration<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        declaration: OnlineRosterMember,
    ) -> Result<(), OnlineLobbyError> {
        self.validate_local_declaration_identity(declaration)?;
        let encoded = encode_member_declaration(&declaration);
        let steam_declaration = MemberLoadoutDeclaration::new(&encoded)?;
        platform.set_member_declaration(steam_declaration, declaration.ready)?;
        Ok(())
    }

    fn rebuild_roster<B: SteamBackend>(
        &mut self,
        platform: &SteamPlatform<B>,
    ) -> Result<(), OnlineLobbyError> {
        let mut rebuilt = OnlineRoster::default();
        self.lobby_member_count = platform.roster_len();
        for member in platform.roster().iter().flatten() {
            let Some(binding) = self.binding(member.user) else {
                continue;
            };
            if !binding.authenticated || binding.retiring {
                continue;
            }
            let (ready, local_seats) = match member.readiness {
                MemberReadiness::Pending => continue,
                MemberReadiness::Declared { ready, local_seats } => (ready, local_seats),
            };
            let Some(loadout) = member.loadout else {
                continue;
            };
            if loadout.seat_count() != local_seats {
                return Err(OnlineLobbyError::ManifestMismatch);
            }
            let declaration = decode_member_declaration(
                binding.peer_id,
                member.user.authenticated(),
                ready,
                loadout.as_str(),
            )?;
            rebuilt.upsert(declaration)?;
        }
        self.roster = rebuilt;
        self.platform_total_seats = usize::from(platform.accepted_seat_total());
        self.seat_capacity = platform.seat_capacity().unwrap_or(0);
        self.effective_joinable = platform.effective_joinable();
        self.roster_all_ready = platform.all_members_match_ready()
            && self.roster.len() == self.lobby_member_count
            && self.roster.total_seats() > 0;
        self.push_event(OnlineLobbyEvent::RosterChanged {
            members: self.roster.len().min(u8::MAX as usize) as u8,
            seats: self.platform_total_seats.min(u8::MAX as usize) as u8,
            all_ready: self.roster_all_ready,
            live_bindings: std::array::from_fn(|index| {
                self.bindings[index]
                    .as_ref()
                    .map(|binding| OnlinePeerIdentity {
                        user: binding.user,
                        peer_id: binding.peer_id,
                    })
            }),
        })
    }

    fn capture_committed_peer_leases<B: SteamBackend>(
        &mut self,
        platform: &SteamPlatform<B>,
    ) -> Result<(), OnlineLobbyError> {
        let mut revisions = [None; MAX_STEAM_LOBBY_MEMBERS];
        for (index, member) in platform.roster().iter().flatten().enumerate() {
            let revision = member
                .loadout
                .ok_or(OnlineLobbyError::ManifestDeclarationsPending)?
                .revision();
            revisions[index] = Some(CommittedMemberRevision {
                user: member.user,
                revision,
            });
        }
        let mut leases = [None; MAX_STEAM_LOBBY_MEMBERS];
        let mut count = 0_usize;
        for binding in self.bindings.iter().flatten() {
            let revision = revisions
                .iter()
                .flatten()
                .find(|record| record.user == binding.user)
                .map(|record| record.revision)
                .ok_or(OnlineLobbyError::ManifestDeclarationsPending)?;
            leases[count] = Some(AuthPeerLease {
                user: binding.user,
                peer_id: binding.peer_id,
                revision,
            });
            count += 1;
        }
        self.committed_peer_leases = leases;
        self.committed_member_revisions = revisions;
        self.pending_results_return = None;
        Ok(())
    }

    fn follow_owner_between_match_epoch<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        changed_user: SteamUserId,
        now_ms: u64,
    ) -> Result<(), OnlineLobbyError> {
        let Some(owner) = self.owner else {
            return Ok(());
        };
        if self.role != Some(OnlineLobbyRole::Client)
            || changed_user != owner
            || !self.confirmed_result_is_final()
        {
            return Ok(());
        }
        let Some(member) = platform
            .roster()
            .iter()
            .flatten()
            .find(|member| member.user == owner)
        else {
            return Ok(());
        };
        let MemberReadiness::Declared { ready: false, .. } = member.readiness else {
            return Ok(());
        };
        let Some(loadout) = member.loadout else {
            return Ok(());
        };
        if !self.initial_authentication_allowed(owner, loadout.revision()) {
            return Ok(());
        }
        let rematch = self.pending_results_return.unwrap_or(false);
        self.complete_return_to_lobby(platform, rematch, now_ms)
    }

    /// Reclaims only mutable, pre-commit identity reservations whose Steam
    /// members have actually departed. A committed match retains its fixed
    /// identity map even when a member is temporarily absent so reconnect
    /// grace and same-identity reclaim remain authoritative.
    fn reconcile_departed_lobby_bindings<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        now_ms: u64,
    ) -> Result<(), OnlineLobbyError> {
        let mut departed = [None; MAX_STEAM_LOBBY_MEMBERS];
        let mut departed_len = 0_usize;
        for binding in self.bindings.iter().flatten() {
            if binding.user != self.local_user
                && !platform
                    .roster()
                    .iter()
                    .flatten()
                    .any(|member| member.user == binding.user)
            {
                departed[departed_len] = Some(binding.user);
                departed_len += 1;
            }
        }

        let mut first_error = None;
        for user in departed[..departed_len].iter().flatten().copied() {
            if self.match_config.is_some() {
                if let Err(error) = self.suspend_committed_peer(platform, user, now_ms)
                    && first_error.is_none()
                {
                    first_error = Some(error);
                }
            } else {
                if let Err(error) = self.isolate_peer_authentication(platform, user)
                    && first_error.is_none()
                {
                    first_error = Some(error);
                }
                // Quality rejection remains fail-closed while the user is an
                // active lobby member (and throughout a fixed match), but an
                // absent pre-commit identity has already lost admission at the
                // platform roster gate.
                for slot in &mut self.quality_rejected_users {
                    if *slot == Some(user) {
                        *slot = None;
                    }
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn suspend_committed_peer<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        user: SteamUserId,
        now_ms: u64,
    ) -> Result<(), OnlineLobbyError> {
        let (peer_id, active_connection) = self
            .binding(user)
            .map(|binding| {
                (
                    binding.peer_id,
                    binding.connection.or(binding.pending_connection),
                )
            })
            .ok_or(OnlineLobbyError::MissingPeerBinding(user))?;

        // Native link invalidation comes first. The coordinator then drops
        // every live auth/ticket/endpoint capability while retaining only the
        // immutable user-to-peer lease and its quality history.
        let close_result = if let Some(transport) = self.transport.as_mut() {
            transport.close_connections_for_user(user).map(|_| ())
        } else {
            Ok(())
        };
        for slot in &mut self.pending_incoming {
            if slot.is_some_and(|pending| pending.user == user) {
                *slot = None;
            }
        }
        self.endpoints
            .retain(|endpoint| endpoint.remote_user != user);
        self.cancel_issued_ticket_for_user(platform, user);
        let _ = platform.end_peer_authentication(user);
        let binding = self
            .binding_mut(user)
            .ok_or(OnlineLobbyError::MissingPeerBinding(user))?;
        binding.authenticated = false;
        binding.admission = None;
        binding.pending_connection = None;
        binding.connection = None;
        binding.retiring_connection = None;

        if let Some(connection) = active_connection {
            self.finish_peer_disconnect(
                connection,
                user,
                peer_id,
                SteamTransportCloseReason::Requested,
                now_ms,
            )?;
        }
        close_result?;
        Ok(())
    }

    fn disconnect_resume_phase(&self) -> Option<ReconnectResumePhase> {
        match self.flow.phase() {
            OnlineLobbyPhase::Countdown => Some(ReconnectResumePhase::Countdown),
            OnlineLobbyPhase::Fighting => Some(ReconnectResumePhase::Fighting),
            OnlineLobbyPhase::Reconnecting => self
                .reconnect_resume
                .or(Some(ReconnectResumePhase::Fighting)),
            _ => None,
        }
    }

    fn flush_deferred_authority_closes(&mut self, now_ms: u64) -> Result<(), OnlineLobbyError> {
        let mut deferred: [Option<(SteamUserId, PeerId, DeferredAuthorityClose)>;
            MAX_STEAM_LOBBY_MEMBERS] = [None; MAX_STEAM_LOBBY_MEMBERS];
        let mut count = 0_usize;
        for binding in self.bindings.iter_mut().flatten() {
            let Some(closed) = binding.deferred_authority_close.take() else {
                continue;
            };
            deferred[count] = Some((binding.user, binding.peer_id, closed));
            count += 1;
        }
        for (user, peer_id, closed) in deferred[..count].iter().flatten().copied() {
            self.finish_peer_disconnect(closed.connection, user, peer_id, closed.reason, now_ms)?;
        }
        Ok(())
    }

    fn finish_peer_disconnect(
        &mut self,
        connection: SteamConnectionId,
        user: SteamUserId,
        peer_id: PeerId,
        reason: SteamTransportCloseReason,
        now_ms: u64,
    ) -> Result<(), OnlineLobbyError> {
        if self.role == Some(OnlineLobbyRole::Client)
            && Some(user) == self.owner
            && self.confirmed_result_is_final()
        {
            // The verified result already ended gameplay. A close callback now
            // only retires its physical keepalive and must not manufacture a
            // reconnect/failure while both users remain in the Steam lobby.
            self.reconnect_resume = None;
            return Ok(());
        }
        let resume = self.disconnect_resume_phase();
        self.push_event(OnlineLobbyEvent::PeerDisconnected {
            connection,
            user,
            peer_id,
            reconnect_allowed: resume.is_some(),
        })?;
        if self.role == Some(OnlineLobbyRole::Client) && Some(user) == self.owner {
            if let Some(resume) = resume {
                self.reconnect_resume = Some(resume);
                if self.authority_disconnect.is_none() {
                    self.failure = Some(online_failure(
                        OnlineFailureCode::ConnectionTimedOut,
                        OnlineFailureSeverity::Recoverable,
                        OnlineRecoveryAction::Reconnect,
                        reason as u16,
                    ));
                }
                if self.flow.phase() != OnlineLobbyPhase::Reconnecting {
                    self.transition(OnlineLobbyPhase::Reconnecting, now_ms)?;
                }
            } else if reason != SteamTransportCloseReason::Requested {
                return Err(OnlineLobbyError::Transport(transport_error_for_close(
                    reason,
                )));
            }
        }
        Ok(())
    }

    fn all_remote_members_connected<B: SteamBackend>(&self, platform: &SteamPlatform<B>) -> bool {
        platform.roster().iter().flatten().all(|member| {
            member.user == self.local_user
                || self
                    .binding(member.user)
                    .is_some_and(|binding| binding.authenticated && binding.connection.is_some())
        })
    }

    fn request_transport(&mut self) -> Result<(), OnlineLobbyError> {
        if self.admission_quiesced {
            return Err(OnlineLobbyError::AdmissionQuiesced);
        }
        let session = self
            .expected_transport_session()
            .ok_or(OnlineLobbyError::InvalidState)?;
        self.transport_request = Some(session);
        self.push_event(OnlineLobbyEvent::TransportRequested(session))
    }

    fn expected_transport_session(&self) -> Option<SteamP2pSession> {
        Some(SteamP2pSession {
            lobby: self.lobby?,
            authority_user: self.owner?,
            role: match self.role? {
                OnlineLobbyRole::ListenAuthority => SteamTransportRole::ListenAuthority,
                OnlineLobbyRole::Client => SteamTransportRole::Client,
            },
            virtual_port: self.config.virtual_port,
        })
    }

    fn prepare_connection_replacement<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        user: SteamUserId,
        connection: SteamConnectionId,
    ) -> Result<(), OnlineLobbyError> {
        let binding = self
            .binding(user)
            .ok_or(OnlineLobbyError::MissingPeerBinding(user))?;
        if binding.connection != Some(connection)
            || binding.pending_connection.is_some()
            || binding.retiring_connection.is_some()
        {
            return Err(OnlineLobbyError::InvalidState);
        }
        self.transport
            .as_mut()
            .ok_or(OnlineLobbyError::TransportNotInstalled)?
            .mark_connection_replacement_eligible(connection)?;

        self.endpoints
            .retain(|endpoint| endpoint.connection != connection);
        let binding = self
            .binding_mut(user)
            .expect("replacement binding was just validated");
        binding.connection = None;
        binding.retiring_connection = Some(connection);
        binding.authenticated = false;
        binding.admission = None;
        binding.authority_terminal_cleanup = None;
        binding.deferred_authority_close = None;

        // Steam authentication is identity-scoped. End the old capability
        // before accepting a fresh reconnect ticket, while the exact old
        // physical generation remains outbound-drain-only in the transport.
        let _ = platform.end_peer_authentication(user);
        self.cancel_issued_ticket_for_user(platform, user);
        Ok(())
    }

    fn try_connect_client(&mut self, now_ms: u64) -> Result<(), OnlineLobbyError> {
        if self.admission_quiesced {
            return Ok(());
        }
        if self.role != Some(OnlineLobbyRole::Client) {
            return Ok(());
        }
        let owner = self.owner.ok_or(OnlineLobbyError::InvalidState)?;
        let Some(binding) = self.binding(owner) else {
            return Ok(());
        };
        if binding.retiring || binding.connection.is_some() || binding.pending_connection.is_some()
        {
            return Ok(());
        }
        let Some(admission) = binding.admission else {
            return Ok(());
        };
        let Some(transport) = self.transport.as_mut() else {
            // Authentication and native transport construction complete on
            // independent callback turns. Retain the approved admission and
            // let `install_transport` start the connection when its factory
            // result arrives.
            return Ok(());
        };
        let connection = transport.connect_p2p(admission, now_ms)?;
        if let Some(binding) = self.binding_mut(owner) {
            binding.pending_connection = Some(connection);
        }
        Ok(())
    }

    fn try_admit_incoming(
        &mut self,
        user: SteamUserId,
        now_ms: u64,
    ) -> Result<(), OnlineLobbyError> {
        if self.admission_quiesced {
            return Ok(());
        }
        let Some(admission) = self
            .binding(user)
            .and_then(|binding| (!binding.retiring).then_some(binding.admission).flatten())
        else {
            return Ok(());
        };
        let Some(index) = self
            .pending_incoming
            .iter()
            .position(|entry| entry.is_some_and(|entry| entry.user == user))
        else {
            return Ok(());
        };
        let pending = self.pending_incoming[index]
            .take()
            .expect("pending incoming was just located");
        self.transport
            .as_mut()
            .ok_or(OnlineLobbyError::TransportNotInstalled)?
            .admit_incoming(pending.connection, admission, now_ms)?;
        let binding = self
            .binding_mut(user)
            .ok_or(OnlineLobbyError::MissingPeerBinding(user))?;
        binding.pending_connection = Some(pending.connection);
        Ok(())
    }

    fn sample_connection_quality<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        now_ms: u64,
    ) -> Result<(), OnlineLobbyError> {
        if self.transport.is_none() {
            return Ok(());
        }
        if now_ms.saturating_sub(self.last_quality_sample_ms)
            < self.config.quality_sample_interval_ms
        {
            return Ok(());
        }
        self.last_quality_sample_ms = now_ms;
        let calibrating_input_delay =
            self.role == Some(OnlineLobbyRole::ListenAuthority) && self.match_config.is_none();
        let local_user = self.local_user;
        let mut connections = [None; MAX_STEAM_LOBBY_MEMBERS];
        let mut connection_count = 0_usize;
        for binding in self.bindings.iter().flatten() {
            if !binding.retiring
                && let Some(connection) = binding.connection
            {
                connections[connection_count] = Some((binding.user, connection));
                connection_count += 1;
            }
        }
        for (user, connection) in connections[..connection_count].iter().flatten().copied() {
            let quality = self
                .transport
                .as_ref()
                .ok_or(OnlineLobbyError::TransportNotInstalled)?
                .connection_quality(connection)?;
            let sample = quality_sample(quality);
            let (snapshot, previous_quality) = {
                let binding = self
                    .binding_mut(user)
                    .ok_or(OnlineLobbyError::MissingPeerBinding(user))?;
                if calibrating_input_delay && binding.user != local_user {
                    binding.precommit_rtt.observe(quality.ping_ms);
                }
                let previous_quality = binding.last_reported_quality;
                let snapshot = binding.quality.observe(sample)?;
                if snapshot.quality != previous_quality {
                    binding.last_reported_quality = snapshot.quality;
                }
                (snapshot, previous_quality)
            };
            if snapshot.quality != previous_quality {
                self.push_event(OnlineLobbyEvent::QualityChanged {
                    user,
                    quality: snapshot,
                })?;
            }
            if previous_quality != NetworkQuality::Reject
                && snapshot.quality == NetworkQuality::Reject
            {
                self.enforce_quality_rejection(platform, user, connection, now_ms)?;
                if self.flow.phase() == OnlineLobbyPhase::Failed {
                    break;
                }
            }
        }
        Ok(())
    }

    fn enforce_quality_rejection<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        user: SteamUserId,
        connection: SteamConnectionId,
        now_ms: u64,
    ) -> Result<(), OnlineLobbyError> {
        let peer_id = self
            .binding(user)
            .map(|binding| binding.peer_id)
            .ok_or(OnlineLobbyError::MissingPeerBinding(user))?;
        let authority_rejecting_remote =
            self.role == Some(OnlineLobbyRole::ListenAuthority) && user != self.local_user;
        let client_rejecting_owner =
            self.role == Some(OnlineLobbyRole::Client) && self.owner == Some(user);
        if !authority_rejecting_remote && !client_rejecting_owner {
            return Err(OnlineLobbyError::InvalidState);
        }
        self.mark_quality_rejected(user)?;
        self.transport
            .as_mut()
            .ok_or(OnlineLobbyError::TransportNotInstalled)?
            .close_connection_for_quality_policy(connection)?;
        for slot in &mut self.pending_incoming {
            if slot.is_some_and(|pending| pending.user == user) {
                *slot = None;
            }
        }
        self.endpoints
            .retain(|endpoint| endpoint.remote_user != user);
        self.cancel_issued_ticket_for_user(platform, user);
        let _ = platform.end_peer_authentication(user);
        let binding = self
            .binding_mut(user)
            .ok_or(OnlineLobbyError::MissingPeerBinding(user))?;
        binding.authenticated = false;
        binding.admission = None;
        binding.pending_connection = None;
        binding.connection = None;
        binding.retiring_connection = None;
        self.rebuild_roster(platform)?;

        if authority_rejecting_remote {
            self.push_event(OnlineLobbyEvent::PeerDisconnected {
                connection,
                user,
                peer_id,
                reconnect_allowed: false,
            })
        } else {
            self.fail(
                platform,
                online_failure(
                    OnlineFailureCode::NetworkQualityRejected,
                    OnlineFailureSeverity::Recoverable,
                    OnlineRecoveryAction::ReturnToLobby,
                    0,
                ),
                now_ms,
            );
            Ok(())
        }
    }

    fn reserve_peer_binding(
        &mut self,
        user: SteamUserId,
        peer_id: PeerId,
    ) -> Result<(), OnlineLobbyError> {
        if let Some(binding) = self.binding(user) {
            if binding.peer_id == peer_id
                && !binding.authenticated
                && !binding.retiring
                && binding.authority_terminal_cleanup.is_none()
            {
                return Ok(());
            }
            return Err(OnlineLobbyError::DuplicatePeerBinding);
        }
        if self
            .bindings
            .iter()
            .flatten()
            .any(|binding| binding.peer_id == peer_id)
        {
            return Err(OnlineLobbyError::DuplicatePeerBinding);
        }
        let slot = self
            .bindings
            .iter_mut()
            .find(|slot| slot.is_none())
            .ok_or(OnlineLobbyError::DuplicatePeerBinding)?;
        *slot = Some(PeerBinding {
            user,
            peer_id,
            authenticated: false,
            admission: None,
            pending_connection: None,
            connection: None,
            retiring_connection: None,
            quality: NetworkQualityMonitor::new(self.config.quality)?,
            last_reported_quality: NetworkQuality::Healthy,
            precommit_rtt: PrecommitRttCalibrator::default(),
            retiring: false,
            authority_terminal_cleanup: None,
            deferred_authority_close: None,
        });
        Ok(())
    }

    fn install_local_binding(&mut self, peer_id: PeerId) -> Result<(), OnlineLobbyError> {
        self.reserve_peer_binding(self.local_user, peer_id)?;
        let binding = self
            .binding_mut(self.local_user)
            .expect("local binding was just reserved");
        binding.authenticated = true;
        Ok(())
    }

    fn binding(&self, user: SteamUserId) -> Option<&PeerBinding> {
        self.bindings
            .iter()
            .flatten()
            .find(|binding| binding.user == user)
    }

    fn user_is_retiring(&self, user: SteamUserId) -> bool {
        self.binding(user)
            .is_some_and(|binding| binding.retiring || binding.authority_terminal_cleanup.is_some())
            || self.retiring_transports.iter().any(|retiring| {
                retiring
                    .authenticated_users
                    .iter()
                    .flatten()
                    .any(|candidate| *candidate == user)
            })
    }

    fn binding_mut(&mut self, user: SteamUserId) -> Option<&mut PeerBinding> {
        self.bindings
            .iter_mut()
            .flatten()
            .find(|binding| binding.user == user)
    }

    fn is_quality_rejected(&self, user: SteamUserId) -> bool {
        self.quality_rejected_users.contains(&Some(user))
    }

    fn mark_quality_rejected(&mut self, user: SteamUserId) -> Result<(), OnlineLobbyError> {
        if self.is_quality_rejected(user) {
            return Ok(());
        }
        let slot = self
            .quality_rejected_users
            .iter_mut()
            .find(|slot| slot.is_none())
            .ok_or(OnlineLobbyError::InvalidState)?;
        *slot = Some(user);
        Ok(())
    }

    fn remove_unauthenticated_binding(&mut self, user: SteamUserId) {
        if let Some(slot) = self.bindings.iter_mut().find(|slot| {
            slot.as_ref()
                .is_some_and(|binding| binding.user == user && !binding.authenticated)
        }) {
            *slot = None;
        }
    }

    fn cancel_issued_ticket_for_user<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        user: SteamUserId,
    ) {
        if let Some(index) = self
            .issued_tickets
            .iter()
            .position(|record| record.lease.remote_user == user)
        {
            let record = self.issued_tickets.swap_remove(index);
            let _ = platform.cancel_auth_ticket(record.lease.handle);
        }
    }

    fn validate_initial_local_declaration(
        &self,
        declaration: OnlineRosterMember,
    ) -> Result<(), OnlineLobbyError> {
        if self.local_declaration.is_some() || self.bindings.iter().any(Option::is_some) {
            return Err(OnlineLobbyError::InvalidState);
        }
        self.validate_local_declaration_identity(declaration)
    }

    fn validate_local_declaration_identity(
        &self,
        declaration: OnlineRosterMember,
    ) -> Result<(), OnlineLobbyError> {
        if declaration.authenticated_user.get() != self.local_user.get() {
            return Err(OnlineLobbyError::LocalDeclarationMismatch);
        }
        declaration.peer_id.validate()?;
        Ok(())
    }

    fn require_local_platform<B: SteamBackend>(
        &self,
        platform: &SteamPlatform<B>,
    ) -> Result<(), OnlineLobbyError> {
        if platform.local_user() == self.local_user {
            Ok(())
        } else {
            Err(OnlineLobbyError::PeerIdentityMismatch)
        }
    }

    fn require_phase(&self, phase: OnlineLobbyPhase) -> Result<(), OnlineLobbyError> {
        if self.flow.phase() == phase {
            Ok(())
        } else {
            Err(OnlineLobbyError::InvalidState)
        }
    }

    fn require_lobby(&self) -> Result<SteamLobbyId, OnlineLobbyError> {
        self.lobby.ok_or(OnlineLobbyError::InvalidState)
    }

    fn advance_time(&mut self, now_ms: u64) -> Result<(), OnlineLobbyError> {
        if now_ms < self.last_now_ms {
            return Err(OnlineLobbyError::TimeRegression);
        }
        self.last_now_ms = now_ms;
        Ok(())
    }

    fn transition(&mut self, next: OnlineLobbyPhase, now_ms: u64) -> Result<(), OnlineLobbyError> {
        let from = self.flow.phase();
        self.flow.transition(next, now_ms, self.config.timeouts)?;
        if from != next {
            self.push_event(OnlineLobbyEvent::StateChanged { from, to: next })?;
        }
        Ok(())
    }

    fn force_phase(&mut self, next: OnlineLobbyPhase, now_ms: u64) -> Result<(), OnlineLobbyError> {
        let from = self.flow.phase();
        self.flow = OnlineFlowMachine::new(now_ms);
        if next != OnlineLobbyPhase::OfflineMenu {
            self.flow.phase = next;
            self.flow.entered_at_ms = now_ms;
            self.flow.deadline_at_ms = self
                .config
                .timeouts
                .for_phase(next)
                .and_then(|duration| now_ms.checked_add(duration));
        }
        if from != next {
            self.push_event(OnlineLobbyEvent::StateChanged { from, to: next })?;
        }
        Ok(())
    }

    fn push_event(&mut self, event: OnlineLobbyEvent) -> Result<(), OnlineLobbyError> {
        if self.events.len() >= self.config.event_capacity {
            return Err(OnlineLobbyError::EventQueueOverflow);
        }
        self.events.push_back(event);
        Ok(())
    }

    fn fail<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        failure: OnlineFailure,
        now_ms: u64,
    ) {
        if let Some(message) = self.authority_disconnect {
            self.failure = Some(OnlineFailure::from_disconnect(message));
            match message.retry {
                RetryDisposition::ReconnectAllowed => {
                    // An expected physical close follows the typed terminal.
                    // It may finish native cleanup, but cannot convert the
                    // already-authoritative reconnect policy into Failed.
                    if self.flow.phase() != OnlineLobbyPhase::Reconnecting {
                        let _ = self.force_phase(OnlineLobbyPhase::Reconnecting, now_ms);
                    }
                    return;
                }
                RetryDisposition::MatchEndedNoContest => {
                    self.outcome = Some(OnlineMatchOutcome::NoContestHostLost);
                    if self.flow.phase() != OnlineLobbyPhase::Results {
                        let _ = self.force_phase(OnlineLobbyPhase::Results, now_ms);
                    }
                    return;
                }
                RetryDisposition::ReturnToLobby | RetryDisposition::Fatal => {}
            }
        }
        let failure = self
            .authority_disconnect
            .map(OnlineFailure::from_disconnect)
            .unwrap_or(failure);
        self.failure = Some(failure);
        let _ = self.teardown_match_transport(platform, now_ms);
        let from = self.flow.phase();
        if from != OnlineLobbyPhase::Failed {
            let _ = self.flow.transition(
                OnlineLobbyPhase::Failed,
                now_ms.max(self.flow.entered_at_ms()),
                self.config.timeouts,
            );
            let _ = self.push_event(OnlineLobbyEvent::StateChanged {
                from,
                to: OnlineLobbyPhase::Failed,
            });
        }
        let _ = self.push_event(OnlineLobbyEvent::DropGameplayEndpoints);
        let _ = self.push_event(OnlineLobbyEvent::Failure(failure));
    }

    fn failure_for_error(&self, error: &OnlineLobbyError) -> OnlineFailure {
        match error {
            OnlineLobbyError::Steam(error) => OnlineFailure::from_steam(*error),
            OnlineLobbyError::Transport(error) => failure_from_transport_error(*error),
            OnlineLobbyError::ManifestMismatch
            | OnlineLobbyError::Roster(_)
            | OnlineLobbyError::Protocol(_) => online_failure(
                OnlineFailureCode::IncompatibleVersion,
                OnlineFailureSeverity::Fatal,
                OnlineRecoveryAction::ReturnToLobby,
                0,
            ),
            OnlineLobbyError::QualityPolicyRejected => online_failure(
                OnlineFailureCode::NetworkQualityRejected,
                OnlineFailureSeverity::Recoverable,
                OnlineRecoveryAction::ReturnToLobby,
                0,
            ),
            OnlineLobbyError::EndpointQueueOverflow
            | OnlineLobbyError::EventQueueOverflow
            | OnlineLobbyError::DuplicatePeerBinding
            | OnlineLobbyError::RetiringTransportCapacity => online_failure(
                OnlineFailureCode::InternalCapacity,
                OnlineFailureSeverity::Fatal,
                OnlineRecoveryAction::ReturnToMenu,
                0,
            ),
            _ => online_failure(
                OnlineFailureCode::InternalFailure,
                OnlineFailureSeverity::Fatal,
                OnlineRecoveryAction::ReturnToMenu,
                0,
            ),
        }
    }

    fn end_no_contest_host_lost<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        now_ms: u64,
    ) -> Result<(), OnlineLobbyError> {
        if self.confirmed_result_is_final() {
            return Ok(());
        }
        if self.outcome == Some(OnlineMatchOutcome::NoContestHostLost) {
            return Ok(());
        }
        self.outcome = Some(OnlineMatchOutcome::NoContestHostLost);
        self.failure = Some(self.authority_disconnect.map_or_else(
            || {
                online_failure(
                    OnlineFailureCode::AuthorityLost,
                    OnlineFailureSeverity::MatchEnded,
                    OnlineRecoveryAction::MatchEndedNoContest,
                    0,
                )
            },
            OnlineFailure::from_disconnect,
        ));
        self.teardown_match_transport(platform, now_ms)?;
        self.reconnect_resume = None;
        if self.flow.phase() != OnlineLobbyPhase::Results {
            if self
                .flow
                .phase()
                .can_transition_to(OnlineLobbyPhase::Results)
            {
                self.transition(OnlineLobbyPhase::Results, now_ms)?;
            } else {
                self.force_phase(OnlineLobbyPhase::Results, now_ms)?;
            }
        }
        self.push_event(OnlineLobbyEvent::DropGameplayEndpoints)?;
        self.push_event(OnlineLobbyEvent::MatchEnded(
            OnlineMatchOutcome::NoContestHostLost,
        ))
    }

    fn host_loss_ends_match(&self) -> bool {
        self.match_config.is_some()
            && matches!(
                self.flow.phase(),
                OnlineLobbyPhase::Countdown
                    | OnlineLobbyPhase::Fighting
                    | OnlineLobbyPhase::Reconnecting
                    | OnlineLobbyPhase::ConfirmingResult
                    | OnlineLobbyPhase::Results
            )
    }

    fn confirmed_result_is_final(&self) -> bool {
        self.flow.phase() == OnlineLobbyPhase::Results
            && self.outcome == Some(OnlineMatchOutcome::Confirmed)
    }

    fn teardown_confirmed_result_transport<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        now_ms: u64,
    ) -> Result<(), OnlineLobbyError> {
        self.teardown_match_transport(platform, now_ms)?;
        self.reconnect_resume = None;
        self.push_event(OnlineLobbyEvent::DropGameplayEndpoints)
    }

    fn teardown_match_transport<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        now_ms: u64,
    ) -> Result<(), OnlineLobbyError> {
        if self.transport.is_some()
            && self.retiring_transports.len() >= MAX_RETIRING_STEAM_TRANSPORTS
        {
            return Err(OnlineLobbyError::RetiringTransportCapacity);
        }

        self.transport_request = None;
        self.relay_status = SteamRelayStatus::default();
        self.endpoints.clear();
        self.pending_incoming = [None; MAX_STEAM_LOBBY_MEMBERS];

        let mut authenticated_users = [None; MAX_STEAM_LOBBY_MEMBERS];
        let mut authenticated_count = 0_usize;
        for slot in &mut self.bindings {
            let Some(binding) = slot else {
                continue;
            };
            if binding.user != self.local_user {
                if binding.authenticated
                    || binding.admission.is_some()
                    || binding.pending_connection.is_some()
                    || binding.connection.is_some()
                    || binding.retiring_connection.is_some()
                {
                    authenticated_users[authenticated_count] = Some(binding.user);
                    authenticated_count += 1;
                    binding.retiring = true;
                }
            } else {
                binding.connection = None;
                binding.pending_connection = None;
                binding.retiring_connection = None;
                binding.admission = None;
            }
        }

        let issued_tickets = std::mem::take(&mut self.issued_tickets);
        let Some(mut transport) = self.transport.take() else {
            self.finish_retiring_resources(platform, authenticated_users, issued_tickets);
            return Ok(());
        };

        self.retirement_metrics.started = self.retirement_metrics.started.saturating_add(1);
        let status = transport.begin_retirement(now_ms);
        if status == SteamTransportRetirementStatus::Draining {
            self.retiring_transports.push_back(RetiringSteamTransport {
                transport,
                authenticated_users,
                issued_tickets,
            });
            self.retirement_metrics.high_water = self
                .retirement_metrics
                .high_water
                .max(self.retiring_transports.len().min(usize::from(u8::MAX)) as u8);
        } else {
            self.record_retirement_outcome(status);
            self.finish_retiring_resources(platform, authenticated_users, issued_tickets);
        }
        Ok(())
    }

    fn pump_retiring_transports<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        now_ms: u64,
    ) -> Result<(), OnlineLobbyError> {
        let count = self.retiring_transports.len();
        let mut completed_any = false;
        for _ in 0..count {
            let mut retiring = self
                .retiring_transports
                .pop_front()
                .expect("bounded retirement count came from queue length");
            // Public events from this old match generation are never routed
            // into the active coordinator. `begin_retirement` cleared the
            // queue, and retirement pumping performs no backend receive.
            while retiring.transport.poll_event().is_some() {}
            let status = retiring.transport.pump_retirement(now_ms);
            if status == SteamTransportRetirementStatus::Draining {
                self.retiring_transports.push_back(retiring);
                continue;
            }
            self.record_retirement_outcome(status);
            self.finish_retiring_resources(
                platform,
                retiring.authenticated_users,
                retiring.issued_tickets,
            );
            completed_any = true;
        }
        if completed_any
            && !self.pending_platform_leave
            && matches!(platform.state(), SteamPlatformState::InLobby(_))
        {
            self.rebuild_roster(platform)?;
        }
        Ok(())
    }

    fn try_complete_pending_platform_leave<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
    ) -> Result<(), OnlineLobbyError> {
        if !self.pending_platform_leave || !self.retiring_transports.is_empty() {
            return Ok(());
        }
        if matches!(platform.state(), SteamPlatformState::InLobby(_)) {
            platform.leave_lobby()?;
        }
        self.pending_platform_leave = false;
        Ok(())
    }

    fn record_retirement_outcome(&mut self, status: SteamTransportRetirementStatus) {
        match status {
            SteamTransportRetirementStatus::Draining => {}
            SteamTransportRetirementStatus::Complete => {
                self.retirement_metrics.completed =
                    self.retirement_metrics.completed.saturating_add(1);
            }
            SteamTransportRetirementStatus::TimedOut => {
                self.retirement_metrics.timed_out =
                    self.retirement_metrics.timed_out.saturating_add(1);
            }
            SteamTransportRetirementStatus::Faulted(_) => {
                self.retirement_metrics.faulted = self.retirement_metrics.faulted.saturating_add(1);
            }
        }
    }

    fn finish_retiring_resources<B: SteamBackend>(
        &mut self,
        platform: &mut SteamPlatform<B>,
        authenticated_users: [Option<SteamUserId>; MAX_STEAM_LOBBY_MEMBERS],
        issued_tickets: Vec<PendingIssuedTicket>,
    ) {
        for record in issued_tickets {
            let _ = platform.cancel_auth_ticket(record.lease.handle);
        }
        for user in authenticated_users.iter().flatten().copied() {
            let _ = platform.end_peer_authentication(user);
            if let Some(slot) = self.bindings.iter_mut().find(|slot| {
                slot.as_ref()
                    .is_some_and(|binding| binding.user == user && binding.retiring)
            }) {
                *slot = None;
            }
        }
    }

    fn clear_lobby_identity_only(&mut self) {
        self.lobby = None;
        self.owner = None;
        self.role = None;
        self.lobby_contract = None;
        self.lobby_member_count = 0;
        self.platform_total_seats = 0;
        self.seat_capacity = 0;
        self.effective_joinable = false;
        self.roster_all_ready = false;
        self.pending_incoming = [None; MAX_STEAM_LOBBY_MEMBERS];
        self.quality_rejected_users = [None; MAX_STEAM_LOBBY_MEMBERS];
        self.roster = OnlineRoster::default();
    }

    fn clear_lobby_state(&mut self) {
        self.clear_lobby_identity_only();
        self.pending_join = None;
        self.local_declaration = None;
        self.bindings = std::array::from_fn(|_| None);
        self.transport = None;
        self.transport_request = None;
        self.endpoints.clear();
        self.issued_tickets.clear();
        self.match_config = None;
        self.committed_input_delay_calibration = None;
        self.countdown_start_tick = None;
        self.reconnect_resume = None;
        self.outcome = None;
        self.failure = None;
        self.authority_disconnect = None;
        self.committed_peer_leases = [None; MAX_STEAM_LOBBY_MEMBERS];
        self.committed_member_revisions = [None; MAX_STEAM_LOBBY_MEMBERS];
        self.pending_results_return = None;
        self.admission_quiesced = false;
    }
}

fn manifest_peer_groups(
    manifest: &MatchManifest,
) -> Result<([Option<ManifestPeerGroup>; MAX_STEAM_LOBBY_MEMBERS], usize), OnlineLobbyError> {
    let mut groups: [Option<ManifestPeerGroup>; MAX_STEAM_LOBBY_MEMBERS] =
        [None; MAX_STEAM_LOBBY_MEMBERS];
    let mut group_count = 0_usize;
    for slot in manifest.slots.iter().filter(|slot| slot.occupied) {
        let assignment = manifest
            .ownership
            .assignment_for_fighter(slot.fighter)
            .ok_or(OnlineLobbyError::ManifestMismatch)?;
        let SeatOwner::Peer(peer_id) = assignment.owner else {
            return Err(OnlineLobbyError::ManifestMismatch);
        };
        let group_index = match groups[..group_count]
            .iter()
            .position(|group| group.is_some_and(|group| group.peer_id == peer_id))
        {
            Some(index) => index,
            None => {
                if group_count >= groups.len() {
                    return Err(OnlineLobbyError::ManifestMismatch);
                }
                groups[group_count] = Some(ManifestPeerGroup {
                    peer_id,
                    seat_count: 0,
                    seats: [OnlineSeatSelection::default(); MAX_FIGHTERS],
                });
                group_count += 1;
                group_count - 1
            }
        };
        let group = groups[group_index]
            .as_mut()
            .expect("manifest group was just installed");
        let seat_index = usize::from(group.seat_count);
        if seat_index >= group.seats.len() {
            return Err(OnlineLobbyError::ManifestMismatch);
        }
        group.seats[seat_index] = OnlineSeatSelection {
            team: slot.team,
            character: slot.character,
            style: slot.style,
            equipment: slot.equipment,
        };
        group.seat_count += 1;
    }
    if group_count == 0 {
        return Err(OnlineLobbyError::ManifestMismatch);
    }
    Ok((groups, group_count))
}

fn manifest_declaration_matches_group(
    user: SteamUserId,
    loadout: MemberLoadoutDeclaration,
    group: ManifestPeerGroup,
) -> Result<bool, OnlineLobbyError> {
    let declaration =
        decode_member_declaration(group.peer_id, user.authenticated(), true, loadout.as_str())
            .map_err(|_| OnlineLobbyError::ManifestMismatch)?;
    Ok(declaration.seats() == &group.seats[..usize::from(group.seat_count)])
}

fn quality_sample(quality: SteamConnectionQuality) -> NetworkQualitySample {
    let delivery = match (
        quality.local_delivery_permyriad,
        quality.remote_delivery_permyriad,
    ) {
        (Some(local), Some(remote)) => Some(local.min(remote)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    };
    NetworkQualitySample {
        rtt_ms: quality.ping_ms.unwrap_or_default().min(u32::from(u16::MAX)) as u16,
        loss_bps: delivery
            .map(|value| 10_000_u16.saturating_sub(value.min(10_000)))
            .unwrap_or_default(),
    }
}

fn timeout_failure(phase: OnlineLobbyPhase) -> OnlineFailure {
    let (code, recovery) = match phase {
        OnlineLobbyPhase::CreatingLobby | OnlineLobbyPhase::JoiningLobby => (
            OnlineFailureCode::LobbyUnavailable,
            OnlineRecoveryAction::Retry,
        ),
        OnlineLobbyPhase::Authenticating => (
            OnlineFailureCode::AuthenticationTimedOut,
            OnlineRecoveryAction::Retry,
        ),
        OnlineLobbyPhase::Connecting | OnlineLobbyPhase::Reconnecting => (
            OnlineFailureCode::ConnectionTimedOut,
            if phase == OnlineLobbyPhase::Reconnecting {
                OnlineRecoveryAction::ReturnToLobby
            } else {
                OnlineRecoveryAction::Retry
            },
        ),
        OnlineLobbyPhase::Loading => (
            OnlineFailureCode::LoadingTimedOut,
            OnlineRecoveryAction::ReturnToLobby,
        ),
        OnlineLobbyPhase::ManifestAgreement
        | OnlineLobbyPhase::InitialSync
        | OnlineLobbyPhase::Ready
        | OnlineLobbyPhase::Countdown
        | OnlineLobbyPhase::ConfirmingResult => (
            OnlineFailureCode::SynchronizationFailed,
            OnlineRecoveryAction::ReturnToLobby,
        ),
        _ => (
            OnlineFailureCode::InternalFailure,
            OnlineRecoveryAction::ReturnToMenu,
        ),
    };
    online_failure(
        code,
        OnlineFailureSeverity::Recoverable,
        recovery,
        phase as u16,
    )
}

fn failure_from_transport_error(error: SteamTransportError) -> OnlineFailure {
    let (code, severity, recovery) = match error {
        SteamTransportError::CapacityExceeded
        | SteamTransportError::EventQueueOverflow
        | SteamTransportError::CallbackQueueOverflow => (
            OnlineFailureCode::InternalCapacity,
            OnlineFailureSeverity::Fatal,
            OnlineRecoveryAction::ReturnToMenu,
        ),
        SteamTransportError::HostedDedicatedSdrUnavailable => (
            OnlineFailureCode::DedicatedUnavailable,
            OnlineFailureSeverity::Fatal,
            OnlineRecoveryAction::ReturnToMenu,
        ),
        SteamTransportError::AdmissionLobbyMismatch
        | SteamTransportError::AdmissionUserMismatch
        | SteamTransportError::AdmissionAuthorityMismatch
        | SteamTransportError::AdmissionIdentityMismatch
        | SteamTransportError::AuthorityIdentityMismatch => (
            OnlineFailureCode::AuthenticationFailed,
            OnlineFailureSeverity::Fatal,
            OnlineRecoveryAction::ReturnToMenu,
        ),
        SteamTransportError::BackendUnavailable
        | SteamTransportError::EndpointNotReady
        | SteamTransportError::UnknownConnection => (
            OnlineFailureCode::ConnectionTimedOut,
            OnlineFailureSeverity::Recoverable,
            OnlineRecoveryAction::Reconnect,
        ),
        _ => (
            OnlineFailureCode::InternalFailure,
            OnlineFailureSeverity::Fatal,
            OnlineRecoveryAction::ReturnToMenu,
        ),
    };
    online_failure(code, severity, recovery, error as u16)
}

fn transport_error_for_close(reason: SteamTransportCloseReason) -> SteamTransportError {
    match reason {
        SteamTransportCloseReason::AdmissionRejected
        | SteamTransportCloseReason::AdmissionTimedOut => {
            SteamTransportError::AdmissionIdentityMismatch
        }
        SteamTransportCloseReason::ConnectTimedOut | SteamTransportCloseReason::RemoteClosed => {
            SteamTransportError::BackendUnavailable
        }
        SteamTransportCloseReason::Requested
        | SteamTransportCloseReason::QualityPolicyRejected
        | SteamTransportCloseReason::LocalProblem
        | SteamTransportCloseReason::EndpointDropped
        | SteamTransportCloseReason::InboundQueueOverflow
        | SteamTransportCloseReason::OversizedDatagram
        | SteamTransportCloseReason::BackendFailure
        | SteamTransportCloseReason::TransportFault => SteamTransportError::BackendOperationFailed,
    }
}

const fn failure_for_member_declaration(reason: MemberDeclarationRejection) -> OnlineFailure {
    match reason {
        MemberDeclarationRejection::LobbyCapacityExceeded => online_failure(
            OnlineFailureCode::LobbyFull,
            OnlineFailureSeverity::Recoverable,
            OnlineRecoveryAction::ReturnToLobby,
            1,
        ),
        MemberDeclarationRejection::Malformed => online_failure(
            OnlineFailureCode::AuthenticationFailed,
            OnlineFailureSeverity::Recoverable,
            OnlineRecoveryAction::ReturnToLobby,
            2,
        ),
        MemberDeclarationRejection::RevisionRegression => online_failure(
            OnlineFailureCode::AuthenticationFailed,
            OnlineFailureSeverity::Recoverable,
            OnlineRecoveryAction::ReturnToLobby,
            3,
        ),
        MemberDeclarationRejection::RevisionConflict => online_failure(
            OnlineFailureCode::AuthenticationFailed,
            OnlineFailureSeverity::Recoverable,
            OnlineRecoveryAction::ReturnToLobby,
            4,
        ),
    }
}

const fn online_failure(
    code: OnlineFailureCode,
    severity: OnlineFailureSeverity,
    recovery: OnlineRecoveryAction,
    detail_code: u16,
) -> OnlineFailure {
    OnlineFailure {
        code,
        severity,
        recovery,
        detail_code,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network_io::NonBlockingDatagramEndpoint;
    use crate::network_protocol::{DefinitionId, DisconnectCode, MatchId, TeamId};
    use crate::online_roster::OnlineSeatSelection;
    use crate::steam_platform::{
        AuthValidationFailure, FakeAuthOutcome, FakeLobbyMemberSeed, FakeSteamBackend,
        FakeSteamControl, JoinOrigin, LicenseStatus, LobbyCreateRequest, LobbyMetadata,
        LobbyVisibility, RegionCode, SteamAppId, SteamClientConfig,
    };
    use crate::steam_transport::{FakeSteamTransportNetwork, SteamTransportConnectionState};

    const HOST_RAW: u64 = 7_001;
    const CLIENT_RAW: u64 = 7_002;

    fn user(value: u64) -> SteamUserId {
        SteamUserId::new(value).unwrap()
    }

    fn peer(value: u64) -> PeerId {
        PeerId::new(value).unwrap()
    }

    fn definition(value: u16) -> DefinitionId {
        DefinitionId::new(value).unwrap()
    }

    fn member(
        steam_user: SteamUserId,
        peer_id: PeerId,
        revision: u16,
        ready: bool,
        character: u16,
        team: u8,
    ) -> OnlineRosterMember {
        OnlineRosterMember::new(
            peer_id,
            steam_user.authenticated(),
            revision,
            ready,
            &[OnlineSeatSelection {
                team: TeamId::new(team).unwrap(),
                character: definition(character),
                style: definition(0),
                equipment: definition(0),
            }],
        )
        .unwrap()
    }

    fn metadata(seats: u8) -> LobbyMetadata {
        LobbyMetadata::current(
            AuthorityKind::Listen,
            LobbyVisibility::Private,
            RegionCode::new("test-region").unwrap(),
            definition(1),
            definition(0),
            seats,
        )
        .unwrap()
    }

    fn fake_platform(
        local_user: SteamUserId,
    ) -> (SteamPlatform<FakeSteamBackend>, FakeSteamControl) {
        let app_id = SteamAppId::new(1_234).unwrap();
        let (backend, control) = FakeSteamBackend::new(app_id, local_user);
        let platform = SteamPlatform::new(SteamClientConfig::production(app_id), backend, 0)
            .expect("valid fake Steam client");
        (platform, control)
    }

    fn coordinator_config() -> OnlineLobbyConfig {
        OnlineLobbyConfig {
            quality: NetworkQualityPolicy {
                transition_samples: 1,
                recovery_samples: 1,
                ..NetworkQualityPolicy::default()
            },
            quality_sample_interval_ms: 1,
            ..OnlineLobbyConfig::default()
        }
    }

    fn feed_precommit_rtt(
        coordinator: &mut OnlineLobbyCoordinator,
        remote_user: SteamUserId,
        ping_ms: u16,
    ) {
        let binding = coordinator
            .binding_mut(remote_user)
            .expect("calibration fixture requires a reserved remote binding");
        for _ in 0..crate::network_quality::MIN_PRECOMMIT_RTT_SAMPLES {
            assert!(binding.precommit_rtt.observe(Some(u32::from(ping_ms))));
        }
    }

    fn seed_lobby(
        control: &FakeSteamControl,
        lobby: SteamLobbyId,
        owner: SteamUserId,
        host: OnlineRosterMember,
        client: OnlineRosterMember,
    ) {
        control
            .seed_lobby(
                lobby,
                &metadata(2),
                true,
                owner,
                &[
                    FakeLobbyMemberSeed {
                        user: user(HOST_RAW),
                        readiness: Some(MemberReadiness::Declared {
                            ready: host.ready,
                            local_seats: host.seat_count() as u8,
                        }),
                    },
                    FakeLobbyMemberSeed {
                        user: user(CLIENT_RAW),
                        readiness: Some(MemberReadiness::Declared {
                            ready: client.ready,
                            local_seats: client.seat_count() as u8,
                        }),
                    },
                ],
            )
            .unwrap();
        control
            .set_member_loadout(
                lobby,
                user(HOST_RAW),
                MemberLoadoutDeclaration::new(&encode_member_declaration(&host)).unwrap(),
            )
            .unwrap();
        control
            .set_member_loadout(
                lobby,
                user(CLIENT_RAW),
                MemberLoadoutDeclaration::new(&encode_member_declaration(&client)).unwrap(),
            )
            .unwrap();
    }

    fn seed_lobby_members(
        control: &FakeSteamControl,
        lobby: SteamLobbyId,
        owner: SteamUserId,
        members: &[OnlineRosterMember],
        seat_capacity: u8,
    ) {
        let seeds: Vec<_> = members
            .iter()
            .map(|member| FakeLobbyMemberSeed {
                user: SteamUserId::new(member.authenticated_user.get()).unwrap(),
                readiness: Some(MemberReadiness::Declared {
                    ready: member.ready,
                    local_seats: member.seat_count() as u8,
                }),
            })
            .collect();
        control
            .seed_lobby(lobby, &metadata(seat_capacity), true, owner, &seeds)
            .unwrap();
        for member in members {
            control
                .set_member_loadout(
                    lobby,
                    SteamUserId::new(member.authenticated_user.get()).unwrap(),
                    MemberLoadoutDeclaration::new(&encode_member_declaration(member)).unwrap(),
                )
                .unwrap();
        }
    }

    fn manifest_for_members(
        members: &[OnlineRosterMember],
        authority_peer: PeerId,
        match_id: [u8; 16],
    ) -> HeadlessMatchConfig {
        let mut roster = OnlineRoster::default();
        for member in members {
            roster.upsert(*member).unwrap();
        }
        roster
            .build_headless_config(
                OnlineManifestOptions::casual_listen(
                    MatchId::new(match_id).unwrap(),
                    authority_peer,
                    definition(0),
                    definition(1),
                    0xAFC0_A101,
                    SimTick(120),
                ),
                SimTick::ZERO,
            )
            .unwrap()
    }

    fn auth_lobby_fixture(
        local_user: SteamUserId,
    ) -> (
        SteamPlatform<FakeSteamBackend>,
        FakeSteamControl,
        OnlineLobbyCoordinator,
        SteamLobbyId,
        OnlineRosterMember,
        OnlineRosterMember,
    ) {
        let host_user = user(HOST_RAW);
        let client_user = user(CLIENT_RAW);
        let host_member = member(host_user, peer(9_901), 1, true, 0, 0);
        let client_member = member(client_user, peer(9_902), 1, true, 1, 1);
        let local_member = if local_user == host_user {
            host_member
        } else {
            assert_eq!(local_user, client_user);
            client_member
        };
        let lobby = SteamLobbyId::new(88_901).unwrap();
        let (mut platform, control) = fake_platform(local_user);
        seed_lobby(&control, lobby, host_user, host_member, client_member);
        let mut coordinator =
            OnlineLobbyCoordinator::new(local_user, coordinator_config(), 0).unwrap();
        coordinator
            .begin_join(&mut platform, join_intent(lobby), local_member, 0)
            .unwrap();
        coordinator.pump(&mut platform, 1).unwrap();
        while coordinator.poll_event().is_some() {}
        (
            platform,
            control,
            coordinator,
            lobby,
            host_member,
            client_member,
        )
    }

    fn commit_auth_fixture(
        platform: &SteamPlatform<FakeSteamBackend>,
        coordinator: &mut OnlineLobbyCoordinator,
        host_member: OnlineRosterMember,
        client_member: OnlineRosterMember,
    ) -> MatchId {
        let remote = if coordinator.local_user == user(HOST_RAW) {
            client_member
        } else {
            host_member
        };
        coordinator
            .reserve_peer_binding(
                SteamUserId::new(remote.authenticated_user.get()).unwrap(),
                remote.peer_id,
            )
            .unwrap();
        coordinator
            .binding_mut(SteamUserId::new(remote.authenticated_user.get()).unwrap())
            .unwrap()
            .authenticated = true;
        let config = manifest_for_members(
            &[host_member, client_member],
            host_member.peer_id,
            *b"auth-lease-v2-01",
        );
        let match_id = config.manifest.match_id;
        coordinator.match_config = Some(config);
        coordinator.capture_committed_peer_leases(platform).unwrap();
        coordinator.flow.phase = OnlineLobbyPhase::Fighting;
        match_id
    }

    fn three_member_client_fixture() -> (
        SteamPlatform<FakeSteamBackend>,
        FakeSteamControl,
        OnlineLobbyCoordinator,
        SteamLobbyId,
        [OnlineRosterMember; 3],
    ) {
        let host_user = user(81_001);
        let third_user = user(81_002);
        let local_user = user(81_003);
        let members = [
            member(host_user, peer(10_101), 1, true, 0, 0),
            member(third_user, peer(10_102), 1, true, 1, 1),
            member(local_user, peer(10_103), 1, true, 2, 0),
        ];
        let lobby = SteamLobbyId::new(88_401).unwrap();
        let (mut platform, control) = fake_platform(local_user);
        seed_lobby_members(&control, lobby, host_user, &members, 4);
        let mut coordinator =
            OnlineLobbyCoordinator::new(local_user, coordinator_config(), 0).unwrap();
        coordinator
            .begin_join(&mut platform, join_intent(lobby), members[2], 0)
            .unwrap();
        coordinator.pump(&mut platform, 1).unwrap();
        coordinator
            .reserve_peer_binding(host_user, members[0].peer_id)
            .unwrap();
        coordinator.binding_mut(host_user).unwrap().authenticated = true;
        coordinator.rebuild_roster(&platform).unwrap();
        assert_eq!(
            coordinator.roster.len(),
            2,
            "a client authenticates only itself and the listen owner"
        );
        coordinator.flow.phase = OnlineLobbyPhase::ManifestAgreement;
        (platform, control, coordinator, lobby, members)
    }

    fn join_intent(lobby: SteamLobbyId) -> LobbyJoinIntent {
        LobbyJoinIntent {
            lobby,
            origin: JoinOrigin::LaunchCommand,
            expires_at_ms: 20_000,
        }
    }

    fn typed_disconnect_client_fixture() -> (
        SteamPlatform<FakeSteamBackend>,
        OnlineLobbyCoordinator,
        MatchId,
    ) {
        let host_user = user(HOST_RAW);
        let local_user = user(CLIENT_RAW);
        let host = member(host_user, peer(71), 1, true, 0, 0);
        let local = member(local_user, peer(72), 1, true, 1, 1);
        let config = manifest_for_members(&[host, local], host.peer_id, *b"typed-disconn-01");
        let match_id = config.manifest.match_id;
        let (platform, _) = fake_platform(local_user);
        let mut coordinator =
            OnlineLobbyCoordinator::new(local_user, coordinator_config(), 0).unwrap();
        coordinator.role = Some(OnlineLobbyRole::Client);
        coordinator.owner = Some(host_user);
        coordinator.match_config = Some(config);
        coordinator.flow.phase = OnlineLobbyPhase::Fighting;
        (platform, coordinator, match_id)
    }

    struct ConnectedHostFixture {
        host_platform: SteamPlatform<FakeSteamBackend>,
        host_control: FakeSteamControl,
        host: OnlineLobbyCoordinator,
        network: FakeSteamTransportNetwork,
        client_transport: SteamTransport,
        host_endpoint: AdmittedSteamEndpoint,
        client_endpoint: AdmittedSteamEndpoint,
        session: SteamP2pSession,
        host_user: SteamUserId,
        client_user: SteamUserId,
        pending_user: SteamUserId,
        client_peer: PeerId,
    }

    fn connected_host_fixture() -> ConnectedHostFixture {
        let host_user = user(94_001);
        let client_user = user(94_002);
        let pending_user = user(94_003);
        let host_peer = peer(14_001);
        let client_peer = peer(14_002);
        let members = [
            member(host_user, host_peer, 1, true, 0, 0),
            member(client_user, client_peer, 1, true, 1, 1),
            member(pending_user, peer(14_003), 1, true, 2, 0),
        ];
        let lobby = SteamLobbyId::new(94_101).unwrap();
        let (mut host_platform, host_control) = fake_platform(host_user);
        seed_lobby_members(&host_control, lobby, host_user, &members, 4);
        host_control
            .set_auth_outcome(client_user, accepted_with_license_owner(client_user))
            .unwrap();

        let config = coordinator_config();
        let mut host = OnlineLobbyCoordinator::new(host_user, config, 0).unwrap();
        host.begin_join(&mut host_platform, join_intent(lobby), members[0], 0)
            .unwrap();
        host.pump(&mut host_platform, 1).unwrap();
        let session = host.take_transport_request().unwrap();
        let network = FakeSteamTransportNetwork::new(32).unwrap();
        host.install_transport(
            network
                .create_transport(host_user, session, config.transport, 1)
                .unwrap(),
            1,
        )
        .unwrap();
        host.pump(&mut host_platform, 2).unwrap();
        host.begin_peer_authentication(
            &mut host_platform,
            client_user,
            client_peer,
            &[1, 4, 2],
            AdmissionPurpose::Initial,
            2,
        )
        .unwrap();
        host.pump(&mut host_platform, 3).unwrap();

        let mut client_transport = network
            .create_transport(
                client_user,
                SteamP2pSession {
                    role: SteamTransportRole::Client,
                    ..session
                },
                config.transport,
                3,
            )
            .unwrap();
        let connection = client_transport
            .connect_p2p(
                AuthenticatedSteamPeer {
                    lobby,
                    user: host_user,
                    license_owner_user: host_user,
                    authenticated_user: host_user.authenticated(),
                    local_seats: 1,
                    purpose: AdmissionPurpose::Initial,
                },
                4,
            )
            .unwrap();
        host.pump(&mut host_platform, 5).unwrap();
        host.pump(&mut host_platform, 6).unwrap();
        client_transport.pump(6).unwrap();
        let host_endpoint = host
            .take_endpoint()
            .expect("host admitted fixture endpoint");
        let client_endpoint = client_transport
            .take_endpoint(connection)
            .expect("client fixture endpoint ready");
        while host.poll_event().is_some() {}
        while client_transport.poll_event().is_some() {}

        ConnectedHostFixture {
            host_platform,
            host_control,
            host,
            network,
            client_transport,
            host_endpoint,
            client_endpoint,
            session,
            host_user,
            client_user,
            pending_user,
            client_peer,
        }
    }

    #[test]
    fn typed_authority_disconnect_dispositions_drive_exact_coordinator_states() {
        for (retry, expected_phase, expected_outcome) in [
            (
                RetryDisposition::ReconnectAllowed,
                OnlineLobbyPhase::Reconnecting,
                None,
            ),
            (
                RetryDisposition::ReturnToLobby,
                OnlineLobbyPhase::Failed,
                None,
            ),
            (
                RetryDisposition::MatchEndedNoContest,
                OnlineLobbyPhase::Results,
                Some(OnlineMatchOutcome::NoContestHostLost),
            ),
            (RetryDisposition::Fatal, OnlineLobbyPhase::Failed, None),
        ] {
            let (mut platform, mut coordinator, match_id) = typed_disconnect_client_fixture();
            let message = DisconnectMessage {
                match_id: Some(match_id),
                code: DisconnectCode::ServerShutdown,
                retry,
                detail_code: 0xAFC,
                last_confirmed_tick: Some(SimTick(55)),
            };
            coordinator
                .apply_authority_disconnect(&mut platform, message, 1)
                .unwrap();
            let status = coordinator.status();
            assert_eq!(status.phase, expected_phase, "{retry:?}");
            assert_eq!(status.outcome, expected_outcome, "{retry:?}");
            assert_eq!(
                status.failure,
                Some(OnlineFailure::from_disconnect(message)),
                "{retry:?}"
            );
            assert!(
                retry != RetryDisposition::ReconnectAllowed || coordinator.match_config().is_some()
            );
        }
    }

    #[test]
    fn typed_authority_disconnect_is_match_bound_first_wins_and_beats_generic_close() {
        let (mut platform, mut coordinator, match_id) = typed_disconnect_client_fixture();
        for rejected in [None, Some(MatchId::new(*b"wrong-disconn-01").unwrap())] {
            assert_eq!(
                coordinator.apply_authority_disconnect(
                    &mut platform,
                    DisconnectMessage {
                        match_id: rejected,
                        code: DisconnectCode::Kicked,
                        retry: RetryDisposition::Fatal,
                        detail_code: 1,
                        last_confirmed_tick: None,
                    },
                    1,
                ),
                Err(OnlineLobbyError::Protocol(
                    ProtocolValidationError::MatchMismatch
                ))
            );
        }

        let first = DisconnectMessage {
            match_id: Some(match_id),
            code: DisconnectCode::Kicked,
            retry: RetryDisposition::ReconnectAllowed,
            detail_code: 91,
            last_confirmed_tick: Some(SimTick(33)),
        };
        coordinator
            .apply_authority_disconnect(&mut platform, first, 2)
            .unwrap();
        coordinator.fail(
            &mut platform,
            online_failure(
                OnlineFailureCode::ConnectionTimedOut,
                OnlineFailureSeverity::Recoverable,
                OnlineRecoveryAction::Reconnect,
                999,
            ),
            3,
        );
        coordinator
            .apply_authority_disconnect(
                &mut platform,
                DisconnectMessage {
                    retry: RetryDisposition::Fatal,
                    detail_code: 92,
                    ..first
                },
                4,
            )
            .unwrap();
        assert_eq!(
            coordinator.status().failure,
            Some(OnlineFailure::from_disconnect(first))
        );
        assert_eq!(coordinator.status().phase, OnlineLobbyPhase::Reconnecting);
    }

    fn accepted_with_license_owner(license_owner: SteamUserId) -> FakeAuthOutcome {
        FakeAuthOutcome {
            license_owner_user: license_owner,
            validation: Ok(()),
            license: LicenseStatus::HasLicense,
        }
    }

    #[test]
    fn malformed_member_isolated_without_poisoning_host_or_healthy_peer() {
        let host_user = user(7_101);
        let bad_user = user(7_102);
        let healthy_user = user(7_103);
        let host_member = member(host_user, peer(31), 1, false, 0, 0);
        let bad_member = member(bad_user, peer(32), 1, false, 1, 1);
        let healthy_member = member(healthy_user, peer(33), 1, false, 2, 0);
        let lobby = SteamLobbyId::new(87_001).unwrap();
        let (mut platform, control) = fake_platform(host_user);
        seed_lobby_members(
            &control,
            lobby,
            host_user,
            &[host_member, bad_member, healthy_member],
            4,
        );
        let mut coordinator =
            OnlineLobbyCoordinator::new(host_user, coordinator_config(), 0).unwrap();
        coordinator
            .begin_join(&mut platform, join_intent(lobby), host_member, 0)
            .unwrap();
        coordinator.pump(&mut platform, 1).unwrap();
        for (remote_user, remote_peer) in [
            (bad_user, bad_member.peer_id),
            (healthy_user, healthy_member.peer_id),
        ] {
            coordinator
                .reserve_peer_binding(remote_user, remote_peer)
                .unwrap();
            coordinator.binding_mut(remote_user).unwrap().authenticated = true;
        }
        coordinator.rebuild_roster(&platform).unwrap();
        while coordinator.poll_event().is_some() {}

        control
            .set_member_data_raw(lobby, bad_user, "afc_ready", "malformed")
            .unwrap();
        control.emit_member_data_changed(lobby, bad_user).unwrap();
        coordinator.pump(&mut platform, 2).unwrap();

        assert_eq!(coordinator.status().phase, OnlineLobbyPhase::Lobby);
        assert!(coordinator.binding(bad_user).is_none());
        assert!(coordinator.binding(healthy_user).is_some());
        let events: Vec<_> = std::iter::from_fn(|| coordinator.poll_event()).collect();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    OnlineLobbyEvent::PeerAuthenticationRejected { user, .. }
                        if *user == bad_user
                ))
                .count(),
            1
        );
        assert!(events.iter().any(|event| matches!(
            event,
            OnlineLobbyEvent::RosterChanged { live_bindings, .. }
                if !live_bindings.iter().flatten().any(|binding| binding.user == bad_user)
                    && live_bindings
                        .iter()
                        .flatten()
                        .any(|binding| binding.user == healthy_user)
        )));

        control.emit_member_data_changed(lobby, bad_user).unwrap();
        coordinator.pump(&mut platform, 3).unwrap();
        assert!(
            !std::iter::from_fn(|| coordinator.poll_event()).any(|event| matches!(
                event,
                OnlineLobbyEvent::PeerAuthenticationRejected { user, .. } if user == bad_user
            ))
        );
    }

    #[test]
    fn malformed_local_declaration_is_terminal_for_listen_and_client_roles() {
        for client_role in [false, true] {
            let local_user = user(if client_role { 7_112 } else { 7_111 });
            let remote_user = user(if client_role { 7_113 } else { 7_114 });
            let owner = if client_role { remote_user } else { local_user };
            let local_member = member(local_user, peer(41), 1, false, 0, 0);
            let remote_member = member(remote_user, peer(42), 1, false, 1, 1);
            let lobby = SteamLobbyId::new(if client_role { 87_012 } else { 87_011 }).unwrap();
            let (mut platform, control) = fake_platform(local_user);
            seed_lobby_members(&control, lobby, owner, &[local_member, remote_member], 4);
            let mut coordinator =
                OnlineLobbyCoordinator::new(local_user, coordinator_config(), 0).unwrap();
            coordinator
                .begin_join(&mut platform, join_intent(lobby), local_member, 0)
                .unwrap();
            coordinator.pump(&mut platform, 1).unwrap();
            while coordinator.poll_event().is_some() {}

            control
                .set_member_data_raw(lobby, local_user, "afc_ready", "malformed")
                .unwrap();
            control.emit_member_data_changed(lobby, local_user).unwrap();
            coordinator.pump(&mut platform, 2).unwrap();

            assert_eq!(coordinator.status().phase, OnlineLobbyPhase::Failed);
            assert_eq!(
                coordinator.status().failure.map(|failure| failure.recovery),
                Some(OnlineRecoveryAction::ReturnToMenu)
            );
            assert!(
                std::iter::from_fn(|| coordinator.poll_event()).any(|event| matches!(
                    event,
                    OnlineLobbyEvent::PeerAuthenticationRejected { user, failure, .. }
                        if user == local_user
                            && failure.recovery == OnlineRecoveryAction::ReturnToMenu
                ))
            );
        }
    }

    #[test]
    fn malformed_owner_declaration_is_terminal_for_client() {
        let owner = user(7_121);
        let local_user = user(7_122);
        let owner_member = member(owner, peer(51), 1, false, 0, 0);
        let local_member = member(local_user, peer(52), 1, false, 1, 1);
        let lobby = SteamLobbyId::new(87_021).unwrap();
        let (mut platform, control) = fake_platform(local_user);
        seed_lobby_members(&control, lobby, owner, &[owner_member, local_member], 4);
        let mut coordinator =
            OnlineLobbyCoordinator::new(local_user, coordinator_config(), 0).unwrap();
        coordinator
            .begin_join(&mut platform, join_intent(lobby), local_member, 0)
            .unwrap();
        coordinator.pump(&mut platform, 1).unwrap();
        coordinator
            .reserve_peer_binding(owner, owner_member.peer_id)
            .unwrap();
        coordinator.binding_mut(owner).unwrap().authenticated = true;
        while coordinator.poll_event().is_some() {}

        control
            .set_member_data_raw(lobby, owner, "afc_ready", "malformed")
            .unwrap();
        control.emit_member_data_changed(lobby, owner).unwrap();
        coordinator.pump(&mut platform, 2).unwrap();

        assert_eq!(coordinator.status().phase, OnlineLobbyPhase::Failed);
        assert!(coordinator.binding(owner).is_none());
        assert_eq!(
            coordinator.status().failure.map(|failure| failure.code),
            Some(OnlineFailureCode::AuthenticationFailed)
        );
    }

    #[test]
    fn unrelated_malformed_member_on_client_only_rebuilds_pending_roster() {
        let owner = user(7_131);
        let local_user = user(7_132);
        let unrelated = user(7_133);
        let owner_member = member(owner, peer(61), 1, false, 0, 0);
        let local_member = member(local_user, peer(62), 1, false, 1, 1);
        let unrelated_member = member(unrelated, peer(63), 1, false, 2, 0);
        let lobby = SteamLobbyId::new(87_031).unwrap();
        let (mut platform, control) = fake_platform(local_user);
        seed_lobby_members(
            &control,
            lobby,
            owner,
            &[owner_member, local_member, unrelated_member],
            4,
        );
        let mut coordinator =
            OnlineLobbyCoordinator::new(local_user, coordinator_config(), 0).unwrap();
        coordinator
            .begin_join(&mut platform, join_intent(lobby), local_member, 0)
            .unwrap();
        coordinator.pump(&mut platform, 1).unwrap();
        for member in [owner_member, unrelated_member] {
            coordinator
                .reserve_peer_binding(
                    SteamUserId::new(member.authenticated_user.get()).unwrap(),
                    member.peer_id,
                )
                .unwrap();
            coordinator
                .binding_mut(SteamUserId::new(member.authenticated_user.get()).unwrap())
                .unwrap()
                .authenticated = true;
        }
        coordinator.rebuild_roster(&platform).unwrap();
        while coordinator.poll_event().is_some() {}

        control
            .set_member_data_raw(lobby, unrelated, "afc_ready", "malformed")
            .unwrap();
        control.emit_member_data_changed(lobby, unrelated).unwrap();
        coordinator.pump(&mut platform, 2).unwrap();

        assert_eq!(coordinator.status().phase, OnlineLobbyPhase::Lobby);
        assert!(coordinator.status().failure.is_none());
        assert!(coordinator.binding(owner).is_some());
        assert!(coordinator.binding(unrelated).is_some());
        let events: Vec<_> = std::iter::from_fn(|| coordinator.poll_event()).collect();
        assert!(!events.iter().any(|event| matches!(
            event,
            OnlineLobbyEvent::PeerAuthenticationRejected { user, .. }
                if *user == unrelated
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            OnlineLobbyEvent::RosterChanged { members, .. } if *members == 2
        )));
    }

    #[test]
    fn pure_flow_transitions_are_guarded_and_deadlined() {
        let timeouts = OnlineLobbyTimeouts {
            platform_operation_ms: 25,
            ..OnlineLobbyTimeouts::default()
        };
        let mut flow = OnlineFlowMachine::new(100);
        flow.transition(OnlineLobbyPhase::CreatingLobby, 101, timeouts)
            .unwrap();
        assert_eq!(flow.phase(), OnlineLobbyPhase::CreatingLobby);
        assert_eq!(flow.deadline_at_ms(), Some(126));
        assert!(!flow.is_expired(125));
        assert!(flow.is_expired(126));
        assert_eq!(
            flow.transition(OnlineLobbyPhase::Loading, 102, timeouts),
            Err(OnlineLobbyError::InvalidTransition {
                from: OnlineLobbyPhase::CreatingLobby,
                to: OnlineLobbyPhase::Loading,
            })
        );
        assert_eq!(
            flow.transition(OnlineLobbyPhase::Lobby, 99, timeouts),
            Err(OnlineLobbyError::TimeRegression)
        );
    }

    #[test]
    fn phase_timeouts_have_stable_user_recovery_projection() {
        let authentication = timeout_failure(OnlineLobbyPhase::Authenticating);
        assert_eq!(
            authentication.code,
            OnlineFailureCode::AuthenticationTimedOut
        );
        assert_eq!(authentication.recovery, OnlineRecoveryAction::Retry);

        let reconnect = timeout_failure(OnlineLobbyPhase::Reconnecting);
        assert_eq!(reconnect.code, OnlineFailureCode::ConnectionTimedOut);
        assert_eq!(reconnect.recovery, OnlineRecoveryAction::ReturnToLobby);

        let loading = timeout_failure(OnlineLobbyPhase::Loading);
        assert_eq!(loading.code, OnlineFailureCode::LoadingTimedOut);
        assert_eq!(loading.recovery, OnlineRecoveryAction::ReturnToLobby);
    }

    #[test]
    fn ticket_replay_is_idempotent_and_peer_isolation_removes_only_attributed_binding() {
        let host_user = user(HOST_RAW);
        let client_user = user(CLIENT_RAW);
        let host_peer = peer(9_101);
        let client_peer = peer(9_102);
        let host_member = member(host_user, host_peer, 1, true, 0, 0);
        let client_member = member(client_user, client_peer, 1, true, 1, 1);
        let lobby = SteamLobbyId::new(88_101).unwrap();
        let (mut platform, control) = fake_platform(host_user);
        seed_lobby(&control, lobby, host_user, host_member, client_member);
        control
            .set_auth_outcome(client_user, accepted_with_license_owner(client_user))
            .unwrap();
        let mut coordinator =
            OnlineLobbyCoordinator::new(host_user, coordinator_config(), 0).unwrap();
        coordinator
            .begin_join(&mut platform, join_intent(lobby), host_member, 0)
            .unwrap();
        coordinator.pump(&mut platform, 1).unwrap();
        coordinator
            .begin_peer_authentication(
                &mut platform,
                client_user,
                client_peer,
                &[1, 2, 3],
                AdmissionPurpose::Initial,
                2,
            )
            .unwrap();
        coordinator
            .begin_peer_authentication(
                &mut platform,
                client_user,
                client_peer,
                &[1, 2, 3],
                AdmissionPurpose::Initial,
                2,
            )
            .unwrap();
        coordinator.pump(&mut platform, 2).unwrap();
        assert!(coordinator.binding(client_user).unwrap().authenticated);
        assert!(coordinator.binding(host_user).unwrap().authenticated);
        assert!(!control.ended_auth_session(client_user));
        coordinator
            .begin_peer_authentication(
                &mut platform,
                client_user,
                client_peer,
                &[1, 2, 3],
                AdmissionPurpose::Initial,
                2,
            )
            .unwrap();

        assert_eq!(
            coordinator
                .isolate_peer_authentication(&mut platform, client_user)
                .unwrap(),
            Some(client_peer)
        );

        assert!(control.ended_auth_session(client_user));
        assert!(coordinator.binding(client_user).is_none());
        assert!(coordinator.binding(host_user).unwrap().authenticated);
        assert_eq!(coordinator.status().connected_remote_peers, 0);
        assert!(coordinator.status().failure.is_none());
    }

    #[test]
    fn rejected_ticket_is_peer_scoped_for_host_and_prevents_manifest_commit() {
        let host_user = user(HOST_RAW);
        let client_user = user(CLIENT_RAW);
        let host_peer = peer(9_111);
        let client_peer = peer(9_112);
        let host_member = member(host_user, host_peer, 1, true, 0, 0);
        let client_member = member(client_user, client_peer, 1, true, 1, 1);
        let lobby = SteamLobbyId::new(88_111).unwrap();
        let (mut platform, control) = fake_platform(host_user);
        seed_lobby(&control, lobby, host_user, host_member, client_member);
        let mut coordinator =
            OnlineLobbyCoordinator::new(host_user, coordinator_config(), 0).unwrap();
        coordinator
            .begin_join(&mut platform, join_intent(lobby), host_member, 0)
            .unwrap();
        coordinator.pump(&mut platform, 1).unwrap();
        coordinator
            .reserve_peer_binding(client_user, client_peer)
            .unwrap();
        coordinator.binding_mut(client_user).unwrap().authenticated = true;
        while coordinator.poll_event().is_some() {}

        let handle = coordinator
            .issue_auth_ticket(&mut platform, client_user, AdmissionPurpose::Initial)
            .unwrap();
        control
            .set_queued_auth_ticket_result(handle.handle, false)
            .unwrap();
        coordinator.pump(&mut platform, 2).unwrap();

        let events: Vec<_> = std::iter::from_fn(|| coordinator.poll_event()).collect();
        let rejection_index = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    OnlineLobbyEvent::PeerAuthenticationRejected { user, .. }
                        if *user == client_user
                )
            })
            .unwrap();
        let barrier_index = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    OnlineLobbyEvent::RosterChanged { live_bindings, .. }
                        if live_bindings
                            .iter()
                            .flatten()
                            .all(|identity| identity.user != client_user)
                )
            })
            .unwrap();
        assert!(rejection_index < barrier_index);
        assert_eq!(coordinator.status().phase, OnlineLobbyPhase::Lobby);
        assert!(coordinator.status().failure.is_none());
        assert!(coordinator.binding(client_user).is_none());
        assert!(coordinator.binding(host_user).is_some());
        assert_eq!(
            coordinator.commit_manifest(
                &mut platform,
                OnlineManifestOptions::casual_listen(
                    MatchId::new([0xa1; 16]).unwrap(),
                    host_peer,
                    definition(0),
                    definition(1),
                    0xa11c_0001,
                    SimTick(120),
                ),
                SimTick::ZERO,
                3,
            ),
            Err(OnlineLobbyError::PeersNotReady)
        );
    }

    #[test]
    fn rejected_ticket_for_client_owner_clears_binding_before_failure() {
        let host_user = user(HOST_RAW);
        let client_user = user(CLIENT_RAW);
        let host_peer = peer(9_121);
        let client_peer = peer(9_122);
        let host_member = member(host_user, host_peer, 1, true, 0, 0);
        let client_member = member(client_user, client_peer, 1, true, 1, 1);
        let lobby = SteamLobbyId::new(88_121).unwrap();
        let (mut platform, control) = fake_platform(client_user);
        seed_lobby(&control, lobby, host_user, host_member, client_member);
        let mut coordinator =
            OnlineLobbyCoordinator::new(client_user, coordinator_config(), 0).unwrap();
        coordinator
            .begin_join(&mut platform, join_intent(lobby), client_member, 0)
            .unwrap();
        coordinator.pump(&mut platform, 1).unwrap();
        coordinator
            .reserve_peer_binding(host_user, host_peer)
            .unwrap();
        coordinator.binding_mut(host_user).unwrap().authenticated = true;
        while coordinator.poll_event().is_some() {}

        let handle = coordinator
            .issue_auth_ticket(&mut platform, host_user, AdmissionPurpose::Initial)
            .unwrap();
        control
            .set_queued_auth_ticket_result(handle.handle, false)
            .unwrap();
        coordinator.pump(&mut platform, 2).unwrap();

        let events: Vec<_> = std::iter::from_fn(|| coordinator.poll_event()).collect();
        let rejection_index = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    OnlineLobbyEvent::PeerAuthenticationRejected { user, .. }
                        if *user == host_user
                )
            })
            .unwrap();
        let barrier_index = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    OnlineLobbyEvent::RosterChanged { live_bindings, .. }
                        if live_bindings
                            .iter()
                            .flatten()
                            .all(|identity| identity.user != host_user)
                )
            })
            .unwrap();
        let failure_index = events
            .iter()
            .position(|event| matches!(event, OnlineLobbyEvent::Failure(_)))
            .unwrap();
        assert!(rejection_index < barrier_index);
        assert!(barrier_index < failure_index);
        assert!(coordinator.binding(host_user).is_none());
        assert_eq!(coordinator.status().phase, OnlineLobbyPhase::Failed);
        assert_eq!(
            coordinator.status().failure.map(|failure| failure.code),
            Some(OnlineFailureCode::AuthenticationFailed)
        );
    }

    #[test]
    fn late_authenticated_peer_rejection_isolated_before_roster_barrier() {
        let host_user = user(HOST_RAW);
        let client_user = user(CLIENT_RAW);
        let host_peer = peer(9_131);
        let client_peer = peer(9_132);
        let host_member = member(host_user, host_peer, 1, true, 0, 0);
        let client_member = member(client_user, client_peer, 1, true, 1, 1);
        let lobby = SteamLobbyId::new(88_131).unwrap();
        let (mut platform, control) = fake_platform(host_user);
        seed_lobby(&control, lobby, host_user, host_member, client_member);
        control
            .set_auth_outcome(client_user, accepted_with_license_owner(client_user))
            .unwrap();
        let mut coordinator =
            OnlineLobbyCoordinator::new(host_user, coordinator_config(), 0).unwrap();
        coordinator
            .begin_join(&mut platform, join_intent(lobby), host_member, 0)
            .unwrap();
        coordinator.pump(&mut platform, 1).unwrap();
        coordinator
            .begin_peer_authentication(
                &mut platform,
                client_user,
                client_peer,
                &[1, 3, 1],
                AdmissionPurpose::Initial,
                2,
            )
            .unwrap();
        coordinator.pump(&mut platform, 2).unwrap();
        assert!(coordinator.binding(client_user).unwrap().authenticated);
        while coordinator.poll_event().is_some() {}

        control
            .emit_auth_validation(
                client_user,
                host_user,
                Err(AuthValidationFailure::TicketCancelled),
            )
            .unwrap();
        coordinator.pump(&mut platform, 3).unwrap();

        let events: Vec<_> = std::iter::from_fn(|| coordinator.poll_event()).collect();
        let rejection_index = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    OnlineLobbyEvent::PeerAuthenticationRejected { user, .. }
                        if *user == client_user
                )
            })
            .unwrap();
        let barrier_index = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    OnlineLobbyEvent::RosterChanged { live_bindings, .. }
                        if live_bindings
                            .iter()
                            .flatten()
                            .all(|identity| identity.user != client_user)
                )
            })
            .unwrap();
        assert!(rejection_index < barrier_index);
        assert!(control.ended_auth_session(client_user));
        assert!(coordinator.binding(client_user).is_none());
        assert!(coordinator.binding(host_user).is_some());
        assert_eq!(coordinator.status().phase, OnlineLobbyPhase::Lobby);
    }

    #[test]
    fn departed_lobby_identity_churn_reclaims_binding_and_quality_capacity() {
        let local = user(HOST_RAW);
        let local_peer = peer(9_201);
        let declaration = member(local, local_peer, 1, false, 0, 0);
        let (mut platform, control) = fake_platform(local);
        let mut coordinator = OnlineLobbyCoordinator::new(local, coordinator_config(), 0).unwrap();
        coordinator
            .begin_create(
                &mut platform,
                LobbyCreateRequest {
                    visibility: LobbyVisibility::Private,
                    maximum_peers: MAX_STEAM_LOBBY_MEMBERS as u8,
                    local_seats: 1,
                },
                metadata(MAX_STEAM_LOBBY_MEMBERS as u8),
                declaration,
                0,
            )
            .unwrap();
        coordinator.pump(&mut platform, 1).unwrap();
        while coordinator.poll_event().is_some() {}
        let SteamPlatformState::InLobby(lobby) = platform.state() else {
            panic!("created lobby missing");
        };

        let reused_peer = peer(9_202);
        let churn_count = MAX_STEAM_LOBBY_MEMBERS * 2 + 1;
        for ordinal in 0..churn_count {
            let remote = user(80_000 + ordinal as u64);
            control
                .emit_membership_change(
                    lobby,
                    remote,
                    crate::steam_platform::LobbyMembershipChange::Entered,
                )
                .unwrap();
            coordinator
                .pump(&mut platform, 2 + (ordinal as u64 * 2))
                .unwrap();
            while coordinator.poll_event().is_some() {}

            coordinator
                .reserve_peer_binding(remote, reused_peer)
                .unwrap();
            coordinator.binding_mut(remote).unwrap().authenticated = true;
            coordinator.mark_quality_rejected(remote).unwrap();
            coordinator.rebuild_roster(&platform).unwrap();
            while coordinator.poll_event().is_some() {}

            control
                .emit_membership_change(
                    lobby,
                    remote,
                    crate::steam_platform::LobbyMembershipChange::Left,
                )
                .unwrap();
            coordinator
                .pump(&mut platform, 3 + (ordinal as u64 * 2))
                .unwrap();

            assert!(coordinator.binding(remote).is_none());
            assert!(!coordinator.quality_rejected_users.contains(&Some(remote)));
            let mut barrier = None;
            while let Some(event) = coordinator.poll_event() {
                if let OnlineLobbyEvent::RosterChanged { live_bindings, .. } = event {
                    barrier = Some(live_bindings);
                }
            }
            let live_bindings = barrier.expect("departure publishes a roster barrier");
            assert_eq!(
                live_bindings.iter().flatten().copied().collect::<Vec<_>>(),
                vec![OnlinePeerIdentity {
                    user: local,
                    peer_id: local_peer,
                }]
            );
        }

        assert_eq!(coordinator.bindings.iter().flatten().count(), 1);
        assert_eq!(
            coordinator.reserve_peer_binding(user(90_000), reused_peer),
            Ok(())
        );
    }

    #[test]
    fn committed_match_roster_barrier_retains_absent_reconnect_identity() {
        let local = user(HOST_RAW);
        let remote = user(CLIENT_RAW);
        let local_peer = peer(9_301);
        let remote_peer = peer(9_302);
        let remote_connection = SteamConnectionId::new(9_302).unwrap();
        let local_member = member(local, local_peer, 1, true, 0, 0);
        let remote_member = member(remote, remote_peer, 1, true, 1, 1);
        let (mut platform, control) = fake_platform(local);
        let lobby = SteamLobbyId::new(88_301).unwrap();
        seed_lobby(&control, lobby, local, local_member, remote_member);
        let mut coordinator = OnlineLobbyCoordinator::new(local, coordinator_config(), 0).unwrap();
        coordinator
            .begin_join(&mut platform, join_intent(lobby), local_member, 0)
            .unwrap();
        coordinator.pump(&mut platform, 1).unwrap();
        coordinator
            .reserve_peer_binding(remote, remote_peer)
            .unwrap();
        let binding = coordinator.binding_mut(remote).unwrap();
        binding.authenticated = true;
        binding.admission = Some(AuthenticatedSteamPeer {
            lobby,
            user: remote,
            license_owner_user: local,
            authenticated_user: remote.authenticated(),
            local_seats: 1,
            purpose: AdmissionPurpose::Initial,
        });
        binding.connection = Some(remote_connection);
        binding
            .quality
            .observe(NetworkQualitySample {
                rtt_ms: 160,
                loss_bps: 0,
            })
            .unwrap();
        let retained_quality = binding.quality.snapshot();
        control
            .set_auth_outcome(remote, accepted_with_license_owner(remote))
            .unwrap();
        platform
            .begin_peer_authentication(lobby, remote, &[7, 3], AdmissionPurpose::Initial, 1)
            .unwrap();
        coordinator.pump(&mut platform, 2).unwrap();
        coordinator.rebuild_roster(&platform).unwrap();
        coordinator.match_config = Some(
            coordinator
                .roster
                .build_headless_config(
                    OnlineManifestOptions::casual_listen(
                        MatchId::new(*b"churn-fixed-0001").unwrap(),
                        local_peer,
                        definition(0),
                        definition(1),
                        0xAFC0_9301,
                        SimTick(120),
                    ),
                    SimTick::ZERO,
                )
                .unwrap(),
        );
        coordinator.flow.phase = OnlineLobbyPhase::Fighting;
        while coordinator.poll_event().is_some() {}

        control
            .emit_membership_change(
                lobby,
                remote,
                crate::steam_platform::LobbyMembershipChange::Left,
            )
            .unwrap();
        coordinator.pump(&mut platform, 3).unwrap();

        let retained = coordinator.binding(remote).unwrap();
        assert_eq!(retained.peer_id, remote_peer);
        assert!(!retained.authenticated);
        assert!(retained.admission.is_none());
        assert!(retained.pending_connection.is_none());
        assert!(retained.connection.is_none());
        assert_eq!(retained.quality.snapshot(), retained_quality);
        assert!(control.ended_auth_session(remote));
        let events: Vec<_> = std::iter::from_fn(|| coordinator.poll_event()).collect();
        let disconnect_index = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    OnlineLobbyEvent::PeerDisconnected {
                        connection,
                        user,
                        peer_id,
                        reconnect_allowed: true,
                        ..
                    } if *connection == remote_connection
                        && *user == remote
                        && *peer_id == remote_peer
                )
            })
            .expect("committed departure emits one disconnect");
        let (barrier_index, live_bindings) = events
            .iter()
            .enumerate()
            .find_map(|(index, event)| match event {
                OnlineLobbyEvent::RosterChanged { live_bindings, .. } => {
                    Some((index, *live_bindings))
                }
                _ => None,
            })
            .expect("fixed-match membership update publishes a barrier");
        assert!(disconnect_index < barrier_index);
        assert!(live_bindings.contains(&Some(OnlinePeerIdentity {
            user: remote,
            peer_id: remote_peer,
        })));
        coordinator
            .reconcile_departed_lobby_bindings(&mut platform, 4)
            .unwrap();
        assert!(
            !std::iter::from_fn(|| coordinator.poll_event()).any(|event| {
                matches!(
                    event,
                    OnlineLobbyEvent::PeerDisconnected { user, .. } if user == remote
                )
            })
        );

        control
            .emit_membership_change(
                lobby,
                remote,
                crate::steam_platform::LobbyMembershipChange::Entered,
            )
            .unwrap();
        coordinator.pump(&mut platform, 5).unwrap();
        control
            .set_member_loadout(
                lobby,
                remote,
                MemberLoadoutDeclaration::new(&encode_member_declaration(&remote_member)).unwrap(),
            )
            .unwrap();
        control
            .set_member_readiness(
                lobby,
                remote,
                MemberReadiness::Declared {
                    ready: true,
                    local_seats: 1,
                },
            )
            .unwrap();
        control.emit_member_data_changed(lobby, remote).unwrap();
        coordinator.pump(&mut platform, 6).unwrap();
        control
            .set_auth_outcome(remote, accepted_with_license_owner(remote))
            .unwrap();
        coordinator
            .begin_peer_authentication(
                &mut platform,
                remote,
                remote_peer,
                &[9, 3, 1],
                AdmissionPurpose::Reconnect,
                7,
            )
            .unwrap();
        assert_eq!(coordinator.binding(remote).unwrap().peer_id, remote_peer);
    }

    #[test]
    fn client_accepts_exact_three_member_manifest_without_authenticating_third_party() {
        let (platform, _, mut coordinator, _, members) = three_member_client_fixture();
        let config = manifest_for_members(&members, members[0].peer_id, *b"manifest-three01");

        coordinator
            .accept_manifest(&platform, config.clone(), 2)
            .unwrap();

        assert_eq!(coordinator.status().phase, OnlineLobbyPhase::Loading);
        assert_eq!(
            coordinator.match_config().map(|config| config.manifest),
            Some(config.manifest)
        );
        assert!(coordinator.binding(user(81_002)).is_none());
    }

    #[test]
    fn exact_manifest_reconstruction_rejects_omission_mutation_and_peer_reassignment() {
        let (platform, _, mut omitted_client, _, members) = three_member_client_fixture();
        let omitted = manifest_for_members(
            &[members[0], members[2]],
            members[0].peer_id,
            *b"manifest-omit001",
        );
        assert_eq!(
            omitted_client.accept_manifest(&platform, omitted, 2),
            Err(OnlineLobbyError::ManifestMismatch)
        );

        let (platform, _, mut mutated_client, _, members) = three_member_client_fixture();
        let mutated_third = member(
            user(81_002),
            members[1].peer_id,
            members[1].revision,
            true,
            0,
            1,
        );
        let mutated = manifest_for_members(
            &[members[0], mutated_third, members[2]],
            members[0].peer_id,
            *b"manifest-mutate1",
        );
        assert_eq!(
            mutated_client.accept_manifest(&platform, mutated, 2),
            Err(OnlineLobbyError::ManifestMismatch)
        );

        let (platform, _, mut reassigned_client, _, members) = three_member_client_fixture();
        let reassigned_third = member(
            user(81_002),
            members[1].peer_id,
            members[1].revision,
            true,
            2,
            0,
        );
        let reassigned_local = member(
            user(81_003),
            members[2].peer_id,
            members[2].revision,
            true,
            1,
            1,
        );
        let reassigned = manifest_for_members(
            &[members[0], reassigned_third, reassigned_local],
            members[0].peer_id,
            *b"manifest-reasgn1",
        );
        assert_eq!(
            reassigned_client.accept_manifest(&platform, reassigned, 2),
            Err(OnlineLobbyError::ManifestMismatch)
        );
    }

    #[test]
    fn pending_coherent_declaration_defers_manifest_until_snapshot_is_complete() {
        let (mut platform, control, mut coordinator, lobby, members) =
            three_member_client_fixture();
        let config = manifest_for_members(&members, members[0].peer_id, *b"manifest-pend001");
        let third_user = user(81_002);
        control
            .set_member_readiness(lobby, third_user, MemberReadiness::Pending)
            .unwrap();
        control.emit_member_data_changed(lobby, third_user).unwrap();
        coordinator.pump(&mut platform, 2).unwrap();

        assert_eq!(
            coordinator.accept_manifest(&platform, config.clone(), 3),
            Err(OnlineLobbyError::ManifestDeclarationsPending)
        );
        assert_eq!(
            coordinator.status().phase,
            OnlineLobbyPhase::ManifestAgreement
        );

        control
            .set_member_readiness(
                lobby,
                third_user,
                MemberReadiness::Declared {
                    ready: true,
                    local_seats: 1,
                },
            )
            .unwrap();
        control.emit_member_data_changed(lobby, third_user).unwrap();
        coordinator.pump(&mut platform, 4).unwrap();
        coordinator.accept_manifest(&platform, config, 5).unwrap();
        assert_eq!(coordinator.status().phase, OnlineLobbyPhase::Loading);
    }

    #[test]
    fn stale_connection_close_cannot_clear_a_replacement_link() {
        let local = user(82_001);
        let remote = user(82_002);
        let other = user(82_003);
        let local_member = member(local, peer(10_201), 1, false, 0, 0);
        let (mut platform, _) = fake_platform(local);
        let config = coordinator_config();
        let mut coordinator = OnlineLobbyCoordinator::new(local, config, 0).unwrap();
        coordinator
            .begin_create(
                &mut platform,
                LobbyCreateRequest {
                    visibility: LobbyVisibility::Private,
                    maximum_peers: 4,
                    local_seats: 1,
                },
                metadata(4),
                local_member,
                0,
            )
            .unwrap();
        coordinator.pump(&mut platform, 1).unwrap();
        let session = coordinator.take_transport_request().unwrap();
        let network = FakeSteamTransportNetwork::new(16).unwrap();
        coordinator
            .install_transport(
                network
                    .create_transport(local, session, config.transport, 1)
                    .unwrap(),
                1,
            )
            .unwrap();
        let client_session = SteamP2pSession {
            role: SteamTransportRole::Client,
            ..session
        };
        let mut old_client = network
            .create_transport(remote, client_session, config.transport, 1)
            .unwrap();
        let mut replacement_client = network
            .create_transport(other, client_session, config.transport, 1)
            .unwrap();
        let host_admission = AuthenticatedSteamPeer {
            lobby: session.lobby,
            user: local,
            license_owner_user: local,
            authenticated_user: local.authenticated(),
            local_seats: 1,
            purpose: AdmissionPurpose::Reconnect,
        };
        let stale_connection = old_client.connect_p2p(host_admission, 2).unwrap();
        let replacement_connection = replacement_client.connect_p2p(host_admission, 2).unwrap();
        let remote_peer = peer(10_202);
        coordinator
            .reserve_peer_binding(remote, remote_peer)
            .unwrap();
        let binding = coordinator.binding_mut(remote).unwrap();
        binding.authenticated = true;
        binding.admission = Some(AuthenticatedSteamPeer {
            lobby: session.lobby,
            user: remote,
            license_owner_user: local,
            authenticated_user: remote.authenticated(),
            local_seats: 1,
            purpose: AdmissionPurpose::Reconnect,
        });
        binding.connection = Some(replacement_connection);

        coordinator
            .handle_transport_event(
                &mut platform,
                SteamTransportEvent::ConnectionClosed {
                    connection: stale_connection,
                    lobby: session.lobby,
                    user: remote,
                    reason: SteamTransportCloseReason::RemoteClosed,
                },
                3,
            )
            .unwrap();

        let retained = coordinator.binding(remote).unwrap();
        assert!(retained.authenticated);
        assert!(retained.admission.is_some());
        assert_eq!(retained.connection, Some(replacement_connection));
        assert!(
            !std::iter::from_fn(|| coordinator.poll_event()).any(|event| {
                matches!(event, OnlineLobbyEvent::PeerDisconnected { user, .. } if user == remote)
            })
        );
    }

    #[test]
    fn authority_terminal_mark_is_exact_and_close_order_independent() {
        let local = user(HOST_RAW);
        let remote = user(CLIENT_RAW);
        let local_peer = peer(9_701);
        let remote_peer = peer(9_702);
        let local_member = member(local, local_peer, 1, true, 0, 0);
        let remote_member = member(remote, remote_peer, 1, true, 1, 1);
        let (mut platform, _) = fake_platform(local);
        let mut coordinator = OnlineLobbyCoordinator::new(local, coordinator_config(), 0).unwrap();
        coordinator
            .begin_create(
                &mut platform,
                LobbyCreateRequest {
                    visibility: LobbyVisibility::Private,
                    maximum_peers: 2,
                    local_seats: 1,
                },
                metadata(2),
                local_member,
                0,
            )
            .unwrap();
        coordinator.pump(&mut platform, 1).unwrap();
        let session = coordinator.take_transport_request().unwrap();
        let lobby = coordinator.lobby.unwrap();

        let network = FakeSteamTransportNetwork::new(8).unwrap();
        let mut host_transport = network
            .create_transport(local, session, SteamTransportConfig::default(), 1)
            .unwrap();
        host_transport.start_listening().unwrap();
        host_transport
            .set_allowed_incoming_users(&[remote])
            .unwrap();
        let mut remote_transport = network
            .create_transport(
                remote,
                SteamP2pSession {
                    role: SteamTransportRole::Client,
                    ..session
                },
                SteamTransportConfig::default(),
                1,
            )
            .unwrap();
        let connection = remote_transport
            .connect_p2p(
                AuthenticatedSteamPeer {
                    lobby,
                    user: local,
                    license_owner_user: local,
                    authenticated_user: local.authenticated(),
                    local_seats: 1,
                    purpose: AdmissionPurpose::Initial,
                },
                1,
            )
            .unwrap();

        coordinator
            .reserve_peer_binding(remote, remote_peer)
            .unwrap();
        let binding = coordinator.binding_mut(remote).unwrap();
        binding.authenticated = true;
        binding.connection = Some(connection);
        coordinator.match_config = Some(manifest_for_members(
            &[local_member, remote_member],
            local_peer,
            *b"terminal-mark001",
        ));
        coordinator.flow.phase = OnlineLobbyPhase::Fighting;
        while coordinator.poll_event().is_some() {}

        assert!(
            !coordinator
                .mark_authority_terminal_drained(
                    &mut platform,
                    remote,
                    peer(9_799),
                    connection,
                    None,
                )
                .unwrap()
        );
        assert!(
            coordinator
                .mark_authority_terminal_drained(
                    &mut platform,
                    remote,
                    remote_peer,
                    connection,
                    None,
                )
                .unwrap()
        );
        coordinator
            .handle_transport_event(
                &mut platform,
                SteamTransportEvent::ConnectionClosed {
                    connection,
                    lobby,
                    user: remote,
                    reason: SteamTransportCloseReason::EndpointDropped,
                },
                2,
            )
            .unwrap();
        assert!(
            coordinator
                .binding(remote)
                .unwrap()
                .deferred_authority_close
                .is_none()
        );
        assert!(
            !std::iter::from_fn(|| coordinator.poll_event()).any(|event| {
                matches!(event, OnlineLobbyEvent::PeerDisconnected { user, .. } if user == remote)
            })
        );

        // Invert callback order. The close fact remains bounded for one
        // coordinator turn and the exact terminal mark consumes it without
        // manufacturing reconnect.
        let binding = coordinator.binding_mut(remote).unwrap();
        binding.authenticated = true;
        binding.connection = Some(connection);
        coordinator
            .handle_transport_event(
                &mut platform,
                SteamTransportEvent::ConnectionClosed {
                    connection,
                    lobby,
                    user: remote,
                    reason: SteamTransportCloseReason::EndpointDropped,
                },
                3,
            )
            .unwrap();
        assert_eq!(
            coordinator
                .binding(remote)
                .unwrap()
                .deferred_authority_close,
            Some(DeferredAuthorityClose {
                connection,
                reason: SteamTransportCloseReason::EndpointDropped,
            })
        );
        assert!(
            coordinator
                .mark_authority_terminal_drained(
                    &mut platform,
                    remote,
                    remote_peer,
                    connection,
                    None,
                )
                .unwrap()
        );
        coordinator.flush_deferred_authority_closes(4).unwrap();
        assert!(
            !std::iter::from_fn(|| coordinator.poll_event()).any(|event| {
                matches!(event, OnlineLobbyEvent::PeerDisconnected { user, .. } if user == remote)
            })
        );
    }

    #[test]
    fn admission_quiesce_rejects_pending_work_but_preserves_established_ack_drain() {
        let ConnectedHostFixture {
            mut host_platform,
            host_control,
            mut host,
            network,
            mut client_transport,
            mut host_endpoint,
            mut client_endpoint,
            session,
            host_user,
            client_user,
            pending_user,
            ..
        } = connected_host_fixture();
        let established = host_endpoint.connection;
        let mut pending_transport = network
            .create_transport(
                pending_user,
                SteamP2pSession {
                    role: SteamTransportRole::Client,
                    ..session
                },
                host.config.transport,
                6,
            )
            .unwrap();
        let pending = pending_transport
            .connect_p2p(
                AuthenticatedSteamPeer {
                    lobby: session.lobby,
                    user: host_user,
                    license_owner_user: host_user,
                    authenticated_user: host_user.authenticated(),
                    local_seats: 1,
                    purpose: AdmissionPurpose::Initial,
                },
                7,
            )
            .unwrap();
        host.pump(&mut host_platform, 7).unwrap();
        assert!(
            host.pending_incoming.iter().flatten().any(|incoming| {
                incoming.user == pending_user && incoming.connection == pending
            })
        );
        assert!(host.events.iter().any(|event| {
            matches!(
                event,
                OnlineLobbyEvent::AuthenticationRequired {
                    user,
                    reconnect: false,
                } if *user == pending_user
            )
        }));

        let ticket = host
            .issue_auth_ticket(&mut host_platform, pending_user, AdmissionPurpose::Initial)
            .unwrap();
        host.quiesce_admission(&mut host_platform).unwrap();

        assert!(host.admission_is_quiesced());
        assert!(host.pending_incoming.iter().all(Option::is_none));
        assert!(host_control.cancelled_ticket(ticket.handle));
        assert!(!host_control.ended_auth_session(client_user));
        assert_eq!(
            host.transport
                .as_ref()
                .unwrap()
                .connection_state(established),
            Some(SteamTransportConnectionState::Connected)
        );
        assert!(!host.transport.as_ref().unwrap().is_listening());
        assert_eq!(host.take_transport_request(), None);
        assert!(host.take_endpoint().is_none());
        assert_eq!(
            host.issue_auth_ticket(&mut host_platform, pending_user, AdmissionPurpose::Initial,),
            Err(OnlineLobbyError::AdmissionQuiesced)
        );
        assert_eq!(
            host.begin_peer_authentication(
                &mut host_platform,
                pending_user,
                peer(14_003),
                &[9],
                AdmissionPurpose::Initial,
                7,
            ),
            Err(OnlineLobbyError::AdmissionQuiesced)
        );
        assert!(!host.events.iter().any(|event| {
            matches!(
                event,
                OnlineLobbyEvent::TransportRequested(_)
                    | OnlineLobbyEvent::AuthTicketReady { .. }
                    | OnlineLobbyEvent::AuthenticationRequired { .. }
                    | OnlineLobbyEvent::PeerAuthenticated { .. }
                    | OnlineLobbyEvent::EndpointReady { .. }
            )
        }));

        // A ticket-ready callback already queued in the fake platform and the
        // rejected pending link are both drained after the fence without
        // recreating an admission capability.
        host.pump(&mut host_platform, 8).unwrap();
        pending_transport.pump(8).unwrap();
        assert!(!std::iter::from_fn(|| host.poll_event()).any(|event| {
            matches!(
                event,
                OnlineLobbyEvent::AuthTicketReady { .. }
                    | OnlineLobbyEvent::AuthenticationRequired { .. }
                    | OnlineLobbyEvent::PeerAuthenticated { .. }
                    | OnlineLobbyEvent::EndpointReady { .. }
            )
        }));

        let final_ack = crate::network_io::AfcDatagram::try_from_slice(&[0xAC, 0x4B]).unwrap();
        assert_eq!(
            host_endpoint.endpoint.try_send(final_ack.clone()),
            crate::network_io::SendOutcome::Sent
        );
        drop(host_endpoint);
        host.pump(&mut host_platform, 9).unwrap();
        client_transport.pump(9).unwrap();
        assert_eq!(
            client_endpoint.endpoint.try_receive(),
            crate::network_io::ReceiveOutcome::Received(final_ack)
        );
        assert_eq!(
            host.transport
                .as_ref()
                .unwrap()
                .connection_state(established),
            Some(SteamTransportConnectionState::Connected)
        );
    }

    #[test]
    fn delayed_old_generation_close_cannot_clear_authenticated_replacement() {
        let ConnectedHostFixture {
            mut host_platform,
            host_control,
            mut host,
            network: _,
            mut client_transport,
            mut host_endpoint,
            mut client_endpoint,
            session,
            client_user,
            client_peer,
            ..
        } = connected_host_fixture();
        let old_connection = host_endpoint.connection;
        host.match_config = Some(manifest_for_members(
            &[
                member(user(94_001), peer(14_001), 1, true, 0, 0),
                member(client_user, client_peer, 1, true, 1, 1),
            ],
            peer(14_001),
            *b"generation-old01",
        ));
        host.flow.phase = OnlineLobbyPhase::Fighting;

        assert!(
            host.mark_authority_terminal_drained(
                &mut host_platform,
                client_user,
                client_peer,
                old_connection,
                Some(RetryDisposition::ReconnectAllowed),
            )
            .unwrap()
        );
        client_transport
            .mark_connection_replacement_eligible(old_connection)
            .unwrap();
        let replacement_pending = host.binding(client_user).unwrap();
        assert_eq!(replacement_pending.connection, None);
        assert_eq!(
            replacement_pending.retiring_connection,
            Some(old_connection)
        );
        assert!(!replacement_pending.authenticated);

        host_control
            .set_auth_outcome(client_user, accepted_with_license_owner(client_user))
            .unwrap();
        host.begin_peer_authentication(
            &mut host_platform,
            client_user,
            client_peer,
            &[8, 8],
            AdmissionPurpose::Reconnect,
            7,
        )
        .unwrap();
        host.pump(&mut host_platform, 8).unwrap();
        let replacement = client_transport
            .connect_p2p(
                AuthenticatedSteamPeer {
                    lobby: session.lobby,
                    user: session.authority_user,
                    license_owner_user: session.authority_user,
                    authenticated_user: session.authority_user.authenticated(),
                    local_seats: 1,
                    purpose: AdmissionPurpose::Reconnect,
                },
                9,
            )
            .unwrap();
        host.pump(&mut host_platform, 10).unwrap();
        host.pump(&mut host_platform, 11).unwrap();
        client_transport.pump(11).unwrap();
        let mut replacement_host_endpoint =
            host.take_endpoint().expect("replacement endpoint admitted");
        assert_eq!(replacement_host_endpoint.connection, replacement);
        let mut replacement_client_endpoint = client_transport
            .take_endpoint(replacement)
            .expect("replacement client endpoint ready");

        let final_old_ack = crate::network_io::AfcDatagram::try_from_slice(&[0xD1]).unwrap();
        assert_eq!(
            host_endpoint.endpoint.try_send(final_old_ack.clone()),
            crate::network_io::SendOutcome::Sent
        );
        drop(host_endpoint);
        host.pump(&mut host_platform, 12).unwrap();
        client_transport.pump(12).unwrap();
        assert_eq!(
            client_endpoint.endpoint.try_receive(),
            crate::network_io::ReceiveOutcome::Received(final_old_ack)
        );

        host.pump(&mut host_platform, 62).unwrap();
        client_transport.pump(62).unwrap();
        let retained = host.binding(client_user).unwrap();
        assert!(retained.authenticated);
        assert_eq!(retained.connection, Some(replacement));
        assert_eq!(retained.retiring_connection, None);
        assert!(
            retained
                .admission
                .is_some_and(|admission| { admission.purpose == AdmissionPurpose::Reconnect })
        );
        assert!(!std::iter::from_fn(|| host.poll_event()).any(|event| {
            matches!(
                event,
                OnlineLobbyEvent::PeerDisconnected { connection, .. }
                    if connection == old_connection
            )
        }));

        let replacement_payload = crate::network_io::AfcDatagram::try_from_slice(&[0xD2]).unwrap();
        assert_eq!(
            replacement_client_endpoint
                .endpoint
                .try_send(replacement_payload.clone()),
            crate::network_io::SendOutcome::Sent
        );
        client_transport.pump(63).unwrap();
        host.pump(&mut host_platform, 63).unwrap();
        assert_eq!(
            replacement_host_endpoint.endpoint.try_receive(),
            crate::network_io::ReceiveOutcome::Received(replacement_payload)
        );
    }

    #[test]
    fn nonmembers_are_rejected_before_pending_capacity_and_cannot_starve_a_member() {
        let local = user(83_001);
        let rogue = user(83_002);
        let legitimate = user(83_003);
        let local_member = member(local, peer(10_301), 1, true, 0, 0);
        let legitimate_peer = peer(10_302);
        let legitimate_member = member(legitimate, legitimate_peer, 1, true, 1, 1);
        let (mut platform, control) = fake_platform(local);
        let config = coordinator_config();
        let mut coordinator = OnlineLobbyCoordinator::new(local, config, 0).unwrap();
        coordinator
            .begin_create(
                &mut platform,
                LobbyCreateRequest {
                    visibility: LobbyVisibility::Private,
                    maximum_peers: 4,
                    local_seats: 1,
                },
                metadata(4),
                local_member,
                0,
            )
            .unwrap();
        coordinator.pump(&mut platform, 1).unwrap();
        let session = coordinator.take_transport_request().unwrap();
        let network = FakeSteamTransportNetwork::new(16).unwrap();
        coordinator
            .install_transport(
                network
                    .create_transport(local, session, config.transport, 1)
                    .unwrap(),
                1,
            )
            .unwrap();
        let client_session = SteamP2pSession {
            role: SteamTransportRole::Client,
            ..session
        };
        let host_admission = AuthenticatedSteamPeer {
            lobby: session.lobby,
            user: local,
            license_owner_user: local,
            authenticated_user: local.authenticated(),
            local_seats: 1,
            purpose: AdmissionPurpose::Initial,
        };
        let mut rogue_transport = network
            .create_transport(rogue, client_session, config.transport, 1)
            .unwrap();
        for attempt in 0..2_u64 {
            rogue_transport
                .connect_p2p(host_admission, 2 + attempt * 2)
                .unwrap();
            coordinator.pump(&mut platform, 3 + attempt * 2).unwrap();
            assert!(coordinator.pending_incoming.iter().all(Option::is_none));
            rogue_transport.pump(3 + attempt * 2).unwrap();
            while rogue_transport.poll_event().is_some() {}
        }

        control
            .emit_membership_change(
                session.lobby,
                legitimate,
                crate::steam_platform::LobbyMembershipChange::Entered,
            )
            .unwrap();
        coordinator.pump(&mut platform, 7).unwrap();
        while coordinator.poll_event().is_some() {}
        let mut legitimate_transport = network
            .create_transport(legitimate, client_session, config.transport, 7)
            .unwrap();
        let legitimate_connection = legitimate_transport.connect_p2p(host_admission, 8).unwrap();
        coordinator.pump(&mut platform, 9).unwrap();

        assert_eq!(
            coordinator
                .pending_incoming
                .iter()
                .flatten()
                .map(|pending| pending.connection)
                .collect::<Vec<_>>(),
            vec![legitimate_connection]
        );
        assert!(
            std::iter::from_fn(|| coordinator.poll_event()).any(|event| {
                matches!(
                    event,
                    OnlineLobbyEvent::AuthenticationRequired {
                        user,
                        reconnect: false,
                    } if user == legitimate
                )
            })
        );

        coordinator
            .reserve_peer_binding(legitimate, legitimate_peer)
            .unwrap();
        let binding = coordinator.binding_mut(legitimate).unwrap();
        binding.authenticated = true;
        binding.admission = Some(AuthenticatedSteamPeer {
            lobby: session.lobby,
            user: legitimate,
            license_owner_user: local,
            authenticated_user: legitimate.authenticated(),
            local_seats: 1,
            purpose: AdmissionPurpose::Initial,
        });
        coordinator.try_admit_incoming(legitimate, 9).unwrap();
        assert_eq!(
            coordinator.binding(legitimate).unwrap().pending_connection,
            Some(legitimate_connection)
        );
        coordinator
            .transport
            .as_mut()
            .unwrap()
            .close_connection(legitimate_connection)
            .unwrap();
        coordinator.pump(&mut platform, 10).unwrap();
        assert!(coordinator.pending_incoming.iter().all(Option::is_none));

        coordinator.match_config = Some(manifest_for_members(
            &[local_member, legitimate_member],
            local_member.peer_id,
            *b"lease-gate-test1",
        ));
        coordinator.flow.phase = OnlineLobbyPhase::Fighting;
        let late_member = user(83_004);
        control
            .emit_membership_change(
                session.lobby,
                late_member,
                crate::steam_platform::LobbyMembershipChange::Entered,
            )
            .unwrap();
        coordinator.pump(&mut platform, 11).unwrap();
        let mut late_transport = network
            .create_transport(late_member, client_session, config.transport, 11)
            .unwrap();
        late_transport.connect_p2p(host_admission, 12).unwrap();
        coordinator.pump(&mut platform, 13).unwrap();
        assert!(coordinator.pending_incoming.iter().all(Option::is_none));
        assert!(coordinator.binding(late_member).is_none());

        // A committed identity lease, unlike a pre-commit roster admission,
        // survives a delayed/temporarily missing platform membership snapshot.
        control
            .emit_membership_change(
                session.lobby,
                legitimate,
                crate::steam_platform::LobbyMembershipChange::Left,
            )
            .unwrap();
        coordinator.pump(&mut platform, 14).unwrap();
        assert!(
            !platform
                .roster()
                .iter()
                .flatten()
                .any(|member| member.user == legitimate)
        );
        assert!(coordinator.binding(legitimate).is_some());
        drop(legitimate_transport);

        let mut reconnect_transport = network
            .create_transport(legitimate, client_session, config.transport, 14)
            .unwrap();
        let reconnect_connection = reconnect_transport
            .connect_p2p(
                AuthenticatedSteamPeer {
                    purpose: AdmissionPurpose::Reconnect,
                    ..host_admission
                },
                15,
            )
            .unwrap();
        coordinator.pump(&mut platform, 16).unwrap();
        assert_eq!(
            coordinator
                .pending_incoming
                .iter()
                .flatten()
                .map(|pending| pending.connection)
                .collect::<Vec<_>>(),
            vec![reconnect_connection]
        );
        assert!(
            std::iter::from_fn(|| coordinator.poll_event()).any(|event| {
                matches!(
                    event,
                    OnlineLobbyEvent::AuthenticationRequired {
                        user,
                        reconnect: true,
                    } if user == legitimate
                )
            })
        );
    }

    #[test]
    fn countdown_reconnect_resumes_countdown_and_preserves_start_boundary() {
        let host_user = user(84_001);
        let local_user = user(84_002);
        let host_member = member(host_user, peer(10_401), 1, true, 0, 0);
        let local_member = member(local_user, peer(10_402), 1, true, 1, 1);
        let lobby = SteamLobbyId::new(88_402).unwrap();
        let (mut platform, control) = fake_platform(local_user);
        seed_lobby_members(&control, lobby, host_user, &[host_member, local_member], 2);
        let mut coordinator =
            OnlineLobbyCoordinator::new(local_user, coordinator_config(), 0).unwrap();
        coordinator
            .begin_join(&mut platform, join_intent(lobby), local_member, 0)
            .unwrap();
        coordinator.pump(&mut platform, 1).unwrap();
        coordinator
            .reserve_peer_binding(host_user, host_member.peer_id)
            .unwrap();
        coordinator.binding_mut(host_user).unwrap().authenticated = true;
        coordinator.flow.phase = OnlineLobbyPhase::ManifestAgreement;
        let config = manifest_for_members(
            &[host_member, local_member],
            host_member.peer_id,
            *b"count-reconnect1",
        );
        coordinator.accept_manifest(&platform, config, 2).unwrap();
        coordinator.mark_content_loaded(3).unwrap();
        coordinator.mark_initial_sync_complete(4).unwrap();
        let start_tick = SimTick(240);
        coordinator.begin_countdown(start_tick, 5).unwrap();

        coordinator.binding_mut(host_user).unwrap().authenticated = false;
        coordinator
            .finish_peer_disconnect(
                SteamConnectionId::new(84_001).unwrap(),
                host_user,
                host_member.peer_id,
                SteamTransportCloseReason::RemoteClosed,
                6,
            )
            .unwrap();
        assert_eq!(coordinator.status().phase, OnlineLobbyPhase::Reconnecting);
        assert_eq!(
            coordinator.reconnect_resume,
            Some(ReconnectResumePhase::Countdown)
        );
        assert_eq!(coordinator.status().countdown_start_tick, Some(start_tick));
        control
            .set_auth_outcome(host_user, accepted_with_license_owner(host_user))
            .unwrap();
        coordinator
            .begin_peer_authentication(
                &mut platform,
                host_user,
                host_member.peer_id,
                &[4, 2],
                AdmissionPurpose::Reconnect,
                7,
            )
            .unwrap();
        assert_eq!(
            coordinator.reconnect_resume,
            Some(ReconnectResumePhase::Countdown)
        );
        coordinator
            .transition(OnlineLobbyPhase::InitialSync, 8)
            .unwrap();
        coordinator.mark_initial_sync_complete(9).unwrap();
        assert_eq!(coordinator.status().phase, OnlineLobbyPhase::Countdown);
        assert_eq!(coordinator.status().countdown_start_tick, Some(start_tick));
        assert_eq!(
            coordinator.mark_fighting(SimTick(239), 10),
            Err(OnlineLobbyError::InvalidState)
        );
        coordinator.mark_fighting(start_tick, 11).unwrap();
        assert_eq!(coordinator.status().phase, OnlineLobbyPhase::Fighting);
    }

    #[test]
    fn return_to_lobby_drops_gameplay_before_releasing_old_match_bindings() {
        let local = user(HOST_RAW);
        let remote = user(CLIENT_RAW);
        let local_peer = peer(9_401);
        let remote_peer = peer(9_402);
        let declaration = member(local, local_peer, 1, true, 0, 0);
        let (mut platform, _) = fake_platform(local);
        let mut coordinator = OnlineLobbyCoordinator::new(local, coordinator_config(), 0).unwrap();
        coordinator
            .begin_create(
                &mut platform,
                LobbyCreateRequest {
                    visibility: LobbyVisibility::Private,
                    maximum_peers: MAX_STEAM_LOBBY_MEMBERS as u8,
                    local_seats: 1,
                },
                metadata(MAX_STEAM_LOBBY_MEMBERS as u8),
                declaration,
                0,
            )
            .unwrap();
        coordinator.pump(&mut platform, 1).unwrap();
        coordinator
            .reserve_peer_binding(remote, remote_peer)
            .unwrap();
        coordinator.binding_mut(remote).unwrap().authenticated = true;
        coordinator.flow.phase = OnlineLobbyPhase::Results;
        coordinator.outcome = Some(OnlineMatchOutcome::Confirmed);
        while coordinator.poll_event().is_some() {}

        coordinator.return_to_lobby(&mut platform, true, 2).unwrap();
        let events: Vec<_> = std::iter::from_fn(|| coordinator.poll_event()).collect();
        let drop_index = events
            .iter()
            .position(|event| *event == OnlineLobbyEvent::DropGameplayEndpoints)
            .unwrap();
        let barrier_index = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    OnlineLobbyEvent::RosterChanged { live_bindings, .. }
                        if live_bindings.iter().flatten().all(|identity| {
                            identity.user == local && identity.peer_id == local_peer
                        })
                            && live_bindings.iter().flatten().count() == 1
                )
            })
            .unwrap();
        let returned_index = events
            .iter()
            .position(|event| *event == OnlineLobbyEvent::ReturnedToLobby { rematch: true })
            .unwrap();
        assert!(drop_index < barrier_index);
        assert!(barrier_index < returned_index);
        assert!(coordinator.binding(remote).is_none());
    }

    #[test]
    fn leave_retires_transport_before_auth_and_lobby_cleanup() {
        let host_user = user(HOST_RAW);
        let local_user = user(CLIENT_RAW);
        let host_peer = peer(9_451);
        let local_peer = peer(9_452);
        let host_member = member(host_user, host_peer, 1, true, 0, 0);
        let local_member = member(local_user, local_peer, 1, true, 1, 1);
        let lobby = SteamLobbyId::new(88_451).unwrap();
        let (mut platform, control) = fake_platform(local_user);
        seed_lobby(&control, lobby, host_user, host_member, local_member);
        let config = coordinator_config();
        let mut coordinator = OnlineLobbyCoordinator::new(local_user, config, 0).unwrap();
        coordinator
            .begin_join(&mut platform, join_intent(lobby), local_member, 0)
            .unwrap();
        coordinator.pump(&mut platform, 1).unwrap();
        let requested = coordinator.take_transport_request().unwrap();

        control
            .set_auth_outcome(host_user, accepted_with_license_owner(host_user))
            .unwrap();
        coordinator
            .begin_peer_authentication(
                &mut platform,
                host_user,
                host_peer,
                &[4, 5],
                AdmissionPurpose::Initial,
                1,
            )
            .unwrap();
        coordinator.pump(&mut platform, 2).unwrap();
        assert!(coordinator.binding(host_user).unwrap().authenticated);
        assert!(!control.ended_auth_session(host_user));

        let network = FakeSteamTransportNetwork::new(16).unwrap();
        let host_session = SteamP2pSession {
            role: SteamTransportRole::ListenAuthority,
            ..requested
        };
        let mut host_transport = network
            .create_transport(host_user, host_session, config.transport, 2)
            .unwrap();
        let mut client_transport = network
            .create_transport(local_user, requested, config.transport, 2)
            .unwrap();
        host_transport.start_listening().unwrap();
        host_transport
            .set_allowed_incoming_users(&[local_user])
            .unwrap();
        let connection = client_transport
            .connect_p2p(
                AuthenticatedSteamPeer {
                    lobby,
                    user: host_user,
                    license_owner_user: host_user,
                    authenticated_user: host_user.authenticated(),
                    local_seats: 1,
                    purpose: AdmissionPurpose::Initial,
                },
                2,
            )
            .unwrap();
        host_transport.pump(3).unwrap();
        let _ = host_transport.poll_event();
        host_transport
            .admit_incoming(
                connection,
                AuthenticatedSteamPeer {
                    lobby,
                    user: local_user,
                    license_owner_user: local_user,
                    authenticated_user: local_user.authenticated(),
                    local_seats: 1,
                    purpose: AdmissionPurpose::Initial,
                },
                3,
            )
            .unwrap();
        host_transport.pump(4).unwrap();
        client_transport.pump(4).unwrap();
        let _ = host_transport.poll_event();
        let _ = client_transport.poll_event();
        let mut host_endpoint = host_transport.take_endpoint(connection).unwrap().endpoint;
        let mut worker_endpoint = client_transport.take_endpoint(connection).unwrap().endpoint;
        coordinator.transport = Some(client_transport);
        coordinator.binding_mut(host_user).unwrap().connection = Some(connection);
        coordinator.flow.phase = OnlineLobbyPhase::Fighting;
        while coordinator.poll_event().is_some() {}

        let final_datagram = crate::network_io::AfcDatagram::try_from_slice(&[0xAC, 0x4B]).unwrap();
        assert_eq!(
            worker_endpoint.try_send(final_datagram.clone()),
            crate::network_io::SendOutcome::Sent
        );
        coordinator.leave_online(&mut platform, 5).unwrap();
        assert_eq!(coordinator.retiring_transport_count(), 1);
        assert!(coordinator.pending_platform_leave);
        assert!(matches!(platform.state(), SteamPlatformState::InLobby(_)));
        assert!(
            !control.ended_auth_session(host_user),
            "platform auth must outlive the application transition"
        );

        // Synchronous worker stop happens after the leave command and before
        // the next coordinator/transport pump.
        drop(worker_endpoint);
        coordinator.pump(&mut platform, 6).unwrap();
        host_transport.pump(6).unwrap();
        assert_eq!(
            host_endpoint.try_receive(),
            crate::network_io::ReceiveOutcome::Received(final_datagram)
        );
        assert_eq!(coordinator.retiring_transport_count(), 1);
        assert!(!control.ended_auth_session(host_user));

        coordinator.pump(&mut platform, 56).unwrap();
        assert_eq!(coordinator.retiring_transport_count(), 0);
        assert!(!coordinator.pending_platform_leave);
        assert!(control.ended_auth_session(host_user));
        assert_eq!(platform.state(), SteamPlatformState::Idle);
        assert_eq!(
            coordinator.transport_retirement_metrics(),
            OnlineTransportRetirementMetrics {
                started: 1,
                completed: 1,
                timed_out: 0,
                faulted: 0,
                high_water: 1,
            }
        );
        coordinator.pump(&mut platform, 57).unwrap();
        assert_eq!(coordinator.transport_retirement_metrics().completed, 1);
    }

    #[test]
    fn fake_steam_create_publishes_loadout_readiness_invites_and_transport_request() {
        let local = user(HOST_RAW);
        let local_peer = peer(11);
        let declaration = member(local, local_peer, 1, false, 0, 0);
        let (mut platform, control) = fake_platform(local);
        let mut coordinator = OnlineLobbyCoordinator::new(local, coordinator_config(), 0).unwrap();
        coordinator
            .begin_create(
                &mut platform,
                LobbyCreateRequest {
                    visibility: LobbyVisibility::Private,
                    maximum_peers: 4,
                    local_seats: 1,
                },
                metadata(4),
                declaration,
                0,
            )
            .unwrap();
        coordinator.pump(&mut platform, 1).unwrap();

        let status = coordinator.status();
        assert_eq!(status.phase, OnlineLobbyPhase::Lobby);
        assert_eq!(status.role, Some(OnlineLobbyRole::ListenAuthority));
        assert_eq!(status.roster_members, 1);
        assert!(!status.all_members_ready);
        assert_eq!(platform.member_loadout(local).unwrap().revision(), 1);
        assert!(coordinator.take_transport_request().is_some());

        assert_eq!(
            coordinator.open_invite_overlay(&mut platform).unwrap(),
            SteamOverlayRequestStatus::Unavailable
        );
        assert_eq!(coordinator.status().phase, OnlineLobbyPhase::Lobby);
        assert!(coordinator.status().failure.is_none());
        control.set_overlay_enabled(true).unwrap();
        assert_eq!(
            coordinator.open_invite_overlay(&mut platform).unwrap(),
            SteamOverlayRequestStatus::Submitted
        );
        assert_eq!(control.invite_overlay_open_count(), 1);
        coordinator.set_ready(&mut platform, true).unwrap();
        assert!(coordinator.status().all_members_ready);

        let remote = user(CLIENT_RAW);
        let remote_declaration = member(remote, peer(12), 1, true, 1, 1);
        let lobby = coordinator.status().lobby.unwrap();
        control
            .emit_membership_change(
                lobby,
                remote,
                crate::steam_platform::LobbyMembershipChange::Entered,
            )
            .unwrap();
        coordinator.pump(&mut platform, 2).unwrap();
        control
            .set_member_loadout(
                lobby,
                remote,
                MemberLoadoutDeclaration::new(&encode_member_declaration(&remote_declaration))
                    .unwrap(),
            )
            .unwrap();
        control
            .set_member_readiness(
                lobby,
                remote,
                MemberReadiness::Declared {
                    ready: true,
                    local_seats: 1,
                },
            )
            .unwrap();
        control.emit_member_data_changed(lobby, remote).unwrap();
        coordinator.pump(&mut platform, 3).unwrap();

        let lease = coordinator
            .issue_auth_ticket(&mut platform, remote, AdmissionPurpose::Initial)
            .unwrap();
        assert!(coordinator.take_ready_auth_ticket(lease).is_none());
        coordinator.pump(&mut platform, 4).unwrap();
        let ticket = coordinator
            .take_ready_auth_ticket(lease)
            .expect("ticket is exposed only after its ready callback");
        assert!(!ticket.bytes().is_empty());
    }

    #[test]
    fn committed_reconnect_lease_survives_transient_roster_disappearance() {
        let host_user = user(HOST_RAW);
        let client_user = user(CLIENT_RAW);
        let (mut platform, control, mut coordinator, lobby, host_member, client_member) =
            auth_lobby_fixture(host_user);
        let match_id = commit_auth_fixture(&platform, &mut coordinator, host_member, client_member);

        control
            .emit_membership_change(
                lobby,
                client_user,
                crate::steam_platform::LobbyMembershipChange::Left,
            )
            .unwrap();
        platform.pump(2).unwrap();
        assert!(
            platform
                .roster()
                .iter()
                .flatten()
                .all(|member| member.user != client_user)
        );

        let sender = AuthPeerLease {
            user: client_user,
            peer_id: client_member.peer_id,
            revision: client_member.revision,
        };
        let scope = AuthSignalScope {
            lobby,
            purpose: AdmissionPurpose::Reconnect,
            owner_revision: host_member.revision,
            match_id: Some(match_id),
        };
        assert_eq!(
            coordinator
                .classify_auth_signal_lease(&platform, sender, scope)
                .unwrap(),
            AuthSignalLeaseStatus::Current
        );
        assert!(
            coordinator
                .authorized_auth_signal_leases()
                .iter()
                .flatten()
                .any(|lease| *lease == sender),
            "signal admission retains the coordinator-authorized identity lease"
        );
    }

    #[test]
    fn first_match_auth_signal_requires_live_lobby_membership() {
        let host_user = user(HOST_RAW);
        let (platform, _control, coordinator, lobby, host_member, _client_member) =
            auth_lobby_fixture(host_user);
        let stranger = user(97_001);
        assert_eq!(
            coordinator.classify_auth_signal_lease(
                &platform,
                AuthPeerLease {
                    user: stranger,
                    peer_id: peer(97_001),
                    revision: 1,
                },
                AuthSignalScope {
                    lobby,
                    purpose: AdmissionPurpose::Initial,
                    owner_revision: host_member.revision,
                    match_id: None,
                },
            ),
            Err(OnlineLobbyError::MissingPeerBinding(stranger))
        );
    }

    #[test]
    fn post_result_initial_uses_immutable_peer_lease_when_roster_callback_is_late() {
        let host_user = user(HOST_RAW);
        let client_user = user(CLIENT_RAW);
        let (mut platform, control, mut coordinator, lobby, host_member, client_member) =
            auth_lobby_fixture(client_user);
        commit_auth_fixture(&platform, &mut coordinator, host_member, client_member);

        control
            .emit_membership_change(
                lobby,
                host_user,
                crate::steam_platform::LobbyMembershipChange::Left,
            )
            .unwrap();
        platform.pump(2).unwrap();
        assert!(
            platform
                .roster()
                .iter()
                .flatten()
                .all(|member| member.user != host_user)
        );

        let stale = coordinator
            .classify_auth_signal_lease(
                &platform,
                AuthPeerLease {
                    user: host_user,
                    peer_id: host_member.peer_id,
                    revision: 1,
                },
                AuthSignalScope {
                    lobby,
                    purpose: AdmissionPurpose::Initial,
                    owner_revision: 1,
                    match_id: None,
                },
            )
            .unwrap();
        assert_eq!(stale, AuthSignalLeaseStatus::Stale);

        let current_sender = AuthPeerLease {
            user: host_user,
            peer_id: host_member.peer_id,
            revision: 2,
        };
        let current_scope = AuthSignalScope {
            lobby,
            purpose: AdmissionPurpose::Initial,
            owner_revision: 2,
            match_id: None,
        };
        assert_eq!(
            coordinator
                .classify_auth_signal_lease(&platform, current_sender, current_scope)
                .unwrap(),
            AuthSignalLeaseStatus::Current
        );
        assert_eq!(
            coordinator.classify_auth_signal_lease(
                &platform,
                AuthPeerLease {
                    peer_id: peer(99_902),
                    ..current_sender
                },
                current_scope,
            ),
            Err(OnlineLobbyError::PeerIdentityMismatch)
        );
    }

    #[test]
    fn reconnect_match_epoch_ignores_stale_scope_and_rejects_current_wrong_peer() {
        let host_user = user(HOST_RAW);
        let client_user = user(CLIENT_RAW);
        let (platform, _control, mut coordinator, lobby, host_member, client_member) =
            auth_lobby_fixture(host_user);
        let match_id = commit_auth_fixture(&platform, &mut coordinator, host_member, client_member);
        let sender = AuthPeerLease {
            user: client_user,
            peer_id: client_member.peer_id,
            revision: client_member.revision,
        };
        let mut scope = AuthSignalScope {
            lobby,
            purpose: AdmissionPurpose::Reconnect,
            owner_revision: host_member.revision,
            match_id: Some(MatchId::new(*b"auth-lease-v2-02").unwrap()),
        };
        assert_eq!(
            coordinator
                .classify_auth_signal_lease(&platform, sender, scope)
                .unwrap(),
            AuthSignalLeaseStatus::Stale
        );

        scope.match_id = Some(match_id);
        assert_eq!(
            coordinator
                .classify_auth_signal_lease(&platform, sender, scope)
                .unwrap(),
            AuthSignalLeaseStatus::Current
        );
        assert_eq!(
            coordinator.classify_auth_signal_lease(
                &platform,
                AuthPeerLease {
                    peer_id: peer(99_903),
                    ..sender
                },
                scope,
            ),
            Err(OnlineLobbyError::PeerIdentityMismatch)
        );
    }

    #[test]
    fn ticket_ready_callback_inversion_cancels_stale_lease_then_delivers_current() {
        let host_user = user(HOST_RAW);
        let client_user = user(CLIENT_RAW);
        let (mut platform, control, mut coordinator, lobby, _host_member, client_member) =
            auth_lobby_fixture(host_user);
        let stale_lease = coordinator
            .issue_auth_ticket(&mut platform, client_user, AdmissionPurpose::Initial)
            .unwrap();

        let next_client = member(
            client_user,
            client_member.peer_id,
            client_member.revision + 1,
            false,
            1,
            1,
        );
        mirror_member_epoch(&control, lobby, next_client);
        coordinator.pump(&mut platform, 2).unwrap();
        assert!(control.cancelled_ticket(stale_lease.handle));
        assert!(coordinator.take_ready_auth_ticket(stale_lease).is_none());
        assert!(
            std::iter::from_fn(|| coordinator.poll_event())
                .all(|event| !matches!(event, OnlineLobbyEvent::AuthTicketReady { .. }))
        );

        let current_lease = coordinator
            .issue_auth_ticket(&mut platform, client_user, AdmissionPurpose::Initial)
            .unwrap();
        assert_eq!(current_lease.remote_revision, next_client.revision);
        coordinator.pump(&mut platform, 3).unwrap();
        assert!(
            std::iter::from_fn(|| coordinator.poll_event()).any(|event| matches!(
                event,
                OnlineLobbyEvent::AuthTicketReady {
                    handle,
                    remote_user,
                } if handle == current_lease.handle && remote_user == client_user
            ))
        );
        assert!(coordinator.take_ready_auth_ticket(current_lease).is_some());
    }

    #[test]
    fn player_coordinator_rejects_public_and_dedicated_metadata_before_create() {
        let local = user(HOST_RAW);
        let declaration = member(local, peer(12), 1, false, 0, 0);

        let (mut public_platform, _) = fake_platform(local);
        let mut public_coordinator =
            OnlineLobbyCoordinator::new(local, coordinator_config(), 0).unwrap();
        let public_metadata = LobbyMetadata::current(
            AuthorityKind::Listen,
            LobbyVisibility::Public,
            RegionCode::new("test-region").unwrap(),
            definition(1),
            definition(0),
            4,
        )
        .unwrap();
        assert!(matches!(
            public_coordinator.begin_create(
                &mut public_platform,
                LobbyCreateRequest {
                    visibility: LobbyVisibility::Public,
                    maximum_peers: 4,
                    local_seats: 1,
                },
                public_metadata,
                declaration,
                0,
            ),
            Err(OnlineLobbyError::Steam(
                SteamPlatformError::PublicLobbiesDisabled
            ))
        ));
        assert_eq!(
            public_coordinator.status().phase,
            OnlineLobbyPhase::OfflineMenu
        );

        let (mut dedicated_platform, _) = fake_platform(local);
        let mut dedicated_coordinator =
            OnlineLobbyCoordinator::new(local, coordinator_config(), 0).unwrap();
        let dedicated_metadata = LobbyMetadata::current(
            AuthorityKind::Dedicated,
            LobbyVisibility::Private,
            RegionCode::new("test-region").unwrap(),
            definition(1),
            definition(0),
            4,
        )
        .unwrap();
        assert!(matches!(
            dedicated_coordinator.begin_create(
                &mut dedicated_platform,
                LobbyCreateRequest {
                    visibility: LobbyVisibility::Private,
                    maximum_peers: 4,
                    local_seats: 1,
                },
                dedicated_metadata,
                declaration,
                0,
            ),
            Err(OnlineLobbyError::Steam(
                SteamPlatformError::DedicatedSdrUnavailable
            ))
        ));
        assert_eq!(
            dedicated_coordinator.status().phase,
            OnlineLobbyPhase::OfflineMenu
        );
    }

    #[test]
    fn countdown_revalidates_the_first_release_manifest_and_lobby_contract() {
        let local = user(HOST_RAW);
        let local_peer = peer(13);
        let declaration = member(local, local_peer, 1, true, 0, 0);
        let mut roster = OnlineRoster::default();
        roster.upsert(declaration).unwrap();
        let config = roster
            .build_headless_config(
                OnlineManifestOptions::casual_listen(
                    MatchId::new(*b"scope-gate-test1").unwrap(),
                    local_peer,
                    definition(0),
                    definition(1),
                    77,
                    SimTick(120),
                ),
                SimTick::ZERO,
            )
            .unwrap();

        let mut conflicting_manifest =
            OnlineLobbyCoordinator::new(local, coordinator_config(), 0).unwrap();
        conflicting_manifest.flow.phase = OnlineLobbyPhase::Ready;
        conflicting_manifest.lobby_contract =
            Some(LobbyContract::first_release(&metadata(4)).unwrap());
        let mut trusted = config.clone();
        trusted.manifest.trusted_results = true;
        conflicting_manifest.match_config = Some(trusted);
        assert_eq!(
            conflicting_manifest.begin_countdown(SimTick(120), 1),
            Err(OnlineLobbyError::ManifestMismatch)
        );
        assert_eq!(conflicting_manifest.countdown_start_tick, None);

        let mut conflicting_metadata =
            OnlineLobbyCoordinator::new(local, coordinator_config(), 0).unwrap();
        conflicting_metadata.flow.phase = OnlineLobbyPhase::Ready;
        conflicting_metadata.lobby_contract = Some(LobbyContract {
            authority: AuthorityKind::Listen,
            visibility: LobbyVisibility::Public,
            rules: definition(1),
            arena: definition(0),
            seat_capacity: 4,
        });
        conflicting_metadata.match_config = Some(config);
        assert_eq!(
            conflicting_metadata.begin_countdown(SimTick(120), 1),
            Err(OnlineLobbyError::ManifestMismatch)
        );
        assert_eq!(conflicting_metadata.countdown_start_tick, None);
    }

    #[test]
    fn sustained_quality_reject_isolates_only_the_bad_authority_peer_until_return() {
        let host_user = user(HOST_RAW);
        let bad_user = user(CLIENT_RAW);
        let healthy_user = user(7_003);
        let host_peer = peer(61);
        let bad_peer = peer(62);
        let healthy_peer = peer(63);
        let host_member = member(host_user, host_peer, 1, true, 0, 0);
        let bad_member = member(bad_user, bad_peer, 1, true, 1, 1);
        let healthy_member = member(healthy_user, healthy_peer, 1, true, 2, 0);
        let lobby = SteamLobbyId::new(88_010).unwrap();
        let (mut platform, control) = fake_platform(host_user);
        seed_lobby_members(
            &control,
            lobby,
            host_user,
            &[host_member, bad_member, healthy_member],
            4,
        );

        let mut config = coordinator_config();
        config.quality.transition_samples = 2;
        let mut coordinator = OnlineLobbyCoordinator::new(host_user, config, 0).unwrap();
        coordinator
            .begin_join(&mut platform, join_intent(lobby), host_member, 0)
            .unwrap();
        coordinator.pump(&mut platform, 1).unwrap();
        let session = coordinator.take_transport_request().unwrap();
        let network = FakeSteamTransportNetwork::new(32).unwrap();
        let mut host_transport = network
            .create_transport(host_user, session, config.transport, 1)
            .unwrap();
        host_transport.start_listening().unwrap();
        host_transport
            .set_allowed_incoming_users(&[bad_user, healthy_user])
            .unwrap();

        let connect_remote =
            |remote_user: SteamUserId, now_ms: u64, host_transport: &mut SteamTransport| {
                let remote_session = SteamP2pSession {
                    role: SteamTransportRole::Client,
                    ..session
                };
                let mut remote_transport = network
                    .create_transport(remote_user, remote_session, config.transport, now_ms)
                    .unwrap();
                let connection = remote_transport
                    .connect_p2p(
                        AuthenticatedSteamPeer {
                            lobby,
                            user: host_user,
                            license_owner_user: host_user,
                            authenticated_user: host_user.authenticated(),
                            local_seats: 1,
                            purpose: AdmissionPurpose::Initial,
                        },
                        now_ms,
                    )
                    .unwrap();
                host_transport.pump(now_ms + 1).unwrap();
                assert!(matches!(
                    host_transport.poll_event(),
                    Some(SteamTransportEvent::IncomingPending {
                        connection: observed,
                        user,
                        ..
                    }) if observed == connection && user == remote_user
                ));
                host_transport
                    .admit_incoming(
                        connection,
                        AuthenticatedSteamPeer {
                            lobby,
                            user: remote_user,
                            license_owner_user: host_user,
                            authenticated_user: remote_user.authenticated(),
                            local_seats: 1,
                            purpose: AdmissionPurpose::Initial,
                        },
                        now_ms + 1,
                    )
                    .unwrap();
                host_transport.pump(now_ms + 2).unwrap();
                remote_transport.pump(now_ms + 2).unwrap();
                assert!(matches!(
                    host_transport.poll_event(),
                    Some(SteamTransportEvent::ConnectionReady {
                        connection: observed,
                        user,
                        ..
                    }) if observed == connection && user == remote_user
                ));
                assert!(matches!(
                    remote_transport.poll_event(),
                    Some(SteamTransportEvent::ConnectionReady {
                        connection: observed,
                        user,
                        ..
                    }) if observed == connection && user == host_user
                ));
                let host_endpoint = host_transport.take_endpoint(connection).unwrap();
                let remote_endpoint = remote_transport.take_endpoint(connection).unwrap();
                (remote_transport, connection, host_endpoint, remote_endpoint)
            };

        let (bad_transport, bad_connection, bad_host_endpoint, bad_remote_endpoint) =
            connect_remote(bad_user, 2, &mut host_transport);
        let (healthy_transport, healthy_connection, healthy_host_endpoint, healthy_remote_endpoint) =
            connect_remote(healthy_user, 5, &mut host_transport);

        coordinator.transport = Some(host_transport);
        for (remote_user, remote_peer, connection) in [
            (bad_user, bad_peer, bad_connection),
            (healthy_user, healthy_peer, healthy_connection),
        ] {
            coordinator
                .reserve_peer_binding(remote_user, remote_peer)
                .unwrap();
            let binding = coordinator.binding_mut(remote_user).unwrap();
            binding.authenticated = true;
            binding.admission = Some(AuthenticatedSteamPeer {
                lobby,
                user: remote_user,
                license_owner_user: host_user,
                authenticated_user: remote_user.authenticated(),
                local_seats: 1,
                purpose: AdmissionPurpose::Initial,
            });
            binding.connection = Some(connection);
        }
        feed_precommit_rtt(&mut coordinator, bad_user, 30);
        feed_precommit_rtt(&mut coordinator, healthy_user, 30);
        coordinator.rebuild_roster(&platform).unwrap();
        for _ in 0..2 {
            coordinator
                .binding_mut(bad_user)
                .unwrap()
                .quality
                .observe(NetworkQualitySample {
                    rtt_ms: 300,
                    loss_bps: 0,
                })
                .unwrap();
        }
        let manifest_options = OnlineManifestOptions::casual_listen(
            MatchId::new(*b"quality-isolate1").unwrap(),
            host_peer,
            definition(0),
            definition(1),
            0xAFC0_9001,
            SimTick(120),
        );
        assert_eq!(
            coordinator.commit_manifest(&mut platform, manifest_options, SimTick::ZERO, 9),
            Err(OnlineLobbyError::PeersNotReady)
        );
        coordinator.binding_mut(bad_user).unwrap().quality =
            NetworkQualityMonitor::new(config.quality).unwrap();
        coordinator
            .commit_manifest(&mut platform, manifest_options, SimTick::ZERO, 10)
            .unwrap();
        let match_config = coordinator.match_config().unwrap().clone();
        coordinator
            .accept_manifest(&platform, match_config, 11)
            .unwrap();
        coordinator.mark_content_loaded(12).unwrap();
        coordinator.mark_initial_sync_complete(13).unwrap();
        coordinator.begin_countdown(SimTick(120), 14).unwrap();
        coordinator.mark_fighting(SimTick(120), 15).unwrap();
        while coordinator.poll_event().is_some() {}

        network
            .set_connection_quality(
                healthy_connection,
                host_user,
                SteamConnectionQuality {
                    ping_ms: Some(120),
                    ..SteamConnectionQuality::default()
                },
            )
            .unwrap();
        coordinator.pump(&mut platform, 20).unwrap();
        coordinator.pump(&mut platform, 21).unwrap();
        assert_eq!(
            coordinator.binding(healthy_user).unwrap().quality.quality(),
            NetworkQuality::Warning
        );
        assert_eq!(
            coordinator
                .transport
                .as_ref()
                .unwrap()
                .connection_state(healthy_connection),
            Some(crate::steam_transport::SteamTransportConnectionState::Connected)
        );

        network
            .set_connection_quality(
                healthy_connection,
                host_user,
                SteamConnectionQuality {
                    ping_ms: Some(200),
                    ..SteamConnectionQuality::default()
                },
            )
            .unwrap();
        for now_ms in 22..=24 {
            coordinator.pump(&mut platform, now_ms).unwrap();
        }
        assert_eq!(
            coordinator.binding(healthy_user).unwrap().quality.quality(),
            NetworkQuality::Degraded
        );
        assert_eq!(
            coordinator
                .transport
                .as_ref()
                .unwrap()
                .connection_state(healthy_connection),
            Some(crate::steam_transport::SteamTransportConnectionState::Connected)
        );

        network
            .set_connection_quality(
                bad_connection,
                host_user,
                SteamConnectionQuality {
                    ping_ms: Some(2_000),
                    ..SteamConnectionQuality::default()
                },
            )
            .unwrap();
        coordinator.pump(&mut platform, 25).unwrap();
        assert_eq!(
            coordinator
                .transport
                .as_ref()
                .unwrap()
                .connection_state(bad_connection),
            Some(crate::steam_transport::SteamTransportConnectionState::Connected),
            "one transient reject-class sample must survive hysteresis"
        );
        coordinator.pump(&mut platform, 26).unwrap();

        assert_eq!(coordinator.status().phase, OnlineLobbyPhase::Fighting);
        assert!(coordinator.status().failure.is_none());
        assert_eq!(
            coordinator
                .transport
                .as_ref()
                .unwrap()
                .connection_state(bad_connection),
            None
        );
        assert_eq!(
            coordinator
                .transport
                .as_ref()
                .unwrap()
                .connection_state(healthy_connection),
            Some(crate::steam_transport::SteamTransportConnectionState::Connected)
        );
        assert_eq!(coordinator.status().connected_remote_peers, 1);
        assert_eq!(
            coordinator.issue_auth_ticket(&mut platform, bad_user, AdmissionPurpose::Reconnect,),
            Err(OnlineLobbyError::QualityPolicyRejected)
        );
        assert_eq!(
            coordinator.begin_peer_authentication(
                &mut platform,
                bad_user,
                bad_peer,
                &[9, 9, 9],
                AdmissionPurpose::Reconnect,
                27,
            ),
            Err(OnlineLobbyError::QualityPolicyRejected)
        );
        assert_eq!(coordinator.status().phase, OnlineLobbyPhase::Fighting);

        coordinator.begin_result_confirmation(28).unwrap();
        coordinator.confirm_result(29).unwrap();
        coordinator
            .return_to_lobby(&mut platform, false, 30)
            .unwrap();
        let ticket = coordinator
            .issue_auth_ticket(&mut platform, bad_user, AdmissionPurpose::Initial)
            .expect("ReturnToLobby resets the match-scoped quality rejection");
        coordinator
            .cancel_auth_ticket(&mut platform, ticket.handle)
            .unwrap();

        drop(bad_transport);
        drop(healthy_transport);
        drop(bad_host_endpoint);
        drop(bad_remote_endpoint);
        drop(healthy_host_endpoint);
        drop(healthy_remote_endpoint);
    }

    #[test]
    fn client_quality_reject_of_owner_exits_without_a_reconnect_loop() {
        let host_user = user(HOST_RAW);
        let client_user = user(CLIENT_RAW);
        let host_peer = peer(71);
        let client_peer = peer(72);
        let host_member = member(host_user, host_peer, 1, true, 0, 0);
        let client_member = member(client_user, client_peer, 1, true, 1, 1);
        let lobby = SteamLobbyId::new(88_011).unwrap();
        let (mut platform, control) = fake_platform(client_user);
        seed_lobby(&control, lobby, host_user, host_member, client_member);

        let mut config = coordinator_config();
        config.quality.transition_samples = 2;
        let mut coordinator = OnlineLobbyCoordinator::new(client_user, config, 0).unwrap();
        coordinator
            .begin_join(&mut platform, join_intent(lobby), client_member, 0)
            .unwrap();
        coordinator.pump(&mut platform, 1).unwrap();
        let client_session = coordinator.take_transport_request().unwrap();
        let host_session = SteamP2pSession {
            role: SteamTransportRole::ListenAuthority,
            ..client_session
        };
        let network = FakeSteamTransportNetwork::new(16).unwrap();
        let mut host_transport = network
            .create_transport(host_user, host_session, config.transport, 1)
            .unwrap();
        let mut client_transport = network
            .create_transport(client_user, client_session, config.transport, 1)
            .unwrap();
        host_transport.start_listening().unwrap();
        host_transport
            .set_allowed_incoming_users(&[client_user])
            .unwrap();
        let connection = client_transport
            .connect_p2p(
                AuthenticatedSteamPeer {
                    lobby,
                    user: host_user,
                    license_owner_user: host_user,
                    authenticated_user: host_user.authenticated(),
                    local_seats: 1,
                    purpose: AdmissionPurpose::Initial,
                },
                2,
            )
            .unwrap();
        host_transport.pump(3).unwrap();
        assert!(matches!(
            host_transport.poll_event(),
            Some(SteamTransportEvent::IncomingPending {
                connection: observed,
                user,
                ..
            }) if observed == connection && user == client_user
        ));
        host_transport
            .admit_incoming(
                connection,
                AuthenticatedSteamPeer {
                    lobby,
                    user: client_user,
                    license_owner_user: host_user,
                    authenticated_user: client_user.authenticated(),
                    local_seats: 1,
                    purpose: AdmissionPurpose::Initial,
                },
                3,
            )
            .unwrap();
        host_transport.pump(4).unwrap();
        client_transport.pump(4).unwrap();
        assert!(matches!(
            host_transport.poll_event(),
            Some(SteamTransportEvent::ConnectionReady {
                connection: observed,
                ..
            }) if observed == connection
        ));
        assert!(matches!(
            client_transport.poll_event(),
            Some(SteamTransportEvent::ConnectionReady {
                connection: observed,
                user,
                ..
            }) if observed == connection && user == host_user
        ));
        let mut host_endpoint = host_transport.take_endpoint(connection).unwrap();
        let mut client_endpoint = client_transport.take_endpoint(connection).unwrap();

        coordinator.transport = Some(client_transport);
        coordinator
            .reserve_peer_binding(host_user, host_peer)
            .unwrap();
        let binding = coordinator.binding_mut(host_user).unwrap();
        binding.authenticated = true;
        binding.admission = Some(AuthenticatedSteamPeer {
            lobby,
            user: host_user,
            license_owner_user: host_user,
            authenticated_user: host_user.authenticated(),
            local_seats: 1,
            purpose: AdmissionPurpose::Initial,
        });
        binding.connection = Some(connection);
        coordinator.rebuild_roster(&platform).unwrap();
        let match_config = coordinator
            .roster
            .build_headless_config(
                OnlineManifestOptions::casual_listen(
                    MatchId::new(*b"quality-client01").unwrap(),
                    host_peer,
                    definition(0),
                    definition(1),
                    0xAFC0_9002,
                    SimTick(120),
                ),
                SimTick::ZERO,
            )
            .unwrap();
        coordinator.flow.phase = OnlineLobbyPhase::ManifestAgreement;
        coordinator
            .accept_manifest(&platform, match_config, 10)
            .unwrap();
        coordinator.mark_content_loaded(11).unwrap();
        coordinator.mark_initial_sync_complete(12).unwrap();
        coordinator.begin_countdown(SimTick(120), 13).unwrap();
        coordinator.mark_fighting(SimTick(120), 14).unwrap();
        while coordinator.poll_event().is_some() {}

        network
            .set_connection_quality(
                connection,
                client_user,
                SteamConnectionQuality {
                    local_delivery_permyriad: Some(8_000),
                    remote_delivery_permyriad: Some(8_000),
                    ..SteamConnectionQuality::default()
                },
            )
            .unwrap();
        coordinator.pump(&mut platform, 20).unwrap();
        assert_eq!(coordinator.status().phase, OnlineLobbyPhase::Fighting);
        assert!(coordinator.status().transport_installed);
        coordinator.pump(&mut platform, 21).unwrap();

        let status = coordinator.status();
        assert_eq!(status.phase, OnlineLobbyPhase::Failed);
        assert_eq!(
            status.failure,
            Some(online_failure(
                OnlineFailureCode::NetworkQualityRejected,
                OnlineFailureSeverity::Recoverable,
                OnlineRecoveryAction::ReturnToLobby,
                0,
            ))
        );
        assert!(!status.transport_installed);
        assert!(coordinator.reconnect_resume.is_none());
        let events: Vec<_> = std::iter::from_fn(|| coordinator.poll_event()).collect();
        assert!(events.iter().any(|event| matches!(
            event,
            OnlineLobbyEvent::QualityChanged { user, quality }
                if *user == host_user && quality.quality == NetworkQuality::Reject
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            OnlineLobbyEvent::StateChanged {
                to: OnlineLobbyPhase::Reconnecting,
                ..
            }
        )));
        coordinator.pump(&mut platform, 22).unwrap();
        assert_eq!(coordinator.status().phase, OnlineLobbyPhase::Failed);
        host_transport.pump(22).unwrap();
        assert!(matches!(
            host_endpoint.endpoint.try_receive(),
            crate::network_io::ReceiveOutcome::Disconnected
        ));
        assert!(matches!(
            client_endpoint.endpoint.try_receive(),
            crate::network_io::ReceiveOutcome::Disconnected
        ));

        coordinator
            .return_to_lobby(&mut platform, false, 23)
            .unwrap();
        assert_eq!(coordinator.status().phase, OnlineLobbyPhase::Lobby);
    }

    #[test]
    fn fake_platform_and_transport_drive_authenticated_manifest_quality_and_host_loss() {
        let host_user = user(HOST_RAW);
        let client_user = user(CLIENT_RAW);
        let host_peer = peer(21);
        let client_peer = peer(22);
        let host_member = member(host_user, host_peer, 1, true, 0, 0);
        let client_member = member(client_user, client_peer, 1, true, 1, 1);
        let lobby = SteamLobbyId::new(88_001).unwrap();

        let (mut host_platform, host_control) = fake_platform(host_user);
        let (mut client_platform, client_control) = fake_platform(client_user);
        seed_lobby(&host_control, lobby, host_user, host_member, client_member);
        seed_lobby(
            &client_control,
            lobby,
            host_user,
            host_member,
            client_member,
        );
        host_control
            .set_auth_outcome(client_user, accepted_with_license_owner(client_user))
            .unwrap();
        client_control
            .set_auth_outcome(host_user, accepted_with_license_owner(host_user))
            .unwrap();

        let config = coordinator_config();
        let mut host = OnlineLobbyCoordinator::new(host_user, config, 0).unwrap();
        let mut client = OnlineLobbyCoordinator::new(client_user, config, 0).unwrap();
        host.begin_join(&mut host_platform, join_intent(lobby), host_member, 0)
            .unwrap();
        client
            .begin_join(&mut client_platform, join_intent(lobby), client_member, 0)
            .unwrap();
        host.pump(&mut host_platform, 1).unwrap();
        client.pump(&mut client_platform, 1).unwrap();
        assert_eq!(host.status().role, Some(OnlineLobbyRole::ListenAuthority));
        assert_eq!(client.status().role, Some(OnlineLobbyRole::Client));

        let network = FakeSteamTransportNetwork::new(32).unwrap();
        let host_session = host.take_transport_request().unwrap();
        let client_session = client.take_transport_request().unwrap();
        host.install_transport(
            network
                .create_transport(host_user, host_session, config.transport, 1)
                .unwrap(),
            1,
        )
        .unwrap();

        host.begin_peer_authentication(
            &mut host_platform,
            client_user,
            client_peer,
            &[1, 2, 3],
            AdmissionPurpose::Initial,
            2,
        )
        .unwrap();
        client
            .begin_peer_authentication(
                &mut client_platform,
                host_user,
                host_peer,
                &[4, 5, 6],
                AdmissionPurpose::Initial,
                2,
            )
            .unwrap();
        host.pump(&mut host_platform, 2).unwrap();
        client.pump(&mut client_platform, 2).unwrap();
        // The auth callback may win the race with native transport creation.
        // The approved admission is retained and dialing begins on install.
        assert_eq!(client.status().phase, OnlineLobbyPhase::Authenticating);
        client
            .install_transport(
                network
                    .create_transport(client_user, client_session, config.transport, 2)
                    .unwrap(),
                2,
            )
            .unwrap();
        host.pump(&mut host_platform, 3).unwrap();
        client.pump(&mut client_platform, 3).unwrap();
        host.pump(&mut host_platform, 4).unwrap();
        client.pump(&mut client_platform, 4).unwrap();

        let host_endpoint = host.take_endpoint().expect("host endpoint admitted");
        let client_endpoint = client.take_endpoint().expect("client endpoint admitted");
        assert_eq!(host_endpoint.connection, client_endpoint.connection);
        assert_eq!(host.status().connected_remote_peers, 1);
        assert_eq!(client.status().phase, OnlineLobbyPhase::ManifestAgreement);
        assert!(host.status().all_members_ready);
        assert_eq!(
            host.status().input_delay_calibration.state,
            InputDelayCalibrationState::Calibrating
        );
        let base_options = OnlineManifestOptions::casual_listen(
            MatchId::new(*b"lobby-flow-test1").unwrap(),
            host_peer,
            definition(0),
            definition(1),
            0xAFC0_1234,
            SimTick(120),
        );
        assert_eq!(
            host.commit_manifest(&mut host_platform, base_options, SimTick::ZERO, 4,),
            Err(OnlineLobbyError::InputDelayCalibrationIncomplete)
        );
        assert_eq!(host.status().phase, OnlineLobbyPhase::Lobby);
        assert!(host.match_config().is_none());

        feed_precommit_rtt(&mut host, client_user, 120);
        assert_eq!(
            host.status().input_delay_calibration,
            InputDelayCalibrationSnapshot {
                state: InputDelayCalibrationState::Ready,
                remote_peer_count: 1,
                calibrated_peer_count: 1,
                worst_p95_rtt_ms: Some(120),
                selected_input_delay_ticks: Some(5),
                required_rollback_ticks: Some(9),
            }
        );
        assert_eq!(
            host.commit_manifest(&mut host_platform, base_options, SimTick::ZERO, 4,),
            Err(OnlineLobbyError::InputDelayCalibrationMismatch)
        );
        let mut insufficient_rollback = base_options;
        insufficient_rollback.input_delay_ticks = 5;
        insufficient_rollback.rollback_limit_ticks = 8;
        assert_eq!(
            host.commit_manifest(&mut host_platform, insufficient_rollback, SimTick::ZERO, 4,),
            Err(OnlineLobbyError::RollbackBudgetExceeded)
        );
        assert_eq!(host.status().phase, OnlineLobbyPhase::Lobby);
        assert!(host.match_config().is_none());

        network
            .set_connection_quality(
                client_endpoint.connection,
                client_user,
                SteamConnectionQuality {
                    // Two prior healthy samples remain in the bounded rolling
                    // window; this sustained-bad fixture still crosses the
                    // degraded average without reaching the reject average.
                    ping_ms: Some(600),
                    local_delivery_permyriad: Some(9_900),
                    remote_delivery_permyriad: Some(9_900),
                    ..SteamConnectionQuality::default()
                },
            )
            .unwrap();
        client.pump(&mut client_platform, 5).unwrap();
        assert_eq!(
            client.status().network_quality.quality,
            NetworkQuality::Degraded
        );

        let mut calibrated_options = base_options;
        calibrated_options.input_delay_ticks = 5;
        host.commit_manifest(&mut host_platform, calibrated_options, SimTick::ZERO, 6)
            .unwrap();
        let match_config = host.match_config().unwrap().clone();
        host.accept_manifest(&host_platform, match_config.clone(), 7)
            .unwrap();
        client
            .accept_manifest(&client_platform, match_config, 7)
            .unwrap();
        for coordinator in [&mut host, &mut client] {
            coordinator.mark_content_loaded(8).unwrap();
            coordinator.mark_initial_sync_complete(9).unwrap();
            coordinator.begin_countdown(SimTick(180), 10).unwrap();
            coordinator.mark_fighting(SimTick(180), 11).unwrap();
        }

        // A gameplay connection can be reclaimed only after both sides end the
        // old auth session and reauthenticate the same identities. The manifest
        // and seat ownership remain immutable through the reconnect.
        let first_connection = client_endpoint.connection;
        network
            .disconnect_locally(first_connection, client_user)
            .unwrap();
        client.pump(&mut client_platform, 12).unwrap();
        host.pump(&mut host_platform, 12).unwrap();
        assert_eq!(client.status().phase, OnlineLobbyPhase::Reconnecting);
        assert_eq!(
            client.status().failure.unwrap().recovery,
            OnlineRecoveryAction::Reconnect
        );
        assert_eq!(host.status().phase, OnlineLobbyPhase::Fighting);
        drop(host_endpoint);
        drop(client_endpoint);

        host.begin_peer_authentication(
            &mut host_platform,
            client_user,
            client_peer,
            &[7, 8, 9],
            AdmissionPurpose::Reconnect,
            13,
        )
        .unwrap();
        client
            .begin_peer_authentication(
                &mut client_platform,
                host_user,
                host_peer,
                &[10, 11, 12],
                AdmissionPurpose::Reconnect,
                13,
            )
            .unwrap();
        host.pump(&mut host_platform, 13).unwrap();
        client.pump(&mut client_platform, 13).unwrap();
        host.pump(&mut host_platform, 14).unwrap();
        client.pump(&mut client_platform, 14).unwrap();
        host.pump(&mut host_platform, 15).unwrap();
        client.pump(&mut client_platform, 15).unwrap();
        let reconnect_host_endpoint = host.take_endpoint().expect("host reconnect endpoint");
        let reconnect_client_endpoint = client.take_endpoint().expect("client reconnect endpoint");
        assert_ne!(reconnect_client_endpoint.connection, first_connection);
        assert_eq!(client.status().phase, OnlineLobbyPhase::InitialSync);
        client.mark_initial_sync_complete(16).unwrap();
        assert_eq!(client.status().phase, OnlineLobbyPhase::Fighting);
        assert!(client.status().failure.is_none());

        client_control
            .emit_membership_change(
                lobby,
                host_user,
                crate::steam_platform::LobbyMembershipChange::Left,
            )
            .unwrap();
        client.pump(&mut client_platform, 17).unwrap();
        assert_eq!(client.status().phase, OnlineLobbyPhase::Results);
        assert_eq!(
            client.status().outcome,
            Some(OnlineMatchOutcome::NoContestHostLost)
        );
        assert_eq!(
            client.status().failure.unwrap().recovery,
            OnlineRecoveryAction::MatchEndedNoContest
        );
        assert_eq!(client_platform.state(), SteamPlatformState::InLobby(lobby));
        assert_eq!(client_platform.lobby_owner(), Some(client_user));
        assert_eq!(client.status().lobby, Some(lobby));
        assert_eq!(client.status().owner, Some(client_user));
        assert_eq!(client.status().role, Some(OnlineLobbyRole::ListenAuthority));
        assert!(!client.status().transport_installed);
        assert_eq!(client.take_transport_request(), None);

        // Keep both reconnect endpoints alive until the coordinator has observed
        // the terminal platform event; dropping one earlier is itself a tested
        // transport disconnect path.
        drop(reconnect_host_endpoint);
        drop(reconnect_client_endpoint);

        client
            .return_to_lobby(&mut client_platform, false, 18)
            .unwrap();
        assert_eq!(client.status().phase, OnlineLobbyPhase::Lobby);
        assert_eq!(client.status().lobby, Some(lobby));
        assert_eq!(client.status().owner, Some(client_user));
        assert_eq!(client.status().role, Some(OnlineLobbyRole::ListenAuthority));
        assert!(!client.status().all_members_ready);
        let successor_session = client
            .take_transport_request()
            .expect("successor requests a fresh between-match transport");
        assert_eq!(successor_session.lobby, lobby);
        assert_eq!(successor_session.authority_user, client_user);
        assert_eq!(successor_session.role, SteamTransportRole::ListenAuthority);

        client.set_ready(&mut client_platform, true).unwrap();
        client
            .commit_manifest(
                &mut client_platform,
                OnlineManifestOptions::casual_listen(
                    MatchId::new(*b"host-loss-next01").unwrap(),
                    client_peer,
                    definition(0),
                    definition(1),
                    0xAFC0_5678,
                    SimTick(320),
                ),
                SimTick(200),
                19,
            )
            .unwrap();
        assert_eq!(client.status().phase, OnlineLobbyPhase::ManifestAgreement);
    }

    #[test]
    fn pre_match_owner_loss_is_a_lobby_failure_not_a_no_contest_result() {
        let host_user = user(HOST_RAW);
        let client_user = user(CLIENT_RAW);
        let host_member = member(host_user, peer(41), 1, true, 0, 0);
        let client_member = member(client_user, peer(42), 1, true, 1, 1);
        let lobby = SteamLobbyId::new(88_002).unwrap();
        let (mut platform, control) = fake_platform(client_user);
        seed_lobby(&control, lobby, host_user, host_member, client_member);
        let mut coordinator =
            OnlineLobbyCoordinator::new(client_user, coordinator_config(), 0).unwrap();
        coordinator
            .begin_join(&mut platform, join_intent(lobby), client_member, 0)
            .unwrap();
        coordinator.pump(&mut platform, 1).unwrap();
        control
            .emit_membership_change(
                lobby,
                host_user,
                crate::steam_platform::LobbyMembershipChange::Left,
            )
            .unwrap();
        coordinator.pump(&mut platform, 2).unwrap();
        assert_eq!(coordinator.status().phase, OnlineLobbyPhase::Failed);
        assert_eq!(coordinator.status().outcome, None);
        assert_eq!(platform.state(), SteamPlatformState::InLobby(lobby));
        assert_eq!(platform.lobby_owner(), Some(client_user));
        assert_eq!(coordinator.status().lobby, Some(lobby));
        assert_eq!(coordinator.status().owner, Some(client_user));
        assert_eq!(
            coordinator.status().role,
            Some(OnlineLobbyRole::ListenAuthority)
        );
        assert_eq!(
            coordinator.status().failure.unwrap().severity,
            OnlineFailureSeverity::Recoverable
        );
        assert_eq!(
            coordinator.status().failure.unwrap().recovery,
            OnlineRecoveryAction::ReturnToLobby
        );
        assert_eq!(coordinator.take_transport_request(), None);

        coordinator
            .return_to_lobby(&mut platform, false, 3)
            .unwrap();
        assert_eq!(coordinator.status().phase, OnlineLobbyPhase::Lobby);
        assert_eq!(coordinator.status().lobby, Some(lobby));
        assert_eq!(
            coordinator.status().role,
            Some(OnlineLobbyRole::ListenAuthority)
        );
        let successor_session = coordinator.take_transport_request().unwrap();
        assert_eq!(successor_session.authority_user, client_user);
        assert_eq!(successor_session.role, SteamTransportRole::ListenAuthority);
    }

    #[test]
    fn confirmed_result_survives_authority_departure_without_duplicate_terminal_event() {
        let host_user = user(85_001);
        let local_user = user(85_002);
        let host_member = member(host_user, peer(10_501), 1, true, 0, 0);
        let local_member = member(local_user, peer(10_502), 1, true, 1, 1);
        let lobby = SteamLobbyId::new(88_403).unwrap();
        let (mut platform, control) = fake_platform(local_user);
        seed_lobby_members(&control, lobby, host_user, &[host_member, local_member], 2);
        let config = coordinator_config();
        let mut coordinator = OnlineLobbyCoordinator::new(local_user, config, 0).unwrap();
        coordinator
            .begin_join(&mut platform, join_intent(lobby), local_member, 0)
            .unwrap();
        coordinator.pump(&mut platform, 1).unwrap();
        let session = coordinator.take_transport_request().unwrap();
        let network = FakeSteamTransportNetwork::new(8).unwrap();
        coordinator
            .install_transport(
                network
                    .create_transport(local_user, session, config.transport, 1)
                    .unwrap(),
                1,
            )
            .unwrap();
        coordinator
            .reserve_peer_binding(host_user, host_member.peer_id)
            .unwrap();
        coordinator.binding_mut(host_user).unwrap().authenticated = true;
        coordinator.match_config = Some(manifest_for_members(
            &[host_member, local_member],
            host_member.peer_id,
            *b"confirmed-host01",
        ));
        coordinator.flow.phase = OnlineLobbyPhase::Results;
        coordinator.outcome = Some(OnlineMatchOutcome::Confirmed);
        coordinator.failure = None;
        while coordinator.poll_event().is_some() {}
        assert!(coordinator.status().transport_installed);

        control
            .emit_membership_change(
                lobby,
                host_user,
                crate::steam_platform::LobbyMembershipChange::Left,
            )
            .unwrap();
        coordinator.pump(&mut platform, 2).unwrap();

        let status = coordinator.status();
        assert_eq!(status.phase, OnlineLobbyPhase::Results);
        assert_eq!(status.outcome, Some(OnlineMatchOutcome::Confirmed));
        assert_eq!(status.failure, None);
        assert!(!status.transport_installed);
        let events: Vec<_> = std::iter::from_fn(|| coordinator.poll_event()).collect();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, OnlineLobbyEvent::DropGameplayEndpoints))
                .count(),
            1
        );
        assert!(!events.iter().any(|event| matches!(
            event,
            OnlineLobbyEvent::MatchEnded(_) | OnlineLobbyEvent::Failure(_)
        )));
    }

    #[test]
    fn confirmed_result_can_reset_to_an_unready_rematch_lobby() {
        let local = user(HOST_RAW);
        let local_peer = peer(31);
        let declaration = member(local, local_peer, 1, true, 0, 0);
        let (mut platform, _) = fake_platform(local);
        let config = coordinator_config();
        let mut coordinator = OnlineLobbyCoordinator::new(local, config, 0).unwrap();
        coordinator
            .begin_create(
                &mut platform,
                LobbyCreateRequest {
                    visibility: LobbyVisibility::Private,
                    maximum_peers: 4,
                    local_seats: 1,
                },
                metadata(1),
                declaration,
                0,
            )
            .unwrap();
        coordinator.pump(&mut platform, 1).unwrap();
        let network = FakeSteamTransportNetwork::new(8).unwrap();
        let session = coordinator.take_transport_request().unwrap();
        coordinator
            .install_transport(
                network
                    .create_transport(local, session, config.transport, 1)
                    .unwrap(),
                1,
            )
            .unwrap();
        assert_eq!(
            coordinator.status().input_delay_calibration,
            InputDelayCalibrationSnapshot {
                state: InputDelayCalibrationState::Ready,
                remote_peer_count: 0,
                calibrated_peer_count: 0,
                worst_p95_rtt_ms: Some(0),
                selected_input_delay_ticks: Some(2),
                required_rollback_ticks: Some(2),
            }
        );
        coordinator
            .commit_manifest(
                &mut platform,
                OnlineManifestOptions::casual_listen(
                    MatchId::new(*b"lobby-rematch-01").unwrap(),
                    local_peer,
                    definition(0),
                    definition(1),
                    99,
                    SimTick(120),
                ),
                SimTick::ZERO,
                2,
            )
            .unwrap();
        assert_eq!(
            coordinator.status().input_delay_calibration.state,
            InputDelayCalibrationState::Committed
        );
        let match_config = coordinator.match_config().unwrap().clone();
        coordinator
            .accept_manifest(&platform, match_config, 3)
            .unwrap();
        coordinator.mark_content_loaded(4).unwrap();
        coordinator.mark_initial_sync_complete(5).unwrap();
        coordinator.begin_countdown(SimTick(120), 6).unwrap();
        coordinator.mark_fighting(SimTick(120), 7).unwrap();
        coordinator.begin_result_confirmation(8).unwrap();
        coordinator.confirm_result(9).unwrap();
        coordinator
            .return_to_lobby(&mut platform, true, 10)
            .unwrap();
        assert_eq!(coordinator.status().phase, OnlineLobbyPhase::Lobby);
        assert!(!coordinator.status().all_members_ready);
        assert!(coordinator.status().manifest_hash.is_none());
        assert!(coordinator.take_transport_request().is_some());
    }

    fn confirmed_results_epoch_pair() -> (
        SteamLobbyId,
        SteamUserId,
        SteamUserId,
        OnlineRosterMember,
        OnlineRosterMember,
        SteamPlatform<FakeSteamBackend>,
        FakeSteamControl,
        OnlineLobbyCoordinator,
        SteamPlatform<FakeSteamBackend>,
        FakeSteamControl,
        OnlineLobbyCoordinator,
    ) {
        let host_user = user(HOST_RAW);
        let client_user = user(CLIENT_RAW);
        let host_member = member(host_user, peer(71), 1, true, 0, 0);
        let client_member = member(client_user, peer(72), 1, true, 1, 1);
        let lobby = SteamLobbyId::new(88_710).unwrap();
        let (mut host_platform, host_control) = fake_platform(host_user);
        let (mut client_platform, client_control) = fake_platform(client_user);
        seed_lobby(&host_control, lobby, host_user, host_member, client_member);
        seed_lobby(
            &client_control,
            lobby,
            host_user,
            host_member,
            client_member,
        );
        let config = coordinator_config();
        let mut host = OnlineLobbyCoordinator::new(host_user, config, 0).unwrap();
        let mut client = OnlineLobbyCoordinator::new(client_user, config, 0).unwrap();
        host.begin_join(&mut host_platform, join_intent(lobby), host_member, 0)
            .unwrap();
        client
            .begin_join(&mut client_platform, join_intent(lobby), client_member, 0)
            .unwrap();
        host.pump(&mut host_platform, 1).unwrap();
        client.pump(&mut client_platform, 1).unwrap();
        assert_eq!(host.status().role, Some(OnlineLobbyRole::ListenAuthority));
        assert_eq!(client.status().role, Some(OnlineLobbyRole::Client));
        let _ = host.take_transport_request();
        let _ = client.take_transport_request();

        let match_config = manifest_for_members(
            &[host_member, client_member],
            host_member.peer_id,
            *b"result-epoch-001",
        );
        for (coordinator, platform) in
            [(&mut host, &host_platform), (&mut client, &client_platform)]
        {
            coordinator.match_config = Some(match_config.clone());
            coordinator.flow.phase = OnlineLobbyPhase::Results;
            coordinator.outcome = Some(OnlineMatchOutcome::Confirmed);
            coordinator.failure = None;
            coordinator.capture_committed_peer_leases(platform).unwrap();
            while coordinator.poll_event().is_some() {}
        }
        (
            lobby,
            host_user,
            client_user,
            host_member,
            client_member,
            host_platform,
            host_control,
            host,
            client_platform,
            client_control,
            client,
        )
    }

    fn mirror_member_epoch(
        control: &FakeSteamControl,
        lobby: SteamLobbyId,
        member: OnlineRosterMember,
    ) {
        let user = SteamUserId::new(member.authenticated_user.get()).unwrap();
        control
            .set_member_loadout(
                lobby,
                user,
                MemberLoadoutDeclaration::new(&encode_member_declaration(&member)).unwrap(),
            )
            .unwrap();
        control
            .set_member_readiness(
                lobby,
                user,
                MemberReadiness::Declared {
                    ready: member.ready,
                    local_seats: member.seat_count() as u8,
                },
            )
            .unwrap();
        control.emit_member_data_changed(lobby, user).unwrap();
    }

    #[test]
    fn client_results_action_waits_for_owner_epoch_without_initial_authentication() {
        let (
            lobby,
            host_user,
            client_user,
            host_member,
            _client_member,
            mut host_platform,
            _host_control,
            mut host,
            mut client_platform,
            client_control,
            mut client,
        ) = confirmed_results_epoch_pair();

        client
            .return_to_lobby(&mut client_platform, true, 2)
            .unwrap();
        client.pump(&mut client_platform, 60).unwrap();
        assert_eq!(client.status().phase, OnlineLobbyPhase::Results);
        assert_eq!(client.status().outcome, Some(OnlineMatchOutcome::Confirmed));
        assert_eq!(client.status().failure, None);
        assert_eq!(client.local_declaration().unwrap().revision, 1);
        assert_eq!(client.take_transport_request(), None);
        assert_eq!(host.status().phase, OnlineLobbyPhase::Results);

        host.return_to_lobby(&mut host_platform, true, 61).unwrap();
        let owner_epoch = host.local_declaration().unwrap();
        assert_eq!(owner_epoch.revision, 2);
        assert!(!owner_epoch.ready);

        // The old physical close can race ahead of Steam member metadata. It is
        // benign only because this exact match already has a confirmed result.
        client
            .finish_peer_disconnect(
                SteamConnectionId::new(88_001).unwrap(),
                host_user,
                host_member.peer_id,
                SteamTransportCloseReason::EndpointDropped,
                62,
            )
            .unwrap();
        client.pump(&mut client_platform, 120).unwrap();
        assert_eq!(client.status().phase, OnlineLobbyPhase::Results);
        assert_eq!(client.status().failure, None);

        mirror_member_epoch(&client_control, lobby, owner_epoch);
        client.pump(&mut client_platform, 121).unwrap();
        assert_eq!(client.status().phase, OnlineLobbyPhase::Lobby);
        assert_eq!(client.status().failure, None);
        assert_eq!(client.local_declaration().unwrap().revision, 2);
        assert!(client.take_transport_request().is_some());
        assert!(
            std::iter::from_fn(|| client.poll_event())
                .any(|event| matches!(event, OnlineLobbyEvent::ReturnedToLobby { rematch: true }))
        );
        assert!(!host.initial_authentication_allowed(client_user, 1));
    }

    #[test]
    fn owner_first_epoch_converges_client_and_gates_host_initial_ticket_until_ack() {
        let (
            lobby,
            host_user,
            client_user,
            host_member,
            _client_member,
            mut host_platform,
            host_control,
            mut host,
            mut client_platform,
            client_control,
            mut client,
        ) = confirmed_results_epoch_pair();

        host.return_to_lobby(&mut host_platform, false, 2).unwrap();
        let owner_epoch = host.local_declaration().unwrap();
        assert_eq!(host.status().phase, OnlineLobbyPhase::Lobby);
        assert!(!host.initial_authentication_allowed(client_user, 1));

        client
            .finish_peer_disconnect(
                SteamConnectionId::new(88_002).unwrap(),
                host_user,
                host_member.peer_id,
                SteamTransportCloseReason::RemoteClosed,
                3,
            )
            .unwrap();
        client.pump(&mut client_platform, 60).unwrap();
        assert_eq!(client.status().phase, OnlineLobbyPhase::Results);
        assert_eq!(client.status().outcome, Some(OnlineMatchOutcome::Confirmed));
        assert_eq!(client.status().failure, None);

        mirror_member_epoch(&client_control, lobby, owner_epoch);
        client.pump(&mut client_platform, 61).unwrap();
        let client_epoch = client.local_declaration().unwrap();
        assert_eq!(client.status().phase, OnlineLobbyPhase::Lobby);
        assert_eq!(client_epoch.revision, 2);
        assert!(!client_epoch.ready);
        assert!(
            std::iter::from_fn(|| client.poll_event())
                .any(|event| matches!(event, OnlineLobbyEvent::ReturnedToLobby { rematch: false }))
        );

        mirror_member_epoch(&host_control, lobby, client_epoch);
        host.pump(&mut host_platform, 62).unwrap();
        assert!(host.initial_authentication_allowed(client_user, 2));
        assert_eq!(host.status().phase, OnlineLobbyPhase::Lobby);
        assert_eq!(host.status().failure, None);
    }
}
