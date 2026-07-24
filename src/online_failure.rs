//! Stable, localizable online failure projection for menus and overlays.
//!
//! Protocol/platform errors stay strongly typed internally. This module maps
//! them to a compact UI contract without exposing authentication material,
//! arbitrary remote strings, or backend implementation details.

use crate::network_protocol::{DisconnectCode, DisconnectMessage, RetryDisposition};
use crate::session::SessionError;
use crate::steam_platform::{
    LobbyExitReason, PeerAuthenticationRejection, SteamBackendError, SteamPlatformError,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnlineFailureCode {
    SteamUnavailable,
    SteamDisconnected,
    OverlayUnavailable,
    InvalidReleaseConfiguration,
    LobbyUnavailable,
    LobbyFull,
    LobbyClosed,
    InviteRequired,
    FriendRequired,
    NotLobbyOwner,
    PublicPlayDisabled,
    InvalidSeatCount,
    IncompatibleVersion,
    AuthenticationFailed,
    OwnershipFailed,
    PlatformBanned,
    AuthenticationTimedOut,
    ConnectionTimedOut,
    NetworkQualityRejected,
    LoadingTimedOut,
    SynchronizationFailed,
    ClockSynchronizationFailed,
    InvalidInput,
    MalformedTraffic,
    RateLimited,
    Kicked,
    AuthorityLost,
    ServerShutdown,
    DedicatedUnavailable,
    InternalCapacity,
    InternalFailure,
}

impl OnlineFailureCode {
    /// Stable localization key. Text can change without changing protocol or
    /// diagnostic identity.
    pub const fn message_key(self) -> &'static str {
        match self {
            Self::SteamUnavailable => "online.error.steam_unavailable",
            Self::SteamDisconnected => "online.error.steam_disconnected",
            Self::OverlayUnavailable => "online.error.overlay_unavailable",
            Self::InvalidReleaseConfiguration => "online.error.release_configuration",
            Self::LobbyUnavailable => "online.error.lobby_unavailable",
            Self::LobbyFull => "online.error.lobby_full",
            Self::LobbyClosed => "online.error.lobby_closed",
            Self::InviteRequired => "online.error.invite_required",
            Self::FriendRequired => "online.error.friend_required",
            Self::NotLobbyOwner => "online.error.not_lobby_owner",
            Self::PublicPlayDisabled => "online.error.public_play_disabled",
            Self::InvalidSeatCount => "online.error.invalid_seat_count",
            Self::IncompatibleVersion => "online.error.incompatible_version",
            Self::AuthenticationFailed => "online.error.authentication_failed",
            Self::OwnershipFailed => "online.error.ownership_failed",
            Self::PlatformBanned => "online.error.platform_banned",
            Self::AuthenticationTimedOut => "online.error.authentication_timeout",
            Self::ConnectionTimedOut => "online.error.connection_timeout",
            Self::NetworkQualityRejected => "online.error.network_quality_rejected",
            Self::LoadingTimedOut => "online.error.loading_timeout",
            Self::SynchronizationFailed => "online.error.synchronization_failed",
            Self::ClockSynchronizationFailed => "online.error.clock_sync_failed",
            Self::InvalidInput => "online.error.invalid_input",
            Self::MalformedTraffic => "online.error.malformed_traffic",
            Self::RateLimited => "online.error.rate_limited",
            Self::Kicked => "online.error.kicked",
            Self::AuthorityLost => "online.error.authority_lost",
            Self::ServerShutdown => "online.error.server_shutdown",
            Self::DedicatedUnavailable => "online.error.dedicated_unavailable",
            Self::InternalCapacity => "online.error.capacity",
            Self::InternalFailure => "online.error.internal",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnlineFailureSeverity {
    Notice,
    Recoverable,
    MatchEnded,
    Fatal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnlineRecoveryAction {
    Dismiss,
    Retry,
    ReturnToLobby,
    Reconnect,
    ReturnToMenu,
    MatchEndedNoContest,
    DisableOnline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OnlineFailure {
    pub code: OnlineFailureCode,
    pub severity: OnlineFailureSeverity,
    pub recovery: OnlineRecoveryAction,
    /// Stable subsystem detail. Never display it as remote-authored text.
    pub detail_code: u16,
}

impl OnlineFailure {
    pub const fn message_key(self) -> &'static str {
        self.code.message_key()
    }

    pub const fn overlay_unavailable() -> Self {
        Self {
            code: OnlineFailureCode::OverlayUnavailable,
            severity: OnlineFailureSeverity::Notice,
            recovery: OnlineRecoveryAction::Dismiss,
            detail_code: 0,
        }
    }

    pub const fn from_disconnect(message: DisconnectMessage) -> Self {
        let code = match message.code {
            DisconnectCode::ClientRequested => OnlineFailureCode::LobbyClosed,
            DisconnectCode::Timeout => OnlineFailureCode::ConnectionTimedOut,
            DisconnectCode::AuthenticationFailed => OnlineFailureCode::AuthenticationFailed,
            DisconnectCode::OwnershipFailed => OnlineFailureCode::OwnershipFailed,
            DisconnectCode::IncompatibleProtocol
            | DisconnectCode::IncompatibleSimulation
            | DisconnectCode::IncompatibleBuild
            | DisconnectCode::IncompatibleContent => OnlineFailureCode::IncompatibleVersion,
            DisconnectCode::InvalidInput => OnlineFailureCode::InvalidInput,
            DisconnectCode::MalformedTraffic => OnlineFailureCode::MalformedTraffic,
            DisconnectCode::RateLimited => OnlineFailureCode::RateLimited,
            DisconnectCode::Kicked => OnlineFailureCode::Kicked,
            DisconnectCode::AuthorityLost => OnlineFailureCode::AuthorityLost,
            DisconnectCode::ServerShutdown => OnlineFailureCode::ServerShutdown,
        };
        let recovery = match message.retry {
            RetryDisposition::ReturnToLobby => OnlineRecoveryAction::ReturnToLobby,
            RetryDisposition::ReconnectAllowed => OnlineRecoveryAction::Reconnect,
            RetryDisposition::MatchEndedNoContest => OnlineRecoveryAction::MatchEndedNoContest,
            RetryDisposition::Fatal => OnlineRecoveryAction::ReturnToMenu,
        };
        let severity = match message.retry {
            RetryDisposition::ReconnectAllowed | RetryDisposition::ReturnToLobby => {
                OnlineFailureSeverity::Recoverable
            }
            RetryDisposition::MatchEndedNoContest => OnlineFailureSeverity::MatchEnded,
            RetryDisposition::Fatal => OnlineFailureSeverity::Fatal,
        };
        Self {
            code,
            severity,
            recovery,
            detail_code: message.detail_code,
        }
    }

    pub const fn from_lobby_exit(reason: LobbyExitReason) -> Option<Self> {
        let (code, severity, recovery, detail_code) = match reason {
            LobbyExitReason::Requested => return None,
            LobbyExitReason::JoinRejected => (
                OnlineFailureCode::LobbyUnavailable,
                OnlineFailureSeverity::Recoverable,
                OnlineRecoveryAction::Retry,
                1,
            ),
            LobbyExitReason::SteamDisconnected => (
                OnlineFailureCode::SteamDisconnected,
                OnlineFailureSeverity::Recoverable,
                OnlineRecoveryAction::Retry,
                2,
            ),
            LobbyExitReason::Removed => (
                OnlineFailureCode::Kicked,
                OnlineFailureSeverity::Recoverable,
                OnlineRecoveryAction::ReturnToMenu,
                3,
            ),
            LobbyExitReason::AuthorityLost => (
                OnlineFailureCode::AuthorityLost,
                OnlineFailureSeverity::MatchEnded,
                OnlineRecoveryAction::MatchEndedNoContest,
                4,
            ),
            LobbyExitReason::ValidationFailed => (
                OnlineFailureCode::IncompatibleVersion,
                OnlineFailureSeverity::Recoverable,
                OnlineRecoveryAction::ReturnToMenu,
                5,
            ),
        };
        Some(Self {
            code,
            severity,
            recovery,
            detail_code,
        })
    }

    pub const fn from_auth_rejection(reason: PeerAuthenticationRejection) -> Self {
        use crate::steam_platform::AuthValidationFailure;

        let code = match reason {
            PeerAuthenticationRejection::Validation(
                AuthValidationFailure::VacBanned | AuthValidationFailure::PublisherBan,
            ) => OnlineFailureCode::PlatformBanned,
            PeerAuthenticationRejection::DoesNotHaveLicense => OnlineFailureCode::OwnershipFailed,
            PeerAuthenticationRejection::NoAuthentication
            | PeerAuthenticationRejection::Validation(_) => OnlineFailureCode::AuthenticationFailed,
            PeerAuthenticationRejection::IntentExpired => OnlineFailureCode::AuthenticationTimedOut,
        };
        Self {
            code,
            severity: OnlineFailureSeverity::Fatal,
            recovery: OnlineRecoveryAction::ReturnToMenu,
            detail_code: 0,
        }
    }

    pub const fn from_session(error: SessionError) -> Self {
        let (code, severity, recovery) = match error {
            SessionError::CompatibilityMismatch
            | SessionError::ManifestMismatch
            | SessionError::SnapshotAfterStart
            | SessionError::StartTickMismatch => (
                OnlineFailureCode::IncompatibleVersion,
                OnlineFailureSeverity::Fatal,
                OnlineRecoveryAction::ReturnToMenu,
            ),
            SessionError::PeerMismatch => (
                OnlineFailureCode::OwnershipFailed,
                OnlineFailureSeverity::Fatal,
                OnlineRecoveryAction::ReturnToMenu,
            ),
            SessionError::ClockNotSynchronized => (
                OnlineFailureCode::ClockSynchronizationFailed,
                OnlineFailureSeverity::Recoverable,
                OnlineRecoveryAction::Retry,
            ),
            SessionError::MissingManifest
            | SessionError::ResultBeforeFight
            | SessionError::ResultIdZero
            | SessionError::TimelineExhausted
            | SessionError::InvalidTimeoutPolicy
            | SessionError::InvalidTransition { .. }
            | SessionError::Protocol(_)
            | SessionError::SessionFailed => (
                OnlineFailureCode::SynchronizationFailed,
                OnlineFailureSeverity::Fatal,
                OnlineRecoveryAction::ReturnToLobby,
            ),
        };
        Self {
            code,
            severity,
            recovery,
            detail_code: 0,
        }
    }

    pub const fn from_steam(error: SteamPlatformError) -> Self {
        let (code, severity, recovery) = match error {
            SteamPlatformError::Backend(SteamBackendError::NotLoggedOn) => (
                OnlineFailureCode::SteamDisconnected,
                OnlineFailureSeverity::Recoverable,
                OnlineRecoveryAction::Retry,
            ),
            SteamPlatformError::Backend(SteamBackendError::InitializationFailed)
            | SteamPlatformError::Backend(SteamBackendError::AlreadyInitialized) => (
                OnlineFailureCode::SteamUnavailable,
                OnlineFailureSeverity::Fatal,
                OnlineRecoveryAction::DisableOnline,
            ),
            SteamPlatformError::Backend(_)
            | SteamPlatformError::Faulted
            | SteamPlatformError::EventQueueOverflow => (
                OnlineFailureCode::InternalFailure,
                OnlineFailureSeverity::Fatal,
                OnlineRecoveryAction::ReturnToMenu,
            ),
            SteamPlatformError::Protocol(_)
            | SteamPlatformError::MetadataMismatch
            | SteamPlatformError::MetadataMissing
            | SteamPlatformError::VisibilityMismatch => (
                OnlineFailureCode::IncompatibleVersion,
                OnlineFailureSeverity::Recoverable,
                OnlineRecoveryAction::ReturnToMenu,
            ),
            SteamPlatformError::SpacewarRequiresExplicitOptIn
            | SteamPlatformError::SpacewarForbiddenInProduction
            | SteamPlatformError::ZeroIdentifier
            | SteamPlatformError::InvalidEventCapacity
            | SteamPlatformError::InvalidTimeout
            | SteamPlatformError::InvalidRegion
            | SteamPlatformError::InvalidAuthority
            | SteamPlatformError::InvalidMetadata
            | SteamPlatformError::InvalidConnectCommand
            | SteamPlatformError::ConnectCommandTooLong
            | SteamPlatformError::DuplicateConnectLobby => (
                OnlineFailureCode::InvalidReleaseConfiguration,
                OnlineFailureSeverity::Fatal,
                OnlineRecoveryAction::DisableOnline,
            ),
            SteamPlatformError::PublicLobbiesDisabled => (
                OnlineFailureCode::PublicPlayDisabled,
                OnlineFailureSeverity::Notice,
                OnlineRecoveryAction::Dismiss,
            ),
            SteamPlatformError::InvalidSeatCount => (
                OnlineFailureCode::InvalidSeatCount,
                OnlineFailureSeverity::Recoverable,
                OnlineRecoveryAction::Dismiss,
            ),
            SteamPlatformError::LobbyCapacityExceeded => (
                OnlineFailureCode::LobbyFull,
                OnlineFailureSeverity::Recoverable,
                OnlineRecoveryAction::ReturnToMenu,
            ),
            SteamPlatformError::InvalidState | SteamPlatformError::UnexpectedLobby => (
                OnlineFailureCode::LobbyClosed,
                OnlineFailureSeverity::Recoverable,
                OnlineRecoveryAction::ReturnToMenu,
            ),
            SteamPlatformError::JoinIntentExpired
            | SteamPlatformError::MemberNotInExpectedLobby
            | SteamPlatformError::MemberMetadataPending => (
                OnlineFailureCode::LobbyUnavailable,
                OnlineFailureSeverity::Recoverable,
                OnlineRecoveryAction::Retry,
            ),
            SteamPlatformError::PrivateLobbyRequiresInvite => (
                OnlineFailureCode::InviteRequired,
                OnlineFailureSeverity::Recoverable,
                OnlineRecoveryAction::ReturnToMenu,
            ),
            SteamPlatformError::FriendsRelationshipRequired => (
                OnlineFailureCode::FriendRequired,
                OnlineFailureSeverity::Recoverable,
                OnlineRecoveryAction::ReturnToMenu,
            ),
            SteamPlatformError::NotLobbyOwner => (
                OnlineFailureCode::NotLobbyOwner,
                OnlineFailureSeverity::Recoverable,
                OnlineRecoveryAction::Dismiss,
            ),
            SteamPlatformError::AuthTicketEmpty
            | SteamPlatformError::AuthTicketTooLarge
            | SteamPlatformError::AuthenticationRejected
            | SteamPlatformError::AdmissionAlreadyConsumed => (
                OnlineFailureCode::AuthenticationFailed,
                OnlineFailureSeverity::Fatal,
                OnlineRecoveryAction::ReturnToMenu,
            ),
            SteamPlatformError::AuthIntentExpired => (
                OnlineFailureCode::AuthenticationTimedOut,
                OnlineFailureSeverity::Recoverable,
                OnlineRecoveryAction::Retry,
            ),
            SteamPlatformError::AuthenticationPending => (
                OnlineFailureCode::AuthenticationFailed,
                OnlineFailureSeverity::Notice,
                OnlineRecoveryAction::Dismiss,
            ),
            SteamPlatformError::AuthCapacityExceeded => (
                OnlineFailureCode::InternalCapacity,
                OnlineFailureSeverity::Fatal,
                OnlineRecoveryAction::ReturnToMenu,
            ),
            SteamPlatformError::DedicatedSdrUnavailable => (
                OnlineFailureCode::DedicatedUnavailable,
                OnlineFailureSeverity::Fatal,
                OnlineRecoveryAction::ReturnToMenu,
            ),
        };
        Self {
            code,
            severity,
            recovery,
            detail_code: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnect_retry_policy_drives_recovery_without_remote_text() {
        let failure = OnlineFailure::from_disconnect(DisconnectMessage {
            match_id: None,
            code: DisconnectCode::AuthorityLost,
            retry: RetryDisposition::MatchEndedNoContest,
            detail_code: 91,
            last_confirmed_tick: None,
        });
        assert_eq!(failure.code, OnlineFailureCode::AuthorityLost);
        assert_eq!(failure.severity, OnlineFailureSeverity::MatchEnded);
        assert_eq!(failure.recovery, OnlineRecoveryAction::MatchEndedNoContest);
        assert_eq!(failure.detail_code, 91);
        assert_eq!(failure.message_key(), "online.error.authority_lost");
    }

    #[test]
    fn every_disconnect_code_has_a_stable_local_projection() {
        for (code, expected) in [
            (
                DisconnectCode::ClientRequested,
                OnlineFailureCode::LobbyClosed,
            ),
            (
                DisconnectCode::Timeout,
                OnlineFailureCode::ConnectionTimedOut,
            ),
            (
                DisconnectCode::AuthenticationFailed,
                OnlineFailureCode::AuthenticationFailed,
            ),
            (
                DisconnectCode::OwnershipFailed,
                OnlineFailureCode::OwnershipFailed,
            ),
            (
                DisconnectCode::IncompatibleProtocol,
                OnlineFailureCode::IncompatibleVersion,
            ),
            (
                DisconnectCode::IncompatibleSimulation,
                OnlineFailureCode::IncompatibleVersion,
            ),
            (
                DisconnectCode::IncompatibleBuild,
                OnlineFailureCode::IncompatibleVersion,
            ),
            (
                DisconnectCode::IncompatibleContent,
                OnlineFailureCode::IncompatibleVersion,
            ),
            (
                DisconnectCode::InvalidInput,
                OnlineFailureCode::InvalidInput,
            ),
            (
                DisconnectCode::MalformedTraffic,
                OnlineFailureCode::MalformedTraffic,
            ),
            (DisconnectCode::RateLimited, OnlineFailureCode::RateLimited),
            (DisconnectCode::Kicked, OnlineFailureCode::Kicked),
            (
                DisconnectCode::AuthorityLost,
                OnlineFailureCode::AuthorityLost,
            ),
            (
                DisconnectCode::ServerShutdown,
                OnlineFailureCode::ServerShutdown,
            ),
        ] {
            let failure = OnlineFailure::from_disconnect(DisconnectMessage {
                match_id: None,
                code,
                retry: RetryDisposition::Fatal,
                detail_code: 0xAFC,
                last_confirmed_tick: None,
            });
            assert_eq!(failure.code, expected, "{code:?}");
            assert_eq!(failure.detail_code, 0xAFC);
        }
    }

    #[test]
    fn every_disconnect_disposition_controls_severity_and_recovery() {
        for (retry, severity, recovery) in [
            (
                RetryDisposition::ReturnToLobby,
                OnlineFailureSeverity::Recoverable,
                OnlineRecoveryAction::ReturnToLobby,
            ),
            (
                RetryDisposition::ReconnectAllowed,
                OnlineFailureSeverity::Recoverable,
                OnlineRecoveryAction::Reconnect,
            ),
            (
                RetryDisposition::MatchEndedNoContest,
                OnlineFailureSeverity::MatchEnded,
                OnlineRecoveryAction::MatchEndedNoContest,
            ),
            (
                RetryDisposition::Fatal,
                OnlineFailureSeverity::Fatal,
                OnlineRecoveryAction::ReturnToMenu,
            ),
        ] {
            let failure = OnlineFailure::from_disconnect(DisconnectMessage {
                match_id: None,
                code: DisconnectCode::ServerShutdown,
                retry,
                detail_code: 17,
                last_confirmed_tick: None,
            });
            assert_eq!(failure.severity, severity, "{retry:?}");
            assert_eq!(failure.recovery, recovery, "{retry:?}");
        }
    }

    #[test]
    fn overlay_unavailable_is_a_local_dismissible_notice() {
        let failure = OnlineFailure::overlay_unavailable();
        assert_eq!(failure.code, OnlineFailureCode::OverlayUnavailable);
        assert_eq!(failure.severity, OnlineFailureSeverity::Notice);
        assert_eq!(failure.recovery, OnlineRecoveryAction::Dismiss);
        assert_eq!(failure.detail_code, 0);
        assert_eq!(failure.message_key(), "online.error.overlay_unavailable");
    }

    #[test]
    fn vac_and_publisher_bans_have_a_distinct_fatal_projection() {
        use crate::steam_platform::AuthValidationFailure;

        for reason in [
            AuthValidationFailure::VacBanned,
            AuthValidationFailure::PublisherBan,
        ] {
            let failure =
                OnlineFailure::from_auth_rejection(PeerAuthenticationRejection::Validation(reason));
            assert_eq!(failure.code, OnlineFailureCode::PlatformBanned);
            assert_eq!(failure.severity, OnlineFailureSeverity::Fatal);
        }
    }

    #[test]
    fn expected_platform_failures_have_actionable_recovery() {
        let full = OnlineFailure::from_steam(SteamPlatformError::LobbyCapacityExceeded);
        assert_eq!(full.code, OnlineFailureCode::LobbyFull);
        assert_eq!(full.recovery, OnlineRecoveryAction::ReturnToMenu);

        let timeout = OnlineFailure::from_steam(SteamPlatformError::AuthIntentExpired);
        assert_eq!(timeout.code, OnlineFailureCode::AuthenticationTimedOut);
        assert_eq!(timeout.recovery, OnlineRecoveryAction::Retry);

        let dedicated = OnlineFailure::from_steam(SteamPlatformError::DedicatedSdrUnavailable);
        assert_eq!(dedicated.code, OnlineFailureCode::DedicatedUnavailable);
        assert_eq!(dedicated.severity, OnlineFailureSeverity::Fatal);
    }

    #[test]
    fn requested_lobby_exit_does_not_create_an_error_dialog() {
        assert_eq!(
            OnlineFailure::from_lobby_exit(LobbyExitReason::Requested),
            None
        );
    }
}
