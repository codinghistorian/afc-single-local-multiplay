//! Native online application runtime and UI-independent screen model.
//!
//! The deterministic simulation never owns this service. On a native Steam
//! build, one [`NativeOnlineRuntime`] owns the sole real Steam platform,
//! [`OnlineLobbyCoordinator`], the Steam gameplay transport factory, and a
//! bounded AFCP control stream on each quarantined socket. Builds without
//! `steam-net` retain the same screen model and fail closed with a localizable
//! unavailable reason.

use core::fmt;
#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
use std::collections::VecDeque;

use crate::headless::HeadlessMatchConfig;
#[cfg(test)]
use crate::match_config::current_compatibility;
#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
use crate::match_config::headless_config_from_manifest;
#[cfg(all(feature = "steam-net", not(target_arch = "wasm32"), not(test)))]
use crate::multiplayer_diagnostics::resolve_diagnostics_root;
#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
use crate::multiplayer_diagnostics::{AuthorityDiagnosticsArchive, SteamPregameTraceDiagnostic};
#[cfg(test)]
use crate::network_codec::encode_packet;
#[cfg(test)]
use crate::network_codec::{WireMessage, decode_packet};
#[cfg(test)]
use crate::network_protocol::StartMessage;
use crate::network_protocol::{DefinitionId, MatchManifest, PeerId, RetryDisposition};
use crate::network_quality::{InputDelayCalibrationSnapshot, NetworkQualitySnapshot};
use crate::online_failure::{
    OnlineFailure, OnlineFailureCode, OnlineFailureSeverity, OnlineRecoveryAction,
};
#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
use crate::online_lobby::OnlinePeerIdentity;
#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
use crate::online_lobby::{
    AuthPeerLease, AuthSignalLeaseStatus, AuthSignalScope, AuthTicketLease, OnlineLobbyConfig,
    OnlineLobbyCoordinator,
};
use crate::online_lobby::{
    OnlineLobbyError, OnlineLobbyEvent, OnlineLobbyRole, OnlineMatchOutcome, OnlineSetupStage,
    OnlineStartBlocker,
};
#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
use crate::online_lobby::{OnlineLobbyPhase, OnlineLobbyStatus};
use crate::online_roster::{
    FirstReleaseOnlinePolicy, OnlineManifestOptions, OnlineRosterMember, OnlineSeatSelection,
};
use crate::reconnect::{AuthenticatedPeer, AuthenticatedUserId};
use crate::remote_online_client::RemoteAuthorityDisconnect;
use crate::simulation::SimTick;
#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
use crate::steam_control::{
    AccountAuthEpoch, ManifestTransactionId, SteamAuthTicketPayload, SteamControlIdentity,
    SteamControlMessage,
};
use crate::steam_platform::{
    AdmissionPurpose, LobbyJoinIntent, MAX_STEAM_AUTH_TICKET_BYTES, MAX_STEAM_LOBBY_MEMBERS,
    RegionCode, SPACEWAR_APP_ID, SteamAppId, SteamClientConfig, SteamInputActionSet,
    SteamInputSnapshot, SteamLobbyId, SteamOverlayRequestStatus, SteamPlatformError, SteamUserId,
};
#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
use crate::steam_platform::{
    AuthTicketHandle, LobbyCreateRequest, LobbyMetadata, LobbyVisibility, SteamBackend,
    SteamPlatform,
};
use crate::steam_transport::{
    AdmittedSteamEndpoint, SteamConnectionId, SteamNetworkReadiness, SteamRelayStatus,
    SteamTransportError,
};
#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
use crate::steam_transport::{
    SteamP2pSession, SteamTransport, SteamTransportCloseReason, SteamTransportConfig,
};

pub const STEAM_APP_ID_ENV: &str = "AFC_STEAM_APP_ID";
pub const STEAM_SPACEWAR_OPT_IN_ENV: &str = "AFC_STEAM_DEV_SPACEWAR_480";
pub const COMPILED_STEAM_APP_ID: Option<&str> = option_env!("AFC_COMPILED_STEAM_APP_ID");
pub const COMPILED_SPACEWAR_OPT_IN: bool = cfg!(feature = "spacewar-dev");
pub const MAX_NATIVE_ONLINE_EVENTS: usize = 128;
pub const MAX_AUTH_SIGNALS_PER_PUMP: usize = 16;
#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
const AUTH_RETRY_DIRECT_ABORT_CODE: u16 = 0xA101;
#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
const AUTH_RETRY_ROSTER_ABORT_CODE: u16 = 0xA102;
#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
const AUTH_REJECT_ROSTER_ABORT_CODE_BASE: u16 = 0xA200;
/// A single Steam user may consume at most one quarter of the bounded
/// pre-game receive budget. Exceeding this quota invalidates only that user's
/// outcomes; it never invalidates another user's signal. Steam's shared
/// ordered queue may still defer a later valid signal until the next pump.
pub const MAX_AUTH_SIGNALS_PER_USER_PER_PUMP: usize =
    MAX_AUTH_SIGNALS_PER_PUMP / MAX_STEAM_LOBBY_MEMBERS;

#[cfg(test)]
const AUTH_SIGNAL_MAGIC: [u8; 4] = *b"AFCA";
#[cfg(test)]
const AUTH_SIGNAL_VERSION: u8 = 3;
#[cfg(test)]
const AUTH_SIGNAL_KIND_HELLO: u8 = 0;
#[cfg(test)]
const AUTH_SIGNAL_KIND_TICKET: u8 = 1;
#[cfg(test)]
const AUTH_SIGNAL_KIND_MANIFEST: u8 = 2;
#[cfg(test)]
const SESSION_HELLO_SIGNAL_BYTES: usize = 32;
#[cfg(test)]
const AUTH_SIGNAL_HEADER_BYTES: usize = 62;
#[cfg(test)]
const MANIFEST_SIGNAL_HEADER_BYTES: usize = 32;
#[cfg(test)]
const MAX_AUTH_SIGNAL_BYTES: usize =
    MANIFEST_SIGNAL_HEADER_BYTES + crate::network_codec::MAX_PACKET_BYTES;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeOnlineUnavailableReason {
    SteamFeatureDisabled,
    UnsupportedPlatform,
    MissingAppId,
    InvalidAppId,
    SpacewarRequiresExplicitOptIn,
    SteamInitializationFailed,
}

impl NativeOnlineUnavailableReason {
    pub const fn diagnostic_code(self) -> u16 {
        match self {
            Self::SteamFeatureDisabled => 101,
            Self::UnsupportedPlatform => 102,
            Self::MissingAppId => 103,
            Self::InvalidAppId => 104,
            Self::SpacewarRequiresExplicitOptIn => 105,
            Self::SteamInitializationFailed => 106,
        }
    }

    pub const fn message_key(self) -> &'static str {
        match self {
            Self::SteamFeatureDisabled => "online.unavailable.steam_feature_disabled",
            Self::UnsupportedPlatform => "online.unavailable.unsupported_platform",
            Self::MissingAppId => "online.unavailable.app_id_missing",
            Self::InvalidAppId => "online.unavailable.app_id_invalid",
            Self::SpacewarRequiresExplicitOptIn => {
                "online.unavailable.spacewar_requires_explicit_opt_in"
            }
            Self::SteamInitializationFailed => "online.unavailable.steam_initialization_failed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeOnlineAvailability {
    Available,
    Unavailable(NativeOnlineUnavailableReason),
}

impl NativeOnlineAvailability {
    pub const fn is_available(self) -> bool {
        matches!(self, Self::Available)
    }

    pub const fn message_key(self) -> &'static str {
        match self {
            Self::Available => "online.available",
            Self::Unavailable(reason) => reason.message_key(),
        }
    }
}

/// Explicit release configuration. There is deliberately no default App ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeSteamReleaseConfig {
    Production { app_id: SteamAppId },
    DevelopmentSpacewar480,
}

impl NativeSteamReleaseConfig {
    pub fn production(app_id: u32) -> Result<Self, NativeOnlineConfigError> {
        let app_id = SteamAppId::new(app_id).map_err(|_| NativeOnlineConfigError::InvalidAppId)?;
        if app_id.get() == SPACEWAR_APP_ID {
            return Err(NativeOnlineConfigError::SpacewarRequiresExplicitOptIn);
        }
        Ok(Self::Production { app_id })
    }

    pub const fn development_spacewar_480() -> Self {
        Self::DevelopmentSpacewar480
    }

    pub fn app_id(self) -> SteamAppId {
        match self {
            Self::Production { app_id } => app_id,
            Self::DevelopmentSpacewar480 => {
                SteamAppId::new(SPACEWAR_APP_ID).expect("Spacewar App ID is non-zero")
            }
        }
    }

    pub fn from_environment() -> Result<Self, NativeOnlineConfigError> {
        Self::from_sources(
            COMPILED_STEAM_APP_ID,
            cfg!(debug_assertions),
            COMPILED_SPACEWAR_OPT_IN,
            |key| std::env::var(key).ok(),
        )
    }

    #[cfg(test)]
    fn from_lookup(
        lookup: impl FnMut(&str) -> Option<String>,
    ) -> Result<Self, NativeOnlineConfigError> {
        Self::from_sources(None, true, false, lookup)
    }

    fn from_sources(
        compiled_raw: Option<&str>,
        development_build: bool,
        compiled_spacewar_opt_in: bool,
        mut lookup: impl FnMut(&str) -> Option<String>,
    ) -> Result<Self, NativeOnlineConfigError> {
        let compiled = compiled_raw.map(parse_app_id).transpose()?;
        let runtime = lookup(STEAM_APP_ID_ENV)
            .as_deref()
            .map(parse_app_id)
            .transpose()?;
        if compiled.is_some() && runtime.is_some() && compiled != runtime {
            return Err(NativeOnlineConfigError::AppIdMismatch);
        }
        if !development_build && compiled.is_none() {
            // A shipping process may validate a redundant runtime value, but
            // it may never select its identity from mutable process state.
            return Err(NativeOnlineConfigError::MissingAppId);
        }
        let app_id = compiled
            .or(if development_build { runtime } else { None })
            .ok_or(NativeOnlineConfigError::MissingAppId)?;
        let runtime_spacewar_opt_in = match lookup(STEAM_SPACEWAR_OPT_IN_ENV).as_deref() {
            None | Some("0") => false,
            Some("1") => true,
            Some(_) => return Err(NativeOnlineConfigError::InvalidSpacewarOptIn),
        };
        let spacewar_opt_in = compiled_spacewar_opt_in || runtime_spacewar_opt_in;
        if app_id.get() == SPACEWAR_APP_ID {
            if !development_build {
                return Err(NativeOnlineConfigError::SpacewarForbiddenInRelease);
            }
            if spacewar_opt_in {
                Ok(Self::DevelopmentSpacewar480)
            } else {
                Err(NativeOnlineConfigError::SpacewarRequiresExplicitOptIn)
            }
        } else if spacewar_opt_in {
            Err(NativeOnlineConfigError::InvalidSpacewarOptIn)
        } else {
            Ok(Self::Production { app_id })
        }
    }

    pub fn steam_client_config(self) -> SteamClientConfig {
        match self {
            Self::Production { app_id } => SteamClientConfig::production(app_id),
            Self::DevelopmentSpacewar480 => SteamClientConfig::development(
                SteamAppId::new(SPACEWAR_APP_ID).expect("Spacewar App ID is non-zero"),
                true,
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeOnlineConfigError {
    MissingAppId,
    InvalidAppId,
    AppIdMismatch,
    InvalidSpacewarOptIn,
    SpacewarRequiresExplicitOptIn,
    SpacewarForbiddenInRelease,
}

impl NativeOnlineConfigError {
    pub const fn unavailable_reason(self) -> NativeOnlineUnavailableReason {
        match self {
            Self::MissingAppId => NativeOnlineUnavailableReason::MissingAppId,
            Self::SpacewarRequiresExplicitOptIn => {
                NativeOnlineUnavailableReason::SpacewarRequiresExplicitOptIn
            }
            Self::InvalidAppId
            | Self::AppIdMismatch
            | Self::InvalidSpacewarOptIn
            | Self::SpacewarForbiddenInRelease => NativeOnlineUnavailableReason::InvalidAppId,
        }
    }
}

fn parse_app_id(raw: &str) -> Result<SteamAppId, NativeOnlineConfigError> {
    if raw.is_empty() || raw.len() > 10 || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(NativeOnlineConfigError::InvalidAppId);
    }
    let parsed = raw
        .parse::<u32>()
        .map_err(|_| NativeOnlineConfigError::InvalidAppId)?;
    SteamAppId::new(parsed).map_err(|_| NativeOnlineConfigError::InvalidAppId)
}

#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
const fn restart_app_id_for_profile(
    config: NativeSteamReleaseConfig,
    release_build: bool,
) -> Option<SteamAppId> {
    if !release_build {
        return None;
    }
    match config {
        NativeSteamReleaseConfig::Production { app_id } => Some(app_id),
        NativeSteamReleaseConfig::DevelopmentSpacewar480 => None,
    }
}

/// Performs Valve's release-only relaunch check before Bevy or Steam client
/// initialization. A `true` result means Steam accepted the relaunch request;
/// the caller must return from `main` immediately.
pub fn restart_native_steam_release_if_necessary() -> bool {
    #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
    {
        let Ok(config) = NativeSteamReleaseConfig::from_environment() else {
            return false;
        };
        let Some(app_id) = restart_app_id_for_profile(config, !cfg!(debug_assertions)) else {
            return false;
        };
        return steamworks::restart_app_if_necessary(steamworks::AppId(app_id.get()));
    }

    #[cfg(not(all(feature = "steam-net", not(target_arch = "wasm32"))))]
    false
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeOnlineCreateRequest {
    pub visibility: NativeOnlineVisibility,
    pub maximum_steam_peers: u8,
    pub region: RegionCode,
    pub rules: DefinitionId,
    pub arena: DefinitionId,
    pub seat_capacity: u8,
    pub local_declaration: OnlineRosterMember,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeOnlineVisibility {
    Private,
    FriendsOnly,
}

impl NativeOnlineVisibility {
    #[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
    const fn steam(self) -> LobbyVisibility {
        match self {
            Self::Private => LobbyVisibility::Private,
            Self::FriendsOnly => LobbyVisibility::FriendsOnly,
        }
    }
}

pub enum NativeOnlineCommand {
    Create(NativeOnlineCreateRequest),
    Join {
        intent: LobbyJoinIntent,
        local_declaration: OnlineRosterMember,
    },
    DeclineJoin,
    SetLocalDeclaration(OnlineRosterMember),
    SetReady(bool),
    /// Retries the single attributed, recoverable Steam setup failure while
    /// preserving the lobby and unrelated authority-star links.
    RetrySteamSetup,
    CommitManifest {
        options: OnlineManifestOptions,
        current_tick: SimTick,
    },
    AcceptManifest(HeadlessMatchConfig),
    ContentLoaded,
    InitialSyncComplete,
    BeginCountdown(SimTick),
    MarkFighting(SimTick),
    BeginResultConfirmation,
    ConfirmResult,
    /// Internal application-to-coordinator handoff for an authenticated,
    /// match-bound authority terminal observed by the remote worker.
    ApplyAuthorityDisconnect(RemoteAuthorityDisconnect),
    /// Irreversibly fences new transport, ticket, AFCP-control, and
    /// gameplay-endpoint admission for the current match while established
    /// connections remain available for bounded terminal/ACK drain.
    QuiesceAdmission,
    /// Internal listen-authority handoff after the application resolves the
    /// authority-worker generation to its exact admitted Steam connection.
    MarkAuthorityTerminalDrained {
        user: SteamUserId,
        peer_id: PeerId,
        connection: SteamConnectionId,
        retry: Option<RetryDisposition>,
    },
    Rematch,
    ReturnToLobby,
    LeaveOnline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeOnlineScreen {
    Unavailable,
    OnlineMenu,
    JoinPrompt,
    CreatingLobby,
    JoiningLobby,
    Lobby,
    Connecting,
    Authenticating,
    ManifestAgreement,
    Loading,
    Ready,
    Countdown,
    Fighting,
    Reconnecting,
    ConfirmingResult,
    Results,
    ReturningToLobby,
    Error,
}

impl NativeOnlineScreen {
    pub const fn message_key(self) -> &'static str {
        match self {
            Self::Unavailable => "online.screen.unavailable",
            Self::OnlineMenu => "online.screen.menu",
            Self::JoinPrompt => "online.screen.join_prompt",
            Self::CreatingLobby => "online.screen.creating_lobby",
            Self::JoiningLobby => "online.screen.joining_lobby",
            Self::Lobby => "online.screen.lobby",
            Self::Connecting => "online.screen.connecting",
            Self::Authenticating => "online.screen.authenticating",
            Self::ManifestAgreement => "online.screen.manifest_agreement",
            Self::Loading => "online.screen.loading",
            Self::Ready => "online.screen.ready",
            Self::Countdown => "online.screen.countdown",
            Self::Fighting => "online.screen.fighting",
            Self::Reconnecting => "online.screen.reconnecting",
            Self::ConfirmingResult => "online.screen.confirming_result",
            Self::Results => "online.screen.results",
            Self::ReturningToLobby => "online.screen.returning_to_lobby",
            Self::Error => "online.screen.error",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NativeOnlineActions {
    pub create_private: bool,
    pub create_friends: bool,
    pub accept_join: bool,
    pub decline_join: bool,
    pub edit_couch_seats_and_loadouts: bool,
    pub toggle_ready: bool,
    pub invite_friends: bool,
    pub leave: bool,
    pub rematch: bool,
    pub return_to_lobby: bool,
    pub return_to_menu: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeOnlineViewModel {
    pub availability: NativeOnlineAvailability,
    pub screen: NativeOnlineScreen,
    pub actions: NativeOnlineActions,
    pub lobby: Option<SteamLobbyId>,
    pub role: Option<OnlineLobbyRole>,
    pub lobby_members: u8,
    pub total_seats: u8,
    pub local_seats: u8,
    pub local_ready: bool,
    pub all_members_ready: bool,
    pub connected_remote_peers: u8,
    pub secure_remote_peers: u8,
    pub required_remote_peers: u8,
    pub verified_remote_accounts: u8,
    pub required_remote_accounts: u8,
    pub steam_network_readiness: SteamNetworkReadiness,
    pub setup_stage: OnlineSetupStage,
    pub start_blocker: Option<OnlineStartBlocker>,
    pub network_quality: NetworkQualitySnapshot,
    pub input_delay_calibration: InputDelayCalibrationSnapshot,
    pub relay_status: SteamRelayStatus,
    pub countdown_start_tick: Option<SimTick>,
    pub outcome: Option<OnlineMatchOutcome>,
    pub failure: Option<OnlineFailure>,
}

impl NativeOnlineViewModel {
    pub const fn screen_message_key(self) -> &'static str {
        self.screen.message_key()
    }

    pub const fn availability_message_key(self) -> &'static str {
        self.availability.message_key()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommittedAuthenticatedRoster {
    entries: [Option<AuthenticatedPeer>; MAX_STEAM_LOBBY_MEMBERS],
    len: u8,
}

impl CommittedAuthenticatedRoster {
    pub const fn len(self) -> usize {
        self.len as usize
    }

    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = AuthenticatedPeer> + '_ {
        self.entries[..self.len()]
            .iter()
            .map(|entry| entry.expect("committed roster prefix is packed"))
    }

    #[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
    fn push(&mut self, peer: AuthenticatedPeer) -> Result<(), NativeOnlineRuntimeError> {
        if self
            .iter()
            .any(|entry| entry.peer_id == peer.peer_id || entry.user_id == peer.user_id)
        {
            return Err(NativeOnlineRuntimeError::InvalidAuthenticatedRoster);
        }
        let index = self.len();
        let Some(slot) = self.entries.get_mut(index) else {
            return Err(NativeOnlineRuntimeError::Capacity);
        };
        *slot = Some(peer);
        self.len += 1;
        Ok(())
    }
}

/// Gameplay handoff keeps the authenticated protocol peer and endpoint atomic.
pub struct NativeOnlineEndpoint {
    pub peer_id: PeerId,
    pub reconnect: bool,
    pub admitted: AdmittedSteamEndpoint,
}

#[derive(Debug)]
pub enum NativeOnlineRuntimeError {
    Unavailable(NativeOnlineUnavailableReason),
    Configuration(NativeOnlineConfigError),
    Steam(SteamPlatformError),
    Lobby(OnlineLobbyError),
    Transport(SteamTransportError),
    Signal(AuthSignalError),
    Capacity,
    TimeRegression,
    InvalidAuthenticatedRoster,
    EndpointIdentityMismatch,
}

impl fmt::Display for NativeOnlineRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "native online runtime failed: {self:?}")
    }
}

impl std::error::Error for NativeOnlineRuntimeError {}

impl From<OnlineLobbyError> for NativeOnlineRuntimeError {
    fn from(value: OnlineLobbyError) -> Self {
        Self::Lobby(value)
    }
}

impl From<SteamPlatformError> for NativeOnlineRuntimeError {
    fn from(value: SteamPlatformError) -> Self {
        Self::Steam(value)
    }
}

impl From<SteamTransportError> for NativeOnlineRuntimeError {
    fn from(value: SteamTransportError) -> Self {
        Self::Transport(value)
    }
}

impl From<AuthSignalError> for NativeOnlineRuntimeError {
    fn from(value: AuthSignalError) -> Self {
        Self::Signal(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthSignalError {
    EmptyTicket,
    TicketTooLarge,
    InvalidEnvelope,
    InvalidIdentity,
    WrongLobby,
    WrongRecipient,
    SenderMismatch,
    UnexpectedPurpose,
    PeerNotInLobby,
    TransportFailed,
    ReceiveBudgetExceeded,
    UnexpectedManifestSender,
    ConflictingManifest,
    SessionAcceptanceFailed,
    SessionLocalOffline,
    SessionRelayUnavailable,
    SessionNetworkConfigUnavailable,
    SessionRightsDenied,
    SessionRemoteTimeout,
    SessionCryptFailure,
    SessionProtocolMismatch,
    SessionInternalFailure,
    SessionSteamConnectivity,
    SessionRendezvousFailed,
    SessionNatFirewall,
    SessionPeerRejected,
    SessionUnknownFailure,
}

/// Stable local diagnostics for the pre-game Steam signaling boundary.
/// These values are never serialized and shipping UI keeps them hidden.
#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
const fn auth_signal_detail_code(error: AuthSignalError) -> u16 {
    match error {
        AuthSignalError::EmptyTicket => 201,
        AuthSignalError::TicketTooLarge => 202,
        AuthSignalError::InvalidEnvelope => 203,
        AuthSignalError::InvalidIdentity => 204,
        AuthSignalError::WrongLobby => 205,
        AuthSignalError::WrongRecipient => 206,
        AuthSignalError::SenderMismatch => 207,
        AuthSignalError::UnexpectedPurpose => 208,
        AuthSignalError::PeerNotInLobby => 209,
        AuthSignalError::TransportFailed => 210,
        AuthSignalError::ReceiveBudgetExceeded => 211,
        AuthSignalError::UnexpectedManifestSender => 212,
        AuthSignalError::ConflictingManifest => 213,
        AuthSignalError::SessionAcceptanceFailed => 214,
        AuthSignalError::SessionLocalOffline => 215,
        AuthSignalError::SessionRelayUnavailable => 216,
        AuthSignalError::SessionNetworkConfigUnavailable => 217,
        AuthSignalError::SessionRightsDenied => 218,
        AuthSignalError::SessionRemoteTimeout => 219,
        AuthSignalError::SessionCryptFailure => 229,
        AuthSignalError::SessionProtocolMismatch => 230,
        AuthSignalError::SessionInternalFailure => 231,
        AuthSignalError::SessionSteamConnectivity => 232,
        AuthSignalError::SessionRendezvousFailed => 233,
        AuthSignalError::SessionNatFirewall => 234,
        AuthSignalError::SessionPeerRejected => 235,
        AuthSignalError::SessionUnknownFailure => 236,
    }
}

#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
const fn auth_signal_error_is_transport(error: AuthSignalError) -> bool {
    matches!(
        error,
        AuthSignalError::TransportFailed
            | AuthSignalError::SessionAcceptanceFailed
            | AuthSignalError::SessionLocalOffline
            | AuthSignalError::SessionRelayUnavailable
            | AuthSignalError::SessionNetworkConfigUnavailable
            | AuthSignalError::SessionRightsDenied
            | AuthSignalError::SessionRemoteTimeout
            | AuthSignalError::SessionCryptFailure
            | AuthSignalError::SessionProtocolMismatch
            | AuthSignalError::SessionInternalFailure
            | AuthSignalError::SessionSteamConnectivity
            | AuthSignalError::SessionRendezvousFailed
            | AuthSignalError::SessionNatFirewall
            | AuthSignalError::SessionPeerRejected
            | AuthSignalError::SessionUnknownFailure
    )
}

/// Legacy pre-AFCP codec fixture retained only by migration/hostility unit tests.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AuthSessionHelloSignal {
    lobby: SteamLobbyId,
    sender: SteamUserId,
    recipient: SteamUserId,
}

#[cfg(test)]
impl AuthSessionHelloSignal {
    const fn new(lobby: SteamLobbyId, sender: SteamUserId, recipient: SteamUserId) -> Self {
        Self {
            lobby,
            sender,
            recipient,
        }
    }

    fn encode(self) -> EncodedPreGameSignal {
        let mut encoded = EncodedPreGameSignal {
            bytes: [0; MAX_AUTH_SIGNAL_BYTES],
            len: SESSION_HELLO_SIGNAL_BYTES,
        };
        encoded.bytes[0..4].copy_from_slice(&AUTH_SIGNAL_MAGIC);
        encoded.bytes[4] = AUTH_SIGNAL_VERSION;
        encoded.bytes[5] = AUTH_SIGNAL_KIND_HELLO;
        encoded.bytes[6..8].fill(0);
        encoded.bytes[8..16].copy_from_slice(&self.lobby.get().to_le_bytes());
        encoded.bytes[16..24].copy_from_slice(&self.sender.get().to_le_bytes());
        encoded.bytes[24..32].copy_from_slice(&self.recipient.get().to_le_bytes());
        encoded
    }

    fn decode(bytes: &[u8]) -> Result<Self, AuthSignalError> {
        if bytes.len() != SESSION_HELLO_SIGNAL_BYTES
            || bytes[0..4] != AUTH_SIGNAL_MAGIC
            || bytes[4] != AUTH_SIGNAL_VERSION
            || bytes[5] != AUTH_SIGNAL_KIND_HELLO
            || bytes[6] != 0
            || bytes[7] != 0
        {
            return Err(AuthSignalError::InvalidEnvelope);
        }
        let lobby =
            SteamLobbyId::new(read_u64(bytes, 8)?).map_err(|_| AuthSignalError::InvalidIdentity)?;
        let sender =
            SteamUserId::new(read_u64(bytes, 16)?).map_err(|_| AuthSignalError::InvalidIdentity)?;
        let recipient =
            SteamUserId::new(read_u64(bytes, 24)?).map_err(|_| AuthSignalError::InvalidIdentity)?;
        Ok(Self::new(lobby, sender, recipient))
    }
}

/// Secret-bearing fixed envelope. Debug output intentionally redacts the bytes.
pub struct AuthTicketSignal {
    pub lobby: SteamLobbyId,
    pub sender: SteamUserId,
    pub recipient: SteamUserId,
    pub sender_peer_id: PeerId,
    pub purpose: AdmissionPurpose,
    pub owner_revision: u16,
    pub sender_revision: u16,
    pub match_id: Option<crate::network_protocol::MatchId>,
    ticket_len: u16,
    ticket: [u8; MAX_STEAM_AUTH_TICKET_BYTES],
}

impl AuthTicketSignal {
    pub fn new(
        lobby: SteamLobbyId,
        sender: SteamUserId,
        recipient: SteamUserId,
        sender_peer_id: PeerId,
        purpose: AdmissionPurpose,
        owner_revision: u16,
        sender_revision: u16,
        match_id: Option<crate::network_protocol::MatchId>,
        ticket: &[u8],
    ) -> Result<Self, AuthSignalError> {
        if ticket.is_empty() {
            return Err(AuthSignalError::EmptyTicket);
        }
        if ticket.len() > MAX_STEAM_AUTH_TICKET_BYTES {
            return Err(AuthSignalError::TicketTooLarge);
        }
        sender_peer_id
            .validate()
            .map_err(|_| AuthSignalError::InvalidIdentity)?;
        if owner_revision == 0
            || sender_revision == 0
            || matches!(
                (purpose, match_id),
                (AdmissionPurpose::Initial, Some(_)) | (AdmissionPurpose::Reconnect, None)
            )
        {
            return Err(AuthSignalError::InvalidEnvelope);
        }
        if let Some(match_id) = match_id {
            match_id
                .validate()
                .map_err(|_| AuthSignalError::InvalidEnvelope)?;
        }
        let mut retained = [0; MAX_STEAM_AUTH_TICKET_BYTES];
        retained[..ticket.len()].copy_from_slice(ticket);
        Ok(Self {
            lobby,
            sender,
            recipient,
            sender_peer_id,
            purpose,
            owner_revision,
            sender_revision,
            match_id,
            ticket_len: ticket.len() as u16,
            ticket: retained,
        })
    }

    pub fn ticket(&self) -> &[u8] {
        &self.ticket[..usize::from(self.ticket_len)]
    }

    #[cfg(test)]
    fn encode(&self) -> EncodedPreGameSignal {
        let mut encoded = EncodedPreGameSignal {
            bytes: [0; MAX_AUTH_SIGNAL_BYTES],
            len: AUTH_SIGNAL_HEADER_BYTES + self.ticket().len(),
        };
        let bytes = &mut encoded.bytes;
        bytes[0..4].copy_from_slice(&AUTH_SIGNAL_MAGIC);
        bytes[4] = AUTH_SIGNAL_VERSION;
        bytes[5] = AUTH_SIGNAL_KIND_TICKET;
        bytes[6] = match self.purpose {
            AdmissionPurpose::Initial => 0,
            AdmissionPurpose::Reconnect => 1,
        };
        bytes[7] = 0;
        bytes[8..16].copy_from_slice(&self.lobby.get().to_le_bytes());
        bytes[16..24].copy_from_slice(&self.sender.get().to_le_bytes());
        bytes[24..32].copy_from_slice(&self.recipient.get().to_le_bytes());
        bytes[32..40].copy_from_slice(&self.sender_peer_id.get().to_le_bytes());
        bytes[40..42].copy_from_slice(&self.owner_revision.to_le_bytes());
        bytes[42..44].copy_from_slice(&self.sender_revision.to_le_bytes());
        if let Some(match_id) = self.match_id {
            bytes[44..60].copy_from_slice(match_id.as_bytes());
        }
        bytes[60..62].copy_from_slice(&self.ticket_len.to_le_bytes());
        bytes[AUTH_SIGNAL_HEADER_BYTES..encoded.len].copy_from_slice(self.ticket());
        encoded
    }

    #[cfg(test)]
    pub fn decode(bytes: &[u8]) -> Result<Self, AuthSignalError> {
        if bytes.len() < AUTH_SIGNAL_HEADER_BYTES
            || bytes.len() > MAX_AUTH_SIGNAL_BYTES
            || bytes[0..4] != AUTH_SIGNAL_MAGIC
            || bytes[4] != AUTH_SIGNAL_VERSION
            || bytes[5] != AUTH_SIGNAL_KIND_TICKET
            || bytes[7] != 0
        {
            return Err(AuthSignalError::InvalidEnvelope);
        }
        let purpose = match bytes[6] {
            0 => AdmissionPurpose::Initial,
            1 => AdmissionPurpose::Reconnect,
            _ => return Err(AuthSignalError::InvalidEnvelope),
        };
        let lobby =
            SteamLobbyId::new(read_u64(bytes, 8)?).map_err(|_| AuthSignalError::InvalidIdentity)?;
        let sender =
            SteamUserId::new(read_u64(bytes, 16)?).map_err(|_| AuthSignalError::InvalidIdentity)?;
        let recipient =
            SteamUserId::new(read_u64(bytes, 24)?).map_err(|_| AuthSignalError::InvalidIdentity)?;
        let sender_peer_id =
            PeerId::new(read_u64(bytes, 32)?).map_err(|_| AuthSignalError::InvalidIdentity)?;
        let owner_revision = u16::from_le_bytes([bytes[40], bytes[41]]);
        let sender_revision = u16::from_le_bytes([bytes[42], bytes[43]]);
        let mut match_id_bytes = [0_u8; 16];
        match_id_bytes.copy_from_slice(&bytes[44..60]);
        let match_id = if match_id_bytes.iter().all(|byte| *byte == 0) {
            None
        } else {
            Some(
                crate::network_protocol::MatchId::new(match_id_bytes)
                    .map_err(|_| AuthSignalError::InvalidEnvelope)?,
            )
        };
        let ticket_len = usize::from(u16::from_le_bytes([bytes[60], bytes[61]]));
        if ticket_len == 0
            || ticket_len > MAX_STEAM_AUTH_TICKET_BYTES
            || bytes.len() != AUTH_SIGNAL_HEADER_BYTES + ticket_len
        {
            return Err(AuthSignalError::InvalidEnvelope);
        }
        Self::new(
            lobby,
            sender,
            recipient,
            sender_peer_id,
            purpose,
            owner_revision,
            sender_revision,
            match_id,
            &bytes[AUTH_SIGNAL_HEADER_BYTES..],
        )
    }
}

impl fmt::Debug for AuthTicketSignal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthTicketSignal")
            .field("lobby", &self.lobby)
            .field("sender", &self.sender)
            .field("recipient", &self.recipient)
            .field("sender_peer_id", &self.sender_peer_id)
            .field("purpose", &self.purpose)
            .field("owner_revision", &self.owner_revision)
            .field("sender_revision", &self.sender_revision)
            .field("match_id", &self.match_id)
            .field("ticket_len", &self.ticket_len)
            .field("ticket", &"<redacted>")
            .finish()
    }
}

impl Drop for AuthTicketSignal {
    fn drop(&mut self) {
        zeroize_auth_signal_bytes(&mut self.ticket);
        self.ticket_len = 0;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootstrapManifestSignal {
    pub lobby: SteamLobbyId,
    pub sender: SteamUserId,
    pub recipient: SteamUserId,
    pub manifest: MatchManifest,
}

impl BootstrapManifestSignal {
    pub fn new(
        lobby: SteamLobbyId,
        sender: SteamUserId,
        recipient: SteamUserId,
        manifest: MatchManifest,
    ) -> Result<Self, AuthSignalError> {
        manifest
            .validate()
            .map_err(|_| AuthSignalError::InvalidEnvelope)?;
        if !FirstReleaseOnlinePolicy::accepts_manifest(&manifest) {
            return Err(AuthSignalError::InvalidEnvelope);
        }
        Ok(Self {
            lobby,
            sender,
            recipient,
            manifest,
        })
    }

    #[cfg(test)]
    fn encode(&self) -> Result<EncodedPreGameSignal, AuthSignalError> {
        let mut packet = [0; crate::network_codec::MAX_PACKET_BYTES];
        let packet_len = encode_packet(
            self.manifest.compatibility.protocol,
            &WireMessage::Start(StartMessage::Manifest(self.manifest)),
            &mut packet,
        )
        .map_err(|_| AuthSignalError::InvalidEnvelope)?;
        let mut encoded = EncodedPreGameSignal {
            bytes: [0; MAX_AUTH_SIGNAL_BYTES],
            len: MANIFEST_SIGNAL_HEADER_BYTES + packet_len,
        };
        encoded.bytes[0..4].copy_from_slice(&AUTH_SIGNAL_MAGIC);
        encoded.bytes[4] = AUTH_SIGNAL_VERSION;
        encoded.bytes[5] = AUTH_SIGNAL_KIND_MANIFEST;
        encoded.bytes[6..8].fill(0);
        encoded.bytes[8..16].copy_from_slice(&self.lobby.get().to_le_bytes());
        encoded.bytes[16..24].copy_from_slice(&self.sender.get().to_le_bytes());
        encoded.bytes[24..32].copy_from_slice(&self.recipient.get().to_le_bytes());
        encoded.bytes[MANIFEST_SIGNAL_HEADER_BYTES..encoded.len]
            .copy_from_slice(&packet[..packet_len]);
        Ok(encoded)
    }

    #[cfg(test)]
    pub fn decode(bytes: &[u8]) -> Result<Self, AuthSignalError> {
        if bytes.len() <= MANIFEST_SIGNAL_HEADER_BYTES
            || bytes.len() > MAX_AUTH_SIGNAL_BYTES
            || bytes[0..4] != AUTH_SIGNAL_MAGIC
            || bytes[4] != AUTH_SIGNAL_VERSION
            || bytes[5] != AUTH_SIGNAL_KIND_MANIFEST
            || bytes[6] != 0
            || bytes[7] != 0
        {
            return Err(AuthSignalError::InvalidEnvelope);
        }
        let lobby =
            SteamLobbyId::new(read_u64(bytes, 8)?).map_err(|_| AuthSignalError::InvalidIdentity)?;
        let sender =
            SteamUserId::new(read_u64(bytes, 16)?).map_err(|_| AuthSignalError::InvalidIdentity)?;
        let recipient =
            SteamUserId::new(read_u64(bytes, 24)?).map_err(|_| AuthSignalError::InvalidIdentity)?;
        let decoded = decode_packet(
            &bytes[MANIFEST_SIGNAL_HEADER_BYTES..],
            &current_compatibility(),
        )
        .map_err(|_| AuthSignalError::InvalidEnvelope)?;
        let WireMessage::Start(StartMessage::Manifest(manifest)) = decoded.message else {
            return Err(AuthSignalError::InvalidEnvelope);
        };
        Self::new(lobby, sender, recipient, manifest)
    }
}

#[cfg(test)]
enum PreGameSignal {
    Hello(AuthSessionHelloSignal),
    Ticket(AuthTicketSignal),
    Manifest(BootstrapManifestSignal),
}

#[cfg(test)]
enum AuthSignalIngress {
    Accepted {
        source: SteamUserId,
        signal: PreGameSignal,
    },
    Rejected {
        source: SteamUserId,
        error: AuthSignalError,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(test)]
enum ManifestIngress {
    Apply,
    Stage,
    ExactDuplicate,
}

#[cfg(test)]
fn classify_manifest_ingress(
    accepted: Option<MatchManifest>,
    pending: Option<BootstrapManifestSignal>,
    phase: OnlineLobbyPhase,
    incoming: BootstrapManifestSignal,
) -> Result<ManifestIngress, AuthSignalError> {
    if let Some(accepted) = accepted {
        return if accepted == incoming.manifest {
            Ok(ManifestIngress::ExactDuplicate)
        } else {
            Err(AuthSignalError::ConflictingManifest)
        };
    }
    if let Some(pending) = pending {
        return if pending == incoming {
            Ok(ManifestIngress::ExactDuplicate)
        } else {
            Err(AuthSignalError::ConflictingManifest)
        };
    }
    match phase {
        OnlineLobbyPhase::ManifestAgreement => Ok(ManifestIngress::Apply),
        OnlineLobbyPhase::Connecting | OnlineLobbyPhase::Authenticating => {
            Ok(ManifestIngress::Stage)
        }
        _ => Err(AuthSignalError::UnexpectedPurpose),
    }
}

#[cfg(test)]
impl PreGameSignal {
    fn sender(&self) -> SteamUserId {
        match self {
            Self::Hello(signal) => signal.sender,
            Self::Ticket(signal) => signal.sender,
            Self::Manifest(signal) => signal.sender,
        }
    }
}

#[cfg(test)]
fn decode_pre_game_signal(bytes: &[u8]) -> Result<PreGameSignal, AuthSignalError> {
    match bytes.get(5).copied() {
        Some(AUTH_SIGNAL_KIND_HELLO) => {
            Ok(PreGameSignal::Hello(AuthSessionHelloSignal::decode(bytes)?))
        }
        Some(AUTH_SIGNAL_KIND_TICKET) => {
            Ok(PreGameSignal::Ticket(AuthTicketSignal::decode(bytes)?))
        }
        Some(AUTH_SIGNAL_KIND_MANIFEST) => Ok(PreGameSignal::Manifest(
            BootstrapManifestSignal::decode(bytes)?,
        )),
        _ => Err(AuthSignalError::InvalidEnvelope),
    }
}

#[cfg(test)]
fn decode_bounded_auth_signal_batch<'a>(
    messages: impl IntoIterator<Item = (SteamUserId, &'a [u8])>,
) -> Vec<AuthSignalIngress> {
    let mut per_user: [Option<(SteamUserId, u8)>; MAX_STEAM_LOBBY_MEMBERS] =
        [None; MAX_STEAM_LOBBY_MEMBERS];
    let mut rejected_users: [Option<SteamUserId>; MAX_STEAM_LOBBY_MEMBERS] =
        [None; MAX_STEAM_LOBBY_MEMBERS];
    let mut outcomes = Vec::with_capacity(MAX_AUTH_SIGNALS_PER_PUMP + 1);

    for (source, bytes) in messages.into_iter().take(MAX_AUTH_SIGNALS_PER_PUMP + 1) {
        if rejected_users.contains(&Some(source)) {
            continue;
        }

        let count = if let Some((_, count)) = per_user
            .iter_mut()
            .flatten()
            .find(|(user, _)| *user == source)
        {
            *count = count.saturating_add(1);
            usize::from(*count)
        } else if let Some(slot) = per_user.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some((source, 1_u8));
            1
        } else {
            MAX_AUTH_SIGNALS_PER_USER_PER_PUMP + 1
        };

        let decoded = if count > MAX_AUTH_SIGNALS_PER_USER_PER_PUMP {
            Err(AuthSignalError::ReceiveBudgetExceeded)
        } else {
            decode_pre_game_signal(bytes).and_then(|signal| {
                if signal.sender() == source {
                    Ok(signal)
                } else {
                    Err(AuthSignalError::SenderMismatch)
                }
            })
        };

        match decoded {
            Ok(signal) => outcomes.push(AuthSignalIngress::Accepted { source, signal }),
            Err(error) => {
                // A later over-limit or malformed message invalidates every
                // signal from that source in this batch. Other users remain
                // independently processable.
                outcomes.retain(|outcome| {
                    !matches!(
                        outcome,
                        AuthSignalIngress::Accepted {
                            source: accepted_source,
                            ..
                        } if *accepted_source == source
                    )
                });
                if let Some(slot) = rejected_users.iter_mut().find(|slot| slot.is_none()) {
                    *slot = Some(source);
                }
                outcomes.push(AuthSignalIngress::Rejected { source, error });
            }
        }
    }

    outcomes
}

#[cfg(test)]
struct EncodedPreGameSignal {
    bytes: [u8; MAX_AUTH_SIGNAL_BYTES],
    len: usize,
}

#[cfg(test)]
impl EncodedPreGameSignal {
    fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

#[cfg(test)]
impl Drop for EncodedPreGameSignal {
    fn drop(&mut self) {
        zeroize_auth_signal_bytes(&mut self.bytes);
        self.len = 0;
    }
}

fn zeroize_auth_signal_bytes(bytes: &mut [u8]) {
    bytes.fill(0);
    // Prevent the secret overwrite from becoming a dead store immediately
    // before the fixed envelope is released.
    std::hint::black_box(bytes);
}

#[cfg(test)]
fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, AuthSignalError> {
    let slice = bytes
        .get(offset..offset + 8)
        .ok_or(AuthSignalError::InvalidEnvelope)?;
    let mut retained = [0; 8];
    retained.copy_from_slice(slice);
    Ok(u64::from_le_bytes(retained))
}

/// Generation-aware rejection gate used by the native Steam runtime.
///
/// `Some(None)` is a current pre-attach/local mapping, `Some(Some(id))` is an
/// attached physical generation, and outer `None` means no mapping exists.
fn authentication_rejection_targets_mapping(
    mapping_connection: Option<Option<SteamConnectionId>>,
    rejected_connection: Option<SteamConnectionId>,
) -> bool {
    match mapping_connection {
        Some(active_connection) => active_connection == rejected_connection,
        None => rejected_connection.is_none(),
    }
}

/// Application-owned runtime. Steam types only exist in the feature-gated
/// inner object, so menus compile unchanged in default and web builds.
pub struct NativeOnlineRuntime {
    availability: NativeOnlineAvailability,
    startup_failure: Option<OnlineFailure>,
    #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
    inner: Option<RealNativeOnlineRuntime>,
}

impl Default for NativeOnlineRuntime {
    fn default() -> Self {
        Self::from_process_environment(0)
    }
}

impl NativeOnlineRuntime {
    pub fn from_process_environment(now_ms: u64) -> Self {
        #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
        {
            match NativeSteamReleaseConfig::from_environment() {
                Ok(release) => match RealNativeOnlineRuntime::initialize(
                    release,
                    OnlineLobbyConfig::default(),
                    now_ms,
                ) {
                    Ok(inner) => Self {
                        availability: NativeOnlineAvailability::Available,
                        startup_failure: None,
                        inner: Some(inner),
                    },
                    Err(_) => {
                        Self::unavailable(NativeOnlineUnavailableReason::SteamInitializationFailed)
                    }
                },
                Err(error) => Self::unavailable(error.unavailable_reason()),
            }
        }

        #[cfg(all(not(feature = "steam-net"), not(target_arch = "wasm32")))]
        {
            let _ = now_ms;
            Self::unavailable(NativeOnlineUnavailableReason::SteamFeatureDisabled)
        }

        #[cfg(target_arch = "wasm32")]
        {
            let _ = now_ms;
            Self::unavailable(NativeOnlineUnavailableReason::UnsupportedPlatform)
        }
    }

    fn unavailable(reason: NativeOnlineUnavailableReason) -> Self {
        Self {
            availability: NativeOnlineAvailability::Unavailable(reason),
            startup_failure: Some(OnlineFailure {
                code: OnlineFailureCode::SteamUnavailable,
                severity: OnlineFailureSeverity::Notice,
                recovery: OnlineRecoveryAction::DisableOnline,
                detail_code: reason.diagnostic_code(),
            }),
            #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
            inner: None,
        }
    }

    pub const fn availability(&self) -> NativeOnlineAvailability {
        self.availability
    }

    /// Latest action-level Steam Input values. Default and web builds expose a
    /// stable empty snapshot and do not link the Steamworks binding.
    pub fn steam_input_snapshot(&self) -> SteamInputSnapshot {
        #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
        if let Some(inner) = &self.inner {
            return inner.steam_input_snapshot();
        }
        SteamInputSnapshot::default()
    }

    pub fn is_overlay_active(&self) -> bool {
        #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
        if let Some(inner) = &self.inner {
            return inner.is_overlay_active();
        }
        false
    }

    pub fn set_steam_input_action_set(
        &mut self,
        action_set: SteamInputActionSet,
    ) -> Result<(), NativeOnlineRuntimeError> {
        #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
        if let Some(inner) = &mut self.inner {
            return inner
                .set_steam_input_action_set(action_set)
                .map_err(Into::into);
        }
        let _ = action_set;
        Ok(())
    }

    pub fn show_steam_input_binding_panel(
        &mut self,
        local_ordinal: usize,
    ) -> Result<SteamOverlayRequestStatus, NativeOnlineRuntimeError> {
        #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
        if let Some(inner) = &mut self.inner {
            return inner
                .show_steam_input_binding_panel(local_ordinal)
                .map_err(Into::into);
        }
        let _ = local_ordinal;
        Ok(SteamOverlayRequestStatus::Unavailable)
    }

    pub fn open_invite_overlay(
        &mut self,
    ) -> Result<SteamOverlayRequestStatus, NativeOnlineRuntimeError> {
        #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
        if let Some(inner) = &mut self.inner {
            return inner.open_invite_overlay().map_err(Into::into);
        }
        Ok(SteamOverlayRequestStatus::Unavailable)
    }

    pub fn view_model(&self) -> NativeOnlineViewModel {
        #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
        if let Some(inner) = &self.inner {
            return inner.view_model();
        }
        unavailable_view(self.availability, self.startup_failure)
    }

    pub fn pump(&mut self, now_ms: u64) -> Result<(), NativeOnlineRuntimeError> {
        #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
        if let Some(inner) = &mut self.inner {
            let result = inner.pump(now_ms);
            if let Err(error) = &result {
                inner.runtime_failure = Some(runtime_failure(error));
            }
            return result;
        }
        let _ = now_ms;
        Err(self.unavailable_error())
    }

    pub fn execute(
        &mut self,
        command: NativeOnlineCommand,
        now_ms: u64,
    ) -> Result<(), NativeOnlineRuntimeError> {
        #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
        if let Some(inner) = &mut self.inner {
            let result = inner.execute(command, now_ms);
            if let Err(error) = &result {
                inner.runtime_failure = Some(runtime_failure(error));
            }
            return result;
        }
        let _ = (command, now_ms);
        Err(self.unavailable_error())
    }

    pub fn poll_event(&mut self) -> Option<OnlineLobbyEvent> {
        #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
        if let Some(inner) = &mut self.inner {
            return inner.events.pop_front();
        }
        None
    }

    pub fn take_endpoint(&mut self) -> Option<NativeOnlineEndpoint> {
        #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
        if let Some(inner) = &mut self.inner {
            return inner.endpoints.pop_front();
        }
        None
    }

    pub fn match_config(&self) -> Option<&HeadlessMatchConfig> {
        #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
        if let Some(inner) = &self.inner {
            return inner.coordinator.match_config();
        }
        None
    }

    /// True while an old match transport still owns bounded outbound drain or
    /// delayed platform-auth cleanup. AppExit and graceful leave keep pumping
    /// until this becomes false or their own outer process deadline expires.
    pub fn transport_retirement_pending(&self) -> bool {
        #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
        if let Some(inner) = &self.inner {
            return inner.coordinator.retiring_transport_count() != 0;
        }
        false
    }

    /// True after shutdown has atomically fenced new online capability for the
    /// current match. A fresh create/join or completed return-to-lobby epoch
    /// clears the fence.
    pub fn admission_is_quiesced(&self) -> bool {
        #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
        if let Some(inner) = &self.inner {
            return inner.admission_quiesced;
        }
        false
    }

    pub fn committed_authenticated_roster(&self) -> Option<CommittedAuthenticatedRoster> {
        #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
        if let Some(inner) = &self.inner {
            return inner.committed_roster;
        }
        None
    }

    /// Ticket-free local platform identity used to construct the local lobby
    /// declaration and to identify the listen host in the committed roster.
    /// Authentication-ticket bytes never cross this application boundary.
    pub fn local_authenticated_user(&self) -> Option<AuthenticatedUserId> {
        #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
        if let Some(inner) = &self.inner {
            return Some(inner.local_authenticated_user());
        }
        None
    }

    /// Constructs a declaration whose authenticated identity is guaranteed to
    /// match the Steam client owned by this runtime. The application chooses a
    /// non-zero protocol peer ID, but cannot accidentally or maliciously bind
    /// the declaration to a different platform user.
    pub fn make_local_declaration(
        &self,
        peer_id: PeerId,
        revision: u16,
        ready: bool,
        seats: &[OnlineSeatSelection],
    ) -> Result<OnlineRosterMember, NativeOnlineRuntimeError> {
        let Some(authenticated_user) = self.local_authenticated_user() else {
            return Err(self.unavailable_error());
        };
        OnlineRosterMember::new(peer_id, authenticated_user, revision, ready, seats)
            .map_err(|_| NativeOnlineRuntimeError::InvalidAuthenticatedRoster)
    }

    fn unavailable_error(&self) -> NativeOnlineRuntimeError {
        let reason = match self.availability {
            NativeOnlineAvailability::Available => {
                NativeOnlineUnavailableReason::SteamInitializationFailed
            }
            NativeOnlineAvailability::Unavailable(reason) => reason,
        };
        NativeOnlineRuntimeError::Unavailable(reason)
    }

    /// Bevy/application-frame convenience. Unavailable builds remain a stable
    /// menu state; available builds record a sanitized failure for the UI.
    pub fn pump_frame(&mut self, now_ms: u64) -> Result<(), NativeOnlineRuntimeError> {
        if self.availability.is_available() {
            self.pump(now_ms)
        } else {
            Ok(())
        }
    }
}

#[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
fn runtime_failure(error: &NativeOnlineRuntimeError) -> OnlineFailure {
    let (code, recovery) = match error {
        NativeOnlineRuntimeError::Signal(_) => (
            OnlineFailureCode::AuthenticationFailed,
            OnlineRecoveryAction::ReturnToMenu,
        ),
        NativeOnlineRuntimeError::Transport(_) => (
            OnlineFailureCode::ConnectionTimedOut,
            OnlineRecoveryAction::ReturnToMenu,
        ),
        NativeOnlineRuntimeError::Capacity => (
            OnlineFailureCode::InternalCapacity,
            OnlineRecoveryAction::ReturnToMenu,
        ),
        NativeOnlineRuntimeError::Unavailable(_)
        | NativeOnlineRuntimeError::Configuration(_)
        | NativeOnlineRuntimeError::Steam(_) => (
            OnlineFailureCode::SteamUnavailable,
            OnlineRecoveryAction::DisableOnline,
        ),
        NativeOnlineRuntimeError::Lobby(_)
        | NativeOnlineRuntimeError::TimeRegression
        | NativeOnlineRuntimeError::InvalidAuthenticatedRoster
        | NativeOnlineRuntimeError::EndpointIdentityMismatch => (
            OnlineFailureCode::InternalFailure,
            OnlineRecoveryAction::ReturnToMenu,
        ),
    };
    OnlineFailure {
        code,
        severity: OnlineFailureSeverity::Fatal,
        recovery,
        detail_code: match error {
            NativeOnlineRuntimeError::Signal(error) => auth_signal_detail_code(*error),
            NativeOnlineRuntimeError::Transport(_) => 220,
            NativeOnlineRuntimeError::Capacity => 221,
            NativeOnlineRuntimeError::Unavailable(_) => 222,
            NativeOnlineRuntimeError::Configuration(_) => 223,
            NativeOnlineRuntimeError::Steam(_) => 224,
            NativeOnlineRuntimeError::Lobby(_) => 225,
            NativeOnlineRuntimeError::TimeRegression => 226,
            NativeOnlineRuntimeError::InvalidAuthenticatedRoster => 227,
            NativeOnlineRuntimeError::EndpointIdentityMismatch => 228,
        },
    }
}

fn unavailable_view(
    availability: NativeOnlineAvailability,
    failure: Option<OnlineFailure>,
) -> NativeOnlineViewModel {
    NativeOnlineViewModel {
        availability,
        screen: NativeOnlineScreen::Unavailable,
        actions: NativeOnlineActions {
            return_to_menu: true,
            ..Default::default()
        },
        lobby: None,
        role: None,
        lobby_members: 0,
        total_seats: 0,
        local_seats: 0,
        local_ready: false,
        all_members_ready: false,
        connected_remote_peers: 0,
        secure_remote_peers: 0,
        required_remote_peers: 0,
        verified_remote_accounts: 0,
        required_remote_accounts: 0,
        steam_network_readiness: SteamNetworkReadiness::default(),
        setup_stage: OnlineSetupStage::PreparingSteamNetwork,
        start_blocker: Some(OnlineStartBlocker::PreparingSteamNetwork),
        network_quality: NetworkQualitySnapshot::default(),
        input_delay_calibration: InputDelayCalibrationSnapshot::default(),
        relay_status: SteamRelayStatus::default(),
        countdown_start_tick: None,
        outcome: None,
        failure,
    }
}

#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
fn project_view(
    availability: NativeOnlineAvailability,
    status: OnlineLobbyStatus,
    local_declaration: Option<OnlineRosterMember>,
    runtime_failure: Option<OnlineFailure>,
) -> NativeOnlineViewModel {
    let failure = runtime_failure.or(status.failure);
    let screen = if runtime_failure.is_some()
        || (failure.is_some() && status.phase == OnlineLobbyPhase::Failed)
    {
        NativeOnlineScreen::Error
    } else {
        match status.phase {
            OnlineLobbyPhase::OfflineMenu => NativeOnlineScreen::OnlineMenu,
            OnlineLobbyPhase::InvitePending => NativeOnlineScreen::JoinPrompt,
            OnlineLobbyPhase::CreatingLobby => NativeOnlineScreen::CreatingLobby,
            OnlineLobbyPhase::JoiningLobby => NativeOnlineScreen::JoiningLobby,
            OnlineLobbyPhase::Lobby => NativeOnlineScreen::Lobby,
            OnlineLobbyPhase::Connecting => NativeOnlineScreen::Connecting,
            OnlineLobbyPhase::Authenticating => NativeOnlineScreen::Authenticating,
            OnlineLobbyPhase::ManifestAgreement => NativeOnlineScreen::ManifestAgreement,
            OnlineLobbyPhase::Loading | OnlineLobbyPhase::InitialSync => {
                NativeOnlineScreen::Loading
            }
            OnlineLobbyPhase::Ready => NativeOnlineScreen::Ready,
            OnlineLobbyPhase::Countdown => NativeOnlineScreen::Countdown,
            OnlineLobbyPhase::Fighting => NativeOnlineScreen::Fighting,
            OnlineLobbyPhase::Reconnecting => NativeOnlineScreen::Reconnecting,
            OnlineLobbyPhase::ConfirmingResult => NativeOnlineScreen::ConfirmingResult,
            OnlineLobbyPhase::Results => NativeOnlineScreen::Results,
            OnlineLobbyPhase::ReturningToLobby => NativeOnlineScreen::ReturningToLobby,
            OnlineLobbyPhase::Failed => NativeOnlineScreen::Error,
        }
    };
    let in_menu = status.phase == OnlineLobbyPhase::OfflineMenu;
    let in_lobby = status.phase == OnlineLobbyPhase::Lobby;
    let in_results = status.phase == OnlineLobbyPhase::Results;
    let phase_actions = NativeOnlineActions {
        create_private: in_menu,
        create_friends: in_menu,
        accept_join: status.phase == OnlineLobbyPhase::InvitePending,
        decline_join: status.phase == OnlineLobbyPhase::InvitePending,
        edit_couch_seats_and_loadouts: in_lobby,
        toggle_ready: in_lobby,
        invite_friends: in_lobby && status.effective_joinable,
        leave: !in_menu && status.phase != OnlineLobbyPhase::ReturningToLobby,
        rematch: in_results && status.outcome == Some(OnlineMatchOutcome::Confirmed),
        return_to_lobby: in_results || status.phase == OnlineLobbyPhase::Failed,
        return_to_menu: in_menu || in_results || status.phase == OnlineLobbyPhase::Failed,
    };
    let actions = if let Some(failure) = failure {
        match failure.recovery {
            OnlineRecoveryAction::ReturnToLobby | OnlineRecoveryAction::MatchEndedNoContest => {
                NativeOnlineActions {
                    return_to_lobby: true,
                    ..Default::default()
                }
            }
            OnlineRecoveryAction::ReturnToMenu | OnlineRecoveryAction::DisableOnline => {
                NativeOnlineActions {
                    return_to_menu: true,
                    ..Default::default()
                }
            }
            OnlineRecoveryAction::Dismiss
            | OnlineRecoveryAction::Retry
            | OnlineRecoveryAction::Reconnect => NativeOnlineActions::default(),
        }
    } else {
        phase_actions
    };
    NativeOnlineViewModel {
        availability,
        screen,
        actions,
        lobby: status.lobby,
        role: status.role,
        lobby_members: status.lobby_members,
        total_seats: status.total_seats,
        local_seats: local_declaration
            .map(|declaration| declaration.seat_count() as u8)
            .unwrap_or(0),
        local_ready: local_declaration.is_some_and(|declaration| declaration.ready),
        all_members_ready: status.all_members_ready,
        connected_remote_peers: status.connected_remote_peers,
        secure_remote_peers: status.secure_remote_peers,
        required_remote_peers: status.required_remote_peers,
        verified_remote_accounts: status.verified_remote_accounts,
        required_remote_accounts: status.required_remote_accounts,
        steam_network_readiness: status.steam_network_readiness,
        setup_stage: status.setup_stage,
        start_blocker: status.start_blocker,
        network_quality: status.network_quality,
        input_delay_calibration: status.input_delay_calibration,
        relay_status: status.relay_status,
        countdown_start_tick: status.countdown_start_tick,
        outcome: status.outcome,
        failure,
    }
}

#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
fn project_ticket_admission_result(
    result: Result<(), OnlineLobbyError>,
) -> Result<(), NativeOnlineRuntimeError> {
    match result {
        Ok(()) => Ok(()),
        // A quality-rejected user's same-match retry is attributable. Project
        // it into the peer-scoped signaling-isolation path rather than turning
        // it into a global lobby/runtime failure for the listen owner.
        Err(OnlineLobbyError::QualityPolicyRejected) => {
            Err(AuthSignalError::UnexpectedPurpose.into())
        }
        Err(OnlineLobbyError::DuplicatePeerBinding | OnlineLobbyError::PeerIdentityMismatch) => {
            Err(AuthSignalError::InvalidIdentity.into())
        }
        Err(OnlineLobbyError::Steam(SteamPlatformError::Backend(
            crate::steam_platform::SteamBackendError::AuthSessionRejected(
                crate::steam_platform::AuthSessionStartFailure::InvalidTicket
                | crate::steam_platform::AuthSessionStartFailure::DuplicateRequest
                | crate::steam_platform::AuthSessionStartFailure::InvalidVersion
                | crate::steam_platform::AuthSessionStartFailure::GameMismatch,
            ),
        ))) => Err(AuthSignalError::InvalidEnvelope.into()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
fn auth_rejection_is_transient(failure: OnlineFailure) -> bool {
    failure.severity == OnlineFailureSeverity::Recoverable
        && failure.recovery == OnlineRecoveryAction::Retry
        && matches!(
            failure.code,
            OnlineFailureCode::AuthenticationTimedOut
                | OnlineFailureCode::SteamDisconnected
                | OnlineFailureCode::SteamUnavailable
                | OnlineFailureCode::ConnectionTimedOut
        )
}

#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
const fn immediate_auth_error_is_transient(error: SteamPlatformError) -> bool {
    matches!(
        error,
        SteamPlatformError::Backend(crate::steam_platform::SteamBackendError::NotLoggedOn)
            | SteamPlatformError::Backend(
                crate::steam_platform::SteamBackendError::AuthSessionRejected(
                    crate::steam_platform::AuthSessionStartFailure::ExpiredTicket
                )
            )
    )
}

#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
mod real {
    use super::*;
    #[cfg(test)]
    use crate::steam_platform::LobbyMember;
    use crate::steam_platform::MemberReadiness;
    #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
    use crate::steam_platform::RealSteamBackend;

    #[derive(Clone, Copy)]
    struct TicketExchange {
        lease: AuthTicketLease,
        sent_sequence: Option<u32>,
        route: TicketRoute,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum TicketRoute {
        Direct,
        ViaAuthority {
            auth_epoch: AccountAuthEpoch,
            ticket_id: u32,
            authority: SteamUserId,
        },
    }

    #[derive(Clone, Copy)]
    struct AuthenticatedMapping {
        user: SteamUserId,
        peer: AuthenticatedPeer,
        connection: Option<SteamConnectionId>,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct RosterAuthParticipant {
        user: SteamUserId,
        prepare_accepted: bool,
        incoming_validated: bool,
        outgoing_accepted: bool,
        process_complete: bool,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct RoutedIncomingTicket {
        sender: SteamUserId,
        ticket_id: u32,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum PendingSteamSetupRetryKind {
        Direct,
        ClosedControl,
        RosterLease,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct PendingSteamSetupRetry {
        user: SteamUserId,
        connection: Option<SteamConnectionId>,
        kind: PendingSteamSetupRetryKind,
        failure: OnlineFailure,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct RosterAuthTransaction {
        epoch: AccountAuthEpoch,
        roster_hash: u64,
        member_count: u8,
        participants: [Option<RosterAuthParticipant>; MAX_STEAM_LOBBY_MEMBERS],
        prepared: bool,
        local_complete_sent: bool,
        globally_complete: bool,
    }

    impl RosterAuthTransaction {
        fn participant(&self, user: SteamUserId) -> Option<&RosterAuthParticipant> {
            self.participants
                .iter()
                .flatten()
                .find(|participant| participant.user == user)
        }

        fn participant_mut(&mut self, user: SteamUserId) -> Option<&mut RosterAuthParticipant> {
            self.participants
                .iter_mut()
                .flatten()
                .find(|participant| participant.user == user)
        }

        fn local_process_complete(&self) -> bool {
            self.participants
                .iter()
                .flatten()
                .all(|participant| participant.incoming_validated && participant.outgoing_accepted)
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct ManifestParticipant {
        user: SteamUserId,
        connection: SteamConnectionId,
        accepted: bool,
        commit_accepted: bool,
        activated: bool,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum ManifestTransactionStage {
        Preparing,
        Committing,
        Activating,
        Activated,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct RuntimeManifestTransaction {
        id: ManifestTransactionId,
        manifest_hash: crate::network_protocol::ManifestHash,
        participants: [Option<ManifestParticipant>; MAX_STEAM_LOBBY_MEMBERS],
        stage: ManifestTransactionStage,
        activation_deadline_ms: Option<u64>,
    }

    impl RuntimeManifestTransaction {
        fn participant(&self, user: SteamUserId) -> Option<&ManifestParticipant> {
            self.participants
                .iter()
                .flatten()
                .find(|participant| participant.user == user)
        }

        fn participant_mut(&mut self, user: SteamUserId) -> Option<&mut ManifestParticipant> {
            self.participants
                .iter_mut()
                .flatten()
                .find(|participant| participant.user == user)
        }
    }

    fn clear_runtime_peer_transport(
        authenticated: &mut [Option<AuthenticatedMapping>; MAX_STEAM_LOBBY_MEMBERS],
        endpoints: &mut VecDeque<NativeOnlineEndpoint>,
        user: SteamUserId,
        connection: SteamConnectionId,
    ) {
        for slot in authenticated {
            if slot.is_some_and(|mapping| {
                mapping.user == user && mapping.connection == Some(connection)
            }) {
                *slot = None;
            }
        }
        endpoints.retain(|endpoint| {
            endpoint.admitted.remote_user != user || endpoint.admitted.connection != connection
        });
    }

    /// A newly visible Steam lobby member may still be completing its local
    /// LobbyEnter transition. Waiting for its coherent member declaration
    /// proves that process has entered the lobby and published application
    /// state before the other peer starts its quarantined control connection.
    #[cfg(test)]
    fn member_can_open_auth_signal_session(member: &LobbyMember) -> bool {
        matches!(member.readiness, MemberReadiness::Declared { .. }) && member.loadout.is_some()
    }

    fn reconcile_runtime_identity_handoffs(
        live_bindings: [Option<OnlinePeerIdentity>; MAX_STEAM_LOBBY_MEMBERS],
        active_members: [Option<SteamUserId>; MAX_STEAM_LOBBY_MEMBERS],
        authenticated: &mut [Option<AuthenticatedMapping>; MAX_STEAM_LOBBY_MEMBERS],
        ticket_exchanges: &mut [Option<TicketExchange>; MAX_STEAM_LOBBY_MEMBERS],
        reconnect_users: &mut [Option<SteamUserId>; MAX_STEAM_LOBBY_MEMBERS],
        endpoints: &mut VecDeque<NativeOnlineEndpoint>,
        pending_manifest: &mut Option<BootstrapManifestSignal>,
        signal_rejected_users: &mut [Option<SteamUserId>; MAX_STEAM_LOBBY_MEMBERS],
    ) {
        let has_user = |user: SteamUserId| {
            live_bindings
                .iter()
                .flatten()
                .any(|identity| identity.user == user)
        };
        let has_identity = |user: SteamUserId, peer_id: PeerId| {
            live_bindings
                .iter()
                .flatten()
                .any(|identity| identity.user == user && identity.peer_id == peer_id)
        };
        let is_active_or_bound =
            |user: SteamUserId| has_user(user) || active_members.contains(&Some(user));

        for slot in authenticated {
            if slot.is_some_and(|mapping| !has_identity(mapping.user, mapping.peer.peer_id)) {
                *slot = None;
            }
        }
        for slot in ticket_exchanges {
            if slot.is_some_and(|record| !is_active_or_bound(record.lease.remote_user)) {
                *slot = None;
            }
        }
        for slot in reconnect_users {
            if slot.is_some_and(|user| !has_user(user)) {
                *slot = None;
            }
        }
        endpoints.retain(|endpoint| has_identity(endpoint.admitted.remote_user, endpoint.peer_id));
        if pending_manifest.is_some_and(|manifest| !is_active_or_bound(manifest.sender)) {
            *pending_manifest = None;
        }

        // Signal rejection remains fail-closed for a malformed active member
        // even after its coordinator binding is isolated. Once the refreshed
        // platform roster proves departure, membership admission rejects that
        // user and retaining this bounded history would only let sequential
        // departed attackers poison a long-lived lobby.
        for slot in signal_rejected_users {
            if slot.is_some_and(|user| !active_members.contains(&Some(user))) {
                *slot = None;
            }
        }
    }

    #[cfg(test)]
    #[derive(Clone, Copy)]
    struct SignalAdmissionPolicy {
        active_lobby: Option<SteamLobbyId>,
        users: [Option<SteamUserId>; MAX_STEAM_LOBBY_MEMBERS],
        quarantined: [Option<SteamUserId>; MAX_STEAM_LOBBY_MEMBERS],
    }

    #[cfg(test)]
    impl Default for SignalAdmissionPolicy {
        fn default() -> Self {
            Self {
                active_lobby: None,
                users: [None; MAX_STEAM_LOBBY_MEMBERS],
                quarantined: [None; MAX_STEAM_LOBBY_MEMBERS],
            }
        }
    }

    #[cfg(test)]
    impl SignalAdmissionPolicy {
        fn contains_member(&self, user: SteamUserId) -> bool {
            self.active_lobby.is_some() && self.users.contains(&Some(user))
        }

        fn allows(&self, user: SteamUserId) -> bool {
            self.contains_member(user) && !self.quarantined.contains(&Some(user))
        }

        fn quarantine(&mut self, user: SteamUserId) {
            if self.quarantined.contains(&Some(user)) {
                return;
            }
            if let Some(slot) = self.quarantined.iter_mut().find(|slot| slot.is_none()) {
                *slot = Some(user);
            }
        }

        fn clear_quarantine(&mut self) {
            self.quarantined = [None; MAX_STEAM_LOBBY_MEMBERS];
        }

        fn carry_quarantine_into(&self, next: &mut Self) {
            if self.active_lobby != next.active_lobby {
                return;
            }
            for user in self.quarantined.iter().flatten().copied() {
                if next.users.contains(&Some(user)) {
                    next.quarantine(user);
                }
            }
        }
    }

    #[cfg(test)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum SignalSessionRequestAction {
        Accept,
        Defer,
        Reject,
    }

    #[cfg(test)]
    fn classify_signal_session_request(
        policy: SignalAdmissionPolicy,
        user: Option<SteamUserId>,
    ) -> SignalSessionRequestAction {
        let Some(user) = user else {
            return SignalSessionRequestAction::Reject;
        };
        if policy.active_lobby.is_none() || policy.quarantined.contains(&Some(user)) {
            return SignalSessionRequestAction::Reject;
        }
        if policy.contains_member(user) {
            SignalSessionRequestAction::Accept
        } else {
            // Keep an unknown legacy request unaccepted while the next roster
            // refresh determines whether it is a valid member.
            SignalSessionRequestAction::Defer
        }
    }

    #[cfg(test)]
    #[derive(Clone, Copy)]
    struct PrimedSignalSessions {
        lobby: Option<SteamLobbyId>,
        users: [Option<SteamUserId>; MAX_STEAM_LOBBY_MEMBERS],
    }

    #[cfg(test)]
    impl Default for PrimedSignalSessions {
        fn default() -> Self {
            Self {
                lobby: None,
                users: [None; MAX_STEAM_LOBBY_MEMBERS],
            }
        }
    }

    #[cfg(test)]
    impl PrimedSignalSessions {
        fn pending_for(
            &mut self,
            policy: SignalAdmissionPolicy,
        ) -> [Option<SteamUserId>; MAX_STEAM_LOBBY_MEMBERS] {
            if self.lobby != policy.active_lobby {
                self.lobby = policy.active_lobby;
                self.users = [None; MAX_STEAM_LOBBY_MEMBERS];
            } else {
                for slot in &mut self.users {
                    if slot.is_some_and(|user| !policy.contains_member(user)) {
                        *slot = None;
                    }
                }
            }

            let mut pending = [None; MAX_STEAM_LOBBY_MEMBERS];
            for user in policy.users.iter().flatten().copied() {
                if !policy.allows(user) || self.users.contains(&Some(user)) {
                    continue;
                }
                if let Some(slot) = pending.iter_mut().find(|slot| slot.is_none()) {
                    *slot = Some(user);
                }
            }
            pending
        }

        fn mark_sent(&mut self, lobby: SteamLobbyId, user: SteamUserId) {
            if self.lobby != Some(lobby) || self.users.contains(&Some(user)) {
                return;
            }
            if let Some(slot) = self.users.iter_mut().find(|slot| slot.is_none()) {
                *slot = Some(user);
            }
        }

        fn clear(&mut self) {
            self.lobby = None;
            self.users = [None; MAX_STEAM_LOBBY_MEMBERS];
        }
    }

    #[cfg(test)]
    #[derive(Clone, Copy)]
    pub(super) struct AuthSignalAdmission {
        active_lobby: Option<SteamLobbyId>,
        users: [Option<SteamUserId>; MAX_STEAM_LOBBY_MEMBERS],
    }

    pub(super) trait NativeTransportFactory<B: SteamBackend> {
        fn create_transport(
            &self,
            platform: &SteamPlatform<B>,
            session: SteamP2pSession,
            config: SteamTransportConfig,
            now_ms: u64,
        ) -> Result<SteamTransport, SteamTransportError>;
    }

    fn auth_signal_peer_failure(error: AuthSignalError) -> OnlineFailure {
        let (code, severity, recovery) = match error {
            AuthSignalError::ReceiveBudgetExceeded => (
                OnlineFailureCode::RateLimited,
                OnlineFailureSeverity::Fatal,
                OnlineRecoveryAction::ReturnToLobby,
            ),
            error if auth_signal_error_is_transport(error) => (
                OnlineFailureCode::ConnectionTimedOut,
                OnlineFailureSeverity::Recoverable,
                OnlineRecoveryAction::Reconnect,
            ),
            _ => (
                OnlineFailureCode::MalformedTraffic,
                OnlineFailureSeverity::Fatal,
                OnlineRecoveryAction::ReturnToLobby,
            ),
        };
        OnlineFailure {
            code,
            severity,
            recovery,
            detail_code: auth_signal_detail_code(error),
        }
    }

    #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
    pub(super) struct RealNativeTransportFactory;

    #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
    impl NativeTransportFactory<RealSteamBackend> for RealNativeTransportFactory {
        fn create_transport(
            &self,
            platform: &SteamPlatform<RealSteamBackend>,
            session: SteamP2pSession,
            config: SteamTransportConfig,
            now_ms: u64,
        ) -> Result<SteamTransport, SteamTransportError> {
            SteamTransport::from_steam_platform(platform, session, config, now_ms)
        }
    }

    pub(super) struct NativeOnlineCore<B, F>
    where
        B: SteamBackend,
        F: NativeTransportFactory<B>,
    {
        // Rust drops fields in declaration order. Endpoint owners close before
        // coordinator transports and coordinator-owned auth/transport state
        // release before the Steam platform.
        pub(super) endpoints: VecDeque<NativeOnlineEndpoint>,
        pub(super) coordinator: OnlineLobbyCoordinator,
        platform: SteamPlatform<B>,
        transport_factory: F,
        local_declaration: Option<OnlineRosterMember>,
        ticket_exchanges: [Option<TicketExchange>; MAX_STEAM_LOBBY_MEMBERS],
        authenticated: [Option<AuthenticatedMapping>; MAX_STEAM_LOBBY_MEMBERS],
        reconnect_users: [Option<SteamUserId>; MAX_STEAM_LOBBY_MEMBERS],
        signal_rejected_users: [Option<SteamUserId>; MAX_STEAM_LOBBY_MEMBERS],
        pending_steam_setup_retries: [Option<PendingSteamSetupRetry>; MAX_STEAM_LOBBY_MEMBERS],
        deferred_control_setup_retry: Option<SteamUserId>,
        pub(super) committed_roster: Option<CommittedAuthenticatedRoster>,
        pub(super) events: VecDeque<OnlineLobbyEvent>,
        pending_manifest: Option<BootstrapManifestSignal>,
        remote_ticket_sequences: [Option<(SteamUserId, u32)>; MAX_STEAM_LOBBY_MEMBERS],
        local_ticket_accepted: [Option<SteamUserId>; MAX_STEAM_LOBBY_MEMBERS],
        roster_auth: Option<RosterAuthTransaction>,
        routed_incoming_tickets: [Option<RoutedIncomingTicket>; MAX_STEAM_LOBBY_MEMBERS],
        next_account_auth_epoch: u64,
        retired_account_auth_epoch: Option<AccountAuthEpoch>,
        manifest_transaction: Option<RuntimeManifestTransaction>,
        next_manifest_transaction_id: u64,
        retired_manifest_transaction: Option<ManifestTransactionId>,
        pub(super) admission_quiesced: bool,
        last_now_ms: u64,
        pub(super) runtime_failure: Option<OnlineFailure>,
    }

    impl<B, F> NativeOnlineCore<B, F>
    where
        B: SteamBackend,
        F: NativeTransportFactory<B>,
    {
        pub(super) fn local_authenticated_user(&self) -> AuthenticatedUserId {
            self.platform.local_user().authenticated()
        }

        pub(super) fn steam_input_snapshot(&self) -> SteamInputSnapshot {
            self.platform.steam_input_snapshot()
        }

        pub(super) fn is_overlay_active(&self) -> bool {
            self.platform.is_overlay_active()
        }

        pub(super) fn set_steam_input_action_set(
            &mut self,
            action_set: SteamInputActionSet,
        ) -> Result<(), SteamPlatformError> {
            self.platform.set_steam_input_action_set(action_set)
        }

        pub(super) fn show_steam_input_binding_panel(
            &mut self,
            local_ordinal: usize,
        ) -> Result<SteamOverlayRequestStatus, SteamPlatformError> {
            self.platform.show_steam_input_binding_panel(local_ordinal)
        }

        pub(super) fn open_invite_overlay(
            &mut self,
        ) -> Result<SteamOverlayRequestStatus, OnlineLobbyError> {
            self.coordinator.open_invite_overlay(&mut self.platform)
        }

        fn from_parts(
            platform: SteamPlatform<B>,
            transport_factory: F,
            lobby_config: OnlineLobbyConfig,
            now_ms: u64,
        ) -> Result<Self, NativeOnlineRuntimeError> {
            let coordinator =
                OnlineLobbyCoordinator::new(platform.local_user(), lobby_config, now_ms)?;
            Ok(Self {
                endpoints: VecDeque::with_capacity(MAX_STEAM_LOBBY_MEMBERS),
                coordinator,
                platform,
                transport_factory,
                local_declaration: None,
                ticket_exchanges: [None; MAX_STEAM_LOBBY_MEMBERS],
                authenticated: [None; MAX_STEAM_LOBBY_MEMBERS],
                reconnect_users: [None; MAX_STEAM_LOBBY_MEMBERS],
                signal_rejected_users: [None; MAX_STEAM_LOBBY_MEMBERS],
                pending_steam_setup_retries: [None; MAX_STEAM_LOBBY_MEMBERS],
                deferred_control_setup_retry: None,
                committed_roster: None,
                events: VecDeque::with_capacity(MAX_NATIVE_ONLINE_EVENTS),
                pending_manifest: None,
                remote_ticket_sequences: [None; MAX_STEAM_LOBBY_MEMBERS],
                local_ticket_accepted: [None; MAX_STEAM_LOBBY_MEMBERS],
                roster_auth: None,
                routed_incoming_tickets: [None; MAX_STEAM_LOBBY_MEMBERS],
                next_account_auth_epoch: 1,
                retired_account_auth_epoch: None,
                manifest_transaction: None,
                next_manifest_transaction_id: 1,
                retired_manifest_transaction: None,
                admission_quiesced: false,
                last_now_ms: now_ms,
                runtime_failure: None,
            })
        }

        pub(super) fn view_model(&self) -> NativeOnlineViewModel {
            project_view(
                NativeOnlineAvailability::Available,
                self.coordinator.status(),
                self.local_declaration,
                self.runtime_failure,
            )
        }

        pub(super) fn execute(
            &mut self,
            command: NativeOnlineCommand,
            now_ms: u64,
        ) -> Result<(), NativeOnlineRuntimeError> {
            if now_ms < self.last_now_ms {
                return Err(NativeOnlineRuntimeError::TimeRegression);
            }
            let result = match command {
                NativeOnlineCommand::Create(request) => {
                    self.admission_quiesced = false;
                    self.reset_signal_isolation()?;
                    let visibility = request.visibility.steam();
                    let metadata = LobbyMetadata::current(
                        crate::network_protocol::AuthorityKind::Listen,
                        visibility,
                        request.region,
                        request.rules,
                        request.arena,
                        request.seat_capacity,
                    )?;
                    let create = LobbyCreateRequest {
                        visibility,
                        maximum_peers: request.maximum_steam_peers,
                        local_seats: request.local_declaration.seat_count() as u8,
                    };
                    self.coordinator.begin_create(
                        &mut self.platform,
                        create,
                        metadata,
                        request.local_declaration,
                        now_ms,
                    )?;
                    self.install_local_mapping(request.local_declaration)?;
                    self.local_declaration = Some(request.local_declaration);
                    Ok(())
                }
                NativeOnlineCommand::Join {
                    intent,
                    local_declaration,
                } => {
                    self.admission_quiesced = false;
                    self.reset_signal_isolation()?;
                    self.coordinator.begin_join(
                        &mut self.platform,
                        intent,
                        local_declaration,
                        now_ms,
                    )?;
                    self.install_local_mapping(local_declaration)?;
                    self.local_declaration = Some(local_declaration);
                    Ok(())
                }
                NativeOnlineCommand::DeclineJoin => self
                    .coordinator
                    .decline_join_request(now_ms)
                    .map_err(Into::into),
                NativeOnlineCommand::SetLocalDeclaration(declaration) => {
                    self.coordinator
                        .set_local_declaration(&mut self.platform, declaration)?;
                    self.replace_local_mapping(declaration)?;
                    self.local_declaration = Some(declaration);
                    Ok(())
                }
                NativeOnlineCommand::SetReady(ready) => {
                    self.coordinator.set_ready(&mut self.platform, ready)?;
                    if let Some(declaration) = &mut self.local_declaration {
                        declaration.ready = ready;
                    }
                    Ok(())
                }
                NativeOnlineCommand::RetrySteamSetup => self.retry_steam_setup(now_ms),
                NativeOnlineCommand::CommitManifest {
                    options,
                    current_tick,
                } => {
                    if self
                        .roster_auth
                        .as_ref()
                        .is_none_or(|auth| !auth.globally_complete)
                    {
                        Err(OnlineLobbyError::PeersNotReady.into())
                    } else {
                        self.coordinator
                            .commit_manifest(&mut self.platform, options, current_tick, now_ms)
                            .map_err(Into::into)
                    }
                }
                NativeOnlineCommand::AcceptManifest(config) => {
                    self.accept_manifest_and_freeze(config, now_ms)
                }
                NativeOnlineCommand::ContentLoaded => self
                    .coordinator
                    .mark_content_loaded(now_ms)
                    .map_err(Into::into),
                NativeOnlineCommand::InitialSyncComplete => self
                    .coordinator
                    .mark_initial_sync_complete(now_ms)
                    .map_err(Into::into),
                NativeOnlineCommand::BeginCountdown(start_tick) => self
                    .coordinator
                    .begin_countdown(start_tick, now_ms)
                    .map_err(Into::into),
                NativeOnlineCommand::MarkFighting(current_tick) => self
                    .coordinator
                    .mark_fighting(current_tick, now_ms)
                    .map_err(Into::into),
                NativeOnlineCommand::BeginResultConfirmation => self
                    .coordinator
                    .begin_result_confirmation(now_ms)
                    .map_err(Into::into),
                NativeOnlineCommand::ConfirmResult => {
                    self.coordinator.confirm_result(now_ms).map_err(Into::into)
                }
                NativeOnlineCommand::ApplyAuthorityDisconnect(disconnect) => self
                    .coordinator
                    .apply_authority_disconnect(&mut self.platform, disconnect.message, now_ms)
                    .map_err(Into::into),
                NativeOnlineCommand::QuiesceAdmission => {
                    self.coordinator.quiesce_admission(&mut self.platform)?;
                    self.admission_quiesced = true;
                    self.ticket_exchanges = [None; MAX_STEAM_LOBBY_MEMBERS];
                    self.reconnect_users = [None; MAX_STEAM_LOBBY_MEMBERS];
                    self.endpoints.clear();
                    self.pending_manifest = None;
                    self.remote_ticket_sequences = [None; MAX_STEAM_LOBBY_MEMBERS];
                    self.local_ticket_accepted = [None; MAX_STEAM_LOBBY_MEMBERS];
                    self.retire_roster_auth_state();
                    self.retire_manifest_transaction();
                    Ok(())
                }
                NativeOnlineCommand::MarkAuthorityTerminalDrained {
                    user,
                    peer_id,
                    connection,
                    retry,
                } => self
                    .coordinator
                    .mark_authority_terminal_drained(
                        &mut self.platform,
                        user,
                        peer_id,
                        connection,
                        retry,
                    )
                    .map(|_| ())
                    .map_err(Into::into),
                NativeOnlineCommand::Rematch => {
                    self.coordinator
                        .return_to_lobby(&mut self.platform, true, now_ms)?;
                    if self.coordinator.status().phase == OnlineLobbyPhase::Lobby {
                        self.admission_quiesced = false;
                        self.reset_match_handoff();
                        self.reset_signal_isolation()?;
                        self.local_declaration = self.coordinator.local_declaration();
                    }
                    Ok(())
                }
                NativeOnlineCommand::ReturnToLobby => {
                    self.coordinator
                        .return_to_lobby(&mut self.platform, false, now_ms)?;
                    if self.coordinator.status().phase == OnlineLobbyPhase::Lobby {
                        self.admission_quiesced = false;
                        self.reset_match_handoff();
                        self.reset_signal_isolation()?;
                        self.local_declaration = self.coordinator.local_declaration();
                    }
                    Ok(())
                }
                NativeOnlineCommand::LeaveOnline => {
                    self.coordinator.leave_online(&mut self.platform, now_ms)?;
                    self.reset_all_session_state();
                    self.reset_signal_isolation()?;
                    Ok(())
                }
            };
            if result.is_ok() && self.pending_steam_setup_retries.iter().all(Option::is_none) {
                self.runtime_failure = None;
            }
            result
        }

        pub(super) fn pump(&mut self, now_ms: u64) -> Result<(), NativeOnlineRuntimeError> {
            if now_ms < self.last_now_ms {
                return Err(NativeOnlineRuntimeError::TimeRegression);
            }
            self.last_now_ms = now_ms;
            // The AFCP activation barrier owns its bounded rollback while no
            // endpoint is exposed. Enforce it before the coordinator's generic
            // Loading timeout can turn the same instant into a terminal match
            // failure.
            self.enforce_activation_deadline(now_ms)?;
            self.drain_coordinator_events(now_ms)?;
            if let Err(error) = self.coordinator.pump(&mut self.platform, now_ms) {
                #[cfg(all(feature = "steam-net", not(target_arch = "wasm32"), not(test)))]
                self.persist_pregame_failure_traces();
                return Err(error.into());
            }
            // Retire coordinator-owned transactions before interpreting any
            // frames that became stale at the same deadline. This makes a
            // delayed Activate/Cancel batch benign regardless of pump order.
            self.drain_coordinator_events(now_ms)?;
            if self.coordinator.steam_backend_reconnect_pending() {
                // Keep pumping the installed transport in the coordinator so
                // established gameplay endpoints remain usable, but do not
                // advance any new ticket, auth, or manifest capability until
                // Steam reports a coherent backend recovery.
                return Ok(());
            }
            self.drain_control_ingress(now_ms)?;
            self.drain_coordinator_events(now_ms)?;
            if !self.admission_quiesced {
                self.install_requested_transport(now_ms)?;
                self.reconcile_ticket_exchanges()?;
                self.flush_ready_tickets()?;
                self.reconcile_roster_authentication()?;
            }
            self.drain_coordinator_events(now_ms)?;
            if !self.admission_quiesced {
                self.try_apply_pending_manifest(now_ms)?;
            }
            self.drain_coordinator_events(now_ms)?;
            Ok(())
        }

        fn drain_control_ingress(&mut self, now_ms: u64) -> Result<(), NativeOnlineRuntimeError> {
            let mut drained = 0_usize;
            let mut per_user = [None; MAX_STEAM_LOBBY_MEMBERS];
            while drained < MAX_AUTH_SIGNALS_PER_PUMP {
                let Some(ingress) = self.coordinator.poll_control() else {
                    break;
                };
                drained += 1;
                let source = ingress.user;
                let token = ingress.token;
                let user_count = if let Some((_, count)) = per_user
                    .iter_mut()
                    .flatten()
                    .find(|(user, _)| *user == source)
                {
                    *count += 1;
                    *count
                } else {
                    let slot = per_user
                        .iter_mut()
                        .find(|slot| slot.is_none())
                        .ok_or(NativeOnlineRuntimeError::Capacity)?;
                    *slot = Some((source, 1_usize));
                    1
                };
                if user_count > MAX_AUTH_SIGNALS_PER_USER_PER_PUMP {
                    self.coordinator.reject_control_ingress(token)?;
                    self.isolate_signal_peer(source, AuthSignalError::ReceiveBudgetExceeded)?;
                    continue;
                }
                let result = match ingress.message {
                    SteamControlMessage::LinkHello { .. } => Ok(()),
                    SteamControlMessage::AuthTicket(ticket) => {
                        let identity = ticket.identity;
                        let signal = AuthTicketSignal::new(
                            identity.lobby,
                            identity.sender,
                            identity.recipient,
                            ticket.sender_peer_id,
                            ticket.purpose,
                            ticket.owner_revision,
                            ticket.sender_revision,
                            ticket.match_id,
                            ticket.ticket(),
                        )?;
                        if self.consume_ticket_signal(source, signal, now_ms)? {
                            self.remember_remote_ticket_sequence(source, ingress.sequence)?;
                        }
                        Ok(())
                    }
                    SteamControlMessage::AuthAccepted {
                        identity,
                        ticket_sequence,
                    } => self.consume_auth_accepted(source, identity, ticket_sequence),
                    SteamControlMessage::RosterPrepare {
                        identity,
                        auth_epoch,
                        roster_hash,
                        member_count,
                    } => self.consume_roster_prepare(
                        source,
                        ingress.connection,
                        identity,
                        auth_epoch,
                        roster_hash,
                        member_count,
                    ),
                    SteamControlMessage::RosterAccepted {
                        identity,
                        auth_epoch,
                        roster_hash,
                        member_count,
                    } => self.consume_roster_accepted(
                        source,
                        ingress.connection,
                        identity,
                        auth_epoch,
                        roster_hash,
                        member_count,
                    ),
                    SteamControlMessage::RoutedAuthTicket {
                        identity,
                        auth_epoch,
                        ticket_id,
                        ticket,
                    } => self.consume_routed_auth_ticket(
                        source, identity, auth_epoch, ticket_id, ticket, now_ms,
                    ),
                    SteamControlMessage::RoutedAuthAccepted {
                        identity,
                        auth_epoch,
                        ticket_id,
                        ticket_sender,
                        ticket_recipient,
                    } => self.consume_routed_auth_accepted(
                        source,
                        identity,
                        auth_epoch,
                        ticket_id,
                        ticket_sender,
                        ticket_recipient,
                    ),
                    SteamControlMessage::RosterAuthComplete {
                        identity,
                        auth_epoch,
                        roster_hash,
                    } => {
                        self.consume_roster_auth_complete(source, identity, auth_epoch, roster_hash)
                    }
                    SteamControlMessage::ManifestPrepare {
                        identity,
                        transaction,
                        manifest,
                    } => self.consume_manifest_prepare(
                        source,
                        ingress.connection,
                        identity,
                        transaction,
                        manifest,
                        now_ms,
                    ),
                    SteamControlMessage::ManifestAccepted {
                        identity,
                        transaction,
                        manifest_hash,
                    } => self.consume_manifest_accepted(
                        source,
                        ingress.connection,
                        identity,
                        transaction,
                        manifest_hash,
                        now_ms,
                    ),
                    SteamControlMessage::ManifestCommit {
                        identity,
                        transaction,
                        manifest_hash,
                    } => self.consume_manifest_commit(
                        source,
                        ingress.connection,
                        identity,
                        transaction,
                        manifest_hash,
                        now_ms,
                    ),
                    SteamControlMessage::ManifestCommitAccepted {
                        identity,
                        transaction,
                        manifest_hash,
                    } => self.consume_manifest_commit_accepted(
                        source,
                        ingress.connection,
                        identity,
                        transaction,
                        manifest_hash,
                        now_ms,
                    ),
                    SteamControlMessage::GameplayActivate {
                        identity,
                        transaction,
                        manifest_hash,
                    } => self.consume_gameplay_activate(
                        source,
                        ingress.connection,
                        identity,
                        transaction,
                        manifest_hash,
                    ),
                    SteamControlMessage::GameplayActivated {
                        identity,
                        transaction,
                        manifest_hash,
                    } => self.consume_gameplay_activated(
                        source,
                        ingress.connection,
                        identity,
                        transaction,
                        manifest_hash,
                    ),
                    SteamControlMessage::Abort {
                        identity,
                        transaction,
                        code: reason_code,
                        permanent,
                    } => self.consume_setup_abort(
                        source,
                        identity,
                        transaction,
                        reason_code,
                        permanent,
                        now_ms,
                    ),
                    SteamControlMessage::SetupCancel {
                        identity,
                        transaction,
                        code,
                    } => self.consume_setup_cancel(
                        source,
                        ingress.connection,
                        identity,
                        transaction,
                        code,
                        now_ms,
                    ),
                };
                match result {
                    Ok(()) => {
                        self.coordinator.accept_control_ingress(token)?;
                        if self.deferred_control_setup_retry == Some(source) {
                            let generation = self
                                .coordinator
                                .control_connection_for_user(source)
                                .ok_or(OnlineLobbyError::MissingPeerBinding(source))?;
                            self.coordinator.retry_control_setup(
                                &mut self.platform,
                                source,
                                generation,
                                now_ms,
                            )?;
                            self.deferred_control_setup_retry = None;
                            break;
                        }
                    }
                    Err(NativeOnlineRuntimeError::Signal(error)) => {
                        self.coordinator.reject_control_ingress(token)?;
                        self.isolate_signal_peer(source, error)?;
                    }
                    Err(error) => {
                        self.coordinator.reject_control_ingress(token)?;
                        return Err(error);
                    }
                }
            }
            Ok(())
        }

        fn isolate_signal_peer(
            &mut self,
            user: SteamUserId,
            error: AuthSignalError,
        ) -> Result<(), NativeOnlineRuntimeError> {
            if self.is_signal_rejected_user(user) {
                return Ok(());
            }
            let connection = self.coordinator.active_connection_for_user(user);
            self.coordinator.isolate_peer_authentication_with_reason(
                &mut self.platform,
                user,
                SteamTransportCloseReason::MalformedControlTraffic,
            )?;
            #[cfg(all(feature = "steam-net", not(target_arch = "wasm32"), not(test)))]
            self.persist_pregame_failure_traces();
            self.mark_signal_rejected_user(user)?;
            self.clear_peer_handoffs(user);
            self.push_event(OnlineLobbyEvent::PeerAuthenticationRejected {
                user,
                connection,
                failure: auth_signal_peer_failure(error),
            })
        }

        fn record_pending_steam_setup_retry(
            &mut self,
            user: SteamUserId,
            connection: Option<SteamConnectionId>,
            kind: PendingSteamSetupRetryKind,
            failure: OnlineFailure,
        ) -> Result<(), NativeOnlineRuntimeError> {
            if !auth_rejection_is_transient(failure) {
                return Err(AuthSignalError::UnexpectedPurpose.into());
            }
            if let Some(slot) = self.pending_steam_setup_retries.iter_mut().find(|slot| {
                slot.is_some_and(|existing| {
                    existing.user == user
                        && existing.connection == connection
                        && existing.kind == kind
                })
            }) {
                *slot = Some(PendingSteamSetupRetry {
                    user,
                    connection,
                    kind,
                    failure,
                });
            } else {
                let slot = self
                    .pending_steam_setup_retries
                    .iter_mut()
                    .find(|slot| slot.is_none())
                    .ok_or(NativeOnlineRuntimeError::Capacity)?;
                *slot = Some(PendingSteamSetupRetry {
                    user,
                    connection,
                    kind,
                    failure,
                });
            }
            self.runtime_failure = self
                .pending_steam_setup_retries
                .iter()
                .flatten()
                .next()
                .map(|pending| pending.failure);
            Ok(())
        }

        fn clear_pending_steam_setup_retries_for_user(&mut self, user: SteamUserId) {
            for slot in &mut self.pending_steam_setup_retries {
                if slot.is_some_and(|pending| pending.user == user) {
                    *slot = None;
                }
            }
            self.runtime_failure = self
                .pending_steam_setup_retries
                .iter()
                .flatten()
                .next()
                .map(|pending| pending.failure);
        }

        fn clear_pending_steam_setup_retry(
            &mut self,
            user: SteamUserId,
            kind: PendingSteamSetupRetryKind,
        ) {
            for slot in &mut self.pending_steam_setup_retries {
                if slot.is_some_and(|pending| pending.user == user && pending.kind == kind) {
                    *slot = None;
                }
            }
            self.runtime_failure = self
                .pending_steam_setup_retries
                .iter()
                .flatten()
                .next()
                .map(|pending| pending.failure);
        }

        fn clear_pending_roster_setup_retries(&mut self) {
            for slot in &mut self.pending_steam_setup_retries {
                if slot
                    .is_some_and(|pending| pending.kind == PendingSteamSetupRetryKind::RosterLease)
                {
                    *slot = None;
                }
            }
            self.runtime_failure = self
                .pending_steam_setup_retries
                .iter()
                .flatten()
                .next()
                .map(|pending| pending.failure);
        }

        fn retry_steam_setup(&mut self, now_ms: u64) -> Result<(), NativeOnlineRuntimeError> {
            let Some((pending_index, pending)) = self
                .pending_steam_setup_retries
                .iter()
                .enumerate()
                .find_map(|(index, pending)| pending.map(|pending| (index, pending)))
            else {
                if self.runtime_failure.is_some_and(|failure| {
                    failure.code == OnlineFailureCode::SteamUnavailable
                        && failure.severity == OnlineFailureSeverity::Recoverable
                        && failure.recovery == OnlineRecoveryAction::Retry
                }) {
                    self.coordinator
                        .retry_steam_network_initialization(now_ms)?;
                    self.runtime_failure = None;
                    return Ok(());
                }
                return Err(OnlineLobbyError::InvalidState.into());
            };
            let lobby = self
                .coordinator
                .status()
                .lobby
                .ok_or(NativeOnlineRuntimeError::Lobby(
                    OnlineLobbyError::InvalidState,
                ))?;
            match pending.kind {
                PendingSteamSetupRetryKind::RosterLease => {
                    // Steam validation callbacks are not generation-tagged.
                    // The first timeout retains the exact native session and
                    // Retry extends only that lease rather than starting a
                    // second BeginAuthSession.
                    if self
                        .platform
                        .roster_authentication_awaits_retry(lobby, pending.user)
                    {
                        self.platform.extend_roster_authentication_deadline(
                            lobby,
                            pending.user,
                            now_ms,
                        )?;
                    } else {
                        // An immediate ExpiredTicket/NotLoggedOn-like result
                        // never created a native lease. Retire this routed
                        // exchange so the originating client can issue a fresh
                        // recipient-bound, one-use ticket.
                        let status = self.coordinator.status();
                        let owner = status.owner.ok_or(AuthSignalError::InvalidIdentity)?;
                        let local = self.platform.local_user();
                        if status.role == Some(OnlineLobbyRole::Client) {
                            let identity = SteamControlIdentity::new(lobby, local, owner)
                                .map_err(|_| AuthSignalError::InvalidIdentity)?;
                            self.coordinator.queue_control_for_user(
                                owner,
                                SteamControlMessage::Abort {
                                    identity,
                                    transaction: None,
                                    code: AUTH_RETRY_ROSTER_ABORT_CODE,
                                    permanent: false,
                                },
                            )?;
                        }
                        self.retire_roster_auth_state();
                    }
                }
                PendingSteamSetupRetryKind::Direct => {
                    let status = self.coordinator.status();
                    if status.role == Some(OnlineLobbyRole::ListenAuthority) {
                        if let Some(roster) = self.roster_auth {
                            for participant in roster.participants.iter().flatten() {
                                let roster_identity = SteamControlIdentity::new(
                                    lobby,
                                    self.platform.local_user(),
                                    participant.user,
                                )
                                .map_err(|_| AuthSignalError::InvalidIdentity)?;
                                self.coordinator.queue_control_for_user(
                                    participant.user,
                                    SteamControlMessage::Abort {
                                        identity: roster_identity,
                                        transaction: None,
                                        code: AUTH_RETRY_ROSTER_ABORT_CODE,
                                        permanent: false,
                                    },
                                )?;
                            }
                        }
                        let identity = SteamControlIdentity::new(
                            lobby,
                            self.platform.local_user(),
                            pending.user,
                        )
                        .map_err(|_| AuthSignalError::InvalidIdentity)?;
                        self.coordinator.queue_control_for_user(
                            pending.user,
                            SteamControlMessage::Abort {
                                identity,
                                transaction: None,
                                code: AUTH_RETRY_DIRECT_ABORT_CODE,
                                permanent: false,
                            },
                        )?;
                        // The authority keeps this quarantined socket alive
                        // until the client consumes Abort and originates the
                        // replacement. This preserves star topology and gives
                        // the reliable retry instruction a delivery window.
                        self.clear_peer_handoffs(pending.user);
                    } else {
                        let identity = SteamControlIdentity::new(
                            lobby,
                            self.platform.local_user(),
                            pending.user,
                        )
                        .map_err(|_| AuthSignalError::InvalidIdentity)?;
                        self.coordinator.queue_control_for_user(
                            pending.user,
                            SteamControlMessage::Abort {
                                identity,
                                transaction: None,
                                code: AUTH_RETRY_DIRECT_ABORT_CODE,
                                permanent: false,
                            },
                        )?;
                    }
                }
                PendingSteamSetupRetryKind::ClosedControl => {
                    let generation = pending.connection.ok_or(OnlineLobbyError::InvalidState)?;
                    if !self
                        .coordinator
                        .control_connection_for_user(pending.user)
                        .is_some_and(|active| active != generation)
                    {
                        self.coordinator.retry_control_setup(
                            &mut self.platform,
                            pending.user,
                            generation,
                            now_ms,
                        )?;
                    }
                }
            }
            self.pending_steam_setup_retries[pending_index] = None;
            self.runtime_failure = self
                .pending_steam_setup_retries
                .iter()
                .flatten()
                .next()
                .map(|pending| pending.failure);
            Ok(())
        }

        fn install_requested_transport(
            &mut self,
            now_ms: u64,
        ) -> Result<(), NativeOnlineRuntimeError> {
            if self.coordinator.retiring_transport_count() != 0 {
                return Ok(());
            }
            let Some(session) = self.coordinator.take_transport_request() else {
                return Ok(());
            };
            let transport = self.transport_factory.create_transport(
                &self.platform,
                session,
                self.coordinator.config().transport,
                now_ms,
            )?;
            self.coordinator
                .install_control_transport(transport, now_ms)?;
            Ok(())
        }

        fn drain_coordinator_events(
            &mut self,
            _now_ms: u64,
        ) -> Result<(), NativeOnlineRuntimeError> {
            let mut drained = 0;
            while let Some(event) = self.coordinator.poll_event() {
                drained += 1;
                if drained > MAX_NATIVE_ONLINE_EVENTS {
                    return Err(NativeOnlineRuntimeError::Capacity);
                }
                match event {
                    OnlineLobbyEvent::TransportRequested(_) => {}
                    OnlineLobbyEvent::SteamBackendReconnectRecovered { paused_ms } => {
                        if let Some(transaction) = self.manifest_transaction.as_mut()
                            && transaction.stage == ManifestTransactionStage::Activating
                            && let Some(deadline_ms) = transaction.activation_deadline_ms.as_mut()
                        {
                            *deadline_ms = deadline_ms
                                .checked_add(paused_ms)
                                .ok_or(NativeOnlineRuntimeError::Capacity)?;
                        }
                        self.push_event(event)?;
                    }
                    OnlineLobbyEvent::ControlRetrying { user, .. } => {
                        #[cfg(all(feature = "steam-net", not(target_arch = "wasm32"), not(test)))]
                        self.persist_pregame_failure_traces();
                        self.clear_peer_handoffs(user);
                        self.push_event(event)?;
                    }
                    OnlineLobbyEvent::ControlReady { user, .. } => {
                        let ready_handle = self
                            .ticket_exchanges
                            .iter()
                            .flatten()
                            .find(|entry| {
                                entry.lease.remote_user == user
                                    && entry.sent_sequence.is_none()
                                    && self.coordinator.auth_ticket_is_ready(entry.lease)
                            })
                            .map(|exchange| exchange.lease.handle);
                        if let Some(handle) = ready_handle {
                            self.send_ready_ticket(handle, user)?;
                        }
                        self.push_event(event)?;
                    }
                    OnlineLobbyEvent::AuthTicketReady {
                        handle,
                        remote_user,
                    } => {
                        if !self.admission_quiesced {
                            self.send_ready_ticket(handle, remote_user)?;
                        }
                    }
                    OnlineLobbyEvent::AuthenticationRequired { user, reconnect } => {
                        if reconnect && !self.admission_quiesced {
                            self.mark_reconnect_user(user)?;
                        }
                    }
                    OnlineLobbyEvent::PeerAuthenticated {
                        user,
                        peer_id,
                        reconnect,
                    } => {
                        if !self.admission_quiesced {
                            self.install_authenticated_mapping(user, peer_id, reconnect)?;
                            if self.coordinator.uses_control_bootstrap() {
                                self.send_auth_accepted(user)?;
                                self.try_finalize_control_secure(user)?;
                            }
                            if reconnect {
                                self.clear_reconnect_user(user);
                            }
                            self.mark_roster_incoming_validated(user)?;
                            self.push_event(event)?;
                        }
                    }
                    OnlineLobbyEvent::PeerAuthenticationRejected {
                        user,
                        connection,
                        failure,
                    } => {
                        #[cfg(all(feature = "steam-net", not(target_arch = "wasm32"), not(test)))]
                        if !auth_rejection_is_transient(failure) {
                            // Coordinator isolation finalizes and drains the
                            // exact attributed connection before publishing
                            // this event. Persist it before any runtime-level
                            // handoff cleanup can discard that evidence.
                            self.persist_pregame_failure_traces();
                        }
                        if self.authentication_rejection_is_current(user, connection) {
                            if auth_rejection_is_transient(failure) {
                                self.record_pending_steam_setup_retry(
                                    user,
                                    connection,
                                    PendingSteamSetupRetryKind::Direct,
                                    failure,
                                )?;
                            } else {
                                // Invalid identity/ticket/version/game/license,
                                // ban, reuse, and malformed protocol outcomes
                                // remain isolated for this lobby lifetime.
                                self.mark_signal_rejected_user(user)?;
                                self.clear_peer_handoffs(user);
                            }
                        }
                        self.push_event(event)?;
                    }
                    OnlineLobbyEvent::RosterPeerAuthenticated { user } => {
                        // A validation callback may arrive after the retained
                        // lease's first timeout but before the player presses
                        // Retry. Approval supersedes only that roster-lease
                        // failure; leaving it queued would let a stale Retry
                        // retire the newly validated proof.
                        self.clear_pending_steam_setup_retry(
                            user,
                            PendingSteamSetupRetryKind::RosterLease,
                        );
                        self.mark_roster_incoming_validated(user)?;
                        if let Some(ticket) = self
                            .routed_incoming_tickets
                            .iter()
                            .flatten()
                            .find(|ticket| ticket.sender == user)
                            .copied()
                        {
                            self.send_routed_auth_accepted(ticket)?;
                        }
                        self.push_event(event)?;
                    }
                    OnlineLobbyEvent::RosterPeerAuthenticationRejected { user, failure } => {
                        if auth_rejection_is_transient(failure) {
                            self.record_pending_steam_setup_retry(
                                user,
                                None,
                                PendingSteamSetupRetryKind::RosterLease,
                                failure,
                            )?;
                            self.push_event(event)?;
                        } else {
                            self.report_permanent_roster_auth_rejection(user, failure)?;
                        }
                    }
                    OnlineLobbyEvent::EndpointReady {
                        connection,
                        user,
                        peer_id,
                        reconnect,
                    } => {
                        if self.admission_quiesced {
                            continue;
                        }
                        let admitted = self
                            .coordinator
                            .take_endpoint()
                            .ok_or(NativeOnlineRuntimeError::EndpointIdentityMismatch)?;
                        if admitted.connection != connection || admitted.remote_user != user {
                            return Err(NativeOnlineRuntimeError::EndpointIdentityMismatch);
                        }
                        if self.endpoints.len() >= MAX_STEAM_LOBBY_MEMBERS {
                            return Err(NativeOnlineRuntimeError::Capacity);
                        }
                        self.bind_authenticated_connection(user, peer_id, connection)?;
                        self.endpoints.push_back(NativeOnlineEndpoint {
                            peer_id,
                            reconnect,
                            admitted,
                        });
                        self.push_event(event)?;
                    }
                    OnlineLobbyEvent::PeerDisconnected {
                        connection,
                        user,
                        reconnect_allowed,
                        pregame_setup_retry,
                        ..
                    } => {
                        if reconnect_allowed {
                            self.remove_ticket_exchange(user);
                            self.clear_active_peer_transport(user, connection);
                            self.mark_reconnect_user(user)?;
                        } else if pregame_setup_retry
                            && self.coordinator.uses_control_bootstrap()
                            && self
                                .platform
                                .roster()
                                .iter()
                                .flatten()
                                .any(|member| member.user == user)
                        {
                            // This exact pre-game generation exhausted its one
                            // automatic socket retry. Retire every per-peer auth
                            // artifact before exposing a generation-bound Retry;
                            // the lobby and unrelated authority-star links stay.
                            self.clear_peer_handoffs(user);
                            self.record_pending_steam_setup_retry(
                                user,
                                Some(connection),
                                PendingSteamSetupRetryKind::ClosedControl,
                                OnlineFailure {
                                    code: OnlineFailureCode::ConnectionTimedOut,
                                    severity: OnlineFailureSeverity::Recoverable,
                                    recovery: OnlineRecoveryAction::Retry,
                                    detail_code: SteamTransportCloseReason::ConnectTimedOut
                                        .diagnostic_code(),
                                },
                            )?;
                        } else {
                            self.remove_ticket_exchange(user);
                            self.clear_active_peer_transport(user, connection);
                        }
                        self.push_event(event)?;
                    }
                    OnlineLobbyEvent::RosterChanged { live_bindings, .. } => {
                        self.reconcile_live_bindings(live_bindings);
                        self.push_event(event)?;
                    }
                    OnlineLobbyEvent::ManifestCommitted(_) => {
                        self.committed_roster = Some(self.freeze_authenticated_roster()?);
                        self.send_manifest_prepare()?;
                        self.push_event(event)?;
                    }
                    OnlineLobbyEvent::ManifestAborted { reason_code } => {
                        #[cfg(all(feature = "steam-net", not(target_arch = "wasm32"), not(test)))]
                        self.persist_pregame_failure_traces();
                        if self.coordinator.status().role == Some(OnlineLobbyRole::ListenAuthority)
                            && let Some(transaction) = self.manifest_transaction
                        {
                            self.send_manifest_rollback_best_effort(transaction, reason_code);
                        }
                        // Notification is deliberately best-effort. A failed
                        // or replaced frozen generation must never strand the
                        // local transaction or block the Lobby rollback that
                        // the coordinator has already completed.
                        self.committed_roster = None;
                        self.pending_manifest = None;
                        self.retire_manifest_transaction();
                        self.endpoints.clear();
                        self.push_event(event)?;
                    }
                    OnlineLobbyEvent::DropGameplayEndpoints => {
                        self.endpoints.clear();
                        self.push_event(event)?;
                    }
                    OnlineLobbyEvent::ReturnedToLobby { .. } => {
                        self.reset_match_handoff();
                        self.reset_signal_isolation()?;
                        self.local_declaration = self.coordinator.local_declaration();
                        self.push_event(event)?;
                    }
                    OnlineLobbyEvent::Failure(failure) => {
                        #[cfg(all(feature = "steam-net", not(target_arch = "wasm32"), not(test)))]
                        self.persist_pregame_failure_traces();
                        self.runtime_failure = Some(failure);
                        self.push_event(event)?;
                    }
                    _ => self.push_event(event)?,
                }
            }
            Ok(())
        }

        #[cfg(all(feature = "steam-net", not(target_arch = "wasm32"), not(test)))]
        fn persist_pregame_failure_traces(&mut self) {
            let root = resolve_diagnostics_root();
            let archive = AuthorityDiagnosticsArchive::new(root.path);
            self.persist_pregame_failure_traces_to(&archive);
        }

        /// Shared production/test persistence seam. It consumes only bounded,
        /// identity-free transport diagnostics; callers never receive ticket,
        /// payload, persona, address, or Steam identity data.
        #[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
        fn persist_pregame_failure_traces_to(&mut self, archive: &AuthorityDiagnosticsArchive) {
            while let Some(trace) = self.coordinator.take_completed_peer_trace() {
                let Ok(trace) = SteamPregameTraceDiagnostic::from_transport(&trace) else {
                    continue;
                };
                if trace.is_pregame_failure() {
                    let _ = archive.save_steam_pregame_trace(&trace);
                }
            }
        }

        fn push_event(&mut self, event: OnlineLobbyEvent) -> Result<(), NativeOnlineRuntimeError> {
            if self.events.len() >= MAX_NATIVE_ONLINE_EVENTS {
                return Err(NativeOnlineRuntimeError::Capacity);
            }
            self.events.push_back(event);
            Ok(())
        }

        fn reconcile_ticket_exchanges(&mut self) -> Result<(), NativeOnlineRuntimeError> {
            let status = self.coordinator.status();
            let Some(owner) = status.owner else {
                return Ok(());
            };
            let local = self.platform.local_user();
            let mut targets = [None; MAX_STEAM_LOBBY_MEMBERS];
            let mut target_count = 0;
            match status.role {
                Some(OnlineLobbyRole::ListenAuthority) => {
                    for member in self.platform.roster().iter().flatten() {
                        if member.user == local {
                            continue;
                        }
                        let complete = matches!(member.readiness, MemberReadiness::Declared { .. })
                            && member.loadout.is_some();
                        let reconnect = self.is_reconnect_user(member.user);
                        let epoch_ready = member.loadout.is_some_and(|loadout| {
                            self.coordinator
                                .initial_authentication_allowed(member.user, loadout.revision())
                        });
                        if (status.phase == OnlineLobbyPhase::Lobby && complete && epoch_ready)
                            || reconnect
                        {
                            targets[target_count] = Some((
                                member.user,
                                if reconnect {
                                    AdmissionPurpose::Reconnect
                                } else {
                                    AdmissionPurpose::Initial
                                },
                            ));
                            target_count += 1;
                        }
                    }
                }
                Some(OnlineLobbyRole::Client) => {
                    let purpose = if status.phase == OnlineLobbyPhase::Reconnecting {
                        Some(AdmissionPurpose::Reconnect)
                    } else if matches!(
                        status.phase,
                        OnlineLobbyPhase::Lobby
                            | OnlineLobbyPhase::Connecting
                            | OnlineLobbyPhase::Authenticating
                    ) {
                        Some(AdmissionPurpose::Initial)
                    } else {
                        None
                    };
                    let local_coherent = self.local_declaration.is_some();
                    let initial_epoch_ready = local_coherent
                        && self
                            .platform
                            .roster()
                            .iter()
                            .flatten()
                            .find(|member| member.user == owner)
                            .is_some_and(|member| {
                                matches!(member.readiness, MemberReadiness::Declared { .. })
                                    && member.loadout.is_some_and(|loadout| {
                                        self.coordinator.initial_authentication_allowed(
                                            owner,
                                            loadout.revision(),
                                        )
                                    })
                            });
                    if let Some(purpose) = purpose
                        && (purpose == AdmissionPurpose::Reconnect || initial_epoch_ready)
                    {
                        targets[0] = Some((owner, purpose));
                        target_count = 1;
                    }
                }
                None => {}
            }
            for (user, purpose) in targets[..target_count].iter().flatten().copied() {
                if self.is_signal_rejected_user(user) {
                    continue;
                }
                if self.ticket_exchanges.iter().flatten().any(|record| {
                    record.lease.remote_user == user && record.lease.scope.purpose == purpose
                }) {
                    continue;
                }
                self.remove_ticket_exchange(user);
                let lease =
                    self.coordinator
                        .issue_auth_ticket(&mut self.platform, user, purpose)?;
                let slot = self
                    .ticket_exchanges
                    .iter_mut()
                    .find(|slot| slot.is_none())
                    .ok_or(NativeOnlineRuntimeError::Capacity)?;
                *slot = Some(TicketExchange {
                    lease,
                    sent_sequence: None,
                    route: TicketRoute::Direct,
                });
            }
            Ok(())
        }

        fn send_ready_ticket(
            &mut self,
            handle: AuthTicketHandle,
            remote_user: SteamUserId,
        ) -> Result<(), NativeOnlineRuntimeError> {
            let index = self
                .ticket_exchanges
                .iter()
                .position(|record| record.is_some_and(|record| record.lease.handle == handle))
                .ok_or(NativeOnlineRuntimeError::Signal(
                    AuthSignalError::InvalidEnvelope,
                ))?;
            let mut exchange = self.ticket_exchanges[index].ok_or(
                NativeOnlineRuntimeError::Signal(AuthSignalError::InvalidEnvelope),
            )?;
            if exchange.lease.remote_user != remote_user || exchange.sent_sequence.is_some() {
                return Err(AuthSignalError::InvalidEnvelope.into());
            }
            let physical_recipient = match exchange.route {
                TicketRoute::Direct => remote_user,
                TicketRoute::ViaAuthority { authority, .. } => authority,
            };
            if !self
                .coordinator
                .control_is_ready_for_user(physical_recipient)
            {
                return Ok(());
            }
            let ticket = self
                .coordinator
                .take_ready_auth_ticket(exchange.lease)
                .ok_or(AuthSignalError::InvalidEnvelope)?;
            let sender = exchange.lease.sender;
            let scope = exchange.lease.scope;
            if sender.user != self.platform.local_user()
                || scope.lobby
                    != self
                        .coordinator
                        .status()
                        .lobby
                        .ok_or(AuthSignalError::WrongLobby)?
            {
                return Err(AuthSignalError::InvalidEnvelope.into());
            }
            let (ticket_handle, ticket_remote, mut ticket_bytes) = ticket.into_parts();
            if ticket_handle != handle || ticket_remote != remote_user {
                zeroize_auth_signal_bytes(&mut ticket_bytes);
                return Err(AuthSignalError::InvalidEnvelope.into());
            }
            let signal = AuthTicketSignal::new(
                scope.lobby,
                sender.user,
                remote_user,
                sender.peer_id,
                scope.purpose,
                scope.owner_revision,
                sender.revision,
                scope.match_id,
                &ticket_bytes,
            );
            zeroize_auth_signal_bytes(&mut ticket_bytes);
            let signal = signal?;
            let payload = SteamAuthTicketPayload::new(
                SteamControlIdentity::new(scope.lobby, sender.user, remote_user)
                    .map_err(|_| AuthSignalError::InvalidEnvelope)?,
                sender.peer_id,
                scope.purpose,
                scope.owner_revision,
                sender.revision,
                scope.match_id,
                signal.ticket(),
            )
            .map_err(|_| AuthSignalError::InvalidEnvelope)?;
            let message = match exchange.route {
                TicketRoute::Direct => SteamControlMessage::AuthTicket(payload),
                TicketRoute::ViaAuthority {
                    auth_epoch,
                    ticket_id,
                    authority,
                } => SteamControlMessage::RoutedAuthTicket {
                    identity: SteamControlIdentity::new(scope.lobby, sender.user, authority)
                        .map_err(|_| AuthSignalError::InvalidEnvelope)?,
                    auth_epoch,
                    ticket_id,
                    ticket: payload,
                },
            };
            let sequence = self
                .coordinator
                .queue_control_for_user(physical_recipient, message)?;
            exchange.sent_sequence = Some(sequence);
            self.ticket_exchanges[index] = Some(exchange);
            Ok(())
        }

        fn flush_ready_tickets(&mut self) -> Result<(), NativeOnlineRuntimeError> {
            let mut ready = [None; MAX_STEAM_LOBBY_MEMBERS];
            let mut count = 0_usize;
            for exchange in self.ticket_exchanges.iter().flatten() {
                if exchange.sent_sequence.is_none()
                    && self.coordinator.auth_ticket_is_ready(exchange.lease)
                    && self
                        .coordinator
                        .control_is_ready_for_user(match exchange.route {
                            TicketRoute::Direct => exchange.lease.remote_user,
                            TicketRoute::ViaAuthority { authority, .. } => authority,
                        })
                {
                    ready[count] = Some((exchange.lease.handle, exchange.lease.remote_user));
                    count += 1;
                }
            }
            for (handle, user) in ready[..count].iter().flatten().copied() {
                self.send_ready_ticket(handle, user)?;
            }
            Ok(())
        }

        #[cfg(test)]
        fn consume_session_hello(
            &self,
            source: SteamUserId,
            signal: AuthSessionHelloSignal,
        ) -> Result<(), NativeOnlineRuntimeError> {
            if self.coordinator.status().lobby != Some(signal.lobby) {
                return Err(AuthSignalError::WrongLobby.into());
            }
            if signal.recipient != self.platform.local_user() {
                return Err(AuthSignalError::WrongRecipient.into());
            }
            if signal.sender != source {
                return Err(AuthSignalError::SenderMismatch.into());
            }
            Ok(())
        }

        fn consume_ticket_signal(
            &mut self,
            source: SteamUserId,
            signal: AuthTicketSignal,
            now_ms: u64,
        ) -> Result<bool, NativeOnlineRuntimeError> {
            let status = self.coordinator.status();
            if status.lobby != Some(signal.lobby) {
                return Err(AuthSignalError::WrongLobby.into());
            }
            if signal.recipient != self.platform.local_user() {
                return Err(AuthSignalError::WrongRecipient.into());
            }
            if signal.sender != source {
                return Err(AuthSignalError::SenderMismatch.into());
            }
            let sender = AuthPeerLease {
                user: source,
                peer_id: signal.sender_peer_id,
                revision: signal.sender_revision,
            };
            let scope = AuthSignalScope {
                lobby: signal.lobby,
                purpose: signal.purpose,
                owner_revision: signal.owner_revision,
                match_id: signal.match_id,
            };
            match self
                .coordinator
                .classify_auth_signal_lease(&self.platform, sender, scope)
            {
                Ok(AuthSignalLeaseStatus::Current) => {}
                Ok(AuthSignalLeaseStatus::Stale) => return Ok(false),
                Err(OnlineLobbyError::MissingPeerBinding(_)) => {
                    return Err(AuthSignalError::PeerNotInLobby.into());
                }
                Err(
                    OnlineLobbyError::PeerIdentityMismatch | OnlineLobbyError::DuplicatePeerBinding,
                ) => return Err(AuthSignalError::InvalidIdentity.into()),
                Err(_) => return Err(AuthSignalError::UnexpectedPurpose.into()),
            }
            let purpose_allowed = match signal.purpose {
                AdmissionPurpose::Initial => matches!(
                    status.phase,
                    OnlineLobbyPhase::Lobby
                        | OnlineLobbyPhase::Connecting
                        | OnlineLobbyPhase::Authenticating
                ),
                AdmissionPurpose::Reconnect => matches!(
                    status.phase,
                    OnlineLobbyPhase::Countdown
                        | OnlineLobbyPhase::Fighting
                        | OnlineLobbyPhase::Reconnecting
                ),
            };
            if !purpose_allowed {
                return Err(AuthSignalError::UnexpectedPurpose.into());
            }
            match self.coordinator.begin_peer_authentication(
                &mut self.platform,
                source,
                signal.sender_peer_id,
                signal.ticket(),
                signal.purpose,
                now_ms,
            ) {
                Ok(()) => {}
                Err(OnlineLobbyError::Steam(error)) if immediate_auth_error_is_transient(error) => {
                    self.record_pending_steam_setup_retry(
                        source,
                        self.coordinator.active_connection_for_user(source),
                        PendingSteamSetupRetryKind::Direct,
                        OnlineFailure::from_steam(error),
                    )?;
                    return Ok(false);
                }
                Err(error) => project_ticket_admission_result(Err(error))?,
            }
            Ok(true)
        }

        fn remember_remote_ticket_sequence(
            &mut self,
            user: SteamUserId,
            sequence: u32,
        ) -> Result<(), NativeOnlineRuntimeError> {
            if let Some((_, existing)) = self
                .remote_ticket_sequences
                .iter()
                .flatten()
                .find(|(candidate, _)| *candidate == user)
            {
                return if *existing == sequence {
                    Ok(())
                } else {
                    Err(AuthSignalError::InvalidEnvelope.into())
                };
            }
            let slot = self
                .remote_ticket_sequences
                .iter_mut()
                .find(|slot| slot.is_none())
                .ok_or(NativeOnlineRuntimeError::Capacity)?;
            *slot = Some((user, sequence));
            Ok(())
        }

        fn send_auth_accepted(
            &mut self,
            user: SteamUserId,
        ) -> Result<(), NativeOnlineRuntimeError> {
            let ticket_sequence = self
                .remote_ticket_sequences
                .iter()
                .flatten()
                .find(|(candidate, _)| *candidate == user)
                .map(|(_, sequence)| *sequence)
                .ok_or(AuthSignalError::InvalidEnvelope)?;
            let lobby = self
                .coordinator
                .status()
                .lobby
                .ok_or(AuthSignalError::WrongLobby)?;
            let identity = SteamControlIdentity::new(lobby, self.platform.local_user(), user)
                .map_err(|_| AuthSignalError::InvalidIdentity)?;
            self.coordinator.queue_control_for_user(
                user,
                SteamControlMessage::AuthAccepted {
                    identity,
                    ticket_sequence,
                },
            )?;
            Ok(())
        }

        fn consume_auth_accepted(
            &mut self,
            source: SteamUserId,
            identity: SteamControlIdentity,
            ticket_sequence: u32,
        ) -> Result<(), NativeOnlineRuntimeError> {
            let status = self.coordinator.status();
            if status.lobby != Some(identity.lobby)
                || identity.sender != source
                || identity.recipient != self.platform.local_user()
            {
                return Err(AuthSignalError::InvalidEnvelope.into());
            }
            let valid = self.ticket_exchanges.iter().flatten().any(|exchange| {
                exchange.lease.remote_user == source
                    && exchange.sent_sequence == Some(ticket_sequence)
            });
            if !valid {
                return Err(AuthSignalError::InvalidEnvelope.into());
            }
            if !self.local_ticket_accepted.contains(&Some(source)) {
                let slot = self
                    .local_ticket_accepted
                    .iter_mut()
                    .find(|slot| slot.is_none())
                    .ok_or(NativeOnlineRuntimeError::Capacity)?;
                *slot = Some(source);
            }
            self.try_finalize_control_secure(source)
        }

        fn try_finalize_control_secure(
            &mut self,
            user: SteamUserId,
        ) -> Result<(), NativeOnlineRuntimeError> {
            let remote_validated = self
                .remote_ticket_sequences
                .iter()
                .flatten()
                .any(|(candidate, _)| *candidate == user)
                && self
                    .authenticated
                    .iter()
                    .flatten()
                    .any(|mapping| mapping.user == user);
            if remote_validated && self.local_ticket_accepted.contains(&Some(user)) {
                self.coordinator.finalize_control_secure(user)?;
                self.clear_pending_steam_setup_retry(
                    user,
                    PendingSteamSetupRetryKind::ClosedControl,
                );
            }
            Ok(())
        }

        fn current_account_roster_identity(
            &self,
        ) -> Result<
            (u64, u8, [Option<SteamUserId>; MAX_STEAM_LOBBY_MEMBERS]),
            NativeOnlineRuntimeError,
        > {
            let lobby = self
                .coordinator
                .status()
                .lobby
                .ok_or(AuthSignalError::WrongLobby)?;
            let mut users = [None; MAX_STEAM_LOBBY_MEMBERS];
            let mut count = 0_usize;
            for member in self.platform.roster().iter().flatten() {
                if !matches!(member.readiness, MemberReadiness::Declared { .. })
                    || member.loadout.is_none()
                {
                    return Err(OnlineLobbyError::ManifestDeclarationsPending.into());
                }
                users[count] = Some(member.user);
                count += 1;
            }
            if !(2..=MAX_STEAM_LOBBY_MEMBERS).contains(&count) {
                return Err(NativeOnlineRuntimeError::InvalidAuthenticatedRoster);
            }
            users[..count]
                .sort_unstable_by_key(|user| user.expect("packed Steam roster identity").get());
            let mut hash = 0xcbf2_9ce4_8422_2325_u64;
            for byte in lobby.get().to_le_bytes().into_iter().chain([count as u8]) {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
            for user in users[..count].iter().flatten() {
                for byte in user.get().to_le_bytes() {
                    hash ^= u64::from(byte);
                    hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
                }
            }
            Ok((hash, count as u8, users))
        }

        fn routed_ticket_id(
            &self,
            sender: SteamUserId,
            recipient: SteamUserId,
        ) -> Result<u32, NativeOnlineRuntimeError> {
            let (_, count, users) = self.current_account_roster_identity()?;
            let sender_index = users[..usize::from(count)]
                .iter()
                .position(|user| *user == Some(sender))
                .ok_or(AuthSignalError::InvalidIdentity)?;
            let recipient_index = users[..usize::from(count)]
                .iter()
                .position(|user| *user == Some(recipient))
                .ok_or(AuthSignalError::InvalidIdentity)?;
            if sender == recipient {
                return Err(AuthSignalError::InvalidIdentity.into());
            }
            Ok(((sender_index as u32 + 1) << 16) | (recipient_index as u32 + 1))
        }

        fn roster_auth_abort_transaction(
            epoch: AccountAuthEpoch,
        ) -> Result<ManifestTransactionId, NativeOnlineRuntimeError> {
            ManifestTransactionId::new(epoch.get())
                .map_err(|_| AuthSignalError::InvalidEnvelope.into())
        }

        fn roster_auth_abort_epoch(
            transaction: Option<ManifestTransactionId>,
        ) -> Result<AccountAuthEpoch, NativeOnlineRuntimeError> {
            let transaction = transaction.ok_or(AuthSignalError::InvalidEnvelope)?;
            AccountAuthEpoch::new(transaction.get())
                .map_err(|_| AuthSignalError::InvalidEnvelope.into())
        }

        fn permanent_roster_auth_abort_code(
            &self,
            rejected_user: SteamUserId,
        ) -> Result<u16, NativeOnlineRuntimeError> {
            let (_, member_count, users) = self.current_account_roster_identity()?;
            let ordinal = users[..usize::from(member_count)]
                .iter()
                .position(|user| *user == Some(rejected_user))
                .ok_or(AuthSignalError::InvalidIdentity)?;
            AUTH_REJECT_ROSTER_ABORT_CODE_BASE
                .checked_add(
                    u16::try_from(ordinal + 1).map_err(|_| NativeOnlineRuntimeError::Capacity)?,
                )
                .ok_or(NativeOnlineRuntimeError::Capacity)
        }

        fn permanent_roster_auth_abort_user(
            &self,
            reason_code: u16,
        ) -> Result<SteamUserId, NativeOnlineRuntimeError> {
            let ordinal = reason_code
                .checked_sub(AUTH_REJECT_ROSTER_ABORT_CODE_BASE)
                .filter(|ordinal| *ordinal != 0)
                .ok_or(AuthSignalError::UnexpectedPurpose)?;
            let (_, member_count, users) = self.current_account_roster_identity()?;
            if ordinal > u16::from(member_count) {
                return Err(AuthSignalError::InvalidIdentity.into());
            }
            users[usize::from(ordinal - 1)].ok_or(AuthSignalError::InvalidIdentity.into())
        }

        fn propagated_permanent_roster_auth_failure(reason_code: u16) -> OnlineFailure {
            OnlineFailure {
                code: OnlineFailureCode::AuthenticationFailed,
                severity: OnlineFailureSeverity::Fatal,
                recovery: OnlineRecoveryAction::ReturnToMenu,
                detail_code: reason_code,
            }
        }

        /// Reports a permanent client-to-client ticket verdict over the only
        /// authenticated physical route: the client-to-authority control
        /// socket. The authority will validate the roster epoch and fan the
        /// bounded rejection out to every participant before retiring it.
        fn report_permanent_roster_auth_rejection(
            &mut self,
            rejected_user: SteamUserId,
            failure: OnlineFailure,
        ) -> Result<(), NativeOnlineRuntimeError> {
            let status = self.coordinator.status();
            let active = self.roster_auth.ok_or(AuthSignalError::UnexpectedPurpose)?;
            let owner = status.owner.ok_or(AuthSignalError::InvalidIdentity)?;
            let local = self.platform.local_user();
            if status.phase != OnlineLobbyPhase::Lobby
                || status.role != Some(OnlineLobbyRole::Client)
                || owner == local
                || rejected_user == local
                || rejected_user == owner
                || active.participant(rejected_user).is_none()
            {
                return Err(AuthSignalError::UnexpectedPurpose.into());
            }
            let identity = SteamControlIdentity::new(
                status.lobby.ok_or(AuthSignalError::WrongLobby)?,
                local,
                owner,
            )
            .map_err(|_| AuthSignalError::InvalidIdentity)?;
            let reason_code = self.permanent_roster_auth_abort_code(rejected_user)?;
            self.coordinator.queue_control_for_user(
                owner,
                SteamControlMessage::Abort {
                    identity,
                    transaction: Some(Self::roster_auth_abort_transaction(active.epoch)?),
                    code: reason_code,
                    permanent: true,
                },
            )?;
            self.apply_permanent_roster_auth_rejection(rejected_user, failure)
        }

        fn apply_permanent_roster_auth_rejection(
            &mut self,
            rejected_user: SteamUserId,
            failure: OnlineFailure,
        ) -> Result<(), NativeOnlineRuntimeError> {
            let newly_rejected = !self.is_signal_rejected_user(rejected_user);
            self.mark_signal_rejected_user(rejected_user)?;
            self.coordinator
                .revoke_remote_account_verification(rejected_user);
            self.retire_roster_auth_state();
            if newly_rejected {
                self.push_event(OnlineLobbyEvent::RosterPeerAuthenticationRejected {
                    user: rejected_user,
                    failure,
                })?;
                self.push_event(OnlineLobbyEvent::Failure(failure))?;
            }
            Ok(())
        }

        fn reconcile_roster_authentication(&mut self) -> Result<(), NativeOnlineRuntimeError> {
            let status = self.coordinator.status();
            if status.phase != OnlineLobbyPhase::Lobby || status.role.is_none() {
                return Ok(());
            }
            let Ok((roster_hash, member_count, users)) = self.current_account_roster_identity()
            else {
                return Ok(());
            };
            if let Some(active) = self.roster_auth
                && (active.roster_hash != roster_hash || active.member_count != member_count)
            {
                self.retire_roster_auth_state();
            }
            if self.roster_auth.is_none()
                && status.role == Some(OnlineLobbyRole::ListenAuthority)
                && status.secure_remote_peers == status.required_remote_peers
                && self.signal_rejected_users.iter().all(Option::is_none)
            {
                let epoch = AccountAuthEpoch::new(self.next_account_auth_epoch)
                    .map_err(|_| AuthSignalError::InvalidEnvelope)?;
                self.next_account_auth_epoch = self
                    .next_account_auth_epoch
                    .checked_add(1)
                    .ok_or(NativeOnlineRuntimeError::Capacity)?;
                let local = self.platform.local_user();
                let lobby = status.lobby.ok_or(AuthSignalError::WrongLobby)?;
                let mut participants = [None; MAX_STEAM_LOBBY_MEMBERS];
                let mut participant_count = 0_usize;
                for user in users[..usize::from(member_count)].iter().flatten().copied() {
                    if user == local {
                        continue;
                    }
                    let direct_validated = self.coordinator.remote_account_is_verified(user)
                        && self.local_ticket_accepted.contains(&Some(user));
                    participants[participant_count] = Some(RosterAuthParticipant {
                        user,
                        prepare_accepted: false,
                        incoming_validated: direct_validated,
                        outgoing_accepted: direct_validated,
                        process_complete: false,
                    });
                    participant_count += 1;
                }
                self.roster_auth = Some(RosterAuthTransaction {
                    epoch,
                    roster_hash,
                    member_count,
                    participants,
                    prepared: true,
                    local_complete_sent: true,
                    globally_complete: false,
                });
                for participant in participants.iter().flatten() {
                    let identity = SteamControlIdentity::new(lobby, local, participant.user)
                        .map_err(|_| AuthSignalError::InvalidIdentity)?;
                    self.coordinator.queue_control_for_user(
                        participant.user,
                        SteamControlMessage::RosterPrepare {
                            identity,
                            auth_epoch: epoch,
                            roster_hash,
                            member_count,
                        },
                    )?;
                }
            }

            let Some(active_snapshot) = self.roster_auth else {
                return Ok(());
            };
            let local = self.platform.local_user();
            let owner = status.owner.ok_or(AuthSignalError::InvalidIdentity)?;
            if status.role == Some(OnlineLobbyRole::Client) && active_snapshot.prepared {
                for participant in active_snapshot.participants.iter().flatten() {
                    if participant.user == owner
                        || self.ticket_exchanges.iter().flatten().any(|exchange| {
                            exchange.lease.remote_user == participant.user
                                && matches!(
                                    exchange.route,
                                    TicketRoute::ViaAuthority { auth_epoch, .. }
                                        if auth_epoch == active_snapshot.epoch
                                )
                        })
                    {
                        continue;
                    }
                    let ticket_id = self.routed_ticket_id(local, participant.user)?;
                    let lease = self.coordinator.issue_auth_ticket(
                        &mut self.platform,
                        participant.user,
                        AdmissionPurpose::Initial,
                    )?;
                    let slot = self
                        .ticket_exchanges
                        .iter_mut()
                        .find(|slot| slot.is_none())
                        .ok_or(NativeOnlineRuntimeError::Capacity)?;
                    *slot = Some(TicketExchange {
                        lease,
                        sent_sequence: None,
                        route: TicketRoute::ViaAuthority {
                            auth_epoch: active_snapshot.epoch,
                            ticket_id,
                            authority: owner,
                        },
                    });
                }
            }

            let participant_users = active_snapshot.participants;
            for participant in participant_users.iter().flatten() {
                if self
                    .platform
                    .roster_authentication_is_validated(participant.user)
                {
                    self.mark_roster_incoming_validated(participant.user)?;
                }
            }
            if status.role == Some(OnlineLobbyRole::Client) {
                let should_send = self.roster_auth.as_ref().is_some_and(|active| {
                    active.local_process_complete() && !active.local_complete_sent
                });
                if should_send {
                    let active = self.roster_auth.expect("roster auth remains active");
                    let identity = SteamControlIdentity::new(
                        status.lobby.ok_or(AuthSignalError::WrongLobby)?,
                        local,
                        owner,
                    )
                    .map_err(|_| AuthSignalError::InvalidIdentity)?;
                    self.coordinator.queue_control_for_user(
                        owner,
                        SteamControlMessage::RosterAuthComplete {
                            identity,
                            auth_epoch: active.epoch,
                            roster_hash: active.roster_hash,
                        },
                    )?;
                    self.roster_auth
                        .as_mut()
                        .expect("roster auth remains active")
                        .local_complete_sent = true;
                }
            } else {
                self.try_finish_authority_roster_auth()?;
            }
            Ok(())
        }

        fn consume_roster_prepare(
            &mut self,
            source: SteamUserId,
            connection: SteamConnectionId,
            identity: SteamControlIdentity,
            auth_epoch: AccountAuthEpoch,
            roster_hash: u64,
            member_count: u8,
        ) -> Result<(), NativeOnlineRuntimeError> {
            let status = self.coordinator.status();
            if self.retired_account_auth_epoch == Some(auth_epoch) {
                return Ok(());
            }
            let (expected_hash, expected_count, users) = self.current_account_roster_identity()?;
            if status.role != Some(OnlineLobbyRole::Client)
                || status.owner != Some(source)
                || status.lobby != Some(identity.lobby)
                || identity.sender != source
                || identity.recipient != self.platform.local_user()
                || self.coordinator.active_connection_for_user(source) != Some(connection)
                || roster_hash != expected_hash
                || member_count != expected_count
                || status.secure_remote_peers != status.required_remote_peers
            {
                return Err(AuthSignalError::InvalidIdentity.into());
            }
            if let Some(active) = self.roster_auth {
                return if active.epoch == auth_epoch
                    && active.roster_hash == roster_hash
                    && active.member_count == member_count
                {
                    Ok(())
                } else {
                    Err(AuthSignalError::InvalidEnvelope.into())
                };
            }
            let local = self.platform.local_user();
            let mut participants = [None; MAX_STEAM_LOBBY_MEMBERS];
            let mut count = 0_usize;
            for user in users[..usize::from(member_count)].iter().flatten().copied() {
                if user == local {
                    continue;
                }
                let direct = user == source
                    && self.coordinator.remote_account_is_verified(user)
                    && self.local_ticket_accepted.contains(&Some(user));
                participants[count] = Some(RosterAuthParticipant {
                    user,
                    prepare_accepted: user == source,
                    incoming_validated: direct,
                    outgoing_accepted: direct,
                    process_complete: false,
                });
                count += 1;
            }
            self.roster_auth = Some(RosterAuthTransaction {
                epoch: auth_epoch,
                roster_hash,
                member_count,
                participants,
                prepared: true,
                local_complete_sent: false,
                globally_complete: false,
            });
            let accepted_identity = SteamControlIdentity::new(identity.lobby, local, source)
                .map_err(|_| AuthSignalError::InvalidIdentity)?;
            self.coordinator.queue_control_for_user(
                source,
                SteamControlMessage::RosterAccepted {
                    identity: accepted_identity,
                    auth_epoch,
                    roster_hash,
                    member_count,
                },
            )?;
            Ok(())
        }

        fn consume_roster_accepted(
            &mut self,
            source: SteamUserId,
            connection: SteamConnectionId,
            identity: SteamControlIdentity,
            auth_epoch: AccountAuthEpoch,
            roster_hash: u64,
            member_count: u8,
        ) -> Result<(), NativeOnlineRuntimeError> {
            if self.retired_account_auth_epoch == Some(auth_epoch) {
                return Ok(());
            }
            let status = self.coordinator.status();
            let active = self
                .roster_auth
                .as_mut()
                .ok_or(AuthSignalError::InvalidEnvelope)?;
            if status.role != Some(OnlineLobbyRole::ListenAuthority)
                || status.lobby != Some(identity.lobby)
                || identity.sender != source
                || identity.recipient != self.platform.local_user()
                || active.epoch != auth_epoch
                || active.roster_hash != roster_hash
                || active.member_count != member_count
                || self.coordinator.active_connection_for_user(source) != Some(connection)
            {
                return Err(AuthSignalError::InvalidEnvelope.into());
            }
            active
                .participant_mut(source)
                .ok_or(AuthSignalError::InvalidIdentity)?
                .prepare_accepted = true;
            Ok(())
        }

        #[allow(clippy::too_many_arguments)]
        fn consume_routed_auth_ticket(
            &mut self,
            source: SteamUserId,
            identity: SteamControlIdentity,
            auth_epoch: AccountAuthEpoch,
            ticket_id: u32,
            ticket: SteamAuthTicketPayload,
            now_ms: u64,
        ) -> Result<(), NativeOnlineRuntimeError> {
            if self.retired_account_auth_epoch == Some(auth_epoch) {
                return Ok(());
            }
            let status = self.coordinator.status();
            let active = self
                .roster_auth
                .as_ref()
                .ok_or(AuthSignalError::InvalidEnvelope)?;
            if status.lobby != Some(identity.lobby)
                || identity.sender != source
                || identity.recipient != self.platform.local_user()
                || active.epoch != auth_epoch
                || ticket.identity.lobby != identity.lobby
                || ticket.purpose != AdmissionPurpose::Initial
                || ticket.match_id.is_some()
                || ticket_id
                    != self.routed_ticket_id(ticket.identity.sender, ticket.identity.recipient)?
            {
                return Err(AuthSignalError::InvalidEnvelope.into());
            }
            match status.role {
                Some(OnlineLobbyRole::ListenAuthority) => {
                    if ticket.identity.sender != source
                        || ticket.identity.recipient == self.platform.local_user()
                        || active
                            .participant(source)
                            .is_none_or(|participant| !participant.prepare_accepted)
                        || active.participant(ticket.identity.recipient).is_none()
                    {
                        return Err(AuthSignalError::InvalidIdentity.into());
                    }
                    let recipient = ticket.identity.recipient;
                    let forward_identity = SteamControlIdentity::new(
                        identity.lobby,
                        self.platform.local_user(),
                        recipient,
                    )
                    .map_err(|_| AuthSignalError::InvalidIdentity)?;
                    self.coordinator.queue_control_for_user(
                        recipient,
                        SteamControlMessage::RoutedAuthTicket {
                            identity: forward_identity,
                            auth_epoch,
                            ticket_id,
                            ticket,
                        },
                    )?;
                }
                Some(OnlineLobbyRole::Client) => {
                    if status.owner != Some(source)
                        || ticket.identity.recipient != self.platform.local_user()
                        || ticket.identity.sender == source
                        || active.participant(ticket.identity.sender).is_none()
                    {
                        return Err(AuthSignalError::InvalidIdentity.into());
                    }
                    let logical_sender = ticket.identity.sender;
                    if let Some(existing) = self
                        .routed_incoming_tickets
                        .iter()
                        .flatten()
                        .find(|record| record.sender == logical_sender)
                    {
                        if existing.ticket_id != ticket_id {
                            return Err(AuthSignalError::InvalidEnvelope.into());
                        }
                        return Ok(());
                    }
                    let slot = self
                        .routed_incoming_tickets
                        .iter_mut()
                        .find(|slot| slot.is_none())
                        .ok_or(NativeOnlineRuntimeError::Capacity)?;
                    *slot = Some(RoutedIncomingTicket {
                        sender: logical_sender,
                        ticket_id,
                    });
                    if let Err(error) = self.platform.begin_roster_authentication(
                        identity.lobby,
                        logical_sender,
                        ticket.ticket(),
                        now_ms,
                    ) {
                        let failure = OnlineFailure::from_steam(error);
                        if immediate_auth_error_is_transient(error) {
                            // No native session exists after an immediate
                            // failure, so this route must receive a fresh
                            // recipient-bound one-use ticket on Retry.
                            self.record_pending_steam_setup_retry(
                                logical_sender,
                                None,
                                PendingSteamSetupRetryKind::RosterLease,
                                failure,
                            )?;
                            self.push_event(OnlineLobbyEvent::RosterPeerAuthenticationRejected {
                                user: logical_sender,
                                failure,
                            })?;
                        } else {
                            self.report_permanent_roster_auth_rejection(logical_sender, failure)?;
                        }
                    }
                }
                None => return Err(AuthSignalError::UnexpectedPurpose.into()),
            }
            Ok(())
        }

        #[allow(clippy::too_many_arguments)]
        fn consume_routed_auth_accepted(
            &mut self,
            source: SteamUserId,
            identity: SteamControlIdentity,
            auth_epoch: AccountAuthEpoch,
            ticket_id: u32,
            ticket_sender: SteamUserId,
            ticket_recipient: SteamUserId,
        ) -> Result<(), NativeOnlineRuntimeError> {
            if self.retired_account_auth_epoch == Some(auth_epoch) {
                return Ok(());
            }
            let status = self.coordinator.status();
            let active = self
                .roster_auth
                .as_ref()
                .ok_or(AuthSignalError::InvalidEnvelope)?;
            if status.lobby != Some(identity.lobby)
                || identity.sender != source
                || identity.recipient != self.platform.local_user()
                || active.epoch != auth_epoch
                || ticket_id != self.routed_ticket_id(ticket_sender, ticket_recipient)?
            {
                return Err(AuthSignalError::InvalidEnvelope.into());
            }
            match status.role {
                Some(OnlineLobbyRole::ListenAuthority) => {
                    if source != ticket_recipient
                        || active.participant(ticket_sender).is_none()
                        || active.participant(ticket_recipient).is_none()
                    {
                        return Err(AuthSignalError::InvalidIdentity.into());
                    }
                    let forward_identity = SteamControlIdentity::new(
                        identity.lobby,
                        self.platform.local_user(),
                        ticket_sender,
                    )
                    .map_err(|_| AuthSignalError::InvalidIdentity)?;
                    self.coordinator.queue_control_for_user(
                        ticket_sender,
                        SteamControlMessage::RoutedAuthAccepted {
                            identity: forward_identity,
                            auth_epoch,
                            ticket_id,
                            ticket_sender,
                            ticket_recipient,
                        },
                    )?;
                }
                Some(OnlineLobbyRole::Client) => {
                    if status.owner != Some(source) || ticket_sender != self.platform.local_user() {
                        return Err(AuthSignalError::InvalidIdentity.into());
                    }
                    let valid = self.ticket_exchanges.iter().flatten().any(|exchange| {
                        exchange.lease.remote_user == ticket_recipient
                            && matches!(
                                exchange.route,
                                TicketRoute::ViaAuthority {
                                    auth_epoch: epoch,
                                    ticket_id: id,
                                    ..
                                } if epoch == auth_epoch && id == ticket_id
                            )
                    });
                    if !valid {
                        return Err(AuthSignalError::InvalidEnvelope.into());
                    }
                    self.roster_auth
                        .as_mut()
                        .and_then(|active| active.participant_mut(ticket_recipient))
                        .ok_or(AuthSignalError::InvalidIdentity)?
                        .outgoing_accepted = true;
                }
                None => return Err(AuthSignalError::UnexpectedPurpose.into()),
            }
            Ok(())
        }

        fn send_routed_auth_accepted(
            &mut self,
            ticket: RoutedIncomingTicket,
        ) -> Result<(), NativeOnlineRuntimeError> {
            let status = self.coordinator.status();
            let owner = status.owner.ok_or(AuthSignalError::InvalidIdentity)?;
            let active = self.roster_auth.ok_or(AuthSignalError::InvalidEnvelope)?;
            let local = self.platform.local_user();
            let identity = SteamControlIdentity::new(
                status.lobby.ok_or(AuthSignalError::WrongLobby)?,
                local,
                owner,
            )
            .map_err(|_| AuthSignalError::InvalidIdentity)?;
            self.coordinator.queue_control_for_user(
                owner,
                SteamControlMessage::RoutedAuthAccepted {
                    identity,
                    auth_epoch: active.epoch,
                    ticket_id: ticket.ticket_id,
                    ticket_sender: ticket.sender,
                    ticket_recipient: local,
                },
            )?;
            Ok(())
        }

        fn mark_roster_incoming_validated(
            &mut self,
            user: SteamUserId,
        ) -> Result<(), NativeOnlineRuntimeError> {
            if let Some(active) = self.roster_auth.as_mut()
                && let Some(participant) = active.participant_mut(user)
            {
                participant.incoming_validated = true;
                self.coordinator.record_remote_account_verified(user)?;
            }
            Ok(())
        }

        fn consume_roster_auth_complete(
            &mut self,
            source: SteamUserId,
            identity: SteamControlIdentity,
            auth_epoch: AccountAuthEpoch,
            roster_hash: u64,
        ) -> Result<(), NativeOnlineRuntimeError> {
            let status = self.coordinator.status();
            if self.retired_account_auth_epoch == Some(auth_epoch) {
                return Ok(());
            }
            let active = self
                .roster_auth
                .as_mut()
                .ok_or(AuthSignalError::InvalidEnvelope)?;
            if status.lobby != Some(identity.lobby)
                || identity.sender != source
                || identity.recipient != self.platform.local_user()
                || active.epoch != auth_epoch
                || active.roster_hash != roster_hash
            {
                return Err(AuthSignalError::InvalidEnvelope.into());
            }
            match status.role {
                Some(OnlineLobbyRole::ListenAuthority) => {
                    let participant = active
                        .participant_mut(source)
                        .ok_or(AuthSignalError::InvalidIdentity)?;
                    if !participant.prepare_accepted {
                        return Err(AuthSignalError::UnexpectedPurpose.into());
                    }
                    participant.process_complete = true;
                    self.try_finish_authority_roster_auth()?;
                }
                Some(OnlineLobbyRole::Client) if status.owner == Some(source) => {
                    if !active.local_process_complete() || !active.local_complete_sent {
                        return Err(AuthSignalError::UnexpectedPurpose.into());
                    }
                    active.globally_complete = true;
                }
                _ => return Err(AuthSignalError::UnexpectedPurpose.into()),
            }
            Ok(())
        }

        fn try_finish_authority_roster_auth(&mut self) -> Result<(), NativeOnlineRuntimeError> {
            let status = self.coordinator.status();
            if status.role != Some(OnlineLobbyRole::ListenAuthority) {
                return Ok(());
            }
            let Some(active) = self.roster_auth.as_mut() else {
                return Ok(());
            };
            if active.globally_complete
                || !active
                    .participants
                    .iter()
                    .flatten()
                    .all(|participant| participant.process_complete)
            {
                return Ok(());
            }
            active.globally_complete = true;
            let snapshot = *active;
            let lobby = status.lobby.ok_or(AuthSignalError::WrongLobby)?;
            let local = self.platform.local_user();
            for participant in snapshot.participants.iter().flatten() {
                let identity = SteamControlIdentity::new(lobby, local, participant.user)
                    .map_err(|_| AuthSignalError::InvalidIdentity)?;
                self.coordinator.queue_control_for_user(
                    participant.user,
                    SteamControlMessage::RosterAuthComplete {
                        identity,
                        auth_epoch: snapshot.epoch,
                        roster_hash: snapshot.roster_hash,
                    },
                )?;
            }
            Ok(())
        }

        fn retire_roster_auth_state(&mut self) {
            if let Some(active) = self.roster_auth.take() {
                self.retired_account_auth_epoch = Some(active.epoch);
            }
            let routed_incoming_users = self
                .routed_incoming_tickets
                .map(|ticket| ticket.map(|ticket| ticket.sender));
            self.routed_incoming_tickets = [None; MAX_STEAM_LOBBY_MEMBERS];
            let routed_handles: Vec<_> = self
                .ticket_exchanges
                .iter()
                .flatten()
                .filter(|exchange| matches!(exchange.route, TicketRoute::ViaAuthority { .. }))
                .map(|exchange| exchange.lease.handle)
                .collect();
            for handle in routed_handles {
                let _ = self
                    .coordinator
                    .cancel_auth_ticket(&mut self.platform, handle);
            }
            self.ticket_exchanges
                .iter_mut()
                .filter(|exchange| {
                    exchange.is_some_and(|exchange| {
                        matches!(exchange.route, TicketRoute::ViaAuthority { .. })
                    })
                })
                .for_each(|exchange| *exchange = None);
            // Only routed sessions belong to this roster transaction. Direct
            // authority-star validation remains valid and must not be revoked
            // when one participant asks to rerun the routed exchange.
            for user in routed_incoming_users.iter().flatten().copied() {
                let _ = self.platform.end_roster_authentication(user);
                self.coordinator.revoke_remote_account_verification(user);
            }
            self.clear_pending_roster_setup_retries();
        }

        fn retire_manifest_transaction(&mut self) {
            if let Some(active) = self.manifest_transaction.take() {
                self.retired_manifest_transaction = Some(active.id);
            }
        }

        fn send_manifest_rollback_best_effort(
            &mut self,
            transaction: RuntimeManifestTransaction,
            reason_code: u16,
        ) {
            let status = self.coordinator.status();
            if status.role != Some(OnlineLobbyRole::ListenAuthority) {
                return;
            }
            let Some(lobby) = status.lobby else {
                return;
            };
            let local = self.platform.local_user();
            for participant in transaction.participants.iter().flatten() {
                if self
                    .coordinator
                    .active_connection_for_user(participant.user)
                    != Some(participant.connection)
                {
                    continue;
                }
                let Ok(identity) = SteamControlIdentity::new(lobby, local, participant.user) else {
                    continue;
                };
                let message = match transaction.stage {
                    ManifestTransactionStage::Preparing | ManifestTransactionStage::Committing => {
                        SteamControlMessage::Abort {
                            identity,
                            transaction: Some(transaction.id),
                            code: reason_code,
                            permanent: false,
                        }
                    }
                    ManifestTransactionStage::Activating => SteamControlMessage::SetupCancel {
                        identity,
                        transaction: transaction.id,
                        code: reason_code,
                    },
                    // Once the authority has received every activation receipt
                    // and begun the final release, rollback is no longer safe.
                    // Reliable control retransmission or ordinary terminal
                    // gameplay handling owns that irrevocable boundary.
                    ManifestTransactionStage::Activated => continue,
                };
                let _ = self
                    .coordinator
                    .queue_control_for_user(participant.user, message);
            }
        }

        fn frozen_manifest_connections(
            transaction: RuntimeManifestTransaction,
        ) -> [Option<(SteamUserId, SteamConnectionId)>; MAX_STEAM_LOBBY_MEMBERS] {
            std::array::from_fn(|index| {
                transaction.participants[index]
                    .map(|participant| (participant.user, participant.connection))
            })
        }

        fn cancel_active_manifest_setup(
            &mut self,
            transaction: RuntimeManifestTransaction,
            reason_code: u16,
            now_ms: u64,
        ) -> Result<(), NativeOnlineRuntimeError> {
            if transaction.stage != ManifestTransactionStage::Activating
                || !self.endpoints.is_empty()
            {
                return Err(OnlineLobbyError::InvalidState.into());
            }
            let status = self.coordinator.status();
            if status.role != Some(OnlineLobbyRole::ListenAuthority) {
                return Err(AuthSignalError::UnexpectedPurpose.into());
            }
            self.send_manifest_rollback_best_effort(transaction, reason_code);

            self.rollback_manifest_setup(transaction, reason_code, now_ms)
        }

        fn rollback_manifest_setup(
            &mut self,
            transaction: RuntimeManifestTransaction,
            reason_code: u16,
            now_ms: u64,
        ) -> Result<(), NativeOnlineRuntimeError> {
            if transaction.stage == ManifestTransactionStage::Activated
                || !self.endpoints.is_empty()
            {
                return Err(OnlineLobbyError::InvalidState.into());
            }
            let participants = Self::frozen_manifest_connections(transaction);
            self.retire_manifest_transaction();
            self.committed_roster = None;
            self.pending_manifest = None;
            self.endpoints.clear();
            self.coordinator.cancel_committed_setup_exact(
                &mut self.platform,
                participants,
                reason_code,
                now_ms,
            )?;
            Ok(())
        }

        fn enforce_activation_deadline(
            &mut self,
            now_ms: u64,
        ) -> Result<(), NativeOnlineRuntimeError> {
            if self.coordinator.steam_backend_reconnect_pending() {
                return Ok(());
            }
            let Some(transaction) = self.manifest_transaction else {
                return Ok(());
            };
            if transaction.stage != ManifestTransactionStage::Activating
                || transaction
                    .activation_deadline_ms
                    .is_none_or(|deadline_ms| now_ms < deadline_ms)
            {
                return Ok(());
            }
            self.cancel_active_manifest_setup(
                transaction,
                OnlineLobbyPhase::Loading.diagnostic_code(),
                now_ms,
            )
        }

        fn send_manifest_prepare(&mut self) -> Result<(), NativeOnlineRuntimeError> {
            let status = self.coordinator.status();
            if status.role != Some(OnlineLobbyRole::ListenAuthority)
                || self
                    .roster_auth
                    .as_ref()
                    .is_none_or(|auth| !auth.globally_complete)
            {
                return Err(AuthSignalError::UnexpectedManifestSender.into());
            }
            let lobby = status.lobby.ok_or(AuthSignalError::WrongLobby)?;
            let local = self.platform.local_user();
            let manifest = self
                .coordinator
                .match_config()
                .ok_or(AuthSignalError::InvalidEnvelope)?
                .manifest;
            let transaction = ManifestTransactionId::new(self.next_manifest_transaction_id)
                .map_err(|_| AuthSignalError::InvalidEnvelope)?;
            self.next_manifest_transaction_id = self
                .next_manifest_transaction_id
                .checked_add(1)
                .ok_or(NativeOnlineRuntimeError::Capacity)?;
            let mut participants = [None; MAX_STEAM_LOBBY_MEMBERS];
            let mut count = 0_usize;
            for member in self.platform.roster().iter().flatten() {
                if member.user == local {
                    continue;
                }
                let connection = self
                    .coordinator
                    .active_connection_for_user(member.user)
                    .ok_or(NativeOnlineRuntimeError::InvalidAuthenticatedRoster)?;
                participants[count] = Some(ManifestParticipant {
                    user: member.user,
                    connection,
                    accepted: false,
                    commit_accepted: false,
                    activated: false,
                });
                count += 1;
            }
            if count != usize::from(status.required_remote_peers) {
                return Err(NativeOnlineRuntimeError::InvalidAuthenticatedRoster);
            }
            self.manifest_transaction = Some(RuntimeManifestTransaction {
                id: transaction,
                manifest_hash: manifest.manifest_hash,
                participants,
                stage: ManifestTransactionStage::Preparing,
                activation_deadline_ms: None,
            });
            for participant in participants.iter().flatten() {
                self.coordinator
                    .begin_control_manifest_agreement(participant.user)?;
                let identity = SteamControlIdentity::new(lobby, local, participant.user)
                    .map_err(|_| AuthSignalError::InvalidIdentity)?;
                self.coordinator.queue_control_for_user(
                    participant.user,
                    SteamControlMessage::ManifestPrepare {
                        identity,
                        transaction,
                        manifest,
                    },
                )?;
            }
            Ok(())
        }

        #[allow(clippy::too_many_arguments)]
        fn consume_manifest_prepare(
            &mut self,
            source: SteamUserId,
            connection: SteamConnectionId,
            identity: SteamControlIdentity,
            transaction: ManifestTransactionId,
            manifest: MatchManifest,
            now_ms: u64,
        ) -> Result<(), NativeOnlineRuntimeError> {
            let status = self.coordinator.status();
            if self.retired_manifest_transaction == Some(transaction) {
                return Ok(());
            }
            if status.role != Some(OnlineLobbyRole::Client)
                || status.owner != Some(source)
                || status.lobby != Some(identity.lobby)
                || identity.sender != source
                || identity.recipient != self.platform.local_user()
                || self.coordinator.active_connection_for_user(source) != Some(connection)
                || self
                    .roster_auth
                    .as_ref()
                    .is_none_or(|auth| !auth.globally_complete)
            {
                return Err(AuthSignalError::UnexpectedManifestSender.into());
            }
            if let Some(active) = self.manifest_transaction {
                return if active.id == transaction && active.manifest_hash == manifest.manifest_hash
                {
                    Ok(())
                } else {
                    Err(AuthSignalError::ConflictingManifest.into())
                };
            }
            self.coordinator.enter_control_manifest_agreement(now_ms)?;
            self.coordinator.begin_control_manifest_agreement(source)?;
            let config = headless_config_from_manifest(manifest)
                .map_err(|_| AuthSignalError::InvalidEnvelope)?;
            self.coordinator
                .prepare_remote_manifest(&self.platform, config, now_ms)?;
            self.committed_roster = Some(self.freeze_authenticated_roster()?);
            let mut participants = [None; MAX_STEAM_LOBBY_MEMBERS];
            participants[0] = Some(ManifestParticipant {
                user: source,
                connection,
                accepted: true,
                commit_accepted: false,
                activated: false,
            });
            self.manifest_transaction = Some(RuntimeManifestTransaction {
                id: transaction,
                manifest_hash: manifest.manifest_hash,
                participants,
                stage: ManifestTransactionStage::Preparing,
                activation_deadline_ms: None,
            });
            let accepted_identity =
                SteamControlIdentity::new(identity.lobby, self.platform.local_user(), source)
                    .map_err(|_| AuthSignalError::InvalidIdentity)?;
            self.coordinator.queue_control_for_user(
                source,
                SteamControlMessage::ManifestAccepted {
                    identity: accepted_identity,
                    transaction,
                    manifest_hash: manifest.manifest_hash,
                },
            )?;
            Ok(())
        }

        #[allow(clippy::too_many_arguments)]
        fn consume_manifest_accepted(
            &mut self,
            source: SteamUserId,
            connection: SteamConnectionId,
            identity: SteamControlIdentity,
            transaction: ManifestTransactionId,
            manifest_hash: crate::network_protocol::ManifestHash,
            _now_ms: u64,
        ) -> Result<(), NativeOnlineRuntimeError> {
            let status = self.coordinator.status();
            if self.retired_manifest_transaction == Some(transaction) {
                return Ok(());
            }
            let active = self
                .manifest_transaction
                .as_mut()
                .ok_or(AuthSignalError::ConflictingManifest)?;
            if status.role != Some(OnlineLobbyRole::ListenAuthority)
                || status.lobby != Some(identity.lobby)
                || identity.sender != source
                || identity.recipient != self.platform.local_user()
                || active.id != transaction
                || active.manifest_hash != manifest_hash
                || active.stage != ManifestTransactionStage::Preparing
                || active
                    .participant(source)
                    .is_none_or(|participant| participant.connection != connection)
            {
                return Err(AuthSignalError::ConflictingManifest.into());
            }
            active
                .participant_mut(source)
                .expect("validated participant exists")
                .accepted = true;
            if active
                .participants
                .iter()
                .flatten()
                .all(|participant| participant.accepted)
            {
                let participants = active.participants;
                active.stage = ManifestTransactionStage::Committing;
                let local = self.platform.local_user();
                for participant in participants.iter().flatten() {
                    let commit_identity =
                        SteamControlIdentity::new(identity.lobby, local, participant.user)
                            .map_err(|_| AuthSignalError::InvalidIdentity)?;
                    self.coordinator.queue_control_for_user(
                        participant.user,
                        SteamControlMessage::ManifestCommit {
                            identity: commit_identity,
                            transaction,
                            manifest_hash,
                        },
                    )?;
                }
            }
            Ok(())
        }

        #[allow(clippy::too_many_arguments)]
        fn consume_manifest_commit(
            &mut self,
            source: SteamUserId,
            connection: SteamConnectionId,
            identity: SteamControlIdentity,
            transaction: ManifestTransactionId,
            manifest_hash: crate::network_protocol::ManifestHash,
            _now_ms: u64,
        ) -> Result<(), NativeOnlineRuntimeError> {
            let status = self.coordinator.status();
            if self.retired_manifest_transaction == Some(transaction) {
                return Ok(());
            }
            let active = self
                .manifest_transaction
                .as_mut()
                .ok_or(AuthSignalError::ConflictingManifest)?;
            if status.role != Some(OnlineLobbyRole::Client)
                || status.owner != Some(source)
                || status.lobby != Some(identity.lobby)
                || identity.sender != source
                || identity.recipient != self.platform.local_user()
                || active.id != transaction
                || active.manifest_hash != manifest_hash
                || active
                    .participant(source)
                    .is_none_or(|participant| participant.connection != connection)
            {
                return Err(AuthSignalError::ConflictingManifest.into());
            }
            if active.stage == ManifestTransactionStage::Preparing {
                active.stage = ManifestTransactionStage::Committing;
            } else if active.stage != ManifestTransactionStage::Committing {
                return Ok(());
            }
            let accepted_identity =
                SteamControlIdentity::new(identity.lobby, self.platform.local_user(), source)
                    .map_err(|_| AuthSignalError::InvalidIdentity)?;
            self.coordinator.queue_control_for_user(
                source,
                SteamControlMessage::ManifestCommitAccepted {
                    identity: accepted_identity,
                    transaction,
                    manifest_hash,
                },
            )?;
            Ok(())
        }

        #[allow(clippy::too_many_arguments)]
        fn consume_manifest_commit_accepted(
            &mut self,
            source: SteamUserId,
            connection: SteamConnectionId,
            identity: SteamControlIdentity,
            transaction: ManifestTransactionId,
            manifest_hash: crate::network_protocol::ManifestHash,
            now_ms: u64,
        ) -> Result<(), NativeOnlineRuntimeError> {
            let status = self.coordinator.status();
            let activation_timeout_ms = self.coordinator.config().timeouts.manifest_agreement_ms;
            if self.retired_manifest_transaction == Some(transaction) {
                return Ok(());
            }
            let active = self
                .manifest_transaction
                .as_mut()
                .ok_or(AuthSignalError::ConflictingManifest)?;
            if status.role != Some(OnlineLobbyRole::ListenAuthority)
                || status.lobby != Some(identity.lobby)
                || identity.sender != source
                || identity.recipient != self.platform.local_user()
                || active.id != transaction
                || active.manifest_hash != manifest_hash
                || active.stage != ManifestTransactionStage::Committing
                || active
                    .participant(source)
                    .is_none_or(|participant| participant.connection != connection)
            {
                return Err(AuthSignalError::ConflictingManifest.into());
            }
            active
                .participant_mut(source)
                .expect("validated participant exists")
                .commit_accepted = true;
            if active
                .participants
                .iter()
                .flatten()
                .all(|participant| participant.commit_accepted)
            {
                let participants = active.participants;
                active.stage = ManifestTransactionStage::Activating;
                active.activation_deadline_ms = Some(now_ms.saturating_add(activation_timeout_ms));
                self.coordinator.commit_prepared_manifest(now_ms)?;
                for participant in participants.iter().flatten() {
                    self.coordinator
                        .arm_control_gameplay_receive(participant.user)?;
                    let activate_identity = SteamControlIdentity::new(
                        identity.lobby,
                        self.platform.local_user(),
                        participant.user,
                    )
                    .map_err(|_| AuthSignalError::InvalidIdentity)?;
                    self.coordinator.queue_control_for_user(
                        participant.user,
                        SteamControlMessage::GameplayActivate {
                            identity: activate_identity,
                            transaction,
                            manifest_hash,
                        },
                    )?;
                }
            }
            Ok(())
        }

        #[allow(clippy::too_many_arguments)]
        fn consume_gameplay_activate(
            &mut self,
            source: SteamUserId,
            connection: SteamConnectionId,
            identity: SteamControlIdentity,
            transaction: ManifestTransactionId,
            manifest_hash: crate::network_protocol::ManifestHash,
        ) -> Result<(), NativeOnlineRuntimeError> {
            let status = self.coordinator.status();
            if self.retired_manifest_transaction == Some(transaction) {
                return Ok(());
            }
            let active = self
                .manifest_transaction
                .as_mut()
                .ok_or(AuthSignalError::ConflictingManifest)?;
            if status.role != Some(OnlineLobbyRole::Client)
                || status.owner != Some(source)
                || status.lobby != Some(identity.lobby)
                || identity.sender != source
                || identity.recipient != self.platform.local_user()
                || active.id != transaction
                || active.manifest_hash != manifest_hash
                || active
                    .participant(source)
                    .is_none_or(|participant| participant.connection != connection)
            {
                return Err(AuthSignalError::ConflictingManifest.into());
            }
            match active.stage {
                ManifestTransactionStage::Committing => {
                    self.coordinator
                        .commit_prepared_manifest(self.last_now_ms)?;
                    self.coordinator.arm_control_gameplay_receive(source)?;
                    active.stage = ManifestTransactionStage::Activating;
                    // After sending the receipt, the client cannot know
                    // whether the authority crossed the all-receipts barrier.
                    // It therefore waits for the reliable final release and
                    // never attempts a unilateral rollback.
                    active.activation_deadline_ms = None;
                }
                ManifestTransactionStage::Activating => {}
                ManifestTransactionStage::Activated => return Ok(()),
                ManifestTransactionStage::Preparing => {
                    return Err(AuthSignalError::ConflictingManifest.into());
                }
            }
            let activated_identity =
                SteamControlIdentity::new(identity.lobby, self.platform.local_user(), source)
                    .map_err(|_| AuthSignalError::InvalidIdentity)?;
            self.coordinator.queue_control_for_user(
                source,
                SteamControlMessage::GameplayActivated {
                    identity: activated_identity,
                    transaction,
                    manifest_hash,
                },
            )?;
            Ok(())
        }

        #[allow(clippy::too_many_arguments)]
        fn consume_gameplay_activated(
            &mut self,
            source: SteamUserId,
            connection: SteamConnectionId,
            identity: SteamControlIdentity,
            transaction: ManifestTransactionId,
            manifest_hash: crate::network_protocol::ManifestHash,
        ) -> Result<(), NativeOnlineRuntimeError> {
            let status = self.coordinator.status();
            if self.retired_manifest_transaction == Some(transaction) {
                return Ok(());
            }
            let snapshot = self
                .manifest_transaction
                .as_ref()
                .copied()
                .ok_or(AuthSignalError::ConflictingManifest)?;
            if status.lobby != Some(identity.lobby)
                || identity.sender != source
                || identity.recipient != self.platform.local_user()
                || snapshot.id != transaction
                || snapshot.manifest_hash != manifest_hash
                || snapshot
                    .participant(source)
                    .is_none_or(|participant| participant.connection != connection)
            {
                return Err(AuthSignalError::ConflictingManifest.into());
            }

            match status.role {
                Some(OnlineLobbyRole::ListenAuthority) => {
                    if snapshot.stage == ManifestTransactionStage::Activated {
                        return Ok(());
                    }
                    if snapshot.stage != ManifestTransactionStage::Activating {
                        return Err(AuthSignalError::ConflictingManifest.into());
                    }
                    let active = self
                        .manifest_transaction
                        .as_mut()
                        .expect("validated manifest transaction remains active");
                    active
                        .participant_mut(source)
                        .expect("validated participant exists")
                        .activated = true;
                    if !active
                        .participants
                        .iter()
                        .flatten()
                        .all(|participant| participant.activated)
                    {
                        return Ok(());
                    }

                    let participants = active.participants;
                    let local = self.platform.local_user();
                    let mut release_users = [None; MAX_STEAM_LOBBY_MEMBERS];
                    for participant in participants.iter().flatten() {
                        if self
                            .coordinator
                            .active_connection_for_user(participant.user)
                            != Some(participant.connection)
                        {
                            return Err(AuthSignalError::ConflictingManifest.into());
                        }
                        let slot = release_users
                            .iter_mut()
                            .find(|slot| slot.is_none())
                            .ok_or(NativeOnlineRuntimeError::Capacity)?;
                        *slot = Some(participant.user);
                    }
                    if !self.coordinator.can_queue_control_for_users(release_users) {
                        return Err(NativeOnlineRuntimeError::Capacity);
                    }

                    // Cross the irrevocable barrier before the first release
                    // can enter a reliable outbox. Any failure beyond this
                    // point must take normal terminal handling and can never
                    // race an activation timeout into SetupCancel.
                    let active = self
                        .manifest_transaction
                        .as_mut()
                        .expect("manifest transaction remains active through release");
                    active.stage = ManifestTransactionStage::Activated;
                    active.activation_deadline_ms = None;
                    for participant in participants.iter().flatten() {
                        let release_identity =
                            SteamControlIdentity::new(identity.lobby, local, participant.user)
                                .map_err(|_| AuthSignalError::InvalidIdentity)?;
                        self.coordinator.queue_control_for_user(
                            participant.user,
                            SteamControlMessage::GameplayActivated {
                                identity: release_identity,
                                transaction,
                                manifest_hash,
                            },
                        )?;
                    }
                    for participant in participants.iter().flatten() {
                        self.coordinator
                            .promote_control_connection(participant.user)?;
                    }
                }
                Some(OnlineLobbyRole::Client) => {
                    if status.owner != Some(source) {
                        return Err(AuthSignalError::UnexpectedManifestSender.into());
                    }
                    if snapshot.stage == ManifestTransactionStage::Activated {
                        return Ok(());
                    }
                    if snapshot.stage != ManifestTransactionStage::Activating {
                        return Err(AuthSignalError::ConflictingManifest.into());
                    }
                    self.coordinator.promote_control_connection(source)?;
                    let active = self
                        .manifest_transaction
                        .as_mut()
                        .expect("manifest transaction remains active through release");
                    active.stage = ManifestTransactionStage::Activated;
                    active.activation_deadline_ms = None;
                }
                None => return Err(AuthSignalError::UnexpectedPurpose.into()),
            }
            Ok(())
        }

        fn consume_setup_abort(
            &mut self,
            source: SteamUserId,
            identity: SteamControlIdentity,
            transaction: Option<ManifestTransactionId>,
            reason_code: u16,
            permanent: bool,
            now_ms: u64,
        ) -> Result<(), NativeOnlineRuntimeError> {
            let status = self.coordinator.status();
            if permanent
                && reason_code > AUTH_REJECT_ROSTER_ABORT_CODE_BASE
                && reason_code
                    <= AUTH_REJECT_ROSTER_ABORT_CODE_BASE + MAX_STEAM_LOBBY_MEMBERS as u16
            {
                return self.consume_permanent_roster_auth_abort(
                    source,
                    identity,
                    transaction,
                    reason_code,
                );
            }
            if transaction.is_none()
                && !permanent
                && status.lobby == Some(identity.lobby)
                && identity.sender == source
                && identity.recipient == self.platform.local_user()
                && matches!(
                    status.phase,
                    OnlineLobbyPhase::Lobby
                        | OnlineLobbyPhase::Connecting
                        | OnlineLobbyPhase::Authenticating
                )
            {
                match (status.role, reason_code) {
                    (Some(OnlineLobbyRole::Client), AUTH_RETRY_DIRECT_ABORT_CODE)
                        if status.owner == Some(source) =>
                    {
                        self.clear_peer_handoffs(source);
                        self.clear_pending_steam_setup_retries_for_user(source);
                        // The ingress ACK must be accepted before this exact
                        // physical generation is retired. The drain loop
                        // performs the targeted replacement immediately after
                        // accepting this frame.
                        self.deferred_control_setup_retry = Some(source);
                        return Ok(());
                    }
                    (Some(OnlineLobbyRole::ListenAuthority), AUTH_RETRY_DIRECT_ABORT_CODE)
                        if self
                            .platform
                            .roster()
                            .iter()
                            .flatten()
                            .any(|member| member.user == source) =>
                    {
                        if let Some(roster) = self.roster_auth {
                            for participant in roster.participants.iter().flatten() {
                                let roster_identity = SteamControlIdentity::new(
                                    identity.lobby,
                                    self.platform.local_user(),
                                    participant.user,
                                )
                                .map_err(|_| AuthSignalError::InvalidIdentity)?;
                                self.coordinator.queue_control_for_user(
                                    participant.user,
                                    SteamControlMessage::Abort {
                                        identity: roster_identity,
                                        transaction: None,
                                        code: AUTH_RETRY_ROSTER_ABORT_CODE,
                                        permanent: false,
                                    },
                                )?;
                            }
                        }
                        let response_identity = SteamControlIdentity::new(
                            identity.lobby,
                            self.platform.local_user(),
                            source,
                        )
                        .map_err(|_| AuthSignalError::InvalidIdentity)?;
                        self.coordinator.queue_control_for_user(
                            source,
                            SteamControlMessage::Abort {
                                identity: response_identity,
                                transaction: None,
                                code: AUTH_RETRY_DIRECT_ABORT_CODE,
                                permanent: false,
                            },
                        )?;
                        self.clear_peer_handoffs(source);
                        return Ok(());
                    }
                    (Some(OnlineLobbyRole::Client), AUTH_RETRY_ROSTER_ABORT_CODE)
                        if status.owner == Some(source) =>
                    {
                        self.retire_roster_auth_state();
                        return Ok(());
                    }
                    (Some(OnlineLobbyRole::ListenAuthority), AUTH_RETRY_ROSTER_ABORT_CODE)
                        if self
                            .roster_auth
                            .as_ref()
                            .is_some_and(|active| active.participant(source).is_some()) =>
                    {
                        let local = self.platform.local_user();
                        let users: Vec<_> = self
                            .roster_auth
                            .expect("validated roster transaction exists")
                            .participants
                            .iter()
                            .flatten()
                            .map(|participant| participant.user)
                            .collect();
                        for user in users {
                            let abort_identity =
                                SteamControlIdentity::new(identity.lobby, local, user)
                                    .map_err(|_| AuthSignalError::InvalidIdentity)?;
                            self.coordinator.queue_control_for_user(
                                user,
                                SteamControlMessage::Abort {
                                    identity: abort_identity,
                                    transaction: None,
                                    code: AUTH_RETRY_ROSTER_ABORT_CODE,
                                    permanent: false,
                                },
                            )?;
                        }
                        self.retire_roster_auth_state();
                        return Ok(());
                    }
                    _ => return Err(AuthSignalError::UnexpectedPurpose.into()),
                }
            }
            if transaction.is_some_and(|id| self.retired_manifest_transaction == Some(id)) {
                return Ok(());
            }
            if permanent
                || identity.lobby != status.lobby.unwrap_or(identity.lobby)
                || identity.sender != source
                || identity.recipient != self.platform.local_user()
                || status.role != Some(OnlineLobbyRole::Client)
                || status.owner != Some(source)
                || transaction != self.manifest_transaction.as_ref().map(|active| active.id)
            {
                return Err(AuthSignalError::UnexpectedPurpose.into());
            }
            self.retire_manifest_transaction();
            self.coordinator
                .abort_manifest_agreement(&mut self.platform, reason_code, now_ms)?;
            Ok(())
        }

        fn consume_permanent_roster_auth_abort(
            &mut self,
            source: SteamUserId,
            identity: SteamControlIdentity,
            transaction: Option<ManifestTransactionId>,
            reason_code: u16,
        ) -> Result<(), NativeOnlineRuntimeError> {
            let status = self.coordinator.status();
            if status.phase != OnlineLobbyPhase::Lobby
                || status.lobby != Some(identity.lobby)
                || identity.sender != source
                || identity.recipient != self.platform.local_user()
            {
                return Err(AuthSignalError::UnexpectedPurpose.into());
            }
            let epoch = Self::roster_auth_abort_epoch(transaction)?;
            let source_is_valid = match status.role {
                Some(OnlineLobbyRole::ListenAuthority) => self
                    .platform
                    .roster()
                    .iter()
                    .flatten()
                    .any(|member| member.user == source),
                Some(OnlineLobbyRole::Client) => status.owner == Some(source),
                None => false,
            };
            if !source_is_valid {
                return Err(AuthSignalError::InvalidIdentity.into());
            }

            // The exact epoch was already retired locally. ACK this delayed
            // reliable frame without decoding its roster ordinal against a
            // potentially newer membership snapshot.
            if self.retired_account_auth_epoch == Some(epoch)
                && self
                    .roster_auth
                    .as_ref()
                    .is_none_or(|active| active.epoch != epoch)
            {
                return Ok(());
            }

            let rejected_user = self.permanent_roster_auth_abort_user(reason_code)?;
            let local = self.platform.local_user();

            match status.role {
                Some(OnlineLobbyRole::ListenAuthority) => {
                    let active = self.roster_auth.ok_or(AuthSignalError::UnexpectedPurpose)?;
                    if active.epoch != epoch {
                        return Err(AuthSignalError::InvalidEnvelope.into());
                    }
                    // A routed ticket recipient reports a different routed
                    // sender. The listen owner validates that both accounts
                    // belong to this exact epoch, then fans the verdict out on
                    // every authority-star control socket.
                    if source == rejected_user
                        || active.participant(source).is_none()
                        || active.participant(rejected_user).is_none()
                    {
                        return Err(AuthSignalError::InvalidIdentity.into());
                    }
                    let transaction = Self::roster_auth_abort_transaction(epoch)?;
                    let lobby = status.lobby.ok_or(AuthSignalError::WrongLobby)?;
                    for participant in active.participants.iter().flatten() {
                        let abort_identity =
                            SteamControlIdentity::new(lobby, local, participant.user)
                                .map_err(|_| AuthSignalError::InvalidIdentity)?;
                        self.coordinator.queue_control_for_user(
                            participant.user,
                            SteamControlMessage::Abort {
                                identity: abort_identity,
                                transaction: Some(transaction),
                                code: reason_code,
                                permanent: true,
                            },
                        )?;
                    }
                }
                Some(OnlineLobbyRole::Client) => {
                    // The authority can fan this verdict to a client before
                    // its earlier RosterPrepare reaches the front of the same
                    // socket's ordered stream on another process. The secure
                    // authority identity plus current canonical roster are
                    // sufficient to retire that epoch; delayed prepare/ticket
                    // frames are then semantically ACKed as known-stale.
                    if let Some(active) = self.roster_auth {
                        if active.epoch != epoch {
                            return Err(AuthSignalError::InvalidEnvelope.into());
                        }
                        if rejected_user != local && active.participant(rejected_user).is_none() {
                            return Err(AuthSignalError::InvalidIdentity.into());
                        }
                    } else {
                        self.retired_account_auth_epoch = Some(epoch);
                    }
                }
                None => return Err(AuthSignalError::UnexpectedPurpose.into()),
            }

            self.apply_permanent_roster_auth_rejection(
                rejected_user,
                Self::propagated_permanent_roster_auth_failure(reason_code),
            )
        }

        fn consume_setup_cancel(
            &mut self,
            source: SteamUserId,
            connection: SteamConnectionId,
            identity: SteamControlIdentity,
            transaction: ManifestTransactionId,
            reason_code: u16,
            now_ms: u64,
        ) -> Result<(), NativeOnlineRuntimeError> {
            let status = self.coordinator.status();
            if self.retired_manifest_transaction == Some(transaction) {
                return Ok(());
            }
            let active = self
                .manifest_transaction
                .as_ref()
                .copied()
                .ok_or(AuthSignalError::UnexpectedPurpose)?;
            if status.lobby != Some(identity.lobby)
                || identity.sender != source
                || identity.recipient != self.platform.local_user()
                || active.id != transaction
                || active
                    .participant(source)
                    .is_none_or(|participant| participant.connection != connection)
            {
                return Err(AuthSignalError::UnexpectedPurpose.into());
            }
            if active.stage == ManifestTransactionStage::Activated {
                // The final release is irrevocable. A cancel sent just before
                // the sender learned that the barrier completed is stale, not
                // malformed, and is safely ACKed without rolling gameplay back.
                return Ok(());
            }
            match status.role {
                Some(OnlineLobbyRole::Client) if status.owner == Some(source) => {
                    self.rollback_manifest_setup(active, reason_code, now_ms)
                }
                _ => Err(AuthSignalError::UnexpectedPurpose.into()),
            }
        }

        #[cfg(test)]
        fn consume_manifest_signal(
            &mut self,
            source: SteamUserId,
            signal: BootstrapManifestSignal,
            now_ms: u64,
        ) -> Result<(), NativeOnlineRuntimeError> {
            let status = self.coordinator.status();
            if status.lobby != Some(signal.lobby) {
                return Err(AuthSignalError::WrongLobby.into());
            }
            if signal.recipient != self.platform.local_user() {
                return Err(AuthSignalError::WrongRecipient.into());
            }
            if signal.sender != source || status.owner != Some(source) {
                return Err(AuthSignalError::UnexpectedManifestSender.into());
            }
            if status.role != Some(OnlineLobbyRole::Client) {
                return Err(AuthSignalError::UnexpectedManifestSender.into());
            }
            match classify_manifest_ingress(
                self.coordinator
                    .match_config()
                    .map(|config| config.manifest),
                self.pending_manifest,
                status.phase,
                signal,
            )? {
                ManifestIngress::ExactDuplicate => return Ok(()),
                ManifestIngress::Stage => {
                    self.pending_manifest = Some(signal);
                    return Ok(());
                }
                ManifestIngress::Apply => {}
            }
            let config = headless_config_from_manifest(signal.manifest)
                .map_err(|_| AuthSignalError::InvalidEnvelope)?;
            match self
                .coordinator
                .accept_manifest(&self.platform, config, now_ms)
            {
                Err(OnlineLobbyError::ManifestDeclarationsPending) => {
                    self.pending_manifest = Some(signal);
                    Ok(())
                }
                Ok(()) => {
                    self.committed_roster = Some(self.freeze_authenticated_roster()?);
                    Ok(())
                }
                Err(error) => Err(error.into()),
            }
        }

        fn try_apply_pending_manifest(
            &mut self,
            now_ms: u64,
        ) -> Result<(), NativeOnlineRuntimeError> {
            if self.coordinator.status().phase != OnlineLobbyPhase::ManifestAgreement {
                return Ok(());
            }
            let Some(signal) = self.pending_manifest.take() else {
                return Ok(());
            };
            let config = headless_config_from_manifest(signal.manifest)
                .map_err(|_| AuthSignalError::InvalidEnvelope)?;
            match self
                .coordinator
                .accept_manifest(&self.platform, config, now_ms)
            {
                Err(OnlineLobbyError::ManifestDeclarationsPending) => {
                    self.pending_manifest = Some(signal);
                    Ok(())
                }
                Ok(()) => {
                    self.committed_roster = Some(self.freeze_authenticated_roster()?);
                    Ok(())
                }
                Err(error) => Err(error.into()),
            }
        }

        fn accept_manifest_and_freeze(
            &mut self,
            config: HeadlessMatchConfig,
            now_ms: u64,
        ) -> Result<(), NativeOnlineRuntimeError> {
            self.coordinator
                .accept_manifest(&self.platform, config, now_ms)?;
            self.committed_roster = Some(self.freeze_authenticated_roster()?);
            Ok(())
        }

        fn install_local_mapping(
            &mut self,
            declaration: OnlineRosterMember,
        ) -> Result<(), NativeOnlineRuntimeError> {
            let mapping = AuthenticatedMapping {
                user: self.platform.local_user(),
                peer: AuthenticatedPeer {
                    peer_id: declaration.peer_id,
                    user_id: self.platform.local_user().authenticated(),
                },
                connection: None,
            };
            self.authenticated = [None; MAX_STEAM_LOBBY_MEMBERS];
            self.authenticated[0] = Some(mapping);
            Ok(())
        }

        fn replace_local_mapping(
            &mut self,
            declaration: OnlineRosterMember,
        ) -> Result<(), NativeOnlineRuntimeError> {
            let local = self.platform.local_user();
            let mapping = self
                .authenticated
                .iter_mut()
                .flatten()
                .find(|mapping| mapping.user == local)
                .ok_or(NativeOnlineRuntimeError::InvalidAuthenticatedRoster)?;
            mapping.peer.peer_id = declaration.peer_id;
            Ok(())
        }

        fn install_authenticated_mapping(
            &mut self,
            user: SteamUserId,
            peer_id: PeerId,
            reconnect: bool,
        ) -> Result<(), NativeOnlineRuntimeError> {
            let replacement = AuthenticatedMapping {
                user,
                peer: AuthenticatedPeer {
                    peer_id,
                    user_id: user.authenticated(),
                },
                connection: None,
            };
            if let Some(existing) = self
                .authenticated
                .iter_mut()
                .flatten()
                .find(|mapping| mapping.user == user)
            {
                if existing.peer != replacement.peer {
                    return Err(NativeOnlineRuntimeError::InvalidAuthenticatedRoster);
                }
                if reconnect {
                    existing.connection = None;
                }
                return Ok(());
            }
            if self.authenticated.iter().flatten().any(|mapping| {
                mapping.peer.peer_id == peer_id || mapping.peer.user_id == replacement.peer.user_id
            }) {
                return Err(NativeOnlineRuntimeError::InvalidAuthenticatedRoster);
            }
            let slot = self
                .authenticated
                .iter_mut()
                .find(|slot| slot.is_none())
                .ok_or(NativeOnlineRuntimeError::Capacity)?;
            *slot = Some(replacement);
            Ok(())
        }

        fn bind_authenticated_connection(
            &mut self,
            user: SteamUserId,
            peer_id: PeerId,
            connection: SteamConnectionId,
        ) -> Result<(), NativeOnlineRuntimeError> {
            let mapping = self
                .authenticated
                .iter_mut()
                .flatten()
                .find(|mapping| mapping.user == user)
                .ok_or(NativeOnlineRuntimeError::EndpointIdentityMismatch)?;
            if mapping.peer.peer_id != peer_id {
                return Err(NativeOnlineRuntimeError::EndpointIdentityMismatch);
            }
            mapping.connection = Some(connection);
            Ok(())
        }

        fn freeze_authenticated_roster(
            &self,
        ) -> Result<CommittedAuthenticatedRoster, NativeOnlineRuntimeError> {
            let mut roster = CommittedAuthenticatedRoster::default();
            let mut peers = [None; MAX_STEAM_LOBBY_MEMBERS];
            let mut peer_count = 0_usize;
            for declaration in self.coordinator.roster_members() {
                peers[peer_count] = Some(AuthenticatedPeer {
                    peer_id: declaration.peer_id,
                    user_id: declaration.authenticated_user,
                });
                peer_count += 1;
            }
            peers[..peer_count].sort_unstable_by_key(|peer| {
                peer.expect("canonical roster prefix is packed")
                    .peer_id
                    .get()
            });
            for peer in peers[..peer_count].iter().flatten().copied() {
                roster.push(peer)?;
            }
            if roster.len() != self.platform.roster_len() {
                return Err(NativeOnlineRuntimeError::InvalidAuthenticatedRoster);
            }
            Ok(roster)
        }

        fn remove_ticket_exchange(&mut self, user: SteamUserId) {
            for slot in &mut self.ticket_exchanges {
                if slot.is_some_and(|record| record.lease.remote_user == user) {
                    *slot = None;
                }
            }
        }

        /// A rejection is destructive only when it names the mapping generation
        /// that is still active. `None` is reserved for local/pre-attach
        /// rejection and cannot clear an already attached replacement.
        fn authentication_rejection_is_current(
            &self,
            user: SteamUserId,
            connection: Option<SteamConnectionId>,
        ) -> bool {
            authentication_rejection_targets_mapping(
                self.authenticated
                    .iter()
                    .flatten()
                    .find(|mapping| mapping.user == user)
                    .map(|mapping| mapping.connection),
                connection,
            )
        }

        fn clear_peer_handoffs(&mut self, user: SteamUserId) {
            if self
                .roster_auth
                .as_ref()
                .is_some_and(|active| active.participant(user).is_some())
            {
                self.retire_roster_auth_state();
            }
            if self
                .manifest_transaction
                .as_ref()
                .is_some_and(|active| active.participant(user).is_some())
            {
                self.retire_manifest_transaction();
            }
            self.remove_ticket_exchange(user);
            self.clear_pending_steam_setup_retries_for_user(user);
            self.clear_reconnect_user(user);
            for slot in &mut self.authenticated {
                if slot.is_some_and(|mapping| mapping.user == user) {
                    *slot = None;
                }
            }
            self.endpoints
                .retain(|endpoint| endpoint.admitted.remote_user != user);
            if self
                .pending_manifest
                .is_some_and(|manifest| manifest.sender == user)
            {
                self.pending_manifest = None;
            }
            for slot in &mut self.remote_ticket_sequences {
                if slot.is_some_and(|(candidate, _)| candidate == user) {
                    *slot = None;
                }
            }
            for slot in &mut self.local_ticket_accepted {
                if *slot == Some(user) {
                    *slot = None;
                }
            }
        }

        fn clear_active_peer_transport(
            &mut self,
            user: SteamUserId,
            connection: SteamConnectionId,
        ) {
            clear_runtime_peer_transport(
                &mut self.authenticated,
                &mut self.endpoints,
                user,
                connection,
            );
        }

        fn reconcile_live_bindings(
            &mut self,
            live_bindings: [Option<OnlinePeerIdentity>; MAX_STEAM_LOBBY_MEMBERS],
        ) {
            let mut active_members = [None; MAX_STEAM_LOBBY_MEMBERS];
            for (slot, member) in active_members
                .iter_mut()
                .zip(self.platform.roster().iter().flatten())
            {
                *slot = Some(member.user);
            }
            reconcile_runtime_identity_handoffs(
                live_bindings,
                active_members,
                &mut self.authenticated,
                &mut self.ticket_exchanges,
                &mut self.reconnect_users,
                &mut self.endpoints,
                &mut self.pending_manifest,
                &mut self.signal_rejected_users,
            );
        }

        fn mark_reconnect_user(
            &mut self,
            user: SteamUserId,
        ) -> Result<(), NativeOnlineRuntimeError> {
            if self.is_reconnect_user(user) {
                return Ok(());
            }
            let slot = self
                .reconnect_users
                .iter_mut()
                .find(|slot| slot.is_none())
                .ok_or(NativeOnlineRuntimeError::Capacity)?;
            *slot = Some(user);
            Ok(())
        }

        fn clear_reconnect_user(&mut self, user: SteamUserId) {
            for slot in &mut self.reconnect_users {
                if *slot == Some(user) {
                    *slot = None;
                }
            }
        }

        fn is_reconnect_user(&self, user: SteamUserId) -> bool {
            self.reconnect_users
                .iter()
                .any(|entry| *entry == Some(user))
        }

        fn is_signal_rejected_user(&self, user: SteamUserId) -> bool {
            self.signal_rejected_users.contains(&Some(user))
        }

        fn mark_signal_rejected_user(
            &mut self,
            user: SteamUserId,
        ) -> Result<(), NativeOnlineRuntimeError> {
            if self.is_signal_rejected_user(user) {
                return Ok(());
            }
            let slot = self
                .signal_rejected_users
                .iter_mut()
                .find(|slot| slot.is_none())
                .ok_or(NativeOnlineRuntimeError::Capacity)?;
            *slot = Some(user);
            Ok(())
        }

        fn reset_signal_isolation(&mut self) -> Result<(), NativeOnlineRuntimeError> {
            self.signal_rejected_users = [None; MAX_STEAM_LOBBY_MEMBERS];
            Ok(())
        }

        fn reset_match_handoff(&mut self) {
            self.retire_roster_auth_state();
            self.retire_manifest_transaction();
            self.ticket_exchanges = [None; MAX_STEAM_LOBBY_MEMBERS];
            self.reconnect_users = [None; MAX_STEAM_LOBBY_MEMBERS];
            self.committed_roster = None;
            self.endpoints.clear();
            self.pending_manifest = None;
            self.remote_ticket_sequences = [None; MAX_STEAM_LOBBY_MEMBERS];
            self.local_ticket_accepted = [None; MAX_STEAM_LOBBY_MEMBERS];
            self.pending_steam_setup_retries = [None; MAX_STEAM_LOBBY_MEMBERS];
            self.deferred_control_setup_retry = None;
            self.runtime_failure = None;
        }

        fn reset_all_session_state(&mut self) {
            self.reset_match_handoff();
            self.authenticated = [None; MAX_STEAM_LOBBY_MEMBERS];
            self.signal_rejected_users = [None; MAX_STEAM_LOBBY_MEMBERS];
            self.local_declaration = None;
            self.admission_quiesced = false;
        }
    }

    #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
    pub(super) type RealNativeOnlineRuntime =
        NativeOnlineCore<RealSteamBackend, RealNativeTransportFactory>;

    #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
    impl NativeOnlineCore<RealSteamBackend, RealNativeTransportFactory> {
        pub(super) fn initialize(
            release: NativeSteamReleaseConfig,
            lobby_config: OnlineLobbyConfig,
            now_ms: u64,
        ) -> Result<Self, NativeOnlineRuntimeError> {
            let platform = SteamPlatform::<RealSteamBackend>::initialize_steam_client(
                release.steam_client_config(),
                now_ms,
            )?;
            Self::from_parts(platform, RealNativeTransportFactory, lobby_config, now_ms)
        }
    }

    #[cfg(test)]
    mod tests {
        use std::cell::RefCell;
        use std::rc::Rc;

        use super::*;
        use crate::steam_platform::{
            AuthenticatedSteamPeer, FakeAuthOutcome, FakeSteamAuthAuthority, FakeSteamBackend,
            FakeSteamControl, LicenseStatus,
        };
        use crate::steam_transport::FakeSteamTransportNetwork;

        const MAX_FAKE_AUTH_ENDPOINT_GENERATIONS: usize = MAX_STEAM_LOBBY_MEMBERS * 2;
        const MAX_FAKE_AUTH_INBOX_MESSAGES: usize = MAX_AUTH_SIGNALS_PER_PUMP * 2;

        #[derive(Clone, Copy, PartialEq, Eq)]
        struct FakeAuthEndpointIdentity {
            user: SteamUserId,
            generation: u64,
        }

        struct FakeAuthSignalEnvelope {
            source: FakeAuthEndpointIdentity,
            encoded: EncodedPreGameSignal,
        }

        struct FakeAuthSignalInbox {
            identity: FakeAuthEndpointIdentity,
            messages: VecDeque<FakeAuthSignalEnvelope>,
        }

        struct FakeAuthSignalBusState {
            next_generation: u64,
            active: [Option<FakeAuthEndpointIdentity>; MAX_STEAM_LOBBY_MEMBERS],
            inboxes: Vec<FakeAuthSignalInbox>,
        }

        struct FakeAuthSignalBus {
            shared: Rc<RefCell<FakeAuthSignalBusState>>,
        }

        impl FakeAuthSignalBus {
            fn new() -> Self {
                Self {
                    shared: Rc::new(RefCell::new(FakeAuthSignalBusState {
                        next_generation: 1,
                        active: [None; MAX_STEAM_LOBBY_MEMBERS],
                        inboxes: Vec::with_capacity(MAX_FAKE_AUTH_ENDPOINT_GENERATIONS),
                    })),
                }
            }

            fn register(
                &self,
                user: SteamUserId,
            ) -> Result<FakeAuthSignalEndpoint, AuthSignalError> {
                let mut state = self.shared.borrow_mut();
                if state.inboxes.len() >= MAX_FAKE_AUTH_ENDPOINT_GENERATIONS {
                    return Err(AuthSignalError::TransportFailed);
                }
                let generation = state.next_generation;
                state.next_generation = state
                    .next_generation
                    .checked_add(1)
                    .ok_or(AuthSignalError::TransportFailed)?;
                let identity = FakeAuthEndpointIdentity { user, generation };
                if let Some(slot) = state
                    .active
                    .iter_mut()
                    .find(|slot| slot.is_some_and(|active| active.user == user))
                {
                    *slot = Some(identity);
                } else {
                    let slot = state
                        .active
                        .iter_mut()
                        .find(|slot| slot.is_none())
                        .ok_or(AuthSignalError::TransportFailed)?;
                    *slot = Some(identity);
                }
                state.inboxes.push(FakeAuthSignalInbox {
                    identity,
                    messages: VecDeque::with_capacity(MAX_FAKE_AUTH_INBOX_MESSAGES),
                });
                drop(state);
                Ok(FakeAuthSignalEndpoint {
                    shared: Rc::clone(&self.shared),
                    identity,
                    policy: RefCell::new(SignalAdmissionPolicy::default()),
                })
            }
        }

        struct FakeAuthSignalEndpoint {
            shared: Rc<RefCell<FakeAuthSignalBusState>>,
            identity: FakeAuthEndpointIdentity,
            policy: RefCell<SignalAdmissionPolicy>,
        }

        impl FakeAuthSignalEndpoint {
            fn send_encoded(
                &self,
                recipient: SteamUserId,
                encoded: EncodedPreGameSignal,
            ) -> Result<(), AuthSignalError> {
                if self.peer_is_quarantined(recipient)? {
                    return Ok(());
                }
                let mut state = self.shared.borrow_mut();
                if !state.active.contains(&Some(self.identity)) {
                    return Err(AuthSignalError::TransportFailed);
                }
                let recipient_identity = state
                    .active
                    .iter()
                    .flatten()
                    .find(|identity| identity.user == recipient)
                    .copied()
                    .ok_or(AuthSignalError::TransportFailed)?;
                let inbox = state
                    .inboxes
                    .iter_mut()
                    .find(|inbox| inbox.identity == recipient_identity)
                    .ok_or(AuthSignalError::TransportFailed)?;
                if inbox.messages.len() >= MAX_FAKE_AUTH_INBOX_MESSAGES {
                    return Err(AuthSignalError::ReceiveBudgetExceeded);
                }
                inbox.messages.push_back(FakeAuthSignalEnvelope {
                    source: self.identity,
                    encoded,
                });
                Ok(())
            }
        }

        impl FakeAuthSignalEndpoint {
            fn refresh_policy(&self, admission: AuthSignalAdmission) {
                let mut next = SignalAdmissionPolicy {
                    active_lobby: admission.active_lobby,
                    users: admission.users,
                    quarantined: [None; MAX_STEAM_LOBBY_MEMBERS],
                };
                let mut policy = self.policy.borrow_mut();
                policy.carry_quarantine_into(&mut next);
                *policy = next;
            }

            fn peer_is_quarantined(&self, user: SteamUserId) -> Result<bool, AuthSignalError> {
                Ok(self.policy.borrow().quarantined.contains(&Some(user)))
            }

            fn quarantine_peer(&self, user: SteamUserId) -> Result<(), AuthSignalError> {
                self.policy.borrow_mut().quarantine(user);
                Ok(())
            }

            fn reset_session_isolation(&self) -> Result<(), AuthSignalError> {
                self.policy.borrow_mut().clear_quarantine();
                Ok(())
            }

            fn quiesce_admission(&self) -> Result<(), AuthSignalError> {
                let mut policy = self.policy.borrow_mut();
                policy.active_lobby = None;
                policy.users = [None; MAX_STEAM_LOBBY_MEMBERS];
                Ok(())
            }

            fn send_ticket(&self, signal: AuthTicketSignal) -> Result<(), AuthSignalError> {
                self.send_encoded(signal.recipient, signal.encode())
            }

            fn send_manifest(
                &self,
                signal: BootstrapManifestSignal,
            ) -> Result<(), AuthSignalError> {
                self.send_encoded(signal.recipient, signal.encode()?)
            }

            fn receive(&self) -> Result<Vec<AuthSignalIngress>, AuthSignalError> {
                let envelopes = {
                    let mut state = self.shared.borrow_mut();
                    if !state.active.contains(&Some(self.identity)) {
                        return Err(AuthSignalError::TransportFailed);
                    }
                    let active = state.active;
                    let inbox = state
                        .inboxes
                        .iter_mut()
                        .find(|inbox| inbox.identity == self.identity)
                        .ok_or(AuthSignalError::TransportFailed)?;
                    let policy = self.policy.borrow();
                    inbox
                        .messages
                        .drain(..)
                        .filter(|envelope| {
                            active.contains(&Some(envelope.source))
                                && policy.allows(envelope.source.user)
                        })
                        .collect::<Vec<_>>()
                };
                Ok(decode_bounded_auth_signal_batch(envelopes.iter().map(
                    |envelope| (envelope.source.user, envelope.encoded.as_slice()),
                )))
            }
        }

        impl Drop for FakeAuthSignalEndpoint {
            fn drop(&mut self) {
                let mut state = self.shared.borrow_mut();
                state
                    .inboxes
                    .retain(|inbox| inbox.identity != self.identity);
                if let Some(slot) = state
                    .active
                    .iter_mut()
                    .find(|slot| **slot == Some(self.identity))
                {
                    *slot = None;
                }
            }
        }

        struct FakeNativeTransportFactory {
            network: FakeSteamTransportNetwork,
        }

        impl NativeTransportFactory<FakeSteamBackend> for FakeNativeTransportFactory {
            fn create_transport(
                &self,
                platform: &SteamPlatform<FakeSteamBackend>,
                session: SteamP2pSession,
                config: SteamTransportConfig,
                now_ms: u64,
            ) -> Result<SteamTransport, SteamTransportError> {
                self.network
                    .create_transport(platform.local_user(), session, config, now_ms)
            }
        }

        type FakeNativeOnlineCore = NativeOnlineCore<FakeSteamBackend, FakeNativeTransportFactory>;

        struct FakeNativeCorePair {
            host: FakeNativeOnlineCore,
            client: FakeNativeOnlineCore,
            network: FakeSteamTransportNetwork,
            host_control: FakeSteamControl,
            client_control: FakeSteamControl,
            lobby: SteamLobbyId,
            host_user: SteamUserId,
            client_user: SteamUserId,
            host_member: OnlineRosterMember,
            client_member: OnlineRosterMember,
            now_ms: u64,
        }

        impl FakeNativeCorePair {
            fn new() -> Self {
                let app_id = SteamAppId::new(12_345).unwrap();
                let host_user = SteamUserId::new(76_001).unwrap();
                let client_user = SteamUserId::new(76_002).unwrap();
                let host_member = test_member(host_user, PeerId::new(601).unwrap(), 0, 0);
                let client_member = test_member(client_user, PeerId::new(602).unwrap(), 1, 1);
                let network = FakeSteamTransportNetwork::new(64).unwrap();
                let auth_authority = FakeSteamAuthAuthority::new();
                let (host_backend, host_control) = FakeSteamBackend::new_with_auth_authority(
                    app_id,
                    host_user,
                    auth_authority.clone(),
                );
                let (client_backend, client_control) =
                    FakeSteamBackend::new_with_auth_authority(app_id, client_user, auth_authority);
                let host_platform =
                    SteamPlatform::new(SteamClientConfig::production(app_id), host_backend, 0)
                        .unwrap();
                let client_platform =
                    SteamPlatform::new(SteamClientConfig::production(app_id), client_backend, 0)
                        .unwrap();
                let lobby_config = OnlineLobbyConfig {
                    quality_sample_interval_ms: 1,
                    ..OnlineLobbyConfig::default()
                };
                let mut host = NativeOnlineCore::from_parts(
                    host_platform,
                    FakeNativeTransportFactory {
                        network: network.clone(),
                    },
                    lobby_config,
                    0,
                )
                .unwrap();
                let mut client = NativeOnlineCore::from_parts(
                    client_platform,
                    FakeNativeTransportFactory {
                        network: network.clone(),
                    },
                    lobby_config,
                    0,
                )
                .unwrap();
                host.execute(
                    NativeOnlineCommand::Create(NativeOnlineCreateRequest {
                        visibility: NativeOnlineVisibility::Private,
                        maximum_steam_peers: 2,
                        region: RegionCode::new("test-region").unwrap(),
                        rules: DefinitionId::new(1).unwrap(),
                        arena: DefinitionId::new(0).unwrap(),
                        seat_capacity: 2,
                        local_declaration: host_member,
                    }),
                    0,
                )
                .unwrap();
                host.pump(1).unwrap();
                let lobby = host.coordinator.status().lobby.unwrap();
                host_control
                    .mirror_lobby_shell_to(&client_control, lobby)
                    .unwrap();
                client
                    .execute(
                        NativeOnlineCommand::Join {
                            intent: LobbyJoinIntent {
                                lobby,
                                origin: crate::steam_platform::JoinOrigin::LaunchCommand,
                                expires_at_ms: 20_000,
                            },
                            local_declaration: client_member,
                        },
                        1,
                    )
                    .unwrap();
                // Fake join mutates only the client's independent backend.
                // Mirror that membership before the client installs its P2P
                // transport so the host listen policy already authorizes it.
                client_control
                    .mirror_lobby_member_to(&host_control, lobby, client_user)
                    .unwrap();
                host.pump(2).unwrap();
                client.pump(2).unwrap();

                let pair = Self {
                    host,
                    client,
                    network,
                    host_control,
                    client_control,
                    lobby,
                    host_user,
                    client_user,
                    host_member,
                    client_member,
                    now_ms: 2,
                };
                pair.mirror();
                pair
            }

            fn mirror(&self) {
                self.host_control
                    .mirror_lobby_owner_state_to(&self.client_control, self.lobby)
                    .unwrap();
                self.host_control
                    .mirror_lobby_member_to(&self.client_control, self.lobby, self.host_user)
                    .unwrap();
                self.client_control
                    .mirror_lobby_member_to(&self.host_control, self.lobby, self.client_user)
                    .unwrap();
            }

            fn pump_once(&mut self) {
                self.now_ms += 1;
                self.mirror();
                if let Err(error) = self.host.pump(self.now_ms) {
                    panic!(
                        "host pump {} failed: {error:?}; host={:?}; client={:?}; retirements=({}, {})",
                        self.now_ms,
                        self.host.coordinator.status(),
                        self.client.coordinator.status(),
                        self.host.coordinator.retiring_transport_count(),
                        self.client.coordinator.retiring_transport_count(),
                    );
                }
                self.mirror();
                if let Err(error) = self.client.pump(self.now_ms) {
                    panic!(
                        "client pump {} failed: {error:?}; host={:?}; client={:?}",
                        self.now_ms,
                        self.host.coordinator.status(),
                        self.client.coordinator.status()
                    );
                }
                self.mirror();
            }

            fn pump_until(&mut self, limit: usize, predicate: impl Fn(&Self) -> bool) {
                for _ in 0..limit {
                    if predicate(self) {
                        return;
                    }
                    self.pump_once();
                }
                assert!(
                    predicate(self),
                    "two-core fixture did not converge: host={:?}, client={:?}, host_connection={:?}, client_connection={:?}, host_tickets={}, client_tickets={}, host_rejected={:?}, client_rejected={:?}, host_last_disconnect={:?}, client_last_disconnect={:?}, host_platform_roster={:?}, client_platform_roster={:?}",
                    self.host.coordinator.status(),
                    self.client.coordinator.status(),
                    self.host
                        .coordinator
                        .control_connection_for_user(self.client_user),
                    self.client
                        .coordinator
                        .control_connection_for_user(self.host_user),
                    self.host.ticket_exchanges.iter().flatten().count(),
                    self.client.ticket_exchanges.iter().flatten().count(),
                    self.host.signal_rejected_users,
                    self.client.signal_rejected_users,
                    self.host
                        .events
                        .iter()
                        .rev()
                        .find(|event| matches!(event, OnlineLobbyEvent::PeerDisconnected { .. })),
                    self.client
                        .events
                        .iter()
                        .rev()
                        .find(|event| matches!(event, OnlineLobbyEvent::PeerDisconnected { .. })),
                    self.host.platform.roster(),
                    self.client.platform.roster(),
                );
            }

            fn pump_until_authenticated_endpoints(&mut self) {
                self.pump_until(80, |pair| {
                    pair.host.authenticated.iter().flatten().count() == 2
                        && pair.client.authenticated.iter().flatten().count() == 2
                        && pair.host.coordinator.status().secure_remote_peers == 1
                        && pair.client.coordinator.status().secure_remote_peers == 1
                });
                assert!(self.host.endpoints.is_empty());
                assert!(self.client.endpoints.is_empty());
            }

            fn ready_and_commit(&mut self, match_id: crate::network_protocol::MatchId) {
                if self
                    .host
                    .local_declaration
                    .is_some_and(|declaration| !declaration.ready)
                {
                    self.host
                        .execute(NativeOnlineCommand::SetReady(true), self.now_ms)
                        .unwrap();
                }
                if self
                    .client
                    .local_declaration
                    .is_some_and(|declaration| !declaration.ready)
                {
                    self.client
                        .execute(NativeOnlineCommand::SetReady(true), self.now_ms)
                        .unwrap_or_else(|error| {
                            panic!(
                                "client ready failed: {error:?}; host={:?}; client={:?}",
                                self.host.coordinator.status(),
                                self.client.coordinator.status()
                            )
                        });
                }
                self.mirror();
                self.pump_until(80, |pair| {
                    pair.host.coordinator.status().all_members_ready
                        && pair.client.coordinator.status().all_members_ready
                        && pair.host.coordinator.status().connected_remote_peers == 1
                        && pair.client.coordinator.status().connected_remote_peers == 1
                        && pair.host.coordinator.status().secure_remote_peers == 1
                        && pair.client.coordinator.status().secure_remote_peers == 1
                        && pair.host.coordinator.status().input_delay_calibration.state
                            == crate::network_quality::InputDelayCalibrationState::Ready
                });
                let calibration = self.host.coordinator.status().input_delay_calibration;
                let mut options = OnlineManifestOptions::casual_listen(
                    match_id,
                    self.host_member.peer_id,
                    DefinitionId::new(0).unwrap(),
                    DefinitionId::new(1).unwrap(),
                    0xAFC0_7601,
                    SimTick(240),
                );
                options.input_delay_ticks = calibration.selected_input_delay_ticks.unwrap();
                options.rollback_limit_ticks = crate::network_protocol::MAX_NORMAL_ROLLBACK_TICKS;
                self.host
                    .execute(
                        NativeOnlineCommand::CommitManifest {
                            options,
                            current_tick: SimTick(120),
                        },
                        self.now_ms,
                    )
                    .unwrap();
                self.pump_until(40, |pair| {
                    pair.host.coordinator.match_config().is_some()
                        && pair.client.coordinator.match_config().is_some()
                        && pair.host.committed_roster.is_some()
                        && pair.client.committed_roster.is_some()
                        && pair.host.coordinator.status().phase == OnlineLobbyPhase::Loading
                        && pair.client.coordinator.status().phase == OnlineLobbyPhase::Loading
                        && pair.host.endpoints.len() == 1
                        && pair.client.endpoints.len() == 1
                });
            }

            fn finish_confirmed_match(&mut self) {
                self.now_ms += 1;
                self.host
                    .execute(NativeOnlineCommand::ContentLoaded, self.now_ms)
                    .unwrap();
                self.client
                    .execute(NativeOnlineCommand::ContentLoaded, self.now_ms)
                    .unwrap();

                self.now_ms += 1;
                self.host
                    .execute(NativeOnlineCommand::InitialSyncComplete, self.now_ms)
                    .unwrap();
                self.client
                    .execute(NativeOnlineCommand::InitialSyncComplete, self.now_ms)
                    .unwrap();

                self.now_ms += 1;
                self.host
                    .execute(
                        NativeOnlineCommand::BeginCountdown(SimTick(240)),
                        self.now_ms,
                    )
                    .unwrap();
                self.client
                    .execute(
                        NativeOnlineCommand::BeginCountdown(SimTick(240)),
                        self.now_ms,
                    )
                    .unwrap();

                self.now_ms += 1;
                self.host
                    .execute(NativeOnlineCommand::MarkFighting(SimTick(240)), self.now_ms)
                    .unwrap();
                self.client
                    .execute(NativeOnlineCommand::MarkFighting(SimTick(240)), self.now_ms)
                    .unwrap();

                self.now_ms += 1;
                self.host
                    .execute(NativeOnlineCommand::BeginResultConfirmation, self.now_ms)
                    .unwrap();
                self.client
                    .execute(NativeOnlineCommand::BeginResultConfirmation, self.now_ms)
                    .unwrap();

                self.now_ms += 1;
                self.host
                    .execute(NativeOnlineCommand::ConfirmResult, self.now_ms)
                    .unwrap();
                self.client
                    .execute(NativeOnlineCommand::ConfirmResult, self.now_ms)
                    .unwrap();
                assert_eq!(
                    self.host.coordinator.status().outcome,
                    Some(OnlineMatchOutcome::Confirmed)
                );
                assert_eq!(
                    self.client.coordinator.status().outcome,
                    Some(OnlineMatchOutcome::Confirmed)
                );
            }

            fn rematch_and_commit_second_generation(&mut self, client_intent_first: bool) {
                let first_match_id =
                    crate::network_protocol::MatchId::new(*b"two-core-match01").unwrap();
                let second_match_id =
                    crate::network_protocol::MatchId::new(*b"two-core-match02").unwrap();
                self.pump_until_authenticated_endpoints();
                self.ready_and_commit(first_match_id);
                let first_connection = self.host.endpoints.front().unwrap().admitted.connection;
                self.finish_confirmed_match();

                self.now_ms += 1;
                if client_intent_first {
                    self.client
                        .execute(NativeOnlineCommand::Rematch, self.now_ms)
                        .unwrap();
                    assert_eq!(
                        self.client.coordinator.status().phase,
                        OnlineLobbyPhase::Results
                    );
                    self.host
                        .execute(NativeOnlineCommand::Rematch, self.now_ms)
                        .unwrap();
                } else {
                    self.host
                        .execute(NativeOnlineCommand::Rematch, self.now_ms)
                        .unwrap();
                    self.client
                        .execute(NativeOnlineCommand::Rematch, self.now_ms)
                        .unwrap();
                    assert_eq!(
                        self.client.coordinator.status().phase,
                        OnlineLobbyPhase::Results
                    );
                }
                assert_eq!(
                    self.host.coordinator.local_declaration().unwrap().revision,
                    2
                );
                assert_eq!(
                    self.client
                        .coordinator
                        .local_declaration()
                        .unwrap()
                        .revision,
                    1
                );

                // The client follows only the owner's mirrored declaration
                // epoch. Initial auth is ready-gated, leaving both users a
                // deterministic Lobby window before generation two starts.
                self.pump_until(40, |pair| {
                    pair.host.coordinator.status().phase == OnlineLobbyPhase::Lobby
                        && pair.client.coordinator.status().phase == OnlineLobbyPhase::Lobby
                        && pair
                            .host
                            .coordinator
                            .local_declaration()
                            .is_some_and(|declaration| declaration.revision == 2)
                        && pair
                            .client
                            .coordinator
                            .local_declaration()
                            .is_some_and(|declaration| declaration.revision == 2)
                });

                self.ready_and_commit(second_match_id);
                let second_connection = self.host.endpoints.front().unwrap().admitted.connection;
                assert_ne!(second_connection, first_connection);
                assert_eq!(
                    self.host
                        .coordinator
                        .match_config()
                        .unwrap()
                        .manifest
                        .match_id,
                    second_match_id
                );
                assert_eq!(
                    self.client
                        .coordinator
                        .match_config()
                        .unwrap()
                        .manifest
                        .match_id,
                    second_match_id
                );

                self.pump_until(200, |pair| {
                    pair.host.coordinator.retiring_transport_count() == 0
                        && pair.client.coordinator.retiring_transport_count() == 0
                });
                for metrics in [
                    self.host.coordinator.transport_retirement_metrics(),
                    self.client.coordinator.transport_retirement_metrics(),
                ] {
                    assert_eq!(metrics.started, 1);
                    assert_eq!(metrics.completed, 1);
                    assert_eq!(metrics.timed_out, 0);
                    assert_eq!(metrics.faulted, 0);
                }

                let host_endpoint_count = self.host.endpoints.len();
                self.now_ms += 1;
                self.host
                    .execute(
                        NativeOnlineCommand::MarkAuthorityTerminalDrained {
                            user: self.client_user,
                            peer_id: self.client_member.peer_id,
                            connection: first_connection,
                            retry: None,
                        },
                        self.now_ms,
                    )
                    .unwrap();
                assert_eq!(
                    self.host
                        .coordinator
                        .active_connection_for_user(self.client_user),
                    Some(second_connection)
                );
                assert_eq!(self.host.endpoints.len(), host_endpoint_count);
                assert_eq!(
                    self.host.endpoints.front().unwrap().admitted.connection,
                    second_connection
                );
            }
        }

        const FOUR_CORE_PEER_COUNT: usize = MAX_STEAM_LOBBY_MEMBERS;
        const FOUR_CORE_HOST: usize = 0;

        struct FakeNativeCoreQuartet {
            cores: [FakeNativeOnlineCore; FOUR_CORE_PEER_COUNT],
            controls: [FakeSteamControl; FOUR_CORE_PEER_COUNT],
            network: FakeSteamTransportNetwork,
            lobby: SteamLobbyId,
            users: [SteamUserId; FOUR_CORE_PEER_COUNT],
            members: [OnlineRosterMember; FOUR_CORE_PEER_COUNT],
            now_ms: u64,
        }

        impl FakeNativeCoreQuartet {
            fn new() -> Self {
                let app_id = SteamAppId::new(12_346).unwrap();
                let users = [
                    SteamUserId::new(77_001).unwrap(),
                    SteamUserId::new(77_002).unwrap(),
                    SteamUserId::new(77_003).unwrap(),
                    SteamUserId::new(77_004).unwrap(),
                ];
                let mut members = [
                    test_member(users[0], PeerId::new(701).unwrap(), 0, 0),
                    test_member(users[1], PeerId::new(702).unwrap(), 1, 1),
                    test_member(users[2], PeerId::new(703).unwrap(), 2, 0),
                    test_member(users[3], PeerId::new(704).unwrap(), 3, 1),
                ];
                for member in &mut members {
                    member.ready = false;
                }

                let network = FakeSteamTransportNetwork::new(128).unwrap();
                let auth_authority = FakeSteamAuthAuthority::new();
                let (host_backend, host_control) = FakeSteamBackend::new_with_auth_authority(
                    app_id,
                    users[0],
                    auth_authority.clone(),
                );
                let (first_backend, first_control) = FakeSteamBackend::new_with_auth_authority(
                    app_id,
                    users[1],
                    auth_authority.clone(),
                );
                let (second_backend, second_control) = FakeSteamBackend::new_with_auth_authority(
                    app_id,
                    users[2],
                    auth_authority.clone(),
                );
                let (third_backend, third_control) =
                    FakeSteamBackend::new_with_auth_authority(app_id, users[3], auth_authority);
                let make_platform = |backend| {
                    SteamPlatform::new(SteamClientConfig::production(app_id), backend, 0).unwrap()
                };
                let lobby_config = OnlineLobbyConfig {
                    quality_sample_interval_ms: 1,
                    ..OnlineLobbyConfig::default()
                };
                let make_core = |backend| {
                    NativeOnlineCore::from_parts(
                        make_platform(backend),
                        FakeNativeTransportFactory {
                            network: network.clone(),
                        },
                        lobby_config,
                        0,
                    )
                    .unwrap()
                };
                let mut cores = [
                    make_core(host_backend),
                    make_core(first_backend),
                    make_core(second_backend),
                    make_core(third_backend),
                ];
                let controls = [host_control, first_control, second_control, third_control];

                cores[FOUR_CORE_HOST]
                    .execute(
                        NativeOnlineCommand::Create(NativeOnlineCreateRequest {
                            visibility: NativeOnlineVisibility::Private,
                            maximum_steam_peers: FOUR_CORE_PEER_COUNT as u8,
                            region: RegionCode::new("test-region").unwrap(),
                            rules: DefinitionId::new(1).unwrap(),
                            arena: DefinitionId::new(0).unwrap(),
                            seat_capacity: FOUR_CORE_PEER_COUNT as u8,
                            local_declaration: members[FOUR_CORE_HOST],
                        }),
                        0,
                    )
                    .unwrap();
                cores[FOUR_CORE_HOST].pump(1).unwrap();
                let lobby = cores[FOUR_CORE_HOST].coordinator.status().lobby.unwrap();

                for control in &controls[1..] {
                    controls[FOUR_CORE_HOST]
                        .mirror_lobby_shell_to(control, lobby)
                        .unwrap();
                }
                for index in 1..FOUR_CORE_PEER_COUNT {
                    cores[index]
                        .execute(
                            NativeOnlineCommand::Join {
                                intent: LobbyJoinIntent {
                                    lobby,
                                    origin: crate::steam_platform::JoinOrigin::LaunchCommand,
                                    expires_at_ms: 20_000,
                                },
                                local_declaration: members[index],
                            },
                            1,
                        )
                        .unwrap();
                }

                let mut quartet = Self {
                    cores,
                    controls,
                    network,
                    lobby,
                    users,
                    members,
                    now_ms: 1,
                };
                // The fake clients own independent lobby replicas. Mirror each
                // declaration only from its owning process, just as Steam would
                // distribute member metadata to every lobby member.
                quartet.mirror();
                quartet.pump_until(80, |quartet| {
                    quartet.cores.iter().all(|core| {
                        core.coordinator.status().phase == OnlineLobbyPhase::Lobby
                            && core.platform.roster_len() == FOUR_CORE_PEER_COUNT
                    })
                });
                quartet
            }

            fn mirror(&self) {
                for target in 1..FOUR_CORE_PEER_COUNT {
                    self.controls[FOUR_CORE_HOST]
                        .mirror_lobby_owner_state_to(&self.controls[target], self.lobby)
                        .unwrap();
                }
                for source in 0..FOUR_CORE_PEER_COUNT {
                    for target in 0..FOUR_CORE_PEER_COUNT {
                        if source == target {
                            continue;
                        }
                        self.controls[source]
                            .mirror_lobby_member_to(
                                &self.controls[target],
                                self.lobby,
                                self.users[source],
                            )
                            .unwrap();
                    }
                }
            }

            fn pump_once(&mut self) {
                self.now_ms += 1;
                self.mirror();
                let first = self.now_ms as usize % FOUR_CORE_PEER_COUNT;
                for offset in 0..FOUR_CORE_PEER_COUNT {
                    let index = (first + offset) % FOUR_CORE_PEER_COUNT;
                    if let Err(error) = self.cores[index].pump(self.now_ms) {
                        let statuses = self
                            .cores
                            .iter()
                            .map(|core| core.coordinator.status())
                            .collect::<Vec<_>>();
                        panic!(
                            "four-core pump {index} at {} failed: {error:?}; statuses={statuses:?}",
                            self.now_ms
                        );
                    }
                    self.mirror();
                }
            }

            fn stage_unstarted_routed_ticket(&mut self) -> (usize, SteamUserId) {
                for _ in 0..160 {
                    self.now_ms += 1;
                    self.mirror();
                    let first = self.now_ms as usize % FOUR_CORE_PEER_COUNT;
                    for offset in 0..FOUR_CORE_PEER_COUNT {
                        let index = (first + offset) % FOUR_CORE_PEER_COUNT;
                        self.cores[index].pump(self.now_ms).unwrap_or_else(|error| {
                            panic!(
                                "four-core staging pump {index} at {} failed: {error:?}",
                                self.now_ms
                            )
                        });
                        self.mirror();
                        if self.cores.iter().all(|core| core.roster_auth.is_some())
                            && let Some((client, rejected_user)) = (1..FOUR_CORE_PEER_COUNT)
                                .find_map(|client| {
                                    self.cores[client]
                                        .ticket_exchanges
                                        .iter()
                                        .flatten()
                                        .find(|exchange| {
                                            matches!(
                                                exchange.route,
                                                TicketRoute::ViaAuthority { .. }
                                            ) && exchange.sent_sequence.is_none()
                                        })
                                        .map(|exchange| (client, exchange.lease.remote_user))
                                })
                        {
                            return (client, rejected_user);
                        }
                    }
                }
                panic!("four-core fixture did not expose an unstarted routed ticket");
            }

            fn pump_until(&mut self, limit: usize, predicate: impl Fn(&Self) -> bool) {
                for _ in 0..limit {
                    if predicate(self) {
                        return;
                    }
                    self.pump_once();
                }
                let statuses = self
                    .cores
                    .iter()
                    .map(|core| core.coordinator.status())
                    .collect::<Vec<_>>();
                let ticket_counts = self
                    .cores
                    .iter()
                    .map(|core| core.ticket_exchanges.iter().flatten().count())
                    .collect::<Vec<_>>();
                let authentication_counts = self
                    .cores
                    .iter()
                    .map(|core| core.authenticated.iter().flatten().count())
                    .collect::<Vec<_>>();
                assert!(
                    predicate(self),
                    "four-core fixture did not converge: statuses={statuses:?}, tickets={ticket_counts:?}, authenticated={authentication_counts:?}"
                );
            }
        }

        include!("native_online_app_core_fixture_tests.in.rs");

        fn test_member(
            user: SteamUserId,
            peer_id: PeerId,
            character: u16,
            team: u8,
        ) -> OnlineRosterMember {
            OnlineRosterMember::new(
                peer_id,
                user.authenticated(),
                1,
                true,
                &[OnlineSeatSelection {
                    team: crate::network_protocol::TeamId::new(team).unwrap(),
                    character: DefinitionId::new(character).unwrap(),
                    style: DefinitionId::new(0).unwrap(),
                    equipment: DefinitionId::new(0).unwrap(),
                }],
            )
            .unwrap()
        }

        fn admitted_endpoint(
            host_user: SteamUserId,
            remote_user: SteamUserId,
            remote_peer: PeerId,
        ) -> NativeOnlineEndpoint {
            let lobby = SteamLobbyId::new(705).unwrap();
            let network = crate::steam_transport::FakeSteamTransportNetwork::new(16).unwrap();
            let session = crate::steam_transport::SteamP2pSession {
                lobby,
                authority_user: host_user,
                role: crate::steam_transport::SteamTransportRole::ListenAuthority,
                virtual_port: 0,
            };
            let mut host = network
                .create_transport(
                    host_user,
                    session,
                    crate::steam_transport::SteamTransportConfig::default(),
                    0,
                )
                .unwrap();
            let mut remote = network
                .create_transport(
                    remote_user,
                    crate::steam_transport::SteamP2pSession {
                        role: crate::steam_transport::SteamTransportRole::Client,
                        ..session
                    },
                    crate::steam_transport::SteamTransportConfig::default(),
                    0,
                )
                .unwrap();
            host.set_allowed_incoming_users(&[remote_user]).unwrap();
            host.start_listening().unwrap();
            let connection = remote
                .connect_p2p(
                    AuthenticatedSteamPeer {
                        lobby,
                        user: host_user,
                        license_owner_user: host_user,
                        authenticated_user: host_user.authenticated(),
                        local_seats: 1,
                        purpose: AdmissionPurpose::Initial,
                    },
                    0,
                )
                .unwrap();
            host.pump(1).unwrap();
            assert!(matches!(
                host.poll_event(),
                Some(crate::steam_transport::SteamTransportEvent::IncomingPending {
                    connection: observed,
                    ..
                }) if observed == connection
            ));
            host.admit_incoming(
                connection,
                AuthenticatedSteamPeer {
                    lobby,
                    user: remote_user,
                    license_owner_user: host_user,
                    authenticated_user: remote_user.authenticated(),
                    local_seats: 1,
                    purpose: AdmissionPurpose::Initial,
                },
                1,
            )
            .unwrap();
            host.pump(2).unwrap();
            remote.pump(2).unwrap();
            assert!(matches!(
                host.poll_event(),
                Some(crate::steam_transport::SteamTransportEvent::ConnectionReady {
                    connection: observed,
                    ..
                }) if observed == connection
            ));
            NativeOnlineEndpoint {
                peer_id: remote_peer,
                reconnect: false,
                admitted: host.take_endpoint(connection).unwrap(),
            }
        }

        #[test]
        fn fake_auth_bus_routes_only_the_exact_active_same_user_generation() {
            let lobby = SteamLobbyId::new(699).unwrap();
            let sender = SteamUserId::new(697).unwrap();
            let recipient = SteamUserId::new(698).unwrap();
            let sender_peer = PeerId::new(696).unwrap();
            let bus = FakeAuthSignalBus::new();
            let old_sender = bus.register(sender).unwrap();
            let recipient_endpoint = bus.register(recipient).unwrap();
            let admission = AuthSignalAdmission {
                active_lobby: Some(lobby),
                users: [Some(sender), None, None, None],
            };
            recipient_endpoint.refresh_policy(admission);
            old_sender.refresh_policy(AuthSignalAdmission {
                active_lobby: Some(lobby),
                users: [Some(recipient), None, None, None],
            });
            old_sender
                .send_ticket(
                    AuthTicketSignal::new(
                        lobby,
                        sender,
                        recipient,
                        sender_peer,
                        AdmissionPurpose::Initial,
                        1,
                        1,
                        None,
                        &[1, 2, 3],
                    )
                    .unwrap(),
                )
                .unwrap();

            let replacement = bus.register(sender).unwrap();
            replacement.refresh_policy(AuthSignalAdmission {
                active_lobby: Some(lobby),
                users: [Some(recipient), None, None, None],
            });
            drop(old_sender);
            assert!(
                recipient_endpoint.receive().unwrap().is_empty(),
                "an envelope attributed to the retired source generation is stale"
            );

            replacement
                .send_ticket(
                    AuthTicketSignal::new(
                        lobby,
                        sender,
                        recipient,
                        sender_peer,
                        AdmissionPurpose::Initial,
                        1,
                        1,
                        None,
                        &[4, 5, 6],
                    )
                    .unwrap(),
                )
                .unwrap();
            let received = recipient_endpoint.receive().unwrap();
            assert_eq!(received.len(), 1);
            assert!(matches!(
                received.first(),
                Some(AuthSignalIngress::Accepted {
                    source,
                    signal: PreGameSignal::Ticket(signal),
                }) if *source == sender && signal.ticket() == [4, 5, 6]
            ));
        }

        #[test]
        fn two_native_cores_create_join_authenticate_and_bind_one_physical_generation() {
            let mut pair = FakeNativeCorePair::new();
            pair.pump_until_authenticated_endpoints();

            assert_eq!(
                pair.host.coordinator.status().role,
                Some(OnlineLobbyRole::ListenAuthority)
            );
            assert_eq!(
                pair.client.coordinator.status().role,
                Some(OnlineLobbyRole::Client)
            );
            assert!(pair.host.endpoints.is_empty());
            assert!(pair.client.endpoints.is_empty());
            let host_connection = pair
                .host
                .coordinator
                .control_connection_for_user(pair.client_user)
                .unwrap();
            let client_connection = pair
                .client
                .coordinator
                .control_connection_for_user(pair.host_user)
                .unwrap();
            assert_eq!(
                host_connection, client_connection,
                "the two independent cores quarantine opposite ends of one physical generation"
            );
            assert_eq!(pair.host.coordinator.status().secure_remote_peers, 1);
            assert_eq!(pair.client.coordinator.status().secure_remote_peers, 1);
        }

        #[test]
        fn transient_setup_failure_retries_with_fresh_tickets_and_same_lobby() {
            let mut pair = FakeNativeCorePair::new();
            pair.pump_until(40, |pair| {
                pair.host.ticket_exchanges.iter().flatten().count() == 1
                    && pair.client.ticket_exchanges.iter().flatten().count() == 1
            });
            let first_connection = pair
                .client
                .coordinator
                .control_connection_for_user(pair.host_user)
                .unwrap();
            let first_host_ticket = pair.host.ticket_exchanges[0].unwrap().lease.handle;
            let first_client_ticket = pair.client.ticket_exchanges[0].unwrap().lease.handle;

            pair.network
                .disconnect_locally(first_connection, pair.client_user)
                .unwrap();
            pair.pump_once();
            assert_eq!(pair.host.coordinator.status().lobby, Some(pair.lobby));
            assert_eq!(pair.client.coordinator.status().lobby, Some(pair.lobby));
            assert!(pair.host_control.cancelled_ticket(first_host_ticket));
            assert!(pair.client_control.cancelled_ticket(first_client_ticket));

            pair.now_ms += crate::steam_transport::CONTROL_RETRY_DELAY_MS;
            pair.pump_until_authenticated_endpoints();
            let replacement = pair
                .client
                .coordinator
                .control_connection_for_user(pair.host_user)
                .unwrap();
            assert_ne!(replacement, first_connection);
            assert_ne!(
                pair.host.ticket_exchanges[0].unwrap().lease.handle,
                first_host_ticket
            );
            assert_ne!(
                pair.client.ticket_exchanges[0].unwrap().lease.handle,
                first_client_ticket
            );
        }

        #[test]
        fn exhausted_pregame_socket_retry_resecures_the_exact_peer_in_place() {
            let mut pair = FakeNativeCorePair::new();
            pair.pump_until_authenticated_endpoints();
            let first_connection = pair
                .client
                .coordinator
                .control_connection_for_user(pair.host_user)
                .unwrap();

            // Consume the generation's one automatic retry, then fail that
            // replacement as well so both cores must expose a manual Retry.
            pair.network
                .disconnect_locally(first_connection, pair.client_user)
                .unwrap();
            pair.pump_once();
            pair.now_ms += crate::steam_transport::CONTROL_RETRY_DELAY_MS;
            pair.pump_until_authenticated_endpoints();
            let exhausted_connection = pair
                .client
                .coordinator
                .control_connection_for_user(pair.host_user)
                .unwrap();
            assert_ne!(exhausted_connection, first_connection);
            let exhausted_host_ticket = pair.host.ticket_exchanges[0].unwrap().lease.handle;
            let exhausted_client_ticket = pair.client.ticket_exchanges[0].unwrap().lease.handle;

            pair.network
                .disconnect_locally(exhausted_connection, pair.client_user)
                .unwrap();
            pair.pump_until(20, |pair| {
                [
                    (&pair.host, pair.client_user),
                    (&pair.client, pair.host_user),
                ]
                .into_iter()
                .all(|(core, user)| {
                    core.pending_steam_setup_retries
                        .iter()
                        .flatten()
                        .any(|pending| {
                            pending.user == user
                                && pending.connection == Some(exhausted_connection)
                                && pending.kind == PendingSteamSetupRetryKind::ClosedControl
                        })
                })
            });

            for core in [&pair.host, &pair.client] {
                let view = core.view_model();
                assert_eq!(view.screen, NativeOnlineScreen::Error);
                assert!(view.failure.is_some_and(|failure| {
                    failure.code == OnlineFailureCode::ConnectionTimedOut
                        && failure.severity == OnlineFailureSeverity::Recoverable
                        && failure.recovery == OnlineRecoveryAction::Retry
                }));
                assert_eq!(core.coordinator.status().lobby, Some(pair.lobby));
            }
            assert_eq!(pair.network.resource_counts().links, 0);
            assert!(pair.host_control.cancelled_ticket(exhausted_host_ticket));
            assert!(
                pair.client_control
                    .cancelled_ticket(exhausted_client_ticket)
            );

            // Only the client must press Retry: it is the sole connection
            // originator. The authority keeps the attributed error/passive
            // wait until that inbound generation authenticates successfully.
            pair.host_control
                .set_automatic_auth_callbacks(false)
                .unwrap();
            pair.client_control
                .set_automatic_auth_callbacks(false)
                .unwrap();
            pair.client
                .execute(NativeOnlineCommand::RetrySteamSetup, pair.now_ms)
                .unwrap();
            assert_eq!(pair.client.view_model().screen, NativeOnlineScreen::Lobby);
            assert_eq!(pair.host.view_model().screen, NativeOnlineScreen::Error);
            assert_eq!(pair.network.resource_counts().links, 0);

            pair.now_ms += crate::steam_transport::CONTROL_RETRY_DELAY_MS;
            pair.pump_until(40, |pair| {
                pair.host_control.pending_auth_validation_count() != 0
                    && pair.client_control.pending_auth_validation_count() != 0
                    && pair
                        .host
                        .coordinator
                        .control_connection_for_user(pair.client_user)
                        .is_some_and(|connection| connection != exhausted_connection)
                    && pair
                        .client
                        .coordinator
                        .control_connection_for_user(pair.host_user)
                        .is_some_and(|connection| connection != exhausted_connection)
                    && pair.host.coordinator.status().secure_remote_peers == 0
                    && pair.client.coordinator.status().secure_remote_peers == 0
            });
            let in_flight_connection = pair
                .host
                .coordinator
                .control_connection_for_user(pair.client_user)
                .unwrap();

            // A user can press the authority's still-visible Retry while the
            // client-originated replacement is already authenticating. That
            // stale action clears only the old error; it cannot reject or
            // replace the newer exact generation.
            pair.host
                .execute(NativeOnlineCommand::RetrySteamSetup, pair.now_ms)
                .unwrap();
            assert_eq!(pair.host.runtime_failure, None);
            assert_eq!(
                pair.host
                    .coordinator
                    .control_connection_for_user(pair.client_user),
                Some(in_flight_connection)
            );
            pair.host_control
                .release_auth_validation(pair.client_user)
                .unwrap();
            pair.client_control
                .release_auth_validation(pair.host_user)
                .unwrap();
            pair.host_control
                .set_automatic_auth_callbacks(true)
                .unwrap();
            pair.client_control
                .set_automatic_auth_callbacks(true)
                .unwrap();
            pair.pump_until_authenticated_endpoints();
            let recovered_connection = pair
                .client
                .coordinator
                .control_connection_for_user(pair.host_user)
                .unwrap();
            assert_eq!(recovered_connection, in_flight_connection);
            assert_ne!(recovered_connection, exhausted_connection);
            assert_eq!(
                pair.host
                    .coordinator
                    .control_connection_for_user(pair.client_user),
                Some(recovered_connection)
            );
            assert_eq!(pair.host.runtime_failure, None);
            assert_eq!(pair.client.runtime_failure, None);
            assert_ne!(
                pair.host.ticket_exchanges[0].unwrap().lease.handle,
                exhausted_host_ticket
            );
            assert_ne!(
                pair.client.ticket_exchanges[0].unwrap().lease.handle,
                exhausted_client_ticket
            );
            assert_eq!(pair.network.resource_counts().links, 1);
        }

        #[test]
        fn transient_auth_rejection_stays_retryable_without_signal_isolation() {
            let mut transient = FakeNativeCorePair::new();
            transient
                .host_control
                .set_auth_outcome(
                    transient.client_user,
                    FakeAuthOutcome {
                        license_owner_user: transient.client_user,
                        validation: Err(
                            crate::steam_platform::AuthValidationFailure::UserNotConnected,
                        ),
                        license: LicenseStatus::HasLicense,
                    },
                )
                .unwrap();
            transient.pump_until(80, |pair| {
                pair.host
                    .pending_steam_setup_retries
                    .iter()
                    .flatten()
                    .any(|pending| pending.user == pair.client_user)
            });
            assert!(
                transient
                    .host
                    .signal_rejected_users
                    .iter()
                    .all(Option::is_none)
            );
            assert_eq!(
                transient.host.coordinator.status().lobby,
                Some(transient.lobby)
            );
            assert_eq!(transient.network.resource_counts().links, 1);
        }

        #[test]
        fn four_peer_auth_retry_replaces_only_failed_star_link() {
            let mut quartet = FakeNativeCoreQuartet::new();
            let failed_index = 2_usize;
            quartet.controls[FOUR_CORE_HOST]
                .set_automatic_auth_callbacks(false)
                .unwrap();
            quartet.controls[FOUR_CORE_HOST]
                .set_auth_outcome(
                    quartet.users[failed_index],
                    FakeAuthOutcome {
                        license_owner_user: quartet.users[failed_index],
                        validation: Err(
                            crate::steam_platform::AuthValidationFailure::VacCheckTimedOut,
                        ),
                        license: LicenseStatus::HasLicense,
                    },
                )
                .unwrap();
            quartet.pump_until(160, |quartet| {
                quartet.controls[FOUR_CORE_HOST].pending_auth_validation_count() == 3
            });
            for index in [1_usize, 3_usize] {
                quartet.controls[FOUR_CORE_HOST]
                    .release_auth_validation(quartet.users[index])
                    .unwrap();
            }
            quartet.controls[FOUR_CORE_HOST]
                .release_auth_validation(quartet.users[failed_index])
                .unwrap();
            quartet.pump_until(240, |quartet| {
                quartet.cores[FOUR_CORE_HOST]
                    .pending_steam_setup_retries
                    .iter()
                    .flatten()
                    .any(|pending| pending.user == quartet.users[failed_index])
                    && [1_usize, 3_usize].into_iter().all(|index| {
                        quartet.cores[FOUR_CORE_HOST]
                            .coordinator
                            .status()
                            .secure_remote_peers
                            >= 2
                            && quartet.cores[index]
                                .coordinator
                                .status()
                                .secure_remote_peers
                                == 1
                    })
            });
            let healthy_connections = [1_usize, 3_usize].map(|index| {
                quartet.cores[FOUR_CORE_HOST]
                    .coordinator
                    .control_connection_for_user(quartet.users[index])
                    .unwrap()
            });
            let failed_connection = quartet.cores[FOUR_CORE_HOST]
                .coordinator
                .control_connection_for_user(quartet.users[failed_index])
                .unwrap();
            quartet.controls[FOUR_CORE_HOST]
                .set_auth_outcome(
                    quartet.users[failed_index],
                    FakeAuthOutcome::accepted(quartet.users[failed_index]),
                )
                .unwrap();
            quartet.controls[FOUR_CORE_HOST]
                .set_automatic_auth_callbacks(true)
                .unwrap();
            quartet.cores[FOUR_CORE_HOST]
                .execute(NativeOnlineCommand::RetrySteamSetup, quartet.now_ms)
                .unwrap();
            quartet.pump_until(800, |quartet| {
                quartet.cores.iter().enumerate().all(|(index, core)| {
                    let required = if index == FOUR_CORE_HOST { 3 } else { 1 };
                    core.coordinator.status().secure_remote_peers == required
                })
            });

            assert_eq!(quartet.network.resource_counts().links, 3);
            for (ordinal, index) in [1_usize, 3_usize].into_iter().enumerate() {
                assert_eq!(
                    quartet.cores[FOUR_CORE_HOST]
                        .coordinator
                        .control_connection_for_user(quartet.users[index]),
                    Some(healthy_connections[ordinal])
                );
            }
            assert_ne!(
                quartet.cores[FOUR_CORE_HOST]
                    .coordinator
                    .control_connection_for_user(quartet.users[failed_index]),
                Some(failed_connection)
            );
            assert_eq!(
                quartet.cores[FOUR_CORE_HOST].coordinator.status().lobby,
                Some(quartet.lobby)
            );
            assert!(
                quartet
                    .cores
                    .iter()
                    .all(|core| core.signal_rejected_users.iter().all(Option::is_none))
            );
        }

        #[test]
        fn roster_timeout_retry_extends_the_retained_native_session() {
            let mut quartet = FakeNativeCoreQuartet::new();
            let retrying_client = 1_usize;
            quartet.pump_until(160, |quartet| {
                quartet.cores[retrying_client]
                    .coordinator
                    .status()
                    .secure_remote_peers
                    == 1
            });
            quartet.controls[retrying_client]
                .set_automatic_auth_callbacks(false)
                .unwrap();
            quartet.pump_until(160, |quartet| {
                quartet.cores[retrying_client]
                    .platform
                    .active_roster_authentication_count()
                    == 2
                    && quartet.controls[retrying_client].pending_auth_validation_count() == 2
            });

            // Validate one routed peer normally and retain exactly one native
            // callback-pending session for the manual timeout extension.
            quartet.controls[retrying_client]
                .release_auth_validation(quartet.users[3])
                .unwrap();
            quartet.pump_once();
            assert_eq!(
                quartet.cores[retrying_client]
                    .platform
                    .active_roster_authentication_count(),
                2
            );
            assert_eq!(
                quartet.controls[retrying_client].active_auth_session_count(),
                3,
                "one direct authority session plus two routed roster sessions remain active"
            );

            quartet.now_ms += crate::steam_platform::DEFAULT_AUTH_INTENT_TTL_MS;
            quartet.cores[retrying_client].pump(quartet.now_ms).unwrap();
            assert!(
                quartet.cores[retrying_client]
                    .pending_steam_setup_retries
                    .iter()
                    .flatten()
                    .any(|pending| {
                        pending.user == quartet.users[2]
                            && pending.kind == PendingSteamSetupRetryKind::RosterLease
                    })
            );
            assert!(
                quartet.cores[retrying_client]
                    .signal_rejected_users
                    .iter()
                    .all(Option::is_none)
            );
            let sessions_before_retry =
                quartet.controls[retrying_client].active_auth_session_count();

            quartet.cores[retrying_client]
                .execute(NativeOnlineCommand::RetrySteamSetup, quartet.now_ms + 1)
                .unwrap();
            assert_eq!(
                quartet.controls[retrying_client].active_auth_session_count(),
                sessions_before_retry,
                "manual retry must extend the retained lease, not BeginAuthSession twice"
            );
            quartet.controls[retrying_client]
                .release_auth_validation(quartet.users[2])
                .unwrap();
            quartet.now_ms += 1;
            quartet.pump_until(240, |quartet| {
                quartet.cores.iter().all(|core| {
                    core.coordinator.status().verified_remote_accounts == 3
                        && core.coordinator.status().required_remote_accounts == 3
                })
            });
        }

        #[test]
        fn roster_callback_after_timeout_supersedes_retry_before_user_action() {
            let mut quartet = FakeNativeCoreQuartet::new();
            let client = 1_usize;
            let late_user = quartet.users[2];
            quartet.pump_until(160, |quartet| {
                quartet.cores[client]
                    .coordinator
                    .status()
                    .secure_remote_peers
                    == 1
            });
            quartet.controls[client]
                .set_automatic_auth_callbacks(false)
                .unwrap();
            quartet.pump_until(160, |quartet| {
                quartet.cores[client]
                    .platform
                    .active_roster_authentication_count()
                    == 2
                    && quartet.controls[client].pending_auth_validation_count() == 2
            });

            quartet.controls[client]
                .release_auth_validation(quartet.users[3])
                .unwrap();
            quartet.pump_once();
            quartet.now_ms += crate::steam_platform::DEFAULT_AUTH_INTENT_TTL_MS;
            quartet.cores[client].pump(quartet.now_ms).unwrap();
            assert!(
                quartet.cores[client]
                    .pending_steam_setup_retries
                    .iter()
                    .flatten()
                    .any(|pending| {
                        pending.user == late_user
                            && pending.kind == PendingSteamSetupRetryKind::RosterLease
                    })
            );

            let sessions_before_callback = quartet.controls[client].active_auth_session_count();
            quartet.controls[client]
                .release_auth_validation(late_user)
                .unwrap();
            quartet.pump_until(240, |quartet| {
                quartet.cores[client]
                    .platform
                    .roster_authentication_is_validated(late_user)
                    && quartet.cores[client]
                        .pending_steam_setup_retries
                        .iter()
                        .flatten()
                        .all(|pending| {
                            pending.user != late_user
                                || pending.kind != PendingSteamSetupRetryKind::RosterLease
                        })
            });

            assert_eq!(quartet.cores[client].runtime_failure, None);
            assert_eq!(
                quartet.controls[client].active_auth_session_count(),
                sessions_before_callback,
                "late approval must retain the validated native session"
            );
            assert!(
                quartet.cores[client]
                    .execute(NativeOnlineCommand::RetrySteamSetup, quartet.now_ms + 1)
                    .is_err(),
                "the superseded retry must no longer be actionable"
            );
            assert!(
                quartet.cores[client]
                    .platform
                    .roster_authentication_is_validated(late_user),
                "a stale Retry command must not retire the newly approved proof"
            );
            quartet.pump_until(240, |quartet| {
                quartet.cores.iter().all(|core| {
                    core.coordinator.status().verified_remote_accounts == 3
                        && core.coordinator.status().required_remote_accounts == 3
                })
            });
        }

        fn assert_permanent_routed_rejection_aborts_full_roster(
            quartet: &mut FakeNativeCoreQuartet,
            rejected_user: SteamUserId,
        ) {
            quartet.pump_until(400, |quartet| {
                quartet.cores.iter().all(|core| {
                    core.signal_rejected_users.contains(&Some(rejected_user))
                        && core.roster_auth.is_none()
                        && core.platform.active_roster_authentication_count() == 0
                        && core.ticket_exchanges.iter().flatten().all(|exchange| {
                            !matches!(exchange.route, TicketRoute::ViaAuthority { .. })
                        })
                }) && (1..FOUR_CORE_PEER_COUNT).all(|index| {
                    quartet.cores[FOUR_CORE_HOST]
                        .coordinator
                        .control_outbox_is_empty(quartet.users[index])
                        && quartet.cores[index]
                            .coordinator
                            .control_outbox_is_empty(quartet.users[FOUR_CORE_HOST])
                })
            });

            let retired_epoch = quartet.cores[FOUR_CORE_HOST]
                .retired_account_auth_epoch
                .expect("authority retires the rejected roster-auth epoch");
            for core in &quartet.cores {
                assert_eq!(core.coordinator.status().phase, OnlineLobbyPhase::Lobby);
                assert_eq!(core.coordinator.status().lobby, Some(quartet.lobby));
                assert_eq!(core.retired_account_auth_epoch, Some(retired_epoch));
                assert!(core.manifest_transaction.is_none());
                assert!(core.committed_roster.is_none());
                assert!(core.events.iter().any(|event| {
                    matches!(
                        event,
                        OnlineLobbyEvent::RosterPeerAuthenticationRejected { user, .. }
                            if *user == rejected_user
                    )
                }));
                assert!(core.events.iter().any(|event| {
                    matches!(
                        event,
                        OnlineLobbyEvent::Failure(OnlineFailure {
                            code: OnlineFailureCode::AuthenticationFailed,
                            ..
                        })
                    )
                }));
            }
            assert_eq!(quartet.network.resource_counts().links, 3);

            // The permanent roster verdict is a lobby-lifetime latch, not a
            // transient retry. Pump well past the delivery point to prove the
            // authority cannot silently start a new epoch and split peers
            // between old and new account views.
            for _ in 0..64 {
                quartet.pump_once();
            }
            assert!(quartet.cores.iter().all(|core| {
                core.coordinator.status().phase == OnlineLobbyPhase::Lobby
                    && core.roster_auth.is_none()
                    && core.retired_account_auth_epoch == Some(retired_epoch)
                    && core.signal_rejected_users.contains(&Some(rejected_user))
            }));
        }

        #[test]
        fn four_core_immediate_invalid_routed_ticket_aborts_every_roster_participant() {
            let mut quartet = FakeNativeCoreQuartet::new();
            let (rejecting_client, rejected_user) = quartet.stage_unstarted_routed_ticket();
            quartet.controls[rejecting_client]
                .set_auth_session_start_failure(
                    rejected_user,
                    crate::steam_platform::AuthSessionStartFailure::InvalidTicket,
                )
                .unwrap();

            assert_permanent_routed_rejection_aborts_full_roster(&mut quartet, rejected_user);
        }

        #[test]
        fn four_core_async_invalid_routed_ticket_aborts_every_roster_participant() {
            let mut quartet = FakeNativeCoreQuartet::new();
            let (rejecting_client, rejected_user) = quartet.stage_unstarted_routed_ticket();
            quartet.controls[rejecting_client]
                .set_auth_outcome(
                    rejected_user,
                    FakeAuthOutcome {
                        license_owner_user: rejected_user,
                        validation: Err(
                            crate::steam_platform::AuthValidationFailure::TicketInvalid,
                        ),
                        license: LicenseStatus::HasLicense,
                    },
                )
                .unwrap();

            assert_permanent_routed_rejection_aborts_full_roster(&mut quartet, rejected_user);
        }

        #[test]
        fn transient_close_during_manifest_retires_the_frozen_generation_unconditionally() {
            let mut pair = FakeNativeCorePair::new();
            pair.pump_until_authenticated_endpoints();
            pair.host
                .execute(NativeOnlineCommand::SetReady(true), pair.now_ms)
                .unwrap();
            pair.client
                .execute(NativeOnlineCommand::SetReady(true), pair.now_ms)
                .unwrap();
            pair.mirror();
            pair.pump_until(80, |pair| {
                pair.host.coordinator.status().all_members_ready
                    && pair.client.coordinator.status().all_members_ready
                    && pair.host.coordinator.status().input_delay_calibration.state
                        == crate::network_quality::InputDelayCalibrationState::Ready
            });

            let connection = pair
                .host
                .coordinator
                .control_connection_for_user(pair.client_user)
                .unwrap();
            let calibration = pair.host.coordinator.status().input_delay_calibration;
            let mut options = OnlineManifestOptions::casual_listen(
                crate::network_protocol::MatchId::new(*b"manifest-close01").unwrap(),
                pair.host_member.peer_id,
                DefinitionId::new(0).unwrap(),
                DefinitionId::new(1).unwrap(),
                0xAFC0_7603,
                SimTick(240),
            );
            options.input_delay_ticks = calibration.selected_input_delay_ticks.unwrap();
            options.rollback_limit_ticks = crate::network_protocol::MAX_NORMAL_ROLLBACK_TICKS;
            pair.host
                .execute(
                    NativeOnlineCommand::CommitManifest {
                        options,
                        current_tick: SimTick(120),
                    },
                    pair.now_ms,
                )
                .unwrap();

            // Consume the coordinator's commit locally, freezing the exact
            // generation, but do not yet give the peer a chance to consume the
            // Prepare frame.
            pair.now_ms += 1;
            pair.mirror();
            pair.host.pump(pair.now_ms).unwrap();
            let transaction = pair
                .host
                .manifest_transaction
                .expect("authority froze a manifest transaction");
            assert_eq!(transaction.stage, ManifestTransactionStage::Preparing);

            // A local early failure schedules a replacement and removes the
            // frozen connection from the active mapping before ManifestAborted
            // is drained. Best-effort Abort delivery must not turn that normal
            // rollback into a pump failure.
            pair.network
                .disconnect_locally(connection, pair.host_user)
                .unwrap();
            pair.now_ms += 1;
            pair.host.pump(pair.now_ms).unwrap();

            assert_eq!(
                pair.host.coordinator.status().phase,
                OnlineLobbyPhase::Lobby
            );
            assert!(pair.host.coordinator.match_config().is_none());
            assert!(pair.host.committed_roster.is_none());
            assert!(pair.host.manifest_transaction.is_none());
            assert_eq!(pair.host.retired_manifest_transaction, Some(transaction.id));
            assert!(pair.host.endpoints.is_empty());
            assert!(pair.host.signal_rejected_users.iter().all(Option::is_none));
        }

        #[test]
        fn two_native_cores_commit_identical_manifest_and_authenticated_rosters() {
            let mut pair = FakeNativeCorePair::new();
            pair.pump_until_authenticated_endpoints();
            let match_id = crate::network_protocol::MatchId::new(*b"two-core-match01").unwrap();
            pair.ready_and_commit(match_id);

            let host_config = pair.host.coordinator.match_config().unwrap();
            let client_config = pair.client.coordinator.match_config().unwrap();
            assert_eq!(host_config.manifest, client_config.manifest);
            assert_eq!(
                host_config.snapshot_contract,
                client_config.snapshot_contract
            );
            assert_eq!(
                (
                    host_config.local_setup.rule_index,
                    host_config.local_setup.arena_index,
                    host_config.local_setup.selected_character_fighter,
                    host_config.local_setup.slots,
                    host_config.local_setup.replay_seed,
                ),
                (
                    client_config.local_setup.rule_index,
                    client_config.local_setup.arena_index,
                    client_config.local_setup.selected_character_fighter,
                    client_config.local_setup.slots,
                    client_config.local_setup.replay_seed,
                )
            );
            assert_eq!(host_config.manifest.match_id, match_id);
            let host_roster = pair.host.committed_roster.unwrap();
            let client_roster = pair.client.committed_roster.unwrap();
            assert_eq!(host_roster, client_roster);
            assert_eq!(host_roster.len(), 2);
            assert_eq!(
                host_roster.iter().collect::<Vec<_>>(),
                vec![
                    AuthenticatedPeer {
                        peer_id: pair.host_member.peer_id,
                        user_id: pair.host_user.authenticated(),
                    },
                    AuthenticatedPeer {
                        peer_id: pair.client_member.peer_id,
                        user_id: pair.client_user.authenticated(),
                    },
                ]
            );
        }

        #[test]
        fn four_native_cores_authenticate_full_roster_and_activate_star_manifest() {
            let mut quartet = FakeNativeCoreQuartet::new();
            quartet.pump_until(400, |quartet| {
                quartet.cores.iter().enumerate().all(|(index, core)| {
                    let status = core.coordinator.status();
                    let required_direct = if index == FOUR_CORE_HOST { 3 } else { 1 };
                    status.connected_remote_peers == required_direct
                        && status.secure_remote_peers == required_direct
                        && status.required_remote_peers == required_direct
                        && status.verified_remote_accounts == 3
                        && status.required_remote_accounts == 3
                })
            });

            assert_eq!(quartet.network.resource_counts().links, 3);
            assert!(quartet.cores.iter().all(|core| core.endpoints.is_empty()));
            assert_eq!(
                quartet.cores[FOUR_CORE_HOST]
                    .authenticated
                    .iter()
                    .flatten()
                    .count(),
                FOUR_CORE_PEER_COUNT
            );
            for index in 1..FOUR_CORE_PEER_COUNT {
                let client = &quartet.cores[index];
                assert_eq!(
                    client.authenticated.iter().flatten().count(),
                    2,
                    "routed account proofs must not invent client-to-client socket bindings"
                );
                assert!(
                    client
                        .coordinator
                        .control_connection_for_user(quartet.users[FOUR_CORE_HOST])
                        .is_some()
                );
                for remote_client in 1..FOUR_CORE_PEER_COUNT {
                    if remote_client != index {
                        assert_eq!(
                            client
                                .coordinator
                                .control_connection_for_user(quartet.users[remote_client]),
                            None
                        );
                    }
                }
            }

            // Exercise declaration propagation independently of socket setup:
            // middle client, authority, last client, then first client.
            for (ready_ordinal, index) in [2, 0, 3, 1].into_iter().enumerate() {
                quartet.cores[index]
                    .execute(NativeOnlineCommand::SetReady(true), quartet.now_ms)
                    .unwrap();
                quartet.pump_once();
                if ready_ordinal + 1 < FOUR_CORE_PEER_COUNT {
                    assert!(
                        quartet
                            .cores
                            .iter()
                            .any(|core| !core.coordinator.status().all_members_ready)
                    );
                }
            }
            quartet.pump_until(120, |quartet| {
                quartet
                    .cores
                    .iter()
                    .all(|core| core.coordinator.status().all_members_ready)
                    && quartet.cores[FOUR_CORE_HOST]
                        .coordinator
                        .status()
                        .input_delay_calibration
                        .state
                        == crate::network_quality::InputDelayCalibrationState::Ready
            });

            let calibration = quartet.cores[FOUR_CORE_HOST]
                .coordinator
                .status()
                .input_delay_calibration;
            let match_id = crate::network_protocol::MatchId::new(*b"four-core-match1").unwrap();
            let mut options = OnlineManifestOptions::casual_listen(
                match_id,
                quartet.members[FOUR_CORE_HOST].peer_id,
                DefinitionId::new(0).unwrap(),
                DefinitionId::new(1).unwrap(),
                0xAFC0_7701,
                SimTick(240),
            );
            options.input_delay_ticks = calibration.selected_input_delay_ticks.unwrap();
            options.rollback_limit_ticks = crate::network_protocol::MAX_NORMAL_ROLLBACK_TICKS;
            quartet.cores[FOUR_CORE_HOST]
                .execute(
                    NativeOnlineCommand::CommitManifest {
                        options,
                        current_tick: SimTick(120),
                    },
                    quartet.now_ms,
                )
                .unwrap();
            quartet.pump_until(160, |quartet| {
                quartet.cores.iter().enumerate().all(|(index, core)| {
                    core.coordinator.match_config().is_some()
                        && core.committed_roster.is_some()
                        && core.coordinator.status().phase == OnlineLobbyPhase::Loading
                        && core.endpoints.len() == if index == FOUR_CORE_HOST { 3 } else { 1 }
                })
            });

            let committed = quartet.cores[FOUR_CORE_HOST].committed_roster.unwrap();
            let manifest = quartet.cores[FOUR_CORE_HOST]
                .coordinator
                .match_config()
                .unwrap()
                .manifest;
            let expected = quartet
                .members
                .iter()
                .map(|member| AuthenticatedPeer {
                    peer_id: member.peer_id,
                    user_id: member.authenticated_user,
                })
                .collect::<Vec<_>>();
            assert_eq!(committed.len(), FOUR_CORE_PEER_COUNT);
            assert_eq!(committed.iter().collect::<Vec<_>>(), expected);
            for core in &quartet.cores {
                assert_eq!(core.committed_roster, Some(committed));
                assert_eq!(core.coordinator.match_config().unwrap().manifest, manifest);
            }
            assert_eq!(manifest.match_id, match_id);

            let mut host_remotes = quartet.cores[FOUR_CORE_HOST]
                .endpoints
                .iter()
                .map(|endpoint| endpoint.admitted.remote_user)
                .collect::<Vec<_>>();
            host_remotes.sort_unstable_by_key(|user| user.get());
            assert_eq!(host_remotes, quartet.users[1..].to_vec());
            for index in 1..FOUR_CORE_PEER_COUNT {
                assert_eq!(
                    quartet.cores[index]
                        .endpoints
                        .front()
                        .unwrap()
                        .admitted
                        .remote_user,
                    quartet.users[FOUR_CORE_HOST]
                );
            }
            assert!(quartet.cores.iter().all(|core| {
                core.manifest_transaction.is_some_and(|transaction| {
                    transaction.stage == ManifestTransactionStage::Activated
                        && transaction.activation_deadline_ms.is_none()
                })
            }));
        }

        #[test]
        fn four_core_partial_activation_times_out_via_scoped_setup_cancel_before_release() {
            let mut quartet = FakeNativeCoreQuartet::new();
            quartet.pump_until(400, |quartet| {
                quartet.cores.iter().enumerate().all(|(index, core)| {
                    let status = core.coordinator.status();
                    let required_direct = if index == FOUR_CORE_HOST { 3 } else { 1 };
                    status.secure_remote_peers == required_direct
                        && status.verified_remote_accounts == 3
                })
            });
            for core in &mut quartet.cores {
                core.execute(NativeOnlineCommand::SetReady(true), quartet.now_ms)
                    .unwrap();
            }
            quartet.mirror();
            quartet.pump_until(120, |quartet| {
                quartet
                    .cores
                    .iter()
                    .all(|core| core.coordinator.status().all_members_ready)
                    && quartet.cores[FOUR_CORE_HOST]
                        .coordinator
                        .status()
                        .input_delay_calibration
                        .state
                        == crate::network_quality::InputDelayCalibrationState::Ready
            });

            let calibration = quartet.cores[FOUR_CORE_HOST]
                .coordinator
                .status()
                .input_delay_calibration;
            let mut options = OnlineManifestOptions::casual_listen(
                crate::network_protocol::MatchId::new(*b"partial-activate").unwrap(),
                quartet.members[FOUR_CORE_HOST].peer_id,
                DefinitionId::new(0).unwrap(),
                DefinitionId::new(1).unwrap(),
                0xAFC0_7702,
                SimTick(240),
            );
            options.input_delay_ticks = calibration.selected_input_delay_ticks.unwrap();
            options.rollback_limit_ticks = crate::network_protocol::MAX_NORMAL_ROLLBACK_TICKS;
            quartet.cores[FOUR_CORE_HOST]
                .execute(
                    NativeOnlineCommand::CommitManifest {
                        options,
                        current_tick: SimTick(120),
                    },
                    quartet.now_ms,
                )
                .unwrap();
            quartet.pump_until(80, |quartet| {
                quartet.cores[FOUR_CORE_HOST]
                    .manifest_transaction
                    .is_some_and(|transaction| {
                        transaction.stage == ManifestTransactionStage::Activating
                    })
            });
            assert!(quartet.cores.iter().all(|core| core.endpoints.is_empty()));

            // Send Activate to everyone, but allow only two clients to
            // receive-arm and return their barrier receipts.
            quartet.now_ms += 1;
            quartet.mirror();
            quartet.cores[FOUR_CORE_HOST].pump(quartet.now_ms).unwrap();
            for index in [1_usize, 2_usize] {
                quartet.cores[index].pump(quartet.now_ms).unwrap();
                quartet.mirror();
                assert_eq!(
                    quartet.cores[index]
                        .manifest_transaction
                        .expect("client retained its transaction")
                        .stage,
                    ManifestTransactionStage::Activating
                );
                assert!(quartet.cores[index].endpoints.is_empty());
            }
            assert_eq!(
                quartet.cores[3]
                    .manifest_transaction
                    .expect("delayed client retained the commit")
                    .stage,
                ManifestTransactionStage::Committing
            );

            quartet.now_ms += 1;
            for index in [1_usize, 2_usize] {
                quartet.cores[index].pump(quartet.now_ms).unwrap();
            }
            quartet.cores[FOUR_CORE_HOST].pump(quartet.now_ms).unwrap();
            let partial = quartet.cores[FOUR_CORE_HOST]
                .manifest_transaction
                .expect("authority still waits for the final receipt");
            assert_eq!(partial.stage, ManifestTransactionStage::Activating);
            assert_eq!(
                partial
                    .participants
                    .iter()
                    .flatten()
                    .filter(|participant| participant.activated)
                    .count(),
                2
            );
            assert!(quartet.cores.iter().all(|core| core.endpoints.is_empty()));

            let original_deadline_ms = partial.activation_deadline_ms.unwrap();
            quartet.controls[FOUR_CORE_HOST].emit_disconnect().unwrap();
            quartet.now_ms += 1;
            quartet.cores[FOUR_CORE_HOST].pump(quartet.now_ms).unwrap();
            assert!(
                quartet.cores[FOUR_CORE_HOST]
                    .coordinator
                    .steam_backend_reconnect_pending()
            );
            assert_eq!(
                quartet.cores[FOUR_CORE_HOST]
                    .coordinator
                    .status()
                    .start_blocker,
                Some(crate::online_lobby::OnlineStartBlocker::PreparingSteamNetwork)
            );

            // The activation deadline occurs just before the 10-second Steam
            // reconnect grace. It must remain frozen, not roll back an
            // otherwise healthy secure star while Steam is reconnecting.
            quartet.now_ms = original_deadline_ms;
            quartet.cores[FOUR_CORE_HOST].pump(quartet.now_ms).unwrap();
            assert!(
                quartet.cores[FOUR_CORE_HOST]
                    .manifest_transaction
                    .is_some_and(
                        |transaction| transaction.stage == ManifestTransactionStage::Activating
                    )
            );
            assert!(quartet.cores[FOUR_CORE_HOST].endpoints.is_empty());

            quartet.controls[FOUR_CORE_HOST].emit_connect().unwrap();
            quartet.cores[FOUR_CORE_HOST].pump(quartet.now_ms).unwrap();
            assert!(
                !quartet.cores[FOUR_CORE_HOST]
                    .coordinator
                    .steam_backend_reconnect_pending()
            );
            let deadline_ms = quartet.cores[FOUR_CORE_HOST]
                .manifest_transaction
                .expect("activation transaction resumes after Steam reconnect")
                .activation_deadline_ms
                .expect("activation retains a bounded deadline");
            assert!(deadline_ms > original_deadline_ms);

            quartet.now_ms = deadline_ms;
            quartet.cores[FOUR_CORE_HOST].pump(quartet.now_ms).unwrap();
            assert_eq!(
                quartet.cores[FOUR_CORE_HOST].coordinator.status().phase,
                OnlineLobbyPhase::Lobby
            );
            assert!(quartet.cores[FOUR_CORE_HOST].manifest_transaction.is_none());
            assert!(quartet.cores[FOUR_CORE_HOST].endpoints.is_empty());
            for index in 1..FOUR_CORE_PEER_COUNT {
                assert!(
                    !quartet.cores[FOUR_CORE_HOST]
                        .coordinator
                        .control_outbox_is_empty(quartet.users[index]),
                    "each still-live frozen participant receives a scoped SetupCancel"
                );
            }

            // Flush SetupCancel after the local rollback. The two receive-armed
            // clients return to Lobby without ever exposing an endpoint; the
            // delayed client treats any earlier queued Activate as part of the
            // same transaction and converges as well.
            quartet.now_ms += 1;
            quartet.cores[FOUR_CORE_HOST].pump(quartet.now_ms).unwrap();
            for index in 1..FOUR_CORE_PEER_COUNT {
                if let Err(error) = quartet.cores[index].pump(quartet.now_ms) {
                    panic!(
                        "client {index} failed to consume SetupCancel: {error:?}; phase={:?}, transaction={:?}, endpoints={}",
                        quartet.cores[index].coordinator.status().phase,
                        quartet.cores[index].manifest_transaction,
                        quartet.cores[index].endpoints.len(),
                    );
                }
            }
            assert!(quartet.cores.iter().all(|core| {
                core.coordinator.status().phase == OnlineLobbyPhase::Lobby
                    && core.coordinator.match_config().is_none()
                    && core.manifest_transaction.is_none()
                    && core.endpoints.is_empty()
            }));
            assert_eq!(quartet.network.resource_counts().links, 3);
        }

        #[test]
        fn manifest_timeout_aborts_both_sides_to_lobby_without_replacing_secure_socket() {
            let mut pair = FakeNativeCorePair::new();
            pair.pump_until_authenticated_endpoints();
            pair.host
                .execute(NativeOnlineCommand::SetReady(true), pair.now_ms)
                .unwrap();
            pair.client
                .execute(NativeOnlineCommand::SetReady(true), pair.now_ms)
                .unwrap();
            pair.mirror();
            pair.pump_until(80, |pair| {
                pair.host.coordinator.status().all_members_ready
                    && pair.client.coordinator.status().all_members_ready
                    && pair.host.coordinator.status().input_delay_calibration.state
                        == crate::network_quality::InputDelayCalibrationState::Ready
            });
            let connection = pair
                .host
                .coordinator
                .control_connection_for_user(pair.client_user)
                .unwrap();
            let calibration = pair.host.coordinator.status().input_delay_calibration;
            let mut options = OnlineManifestOptions::casual_listen(
                crate::network_protocol::MatchId::new(*b"manifest-abort01").unwrap(),
                pair.host_member.peer_id,
                DefinitionId::new(0).unwrap(),
                DefinitionId::new(1).unwrap(),
                0xAFC0_7602,
                SimTick(240),
            );
            options.input_delay_ticks = calibration.selected_input_delay_ticks.unwrap();
            options.rollback_limit_ticks = crate::network_protocol::MAX_NORMAL_ROLLBACK_TICKS;
            pair.host
                .execute(
                    NativeOnlineCommand::CommitManifest {
                        options,
                        current_tick: SimTick(120),
                    },
                    pair.now_ms,
                )
                .unwrap();

            pair.now_ms += 1;
            pair.mirror();
            pair.host.pump(pair.now_ms).unwrap();
            assert_eq!(
                pair.host.coordinator.status().phase,
                OnlineLobbyPhase::ManifestAgreement
            );
            let transaction = pair
                .host
                .manifest_transaction
                .expect("authority opened an AFCP manifest transaction");
            pair.now_ms += pair
                .host
                .coordinator
                .config()
                .timeouts
                .manifest_agreement_ms;
            pair.host.pump(pair.now_ms).unwrap();
            assert_eq!(
                pair.host.coordinator.status().phase,
                OnlineLobbyPhase::Lobby
            );

            // Deliver Prepare only after the authority has already timed out.
            // The subsequent scoped Abort must still converge the client
            // without relying on authority-first application pump ordering.
            pair.client.pump(pair.now_ms).unwrap();
            assert_eq!(
                pair.client.coordinator.status().phase,
                OnlineLobbyPhase::ManifestAgreement
            );
            pair.now_ms += 1;
            pair.client.pump(pair.now_ms).unwrap();
            assert_eq!(
                pair.client.coordinator.status().phase,
                OnlineLobbyPhase::ManifestAgreement
            );
            pair.host.pump(pair.now_ms).unwrap();
            pair.client.pump(pair.now_ms).unwrap();

            assert_eq!(
                pair.client.coordinator.status().phase,
                OnlineLobbyPhase::Lobby
            );
            assert!(pair.host.coordinator.match_config().is_none());
            assert!(pair.client.coordinator.match_config().is_none());
            assert_eq!(pair.host.coordinator.status().secure_remote_peers, 1);
            assert_eq!(pair.client.coordinator.status().secure_remote_peers, 1);
            assert_eq!(
                pair.host
                    .coordinator
                    .control_connection_for_user(pair.client_user),
                Some(connection)
            );
            assert_eq!(
                pair.client
                    .coordinator
                    .control_connection_for_user(pair.host_user),
                Some(connection)
            );

            assert_eq!(pair.host.retired_manifest_transaction, Some(transaction.id));
            assert_eq!(
                pair.client.retired_manifest_transaction,
                Some(transaction.id)
            );

            // Delayed frames from the retired transaction are semantically
            // accepted and ACKed as known-stale. They must not isolate the peer,
            // resurrect Loading, or replace the still-Secure physical link.
            let client_to_host =
                SteamControlIdentity::new(pair.lobby, pair.client_user, pair.host_user).unwrap();
            let host_to_client =
                SteamControlIdentity::new(pair.lobby, pair.host_user, pair.client_user).unwrap();
            pair.client
                .coordinator
                .queue_control_for_user(
                    pair.host_user,
                    SteamControlMessage::ManifestCommitAccepted {
                        identity: client_to_host,
                        transaction: transaction.id,
                        manifest_hash: transaction.manifest_hash,
                    },
                )
                .unwrap();
            pair.host
                .coordinator
                .queue_control_for_user(
                    pair.client_user,
                    SteamControlMessage::GameplayActivate {
                        identity: host_to_client,
                        transaction: transaction.id,
                        manifest_hash: transaction.manifest_hash,
                    },
                )
                .unwrap();
            pair.client
                .coordinator
                .queue_control_for_user(
                    pair.host_user,
                    SteamControlMessage::GameplayActivated {
                        identity: client_to_host,
                        transaction: transaction.id,
                        manifest_hash: transaction.manifest_hash,
                    },
                )
                .unwrap();
            pair.pump_until(20, |pair| {
                pair.host
                    .coordinator
                    .control_outbox_is_empty(pair.client_user)
                    && pair
                        .client
                        .coordinator
                        .control_outbox_is_empty(pair.host_user)
            });
            assert_eq!(
                pair.host.coordinator.status().phase,
                OnlineLobbyPhase::Lobby
            );
            assert_eq!(
                pair.client.coordinator.status().phase,
                OnlineLobbyPhase::Lobby
            );
            assert!(pair.host.signal_rejected_users.iter().all(Option::is_none));
            assert!(
                pair.client
                    .signal_rejected_users
                    .iter()
                    .all(Option::is_none)
            );
            assert_eq!(
                pair.host
                    .coordinator
                    .control_connection_for_user(pair.client_user),
                Some(connection)
            );
            assert_eq!(
                pair.client
                    .coordinator
                    .control_connection_for_user(pair.host_user),
                Some(connection)
            );
        }

        #[test]
        fn thousand_cycle_create_join_start_return_cleanup_soak_leaves_zero_resources() {
            for _ in 0..1_000 {
                let mut pair = FakeNativeCorePair::new();
                let network = pair.network.clone();
                let host_control = pair.host_control.clone();
                let client_control = pair.client_control.clone();
                pair.pump_until_authenticated_endpoints();
                pair.ready_and_commit(
                    crate::network_protocol::MatchId::new(*b"cleanup-soak-001").unwrap(),
                );
                pair.finish_confirmed_match();

                pair.now_ms += 1;
                pair.host
                    .execute(NativeOnlineCommand::ReturnToLobby, pair.now_ms)
                    .unwrap();
                pair.client
                    .execute(NativeOnlineCommand::ReturnToLobby, pair.now_ms)
                    .unwrap();
                pair.pump_until(80, |pair| {
                    pair.host.coordinator.status().phase == OnlineLobbyPhase::Lobby
                        && pair.client.coordinator.status().phase == OnlineLobbyPhase::Lobby
                });

                pair.now_ms += 1;
                pair.client
                    .execute(NativeOnlineCommand::LeaveOnline, pair.now_ms)
                    .unwrap();
                pair.host
                    .execute(NativeOnlineCommand::LeaveOnline, pair.now_ms)
                    .unwrap();
                for _ in 0..400 {
                    if pair.host.coordinator.retiring_transport_count() == 0
                        && pair.client.coordinator.retiring_transport_count() == 0
                    {
                        break;
                    }
                    pair.now_ms += 1;
                    pair.host.pump(pair.now_ms).unwrap();
                    pair.client.pump(pair.now_ms).unwrap();
                }
                assert_eq!(pair.host.coordinator.retiring_transport_count(), 0);
                assert_eq!(pair.client.coordinator.retiring_transport_count(), 0);
                assert!(pair.host.endpoints.is_empty());
                assert!(pair.client.endpoints.is_empty());
                drop(pair);

                assert_eq!(
                    network.resource_counts(),
                    crate::steam_transport::FakeSteamTransportResourceCounts::default()
                );
                assert_eq!(host_control.active_issued_ticket_count(), 0);
                assert_eq!(client_control.active_issued_ticket_count(), 0);
                assert_eq!(host_control.active_auth_session_count(), 0);
                assert_eq!(client_control.active_auth_session_count(), 0);
            }
        }

        #[test]
        fn two_native_cores_client_first_rematch_uses_a_new_exact_generation() {
            let mut pair = FakeNativeCorePair::new();
            pair.rematch_and_commit_second_generation(true);
        }

        #[test]
        fn two_native_cores_owner_first_rematch_uses_a_new_exact_generation() {
            let mut pair = FakeNativeCorePair::new();
            pair.rematch_and_commit_second_generation(false);
        }

        #[test]
        fn auth_signal_session_waits_for_remote_lobby_declaration() {
            let remote = SteamUserId::new(689).unwrap();
            let declaration = test_member(remote, PeerId::new(688).unwrap(), 0, 1);
            let loadout = crate::steam_platform::MemberLoadoutDeclaration::new(
                &crate::online_roster::encode_member_declaration(&declaration),
            )
            .unwrap();

            assert!(!member_can_open_auth_signal_session(&LobbyMember {
                user: remote,
                readiness: MemberReadiness::Pending,
                loadout: None,
            }));
            assert!(!member_can_open_auth_signal_session(&LobbyMember {
                user: remote,
                readiness: MemberReadiness::Declared {
                    ready: false,
                    local_seats: 1,
                },
                loadout: None,
            }));
            assert!(member_can_open_auth_signal_session(&LobbyMember {
                user: remote,
                readiness: MemberReadiness::Declared {
                    ready: false,
                    local_seats: 1,
                },
                loadout: Some(loadout),
            }));
        }

        #[test]
        fn signal_session_requests_defer_until_lobby_membership_is_known() {
            let lobby = SteamLobbyId::new(690).unwrap();
            let member = SteamUserId::new(691).unwrap();
            let pending_member = SteamUserId::new(692).unwrap();
            let mut policy = SignalAdmissionPolicy {
                active_lobby: Some(lobby),
                users: [Some(member), None, None, None],
                quarantined: [None; MAX_STEAM_LOBBY_MEMBERS],
            };

            assert_eq!(
                classify_signal_session_request(policy, Some(member)),
                SignalSessionRequestAction::Accept
            );
            assert_eq!(
                classify_signal_session_request(policy, Some(pending_member)),
                SignalSessionRequestAction::Defer
            );
            assert_eq!(
                classify_signal_session_request(policy, None),
                SignalSessionRequestAction::Reject
            );

            policy.quarantine(member);
            assert_eq!(
                classify_signal_session_request(policy, Some(member)),
                SignalSessionRequestAction::Reject
            );
            policy.active_lobby = None;
            assert_eq!(
                classify_signal_session_request(policy, Some(pending_member)),
                SignalSessionRequestAction::Reject
            );
        }

        #[test]
        fn session_hello_priming_retries_and_resets_at_membership_boundaries() {
            let lobby = SteamLobbyId::new(693).unwrap();
            let retained = SteamUserId::new(694).unwrap();
            let replacement = SteamUserId::new(695).unwrap();
            let policy = SignalAdmissionPolicy {
                active_lobby: Some(lobby),
                users: [Some(retained), None, None, None],
                quarantined: [None; MAX_STEAM_LOBBY_MEMBERS],
            };
            let mut primed = PrimedSignalSessions::default();

            assert_eq!(
                primed.pending_for(policy),
                [Some(retained), None, None, None]
            );
            primed.mark_sent(lobby, retained);
            assert_eq!(primed.pending_for(policy), [None; MAX_STEAM_LOBBY_MEMBERS]);

            let replaced = SignalAdmissionPolicy {
                active_lobby: Some(lobby),
                users: [Some(replacement), None, None, None],
                quarantined: [None; MAX_STEAM_LOBBY_MEMBERS],
            };
            assert_eq!(
                primed.pending_for(replaced),
                [Some(replacement), None, None, None]
            );

            primed.mark_sent(lobby, replacement);
            let next_lobby = SignalAdmissionPolicy {
                active_lobby: Some(SteamLobbyId::new(696).unwrap()),
                users: [Some(replacement), None, None, None],
                quarantined: [None; MAX_STEAM_LOBBY_MEMBERS],
            };
            assert_eq!(
                primed.pending_for(next_lobby),
                [Some(replacement), None, None, None]
            );
            primed.clear();
            assert_eq!(primed.lobby, None);
            assert_eq!(primed.users, [None; MAX_STEAM_LOBBY_MEMBERS]);
        }

        #[test]
        fn signal_quarantine_is_peer_scoped_and_clears_at_session_boundary() {
            let lobby = SteamLobbyId::new(700).unwrap();
            let rejected = SteamUserId::new(701).unwrap();
            let valid = SteamUserId::new(702).unwrap();
            let mut policy = SignalAdmissionPolicy {
                active_lobby: Some(lobby),
                users: [Some(rejected), Some(valid), None, None],
                quarantined: [None; MAX_STEAM_LOBBY_MEMBERS],
            };
            policy.quarantine(rejected);
            assert!(!policy.allows(rejected));
            assert!(policy.allows(valid));

            let mut same_lobby = SignalAdmissionPolicy {
                active_lobby: Some(lobby),
                users: [Some(rejected), Some(valid), None, None],
                quarantined: [None; MAX_STEAM_LOBBY_MEMBERS],
            };
            policy.carry_quarantine_into(&mut same_lobby);
            assert!(!same_lobby.allows(rejected));
            same_lobby.clear_quarantine();
            assert!(same_lobby.allows(rejected));
            assert!(same_lobby.allows(valid));

            let mut next_lobby = SignalAdmissionPolicy {
                active_lobby: Some(SteamLobbyId::new(703).unwrap()),
                users: [Some(rejected), None, None, None],
                quarantined: [None; MAX_STEAM_LOBBY_MEMBERS],
            };
            policy.carry_quarantine_into(&mut next_lobby);
            assert!(next_lobby.allows(rejected));
        }

        #[test]
        fn departed_signal_quarantine_does_not_exhaust_across_member_churn() {
            let lobby = SteamLobbyId::new(704).unwrap();
            let mut policy = SignalAdmissionPolicy {
                active_lobby: Some(lobby),
                users: [Some(SteamUserId::new(710).unwrap()), None, None, None],
                quarantined: [None; MAX_STEAM_LOBBY_MEMBERS],
            };

            for ordinal in 0..(MAX_STEAM_LOBBY_MEMBERS * 2 + 1) {
                let departed = policy.users[0].unwrap();
                policy.quarantine(departed);
                assert!(!policy.allows(departed));

                let replacement = SteamUserId::new(711 + ordinal as u64).unwrap();
                let mut refreshed = SignalAdmissionPolicy {
                    active_lobby: Some(lobby),
                    users: [Some(replacement), None, None, None],
                    quarantined: [None; MAX_STEAM_LOBBY_MEMBERS],
                };
                policy.carry_quarantine_into(&mut refreshed);
                assert!(!refreshed.quarantined.contains(&Some(departed)));
                assert!(refreshed.allows(replacement));
                policy = refreshed;
            }
        }

        #[test]
        fn roster_barrier_clears_all_departed_runtime_handoffs() {
            let host = SteamUserId::new(720).unwrap();
            let retained_user = SteamUserId::new(721).unwrap();
            let departed_user = SteamUserId::new(722).unwrap();
            let active_rejected_user = SteamUserId::new(723).unwrap();
            let retained_peer = PeerId::new(72).unwrap();
            let departed_peer = PeerId::new(73).unwrap();
            let live_bindings = [
                Some(OnlinePeerIdentity {
                    user: retained_user,
                    peer_id: retained_peer,
                }),
                None,
                None,
                None,
            ];
            let active_members = [Some(retained_user), Some(active_rejected_user), None, None];
            let mut authenticated = [
                Some(AuthenticatedMapping {
                    user: retained_user,
                    peer: AuthenticatedPeer {
                        peer_id: retained_peer,
                        user_id: retained_user.authenticated(),
                    },
                    connection: None,
                }),
                Some(AuthenticatedMapping {
                    user: departed_user,
                    peer: AuthenticatedPeer {
                        peer_id: departed_peer,
                        user_id: departed_user.authenticated(),
                    },
                    connection: None,
                }),
                None,
                None,
            ];
            let mut ticket_exchanges = [
                Some(TicketExchange {
                    lease: AuthTicketLease {
                        handle: AuthTicketHandle::for_test(1),
                        remote_user: retained_user,
                        remote_revision: 1,
                        sender: AuthPeerLease {
                            user: host,
                            peer_id: PeerId::new(70).unwrap(),
                            revision: 1,
                        },
                        scope: AuthSignalScope {
                            lobby: SteamLobbyId::new(705).unwrap(),
                            purpose: AdmissionPurpose::Initial,
                            owner_revision: 1,
                            match_id: None,
                        },
                    },
                    sent_sequence: None,
                    route: TicketRoute::Direct,
                }),
                Some(TicketExchange {
                    lease: AuthTicketLease {
                        handle: AuthTicketHandle::for_test(2),
                        remote_user: departed_user,
                        remote_revision: 1,
                        sender: AuthPeerLease {
                            user: host,
                            peer_id: PeerId::new(70).unwrap(),
                            revision: 1,
                        },
                        scope: AuthSignalScope {
                            lobby: SteamLobbyId::new(705).unwrap(),
                            purpose: AdmissionPurpose::Reconnect,
                            owner_revision: 1,
                            match_id: Some(
                                crate::network_protocol::MatchId::new(*b"mapping-test-001")
                                    .unwrap(),
                            ),
                        },
                    },
                    sent_sequence: Some(1),
                    route: TicketRoute::Direct,
                }),
                None,
                None,
            ];
            let mut reconnect_users = [Some(retained_user), Some(departed_user), None, None];
            let mut endpoints =
                VecDeque::from([admitted_endpoint(host, departed_user, departed_peer)]);
            let mut pending_manifest = None;
            let mut signal_rejected_users =
                [Some(active_rejected_user), Some(departed_user), None, None];

            reconcile_runtime_identity_handoffs(
                live_bindings,
                active_members,
                &mut authenticated,
                &mut ticket_exchanges,
                &mut reconnect_users,
                &mut endpoints,
                &mut pending_manifest,
                &mut signal_rejected_users,
            );

            assert_eq!(
                authenticated
                    .iter()
                    .flatten()
                    .map(|entry| entry.user)
                    .collect::<Vec<_>>(),
                vec![retained_user]
            );
            assert_eq!(
                ticket_exchanges
                    .iter()
                    .flatten()
                    .map(|entry| entry.lease.remote_user)
                    .collect::<Vec<_>>(),
                vec![retained_user]
            );
            assert_eq!(
                reconnect_users
                    .iter()
                    .flatten()
                    .copied()
                    .collect::<Vec<_>>(),
                vec![retained_user]
            );
            assert!(endpoints.is_empty());
            assert_eq!(
                signal_rejected_users
                    .iter()
                    .flatten()
                    .copied()
                    .collect::<Vec<_>>(),
                vec![active_rejected_user]
            );
        }

        #[test]
        fn disconnect_clears_active_mapping_and_endpoint_without_touching_reconnect_state() {
            let host = SteamUserId::new(730).unwrap();
            let remote = SteamUserId::new(731).unwrap();
            let remote_peer = PeerId::new(74).unwrap();
            let endpoint = admitted_endpoint(host, remote, remote_peer);
            let connection = endpoint.admitted.connection;
            let mut authenticated = [
                Some(AuthenticatedMapping {
                    user: remote,
                    peer: AuthenticatedPeer {
                        peer_id: remote_peer,
                        user_id: remote.authenticated(),
                    },
                    connection: Some(connection),
                }),
                None,
                None,
                None,
            ];
            let mut endpoints = VecDeque::from([endpoint]);
            let reconnect_users = [Some(remote), None, None, None];
            let mut committed = CommittedAuthenticatedRoster::default();
            committed
                .push(AuthenticatedPeer {
                    peer_id: remote_peer,
                    user_id: remote.authenticated(),
                })
                .unwrap();

            clear_runtime_peer_transport(&mut authenticated, &mut endpoints, remote, connection);

            assert!(authenticated.iter().all(Option::is_none));
            assert!(endpoints.is_empty());
            assert_eq!(reconnect_users[0], Some(remote));
            assert_eq!(committed.len(), 1);
        }
    }
}

#[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
use real::RealNativeOnlineRuntime;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::match_config::canonical_manifest_hash;
    use crate::network_protocol::{MatchId, TeamId};
    use crate::online_roster::{OnlineRoster, OnlineSeatSelection};
    use crate::reconnect::AuthenticatedUserId;

    fn ids() -> (SteamLobbyId, SteamUserId, SteamUserId, PeerId) {
        (
            SteamLobbyId::new(90).unwrap(),
            SteamUserId::new(10).unwrap(),
            SteamUserId::new(20).unwrap(),
            PeerId::new(3).unwrap(),
        )
    }

    #[test]
    fn development_app_id_is_required_and_spacewar_needs_exact_opt_in() {
        let missing = NativeSteamReleaseConfig::from_lookup(|_| None).unwrap_err();
        assert_eq!(missing, NativeOnlineConfigError::MissingAppId);

        let rejected = NativeSteamReleaseConfig::from_lookup(|key| match key {
            STEAM_APP_ID_ENV => Some("480".to_owned()),
            _ => None,
        })
        .unwrap_err();
        assert_eq!(
            rejected,
            NativeOnlineConfigError::SpacewarRequiresExplicitOptIn
        );

        let development = NativeSteamReleaseConfig::from_lookup(|key| match key {
            STEAM_APP_ID_ENV => Some("480".to_owned()),
            STEAM_SPACEWAR_OPT_IN_ENV => Some("1".to_owned()),
            _ => None,
        })
        .unwrap();
        assert_eq!(
            development,
            NativeSteamReleaseConfig::DevelopmentSpacewar480
        );
        assert!(development.steam_client_config().validate().is_ok());

        let compiled_development =
            NativeSteamReleaseConfig::from_sources(Some("480"), true, true, |_| None).unwrap();
        assert_eq!(compiled_development, development);

        let invalid_compiled_opt_in =
            NativeSteamReleaseConfig::from_sources(Some("123456"), true, true, |_| None)
                .unwrap_err();
        assert_eq!(
            invalid_compiled_opt_in,
            NativeOnlineConfigError::InvalidSpacewarOptIn
        );

        let production = NativeSteamReleaseConfig::from_lookup(|key| match key {
            STEAM_APP_ID_ENV => Some("123456".to_owned()),
            _ => None,
        })
        .unwrap();
        assert!(matches!(
            production,
            NativeSteamReleaseConfig::Production { .. }
        ));
    }

    #[test]
    fn release_uses_only_the_baked_app_id_and_rejects_runtime_mismatch() {
        let release =
            NativeSteamReleaseConfig::from_sources(Some("123456"), false, false, |_| None).expect(
                "a release binary uses its compile-time App ID without process configuration",
            );
        assert_eq!(release.app_id().get(), 123_456);
        assert_eq!(
            restart_app_id_for_profile(release, true),
            Some(release.app_id())
        );
        assert_eq!(restart_app_id_for_profile(release, false), None);

        let same = NativeSteamReleaseConfig::from_sources(Some("123456"), false, false, |key| {
            (key == STEAM_APP_ID_ENV).then(|| "123456".to_owned())
        })
        .unwrap();
        assert_eq!(same, release);

        let mismatch =
            NativeSteamReleaseConfig::from_sources(Some("123456"), false, false, |key| {
                (key == STEAM_APP_ID_ENV).then(|| "654321".to_owned())
            })
            .unwrap_err();
        assert_eq!(mismatch, NativeOnlineConfigError::AppIdMismatch);

        let runtime_only = NativeSteamReleaseConfig::from_sources(None, false, false, |key| {
            (key == STEAM_APP_ID_ENV).then(|| "123456".to_owned())
        })
        .unwrap_err();
        assert_eq!(runtime_only, NativeOnlineConfigError::MissingAppId);
    }

    #[test]
    fn release_never_uses_spacewar_even_with_the_development_opt_in() {
        let error = NativeSteamReleaseConfig::from_sources(Some("480"), false, true, |key| {
            (key == STEAM_SPACEWAR_OPT_IN_ENV).then(|| "1".to_owned())
        })
        .unwrap_err();
        assert_eq!(error, NativeOnlineConfigError::SpacewarForbiddenInRelease);
        assert_eq!(
            restart_app_id_for_profile(NativeSteamReleaseConfig::development_spacewar_480(), true),
            None
        );
    }

    #[test]
    fn development_runtime_override_must_match_a_baked_app_id() {
        let mismatch = NativeSteamReleaseConfig::from_sources(Some("123456"), true, false, |key| {
            (key == STEAM_APP_ID_ENV).then(|| "654321".to_owned())
        })
        .unwrap_err();
        assert_eq!(mismatch, NativeOnlineConfigError::AppIdMismatch);

        let runtime_only = NativeSteamReleaseConfig::from_sources(None, true, false, |key| {
            (key == STEAM_APP_ID_ENV).then(|| "654321".to_owned())
        })
        .unwrap();
        assert_eq!(runtime_only.app_id().get(), 654_321);
    }

    #[test]
    fn auth_signal_round_trip_is_exact_and_debug_redacts_secret() {
        let (lobby, sender, recipient, peer) = ids();
        let match_id = crate::network_protocol::MatchId::new(*b"auth-ticket-v3-1").unwrap();
        let signal = AuthTicketSignal::new(
            lobby,
            sender,
            recipient,
            peer,
            AdmissionPurpose::Reconnect,
            7,
            8,
            Some(match_id),
            &[7, 8, 9, 10],
        )
        .unwrap();
        let debug = format!("{signal:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("7, 8, 9"));
        let encoded = signal.encode();
        assert_eq!(encoded.len, AUTH_SIGNAL_HEADER_BYTES + 4);
        assert_eq!(&encoded.as_slice()[0..4], b"AFCA");
        assert_eq!(encoded.as_slice()[4], AUTH_SIGNAL_VERSION);
        assert_eq!(encoded.as_slice()[5], AUTH_SIGNAL_KIND_TICKET);
        assert_eq!(encoded.as_slice()[6], 1);
        assert_eq!(encoded.as_slice()[7], 0);
        assert_eq!(&encoded.as_slice()[8..16], &lobby.get().to_le_bytes());
        assert_eq!(&encoded.as_slice()[16..24], &sender.get().to_le_bytes());
        assert_eq!(&encoded.as_slice()[24..32], &recipient.get().to_le_bytes());
        assert_eq!(&encoded.as_slice()[32..40], &peer.get().to_le_bytes());
        assert_eq!(&encoded.as_slice()[40..42], &7_u16.to_le_bytes());
        assert_eq!(&encoded.as_slice()[42..44], &8_u16.to_le_bytes());
        assert_eq!(&encoded.as_slice()[44..60], match_id.as_bytes());
        assert_eq!(&encoded.as_slice()[60..62], &4_u16.to_le_bytes());
        assert_eq!(&encoded.as_slice()[62..], &[7, 8, 9, 10]);
        let decoded = AuthTicketSignal::decode(encoded.as_slice()).unwrap();
        assert_eq!(decoded.lobby, lobby);
        assert_eq!(decoded.sender, sender);
        assert_eq!(decoded.recipient, recipient);
        assert_eq!(decoded.sender_peer_id, peer);
        assert_eq!(decoded.purpose, AdmissionPurpose::Reconnect);
        assert_eq!(decoded.owner_revision, 7);
        assert_eq!(decoded.sender_revision, 8);
        assert_eq!(decoded.match_id, Some(match_id));
        assert_eq!(decoded.ticket(), &[7, 8, 9, 10]);
    }

    #[test]
    fn auth_session_hello_round_trip_is_lobby_and_peer_bound() {
        let (lobby, sender, recipient, _) = ids();
        let hello = AuthSessionHelloSignal::new(lobby, sender, recipient);
        let encoded = hello.encode();

        assert_eq!(encoded.len, SESSION_HELLO_SIGNAL_BYTES);
        assert_eq!(&encoded.as_slice()[0..4], b"AFCA");
        assert_eq!(encoded.as_slice()[4], AUTH_SIGNAL_VERSION);
        assert_eq!(encoded.as_slice()[5], AUTH_SIGNAL_KIND_HELLO);
        assert_eq!(&encoded.as_slice()[8..16], &lobby.get().to_le_bytes());
        assert_eq!(&encoded.as_slice()[16..24], &sender.get().to_le_bytes());
        assert_eq!(&encoded.as_slice()[24..32], &recipient.get().to_le_bytes());

        assert!(matches!(
            decode_pre_game_signal(encoded.as_slice()).unwrap(),
            PreGameSignal::Hello(decoded) if decoded == hello
        ));

        let mut malformed = encoded.as_slice().to_vec();
        malformed[6] = 1;
        assert_eq!(
            AuthSessionHelloSignal::decode(&malformed).unwrap_err(),
            AuthSignalError::InvalidEnvelope
        );
    }

    #[test]
    fn quality_rejected_ticket_retry_projects_to_peer_scoped_signal_isolation() {
        assert!(matches!(
            project_ticket_admission_result(Err(OnlineLobbyError::QualityPolicyRejected)),
            Err(NativeOnlineRuntimeError::Signal(
                AuthSignalError::UnexpectedPurpose
            ))
        ));
        assert!(matches!(
            project_ticket_admission_result(Err(OnlineLobbyError::InvalidState)),
            Err(NativeOnlineRuntimeError::Lobby(
                OnlineLobbyError::InvalidState
            ))
        ));
        assert!(matches!(
            project_ticket_admission_result(Err(OnlineLobbyError::DuplicatePeerBinding)),
            Err(NativeOnlineRuntimeError::Signal(
                AuthSignalError::InvalidIdentity
            ))
        ));
        assert!(matches!(
            project_ticket_admission_result(Err(OnlineLobbyError::Steam(
                SteamPlatformError::Backend(
                    crate::steam_platform::SteamBackendError::AuthSessionRejected(
                        crate::steam_platform::AuthSessionStartFailure::InvalidTicket,
                    ),
                ),
            ))),
            Err(NativeOnlineRuntimeError::Signal(
                AuthSignalError::InvalidEnvelope
            ))
        ));
    }

    #[test]
    fn auth_signal_rejects_truncation_extension_and_identity_corruption() {
        let (lobby, sender, recipient, peer) = ids();
        let signal = AuthTicketSignal::new(
            lobby,
            sender,
            recipient,
            peer,
            AdmissionPurpose::Initial,
            1,
            1,
            None,
            &[1, 2, 3],
        )
        .unwrap();
        let encoded = signal.encode();
        assert_eq!(
            AuthTicketSignal::decode(&encoded.as_slice()[..encoded.as_slice().len() - 1])
                .unwrap_err(),
            AuthSignalError::InvalidEnvelope
        );
        let mut extended = encoded.as_slice().to_vec();
        extended.push(0);
        assert_eq!(
            AuthTicketSignal::decode(&extended).unwrap_err(),
            AuthSignalError::InvalidEnvelope
        );
        let mut zero_sender = encoded.as_slice().to_vec();
        zero_sender[16..24].fill(0);
        assert_eq!(
            AuthTicketSignal::decode(&zero_sender).unwrap_err(),
            AuthSignalError::InvalidIdentity
        );
    }

    #[test]
    fn auth_signal_v3_rejects_v1_zero_epochs_and_purpose_match_mismatch() {
        let (lobby, sender, recipient, peer) = ids();
        assert_eq!(
            AuthTicketSignal::new(
                lobby,
                sender,
                recipient,
                peer,
                AdmissionPurpose::Initial,
                0,
                1,
                None,
                &[1],
            )
            .unwrap_err(),
            AuthSignalError::InvalidEnvelope
        );
        assert_eq!(
            AuthTicketSignal::new(
                lobby,
                sender,
                recipient,
                peer,
                AdmissionPurpose::Initial,
                1,
                0,
                None,
                &[1],
            )
            .unwrap_err(),
            AuthSignalError::InvalidEnvelope
        );
        assert_eq!(
            AuthTicketSignal::new(
                lobby,
                sender,
                recipient,
                peer,
                AdmissionPurpose::Reconnect,
                1,
                1,
                None,
                &[1],
            )
            .unwrap_err(),
            AuthSignalError::InvalidEnvelope
        );

        let initial = AuthTicketSignal::new(
            lobby,
            sender,
            recipient,
            peer,
            AdmissionPurpose::Initial,
            1,
            1,
            None,
            &[1],
        )
        .unwrap()
        .encode();
        assert_eq!(&initial.as_slice()[44..60], &[0; 16]);

        let mut v1 = initial.as_slice().to_vec();
        v1[4] = 1;
        assert_eq!(
            AuthTicketSignal::decode(&v1).unwrap_err(),
            AuthSignalError::InvalidEnvelope
        );
        let mut zero_owner_revision = initial.as_slice().to_vec();
        zero_owner_revision[40..42].fill(0);
        assert_eq!(
            AuthTicketSignal::decode(&zero_owner_revision).unwrap_err(),
            AuthSignalError::InvalidEnvelope
        );
        let mut zero_sender_revision = initial.as_slice().to_vec();
        zero_sender_revision[42..44].fill(0);
        assert_eq!(
            AuthTicketSignal::decode(&zero_sender_revision).unwrap_err(),
            AuthSignalError::InvalidEnvelope
        );
        let mut initial_with_match = initial.as_slice().to_vec();
        initial_with_match[44..60].copy_from_slice(b"auth-ticket-v3-2");
        assert_eq!(
            AuthTicketSignal::decode(&initial_with_match).unwrap_err(),
            AuthSignalError::InvalidEnvelope
        );
        let mut reconnect_without_match = initial.as_slice().to_vec();
        reconnect_without_match[6] = 1;
        assert_eq!(
            AuthTicketSignal::decode(&reconnect_without_match).unwrap_err(),
            AuthSignalError::InvalidEnvelope
        );
    }

    #[test]
    fn auth_signal_zeroization_overwrites_every_secret_byte() {
        let mut bytes = [0xA5; MAX_STEAM_AUTH_TICKET_BYTES];
        zeroize_auth_signal_bytes(&mut bytes);
        assert!(bytes.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn auth_signal_batch_quarantines_over_limit_user_without_dropping_valid_peer() {
        let lobby = SteamLobbyId::new(90).unwrap();
        let attacker = SteamUserId::new(10).unwrap();
        let valid_user = SteamUserId::new(20).unwrap();
        let recipient = SteamUserId::new(30).unwrap();
        let attacker_signal = AuthTicketSignal::new(
            lobby,
            attacker,
            recipient,
            PeerId::new(3).unwrap(),
            AdmissionPurpose::Initial,
            1,
            1,
            None,
            &[1],
        )
        .unwrap()
        .encode();
        let valid_signal = AuthTicketSignal::new(
            lobby,
            valid_user,
            recipient,
            PeerId::new(4).unwrap(),
            AdmissionPurpose::Initial,
            1,
            1,
            None,
            &[2],
        )
        .unwrap()
        .encode();

        let batch = std::iter::repeat((attacker, attacker_signal.as_slice()))
            .take(MAX_AUTH_SIGNALS_PER_USER_PER_PUMP + 1)
            .chain(std::iter::once((valid_user, valid_signal.as_slice())));
        let outcomes = decode_bounded_auth_signal_batch(batch);

        assert_eq!(outcomes.len(), 2);
        assert!(matches!(
            outcomes.first(),
            Some(AuthSignalIngress::Rejected {
                source,
                error: AuthSignalError::ReceiveBudgetExceeded,
            }) if *source == attacker
        ));
        assert!(matches!(
            outcomes.get(1),
            Some(AuthSignalIngress::Accepted { source, signal })
                if *source == valid_user && signal.sender() == valid_user
        ));
    }

    #[test]
    fn malformed_attributed_auth_signal_does_not_poison_other_user_in_batch() {
        let (lobby, valid_user, recipient, peer) = ids();
        let attacker = SteamUserId::new(11).unwrap();
        let valid_signal = AuthTicketSignal::new(
            lobby,
            valid_user,
            recipient,
            peer,
            AdmissionPurpose::Initial,
            1,
            1,
            None,
            &[7],
        )
        .unwrap()
        .encode();
        let outcomes = decode_bounded_auth_signal_batch([
            (attacker, &[0_u8, 1, 2][..]),
            (valid_user, valid_signal.as_slice()),
        ]);

        assert_eq!(outcomes.len(), 2);
        assert!(matches!(
            outcomes.first(),
            Some(AuthSignalIngress::Rejected {
                source,
                error: AuthSignalError::InvalidEnvelope,
            }) if *source == attacker
        ));
        assert!(matches!(
            outcomes.get(1),
            Some(AuthSignalIngress::Accepted { source, signal })
                if *source == valid_user && signal.sender() == valid_user
        ));
    }

    #[test]
    fn manifest_bootstrap_uses_canonical_wire_codec_and_rejects_trailing_data() {
        let (lobby, sender, recipient, peer) = ids();
        let mut roster = OnlineRoster::default();
        roster
            .upsert(
                OnlineRosterMember::new(
                    peer,
                    sender.authenticated(),
                    1,
                    true,
                    &[OnlineSeatSelection {
                        team: TeamId::new(1).unwrap(),
                        character: DefinitionId::new(1).unwrap(),
                        style: DefinitionId::new(1).unwrap(),
                        equipment: DefinitionId::new(1).unwrap(),
                    }],
                )
                .unwrap(),
            )
            .unwrap();
        let config = roster
            .build_headless_config(
                OnlineManifestOptions::casual_listen(
                    MatchId::new([9; 16]).unwrap(),
                    peer,
                    DefinitionId::new(0).unwrap(),
                    DefinitionId::new(0).unwrap(),
                    44,
                    SimTick(120),
                ),
                SimTick::ZERO,
            )
            .unwrap();
        let signal =
            BootstrapManifestSignal::new(lobby, sender, recipient, config.manifest).unwrap();
        let encoded = signal.encode().unwrap();
        let decoded = BootstrapManifestSignal::decode(encoded.as_slice()).unwrap();
        assert_eq!(decoded, signal);
        let reconstructed = headless_config_from_manifest(decoded.manifest).unwrap();
        assert_eq!(reconstructed.manifest, config.manifest);

        assert_eq!(
            classify_manifest_ingress(None, None, OnlineLobbyPhase::Connecting, signal,).unwrap(),
            ManifestIngress::Stage
        );
        assert_eq!(
            classify_manifest_ingress(
                None,
                Some(signal),
                OnlineLobbyPhase::ManifestAgreement,
                signal,
            )
            .unwrap(),
            ManifestIngress::ExactDuplicate
        );
        assert_eq!(
            classify_manifest_ingress(
                Some(config.manifest),
                None,
                OnlineLobbyPhase::Loading,
                signal,
            )
            .unwrap(),
            ManifestIngress::ExactDuplicate
        );
        let mut conflicting_manifest = config.manifest;
        conflicting_manifest.master_gameplay_seed += 1;
        conflicting_manifest.manifest_hash = canonical_manifest_hash(&conflicting_manifest);
        let conflicting =
            BootstrapManifestSignal::new(lobby, sender, recipient, conflicting_manifest).unwrap();
        assert_eq!(
            classify_manifest_ingress(
                None,
                Some(signal),
                OnlineLobbyPhase::ManifestAgreement,
                conflicting,
            )
            .unwrap_err(),
            AuthSignalError::ConflictingManifest
        );

        let mut extended = encoded.as_slice().to_vec();
        extended.push(0);
        assert_eq!(
            BootstrapManifestSignal::decode(&extended).unwrap_err(),
            AuthSignalError::InvalidEnvelope
        );

        let mut dedicated = config.manifest;
        dedicated.authority = crate::network_protocol::AuthorityKind::Dedicated;
        dedicated.trusted_results = true;
        dedicated.manifest_hash = canonical_manifest_hash(&dedicated);
        assert_eq!(
            BootstrapManifestSignal::new(lobby, sender, recipient, dedicated).unwrap_err(),
            AuthSignalError::InvalidEnvelope
        );
    }

    #[test]
    fn fixed_committed_roster_rejects_duplicate_identity_and_peer() {
        let mut roster = CommittedAuthenticatedRoster::default();
        let first = AuthenticatedPeer {
            peer_id: PeerId::new(1).unwrap(),
            user_id: AuthenticatedUserId::new(10).unwrap(),
        };
        roster.push(first).unwrap();
        assert_eq!(roster.len(), 1);
        assert!(roster.push(first).is_err());
        assert!(
            roster
                .push(AuthenticatedPeer {
                    peer_id: PeerId::new(1).unwrap(),
                    user_id: AuthenticatedUserId::new(11).unwrap(),
                })
                .is_err()
        );
    }

    #[test]
    fn auth_signal_diagnostic_codes_are_stable_and_distinct() {
        let errors = [
            AuthSignalError::EmptyTicket,
            AuthSignalError::TicketTooLarge,
            AuthSignalError::InvalidEnvelope,
            AuthSignalError::InvalidIdentity,
            AuthSignalError::WrongLobby,
            AuthSignalError::WrongRecipient,
            AuthSignalError::SenderMismatch,
            AuthSignalError::UnexpectedPurpose,
            AuthSignalError::PeerNotInLobby,
            AuthSignalError::TransportFailed,
            AuthSignalError::ReceiveBudgetExceeded,
            AuthSignalError::UnexpectedManifestSender,
            AuthSignalError::ConflictingManifest,
            AuthSignalError::SessionAcceptanceFailed,
            AuthSignalError::SessionLocalOffline,
            AuthSignalError::SessionRelayUnavailable,
            AuthSignalError::SessionNetworkConfigUnavailable,
            AuthSignalError::SessionRightsDenied,
            AuthSignalError::SessionRemoteTimeout,
            AuthSignalError::SessionCryptFailure,
            AuthSignalError::SessionProtocolMismatch,
            AuthSignalError::SessionInternalFailure,
            AuthSignalError::SessionSteamConnectivity,
            AuthSignalError::SessionRendezvousFailed,
            AuthSignalError::SessionNatFirewall,
            AuthSignalError::SessionPeerRejected,
            AuthSignalError::SessionUnknownFailure,
        ];
        let mut codes = [0_u16; 27];
        for (index, error) in errors.into_iter().enumerate() {
            codes[index] = auth_signal_detail_code(error);
        }
        assert_eq!(
            codes,
            [
                201, 202, 203, 204, 205, 206, 207, 208, 209, 210, 211, 212, 213, 214, 215, 216,
                217, 218, 219, 229, 230, 231, 232, 233, 234, 235, 236,
            ]
        );
        for (index, code) in codes.into_iter().enumerate() {
            assert!(!codes[..index].contains(&code));
        }
    }

    #[test]
    fn delayed_auth_rejection_targets_only_its_exact_native_generation() {
        let old = SteamConnectionId::new(401).unwrap();
        let replacement = SteamConnectionId::new(402).unwrap();

        assert!(authentication_rejection_targets_mapping(
            Some(Some(old)),
            Some(old)
        ));
        assert!(!authentication_rejection_targets_mapping(
            Some(Some(replacement)),
            Some(old)
        ));
        assert!(!authentication_rejection_targets_mapping(
            Some(Some(replacement)),
            None
        ));
        assert!(authentication_rejection_targets_mapping(Some(None), None));
        assert!(authentication_rejection_targets_mapping(None, None));
        assert!(!authentication_rejection_targets_mapping(None, Some(old)));
    }

    #[test]
    fn non_steam_runtime_exposes_localized_unavailable_screen() {
        #[cfg(not(feature = "steam-net"))]
        {
            let runtime = NativeOnlineRuntime::from_process_environment(0);
            let view = runtime.view_model();
            assert_eq!(
                view.availability,
                NativeOnlineAvailability::Unavailable(
                    NativeOnlineUnavailableReason::SteamFeatureDisabled
                )
            );
            assert_eq!(view.screen, NativeOnlineScreen::Unavailable);
            assert_eq!(
                view.availability_message_key(),
                "online.unavailable.steam_feature_disabled"
            );
            assert!(view.actions.return_to_menu);
            assert!(!view.actions.create_private);
        }
    }

    #[test]
    fn screen_projection_covers_lobby_countdown_reconnect_results_and_errors() {
        let local_user = SteamUserId::new(10).unwrap();
        let declaration = OnlineRosterMember::new(
            PeerId::new(1).unwrap(),
            local_user.authenticated(),
            1,
            true,
            &[OnlineSeatSelection {
                team: TeamId::new(1).unwrap(),
                character: DefinitionId::new(1).unwrap(),
                style: DefinitionId::new(1).unwrap(),
                equipment: DefinitionId::new(1).unwrap(),
            }],
        )
        .unwrap();
        for (phase, screen) in [
            (OnlineLobbyPhase::Lobby, NativeOnlineScreen::Lobby),
            (OnlineLobbyPhase::Countdown, NativeOnlineScreen::Countdown),
            (OnlineLobbyPhase::Fighting, NativeOnlineScreen::Fighting),
            (
                OnlineLobbyPhase::Reconnecting,
                NativeOnlineScreen::Reconnecting,
            ),
            (OnlineLobbyPhase::Results, NativeOnlineScreen::Results),
            (OnlineLobbyPhase::Failed, NativeOnlineScreen::Error),
        ] {
            let status = OnlineLobbyStatus {
                phase,
                deadline_at_ms: None,
                lobby: None,
                owner: None,
                role: None,
                pending_join: None,
                lobby_members: 1,
                roster_members: 1,
                total_seats: 1,
                seat_capacity: 4,
                effective_joinable: true,
                all_members_ready: true,
                connected_remote_peers: 0,
                secure_remote_peers: 0,
                required_remote_peers: 0,
                verified_remote_accounts: 0,
                required_remote_accounts: 0,
                transport_installed: false,
                relay_status: SteamRelayStatus::default(),
                steam_network_readiness: crate::steam_transport::SteamNetworkReadiness::default(),
                setup_stage: crate::online_lobby::OnlineSetupStage::PreparingSteamNetwork,
                start_blocker: None,
                manifest_hash: None,
                countdown_start_tick: None,
                network_quality: NetworkQualitySnapshot::default(),
                input_delay_calibration: InputDelayCalibrationSnapshot::default(),
                outcome: if phase == OnlineLobbyPhase::Results {
                    Some(OnlineMatchOutcome::Confirmed)
                } else {
                    None
                },
                failure: None,
            };
            let view = project_view(
                NativeOnlineAvailability::Available,
                status,
                Some(declaration),
                None,
            );
            assert_eq!(view.screen, screen);
            assert_eq!(view.local_seats, 1);
            assert!(view.local_ready);
        }

        let fatal = OnlineFailure {
            code: OnlineFailureCode::ConnectionTimedOut,
            severity: OnlineFailureSeverity::Fatal,
            recovery: OnlineRecoveryAction::ReturnToMenu,
            detail_code: 19,
        };
        let error = project_view(
            NativeOnlineAvailability::Available,
            OnlineLobbyStatus {
                phase: OnlineLobbyPhase::Fighting,
                deadline_at_ms: None,
                lobby: None,
                owner: None,
                role: Some(OnlineLobbyRole::Client),
                pending_join: None,
                lobby_members: 2,
                roster_members: 2,
                total_seats: 2,
                seat_capacity: 4,
                effective_joinable: false,
                all_members_ready: true,
                connected_remote_peers: 1,
                secure_remote_peers: 1,
                required_remote_peers: 1,
                verified_remote_accounts: 1,
                required_remote_accounts: 1,
                transport_installed: false,
                relay_status: SteamRelayStatus::default(),
                steam_network_readiness: crate::steam_transport::SteamNetworkReadiness::default(),
                setup_stage: crate::online_lobby::OnlineSetupStage::GameplayReady,
                start_blocker: None,
                manifest_hash: None,
                countdown_start_tick: Some(SimTick(120)),
                network_quality: NetworkQualitySnapshot::default(),
                input_delay_calibration: InputDelayCalibrationSnapshot::default(),
                outcome: None,
                failure: None,
            },
            Some(declaration),
            Some(fatal),
        );
        assert_eq!(error.screen, NativeOnlineScreen::Error);
        assert_eq!(error.failure, Some(fatal));
        assert!(error.actions.return_to_menu);
        assert!(!error.actions.leave);
        assert!(!error.actions.create_private);
        assert!(!error.actions.toggle_ready);
        assert!(!error.actions.rematch);
    }

    #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
    #[test]
    fn fatal_transport_pump_failure_returns_to_menu_instead_of_reconnect() {
        let failure = runtime_failure(&NativeOnlineRuntimeError::Transport(
            SteamTransportError::BackendUnavailable,
        ));
        assert_eq!(failure.severity, OnlineFailureSeverity::Fatal);
        assert_eq!(failure.recovery, OnlineRecoveryAction::ReturnToMenu);
    }
}
