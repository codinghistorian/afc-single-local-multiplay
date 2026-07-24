//! Native online application runtime and UI-independent screen model.
//!
//! The deterministic simulation never owns this service. On a native Steam
//! build, one [`NativeOnlineRuntime`] owns the sole real Steam platform,
//! [`OnlineLobbyCoordinator`], the Steam gameplay transport factory, and a
//! bounded pre-game authentication signaling channel. Builds without
//! `steam-net` retain the same screen model and fail closed with a localizable
//! unavailable reason.

use core::fmt;
#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
use std::collections::VecDeque;

use crate::headless::HeadlessMatchConfig;
use crate::match_config::current_compatibility;
#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
use crate::match_config::headless_config_from_manifest;
#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
use crate::network_codec::encode_packet;
use crate::network_codec::{WireMessage, decode_packet};
use crate::network_protocol::{
    DefinitionId, MatchManifest, PeerId, RetryDisposition, StartMessage,
};
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
    OnlineLobbyError, OnlineLobbyEvent, OnlineLobbyRole, OnlineMatchOutcome,
};
#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
use crate::online_lobby::{OnlineLobbyPhase, OnlineLobbyStatus};
use crate::online_roster::{
    FirstReleaseOnlinePolicy, OnlineManifestOptions, OnlineRosterMember, OnlineSeatSelection,
};
use crate::reconnect::{AuthenticatedPeer, AuthenticatedUserId};
use crate::remote_online_client::RemoteAuthorityDisconnect;
use crate::simulation::SimTick;
use crate::steam_platform::{
    AdmissionPurpose, LobbyJoinIntent, MAX_STEAM_AUTH_TICKET_BYTES, MAX_STEAM_LOBBY_MEMBERS,
    RegionCode, SPACEWAR_APP_ID, SteamAppId, SteamClientConfig, SteamInputActionSet,
    SteamInputSnapshot, SteamLobbyId, SteamOverlayRequestStatus, SteamPlatformError, SteamUserId,
};
#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
use crate::steam_platform::{
    AuthTicketHandle, LobbyCreateRequest, LobbyMetadata, LobbyVisibility, SteamBackend,
    SteamPlatform, SteamPlatformState,
};
use crate::steam_transport::{
    AdmittedSteamEndpoint, SteamConnectionId, SteamRelayStatus, SteamTransportError,
};
#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
use crate::steam_transport::{SteamP2pSession, SteamTransport, SteamTransportConfig};

pub const STEAM_APP_ID_ENV: &str = "AFC_STEAM_APP_ID";
pub const STEAM_SPACEWAR_OPT_IN_ENV: &str = "AFC_STEAM_DEV_SPACEWAR_480";
pub const COMPILED_STEAM_APP_ID: Option<&str> = option_env!("AFC_COMPILED_STEAM_APP_ID");
pub const AUTH_SIGNAL_CHANNEL: u32 = 0x41_46_43;
pub const MAX_NATIVE_ONLINE_EVENTS: usize = 128;
pub const MAX_AUTH_SIGNALS_PER_PUMP: usize = 16;
/// A single Steam user may consume at most one quarter of the bounded
/// pre-game receive budget. Exceeding this quota invalidates only that user's
/// outcomes; it never invalidates another user's signal. Steam's shared
/// ordered queue may still defer a later valid signal until the next pump.
pub const MAX_AUTH_SIGNALS_PER_USER_PER_PUMP: usize =
    MAX_AUTH_SIGNALS_PER_PUMP / MAX_STEAM_LOBBY_MEMBERS;

const AUTH_SIGNAL_MAGIC: [u8; 4] = *b"AFCA";
const AUTH_SIGNAL_VERSION: u8 = 2;
const AUTH_SIGNAL_KIND_TICKET: u8 = 1;
const AUTH_SIGNAL_KIND_MANIFEST: u8 = 2;
const AUTH_SIGNAL_HEADER_BYTES: usize = 62;
const MANIFEST_SIGNAL_HEADER_BYTES: usize = 32;
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
        Self::from_sources(COMPILED_STEAM_APP_ID, cfg!(debug_assertions), |key| {
            std::env::var(key).ok()
        })
    }

    #[cfg(test)]
    fn from_lookup(
        lookup: impl FnMut(&str) -> Option<String>,
    ) -> Result<Self, NativeOnlineConfigError> {
        Self::from_sources(None, true, lookup)
    }

    fn from_sources(
        compiled_raw: Option<&str>,
        development_build: bool,
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
        let spacewar_opt_in = match lookup(STEAM_SPACEWAR_OPT_IN_ENV).as_deref() {
            None | Some("0") => false,
            Some("1") => true,
            Some(_) => return Err(NativeOnlineConfigError::InvalidSpacewarOptIn),
        };
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
    /// Irreversibly fences new transport, ticket, authentication-signal, and
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

    #[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
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

    #[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
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

#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
enum PreGameSignal {
    Ticket(AuthTicketSignal),
    Manifest(BootstrapManifestSignal),
}

#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
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
#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
enum ManifestIngress {
    Apply,
    Stage,
    ExactDuplicate,
}

#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
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

#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
impl PreGameSignal {
    fn sender(&self) -> SteamUserId {
        match self {
            Self::Ticket(signal) => signal.sender,
            Self::Manifest(signal) => signal.sender,
        }
    }
}

#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
fn decode_pre_game_signal(bytes: &[u8]) -> Result<PreGameSignal, AuthSignalError> {
    match bytes.get(5).copied() {
        Some(AUTH_SIGNAL_KIND_TICKET) => {
            Ok(PreGameSignal::Ticket(AuthTicketSignal::decode(bytes)?))
        }
        Some(AUTH_SIGNAL_KIND_MANIFEST) => Ok(PreGameSignal::Manifest(
            BootstrapManifestSignal::decode(bytes)?,
        )),
        _ => Err(AuthSignalError::InvalidEnvelope),
    }
}

#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
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

#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
struct EncodedPreGameSignal {
    bytes: [u8; MAX_AUTH_SIGNAL_BYTES],
    len: usize,
}

#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
impl EncodedPreGameSignal {
    fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
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
                detail_code: reason as u16,
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
        detail_code: 0,
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
        Err(error) => Err(error.into()),
    }
}

#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
mod real {
    #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
    use std::sync::atomic::{AtomicBool, Ordering};
    #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
    use std::sync::{Arc, Mutex};

    #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
    use arrayvec::ArrayVec;
    #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
    use steamworks::networking_types::{NetworkingIdentity, SendFlags};

    use super::*;
    use crate::steam_platform::MemberReadiness;
    #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
    use crate::steam_platform::{RealClientOwnershipGuard, RealSteamBackend};

    #[derive(Clone, Copy)]
    struct TicketExchange {
        lease: AuthTicketLease,
        sent: bool,
    }

    #[derive(Clone, Copy)]
    struct AuthenticatedMapping {
        user: SteamUserId,
        peer: AuthenticatedPeer,
        connection: Option<SteamConnectionId>,
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

    #[derive(Clone, Copy)]
    struct SignalAdmissionPolicy {
        active_lobby: Option<SteamLobbyId>,
        users: [Option<SteamUserId>; MAX_STEAM_LOBBY_MEMBERS],
        quarantined: [Option<SteamUserId>; MAX_STEAM_LOBBY_MEMBERS],
    }

    impl Default for SignalAdmissionPolicy {
        fn default() -> Self {
            Self {
                active_lobby: None,
                users: [None; MAX_STEAM_LOBBY_MEMBERS],
                quarantined: [None; MAX_STEAM_LOBBY_MEMBERS],
            }
        }
    }

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

    #[derive(Clone, Copy)]
    pub(super) struct AuthSignalAdmission {
        active_lobby: Option<SteamLobbyId>,
        users: [Option<SteamUserId>; MAX_STEAM_LOBBY_MEMBERS],
    }

    pub(super) trait NativeAuthSignalPort {
        fn refresh_policy(&self, admission: AuthSignalAdmission);
        fn peer_is_quarantined(&self, user: SteamUserId) -> Result<bool, AuthSignalError>;
        fn quarantine_peer(&self, user: SteamUserId) -> Result<(), AuthSignalError>;
        fn reset_session_isolation(&self) -> Result<(), AuthSignalError>;
        fn quiesce_admission(&self) -> Result<(), AuthSignalError>;
        fn send_ticket(&self, signal: AuthTicketSignal) -> Result<(), AuthSignalError>;
        fn send_manifest(&self, signal: BootstrapManifestSignal) -> Result<(), AuthSignalError>;
        fn receive(&self) -> Result<Vec<AuthSignalIngress>, AuthSignalError>;
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

    /// `steamworks` 0.12.2 does not expose CloseSessionWithUser on its safe
    /// `NetworkingMessages` wrapper. Keep this exact-pin compatibility shim
    /// private and pass only a previously validated numeric Steam identity.
    #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
    fn close_attributed_auth_signal_session(user: SteamUserId) {
        let mut identity = steamworks::sys::SteamNetworkingIdentity {
            m_eType: steamworks::sys::ESteamNetworkingIdentityType::
                k_ESteamNetworkingIdentityType_Invalid,
            m_cbSize: 0,
            __bindgen_anon_1: steamworks::sys::SteamNetworkingIdentity__bindgen_ty_2 {
                m_steamID64: 0,
            },
        };
        // SAFETY: all functions come from the exact `steamworks`/Steamworks
        // SDK pin used by the safe wrapper. `identity` is initialized with
        // the SDK's invalid discriminant, then populated with a non-zero,
        // validated Steam user before its pointer is passed to the client
        // messages interface. A null interface is checked and never called.
        unsafe {
            steamworks::sys::SteamAPI_SteamNetworkingIdentity_Clear(&mut identity);
            steamworks::sys::SteamAPI_SteamNetworkingIdentity_SetSteamID64(
                &mut identity,
                user.get(),
            );
            let messages = steamworks::sys::SteamAPI_SteamNetworkingMessages_SteamAPI_v002();
            if !messages.is_null() {
                let _ = steamworks::sys::SteamAPI_ISteamNetworkingMessages_CloseSessionWithUser(
                    messages, &identity,
                );
            }
        }
    }

    fn auth_signal_peer_failure(error: AuthSignalError) -> OnlineFailure {
        let (code, severity, recovery) = match error {
            AuthSignalError::ReceiveBudgetExceeded => (
                OnlineFailureCode::RateLimited,
                OnlineFailureSeverity::Fatal,
                OnlineRecoveryAction::ReturnToLobby,
            ),
            AuthSignalError::TransportFailed => (
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
            detail_code: 0,
        }
    }

    #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
    pub(super) struct SteamAuthSignalChannel {
        messages: steamworks::networking_messages::NetworkingMessages,
        policy: Arc<Mutex<SignalAdmissionPolicy>>,
        failed: Arc<AtomicBool>,
        failed_users: Arc<Mutex<ArrayVec<SteamUserId, MAX_STEAM_LOBBY_MEMBERS>>>,
        callback_owner_alive: Arc<AtomicBool>,
        _ownership: Arc<RealClientOwnershipGuard>,
    }

    #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
    impl SteamAuthSignalChannel {
        fn new(platform: &SteamPlatform<RealSteamBackend>) -> Self {
            let (client, callback_owner_alive, ownership) =
                platform.steam_transport_client_access();
            let messages = client.networking_messages();
            let policy = Arc::new(Mutex::new(SignalAdmissionPolicy::default()));
            let failed = Arc::new(AtomicBool::new(false));
            let failed_users = Arc::new(Mutex::new(ArrayVec::new()));

            messages.session_request_callback({
                let policy = policy.clone();
                move |request| {
                    let Some(raw_user) = request.remote().steam_id().map(|id| id.raw()) else {
                        request.reject();
                        return;
                    };
                    let user = SteamUserId::new(raw_user).ok();
                    let allowed = user.is_some_and(|user| {
                        policy.lock().ok().is_some_and(|policy| policy.allows(user))
                    });
                    if allowed {
                        let _ = request.accept();
                    } else {
                        request.reject();
                    }
                }
            });
            messages.session_failed_callback({
                let failed = failed.clone();
                let failed_users = failed_users.clone();
                let policy = policy.clone();
                move |info| {
                    let user = info
                        .identity_remote()
                        .and_then(|identity| identity.steam_id())
                        .and_then(|id| SteamUserId::new(id.raw()).ok());
                    let Some(user) = user else {
                        failed.store(true, Ordering::Release);
                        return;
                    };
                    let allowed = match policy.lock() {
                        Ok(policy) => policy.allows(user),
                        Err(_) => {
                            failed.store(true, Ordering::Release);
                            return;
                        }
                    };
                    if !allowed {
                        close_attributed_auth_signal_session(user);
                        return;
                    }
                    let Ok(mut failed_users) = failed_users.lock() else {
                        failed.store(true, Ordering::Release);
                        return;
                    };
                    if failed_users.contains(&user) {
                        return;
                    }
                    if failed_users.try_push(user).is_err() {
                        // The admission policy permits at most the fixed lobby
                        // capacity, so this can only be a redundant or stale
                        // callback race. Close the attributable session without
                        // converting it into host-global failure.
                        close_attributed_auth_signal_session(user);
                    }
                }
            });

            Self {
                messages,
                policy,
                failed,
                failed_users,
                callback_owner_alive,
                _ownership: ownership,
            }
        }

        fn apply_policy(&self, admission: AuthSignalAdmission) {
            let mut next = SignalAdmissionPolicy {
                active_lobby: admission.active_lobby,
                users: admission.users,
                quarantined: [None; MAX_STEAM_LOBBY_MEMBERS],
            };
            if let Ok(mut policy) = self.policy.lock() {
                policy.carry_quarantine_into(&mut next);
                *policy = next;
            } else {
                self.failed.store(true, Ordering::Release);
            }
        }

        fn peer_is_quarantined(&self, user: SteamUserId) -> Result<bool, AuthSignalError> {
            self.policy
                .lock()
                .map(|policy| policy.quarantined.contains(&Some(user)))
                .map_err(|_| AuthSignalError::TransportFailed)
        }

        fn peer_is_member(&self, user: SteamUserId) -> Result<bool, AuthSignalError> {
            self.policy
                .lock()
                .map(|policy| policy.contains_member(user))
                .map_err(|_| AuthSignalError::TransportFailed)
        }

        fn peer_is_allowed(&self, user: SteamUserId) -> Result<bool, AuthSignalError> {
            self.policy
                .lock()
                .map(|policy| policy.allows(user))
                .map_err(|_| AuthSignalError::TransportFailed)
        }

        fn quarantine_peer(&self, user: SteamUserId) -> Result<(), AuthSignalError> {
            self.policy
                .lock()
                .map_err(|_| AuthSignalError::TransportFailed)?
                .quarantine(user);
            close_attributed_auth_signal_session(user);
            Ok(())
        }

        fn reset_session_isolation(&self) -> Result<(), AuthSignalError> {
            self.policy
                .lock()
                .map_err(|_| AuthSignalError::TransportFailed)?
                .clear_quarantine();
            self.failed_users
                .lock()
                .map_err(|_| AuthSignalError::TransportFailed)?
                .clear();
            Ok(())
        }

        fn quiesce_admission(&self) -> Result<(), AuthSignalError> {
            let mut policy = self
                .policy
                .lock()
                .map_err(|_| AuthSignalError::TransportFailed)?;
            policy.active_lobby = None;
            policy.users = [None; MAX_STEAM_LOBBY_MEMBERS];
            Ok(())
        }

        fn send_ticket(&self, signal: AuthTicketSignal) -> Result<(), AuthSignalError> {
            self.send_encoded(signal.recipient, signal.encode())
        }

        fn send_manifest(&self, signal: BootstrapManifestSignal) -> Result<(), AuthSignalError> {
            self.send_encoded(signal.recipient, signal.encode()?)
        }

        fn send_encoded(
            &self,
            recipient: SteamUserId,
            encoded: EncodedPreGameSignal,
        ) -> Result<(), AuthSignalError> {
            if !self.callback_owner_alive.load(Ordering::Acquire)
                || self.failed.load(Ordering::Acquire)
            {
                return Err(AuthSignalError::TransportFailed);
            }
            if self.peer_is_quarantined(recipient)? {
                return Ok(());
            }
            let identity =
                NetworkingIdentity::new_steam_id(steamworks::SteamId::from_raw(recipient.get()));
            self.messages
                .send_message_to_user(
                    identity,
                    SendFlags::RELIABLE_NO_NAGLE | SendFlags::AUTO_RESTART_BROKEN_SESSION,
                    encoded.as_slice(),
                    AUTH_SIGNAL_CHANNEL,
                )
                .map_err(|_| AuthSignalError::TransportFailed)
        }

        fn receive(&self) -> Result<Vec<AuthSignalIngress>, AuthSignalError> {
            if !self.callback_owner_alive.load(Ordering::Acquire)
                || self.failed.swap(false, Ordering::AcqRel)
            {
                return Err(AuthSignalError::TransportFailed);
            }
            let failed_users: ArrayVec<SteamUserId, MAX_STEAM_LOBBY_MEMBERS> = {
                let mut failed_users = self
                    .failed_users
                    .lock()
                    .map_err(|_| AuthSignalError::TransportFailed)?;
                failed_users.drain(..).collect()
            };
            let mut outcomes =
                Vec::with_capacity(MAX_AUTH_SIGNALS_PER_PUMP + MAX_STEAM_LOBBY_MEMBERS);
            for source in failed_users {
                if !self.peer_is_member(source)? {
                    // Membership is the first admission gate. A callback that
                    // raced a terminal departure must not consume the bounded
                    // quarantine for a user the refreshed policy already
                    // rejects and whose attributable session can be closed.
                    close_attributed_auth_signal_session(source);
                    continue;
                }
                self.quarantine_peer(source)?;
                outcomes.push(AuthSignalIngress::Rejected {
                    source,
                    error: AuthSignalError::TransportFailed,
                });
            }
            let messages = self
                .messages
                .receive_messages_on_channel(AUTH_SIGNAL_CHANNEL, MAX_AUTH_SIGNALS_PER_PUMP + 1);
            let mut attributed = Vec::with_capacity(messages.len());
            for message in &messages {
                let source = message
                    .identity_peer()
                    .steam_id()
                    .and_then(|id| SteamUserId::new(id.raw()).ok())
                    .ok_or(AuthSignalError::InvalidIdentity)?;
                if self.peer_is_allowed(source)? {
                    attributed.push((source, message.data()));
                } else if !self.peer_is_quarantined(source)? {
                    // A stale session from outside the current roster is
                    // attributable and closed, but it consumes no rejection
                    // slot and cannot fault the active host.
                    close_attributed_auth_signal_session(source);
                }
            }
            let decoded = decode_bounded_auth_signal_batch(attributed);
            for outcome in &decoded {
                if let AuthSignalIngress::Rejected { source, .. } = outcome {
                    self.quarantine_peer(*source)?;
                }
            }
            outcomes.extend(decoded);
            Ok(outcomes)
        }
    }

    #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
    impl NativeAuthSignalPort for SteamAuthSignalChannel {
        fn refresh_policy(&self, admission: AuthSignalAdmission) {
            self.apply_policy(admission);
        }

        fn peer_is_quarantined(&self, user: SteamUserId) -> Result<bool, AuthSignalError> {
            SteamAuthSignalChannel::peer_is_quarantined(self, user)
        }

        fn quarantine_peer(&self, user: SteamUserId) -> Result<(), AuthSignalError> {
            SteamAuthSignalChannel::quarantine_peer(self, user)
        }

        fn reset_session_isolation(&self) -> Result<(), AuthSignalError> {
            SteamAuthSignalChannel::reset_session_isolation(self)
        }

        fn quiesce_admission(&self) -> Result<(), AuthSignalError> {
            SteamAuthSignalChannel::quiesce_admission(self)
        }

        fn send_ticket(&self, signal: AuthTicketSignal) -> Result<(), AuthSignalError> {
            SteamAuthSignalChannel::send_ticket(self, signal)
        }

        fn send_manifest(&self, signal: BootstrapManifestSignal) -> Result<(), AuthSignalError> {
            SteamAuthSignalChannel::send_manifest(self, signal)
        }

        fn receive(&self) -> Result<Vec<AuthSignalIngress>, AuthSignalError> {
            SteamAuthSignalChannel::receive(self)
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

    pub(super) struct NativeOnlineCore<B, S, F>
    where
        B: SteamBackend,
        S: NativeAuthSignalPort,
        F: NativeTransportFactory<B>,
    {
        // Rust drops fields in declaration order. Endpoint owners close before
        // coordinator transports; signaling callbacks and coordinator-owned
        // auth/transport state release before the Steam platform.
        pub(super) endpoints: VecDeque<NativeOnlineEndpoint>,
        signaling: S,
        pub(super) coordinator: OnlineLobbyCoordinator,
        platform: SteamPlatform<B>,
        transport_factory: F,
        local_declaration: Option<OnlineRosterMember>,
        ticket_exchanges: [Option<TicketExchange>; MAX_STEAM_LOBBY_MEMBERS],
        authenticated: [Option<AuthenticatedMapping>; MAX_STEAM_LOBBY_MEMBERS],
        reconnect_users: [Option<SteamUserId>; MAX_STEAM_LOBBY_MEMBERS],
        signal_rejected_users: [Option<SteamUserId>; MAX_STEAM_LOBBY_MEMBERS],
        pub(super) committed_roster: Option<CommittedAuthenticatedRoster>,
        pub(super) events: VecDeque<OnlineLobbyEvent>,
        pending_manifest: Option<BootstrapManifestSignal>,
        pub(super) admission_quiesced: bool,
        last_now_ms: u64,
        pub(super) runtime_failure: Option<OnlineFailure>,
    }

    impl<B, S, F> NativeOnlineCore<B, S, F>
    where
        B: SteamBackend,
        S: NativeAuthSignalPort,
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
            signaling: S,
            transport_factory: F,
            lobby_config: OnlineLobbyConfig,
            now_ms: u64,
        ) -> Result<Self, NativeOnlineRuntimeError> {
            let coordinator =
                OnlineLobbyCoordinator::new(platform.local_user(), lobby_config, now_ms)?;
            Ok(Self {
                endpoints: VecDeque::with_capacity(MAX_STEAM_LOBBY_MEMBERS),
                signaling,
                coordinator,
                platform,
                transport_factory,
                local_declaration: None,
                ticket_exchanges: [None; MAX_STEAM_LOBBY_MEMBERS],
                authenticated: [None; MAX_STEAM_LOBBY_MEMBERS],
                reconnect_users: [None; MAX_STEAM_LOBBY_MEMBERS],
                signal_rejected_users: [None; MAX_STEAM_LOBBY_MEMBERS],
                committed_roster: None,
                events: VecDeque::with_capacity(MAX_NATIVE_ONLINE_EVENTS),
                pending_manifest: None,
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
                NativeOnlineCommand::CommitManifest {
                    options,
                    current_tick,
                } => self
                    .coordinator
                    .commit_manifest(&mut self.platform, options, current_tick, now_ms)
                    .map_err(Into::into),
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
                    self.signaling.quiesce_admission()?;
                    self.ticket_exchanges = [None; MAX_STEAM_LOBBY_MEMBERS];
                    self.reconnect_users = [None; MAX_STEAM_LOBBY_MEMBERS];
                    self.endpoints.clear();
                    self.pending_manifest = None;
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
            if result.is_ok() {
                self.runtime_failure = None;
            }
            result
        }

        pub(super) fn pump(&mut self, now_ms: u64) -> Result<(), NativeOnlineRuntimeError> {
            if now_ms < self.last_now_ms {
                return Err(NativeOnlineRuntimeError::TimeRegression);
            }
            self.last_now_ms = now_ms;
            if self.admission_quiesced {
                self.signaling.quiesce_admission()?;
            } else {
                self.signaling.refresh_policy(self.auth_signal_admission());
            }
            self.coordinator.pump(&mut self.platform, now_ms)?;
            if self.admission_quiesced {
                self.signaling.quiesce_admission()?;
            } else {
                self.signaling.refresh_policy(self.auth_signal_admission());
            }
            self.drain_coordinator_events(now_ms)?;
            if !self.admission_quiesced {
                self.install_requested_transport(now_ms)?;
                self.reconcile_ticket_exchanges()?;
            }
            self.drain_coordinator_events(now_ms)?;
            if !self.admission_quiesced {
                self.try_apply_pending_manifest(now_ms)?;
            }
            for ingress in self.signaling.receive()? {
                if self.admission_quiesced {
                    continue;
                }
                match ingress {
                    AuthSignalIngress::Rejected { source, error } => {
                        self.isolate_signal_peer(source, error)?;
                    }
                    AuthSignalIngress::Accepted { source, signal } => {
                        let result = match signal {
                            PreGameSignal::Ticket(signal) => self
                                .consume_ticket_signal(source, signal, now_ms)
                                .map(|_| ()),
                            PreGameSignal::Manifest(signal) => {
                                self.consume_manifest_signal(source, signal, now_ms)
                            }
                        };
                        match result {
                            Ok(()) => {}
                            Err(NativeOnlineRuntimeError::Signal(error)) => {
                                self.isolate_signal_peer(source, error)?;
                            }
                            Err(error) => return Err(error),
                        }
                    }
                }
            }
            if !self.admission_quiesced {
                self.try_apply_pending_manifest(now_ms)?;
            }
            self.drain_coordinator_events(now_ms)?;
            Ok(())
        }

        fn auth_signal_admission(&self) -> AuthSignalAdmission {
            let mut admission = AuthSignalAdmission {
                active_lobby: None,
                users: [None; MAX_STEAM_LOBBY_MEMBERS],
            };
            let SteamPlatformState::InLobby(lobby) = self.platform.state() else {
                return admission;
            };
            admission.active_lobby = Some(lobby);
            let local = self.platform.local_user();
            for member in self.platform.roster().iter().flatten() {
                if member.user == local {
                    continue;
                }
                if let Some(slot) = admission.users.iter_mut().find(|slot| slot.is_none()) {
                    *slot = Some(member.user);
                }
            }
            for lease in self
                .coordinator
                .authorized_auth_signal_leases()
                .iter()
                .flatten()
                .copied()
            {
                if lease.user == local || admission.users.contains(&Some(lease.user)) {
                    continue;
                }
                if let Some(slot) = admission.users.iter_mut().find(|slot| slot.is_none()) {
                    *slot = Some(lease.user);
                }
            }
            admission
        }

        fn isolate_signal_peer(
            &mut self,
            user: SteamUserId,
            error: AuthSignalError,
        ) -> Result<(), NativeOnlineRuntimeError> {
            self.signaling.quarantine_peer(user)?;
            if self.is_signal_rejected_user(user) {
                return Ok(());
            }
            let connection = self.coordinator.active_connection_for_user(user);
            self.coordinator
                .isolate_peer_authentication(&mut self.platform, user)?;
            self.mark_signal_rejected_user(user)?;
            self.clear_peer_handoffs(user);
            self.push_event(OnlineLobbyEvent::PeerAuthenticationRejected {
                user,
                connection,
                failure: auth_signal_peer_failure(error),
            })
        }

        fn install_requested_transport(
            &mut self,
            now_ms: u64,
        ) -> Result<(), NativeOnlineRuntimeError> {
            let Some(session) = self.coordinator.take_transport_request() else {
                return Ok(());
            };
            let transport = self.transport_factory.create_transport(
                &self.platform,
                session,
                self.coordinator.config().transport,
                now_ms,
            )?;
            self.coordinator.install_transport(transport, now_ms)?;
            Ok(())
        }

        fn drain_coordinator_events(
            &mut self,
            now_ms: u64,
        ) -> Result<(), NativeOnlineRuntimeError> {
            let mut drained = 0;
            while let Some(event) = self.coordinator.poll_event() {
                drained += 1;
                if drained > MAX_NATIVE_ONLINE_EVENTS {
                    return Err(NativeOnlineRuntimeError::Capacity);
                }
                match event {
                    OnlineLobbyEvent::TransportRequested(_) => {}
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
                            if reconnect {
                                self.clear_reconnect_user(user);
                            }
                            self.push_event(event)?;
                        }
                    }
                    OnlineLobbyEvent::PeerAuthenticationRejected {
                        user, connection, ..
                    } => {
                        // A platform authentication revocation is persistent for
                        // the member's current lobby lifetime. Quarantine it
                        // before clearing the exchange so reconciliation cannot
                        // issue a fresh ticket later in this pump.
                        if self.authentication_rejection_is_current(user, connection) {
                            self.signaling.quarantine_peer(user)?;
                            self.mark_signal_rejected_user(user)?;
                            self.clear_peer_handoffs(user);
                        }
                        self.push_event(event)?;
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
                        ..
                    } => {
                        self.remove_ticket_exchange(user);
                        self.clear_active_peer_transport(user, connection);
                        if reconnect_allowed {
                            self.mark_reconnect_user(user)?;
                        }
                        self.push_event(event)?;
                    }
                    OnlineLobbyEvent::RosterChanged { live_bindings, .. } => {
                        self.reconcile_live_bindings(live_bindings);
                        self.push_event(event)?;
                    }
                    OnlineLobbyEvent::ManifestCommitted(_) => {
                        self.committed_roster = Some(self.freeze_authenticated_roster()?);
                        self.send_committed_manifest()?;
                        let local_config = self
                            .coordinator
                            .match_config()
                            .cloned()
                            .ok_or(AuthSignalError::InvalidEnvelope)?;
                        self.coordinator
                            .accept_manifest(&self.platform, local_config, now_ms)?;
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
                        self.runtime_failure = Some(failure);
                        self.push_event(event)?;
                    }
                    _ => self.push_event(event)?,
                }
            }
            Ok(())
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
                        let complete = matches!(
                            member.readiness,
                            MemberReadiness::Declared { ready: true, .. }
                        ) && member.loadout.is_some();
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
                    let local_ready = self
                        .local_declaration
                        .is_some_and(|declaration| declaration.ready);
                    let initial_epoch_ready = local_ready
                        && self
                            .platform
                            .roster()
                            .iter()
                            .flatten()
                            .find(|member| member.user == owner)
                            .is_some_and(|member| {
                                matches!(
                                    member.readiness,
                                    MemberReadiness::Declared { ready: true, .. }
                                ) && member.loadout.is_some_and(|loadout| {
                                    self.coordinator
                                        .initial_authentication_allowed(owner, loadout.revision())
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
                if self.is_signal_rejected_user(user) || self.signaling.peer_is_quarantined(user)? {
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
                *slot = Some(TicketExchange { lease, sent: false });
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
            if exchange.lease.remote_user != remote_user || exchange.sent {
                return Err(AuthSignalError::InvalidEnvelope.into());
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
            self.signaling.send_ticket(signal)?;
            exchange.sent = true;
            self.ticket_exchanges[index] = Some(exchange);
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
            project_ticket_admission_result(self.coordinator.begin_peer_authentication(
                &mut self.platform,
                source,
                signal.sender_peer_id,
                signal.ticket(),
                signal.purpose,
                now_ms,
            ))?;
            Ok(true)
        }

        fn send_committed_manifest(&self) -> Result<(), NativeOnlineRuntimeError> {
            let status = self.coordinator.status();
            if status.role != Some(OnlineLobbyRole::ListenAuthority) {
                return Err(AuthSignalError::UnexpectedManifestSender.into());
            }
            let lobby = status.lobby.ok_or(AuthSignalError::WrongLobby)?;
            let local = self.platform.local_user();
            let manifest = self
                .coordinator
                .match_config()
                .ok_or(AuthSignalError::InvalidEnvelope)?
                .manifest;
            for member in self.platform.roster().iter().flatten() {
                if member.user == local {
                    continue;
                }
                if self.is_signal_rejected_user(member.user)
                    || self.signaling.peer_is_quarantined(member.user)?
                {
                    continue;
                }
                if !self
                    .authenticated
                    .iter()
                    .flatten()
                    .any(|mapping| mapping.user == member.user)
                {
                    return Err(NativeOnlineRuntimeError::InvalidAuthenticatedRoster);
                }
                let signal = BootstrapManifestSignal::new(lobby, local, member.user, manifest)?;
                self.signaling.send_manifest(signal)?;
            }
            Ok(())
        }

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
            for member in self.platform.roster().iter().flatten() {
                let mapping = self
                    .authenticated
                    .iter()
                    .flatten()
                    .find(|mapping| mapping.user == member.user)
                    .ok_or(NativeOnlineRuntimeError::InvalidAuthenticatedRoster)?;
                roster.push(mapping.peer)?;
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
            self.remove_ticket_exchange(user);
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
            self.signaling.reset_session_isolation()?;
            self.signal_rejected_users = [None; MAX_STEAM_LOBBY_MEMBERS];
            Ok(())
        }

        fn reset_match_handoff(&mut self) {
            self.ticket_exchanges = [None; MAX_STEAM_LOBBY_MEMBERS];
            self.reconnect_users = [None; MAX_STEAM_LOBBY_MEMBERS];
            self.committed_roster = None;
            self.endpoints.clear();
            self.pending_manifest = None;
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
        NativeOnlineCore<RealSteamBackend, SteamAuthSignalChannel, RealNativeTransportFactory>;

    #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
    impl NativeOnlineCore<RealSteamBackend, SteamAuthSignalChannel, RealNativeTransportFactory> {
        pub(super) fn initialize(
            release: NativeSteamReleaseConfig,
            lobby_config: OnlineLobbyConfig,
            now_ms: u64,
        ) -> Result<Self, NativeOnlineRuntimeError> {
            let platform = SteamPlatform::<RealSteamBackend>::initialize_steam_client(
                release.steam_client_config(),
                now_ms,
            )?;
            let signaling = SteamAuthSignalChannel::new(&platform);
            Self::from_parts(
                platform,
                signaling,
                RealNativeTransportFactory,
                lobby_config,
                now_ms,
            )
        }
    }

    #[cfg(test)]
    mod tests {
        use std::cell::RefCell;
        use std::rc::Rc;

        use super::*;
        use crate::steam_platform::{AuthenticatedSteamPeer, FakeSteamBackend, FakeSteamControl};
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

        impl NativeAuthSignalPort for FakeAuthSignalEndpoint {
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

        type FakeNativeOnlineCore =
            NativeOnlineCore<FakeSteamBackend, FakeAuthSignalEndpoint, FakeNativeTransportFactory>;

        struct FakeNativeCorePair {
            host: FakeNativeOnlineCore,
            client: FakeNativeOnlineCore,
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
                let bus = FakeAuthSignalBus::new();
                let network = FakeSteamTransportNetwork::new(64).unwrap();
                let (host_backend, host_control) = FakeSteamBackend::new(app_id, host_user);
                let (client_backend, client_control) = FakeSteamBackend::new(app_id, client_user);
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
                    bus.register(host_user).unwrap(),
                    FakeNativeTransportFactory {
                        network: network.clone(),
                    },
                    lobby_config,
                    0,
                )
                .unwrap();
                let mut client = NativeOnlineCore::from_parts(
                    client_platform,
                    bus.register(client_user).unwrap(),
                    FakeNativeTransportFactory { network },
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
                    "two-core fixture did not converge: host={:?}, client={:?}",
                    self.host.coordinator.status(),
                    self.client.coordinator.status()
                );
            }

            fn pump_until_authenticated_endpoints(&mut self) {
                self.pump_until(80, |pair| {
                    pair.host.endpoints.len() == 1
                        && pair.client.endpoints.len() == 1
                        && pair.host.authenticated.iter().flatten().count() == 2
                        && pair.client.authenticated.iter().flatten().count() == 2
                });
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
                        && pair.host.endpoints.len() == 1
                        && pair.client.endpoints.len() == 1
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
                let first_connection = self.host.endpoints.front().unwrap().admitted.connection;
                self.ready_and_commit(first_match_id);
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
            let host_endpoint = pair.host.endpoints.front().unwrap();
            let client_endpoint = pair.client.endpoints.front().unwrap();
            assert_eq!(host_endpoint.peer_id, pair.client_member.peer_id);
            assert_eq!(client_endpoint.peer_id, pair.host_member.peer_id);
            assert_eq!(host_endpoint.admitted.remote_user, pair.client_user);
            assert_eq!(client_endpoint.admitted.remote_user, pair.host_user);
            assert_eq!(
                host_endpoint.admitted.connection, client_endpoint.admitted.connection,
                "the two independent cores bind opposite ends of one fake physical generation"
            );
            assert!(!host_endpoint.reconnect);
            assert!(!client_endpoint.reconnect);
            assert_eq!(
                host_endpoint.admitted.admission.purpose,
                AdmissionPurpose::Initial
            );
            assert_eq!(
                client_endpoint.admitted.admission.purpose,
                AdmissionPurpose::Initial
            );
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
                    sent: false,
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
                    sent: true,
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
        let release = NativeSteamReleaseConfig::from_sources(Some("123456"), false, |_| None)
            .expect("a release binary uses its compile-time App ID without process configuration");
        assert_eq!(release.app_id().get(), 123_456);
        assert_eq!(
            restart_app_id_for_profile(release, true),
            Some(release.app_id())
        );
        assert_eq!(restart_app_id_for_profile(release, false), None);

        let same = NativeSteamReleaseConfig::from_sources(Some("123456"), false, |key| {
            (key == STEAM_APP_ID_ENV).then(|| "123456".to_owned())
        })
        .unwrap();
        assert_eq!(same, release);

        let mismatch = NativeSteamReleaseConfig::from_sources(Some("123456"), false, |key| {
            (key == STEAM_APP_ID_ENV).then(|| "654321".to_owned())
        })
        .unwrap_err();
        assert_eq!(mismatch, NativeOnlineConfigError::AppIdMismatch);

        let runtime_only = NativeSteamReleaseConfig::from_sources(None, false, |key| {
            (key == STEAM_APP_ID_ENV).then(|| "123456".to_owned())
        })
        .unwrap_err();
        assert_eq!(runtime_only, NativeOnlineConfigError::MissingAppId);
    }

    #[test]
    fn release_never_uses_spacewar_even_with_the_development_opt_in() {
        let error = NativeSteamReleaseConfig::from_sources(Some("480"), false, |key| {
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
        let mismatch = NativeSteamReleaseConfig::from_sources(Some("123456"), true, |key| {
            (key == STEAM_APP_ID_ENV).then(|| "654321".to_owned())
        })
        .unwrap_err();
        assert_eq!(mismatch, NativeOnlineConfigError::AppIdMismatch);

        let runtime_only = NativeSteamReleaseConfig::from_sources(None, true, |key| {
            (key == STEAM_APP_ID_ENV).then(|| "654321".to_owned())
        })
        .unwrap();
        assert_eq!(runtime_only.app_id().get(), 654_321);
    }

    #[test]
    fn auth_signal_round_trip_is_exact_and_debug_redacts_secret() {
        let (lobby, sender, recipient, peer) = ids();
        let match_id = crate::network_protocol::MatchId::new(*b"auth-ticket-v2-1").unwrap();
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
        assert_eq!(encoded.as_slice()[4], 2);
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
    fn auth_signal_v2_rejects_v1_zero_epochs_and_purpose_match_mismatch() {
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
        initial_with_match[44..60].copy_from_slice(b"auth-ticket-v2-2");
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
                transport_installed: false,
                relay_status: SteamRelayStatus::default(),
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
                transport_installed: false,
                relay_status: SteamRelayStatus::default(),
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
