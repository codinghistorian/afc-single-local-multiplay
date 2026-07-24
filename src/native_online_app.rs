//! Player-facing native online composition and Bevy UI.
//!
//! This layer is deliberately outside the canonical simulation. It owns the
//! listen/remote worker handles, translates bounded platform handoffs into
//! those workers, samples only locally owned couch seats, and projects the
//! latest predicted snapshot into the rendered world. The Steam/runtime API is
//! abstracted behind [`NativeOnlineRuntimePort`] so the menu lifecycle can be
//! tested without Steam or real network endpoints.

use core::fmt;
use std::collections::VecDeque;

use arrayvec::ArrayVec;
use bevy::prelude::*;
use bevy::time::Real;
use bevy::ui::UiTargetCamera;

use crate::arena_defs::{ActiveArena, arena_definitions};
use crate::authority_peer_hub::AuthorityConnectionId;
use crate::camera::{GameplayCameraControl, UiCamera, camera_relative_direction};
use crate::components::{PlayerControlBindings, PlayerKeyBindings};
use crate::game_state::{MatchState, RULE_PRESETS};
use crate::headless::HeadlessMatchConfig;
use crate::listen_authority::{
    ListenAuthenticatedRoster, ListenAuthorityCommand, ListenAuthorityConfig, ListenAuthorityEvent,
    ListenAuthorityOperation, ListenAuthorityPhase, ListenAuthoritySubmitOutcome,
    ListenOnlineMatch,
};
use crate::match_presentation::{
    ConfirmedMatchPresentation, MatchPresentationPolicy, OnlinePanelMode, PresentationMusicTrack,
    PresentationPhase, PresentationResultSfx, PresentedAbortReason, PresentedLocalOutcome,
    PresentedMatchOutcome,
};
use crate::native_online::{
    CommittedAuthenticatedRoster, NativeOnlineActions, NativeOnlineAvailability,
    NativeOnlineCommand, NativeOnlineCreateRequest, NativeOnlineEndpoint, NativeOnlineRuntime,
    NativeOnlineRuntimeError, NativeOnlineScreen, NativeOnlineUnavailableReason,
    NativeOnlineViewModel, NativeOnlineVisibility,
};
use crate::network_protocol::{
    DefinitionId, MAX_LOCAL_SEATS, MatchId, PeerId, ReconnectClaim, RetryDisposition, TeamId,
};
use crate::network_quality::{
    InputDelayCalibrationSnapshot, InputDelayCalibrationState, NetworkQuality,
    NetworkQualitySample, NetworkQualitySnapshot,
};
use crate::online_failure::{
    OnlineFailure, OnlineFailureCode, OnlineFailureSeverity, OnlineRecoveryAction,
};
use crate::online_lobby::{
    OnlineLobbyEvent, OnlineLobbyRole, OnlineMatchOutcome, OnlinePeerIdentity,
};
use crate::online_roster::{OnlineManifestOptions, OnlineRosterMember, OnlineSeatSelection};
use crate::reconnect::{AuthenticatedPeer, AuthenticatedUserId};
use crate::remote_online_client::{
    RemoteAuthorityDisconnect, RemoteCommandSubmitOutcome, RemoteOnlineClient,
    RemoteOnlineClientConfig, RemoteOnlineClientPhase, RemoteOnlineClientStartError,
    RemoteOnlinePresentationError, RemoteOnlineTerminal,
};
use crate::simulation::{SimTick, SimulationDriveMode};
use crate::steam_platform::{
    LobbyJoinIntent, MAX_STEAM_INPUT_CONTROLLERS, MAX_STEAM_LOBBY_MEMBERS, RegionCode,
    SteamInputActionSet, SteamInputControllerSnapshot, SteamInputSnapshot, SteamMenuAction,
    SteamMenuInputMask, SteamOverlayRequestStatus, SteamUserId,
};
use crate::steam_transport::SteamConnectionId;
use crate::tick_input::{
    InputMask, LocalSeatId, LocalTickInputState, QuantizedMovement, RawInputButton,
    RenderInputSample,
};
use crate::user_mode::{UserModeGameplayScene, UserModeState};

const ONLINE_MAX_STAGED_ENDPOINTS: usize = MAX_STEAM_LOBBY_MEMBERS;
const ONLINE_MAX_PENDING_AUTHORITY_COMMANDS: usize = 16;
const ONLINE_MAX_AUTHORITY_ENDPOINT_RECORDS: usize = MAX_STEAM_LOBBY_MEMBERS * 2;
const ONLINE_COUNTDOWN_LEAD_TICKS: u64 = 120;
const ONLINE_DEFAULT_REGION: &str = "auto";
const ONLINE_MAX_STEAM_PEERS: u8 = 4;
const ONLINE_SEAT_CAPACITY: u8 = MAX_LOCAL_SEATS;
const ONLINE_COMPACT_PANEL_WIDTH_PERCENT: f32 = 38.0;
const ONLINE_COMPACT_PANEL_MAX_WIDTH: f32 = 420.0;
pub const OVERLAY_UNAVAILABLE_NOTICE_MS: u64 = 4_000;
/// Process exit keeps both authority and Steam owners alive through the
/// authority's two-second terminal deadline plus the transport's bounded
/// retirement margin. The emergency path begins only after this outer bound.
pub const NATIVE_ONLINE_APP_EXIT_GRACE_MS: u64 = 2_500;

/// Narrow application-facing runtime contract. Tests substitute a fake port;
/// production delegates to the sole application-owned `NativeOnlineRuntime`.
pub trait NativeOnlineRuntimePort {
    fn view_model(&self) -> NativeOnlineViewModel;
    fn execute_port(
        &mut self,
        command: NativeOnlineCommand,
        now_ms: u64,
    ) -> Result<(), NativeOnlineRuntimeError>;
    fn open_invite_overlay_port(
        &mut self,
    ) -> Result<SteamOverlayRequestStatus, NativeOnlineRuntimeError>;
    fn poll_event_port(&mut self) -> Option<OnlineLobbyEvent>;
    fn take_endpoint_port(&mut self) -> Option<NativeOnlineEndpoint>;
    fn match_config_port(&self) -> Option<HeadlessMatchConfig>;
    fn committed_roster_port(&self) -> Option<CommittedAuthenticatedRoster>;
    /// Read-only committed peer values for application-level runtime fakes.
    ///
    /// Production derives this bounded copy exclusively from the runtime's
    /// already-validated committed roster. The listen startup path still
    /// reconstructs and validates [`ListenAuthenticatedRoster`] before
    /// spawning workers.
    fn committed_peers_port(&self) -> Option<ArrayVec<AuthenticatedPeer, MAX_STEAM_LOBBY_MEMBERS>> {
        let committed = self.committed_roster_port()?;
        let mut peers = ArrayVec::new();
        for peer in committed.iter() {
            peers.push(peer);
        }
        Some(peers)
    }
    fn local_authenticated_user_port(&self) -> Option<AuthenticatedUserId>;
    fn transport_retirement_pending_port(&self) -> bool {
        false
    }
    fn make_local_declaration_port(
        &self,
        peer_id: PeerId,
        revision: u16,
        ready: bool,
        seats: &[OnlineSeatSelection],
    ) -> Result<OnlineRosterMember, NativeOnlineRuntimeError>;
}

impl NativeOnlineRuntimePort for NativeOnlineRuntime {
    fn view_model(&self) -> NativeOnlineViewModel {
        NativeOnlineRuntime::view_model(self)
    }

    fn execute_port(
        &mut self,
        command: NativeOnlineCommand,
        now_ms: u64,
    ) -> Result<(), NativeOnlineRuntimeError> {
        NativeOnlineRuntime::execute(self, command, now_ms)
    }

    fn open_invite_overlay_port(
        &mut self,
    ) -> Result<SteamOverlayRequestStatus, NativeOnlineRuntimeError> {
        NativeOnlineRuntime::open_invite_overlay(self)
    }

    fn poll_event_port(&mut self) -> Option<OnlineLobbyEvent> {
        NativeOnlineRuntime::poll_event(self)
    }

    fn take_endpoint_port(&mut self) -> Option<NativeOnlineEndpoint> {
        NativeOnlineRuntime::take_endpoint(self)
    }

    fn match_config_port(&self) -> Option<HeadlessMatchConfig> {
        NativeOnlineRuntime::match_config(self).cloned()
    }

    fn committed_roster_port(&self) -> Option<CommittedAuthenticatedRoster> {
        NativeOnlineRuntime::committed_authenticated_roster(self)
    }

    fn local_authenticated_user_port(&self) -> Option<AuthenticatedUserId> {
        NativeOnlineRuntime::local_authenticated_user(self)
    }

    fn transport_retirement_pending_port(&self) -> bool {
        NativeOnlineRuntime::transport_retirement_pending(self)
    }

    fn make_local_declaration_port(
        &self,
        peer_id: PeerId,
        revision: u16,
        ready: bool,
        seats: &[OnlineSeatSelection],
    ) -> Result<OnlineRosterMember, NativeOnlineRuntimeError> {
        NativeOnlineRuntime::make_local_declaration(self, peer_id, revision, ready, seats)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeOnlineSessionKind {
    ListenOwner,
    RemoteClient,
}

enum ActiveNativeOnlineMatch {
    Listen(ListenOnlineMatch),
    Remote(RemoteOnlineClient),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProjectedConfirmedTerminal {
    match_id: MatchId,
    result: crate::session::ConfirmedSessionResult,
}

fn terminal_is_projected_for_match(
    terminal: Option<RemoteOnlineTerminal>,
    match_id: MatchId,
    projected: Option<ProjectedConfirmedTerminal>,
) -> bool {
    matches!(
        terminal,
        Some(RemoteOnlineTerminal::Completed(result))
            if projected == Some(ProjectedConfirmedTerminal { match_id, result })
    )
}

impl ActiveNativeOnlineMatch {
    fn kind(&self) -> NativeOnlineSessionKind {
        match self {
            Self::Listen(_) => NativeOnlineSessionKind::ListenOwner,
            Self::Remote(_) => NativeOnlineSessionKind::RemoteClient,
        }
    }

    fn client(&self) -> &RemoteOnlineClient {
        match self {
            Self::Listen(online_match) => &online_match.host_client,
            Self::Remote(client) => client,
        }
    }

    fn client_mut(&mut self) -> &mut RemoteOnlineClient {
        match self {
            Self::Listen(online_match) => &mut online_match.host_client,
            Self::Remote(client) => client,
        }
    }

    fn stop(mut self) {
        match &mut self {
            Self::Listen(online_match) => {
                online_match.host_client.stop();
                let _ = online_match.authority.shutdown();
            }
            Self::Remote(client) => client.stop(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AuthenticatedPeerBinding {
    user: SteamUserId,
    peer_id: PeerId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuthorityEndpointState {
    Submitted {
        reconnect: bool,
        terminal_expected: bool,
    },
    Attached {
        connection: AuthorityConnectionId,
        terminal_expected: bool,
    },
    TerminalDrained {
        connection: AuthorityConnectionId,
        retry: Option<RetryDisposition>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AuthorityEndpointRecord {
    user: SteamUserId,
    peer_id: PeerId,
    steam_connection: SteamConnectionId,
    state: AuthorityEndpointState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingAuthorityDetach {
    peer_id: PeerId,
    steam_connection: SteamConnectionId,
    defer_once: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingAuthorityRevocation {
    user: SteamUserId,
    steam_connection: SteamConnectionId,
}

/// Process-local entropy mixed into every online match identity and gameplay
/// seed. This value is deliberately not `Debug`: it must never enter logs or
/// diagnostics as an accidental stable process identifier.
#[derive(Clone, Copy, PartialEq, Eq)]
struct OnlineMatchNonce([u8; 32]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListenShutdownAction {
    Rematch,
    ReturnToLobby,
    LeaveOnline,
    ReturnToMenu,
    Retry,
    CoordinatorDrop,
    AppExit,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NativeOnlineApplicationMetrics {
    pub runtime_events: u64,
    pub endpoints_staged: u64,
    pub sessions_started: u64,
    pub sessions_stopped: u64,
    pub authority_commands_retried: u64,
    pub authority_command_rejections: u64,
    pub local_input_backpressure: u64,
    /// Reconnect claims intentionally use tick zero as a conservative hint.
    /// The authority still sends its current retained snapshot.
    pub conservative_full_resyncs: u64,
    pub platform_bans_forwarded: u64,
    pub authentication_revocations_forwarded: u64,
    pub authority_terminal_marks: u64,
    pub stale_authority_terminal_events: u64,
    pub graceful_shutdowns_started: u64,
    pub graceful_shutdowns_completed: u64,
    pub graceful_shutdown_emergency_fallbacks: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeOnlineApplicationError {
    Runtime,
    InvalidAction,
    MissingLocalIdentity,
    MissingLocalPeer,
    MissingJoinIntent,
    MissingManifest,
    MissingCommittedRoster,
    MissingListenHost,
    EndpointCapacity,
    AuthorityCommandCapacity,
    AuthorityDisconnected,
    ListenStart,
    RemoteStart,
    Presentation,
    TrustedResultsDisabled,
    DedicatedModeDisabled,
    EntropyUnavailable,
    TimelineExhausted,
}

impl fmt::Display for NativeOnlineApplicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "native online application failed: {self:?}")
    }
}

impl std::error::Error for NativeOnlineApplicationError {}

impl From<NativeOnlineRuntimeError> for NativeOnlineApplicationError {
    fn from(_: NativeOnlineRuntimeError) -> Self {
        Self::Runtime
    }
}

impl From<RemoteOnlineClientStartError> for NativeOnlineApplicationError {
    fn from(_: RemoteOnlineClientStartError) -> Self {
        Self::RemoteStart
    }
}

impl From<RemoteOnlinePresentationError> for NativeOnlineApplicationError {
    fn from(_: RemoteOnlinePresentationError) -> Self {
        Self::Presentation
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CouchSeatEditor {
    seats: [OnlineSeatSelection; MAX_LOCAL_SEATS as usize],
    seat_count: u8,
    selected_seat: u8,
    revision: u16,
    arena: DefinitionId,
    rules: DefinitionId,
}

impl Default for CouchSeatEditor {
    fn default() -> Self {
        let mut seats = [OnlineSeatSelection::default(); MAX_LOCAL_SEATS as usize];
        for (index, seat) in seats.iter_mut().enumerate() {
            *seat = OnlineSeatSelection {
                team: TeamId::new((index % 2) as u8).expect("default online team is valid"),
                character: DefinitionId::new(index as u16)
                    .expect("default character definition is valid"),
                style: DefinitionId::new((index % 3) as u16)
                    .expect("default style definition is valid"),
                equipment: DefinitionId::new((index % 4) as u16)
                    .expect("default equipment definition is valid"),
            };
        }
        Self {
            seats,
            seat_count: 1,
            selected_seat: 0,
            revision: 1,
            arena: DefinitionId::new(0).expect("default arena definition is valid"),
            rules: DefinitionId::new(0).expect("default rules definition is valid"),
        }
    }
}

impl CouchSeatEditor {
    fn seats(&self) -> &[OnlineSeatSelection] {
        &self.seats[..usize::from(self.seat_count)]
    }

    fn selected(&self) -> OnlineSeatSelection {
        self.seats[usize::from(self.selected_seat)]
    }

    fn selected_mut(&mut self) -> &mut OnlineSeatSelection {
        &mut self.seats[usize::from(self.selected_seat)]
    }

    fn revise(&mut self) -> Result<(), NativeOnlineApplicationError> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(NativeOnlineApplicationError::TimelineExhausted)?;
        Ok(())
    }

    fn add_seat(&mut self) -> Result<(), NativeOnlineApplicationError> {
        if self.seat_count >= MAX_LOCAL_SEATS {
            return Err(NativeOnlineApplicationError::InvalidAction);
        }
        self.revise()?;
        self.seat_count += 1;
        self.selected_seat = self.seat_count - 1;
        Ok(())
    }

    fn remove_seat(&mut self) -> Result<(), NativeOnlineApplicationError> {
        if self.seat_count <= 1 {
            return Err(NativeOnlineApplicationError::InvalidAction);
        }
        self.revise()?;
        self.seat_count -= 1;
        self.selected_seat = self.selected_seat.min(self.seat_count - 1);
        Ok(())
    }

    fn select_previous(&mut self) {
        self.selected_seat = if self.selected_seat == 0 {
            self.seat_count - 1
        } else {
            self.selected_seat - 1
        };
    }

    fn select_next(&mut self) {
        self.selected_seat = (self.selected_seat + 1) % self.seat_count;
    }

    fn cycle_character(&mut self, delta: i8) -> Result<(), NativeOnlineApplicationError> {
        self.revise()?;
        let current = self.selected().character.get() as i16;
        self.selected_mut().character =
            DefinitionId::new((current + i16::from(delta)).rem_euclid(8) as u16)
                .expect("wrapped character definition is valid");
        Ok(())
    }

    fn cycle_style(&mut self, delta: i8) -> Result<(), NativeOnlineApplicationError> {
        self.revise()?;
        let current = self.selected().style.get() as i16;
        self.selected_mut().style =
            DefinitionId::new((current + i16::from(delta)).rem_euclid(3) as u16)
                .expect("wrapped style definition is valid");
        Ok(())
    }

    fn cycle_equipment(&mut self, delta: i8) -> Result<(), NativeOnlineApplicationError> {
        self.revise()?;
        let current = self.selected().equipment.get() as i16;
        self.selected_mut().equipment =
            DefinitionId::new((current + i16::from(delta)).rem_euclid(4) as u16)
                .expect("wrapped equipment definition is valid");
        Ok(())
    }

    fn toggle_team(&mut self) -> Result<(), NativeOnlineApplicationError> {
        self.revise()?;
        let next = if self.selected().team.get() == 0 {
            1
        } else {
            0
        };
        self.selected_mut().team = TeamId::new(next).expect("two-team online choice is valid");
        Ok(())
    }

    fn cycle_arena(&mut self, delta: i8) {
        let count = arena_definitions().len() as i32;
        let next = (i32::from(self.arena.get()) + i32::from(delta)).rem_euclid(count);
        self.arena = DefinitionId::new(next as u16).expect("authored arena index is valid");
    }

    fn cycle_rules(&mut self, delta: i8) {
        let count = RULE_PRESETS.len() as i32;
        let next = (i32::from(self.rules.get()) + i32::from(delta)).rem_euclid(count);
        self.rules = DefinitionId::new(next as u16).expect("authored rules index is valid");
    }
}

/// Typed player actions. Unsupported trusted/dedicated requests remain as a
/// defensive application boundary for injected callers, but are never rendered
/// as first-release player-facing capabilities.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeOnlineUiAction {
    CreatePrivate,
    CreateFriends,
    AcceptJoin,
    DeclineJoin,
    InviteFriends,
    AddSeat,
    RemoveSeat,
    PreviousSeat,
    NextSeat,
    PreviousCharacter,
    NextCharacter,
    PreviousStyle,
    NextStyle,
    PreviousEquipment,
    NextEquipment,
    ToggleTeam,
    PreviousArena,
    NextArena,
    PreviousRules,
    NextRules,
    ToggleReady,
    StartMatch,
    Rematch,
    ReturnToLobby,
    RequestLeave,
    CancelLeave,
    LeaveOnline,
    ReturnToMenu,
    Retry,
    DismissError,
    RequestTrusted,
    RequestDedicated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayUnavailableSurface {
    InviteFriends,
    ControllerBindings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverlayUnavailableNotice {
    pub surface: OverlayUnavailableSurface,
    pub failure: OnlineFailure,
    pub dismiss_at_ms: u64,
}

impl OverlayUnavailableNotice {
    const fn new(surface: OverlayUnavailableSurface, now_ms: u64) -> Self {
        Self {
            surface,
            failure: OnlineFailure::overlay_unavailable(),
            dismiss_at_ms: now_ms.saturating_add(OVERLAY_UNAVAILABLE_NOTICE_MS),
        }
    }

    const fn expired(self, now_ms: u64) -> bool {
        now_ms >= self.dismiss_at_ms
    }
}

pub struct NativeOnlineApplication {
    editor: CouchSeatEditor,
    local_peer_id: Option<PeerId>,
    pending_join: Option<LobbyJoinIntent>,
    bindings: [Option<AuthenticatedPeerBinding>; MAX_STEAM_LOBBY_MEMBERS],
    staged_endpoints: VecDeque<NativeOnlineEndpoint>,
    pending_authority_commands: VecDeque<ListenAuthorityCommand>,
    pending_authority_detaches: VecDeque<PendingAuthorityDetach>,
    pending_authority_revocations: VecDeque<PendingAuthorityRevocation>,
    authority_endpoints: [Option<AuthorityEndpointRecord>; ONLINE_MAX_AUTHORITY_ENDPOINT_RECORDS],
    active: Option<ActiveNativeOnlineMatch>,
    listen_shutdown: Option<ListenShutdownAction>,
    awaiting_transport_retirement: bool,
    remote_quality_user: Option<SteamUserId>,
    content_ready: bool,
    coordinator_content_marked: bool,
    initial_sync_marked: bool,
    result_confirmation_started: bool,
    result_confirmed: bool,
    projected_confirmed_result: Option<ProjectedConfirmedTerminal>,
    leave_confirmation_open: bool,
    overlay_notice: Option<OverlayUnavailableNotice>,
    match_nonce: Option<OnlineMatchNonce>,
    match_counter: u64,
    failure_override: Option<OnlineFailure>,
    authority_disconnect: Option<RemoteAuthorityDisconnect>,
    request_online_focus: bool,
    request_leave_user_mode: bool,
    release_render_world: bool,
    controller_menu_held: [SteamMenuInputMask; MAX_STEAM_INPUT_CONTROLLERS],
    controller_menu_screen: Option<NativeOnlineScreen>,
    controller_selected_action: Option<NativeOnlineUiAction>,
    metrics: NativeOnlineApplicationMetrics,
    #[cfg(test)]
    listen_diagnostics_root: Option<std::path::PathBuf>,
}

impl Default for NativeOnlineApplication {
    fn default() -> Self {
        Self {
            editor: CouchSeatEditor::default(),
            local_peer_id: None,
            pending_join: None,
            bindings: [None; MAX_STEAM_LOBBY_MEMBERS],
            staged_endpoints: VecDeque::with_capacity(ONLINE_MAX_STAGED_ENDPOINTS),
            pending_authority_commands: VecDeque::with_capacity(
                ONLINE_MAX_PENDING_AUTHORITY_COMMANDS,
            ),
            pending_authority_detaches: VecDeque::with_capacity(MAX_STEAM_LOBBY_MEMBERS),
            pending_authority_revocations: VecDeque::with_capacity(MAX_STEAM_LOBBY_MEMBERS),
            authority_endpoints: [None; ONLINE_MAX_AUTHORITY_ENDPOINT_RECORDS],
            active: None,
            listen_shutdown: None,
            awaiting_transport_retirement: false,
            remote_quality_user: None,
            content_ready: false,
            coordinator_content_marked: false,
            initial_sync_marked: false,
            result_confirmation_started: false,
            result_confirmed: false,
            projected_confirmed_result: None,
            leave_confirmation_open: false,
            overlay_notice: None,
            match_nonce: None,
            match_counter: 0,
            failure_override: None,
            authority_disconnect: None,
            request_online_focus: false,
            request_leave_user_mode: false,
            release_render_world: false,
            controller_menu_held: [SteamMenuInputMask::NONE; MAX_STEAM_INPUT_CONTROLLERS],
            controller_menu_screen: None,
            controller_selected_action: None,
            metrics: NativeOnlineApplicationMetrics::default(),
            #[cfg(test)]
            listen_diagnostics_root: None,
        }
    }
}

impl Drop for NativeOnlineApplication {
    fn drop(&mut self) {
        // Window/process teardown must synchronously join gameplay workers on
        // the main thread even if no UI command ran first.
        self.clear_active_match();
    }
}

impl NativeOnlineApplication {
    pub const fn metrics(&self) -> NativeOnlineApplicationMetrics {
        self.metrics
    }

    pub fn active_session_kind(&self) -> Option<NativeOnlineSessionKind> {
        self.active.as_ref().map(ActiveNativeOnlineMatch::kind)
    }

    pub fn local_seat_count(&self) -> usize {
        usize::from(self.editor.seat_count)
    }

    pub fn accepts_gameplay_input(&self) -> bool {
        if self.listen_shutdown.is_some() {
            return false;
        }
        self.active.as_ref().is_some_and(|active| {
            matches!(
                active.client().status().phase,
                RemoteOnlineClientPhase::Fighting | RemoteOnlineClientPhase::ConfirmingResult
            )
        })
    }

    pub fn set_content_ready(&mut self, ready: bool) {
        self.content_ready = ready;
        if ready {
            if let Some(active) = &self.active {
                active.client().mark_content_loaded();
            }
        }
    }

    pub const fn overlay_notice(&self) -> Option<OverlayUnavailableNotice> {
        self.overlay_notice
    }

    pub fn observe_overlay_request(
        &mut self,
        surface: OverlayUnavailableSurface,
        status: SteamOverlayRequestStatus,
        now_ms: u64,
    ) {
        match status {
            SteamOverlayRequestStatus::Submitted => self.overlay_notice = None,
            SteamOverlayRequestStatus::Unavailable => {
                self.overlay_notice = Some(OverlayUnavailableNotice::new(surface, now_ms));
            }
        }
    }

    pub fn dismiss_overlay_notice(&mut self) {
        self.overlay_notice = None;
    }

    fn expire_overlay_notice(&mut self, now_ms: u64) {
        if self
            .overlay_notice
            .is_some_and(|notice| notice.expired(now_ms))
        {
            self.overlay_notice = None;
        }
    }

    pub fn dispatch<R: NativeOnlineRuntimePort>(
        &mut self,
        runtime: &mut R,
        action: NativeOnlineUiAction,
        now_ms: u64,
    ) -> Result<(), NativeOnlineApplicationError> {
        let result = self.dispatch_inner(runtime, action, now_ms);
        if let Err(error) = &result {
            self.observe_application_error(error);
        }
        result
    }

    fn dispatch_inner<R: NativeOnlineRuntimePort>(
        &mut self,
        runtime: &mut R,
        action: NativeOnlineUiAction,
        now_ms: u64,
    ) -> Result<(), NativeOnlineApplicationError> {
        match action {
            NativeOnlineUiAction::CreatePrivate => {
                self.create_lobby(runtime, NativeOnlineVisibility::Private, now_ms)
            }
            NativeOnlineUiAction::CreateFriends => {
                self.create_lobby(runtime, NativeOnlineVisibility::FriendsOnly, now_ms)
            }
            NativeOnlineUiAction::AcceptJoin => {
                let intent = self
                    .pending_join
                    .take()
                    .ok_or(NativeOnlineApplicationError::MissingJoinIntent)?;
                let declaration = self.make_declaration(runtime, false)?;
                runtime.execute_port(
                    NativeOnlineCommand::Join {
                        intent,
                        local_declaration: declaration,
                    },
                    now_ms,
                )?;
                self.reset_match_gates();
                Ok(())
            }
            NativeOnlineUiAction::DeclineJoin => {
                runtime.execute_port(NativeOnlineCommand::DeclineJoin, now_ms)?;
                self.pending_join = None;
                Ok(())
            }
            NativeOnlineUiAction::InviteFriends => {
                let status = runtime.open_invite_overlay_port()?;
                self.observe_overlay_request(
                    OverlayUnavailableSurface::InviteFriends,
                    status,
                    now_ms,
                );
                Ok(())
            }
            NativeOnlineUiAction::AddSeat => {
                if runtime.view_model().screen == NativeOnlineScreen::Lobby
                    && runtime.view_model().total_seats >= ONLINE_SEAT_CAPACITY
                {
                    return Err(NativeOnlineApplicationError::InvalidAction);
                }
                self.edit_and_publish(runtime, now_ms, CouchSeatEditor::add_seat)
            }
            NativeOnlineUiAction::RemoveSeat => {
                self.edit_and_publish(runtime, now_ms, CouchSeatEditor::remove_seat)
            }
            NativeOnlineUiAction::PreviousSeat => {
                self.editor.select_previous();
                Ok(())
            }
            NativeOnlineUiAction::NextSeat => {
                self.editor.select_next();
                Ok(())
            }
            NativeOnlineUiAction::PreviousCharacter => {
                self.edit_and_publish(runtime, now_ms, |editor| editor.cycle_character(-1))
            }
            NativeOnlineUiAction::NextCharacter => {
                self.edit_and_publish(runtime, now_ms, |editor| editor.cycle_character(1))
            }
            NativeOnlineUiAction::PreviousStyle => {
                self.edit_and_publish(runtime, now_ms, |editor| editor.cycle_style(-1))
            }
            NativeOnlineUiAction::NextStyle => {
                self.edit_and_publish(runtime, now_ms, |editor| editor.cycle_style(1))
            }
            NativeOnlineUiAction::PreviousEquipment => {
                self.edit_and_publish(runtime, now_ms, |editor| editor.cycle_equipment(-1))
            }
            NativeOnlineUiAction::NextEquipment => {
                self.edit_and_publish(runtime, now_ms, |editor| editor.cycle_equipment(1))
            }
            NativeOnlineUiAction::ToggleTeam => {
                self.edit_and_publish(runtime, now_ms, CouchSeatEditor::toggle_team)
            }
            NativeOnlineUiAction::PreviousArena
            | NativeOnlineUiAction::NextArena
            | NativeOnlineUiAction::PreviousRules
            | NativeOnlineUiAction::NextRules => {
                // Arena/rules are host-authored immutable manifest fields. They
                // may be chosen only before creating a lobby and are never
                // republished as mutable roster data.
                if runtime.view_model().screen != NativeOnlineScreen::OnlineMenu {
                    return Err(NativeOnlineApplicationError::InvalidAction);
                }
                match action {
                    NativeOnlineUiAction::PreviousArena => self.editor.cycle_arena(-1),
                    NativeOnlineUiAction::NextArena => self.editor.cycle_arena(1),
                    NativeOnlineUiAction::PreviousRules => self.editor.cycle_rules(-1),
                    NativeOnlineUiAction::NextRules => self.editor.cycle_rules(1),
                    _ => unreachable!(),
                }
                Ok(())
            }
            NativeOnlineUiAction::ToggleReady => {
                let ready = !runtime.view_model().local_ready;
                runtime.execute_port(NativeOnlineCommand::SetReady(ready), now_ms)?;
                Ok(())
            }
            NativeOnlineUiAction::StartMatch => self.commit_casual_listen(runtime, now_ms),
            NativeOnlineUiAction::Rematch => {
                if self.active_session_kind() == Some(NativeOnlineSessionKind::ListenOwner) {
                    return self.begin_listen_shutdown(
                        runtime,
                        ListenShutdownAction::Rematch,
                        now_ms,
                    );
                }
                runtime.execute_port(NativeOnlineCommand::Rematch, now_ms)?;
                if runtime.view_model().screen != NativeOnlineScreen::Results {
                    self.clear_active_match();
                    self.reset_match_gates();
                }
                Ok(())
            }
            NativeOnlineUiAction::ReturnToLobby => {
                if self.active_session_kind() == Some(NativeOnlineSessionKind::ListenOwner) {
                    return self.begin_listen_shutdown(
                        runtime,
                        ListenShutdownAction::ReturnToLobby,
                        now_ms,
                    );
                }
                runtime.execute_port(NativeOnlineCommand::ReturnToLobby, now_ms)?;
                if runtime.view_model().screen != NativeOnlineScreen::Results {
                    self.clear_active_match();
                    self.reset_match_gates();
                }
                Ok(())
            }
            NativeOnlineUiAction::RequestLeave => {
                if self.active.is_none() {
                    return Err(NativeOnlineApplicationError::InvalidAction);
                }
                self.leave_confirmation_open = true;
                Ok(())
            }
            NativeOnlineUiAction::CancelLeave => {
                self.leave_confirmation_open = false;
                Ok(())
            }
            NativeOnlineUiAction::LeaveOnline => {
                self.leave_confirmation_open = false;
                if self.active_session_kind() == Some(NativeOnlineSessionKind::ListenOwner) {
                    return self.begin_listen_shutdown(
                        runtime,
                        ListenShutdownAction::LeaveOnline,
                        now_ms,
                    );
                }
                runtime.execute_port(NativeOnlineCommand::LeaveOnline, now_ms)?;
                self.clear_online_session();
                self.awaiting_transport_retirement = runtime.transport_retirement_pending_port();
                Ok(())
            }
            NativeOnlineUiAction::ReturnToMenu => {
                if self.active_session_kind() == Some(NativeOnlineSessionKind::ListenOwner) {
                    return self.begin_listen_shutdown(
                        runtime,
                        ListenShutdownAction::ReturnToMenu,
                        now_ms,
                    );
                }
                if !matches!(
                    runtime.view_model().screen,
                    NativeOnlineScreen::OnlineMenu | NativeOnlineScreen::Unavailable
                ) {
                    // A fatal platform/runtime fault can make native leave fail.
                    // Local worker teardown and user-mode recovery are never
                    // conditional on that best-effort external cleanup.
                    let _ = runtime.execute_port(NativeOnlineCommand::LeaveOnline, now_ms);
                }
                self.clear_online_session();
                self.awaiting_transport_retirement = runtime.transport_retirement_pending_port();
                self.request_leave_user_mode = true;
                Ok(())
            }
            NativeOnlineUiAction::Retry => {
                if self.active_session_kind() == Some(NativeOnlineSessionKind::ListenOwner) {
                    return self.begin_listen_shutdown(
                        runtime,
                        ListenShutdownAction::Retry,
                        now_ms,
                    );
                }
                if !matches!(
                    runtime.view_model().screen,
                    NativeOnlineScreen::OnlineMenu | NativeOnlineScreen::Unavailable
                ) {
                    let _ = runtime.execute_port(NativeOnlineCommand::LeaveOnline, now_ms);
                }
                self.clear_online_session();
                self.awaiting_transport_retirement = runtime.transport_retirement_pending_port();
                self.failure_override = None;
                self.request_online_focus = true;
                Ok(())
            }
            NativeOnlineUiAction::DismissError => {
                self.failure_override = None;
                Ok(())
            }
            NativeOnlineUiAction::RequestTrusted => {
                Err(NativeOnlineApplicationError::TrustedResultsDisabled)
            }
            NativeOnlineUiAction::RequestDedicated => {
                Err(NativeOnlineApplicationError::DedicatedModeDisabled)
            }
        }
    }

    fn create_lobby<R: NativeOnlineRuntimePort>(
        &mut self,
        runtime: &mut R,
        visibility: NativeOnlineVisibility,
        now_ms: u64,
    ) -> Result<(), NativeOnlineApplicationError> {
        let declaration = self.make_declaration(runtime, false)?;
        let region = RegionCode::new(ONLINE_DEFAULT_REGION)
            .map_err(|_| NativeOnlineApplicationError::InvalidAction)?;
        runtime.execute_port(
            NativeOnlineCommand::Create(NativeOnlineCreateRequest {
                visibility,
                maximum_steam_peers: ONLINE_MAX_STEAM_PEERS,
                region,
                rules: self.editor.rules,
                arena: self.editor.arena,
                seat_capacity: ONLINE_SEAT_CAPACITY,
                local_declaration: declaration,
            }),
            now_ms,
        )?;
        self.reset_match_gates();
        Ok(())
    }

    fn ensure_local_peer<R: NativeOnlineRuntimePort>(
        &mut self,
        runtime: &R,
    ) -> Result<PeerId, NativeOnlineApplicationError> {
        if let Some(peer_id) = self.local_peer_id {
            return Ok(peer_id);
        }
        let user = runtime
            .local_authenticated_user_port()
            .ok_or(NativeOnlineApplicationError::MissingLocalIdentity)?;
        let nonce = self.match_counter.wrapping_add(1);
        let mixed = mix_online_identity(user.get() ^ nonce.rotate_left(29));
        let peer_id = PeerId::new(mixed.max(1))
            .map_err(|_| NativeOnlineApplicationError::MissingLocalPeer)?;
        self.local_peer_id = Some(peer_id);
        Ok(peer_id)
    }

    fn make_declaration<R: NativeOnlineRuntimePort>(
        &mut self,
        runtime: &R,
        ready: bool,
    ) -> Result<OnlineRosterMember, NativeOnlineApplicationError> {
        self.make_declaration_for_editor(runtime, ready, self.editor)
    }

    fn make_declaration_for_editor<R: NativeOnlineRuntimePort>(
        &mut self,
        runtime: &R,
        ready: bool,
        editor: CouchSeatEditor,
    ) -> Result<OnlineRosterMember, NativeOnlineApplicationError> {
        let peer_id = self.ensure_local_peer(runtime)?;
        runtime
            .make_local_declaration_port(peer_id, editor.revision, ready, editor.seats())
            .map_err(Into::into)
    }

    fn edit_and_publish<R, F>(
        &mut self,
        runtime: &mut R,
        now_ms: u64,
        edit: F,
    ) -> Result<(), NativeOnlineApplicationError>
    where
        R: NativeOnlineRuntimePort,
        F: FnOnce(&mut CouchSeatEditor) -> Result<(), NativeOnlineApplicationError>,
    {
        let mut candidate = self.editor;
        edit(&mut candidate)?;
        if runtime.view_model().screen == NativeOnlineScreen::Lobby {
            let declaration = self.make_declaration_for_editor(runtime, false, candidate)?;
            runtime.execute_port(
                NativeOnlineCommand::SetLocalDeclaration(declaration),
                now_ms,
            )?;
        }
        self.editor = candidate;
        Ok(())
    }

    fn commit_casual_listen<R: NativeOnlineRuntimePort>(
        &mut self,
        runtime: &mut R,
        now_ms: u64,
    ) -> Result<(), NativeOnlineApplicationError> {
        let view = runtime.view_model();
        if view.screen != NativeOnlineScreen::Lobby
            || view.role != Some(OnlineLobbyRole::ListenAuthority)
            || !view.all_members_ready
            || view.input_delay_calibration.state != InputDelayCalibrationState::Ready
        {
            return Err(NativeOnlineApplicationError::InvalidAction);
        }
        let selected_input_delay_ticks = view
            .input_delay_calibration
            .selected_input_delay_ticks
            .ok_or(NativeOnlineApplicationError::InvalidAction)?;
        let authority_peer = self
            .local_peer_id
            .ok_or(NativeOnlineApplicationError::MissingLocalPeer)?;
        let authenticated = runtime
            .local_authenticated_user_port()
            .ok_or(NativeOnlineApplicationError::MissingLocalIdentity)?;
        let match_nonce = self.ensure_match_nonce()?;
        self.match_counter = self
            .match_counter
            .checked_add(1)
            .ok_or(NativeOnlineApplicationError::TimelineExhausted)?;
        let match_id = online_match_id(authenticated, match_nonce, self.match_counter, now_ms)?;
        let start_tick = SimTick(ONLINE_COUNTDOWN_LEAD_TICKS);
        let mut options = OnlineManifestOptions::casual_listen(
            match_id,
            authority_peer,
            self.editor.arena,
            self.editor.rules,
            online_master_gameplay_seed(authenticated, match_nonce, self.match_counter, now_ms),
            start_tick,
        );
        options.input_delay_ticks = selected_input_delay_ticks;
        runtime.execute_port(
            NativeOnlineCommand::CommitManifest {
                options,
                current_tick: SimTick::ZERO,
            },
            now_ms,
        )?;
        Ok(())
    }

    fn ensure_match_nonce(&mut self) -> Result<OnlineMatchNonce, NativeOnlineApplicationError> {
        if let Some(nonce) = self.match_nonce {
            return Ok(nonce);
        }
        let nonce = generate_online_match_nonce_with(getrandom::fill)?;
        self.match_nonce = Some(nonce);
        Ok(nonce)
    }

    #[cfg(test)]
    fn inject_match_nonce_for_test(&mut self, bytes: [u8; 32]) {
        assert_eq!(
            self.match_counter, 0,
            "test nonce must be injected before any match is committed"
        );
        self.match_nonce = Some(OnlineMatchNonce(bytes));
    }

    fn reset_match_gates(&mut self) {
        self.coordinator_content_marked = false;
        self.initial_sync_marked = false;
        self.result_confirmation_started = false;
        self.result_confirmed = false;
        self.projected_confirmed_result = None;
        self.leave_confirmation_open = false;
        self.failure_override = None;
        self.authority_disconnect = None;
    }

    fn begin_listen_shutdown<R: NativeOnlineRuntimePort>(
        &mut self,
        runtime: &mut R,
        action: ListenShutdownAction,
        now_ms: u64,
    ) -> Result<(), NativeOnlineApplicationError> {
        if let Some(existing) = self.listen_shutdown {
            return if existing == action {
                Ok(())
            } else {
                Err(NativeOnlineApplicationError::InvalidAction)
            };
        }
        // Stop new Steam admissions before the authority raises its monotonic
        // gameplay/shutdown fence. Existing exact endpoint generations remain
        // owned and continue draining.
        runtime.execute_port(NativeOnlineCommand::QuiesceAdmission, now_ms)?;
        let outcome = match &self.active {
            Some(ActiveNativeOnlineMatch::Listen(online_match)) => {
                online_match.authority.try_begin_graceful_shutdown()
            }
            _ => return Err(NativeOnlineApplicationError::InvalidAction),
        };

        self.listen_shutdown = Some(action);
        self.leave_confirmation_open = false;
        self.staged_endpoints.clear();
        for record in self.authority_endpoints.iter_mut().flatten() {
            match &mut record.state {
                AuthorityEndpointState::Submitted {
                    terminal_expected, ..
                }
                | AuthorityEndpointState::Attached {
                    terminal_expected, ..
                } => *terminal_expected = true,
                AuthorityEndpointState::TerminalDrained { .. } => {}
            }
        }
        self.metrics.graceful_shutdowns_started =
            self.metrics.graceful_shutdowns_started.saturating_add(1);
        if let Err(error) = self.retain_authority_outcome(outcome) {
            // The worker terminal is consumed on the next pump. Retain the
            // requested transition so that terminal handling can fail closed
            // instead of exposing a frozen gameplay screen.
            return Err(error);
        }
        Ok(())
    }

    fn complete_listen_shutdown<R: NativeOnlineRuntimePort>(
        &mut self,
        runtime: &mut R,
        now_ms: u64,
    ) -> Result<(), NativeOnlineApplicationError> {
        let action = self
            .listen_shutdown
            .take()
            .ok_or(NativeOnlineApplicationError::AuthorityDisconnected)?;
        let command_result = match action {
            ListenShutdownAction::Rematch => {
                runtime.execute_port(NativeOnlineCommand::Rematch, now_ms)
            }
            ListenShutdownAction::ReturnToLobby => {
                runtime.execute_port(NativeOnlineCommand::ReturnToLobby, now_ms)
            }
            ListenShutdownAction::LeaveOnline
            | ListenShutdownAction::ReturnToMenu
            | ListenShutdownAction::Retry
            | ListenShutdownAction::AppExit => {
                runtime.execute_port(NativeOnlineCommand::LeaveOnline, now_ms)
            }
            ListenShutdownAction::CoordinatorDrop => Ok(()),
        };

        match action {
            ListenShutdownAction::Rematch | ListenShutdownAction::ReturnToLobby => {
                self.clear_active_match();
                self.reset_match_gates();
            }
            ListenShutdownAction::LeaveOnline | ListenShutdownAction::AppExit => {
                self.clear_online_session();
            }
            ListenShutdownAction::ReturnToMenu => {
                self.clear_online_session();
                self.request_leave_user_mode = true;
            }
            ListenShutdownAction::Retry => {
                self.clear_online_session();
                self.failure_override = None;
                self.request_online_focus = true;
            }
            ListenShutdownAction::CoordinatorDrop => {
                self.clear_active_match();
            }
        }
        self.metrics.graceful_shutdowns_completed =
            self.metrics.graceful_shutdowns_completed.saturating_add(1);
        self.awaiting_transport_retirement = runtime.transport_retirement_pending_port();
        command_result.map_err(Into::into)
    }

    fn clear_active_match(&mut self) {
        if let Some(active) = self.active.take() {
            active.stop();
            self.metrics.sessions_stopped = self.metrics.sessions_stopped.saturating_add(1);
        }
        self.pending_authority_commands.clear();
        self.pending_authority_detaches.clear();
        self.pending_authority_revocations.clear();
        self.staged_endpoints.clear();
        self.authority_endpoints.fill(None);
        self.listen_shutdown = None;
        self.remote_quality_user = None;
        self.projected_confirmed_result = None;
        self.leave_confirmation_open = false;
        self.release_render_world = true;
    }

    fn clear_online_session(&mut self) {
        self.clear_active_match();
        self.pending_join = None;
        self.bindings = [None; MAX_STEAM_LOBBY_MEMBERS];
        self.local_peer_id = None;
        self.reset_match_gates();
    }

    fn handle_fatal_runtime_pump(&mut self, failure: OnlineFailure) {
        self.clear_online_session();
        self.failure_override = Some(failure);
        self.request_online_focus = true;
    }

    fn observe_application_error(&mut self, error: &NativeOnlineApplicationError) {
        let (code, recovery) = match error {
            NativeOnlineApplicationError::TrustedResultsDisabled => (
                OnlineFailureCode::PublicPlayDisabled,
                OnlineRecoveryAction::Dismiss,
            ),
            NativeOnlineApplicationError::DedicatedModeDisabled => (
                OnlineFailureCode::DedicatedUnavailable,
                OnlineRecoveryAction::Dismiss,
            ),
            NativeOnlineApplicationError::MissingLocalIdentity => (
                OnlineFailureCode::SteamUnavailable,
                OnlineRecoveryAction::DisableOnline,
            ),
            NativeOnlineApplicationError::EndpointCapacity
            | NativeOnlineApplicationError::AuthorityCommandCapacity => (
                OnlineFailureCode::InternalCapacity,
                OnlineRecoveryAction::ReturnToMenu,
            ),
            NativeOnlineApplicationError::RemoteStart
            | NativeOnlineApplicationError::ListenStart
            | NativeOnlineApplicationError::MissingManifest
            | NativeOnlineApplicationError::MissingCommittedRoster
            | NativeOnlineApplicationError::MissingListenHost
            | NativeOnlineApplicationError::Presentation => (
                OnlineFailureCode::SynchronizationFailed,
                OnlineRecoveryAction::ReturnToLobby,
            ),
            _ => (
                OnlineFailureCode::InternalFailure,
                OnlineRecoveryAction::ReturnToMenu,
            ),
        };
        self.failure_override = Some(OnlineFailure {
            code,
            severity: if recovery == OnlineRecoveryAction::Dismiss {
                OnlineFailureSeverity::Notice
            } else {
                OnlineFailureSeverity::Fatal
            },
            recovery,
            detail_code: 0,
        });
    }

    fn observe_terminal_worker(
        &mut self,
        terminal: Option<RemoteOnlineTerminal>,
        screen: NativeOnlineScreen,
    ) {
        let Some(RemoteOnlineTerminal::Failed(failure)) = terminal else {
            return;
        };
        if self.authority_disconnect.is_some() {
            return;
        }
        // A recoverable transport failure retains the client so a fresh
        // authenticated endpoint can replace its stopped worker. A fatal
        // terminal cannot be reused: synchronously join it and release every
        // session-local handoff before exposing the failure to the UI.
        if failure.severity == OnlineFailureSeverity::Fatal {
            self.clear_active_match();
            self.failure_override = Some(failure);
        } else if screen != NativeOnlineScreen::Reconnecting {
            self.failure_override = Some(failure);
        }
    }

    fn apply_authority_disconnect<R: NativeOnlineRuntimePort>(
        &mut self,
        runtime: &mut R,
        disconnect: RemoteAuthorityDisconnect,
        now_ms: u64,
    ) -> Result<(), NativeOnlineApplicationError> {
        let Some(ActiveNativeOnlineMatch::Remote(client)) = self.active.as_ref() else {
            return Err(NativeOnlineApplicationError::InvalidAction);
        };
        let match_id = client.manifest().match_id;
        if runtime.view_model().role != Some(OnlineLobbyRole::Client)
            || disconnect.generation != client.generation()
            || disconnect.message.match_id != Some(match_id)
        {
            return Err(NativeOnlineApplicationError::InvalidAction);
        }
        if self.authority_disconnect.is_some() {
            // The first valid terminal for a worker generation wins. Reliable
            // replay and a conflicting later publication are both idempotent.
            return Ok(());
        }

        runtime.execute_port(
            NativeOnlineCommand::ApplyAuthorityDisconnect(disconnect),
            now_ms,
        )?;
        self.authority_disconnect = Some(disconnect);
        self.failure_override = Some(OnlineFailure::from_disconnect(disconnect.message));
        if disconnect.message.retry != RetryDisposition::ReconnectAllowed {
            self.clear_active_match();
        }
        Ok(())
    }
}

/// Exclusive application-frame driver. Both non-send owners are temporarily
/// removed so snapshot projection can mutate the render world without aliasing
/// either worker handle.
pub fn drive_native_online_application(world: &mut World) {
    let now_ms = world
        .get_resource::<Time<Real>>()
        .map(|time| time.elapsed().as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0);
    let content_ready = world
        .get_resource::<UserModeGameplayScene>()
        .is_none_or(UserModeGameplayScene::ready_for_battle);
    let was_online_visible = world
        .get_resource::<UserModeState>()
        .is_some_and(UserModeState::online_active);
    let offline_match_is_fighting = world
        .get_resource::<MatchState>()
        .is_some_and(MatchState::is_fighting);

    let Some(mut runtime) = world.remove_non_send_resource::<NativeOnlineRuntime>() else {
        return;
    };
    let Some(mut application) = world.remove_non_send_resource::<NativeOnlineApplication>() else {
        world.insert_non_send_resource(runtime);
        return;
    };
    application.expire_overlay_notice(now_ms);

    let fatal_pump = runtime.pump_frame(now_ms).is_err();
    if fatal_pump {
        let failure = runtime.view_model().failure.unwrap_or(OnlineFailure {
            code: OnlineFailureCode::InternalFailure,
            severity: OnlineFailureSeverity::Fatal,
            recovery: OnlineRecoveryAction::ReturnToMenu,
            detail_code: 0,
        });
        application.handle_fatal_runtime_pump(failure);
    } else {
        application.set_content_ready(content_ready);
        if let Err(error) = application.pump(&mut runtime, now_ms) {
            application.observe_application_error(&error);
        }
        let steam_input_action_set = if application.accepts_gameplay_input()
            && !application.leave_confirmation_open
            || (application.active.is_none() && offline_match_is_fighting)
        {
            SteamInputActionSet::Gameplay
        } else {
            SteamInputActionSet::Menu
        };
        if runtime
            .set_steam_input_action_set(steam_input_action_set)
            .is_err()
        {
            application.observe_application_error(&NativeOnlineApplicationError::Runtime);
        }

        if application.active.is_some() {
            world.insert_resource(SimulationDriveMode::ExternalProjection);
            if let Err(error) = application.project_latest(world) {
                application.observe_application_error(&error);
            }
        }
    }

    let focus_online = application.request_online_focus
        || runtime.view_model().screen == NativeOnlineScreen::JoinPrompt;
    let leave_online = application.request_leave_user_mode;
    let release_render_world = application.release_render_world;
    application.request_online_focus = false;
    application.request_leave_user_mode = false;
    application.release_render_world = false;

    let mut snapshot = application.ui_snapshot(
        &runtime,
        was_online_visible || focus_online || application.active.is_some(),
    );
    snapshot.confirmed_match = world.get_resource::<ConfirmedMatchPresentation>().cloned();
    let preserve_confirmed_presentation = snapshot.screen == NativeOnlineScreen::Results
        && snapshot.outcome == Some(OnlineMatchOutcome::Confirmed);
    world.insert_non_send_resource(runtime);
    world.insert_non_send_resource(application);
    world.insert_resource(snapshot);

    if let Some(mut user_mode) = world.get_resource_mut::<UserModeState>() {
        if focus_online {
            user_mode.enter_online();
        }
        if leave_online {
            user_mode.leave_online();
        }
    }
    if release_render_world {
        release_native_online_projection_target(world, preserve_confirmed_presentation);
        if let Some(mut inputs) = world.get_resource_mut::<LocalTickInputState>() {
            inputs.reset_all_sessions();
        }
        if let Some(mut match_state) = world.get_resource_mut::<crate::game_state::MatchState>() {
            match_state.return_to_setup();
        }
    }
}

/// Releases match-scoped render state while retaining an already-confirmed
/// result throughout the Results phase. The next lobby epoch calls this with
/// `preserve_confirmed` false and removes the retained presentation before a
/// new match can project into the world.
fn release_native_online_projection_target(world: &mut World, preserve_confirmed: bool) {
    let retained = preserve_confirmed
        .then(|| world.get_resource::<ConfirmedMatchPresentation>().cloned())
        .flatten();
    crate::presentation_projection::release_projection_target(world);
    if let Some(presentation) = retained {
        world.insert_resource(presentation);
    }
}

pub fn handle_native_online_ui_input(
    time: Res<Time<Real>>,
    keys: Res<ButtonInput<KeyCode>>,
    user_mode: Res<UserModeState>,
    snapshot: Res<NativeOnlineUiSnapshot>,
    interactions: Query<(&Interaction, &NativeOnlineUiAction), Changed<Interaction>>,
    mut runtime: NonSendMut<NativeOnlineRuntime>,
    mut application: NonSendMut<NativeOnlineApplication>,
) {
    let controller_intent =
        application.controller_menu_intent(&snapshot, runtime.steam_input_snapshot());
    // Keep the online latch warm while its UI is hidden so the accept button
    // that selected Online cannot fire again on entry. The user-mode shell
    // owns binding-panel requests until the online panel is actually visible.
    if !user_mode.online_active() {
        return;
    }
    if let ControllerMenuIntent::OpenBindings(local_ordinal) = controller_intent {
        if let Ok(status) = runtime.show_steam_input_binding_panel(local_ordinal) {
            application.observe_overlay_request(
                OverlayUnavailableSurface::ControllerBindings,
                status,
                time.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            );
        }
    }
    let pointer_action = interactions.iter().find_map(|(interaction, action)| {
        (*interaction == Interaction::Pressed).then_some(*action)
    });
    let action = pointer_action
        .or_else(|| native_online_keyboard_action(&keys, &snapshot))
        .or_else(|| match controller_intent {
            ControllerMenuIntent::Dispatch(action) => Some(action),
            ControllerMenuIntent::None | ControllerMenuIntent::OpenBindings(_) => None,
        });
    let Some(action) = action else {
        return;
    };
    let now_ms = time.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    let _ = application.dispatch(&mut *runtime, action, now_ms);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControllerMenuIntent {
    None,
    Dispatch(NativeOnlineUiAction),
    OpenBindings(usize),
}

const CONTROLLER_MENU_ACTION_ORDER: [NativeOnlineUiAction; 30] = [
    NativeOnlineUiAction::CreatePrivate,
    NativeOnlineUiAction::CreateFriends,
    NativeOnlineUiAction::AcceptJoin,
    NativeOnlineUiAction::DeclineJoin,
    NativeOnlineUiAction::ToggleReady,
    NativeOnlineUiAction::StartMatch,
    NativeOnlineUiAction::InviteFriends,
    NativeOnlineUiAction::AddSeat,
    NativeOnlineUiAction::RemoveSeat,
    NativeOnlineUiAction::PreviousSeat,
    NativeOnlineUiAction::NextSeat,
    NativeOnlineUiAction::PreviousCharacter,
    NativeOnlineUiAction::NextCharacter,
    NativeOnlineUiAction::PreviousStyle,
    NativeOnlineUiAction::NextStyle,
    NativeOnlineUiAction::PreviousEquipment,
    NativeOnlineUiAction::NextEquipment,
    NativeOnlineUiAction::ToggleTeam,
    NativeOnlineUiAction::PreviousArena,
    NativeOnlineUiAction::NextArena,
    NativeOnlineUiAction::PreviousRules,
    NativeOnlineUiAction::NextRules,
    NativeOnlineUiAction::Rematch,
    NativeOnlineUiAction::ReturnToLobby,
    NativeOnlineUiAction::RequestLeave,
    NativeOnlineUiAction::CancelLeave,
    NativeOnlineUiAction::LeaveOnline,
    NativeOnlineUiAction::ReturnToMenu,
    NativeOnlineUiAction::Retry,
    NativeOnlineUiAction::DismissError,
];

impl NativeOnlineApplication {
    fn controller_menu_intent(
        &mut self,
        snapshot: &NativeOnlineUiSnapshot,
        steam_input: SteamInputSnapshot,
    ) -> ControllerMenuIntent {
        let screen_changed = self.controller_menu_screen != Some(snapshot.screen);
        if screen_changed {
            self.controller_menu_screen = Some(snapshot.screen);
            self.controller_selected_action = first_available_controller_action(snapshot);
        }
        if self
            .controller_selected_action
            .is_none_or(|action| !native_online_action_available(snapshot, action))
        {
            self.controller_selected_action = first_available_controller_action(snapshot);
        }

        let mut pressed = SteamMenuInputMask::NONE;
        let mut binding_ordinal = None;
        for local_ordinal in 0..MAX_STEAM_INPUT_CONTROLLERS {
            let held = steam_input.controllers[local_ordinal].menu_held;
            if screen_changed {
                self.controller_menu_held[local_ordinal] = held;
                continue;
            }
            let just_pressed = held.without(self.controller_menu_held[local_ordinal]);
            self.controller_menu_held[local_ordinal] = held;
            pressed = pressed.union(just_pressed);
            if binding_ordinal.is_none()
                && just_pressed.contains(SteamMenuAction::OpenBindings)
                && steam_input.controllers[local_ordinal].connected()
            {
                binding_ordinal = Some(local_ordinal);
            }
        }
        if screen_changed {
            return ControllerMenuIntent::None;
        }
        if let Some(local_ordinal) = binding_ordinal {
            return ControllerMenuIntent::OpenBindings(local_ordinal);
        }
        if pressed.contains(SteamMenuAction::Back) {
            return ControllerMenuIntent::Dispatch(native_online_back_action(snapshot));
        }

        let previous =
            pressed.contains(SteamMenuAction::Up) || pressed.contains(SteamMenuAction::Left);
        let next =
            pressed.contains(SteamMenuAction::Down) || pressed.contains(SteamMenuAction::Right);
        if previous ^ next {
            self.controller_selected_action = cycle_available_controller_action(
                snapshot,
                self.controller_selected_action,
                if previous { -1 } else { 1 },
            );
        }
        if pressed.contains(SteamMenuAction::Accept) {
            if let Some(action) = self.controller_selected_action {
                return ControllerMenuIntent::Dispatch(action);
            }
        }
        ControllerMenuIntent::None
    }
}

fn native_online_back_action(snapshot: &NativeOnlineUiSnapshot) -> NativeOnlineUiAction {
    if snapshot.leave_confirmation_open {
        return NativeOnlineUiAction::CancelLeave;
    }
    match snapshot.screen {
        NativeOnlineScreen::JoinPrompt => NativeOnlineUiAction::DeclineJoin,
        NativeOnlineScreen::Results => NativeOnlineUiAction::ReturnToLobby,
        NativeOnlineScreen::Error
            if snapshot
                .failure
                .is_some_and(|failure| failure.recovery == OnlineRecoveryAction::Dismiss) =>
        {
            NativeOnlineUiAction::DismissError
        }
        NativeOnlineScreen::Error
            if snapshot.failure.is_some_and(|failure| {
                matches!(
                    failure.recovery,
                    OnlineRecoveryAction::ReturnToLobby | OnlineRecoveryAction::MatchEndedNoContest
                )
            }) =>
        {
            NativeOnlineUiAction::ReturnToLobby
        }
        NativeOnlineScreen::Error
            if snapshot.failure.is_some_and(|failure| {
                matches!(
                    failure.recovery,
                    OnlineRecoveryAction::ReturnToMenu | OnlineRecoveryAction::DisableOnline
                )
            }) =>
        {
            NativeOnlineUiAction::ReturnToMenu
        }
        NativeOnlineScreen::Error
            if snapshot
                .failure
                .is_some_and(|failure| failure.recovery == OnlineRecoveryAction::Retry) =>
        {
            NativeOnlineUiAction::Retry
        }
        NativeOnlineScreen::OnlineMenu | NativeOnlineScreen::Unavailable => {
            NativeOnlineUiAction::ReturnToMenu
        }
        _ => NativeOnlineUiAction::RequestLeave,
    }
}

fn first_available_controller_action(
    snapshot: &NativeOnlineUiSnapshot,
) -> Option<NativeOnlineUiAction> {
    CONTROLLER_MENU_ACTION_ORDER
        .into_iter()
        .find(|action| native_online_action_available(snapshot, *action))
}

fn cycle_available_controller_action(
    snapshot: &NativeOnlineUiSnapshot,
    selected: Option<NativeOnlineUiAction>,
    delta: i8,
) -> Option<NativeOnlineUiAction> {
    let available_count = CONTROLLER_MENU_ACTION_ORDER
        .iter()
        .filter(|action| native_online_action_available(snapshot, **action))
        .count();
    if available_count == 0 {
        return None;
    }
    let current_available_index = CONTROLLER_MENU_ACTION_ORDER
        .iter()
        .filter(|action| native_online_action_available(snapshot, **action))
        .position(|action| Some(*action) == selected)
        .unwrap_or(0);
    let next_available_index = (current_available_index as isize + isize::from(delta))
        .rem_euclid(available_count as isize) as usize;
    CONTROLLER_MENU_ACTION_ORDER
        .iter()
        .copied()
        .filter(|action| native_online_action_available(snapshot, *action))
        .nth(next_available_index)
}

fn native_online_keyboard_action(
    keys: &ButtonInput<KeyCode>,
    snapshot: &NativeOnlineUiSnapshot,
) -> Option<NativeOnlineUiAction> {
    if keys.just_pressed(KeyCode::Escape) {
        return Some(native_online_back_action(snapshot));
    }
    match snapshot.screen {
        NativeOnlineScreen::OnlineMenu => {
            if keys.just_pressed(KeyCode::Digit1) {
                Some(NativeOnlineUiAction::CreatePrivate)
            } else if keys.just_pressed(KeyCode::Digit2) {
                Some(NativeOnlineUiAction::CreateFriends)
            } else {
                native_online_editor_keyboard_action(keys)
            }
        }
        NativeOnlineScreen::JoinPrompt => {
            if keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::KeyY) {
                Some(NativeOnlineUiAction::AcceptJoin)
            } else if keys.just_pressed(KeyCode::KeyN) {
                Some(NativeOnlineUiAction::DeclineJoin)
            } else {
                native_online_editor_keyboard_action(keys)
            }
        }
        NativeOnlineScreen::Lobby => {
            if keys.just_pressed(KeyCode::KeyR) {
                Some(NativeOnlineUiAction::ToggleReady)
            } else if keys.just_pressed(KeyCode::KeyI) {
                Some(NativeOnlineUiAction::InviteFriends)
            } else if keys.just_pressed(KeyCode::Enter)
                && native_online_action_available(snapshot, NativeOnlineUiAction::StartMatch)
            {
                Some(NativeOnlineUiAction::StartMatch)
            } else {
                native_online_editor_keyboard_action(keys)
            }
        }
        NativeOnlineScreen::Results if keys.just_pressed(KeyCode::Enter) => {
            Some(NativeOnlineUiAction::Rematch)
        }
        NativeOnlineScreen::Error
            if keys.just_pressed(KeyCode::Enter)
                && native_online_action_available(snapshot, NativeOnlineUiAction::Retry) =>
        {
            Some(NativeOnlineUiAction::Retry)
        }
        _ => None,
    }
}

fn native_online_editor_keyboard_action(
    keys: &ButtonInput<KeyCode>,
) -> Option<NativeOnlineUiAction> {
    if keys.just_pressed(KeyCode::Insert) {
        Some(NativeOnlineUiAction::AddSeat)
    } else if keys.just_pressed(KeyCode::Delete) {
        Some(NativeOnlineUiAction::RemoveSeat)
    } else if keys.just_pressed(KeyCode::PageUp) {
        Some(NativeOnlineUiAction::PreviousSeat)
    } else if keys.just_pressed(KeyCode::PageDown) {
        Some(NativeOnlineUiAction::NextSeat)
    } else if keys.just_pressed(KeyCode::KeyQ) {
        Some(NativeOnlineUiAction::PreviousCharacter)
    } else if keys.just_pressed(KeyCode::KeyE) {
        Some(NativeOnlineUiAction::NextCharacter)
    } else if keys.just_pressed(KeyCode::KeyZ) {
        Some(NativeOnlineUiAction::PreviousStyle)
    } else if keys.just_pressed(KeyCode::KeyX) {
        Some(NativeOnlineUiAction::NextStyle)
    } else if keys.just_pressed(KeyCode::KeyC) {
        Some(NativeOnlineUiAction::PreviousEquipment)
    } else if keys.just_pressed(KeyCode::KeyV) {
        Some(NativeOnlineUiAction::NextEquipment)
    } else if keys.just_pressed(KeyCode::KeyT) {
        Some(NativeOnlineUiAction::ToggleTeam)
    } else if keys.just_pressed(KeyCode::PageUp) {
        Some(NativeOnlineUiAction::PreviousArena)
    } else if keys.just_pressed(KeyCode::PageDown) {
        Some(NativeOnlineUiAction::NextArena)
    } else if keys.just_pressed(KeyCode::Home) {
        Some(NativeOnlineUiAction::PreviousRules)
    } else if keys.just_pressed(KeyCode::End) {
        Some(NativeOnlineUiAction::NextRules)
    } else {
        None
    }
}

/// Render-rate sampler for local couch ordinals. Global protocol seat mapping
/// is performed later by `RemoteOnlineClient::sample_local_inputs`.
pub fn sample_native_online_render_input(
    keys: Res<ButtonInput<KeyCode>>,
    camera: Res<GameplayCameraControl>,
    bindings: Res<PlayerKeyBindings>,
    match_state: Option<Res<MatchState>>,
    runtime: Option<NonSend<NativeOnlineRuntime>>,
    application: NonSend<NativeOnlineApplication>,
    mut inputs: ResMut<LocalTickInputState>,
) {
    let overlay_active = runtime
        .as_ref()
        .is_some_and(|runtime| runtime.is_overlay_active());
    let steam_input = runtime
        .as_ref()
        .map_or_else(SteamInputSnapshot::default, |runtime| {
            runtime.steam_input_snapshot()
        });
    // The offline sampler owns this accumulator whenever no native match is
    // active. Steam Input uses its separate source channel so it can augment
    // (not replace) the keyboard samples already merged by that sampler.
    if application.active.is_none() {
        if match_state.is_none_or(|state| !state.is_fighting()) {
            return;
        }
        for local_index in 0..MAX_LOCAL_SEATS as usize {
            let Some(seat) = LocalSeatId::new(local_index) else {
                continue;
            };
            let (movement, held) = steam_input
                .controller(local_index)
                .map(|controller| sample_native_steam_controller(controller, camera.yaw))
                .unwrap_or((QuantizedMovement::ZERO, InputMask::NONE));
            inputs.merge_steam_controller_state(seat, movement, held);
        }
        return;
    }
    if !application.accepts_gameplay_input() {
        inputs.reset_all_input();
        return;
    }
    if neutralize_online_input(
        application.leave_confirmation_open,
        overlay_active,
        &mut inputs,
    ) {
        // Keep the fixed-rate sender alive, but drain only neutral frames while
        // a local modal or the Steam Overlay owns the controls.
        return;
    }
    for local_index in 0..application.local_seat_count() {
        let Some(seat) = LocalSeatId::new(local_index) else {
            continue;
        };
        let Some(binding) = bindings.bindings_for_player(local_index) else {
            inputs.reset_seat_input(seat);
            continue;
        };
        inputs.merge_render_sample(
            seat,
            sample_native_online_keyboard(&keys, camera.yaw, binding),
        );
        let (movement, held) = steam_input
            .controller(local_index)
            .map(|controller| sample_native_steam_controller(controller, camera.yaw))
            .unwrap_or((QuantizedMovement::ZERO, InputMask::NONE));
        inputs.merge_steam_controller_state(seat, movement, held);
    }
    for local_index in application.local_seat_count()..MAX_LOCAL_SEATS as usize {
        if let Some(seat) = LocalSeatId::new(local_index) {
            inputs.reset_seat_input(seat);
        }
    }
}

fn neutralize_online_input(
    leave_confirmation_open: bool,
    overlay_active: bool,
    inputs: &mut LocalTickInputState,
) -> bool {
    let neutral = leave_confirmation_open || overlay_active;
    if neutral {
        inputs.reset_all_input();
    }
    neutral
}

fn sample_native_steam_controller(
    controller: SteamInputControllerSnapshot,
    camera_yaw: f32,
) -> (QuantizedMovement, InputMask) {
    let [analog_x, analog_y] = controller.movement.to_unit_axes();
    let mut raw = Vec2::new(analog_x, analog_y);
    if raw.length_squared() <= f32::EPSILON {
        if controller.gameplay_held.contains(InputMask::LEFT) {
            raw.x -= 1.0;
        }
        if controller.gameplay_held.contains(InputMask::RIGHT) {
            raw.x += 1.0;
        }
        if controller.gameplay_held.contains(InputMask::UP) {
            raw.y -= 1.0;
        }
        if controller.gameplay_held.contains(InputMask::DOWN) {
            raw.y += 1.0;
        }
    }
    let movement =
        camera_relative_direction(raw.normalize_or_zero(), camera_yaw).normalize_or_zero();
    (
        QuantizedMovement::from_unit_axes(movement.x, movement.y),
        controller.gameplay_held,
    )
}

/// Run condition for the legacy/offline render sampler. Register it on
/// `fighter::sample_local_player_input`; while an online worker is active this
/// module is the sole owner of local ordinal input accumulation. The application
/// driver refreshes this send-safe projection earlier in the same chained
/// `PreUpdate`; run conditions must not borrow the thread-bound worker owner
/// because Bevy may evaluate conditions on a compute worker.
pub fn offline_local_input_enabled(snapshot: Res<NativeOnlineUiSnapshot>) -> bool {
    snapshot.session_kind.is_none()
}

fn sample_native_online_keyboard(
    keys: &ButtonInput<KeyCode>,
    camera_yaw: f32,
    bindings: PlayerControlBindings,
) -> RenderInputSample {
    let mut held = InputMask::NONE;
    let mut pressed = InputMask::NONE;
    let mut released = InputMask::NONE;
    for (button, key) in [
        (RawInputButton::Left, bindings.left),
        (RawInputButton::Right, bindings.right),
        (RawInputButton::Up, bindings.up),
        (RawInputButton::Down, bindings.down),
        (RawInputButton::AimGrab, bindings.aim_grab),
        (RawInputButton::Heavy, bindings.heavy),
        (RawInputButton::Light, bindings.light),
        (RawInputButton::Jump, bindings.jump),
    ] {
        let mask = button.mask();
        if keys.pressed(key) {
            held.insert(mask);
        }
        if keys.just_pressed(key) {
            pressed.insert(mask);
        }
        if keys.just_released(key) {
            released.insert(mask);
        }
    }
    let mut raw = Vec2::ZERO;
    if keys.pressed(bindings.left) {
        raw.x -= 1.0;
    }
    if keys.pressed(bindings.right) {
        raw.x += 1.0;
    }
    if keys.pressed(bindings.down) {
        raw.y += 1.0;
    }
    if keys.pressed(bindings.up) {
        raw.y -= 1.0;
    }
    let movement =
        camera_relative_direction(raw.normalize_or_zero(), camera_yaw).normalize_or_zero();
    RenderInputSample {
        movement: QuantizedMovement::from_unit_axes(movement.x, movement.y),
        held,
        pressed,
        released,
    }
}

/// Fixed-rate submission. This never advances canonical state on the render
/// thread; it only queues local samples to the 60 Hz remote-client worker.
pub fn submit_native_online_inputs(
    mut application: NonSendMut<NativeOnlineApplication>,
    mut inputs: ResMut<LocalTickInputState>,
) {
    if let Err(error) = application.submit_local_inputs(&mut inputs) {
        application.observe_application_error(&error);
    }
}

/// Main-thread process teardown after Bevy translates a window close into
/// [`AppExit`]. Workers are joined before the Steam/runtime owner is asked to
/// leave, so no gameplay endpoint can outlive its platform session.
pub fn teardown_native_online_on_exit(
    mut exits: MessageReader<AppExit>,
    time: Option<Res<Time<Real>>>,
    mut runtime: NonSendMut<NativeOnlineRuntime>,
    mut application: NonSendMut<NativeOnlineApplication>,
) {
    if exits.read().next().is_none() {
        return;
    }
    let started_at_ms = time
        .as_ref()
        .map(|time| time.elapsed().as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0);
    let wall_started = std::time::Instant::now();
    let mut emergency = false;

    if application.active_session_kind() == Some(NativeOnlineSessionKind::ListenOwner) {
        emergency = application
            .begin_listen_shutdown(&mut *runtime, ListenShutdownAction::AppExit, started_at_ms)
            .is_err();
    } else {
        let _ = runtime.execute(NativeOnlineCommand::LeaveOnline, started_at_ms);
        application.clear_online_session();
        application.awaiting_transport_retirement = runtime.transport_retirement_pending();
    }

    while !emergency
        && (application.active.is_some()
            || application.listen_shutdown.is_some()
            || application.awaiting_transport_retirement
            || runtime.transport_retirement_pending())
    {
        let elapsed_ms = wall_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        if elapsed_ms >= NATIVE_ONLINE_APP_EXIT_GRACE_MS {
            emergency = true;
            break;
        }
        let now_ms = started_at_ms.saturating_add(elapsed_ms);
        if runtime.pump_frame(now_ms).is_err() || application.pump(&mut *runtime, now_ms).is_err() {
            emergency = true;
            break;
        }
        std::thread::yield_now();
    }

    if emergency {
        application.metrics.graceful_shutdown_emergency_fallbacks = application
            .metrics
            .graceful_shutdown_emergency_fallbacks
            .saturating_add(1);
        // The outer process deadline is the only normal path allowed to use
        // the authority worker's emergency stop/join fallback.
        application.clear_online_session();
        let now_ms = started_at_ms
            .saturating_add(wall_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64);
        let _ = runtime.execute(NativeOnlineCommand::LeaveOnline, now_ms);
        application.awaiting_transport_retirement = runtime.transport_retirement_pending();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use crate::online_roster::OnlineRoster;
    use crate::steam_platform::{AdmissionPurpose, AuthenticatedSteamPeer, SteamLobbyId};
    use crate::steam_transport::{
        FakeSteamTransportNetwork, SteamP2pSession, SteamTransport, SteamTransportConfig,
        SteamTransportRole,
    };

    #[test]
    fn process_nonce_separates_restart_match_ids_and_master_seeds() {
        let user = AuthenticatedUserId::new(44_001).unwrap();
        let nonce_a = OnlineMatchNonce([0x1a; 32]);
        let nonce_b = OnlineMatchNonce([0xb7; 32]);

        let id_a = online_match_id(user, nonce_a, 1, 55_000).unwrap();
        let id_b = online_match_id(user, nonce_b, 1, 55_000).unwrap();
        assert_ne!(id_a, id_b);
        assert_ne!(
            online_master_gameplay_seed(user, nonce_a, 1, 55_000),
            online_master_gameplay_seed(user, nonce_b, 1, 55_000)
        );
    }

    #[test]
    fn injected_match_nonce_is_deterministic() {
        let bytes = core::array::from_fn(|index| index as u8);
        let mut first = NativeOnlineApplication::default();
        let mut second = NativeOnlineApplication::default();
        first.inject_match_nonce_for_test(bytes);
        second.inject_match_nonce_for_test(bytes);

        let first_nonce = first.ensure_match_nonce().unwrap();
        let second_nonce = second.ensure_match_nonce().unwrap();
        let user = AuthenticatedUserId::new(44_002).unwrap();
        assert_eq!(
            online_match_id(user, first_nonce, 9, 77_000).unwrap(),
            online_match_id(user, second_nonce, 9, 77_000).unwrap()
        );
        assert_eq!(
            online_master_gameplay_seed(user, first_nonce, 9, 77_000),
            online_master_gameplay_seed(user, second_nonce, 9, 77_000)
        );
    }

    #[test]
    fn reserved_zero_match_id_is_impossible_for_degenerate_inputs() {
        let user = AuthenticatedUserId::new(1).unwrap();
        let match_id = online_match_id(user, OnlineMatchNonce([0; 32]), 0, 0).unwrap();
        assert_ne!(*match_id.as_bytes(), [0; 16]);
    }

    #[test]
    fn entropy_source_failure_is_explicit() {
        let failure = match generate_online_match_nonce_with(|_| Err::<(), ()>(())) {
            Ok(_) => panic!("failed entropy provider must not produce a nonce"),
            Err(failure) => failure,
        };
        assert_eq!(failure, NativeOnlineApplicationError::EntropyUnavailable);
    }

    #[test]
    fn confirmed_result_presentation_survives_release_until_the_next_epoch() {
        let expected = ConfirmedMatchPresentation {
            key: crate::confirmed_progression::ConfirmedResultKey {
                match_id: MatchId::new(*b"result-retain-01").unwrap(),
                result_id: 7,
            },
            final_tick: SimTick(90),
            outcome: PresentedMatchOutcome::Draw,
            local_outcome: PresentedLocalOutcome::Draw,
            fighters: [None; crate::determinism::FIGHTER_CAPACITY as usize],
        };
        let mut world = World::new();
        world.insert_resource(expected.clone());

        release_native_online_projection_target(&mut world, true);
        assert_eq!(
            world.get_resource::<ConfirmedMatchPresentation>(),
            Some(&expected)
        );
        // Results can render for an arbitrary number of post-match frames.
        release_native_online_projection_target(&mut world, true);
        assert_eq!(
            world.get_resource::<ConfirmedMatchPresentation>(),
            Some(&expected)
        );

        // Entering the next owner-authored lobby epoch clears the old result.
        release_native_online_projection_target(&mut world, false);
        assert!(world.get_resource::<ConfirmedMatchPresentation>().is_none());
    }

    #[test]
    fn prior_match_projected_result_cannot_unlock_new_session_results() {
        let result = crate::session::ConfirmedSessionResult {
            result_id: 7,
            final_tick: SimTick(90),
            final_hash: crate::network_protocol::StateHash(0xAFC),
        };
        let old_match = MatchId::new([1; 16]).unwrap();
        let new_match = MatchId::new([2; 16]).unwrap();
        let projected = Some(ProjectedConfirmedTerminal {
            match_id: old_match,
            result,
        });
        let terminal = Some(RemoteOnlineTerminal::Completed(result));
        assert!(terminal_is_projected_for_match(
            terminal, old_match, projected
        ));
        assert!(!terminal_is_projected_for_match(
            terminal, new_match, projected
        ));
    }

    #[derive(Resource, Default)]
    struct OfflineSamplerRunCount(u8);

    fn count_offline_sampler_runs(mut count: ResMut<OfflineSamplerRunCount>) {
        count.0 += 1;
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum RecordedCommand {
        Create {
            visibility: NativeOnlineVisibility,
            peer_id: PeerId,
            user_id: AuthenticatedUserId,
            seats: u8,
        },
        Join {
            intent: LobbyJoinIntent,
            peer_id: PeerId,
            seats: u8,
        },
        SetDeclaration {
            revision: u16,
            ready: bool,
            seats: u8,
        },
        SetReady(bool),
        Commit(OnlineManifestOptions),
        AuthorityTerminalDrained {
            user: SteamUserId,
            peer_id: PeerId,
            connection: SteamConnectionId,
            retry: Option<RetryDisposition>,
        },
        Invite,
        Decline,
        Leave,
        Other,
    }

    struct FakeRuntime {
        view: NativeOnlineViewModel,
        local_user: AuthenticatedUserId,
        events: VecDeque<OnlineLobbyEvent>,
        endpoints: VecDeque<NativeOnlineEndpoint>,
        commands: Vec<RecordedCommand>,
        config: Option<HeadlessMatchConfig>,
        committed: Option<CommittedAuthenticatedRoster>,
        committed_peers: Option<ArrayVec<AuthenticatedPeer, MAX_STEAM_LOBBY_MEMBERS>>,
        local_declaration: Option<OnlineRosterMember>,
        committed_options: Option<OnlineManifestOptions>,
        lifecycle: LifecycleCommandCounts,
        overlay_status: SteamOverlayRequestStatus,
        overlay_request_count: u32,
        resume_after_sync: bool,
        reject_next_declaration: bool,
        reject_leave: bool,
        transport_retirement_pending: bool,
    }

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    struct LifecycleCommandCounts {
        manifests_accepted: u64,
        content_loaded: u64,
        initial_sync_completed: u64,
        countdowns_begun: u64,
        fighting_marked: u64,
        result_confirmations_begun: u64,
        results_confirmed: u64,
        rematches: u64,
        returns_to_lobby: u64,
        leaves: u64,
    }

    impl FakeRuntime {
        fn available() -> Self {
            Self::available_as(AuthenticatedUserId::new(7_654_321).unwrap())
        }

        fn available_as(local_user: AuthenticatedUserId) -> Self {
            Self {
                view: view_for(NativeOnlineScreen::OnlineMenu),
                local_user,
                events: VecDeque::new(),
                endpoints: VecDeque::new(),
                commands: Vec::new(),
                config: None,
                committed: None,
                committed_peers: None,
                local_declaration: None,
                committed_options: None,
                lifecycle: LifecycleCommandCounts::default(),
                overlay_status: SteamOverlayRequestStatus::Submitted,
                overlay_request_count: 0,
                resume_after_sync: false,
                reject_next_declaration: false,
                reject_leave: false,
                transport_retirement_pending: false,
            }
        }

        fn set_screen(&mut self, screen: NativeOnlineScreen) {
            self.view.screen = screen;
            self.view.actions = view_for(screen).actions;
        }

        fn begin_reconnect(&mut self) {
            self.resume_after_sync = true;
            self.set_screen(NativeOnlineScreen::Reconnecting);
        }
    }

    impl NativeOnlineRuntimePort for FakeRuntime {
        fn view_model(&self) -> NativeOnlineViewModel {
            self.view
        }

        fn execute_port(
            &mut self,
            command: NativeOnlineCommand,
            _now_ms: u64,
        ) -> Result<(), NativeOnlineRuntimeError> {
            if self.reject_next_declaration
                && matches!(&command, NativeOnlineCommand::SetLocalDeclaration(_))
            {
                self.reject_next_declaration = false;
                return Err(NativeOnlineRuntimeError::Capacity);
            }
            if self.reject_leave && matches!(&command, NativeOnlineCommand::LeaveOnline) {
                self.lifecycle.leaves += 1;
                return Err(NativeOnlineRuntimeError::Capacity);
            }
            let recorded = match command {
                NativeOnlineCommand::Create(request) => {
                    self.local_declaration = Some(request.local_declaration);
                    self.set_screen(NativeOnlineScreen::Lobby);
                    self.view.role = Some(OnlineLobbyRole::ListenAuthority);
                    self.view.lobby = Some(SteamLobbyId::new(60_001).unwrap());
                    self.view.lobby_members = 1;
                    self.view.total_seats = request.local_declaration.seat_count() as u8;
                    self.view.local_seats = request.local_declaration.seat_count() as u8;
                    RecordedCommand::Create {
                        visibility: request.visibility,
                        peer_id: request.local_declaration.peer_id,
                        user_id: request.local_declaration.authenticated_user,
                        seats: request.local_declaration.seat_count() as u8,
                    }
                }
                NativeOnlineCommand::Join {
                    intent,
                    local_declaration,
                } => {
                    self.local_declaration = Some(local_declaration);
                    self.set_screen(NativeOnlineScreen::Lobby);
                    self.view.role = Some(OnlineLobbyRole::Client);
                    self.view.lobby = Some(intent.lobby);
                    self.view.lobby_members = 2;
                    self.view.total_seats = 2;
                    self.view.local_seats = local_declaration.seat_count() as u8;
                    RecordedCommand::Join {
                        intent,
                        peer_id: local_declaration.peer_id,
                        seats: local_declaration.seat_count() as u8,
                    }
                }
                NativeOnlineCommand::SetLocalDeclaration(declaration) => {
                    self.local_declaration = Some(declaration);
                    self.view.local_ready = false;
                    self.view.local_seats = declaration.seat_count() as u8;
                    RecordedCommand::SetDeclaration {
                        revision: declaration.revision,
                        ready: declaration.ready,
                        seats: declaration.seat_count() as u8,
                    }
                }
                NativeOnlineCommand::SetReady(ready) => {
                    self.view.local_ready = ready;
                    RecordedCommand::SetReady(ready)
                }
                NativeOnlineCommand::CommitManifest { options, .. } => {
                    self.committed_options = Some(options);
                    self.set_screen(NativeOnlineScreen::ManifestAgreement);
                    RecordedCommand::Commit(options)
                }
                NativeOnlineCommand::AcceptManifest(config) => {
                    self.config = Some(config);
                    self.lifecycle.manifests_accepted += 1;
                    self.view.outcome = None;
                    self.set_screen(NativeOnlineScreen::Loading);
                    RecordedCommand::Other
                }
                NativeOnlineCommand::ContentLoaded => {
                    self.lifecycle.content_loaded += 1;
                    RecordedCommand::Other
                }
                NativeOnlineCommand::InitialSyncComplete => {
                    self.lifecycle.initial_sync_completed += 1;
                    if self.resume_after_sync {
                        self.resume_after_sync = false;
                        self.set_screen(NativeOnlineScreen::Fighting);
                    } else {
                        self.set_screen(NativeOnlineScreen::Ready);
                    }
                    RecordedCommand::Other
                }
                NativeOnlineCommand::BeginCountdown(start_tick) => {
                    self.lifecycle.countdowns_begun += 1;
                    self.view.countdown_start_tick = Some(start_tick);
                    self.set_screen(NativeOnlineScreen::Countdown);
                    RecordedCommand::Other
                }
                NativeOnlineCommand::MarkFighting(_) => {
                    self.lifecycle.fighting_marked += 1;
                    self.set_screen(NativeOnlineScreen::Fighting);
                    RecordedCommand::Other
                }
                NativeOnlineCommand::BeginResultConfirmation => {
                    self.lifecycle.result_confirmations_begun += 1;
                    self.set_screen(NativeOnlineScreen::ConfirmingResult);
                    RecordedCommand::Other
                }
                NativeOnlineCommand::ConfirmResult => {
                    self.lifecycle.results_confirmed += 1;
                    self.view.outcome = Some(OnlineMatchOutcome::Confirmed);
                    self.set_screen(NativeOnlineScreen::Results);
                    RecordedCommand::Other
                }
                NativeOnlineCommand::ApplyAuthorityDisconnect(disconnect) => {
                    let failure = OnlineFailure::from_disconnect(disconnect.message);
                    self.view.failure = Some(failure);
                    match disconnect.message.retry {
                        RetryDisposition::ReconnectAllowed => self.begin_reconnect(),
                        RetryDisposition::ReturnToLobby | RetryDisposition::Fatal => {
                            self.set_screen(NativeOnlineScreen::Error);
                        }
                        RetryDisposition::MatchEndedNoContest => {
                            self.view.outcome = Some(OnlineMatchOutcome::NoContestHostLost);
                            self.set_screen(NativeOnlineScreen::Results);
                        }
                    }
                    RecordedCommand::Other
                }
                NativeOnlineCommand::MarkAuthorityTerminalDrained {
                    user,
                    peer_id,
                    connection,
                    retry,
                } => RecordedCommand::AuthorityTerminalDrained {
                    user,
                    peer_id,
                    connection,
                    retry,
                },
                NativeOnlineCommand::QuiesceAdmission => RecordedCommand::Other,
                NativeOnlineCommand::Rematch => {
                    self.lifecycle.rematches += 1;
                    self.config = None;
                    self.view.outcome = None;
                    self.view.countdown_start_tick = None;
                    self.view.local_ready = true;
                    self.view.all_members_ready = true;
                    self.resume_after_sync = false;
                    self.set_screen(NativeOnlineScreen::Lobby);
                    RecordedCommand::Other
                }
                NativeOnlineCommand::ReturnToLobby => {
                    self.lifecycle.returns_to_lobby += 1;
                    self.config = None;
                    self.view.outcome = None;
                    self.view.countdown_start_tick = None;
                    self.view.local_ready = true;
                    self.view.all_members_ready = true;
                    self.resume_after_sync = false;
                    self.set_screen(NativeOnlineScreen::Lobby);
                    RecordedCommand::Other
                }
                NativeOnlineCommand::DeclineJoin => {
                    self.set_screen(NativeOnlineScreen::OnlineMenu);
                    RecordedCommand::Decline
                }
                NativeOnlineCommand::LeaveOnline => {
                    self.lifecycle.leaves += 1;
                    self.config = None;
                    self.resume_after_sync = false;
                    self.view = view_for(NativeOnlineScreen::OnlineMenu);
                    RecordedCommand::Leave
                }
            };
            self.commands.push(recorded);
            Ok(())
        }

        fn open_invite_overlay_port(
            &mut self,
        ) -> Result<SteamOverlayRequestStatus, NativeOnlineRuntimeError> {
            self.overlay_request_count = self.overlay_request_count.saturating_add(1);
            self.commands.push(RecordedCommand::Invite);
            Ok(self.overlay_status)
        }

        fn poll_event_port(&mut self) -> Option<OnlineLobbyEvent> {
            self.events.pop_front()
        }

        fn take_endpoint_port(&mut self) -> Option<NativeOnlineEndpoint> {
            self.endpoints.pop_front()
        }

        fn match_config_port(&self) -> Option<HeadlessMatchConfig> {
            self.config.clone()
        }

        fn committed_roster_port(&self) -> Option<CommittedAuthenticatedRoster> {
            self.committed
        }

        fn committed_peers_port(
            &self,
        ) -> Option<ArrayVec<AuthenticatedPeer, MAX_STEAM_LOBBY_MEMBERS>> {
            if let Some(peers) = &self.committed_peers {
                return Some(peers.clone());
            }
            let committed = self.committed?;
            let mut peers = ArrayVec::new();
            for peer in committed.iter() {
                peers.push(peer);
            }
            Some(peers)
        }

        fn local_authenticated_user_port(&self) -> Option<AuthenticatedUserId> {
            Some(self.local_user)
        }

        fn transport_retirement_pending_port(&self) -> bool {
            self.transport_retirement_pending
        }

        fn make_local_declaration_port(
            &self,
            peer_id: PeerId,
            revision: u16,
            ready: bool,
            seats: &[OnlineSeatSelection],
        ) -> Result<OnlineRosterMember, NativeOnlineRuntimeError> {
            OnlineRosterMember::new(peer_id, self.local_user, revision, ready, seats)
                .map_err(|_| NativeOnlineRuntimeError::InvalidAuthenticatedRoster)
        }
    }

    fn view_for(screen: NativeOnlineScreen) -> NativeOnlineViewModel {
        let in_menu = screen == NativeOnlineScreen::OnlineMenu;
        let in_lobby = screen == NativeOnlineScreen::Lobby;
        NativeOnlineViewModel {
            availability: NativeOnlineAvailability::Available,
            screen,
            actions: NativeOnlineActions {
                create_private: in_menu,
                create_friends: in_menu,
                accept_join: screen == NativeOnlineScreen::JoinPrompt,
                decline_join: screen == NativeOnlineScreen::JoinPrompt,
                edit_couch_seats_and_loadouts: in_lobby,
                toggle_ready: in_lobby,
                invite_friends: in_lobby,
                leave: !in_menu,
                rematch: screen == NativeOnlineScreen::Results,
                return_to_lobby: screen == NativeOnlineScreen::Results,
                return_to_menu: in_menu || screen == NativeOnlineScreen::Results,
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
            relay_status: crate::steam_transport::SteamRelayStatus::default(),
            countdown_start_tick: None,
            outcome: None,
            failure: None,
        }
    }

    fn ready_input_delay_calibration(p95_rtt_ms: u16) -> InputDelayCalibrationSnapshot {
        let (input_delay_ticks, required_rollback_ticks) =
            crate::network_quality::calibrated_input_delay(p95_rtt_ms);
        InputDelayCalibrationSnapshot {
            state: InputDelayCalibrationState::Ready,
            remote_peer_count: 1,
            calibrated_peer_count: 1,
            worst_p95_rtt_ms: Some(p95_rtt_ms),
            selected_input_delay_ticks: Some(input_delay_ticks),
            required_rollback_ticks: Some(required_rollback_ticks),
        }
    }

    struct TemporaryDiagnosticsRoot {
        path: PathBuf,
    }

    impl TemporaryDiagnosticsRoot {
        fn unique() -> Self {
            static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            Self {
                path: std::env::temp_dir().join(format!(
                    "afc-native-online-app-{}-{sequence}",
                    std::process::id()
                )),
            }
        }
    }

    impl Drop for TemporaryDiagnosticsRoot {
        fn drop(&mut self) {
            match std::fs::remove_dir_all(&self.path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!(
                    "failed to remove lifecycle diagnostics root {}: {error}",
                    self.path.display()
                ),
            }
        }
    }

    struct PumpedFakeSteamPair {
        _network: FakeSteamTransportNetwork,
        host: SteamTransport,
        remote: SteamTransport,
        next_now_ms: u64,
    }

    impl PumpedFakeSteamPair {
        fn admitted(
            lobby: SteamLobbyId,
            host_user: SteamUserId,
            remote_user: SteamUserId,
            host_peer: PeerId,
            remote_peer: PeerId,
            base_now_ms: u64,
        ) -> (Self, NativeOnlineEndpoint, NativeOnlineEndpoint) {
            Self::admitted_for_purpose(
                lobby,
                host_user,
                remote_user,
                host_peer,
                remote_peer,
                AdmissionPurpose::Initial,
                base_now_ms,
            )
        }

        fn admitted_for_purpose(
            lobby: SteamLobbyId,
            host_user: SteamUserId,
            remote_user: SteamUserId,
            host_peer: PeerId,
            remote_peer: PeerId,
            purpose: AdmissionPurpose,
            base_now_ms: u64,
        ) -> (Self, NativeOnlineEndpoint, NativeOnlineEndpoint) {
            let network = FakeSteamTransportNetwork::new(256).unwrap();
            let transport_config = SteamTransportConfig {
                endpoint_queue_packets: 256,
                max_send_datagrams_per_connection_per_pump: 64,
                max_receive_datagrams_per_connection_per_pump: 64,
                ..default()
            };
            let host_session = SteamP2pSession {
                lobby,
                authority_user: host_user,
                role: SteamTransportRole::ListenAuthority,
                virtual_port: 0,
            };
            let remote_session = SteamP2pSession {
                role: SteamTransportRole::Client,
                ..host_session
            };
            let mut host = network
                .create_transport(host_user, host_session, transport_config, base_now_ms)
                .unwrap();
            let mut remote = network
                .create_transport(remote_user, remote_session, transport_config, base_now_ms)
                .unwrap();
            host.start_listening().unwrap();
            host.set_allowed_incoming_users(&[remote_user]).unwrap();
            let connection = remote
                .connect_p2p(
                    AuthenticatedSteamPeer {
                        lobby,
                        user: host_user,
                        license_owner_user: host_user,
                        authenticated_user: AuthenticatedUserId::new(host_user.get()).unwrap(),
                        local_seats: 1,
                        purpose,
                    },
                    base_now_ms,
                )
                .unwrap();
            host.pump(base_now_ms + 1).unwrap();
            let pending = host
                .poll_event()
                .expect("listen transport receives the pending connection");
            assert!(matches!(
                pending,
                crate::steam_transport::SteamTransportEvent::IncomingPending {
                    connection: observed,
                    lobby: observed_lobby,
                    user,
                    ..
                } if observed == connection && observed_lobby == lobby && user == remote_user
            ));
            host.admit_incoming(
                connection,
                AuthenticatedSteamPeer {
                    lobby,
                    user: remote_user,
                    license_owner_user: host_user,
                    authenticated_user: AuthenticatedUserId::new(remote_user.get()).unwrap(),
                    local_seats: 1,
                    purpose,
                },
                base_now_ms + 1,
            )
            .unwrap();
            host.pump(base_now_ms + 2).unwrap();
            remote.pump(base_now_ms + 2).unwrap();
            assert!(matches!(
                host.poll_event(),
                Some(crate::steam_transport::SteamTransportEvent::ConnectionReady {
                    connection: observed,
                    lobby: observed_lobby,
                    user,
                }) if observed == connection
                    && observed_lobby == lobby
                    && user == remote_user
            ));
            assert!(matches!(
                remote.poll_event(),
                Some(crate::steam_transport::SteamTransportEvent::ConnectionReady {
                    connection: observed,
                    lobby: observed_lobby,
                    user,
                }) if observed == connection
                    && observed_lobby == lobby
                    && user == host_user
            ));
            assert!(network.connection_was_accepted(connection).unwrap());
            let host_admitted = host.take_endpoint(connection).unwrap();
            let remote_admitted = remote.take_endpoint(connection).unwrap();
            let host_handoff = NativeOnlineEndpoint {
                peer_id: remote_peer,
                reconnect: purpose == AdmissionPurpose::Reconnect,
                admitted: host_admitted,
            };
            let remote_handoff = NativeOnlineEndpoint {
                peer_id: host_peer,
                reconnect: purpose == AdmissionPurpose::Reconnect,
                admitted: remote_admitted,
            };
            (
                Self {
                    _network: network,
                    host,
                    remote,
                    next_now_ms: base_now_ms + 3,
                },
                host_handoff,
                remote_handoff,
            )
        }

        fn pump(&mut self) {
            let now_ms = self.next_now_ms;
            self.next_now_ms = self.next_now_ms.saturating_add(1);
            self.host.pump(now_ms).unwrap();
            self.remote.pump(now_ms).unwrap();
            while self.host.poll_event().is_some() {}
            while self.remote.poll_event().is_some() {}
        }
    }

    fn ready_member(declaration: OnlineRosterMember) -> OnlineRosterMember {
        OnlineRosterMember::new(
            declaration.peer_id,
            declaration.authenticated_user,
            declaration.revision,
            true,
            declaration.seats(),
        )
        .unwrap()
    }

    fn build_lifecycle_config(
        host: OnlineRosterMember,
        remote: OnlineRosterMember,
        options: OnlineManifestOptions,
    ) -> HeadlessMatchConfig {
        let mut roster = OnlineRoster::default();
        roster.upsert(host).unwrap();
        roster.upsert(remote).unwrap();
        roster
            .build_headless_config(options, SimTick::ZERO)
            .unwrap()
    }

    fn active_remote_failure_fixture() -> (NativeOnlineApplication, FakeRuntime, PumpedFakeSteamPair)
    {
        let lobby = SteamLobbyId::new(61_001).unwrap();
        let host_user = SteamUserId::new(92_001).unwrap();
        let remote_user = SteamUserId::new(92_002).unwrap();
        let host_authenticated = AuthenticatedUserId::new(host_user.get()).unwrap();
        let remote_authenticated = AuthenticatedUserId::new(remote_user.get()).unwrap();
        let host_peer = PeerId::new(51_001).unwrap();
        let remote_peer = PeerId::new(51_002).unwrap();
        let host_member = OnlineRosterMember::new(
            host_peer,
            host_authenticated,
            1,
            true,
            &[OnlineSeatSelection {
                team: TeamId::new(0).unwrap(),
                character: DefinitionId::new(0).unwrap(),
                style: DefinitionId::new(0).unwrap(),
                equipment: DefinitionId::new(0).unwrap(),
            }],
        )
        .unwrap();
        let remote_member = OnlineRosterMember::new(
            remote_peer,
            remote_authenticated,
            1,
            true,
            &[OnlineSeatSelection {
                team: TeamId::new(1).unwrap(),
                character: DefinitionId::new(1).unwrap(),
                style: DefinitionId::new(0).unwrap(),
                equipment: DefinitionId::new(0).unwrap(),
            }],
        )
        .unwrap();
        let config = build_lifecycle_config(
            host_member,
            remote_member,
            OnlineManifestOptions::casual_listen(
                MatchId::new([0xfa; 16]).unwrap(),
                host_peer,
                DefinitionId::new(0).unwrap(),
                DefinitionId::new(0).unwrap(),
                0xa11c_e5e5,
                SimTick(ONLINE_COUNTDOWN_LEAD_TICKS),
            ),
        );
        let (transport, staged_host_endpoint, remote_endpoint) = PumpedFakeSteamPair::admitted(
            lobby,
            host_user,
            remote_user,
            host_peer,
            remote_peer,
            300_000,
        );
        let client = RemoteOnlineClient::spawn(
            remote_endpoint.admitted.endpoint,
            config,
            remote_peer,
            RemoteOnlineClientConfig::default(),
        )
        .unwrap();
        let mut application = NativeOnlineApplication::default();
        application.local_peer_id = Some(remote_peer);
        application.active = Some(ActiveNativeOnlineMatch::Remote(client));
        application.metrics.sessions_started = 1;
        application.staged_endpoints.push_back(staged_host_endpoint);
        application
            .pending_authority_commands
            .push_back(ListenAuthorityCommand::BeginShutdown);
        let mut runtime = FakeRuntime::available_as(remote_authenticated);
        runtime.view = view_for(NativeOnlineScreen::Fighting);
        runtime.view.role = Some(OnlineLobbyRole::Client);
        (application, runtime, transport)
    }

    fn active_listen_security_fixture() -> (
        NativeOnlineApplication,
        FakeRuntime,
        PumpedFakeSteamPair,
        NativeOnlineEndpoint,
        SteamUserId,
        PeerId,
    ) {
        let lobby = SteamLobbyId::new(61_101).unwrap();
        let host_user = SteamUserId::new(93_001).unwrap();
        let remote_user = SteamUserId::new(93_002).unwrap();
        let host_peer = PeerId::new(52_001).unwrap();
        let remote_peer = PeerId::new(52_002).unwrap();
        let host = AuthenticatedPeer {
            peer_id: host_peer,
            user_id: host_user.authenticated(),
        };
        let remote = AuthenticatedPeer {
            peer_id: remote_peer,
            user_id: remote_user.authenticated(),
        };
        let host_member = OnlineRosterMember::new(
            host_peer,
            host.user_id,
            1,
            true,
            &[OnlineSeatSelection {
                team: TeamId::new(0).unwrap(),
                character: DefinitionId::new(0).unwrap(),
                style: DefinitionId::new(0).unwrap(),
                equipment: DefinitionId::new(0).unwrap(),
            }],
        )
        .unwrap();
        let remote_member = OnlineRosterMember::new(
            remote_peer,
            remote.user_id,
            1,
            true,
            &[OnlineSeatSelection {
                team: TeamId::new(1).unwrap(),
                character: DefinitionId::new(1).unwrap(),
                style: DefinitionId::new(0).unwrap(),
                equipment: DefinitionId::new(0).unwrap(),
            }],
        )
        .unwrap();
        let config = build_lifecycle_config(
            host_member,
            remote_member,
            OnlineManifestOptions::casual_listen(
                MatchId::new([0xfb; 16]).unwrap(),
                host_peer,
                DefinitionId::new(0).unwrap(),
                DefinitionId::new(0).unwrap(),
                0x5ec0_0017,
                SimTick(ONLINE_COUNTDOWN_LEAD_TICKS),
            ),
        );
        let (mut transport, host_endpoint, remote_endpoint) = PumpedFakeSteamPair::admitted(
            lobby,
            host_user,
            remote_user,
            host_peer,
            remote_peer,
            310_000,
        );

        let mut runtime = FakeRuntime::available_as(host.user_id);
        runtime.view.role = Some(OnlineLobbyRole::ListenAuthority);
        runtime.view.lobby = Some(lobby);
        runtime.committed_peers = Some(ArrayVec::from_iter([host, remote]));
        runtime
            .execute_port(NativeOnlineCommand::AcceptManifest(config), 1)
            .unwrap();
        runtime
            .events
            .push_back(OnlineLobbyEvent::PeerAuthenticated {
                user: remote_user,
                peer_id: remote_peer,
                reconnect: false,
            });
        runtime.endpoints.push_back(host_endpoint);

        let mut application = NativeOnlineApplication::default();
        application.local_peer_id = Some(host_peer);
        application.pump(&mut runtime, 2).unwrap();
        assert!(matches!(
            application.active,
            Some(ActiveNativeOnlineMatch::Listen(_))
        ));
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            transport.pump();
            application.pump(&mut runtime, 3).unwrap();
            let attached = match &application.active {
                Some(ActiveNativeOnlineMatch::Listen(online_match)) => {
                    online_match.authority.status().peer(remote_peer).is_some()
                        && application
                            .authority_endpoints
                            .iter()
                            .flatten()
                            .any(|record| {
                                record.peer_id == remote_peer
                                    && matches!(
                                        record.state,
                                        AuthorityEndpointState::Attached { .. }
                                    )
                            })
                }
                _ => false,
            };
            if attached {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "listen security fixture did not attach the remote peer"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        (
            application,
            runtime,
            transport,
            remote_endpoint,
            remote_user,
            remote_peer,
        )
    }

    fn stage_lifecycle_session(
        host_runtime: &mut FakeRuntime,
        remote_runtime: &mut FakeRuntime,
        config: HeadlessMatchConfig,
        host_peer: AuthenticatedPeer,
        remote_peer: AuthenticatedPeer,
        host_user: SteamUserId,
        remote_user: SteamUserId,
        host_endpoint: NativeOnlineEndpoint,
        remote_endpoint: NativeOnlineEndpoint,
        now_ms: u64,
    ) {
        let mut committed = ArrayVec::new();
        committed.push(host_peer);
        committed.push(remote_peer);
        host_runtime.committed_peers = Some(committed.clone());
        remote_runtime.committed_peers = Some(committed);
        host_runtime
            .execute_port(NativeOnlineCommand::AcceptManifest(config.clone()), now_ms)
            .unwrap();
        remote_runtime
            .execute_port(NativeOnlineCommand::AcceptManifest(config), now_ms)
            .unwrap();
        host_runtime
            .events
            .push_back(OnlineLobbyEvent::PeerAuthenticated {
                user: remote_user,
                peer_id: remote_peer.peer_id,
                reconnect: false,
            });
        remote_runtime
            .events
            .push_back(OnlineLobbyEvent::PeerAuthenticated {
                user: host_user,
                peer_id: host_peer.peer_id,
                reconnect: false,
            });
        host_runtime.endpoints.push_back(host_endpoint);
        remote_runtime.endpoints.push_back(remote_endpoint);
    }

    fn pump_lifecycle_frame(
        host_application: &mut NativeOnlineApplication,
        host_runtime: &mut FakeRuntime,
        remote_application: &mut NativeOnlineApplication,
        remote_runtime: &mut FakeRuntime,
        transport: &mut PumpedFakeSteamPair,
        host_target: &mut World,
        remote_target: &mut World,
        now_ms: &mut u64,
    ) {
        transport.pump();
        host_application.pump(host_runtime, *now_ms).unwrap();
        remote_application.pump(remote_runtime, *now_ms).unwrap();
        transport.pump();
        host_application.project_latest(host_target).unwrap();
        remote_application.project_latest(remote_target).unwrap();
        *now_ms = now_ms.saturating_add(1);
    }

    fn drive_lifecycle_to_fighting(
        host_application: &mut NativeOnlineApplication,
        host_runtime: &mut FakeRuntime,
        remote_application: &mut NativeOnlineApplication,
        remote_runtime: &mut FakeRuntime,
        transport: &mut PumpedFakeSteamPair,
        host_target: &mut World,
        remote_target: &mut World,
        now_ms: &mut u64,
    ) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            pump_lifecycle_frame(
                host_application,
                host_runtime,
                remote_application,
                remote_runtime,
                transport,
                host_target,
                remote_target,
                now_ms,
            );
            let host_phase = host_application
                .active
                .as_ref()
                .map(|active| active.client().status().phase);
            let remote_phase = remote_application
                .active
                .as_ref()
                .map(|active| active.client().status().phase);
            if host_runtime.view.screen == NativeOnlineScreen::Fighting
                && remote_runtime.view.screen == NativeOnlineScreen::Fighting
                && host_phase == Some(RemoteOnlineClientPhase::Fighting)
                && remote_phase == Some(RemoteOnlineClientPhase::Fighting)
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "online workers did not reach fighting: host={host_phase:?}/{:?}, remote={remote_phase:?}/{:?}",
                host_runtime.view.screen,
                remote_runtime.view.screen
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn drive_lifecycle_to_confirmed_result(
        host_application: &mut NativeOnlineApplication,
        host_runtime: &mut FakeRuntime,
        remote_application: &mut NativeOnlineApplication,
        remote_runtime: &mut FakeRuntime,
        transport: &mut PumpedFakeSteamPair,
        host_target: &mut World,
        remote_target: &mut World,
        now_ms: &mut u64,
    ) -> (
        crate::session::ConfirmedSessionResult,
        crate::session::ConfirmedSessionResult,
        crate::remote_online_client::RemoteOnlineWorkerMetrics,
        crate::remote_online_client::RemoteOnlineWorkerMetrics,
    ) {
        let seat = LocalSeatId::new(0).unwrap();
        let mut host_inputs = LocalTickInputState::default();
        let mut remote_inputs = LocalTickInputState::default();
        host_inputs.merge_render_sample(seat, RenderInputSample::default());
        host_application
            .submit_local_inputs(&mut host_inputs)
            .unwrap();
        remote_inputs.merge_render_sample(
            seat,
            RenderInputSample {
                movement: QuantizedMovement::new(127, 0),
                ..default()
            },
        );
        remote_application
            .submit_local_inputs(&mut remote_inputs)
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(35);
        loop {
            pump_lifecycle_frame(
                host_application,
                host_runtime,
                remote_application,
                remote_runtime,
                transport,
                host_target,
                remote_target,
                now_ms,
            );

            let host_status = host_application
                .active
                .as_ref()
                .expect("host worker remains active through results")
                .client()
                .status();
            let remote_status = remote_application
                .active
                .as_ref()
                .expect("remote worker remains active through results")
                .client()
                .status();

            let host_terminal = host_application
                .active
                .as_ref()
                .and_then(|active| active.client().terminal());
            let remote_terminal = remote_application
                .active
                .as_ref()
                .and_then(|active| active.client().terminal());
            if let (
                Some(RemoteOnlineTerminal::Completed(host_result)),
                Some(RemoteOnlineTerminal::Completed(remote_result)),
            ) = (host_terminal, remote_terminal)
                && host_runtime.view.screen == NativeOnlineScreen::Results
                && remote_runtime.view.screen == NativeOnlineScreen::Results
            {
                let host_metrics = host_application.active.as_ref().unwrap().client().metrics();
                let remote_metrics = remote_application
                    .active
                    .as_ref()
                    .unwrap()
                    .client()
                    .metrics();
                return (host_result, remote_result, host_metrics, remote_metrics);
            }
            let authority_status = match host_application.active.as_ref() {
                Some(ActiveNativeOnlineMatch::Listen(online_match)) => {
                    Some(online_match.authority.status())
                }
                _ => None,
            };
            assert!(
                !matches!(
                    host_terminal,
                    Some(RemoteOnlineTerminal::Failed(_) | RemoteOnlineTerminal::Stopped)
                ),
                "host worker ended abnormally: terminal={host_terminal:?}, client={host_status:?}, authority={authority_status:?}, remote={remote_status:?}/{remote_terminal:?}"
            );
            assert!(
                !matches!(
                    remote_terminal,
                    Some(RemoteOnlineTerminal::Failed(_) | RemoteOnlineTerminal::Stopped)
                ),
                "remote worker ended abnormally: terminal={remote_terminal:?}, client={remote_status:?}, authority={authority_status:?}, host={host_status:?}/{host_terminal:?}, host_transport={:?}, remote_transport={:?}",
                transport.host.metrics(),
                transport.remote.metrics()
            );
            assert!(
                Instant::now() < deadline,
                "normal stock match did not finish: host={host_status:?}/{host_terminal:?}, remote={remote_status:?}/{remote_terminal:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn drive_listen_shutdown_to_completion(
        host_application: &mut NativeOnlineApplication,
        host_runtime: &mut FakeRuntime,
        remote_application: &mut NativeOnlineApplication,
        remote_runtime: &mut FakeRuntime,
        transport: &mut PumpedFakeSteamPair,
        now_ms: &mut u64,
    ) {
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            transport.pump();
            host_application.pump(host_runtime, *now_ms).unwrap();
            remote_application.pump(remote_runtime, *now_ms).unwrap();
            transport.pump();
            *now_ms = now_ms.saturating_add(1);
            if host_application.active.is_none() && host_application.listen_shutdown.is_none() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "listen shutdown did not drain: phase={:?}, terminal={:?}",
                host_application
                    .active
                    .as_ref()
                    .and_then(|active| match active {
                        ActiveNativeOnlineMatch::Listen(online_match) => {
                            Some(online_match.authority.status().phase)
                        }
                        ActiveNativeOnlineMatch::Remote(_) => None,
                    }),
                host_application
                    .active
                    .as_ref()
                    .and_then(|active| match active {
                        ActiveNativeOnlineMatch::Listen(online_match) =>
                            online_match.authority.terminal(),
                        ActiveNativeOnlineMatch::Remote(_) => None,
                    }),
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn couch_editor_and_create_command_bind_real_local_identity() {
        let mut runtime = FakeRuntime::available();
        let mut application = NativeOnlineApplication::default();
        application
            .dispatch(&mut runtime, NativeOnlineUiAction::AddSeat, 10)
            .unwrap();
        application
            .dispatch(&mut runtime, NativeOnlineUiAction::NextCharacter, 11)
            .unwrap();
        application
            .dispatch(&mut runtime, NativeOnlineUiAction::ToggleTeam, 12)
            .unwrap();
        application
            .dispatch(&mut runtime, NativeOnlineUiAction::CreateFriends, 13)
            .unwrap();

        let Some(RecordedCommand::Create {
            visibility,
            peer_id,
            user_id,
            seats,
        }) = runtime.commands.last().copied()
        else {
            panic!("expected create command")
        };
        assert_eq!(visibility, NativeOnlineVisibility::FriendsOnly);
        assert_eq!(user_id, runtime.local_user);
        assert_eq!(Some(peer_id), application.local_peer_id);
        assert_eq!(seats, 2);
        assert_ne!(application.editor.seats()[1].character.get(), 1);
        assert_eq!(application.editor.seats()[1].team.get(), 0);
    }

    #[test]
    fn host_precreate_editor_covers_all_arenas_and_rules_then_freezes_in_lobby() {
        let mut runtime = FakeRuntime::available();
        let mut application = NativeOnlineApplication::default();
        let mut arenas = std::collections::BTreeSet::new();
        let mut rules = std::collections::BTreeSet::new();
        for _ in 0..arena_definitions().len() {
            arenas.insert(application.editor.arena.get());
            application
                .dispatch(&mut runtime, NativeOnlineUiAction::NextArena, 1)
                .unwrap();
        }
        for _ in 0..RULE_PRESETS.len() {
            rules.insert(application.editor.rules.get());
            application
                .dispatch(&mut runtime, NativeOnlineUiAction::NextRules, 1)
                .unwrap();
        }
        assert_eq!(arenas.len(), arena_definitions().len());
        assert_eq!(rules.len(), RULE_PRESETS.len());

        runtime.set_screen(NativeOnlineScreen::Lobby);
        runtime.view.role = Some(OnlineLobbyRole::ListenAuthority);
        let frozen = application.editor;
        assert_eq!(
            application.dispatch(&mut runtime, NativeOnlineUiAction::NextArena, 2),
            Err(NativeOnlineApplicationError::InvalidAction)
        );
        assert_eq!(
            application.dispatch(&mut runtime, NativeOnlineUiAction::NextRules, 3),
            Err(NativeOnlineApplicationError::InvalidAction)
        );
        assert_eq!(application.editor, frozen);
    }

    #[test]
    fn local_modal_and_active_overlay_neutralize_actions_without_stopping_tick_drain() {
        let seat = LocalSeatId::new(0).unwrap();
        let mut inputs = LocalTickInputState::default();
        inputs.merge_render_sample(
            seat,
            RenderInputSample {
                movement: QuantizedMovement::new(127, 0),
                held: InputMask::LIGHT,
                pressed: InputMask::LIGHT,
                ..default()
            },
        );
        assert!(neutralize_online_input(true, false, &mut inputs));
        let first = inputs.drain_for_tick(seat, 1);
        assert_eq!(first.movement, QuantizedMovement::ZERO);
        assert_eq!(first.held, InputMask::NONE);
        assert_eq!(first.pressed, InputMask::NONE);
        let second = inputs.drain_for_tick(seat, 2);
        assert_eq!(second.sequence.0, first.sequence.0.wrapping_add(1));

        inputs.merge_render_sample(
            seat,
            RenderInputSample {
                held: InputMask::HEAVY,
                pressed: InputMask::HEAVY,
                ..default()
            },
        );
        assert!(neutralize_online_input(false, true, &mut inputs));
        let overlay_frame = inputs.drain_for_tick(seat, 3);
        assert_eq!(overlay_frame.held, InputMask::NONE);
        assert_eq!(overlay_frame.pressed, InputMask::NONE);
        assert_eq!(
            overlay_frame.sequence.0,
            second.sequence.0.wrapping_add(1),
            "online ticks continue while the overlay owns local input"
        );
    }

    #[test]
    fn overlay_unavailable_notice_is_nonfatal_dismissible_and_expires_at_four_seconds() {
        let mut runtime = FakeRuntime::available();
        runtime.set_screen(NativeOnlineScreen::Lobby);
        runtime.overlay_status = SteamOverlayRequestStatus::Unavailable;
        let mut application = NativeOnlineApplication::default();

        application
            .dispatch(&mut runtime, NativeOnlineUiAction::InviteFriends, 100)
            .unwrap();
        let notice = application.overlay_notice().expect("notice is retained");
        assert_eq!(notice.surface, OverlayUnavailableSurface::InviteFriends);
        assert_eq!(notice.failure, OnlineFailure::overlay_unavailable());
        assert_eq!(notice.dismiss_at_ms, 4_100);
        assert_eq!(runtime.overlay_request_count, 1);
        assert_eq!(runtime.view.screen, NativeOnlineScreen::Lobby);
        assert!(runtime.view.failure.is_none());
        assert!(application.failure_override.is_none());
        assert_eq!(application.metrics.sessions_stopped, 0);
        let snapshot = application.ui_snapshot(&runtime, true);
        assert_eq!(snapshot.screen, NativeOnlineScreen::Lobby);
        assert!(snapshot.failure.is_none());
        assert_eq!(snapshot.overlay_notice, Some(notice));

        application.expire_overlay_notice(4_099);
        assert_eq!(application.overlay_notice(), Some(notice));
        application.expire_overlay_notice(4_100);
        assert!(application.overlay_notice().is_none());

        application.observe_overlay_request(
            OverlayUnavailableSurface::ControllerBindings,
            SteamOverlayRequestStatus::Unavailable,
            u64::MAX - 10,
        );
        let saturated = application.overlay_notice().unwrap();
        assert_eq!(saturated.dismiss_at_ms, u64::MAX);
        application.expire_overlay_notice(u64::MAX - 1);
        assert_eq!(application.overlay_notice(), Some(saturated));
        application.dismiss_overlay_notice();
        assert!(application.overlay_notice().is_none());
    }

    #[test]
    fn held_binding_action_does_not_reopen_or_extend_overlay_notice() {
        let mut snapshot = NativeOnlineUiSnapshot::default();
        snapshot.screen = NativeOnlineScreen::Lobby;
        snapshot.actions.invite_friends = true;
        let mut application = NativeOnlineApplication::default();
        application.controller_menu_intent(&snapshot, SteamInputSnapshot::default());
        let mut bindings = SteamMenuInputMask::NONE;
        bindings.insert(SteamMenuAction::OpenBindings);
        let held = steam_input_with_menu(2, bindings);

        assert_eq!(
            application.controller_menu_intent(&snapshot, held),
            ControllerMenuIntent::OpenBindings(2)
        );
        application.observe_overlay_request(
            OverlayUnavailableSurface::ControllerBindings,
            SteamOverlayRequestStatus::Unavailable,
            200,
        );
        let first = application.overlay_notice().unwrap();
        assert_eq!(
            application.controller_menu_intent(&snapshot, held),
            ControllerMenuIntent::None
        );
        assert_eq!(application.overlay_notice(), Some(first));
        assert_eq!(first.dismiss_at_ms, 4_200);
    }

    #[test]
    fn shared_policy_matrix_keeps_combat_hud_and_uses_nonblocking_strip() {
        let mut snapshot = NativeOnlineUiSnapshot {
            visible: true,
            screen: NativeOnlineScreen::Fighting,
            ..default()
        };
        let fighting = presentation_policy_for(&snapshot, 6);
        assert_eq!(fighting.phase, PresentationPhase::Fighting);
        assert_eq!(fighting.panel, OnlinePanelMode::FightStrip);
        assert!(fighting.gameplay_hud_visible);
        assert_eq!(fighting.music, PresentationMusicTrack::Arena(6));
        assert!(ONLINE_COMPACT_PANEL_WIDTH_PERCENT <= 40.0);
        assert!(ONLINE_COMPACT_PANEL_MAX_WIDTH <= 420.0);

        snapshot.screen = NativeOnlineScreen::Countdown;
        assert_eq!(
            presentation_policy_for(&snapshot, 6).panel,
            OnlinePanelMode::CountdownStrip
        );
        snapshot.screen = NativeOnlineScreen::Reconnecting;
        assert_eq!(
            presentation_policy_for(&snapshot, 6).panel,
            OnlinePanelMode::ReconnectStrip
        );
        snapshot.screen = NativeOnlineScreen::ConfirmingResult;
        assert_eq!(
            presentation_policy_for(&snapshot, 6).panel,
            OnlinePanelMode::ConfirmingStrip
        );
        snapshot.screen = NativeOnlineScreen::Results;
        let results = presentation_policy_for(&snapshot, 6);
        assert_eq!(results.panel, OnlinePanelMode::Results);
        assert_eq!(results.music, PresentationMusicTrack::None);
        assert!(!results.gameplay_hud_visible);

        snapshot.screen = NativeOnlineScreen::Fighting;
        snapshot.leave_confirmation_open = true;
        let leave = presentation_policy_for(&snapshot, 6);
        assert_eq!(leave.panel, OnlinePanelMode::LeaveConfirmation);
        assert!(leave.gameplay_hud_visible);
    }

    #[test]
    fn launch_invite_is_retained_and_joined_exactly_once() {
        let mut runtime = FakeRuntime::available();
        let intent =
            LobbyJoinIntent::friends_list(SteamLobbyId::new(44).unwrap(), 100, 5_000).unwrap();
        runtime.view = view_for(NativeOnlineScreen::JoinPrompt);
        runtime
            .events
            .push_back(OnlineLobbyEvent::JoinRequested(intent));
        let mut application = NativeOnlineApplication::default();
        application.pump(&mut runtime, 101).unwrap();
        assert!(application.request_online_focus);
        application
            .dispatch(&mut runtime, NativeOnlineUiAction::AcceptJoin, 102)
            .unwrap();
        assert!(application.pending_join.is_none());
        assert!(matches!(
            runtime.commands.last(),
            Some(RecordedCommand::Join {
                intent: recorded,
                seats: 1,
                ..
            }) if *recorded == intent
        ));
        assert_eq!(
            application.dispatch(&mut runtime, NativeOnlineUiAction::AcceptJoin, 103),
            Err(NativeOnlineApplicationError::MissingJoinIntent)
        );
    }

    #[test]
    fn lobby_loadout_edit_increments_revision_clears_ready_and_supports_couch_seats() {
        let mut runtime = FakeRuntime::available();
        runtime.view = view_for(NativeOnlineScreen::Lobby);
        runtime.view.role = Some(OnlineLobbyRole::Client);
        runtime.view.local_ready = true;
        let mut application = NativeOnlineApplication::default();
        application
            .dispatch(&mut runtime, NativeOnlineUiAction::AddSeat, 50)
            .unwrap();
        assert!(matches!(
            runtime.commands.last(),
            Some(RecordedCommand::SetDeclaration {
                revision: 2,
                ready: false,
                seats: 2,
            })
        ));
        application
            .dispatch(&mut runtime, NativeOnlineUiAction::ToggleReady, 51)
            .unwrap();
        assert_eq!(
            runtime.commands.last(),
            Some(&RecordedCommand::SetReady(true))
        );
    }

    #[test]
    fn rejected_lobby_edit_keeps_editor_revision_and_selection_transactional() {
        let mut runtime = FakeRuntime::available();
        runtime.view = view_for(NativeOnlineScreen::Lobby);
        runtime.view.role = Some(OnlineLobbyRole::Client);
        runtime.view.total_seats = 1;
        runtime.view.local_seats = 1;
        runtime.reject_next_declaration = true;
        let mut application = NativeOnlineApplication::default();
        let before = application.editor;

        assert_eq!(
            application.dispatch(&mut runtime, NativeOnlineUiAction::AddSeat, 60),
            Err(NativeOnlineApplicationError::Runtime)
        );
        assert_eq!(application.editor, before);
        assert!(runtime.commands.is_empty());

        application
            .dispatch(&mut runtime, NativeOnlineUiAction::AddSeat, 61)
            .unwrap();
        assert_eq!(application.editor.revision, before.revision + 1);
        assert_eq!(application.editor.seat_count, before.seat_count + 1);
        assert!(matches!(
            runtime.commands.last(),
            Some(RecordedCommand::SetDeclaration {
                revision: 2,
                ready: false,
                seats: 2,
            })
        ));
    }

    #[test]
    fn full_lobby_rejects_add_seat_without_mutating_editor_or_runtime() {
        let mut runtime = FakeRuntime::available();
        runtime.view = view_for(NativeOnlineScreen::Lobby);
        runtime.view.role = Some(OnlineLobbyRole::Client);
        runtime.view.total_seats = ONLINE_SEAT_CAPACITY;
        runtime.view.local_seats = 1;
        let mut application = NativeOnlineApplication::default();
        let before = application.editor;

        assert_eq!(
            application.dispatch(&mut runtime, NativeOnlineUiAction::AddSeat, 70),
            Err(NativeOnlineApplicationError::InvalidAction)
        );
        assert_eq!(application.editor, before);
        assert!(runtime.commands.is_empty());
    }

    #[test]
    fn owner_commit_is_casual_listen_and_never_trusted_or_dedicated() {
        let mut runtime = FakeRuntime::available();
        runtime.view = view_for(NativeOnlineScreen::Lobby);
        runtime.view.role = Some(OnlineLobbyRole::ListenAuthority);
        runtime.view.all_members_ready = true;
        runtime.view.input_delay_calibration = ready_input_delay_calibration(120);
        let mut application = NativeOnlineApplication::default();
        application.ensure_local_peer(&runtime).unwrap();
        application
            .dispatch(&mut runtime, NativeOnlineUiAction::StartMatch, 5_000)
            .unwrap();
        let Some(RecordedCommand::Commit(options)) = runtime.commands.last().copied() else {
            panic!("expected manifest commit")
        };
        assert_eq!(
            options.authority,
            crate::network_protocol::AuthorityKind::Listen
        );
        assert_eq!(options.authority_peer, application.local_peer_id);
        assert!(!options.trusted_results);
        assert_eq!(
            options.agreed_start_tick,
            SimTick(ONLINE_COUNTDOWN_LEAD_TICKS)
        );
        assert_eq!(options.input_delay_ticks, 5);
    }

    #[test]
    fn native_online_ui_queries_initialize_without_aliasing_mutable_components() {
        let mut app = App::new();
        app.init_resource::<NativeOnlineUiSnapshot>()
            .init_resource::<MatchPresentationPolicy>()
            .add_systems(
                Update,
                (update_native_online_ui, update_native_online_button_styles).chain(),
            );
        app.update();
    }

    #[test]
    fn offline_input_condition_is_send_safe_and_tracks_active_session_projection() {
        let condition = bevy::ecs::system::IntoSystem::into_system(offline_local_input_enabled);
        assert!(
            bevy::ecs::system::System::is_send(&condition),
            "run conditions may be evaluated on a compute worker"
        );

        let mut app = App::new();
        app.init_resource::<NativeOnlineUiSnapshot>()
            .init_resource::<OfflineSamplerRunCount>()
            .add_systems(
                Update,
                count_offline_sampler_runs.run_if(offline_local_input_enabled),
            );
        app.update();
        assert_eq!(app.world().resource::<OfflineSamplerRunCount>().0, 1);

        app.world_mut()
            .resource_mut::<NativeOnlineUiSnapshot>()
            .session_kind = Some(NativeOnlineSessionKind::ListenOwner);
        app.update();
        assert_eq!(app.world().resource::<OfflineSamplerRunCount>().0, 1);

        app.world_mut()
            .resource_mut::<NativeOnlineUiSnapshot>()
            .session_kind = None;
        app.update();
        assert_eq!(app.world().resource::<OfflineSamplerRunCount>().0, 2);
    }

    #[test]
    fn window_close_requests_app_exit_join_workers_and_drop_non_send_owners_on_main_thread() {
        let (application, _fake_runtime, _transport) = active_remote_failure_fixture();
        let mut app = App::new();
        app.add_plugins(bevy::window::WindowPlugin::default())
            .insert_non_send_resource(NativeOnlineRuntime::default())
            .insert_non_send_resource(application)
            .add_systems(Last, teardown_native_online_on_exit);

        let window = {
            let world = app.world_mut();
            let mut windows = world.query_filtered::<Entity, With<bevy::window::PrimaryWindow>>();
            windows.single(world).unwrap()
        };
        app.world_mut()
            .resource_mut::<bevy::ecs::message::Messages<bevy::window::WindowCloseRequested>>()
            .write(bevy::window::WindowCloseRequested { window });

        app.update();
        assert_eq!(app.should_exit(), None);
        assert!(
            app.world()
                .get_non_send_resource::<NativeOnlineApplication>()
                .unwrap()
                .active
                .is_some()
        );

        app.update();
        assert_eq!(app.should_exit(), Some(AppExit::Success));
        let application = app
            .world()
            .get_non_send_resource::<NativeOnlineApplication>()
            .unwrap();
        assert!(application.active.is_none());
        assert_eq!(application.metrics.sessions_stopped, 1);
        assert!(application.staged_endpoints.is_empty());
        assert!(application.pending_authority_commands.is_empty());
        assert!(
            app.world()
                .get_non_send_resource::<NativeOnlineRuntime>()
                .is_some()
        );

        let application = app
            .world_mut()
            .remove_non_send_resource::<NativeOnlineApplication>()
            .unwrap();
        let runtime = app
            .world_mut()
            .remove_non_send_resource::<NativeOnlineRuntime>()
            .unwrap();
        drop(application);
        drop(runtime);
        drop(app);
    }

    #[test]
    fn listen_host_removes_only_rejected_remote_binding_before_match_start() {
        let host = AuthenticatedUserId::new(94_001).unwrap();
        let rejected_user = SteamUserId::new(94_002).unwrap();
        let valid_user = SteamUserId::new(94_003).unwrap();
        let rejected_peer = PeerId::new(53_002).unwrap();
        let valid_peer = PeerId::new(53_003).unwrap();
        let mut runtime = FakeRuntime::available_as(host);
        runtime.view = view_for(NativeOnlineScreen::Lobby);
        runtime.view.role = Some(OnlineLobbyRole::ListenAuthority);
        let mut application = NativeOnlineApplication::default();
        application
            .install_peer_binding(rejected_user, rejected_peer)
            .unwrap();
        application
            .install_peer_binding(valid_user, valid_peer)
            .unwrap();
        let failure = OnlineFailure {
            code: OnlineFailureCode::MalformedTraffic,
            severity: OnlineFailureSeverity::Fatal,
            recovery: OnlineRecoveryAction::ReturnToLobby,
            detail_code: 0,
        };
        runtime
            .events
            .push_back(OnlineLobbyEvent::PeerAuthenticationRejected {
                user: rejected_user,
                connection: None,
                failure,
            });

        application.pump(&mut runtime, 1).unwrap();

        assert!(application.failure_override.is_none());
        assert!(
            application.bindings.iter().flatten().all(|binding| {
                binding.user != rejected_user && binding.peer_id != rejected_peer
            })
        );
        assert!(
            application
                .bindings
                .iter()
                .flatten()
                .any(|binding| binding.user == valid_user && binding.peer_id == valid_peer)
        );
        let snapshot = application.ui_snapshot(&runtime, true);
        assert_eq!(snapshot.screen, NativeOnlineScreen::Lobby);
        assert_eq!(snapshot.failure, None);
    }

    #[test]
    fn client_owner_auth_rejection_clears_authority_handoffs_before_failure() {
        let owner = SteamUserId::new(92_001).unwrap();
        let owner_peer = PeerId::new(51_001).unwrap();
        let (mut application, mut runtime, _transport) = active_remote_failure_fixture();
        let owner_connection = application
            .staged_endpoints
            .front()
            .expect("failure fixture retains a Steam endpoint")
            .admitted
            .connection;
        application.install_peer_binding(owner, owner_peer).unwrap();
        application.remote_quality_user = Some(owner);
        assert!(!application.staged_endpoints.is_empty());
        assert!(!application.pending_authority_commands.is_empty());
        let failure = OnlineFailure {
            code: OnlineFailureCode::AuthenticationFailed,
            severity: OnlineFailureSeverity::Fatal,
            recovery: OnlineRecoveryAction::ReturnToMenu,
            detail_code: 77,
        };

        application
            .handle_runtime_event(
                &mut runtime,
                OnlineLobbyEvent::PeerAuthenticationRejected {
                    user: owner,
                    connection: Some(owner_connection),
                    failure,
                },
                1,
            )
            .unwrap();

        assert_eq!(application.failure_override, Some(failure));
        assert_eq!(application.remote_quality_user, None);
        assert!(application.bindings.iter().all(Option::is_none));
        assert!(application.staged_endpoints.is_empty());
        assert!(application.pending_authority_commands.is_empty());
        assert!(application.active.is_none());
        assert!(application.release_render_world);
        assert_eq!(application.metrics.sessions_stopped, 1);
    }

    #[test]
    fn roster_barrier_reclaims_application_binding_capacity_across_member_churn() {
        let host_user = SteamUserId::new(95_001).unwrap();
        let stable_user_a = SteamUserId::new(95_002).unwrap();
        let stable_user_b = SteamUserId::new(95_003).unwrap();
        let host_peer = PeerId::new(54_001).unwrap();
        let stable_peer_a = PeerId::new(54_002).unwrap();
        let stable_peer_b = PeerId::new(54_003).unwrap();
        let live_bindings = [
            Some(OnlinePeerIdentity {
                user: host_user,
                peer_id: host_peer,
            }),
            Some(OnlinePeerIdentity {
                user: stable_user_a,
                peer_id: stable_peer_a,
            }),
            Some(OnlinePeerIdentity {
                user: stable_user_b,
                peer_id: stable_peer_b,
            }),
            None,
        ];
        let mut runtime = FakeRuntime::available_as(host_user.authenticated());
        runtime.view = view_for(NativeOnlineScreen::Lobby);
        runtime.view.role = Some(OnlineLobbyRole::ListenAuthority);
        let mut application = NativeOnlineApplication::default();
        for identity in live_bindings.iter().flatten().copied() {
            application
                .install_peer_binding(identity.user, identity.peer_id)
                .unwrap();
        }

        for ordinal in 0..(MAX_STEAM_LOBBY_MEMBERS * 2 + 1) {
            let departed_user = SteamUserId::new(96_000 + ordinal as u64).unwrap();
            let departed_peer = PeerId::new(55_000).unwrap();
            application
                .install_peer_binding(departed_user, departed_peer)
                .unwrap();
            application.remote_quality_user = Some(departed_user);
            let (staged_guard, staged_endpoint, _staged_remote) = PumpedFakeSteamPair::admitted(
                SteamLobbyId::new(65_000 + ordinal as u64).unwrap(),
                host_user,
                departed_user,
                host_peer,
                departed_peer,
                200_000 + ordinal as u64 * 1_000,
            );
            application.staged_endpoints.push_back(staged_endpoint);
            let (command_guard, command_endpoint, _command_remote) = PumpedFakeSteamPair::admitted(
                SteamLobbyId::new(66_000 + ordinal as u64).unwrap(),
                host_user,
                departed_user,
                host_peer,
                departed_peer,
                300_000 + ordinal as u64 * 1_000,
            );
            application.pending_authority_commands.push_back(
                ListenAuthorityCommand::AttachInitial {
                    peer_id: departed_peer,
                    user_id: departed_user.authenticated(),
                    endpoint: command_endpoint.admitted.endpoint.into(),
                },
            );
            runtime.events.push_back(OnlineLobbyEvent::RosterChanged {
                members: 3,
                seats: 3,
                all_ready: false,
                live_bindings,
            });

            application.pump(&mut runtime, ordinal as u64 + 1).unwrap();

            assert_eq!(application.bindings.iter().flatten().count(), 3);
            assert!(application.bindings.iter().flatten().all(|binding| {
                live_bindings.iter().flatten().any(|identity| {
                    identity.user == binding.user && identity.peer_id == binding.peer_id
                })
            }));
            assert_eq!(application.remote_quality_user, None);
            assert!(application.staged_endpoints.is_empty());
            assert!(application.pending_authority_commands.is_empty());
            assert!(application.failure_override.is_none());
            drop(staged_guard);
            drop(command_guard);
        }
    }

    #[test]
    fn remote_auth_revocation_does_not_set_listen_host_global_failure() {
        let (
            mut application,
            mut runtime,
            mut transport,
            _remote_endpoint,
            remote_user,
            remote_peer,
        ) = active_listen_security_fixture();
        let failure = OnlineFailure {
            code: OnlineFailureCode::AuthenticationFailed,
            severity: OnlineFailureSeverity::Fatal,
            recovery: OnlineRecoveryAction::ReturnToLobby,
            detail_code: 41,
        };
        let expected_steam_connection = application
            .authority_endpoints
            .iter()
            .flatten()
            .find(|record| record.peer_id == remote_peer)
            .expect("fixture retains the admitted Steam generation")
            .steam_connection;
        runtime
            .events
            .push_back(OnlineLobbyEvent::PeerAuthenticationRejected {
                user: remote_user,
                connection: Some(expected_steam_connection),
                failure,
            });
        // The coordinator's immediately following roster event is the
        // cleanup-before-reallocation barrier. Authentication revocation must
        // already have been forwarded while the user-to-peer binding existed.
        runtime.events.push_back(OnlineLobbyEvent::RosterChanged {
            members: 1,
            seats: 1,
            all_ready: true,
            live_bindings: [None; MAX_STEAM_LOBBY_MEMBERS],
        });

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            transport.pump();
            application.pump(&mut runtime, 10).unwrap();
            let removed = match &application.active {
                Some(ActiveNativeOnlineMatch::Listen(online_match)) => online_match
                    .authority
                    .status()
                    .peer(remote_peer)
                    .is_some_and(|peer| peer.connection.is_none() && peer.phase.is_none()),
                _ => false,
            };
            if removed && application.metrics.authority_terminal_marks == 1 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "authority did not revoke the rejected remote peer: status={:?}, app_metrics={:?}, endpoints={:?}, commands={:?}",
                application.active.as_ref().and_then(|active| match active {
                    ActiveNativeOnlineMatch::Listen(online_match) =>
                        Some(online_match.authority.status()),
                    ActiveNativeOnlineMatch::Remote(_) => None,
                }),
                application.metrics,
                application.authority_endpoints,
                runtime.commands,
            );
            std::thread::sleep(Duration::from_millis(1));
        }

        assert!(application.active.is_some());
        assert!(application.failure_override.is_none());
        assert_eq!(application.metrics.authentication_revocations_forwarded, 1);
        assert!(
            runtime.commands.iter().any(|command| {
                *command
                    == RecordedCommand::AuthorityTerminalDrained {
                        user: remote_user,
                        peer_id: remote_peer,
                        connection: expected_steam_connection,
                        retry: Some(RetryDisposition::Fatal),
                    }
            }),
            "unexpected terminal command set: {:?}",
            runtime.commands
        );
        assert!(
            application
                .bindings
                .iter()
                .flatten()
                .all(|binding| binding.user != remote_user)
        );
        assert_ne!(
            application.ui_snapshot(&runtime, true).screen,
            NativeOnlineScreen::Error
        );
    }

    #[test]
    fn remote_platform_ban_does_not_set_listen_host_global_failure() {
        let (
            mut application,
            mut runtime,
            mut transport,
            _remote_endpoint,
            remote_user,
            remote_peer,
        ) = active_listen_security_fixture();
        let failure = OnlineFailure {
            code: OnlineFailureCode::PlatformBanned,
            severity: OnlineFailureSeverity::Fatal,
            recovery: OnlineRecoveryAction::ReturnToLobby,
            detail_code: 42,
        };
        let expected_steam_connection = application
            .authority_endpoints
            .iter()
            .flatten()
            .find(|record| record.peer_id == remote_peer)
            .expect("fixture retains the admitted Steam generation")
            .steam_connection;
        runtime
            .events
            .push_back(OnlineLobbyEvent::PeerAuthenticationRejected {
                user: remote_user,
                connection: Some(expected_steam_connection),
                failure,
            });

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            transport.pump();
            application.pump(&mut runtime, 20).unwrap();
            let removed = match &application.active {
                Some(ActiveNativeOnlineMatch::Listen(online_match)) => online_match
                    .authority
                    .status()
                    .peer(remote_peer)
                    .is_some_and(|peer| peer.connection.is_none() && peer.phase.is_none()),
                _ => false,
            };
            if removed && application.metrics.authority_terminal_marks == 1 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "authority did not enforce the remote platform ban: status={:?}, app_metrics={:?}, endpoints={:?}, commands={:?}",
                application.active.as_ref().and_then(|active| match active {
                    ActiveNativeOnlineMatch::Listen(online_match) =>
                        Some(online_match.authority.status()),
                    ActiveNativeOnlineMatch::Remote(_) => None,
                }),
                application.metrics,
                application.authority_endpoints,
                runtime.commands,
            );
            std::thread::sleep(Duration::from_millis(1));
        }

        assert!(application.active.is_some());
        assert!(application.failure_override.is_none());
        assert_eq!(application.metrics.platform_bans_forwarded, 1);
        assert!(
            runtime.commands.iter().any(|command| {
                *command
                    == RecordedCommand::AuthorityTerminalDrained {
                        user: remote_user,
                        peer_id: remote_peer,
                        connection: expected_steam_connection,
                        retry: Some(RetryDisposition::Fatal),
                    }
            }),
            "unexpected terminal command set: {:?}",
            runtime.commands
        );
        assert!(
            application
                .bindings
                .iter()
                .flatten()
                .all(|binding| binding.user != remote_user)
        );
        assert_ne!(
            application.ui_snapshot(&runtime, true).screen,
            NativeOnlineScreen::Error
        );
    }

    #[test]
    fn two_native_applications_reconnect_confirm_rematch_and_teardown_real_online_workers() {
        let diagnostics = TemporaryDiagnosticsRoot::unique();
        let lobby = SteamLobbyId::new(60_001).unwrap();
        let host_user = SteamUserId::new(91_001).unwrap();
        let remote_user = SteamUserId::new(91_002).unwrap();
        let host_authenticated = AuthenticatedUserId::new(host_user.get()).unwrap();
        let remote_authenticated = AuthenticatedUserId::new(remote_user.get()).unwrap();
        let mut host_runtime = FakeRuntime::available_as(host_authenticated);
        let mut remote_runtime = FakeRuntime::available_as(remote_authenticated);
        let mut host_application = NativeOnlineApplication::default();
        let mut remote_application = NativeOnlineApplication::default();
        host_application.listen_diagnostics_root = Some(diagnostics.path.clone());

        // Use the committed authored stock ruleset on Crown Ring. One ordinary
        // continuous remote movement command causes three normal ring-outs; no
        // simulation/result test hook is involved.
        host_application.editor.rules = DefinitionId::new(2).unwrap();
        remote_application.editor.rules = DefinitionId::new(2).unwrap();
        host_application.editor.arena = DefinitionId::new(0).unwrap();
        remote_application.editor.arena = DefinitionId::new(0).unwrap();
        remote_application
            .dispatch(&mut remote_runtime, NativeOnlineUiAction::ToggleTeam, 1)
            .unwrap();

        host_application
            .dispatch(&mut host_runtime, NativeOnlineUiAction::CreatePrivate, 10)
            .unwrap();
        let join_intent = LobbyJoinIntent::friends_list(lobby, 11, 60_000).unwrap();
        remote_runtime.view = view_for(NativeOnlineScreen::JoinPrompt);
        remote_runtime
            .events
            .push_back(OnlineLobbyEvent::JoinRequested(join_intent));
        remote_application.pump(&mut remote_runtime, 11).unwrap();
        remote_application
            .dispatch(&mut remote_runtime, NativeOnlineUiAction::AcceptJoin, 12)
            .unwrap();
        assert_eq!(
            host_runtime.view.role,
            Some(OnlineLobbyRole::ListenAuthority)
        );
        assert_eq!(remote_runtime.view.role, Some(OnlineLobbyRole::Client));

        host_application
            .dispatch(&mut host_runtime, NativeOnlineUiAction::ToggleReady, 13)
            .unwrap();
        remote_application
            .dispatch(&mut remote_runtime, NativeOnlineUiAction::ToggleReady, 13)
            .unwrap();
        for runtime in [&mut host_runtime, &mut remote_runtime] {
            runtime.view.lobby = Some(lobby);
            runtime.view.lobby_members = 2;
            runtime.view.total_seats = 2;
            runtime.view.all_members_ready = true;
        }
        host_runtime.view.input_delay_calibration = ready_input_delay_calibration(120);
        let host_member = ready_member(
            host_runtime
                .local_declaration
                .expect("create command carries the host declaration"),
        );
        let remote_member = ready_member(
            remote_runtime
                .local_declaration
                .expect("join command carries the remote declaration"),
        );
        let host_peer = AuthenticatedPeer {
            peer_id: host_member.peer_id,
            user_id: host_authenticated,
        };
        let remote_peer = AuthenticatedPeer {
            peer_id: remote_member.peer_id,
            user_id: remote_authenticated,
        };

        host_application
            .dispatch(&mut host_runtime, NativeOnlineUiAction::StartMatch, 20)
            .unwrap();
        let first_options = host_runtime
            .committed_options
            .expect("start command commits immutable manifest options");
        let first_config = build_lifecycle_config(host_member, remote_member, first_options);
        assert_eq!(first_config.manifest.rules, DefinitionId::new(2).unwrap());
        assert_eq!(first_config.manifest.arena, DefinitionId::new(0).unwrap());
        let mut host_projection = crate::headless::build_headless_simulation(first_config.clone())
            .expect("host projection target builds");
        let mut remote_projection =
            crate::headless::build_headless_simulation(first_config.clone())
                .expect("remote projection target builds");
        let (mut first_transport, first_host_endpoint, first_remote_endpoint) =
            PumpedFakeSteamPair::admitted(
                lobby,
                host_user,
                remote_user,
                host_peer.peer_id,
                remote_peer.peer_id,
                100_000,
            );
        let first_connection = first_host_endpoint.admitted.connection;
        stage_lifecycle_session(
            &mut host_runtime,
            &mut remote_runtime,
            first_config.clone(),
            host_peer,
            remote_peer,
            host_user,
            remote_user,
            first_host_endpoint,
            first_remote_endpoint,
            21,
        );
        host_application.set_content_ready(true);
        remote_application.set_content_ready(true);
        let mut now_ms = 22;
        drive_lifecycle_to_fighting(
            &mut host_application,
            &mut host_runtime,
            &mut remote_application,
            &mut remote_runtime,
            &mut first_transport,
            host_projection.world_mut(),
            remote_projection.world_mut(),
            &mut now_ms,
        );
        assert_eq!(
            host_application.active_session_kind(),
            Some(NativeOnlineSessionKind::ListenOwner)
        );
        assert_eq!(
            remote_application.active_session_kind(),
            Some(NativeOnlineSessionKind::RemoteClient)
        );
        assert_eq!(host_runtime.lifecycle.content_loaded, 1);
        assert_eq!(remote_runtime.lifecycle.content_loaded, 1);
        assert_eq!(host_runtime.lifecycle.initial_sync_completed, 1);
        assert_eq!(remote_runtime.lifecycle.initial_sync_completed, 1);
        assert_eq!(host_runtime.lifecycle.countdowns_begun, 1);
        assert_eq!(remote_runtime.lifecycle.countdowns_begun, 1);
        assert_eq!(host_runtime.lifecycle.fighting_marked, 1);
        assert_eq!(remote_runtime.lifecycle.fighting_marked, 1);

        host_runtime
            .events
            .push_back(OnlineLobbyEvent::PeerDisconnected {
                connection: first_connection,
                user: remote_user,
                peer_id: remote_peer.peer_id,
                reconnect_allowed: true,
            });
        remote_runtime
            .events
            .push_back(OnlineLobbyEvent::PeerDisconnected {
                connection: first_connection,
                user: host_user,
                peer_id: host_peer.peer_id,
                reconnect_allowed: true,
            });
        remote_runtime.begin_reconnect();
        host_application.pump(&mut host_runtime, now_ms).unwrap();
        remote_application
            .pump(&mut remote_runtime, now_ms)
            .unwrap();
        drop(first_transport);

        let (mut first_transport, first_reconnect_host_endpoint, first_reconnect_remote_endpoint) =
            PumpedFakeSteamPair::admitted_for_purpose(
                lobby,
                host_user,
                remote_user,
                host_peer.peer_id,
                remote_peer.peer_id,
                AdmissionPurpose::Reconnect,
                150_000,
            );
        let first_reconnect_host_connection = first_reconnect_host_endpoint.admitted.connection;
        host_runtime
            .events
            .push_back(OnlineLobbyEvent::PeerAuthenticated {
                user: remote_user,
                peer_id: remote_peer.peer_id,
                reconnect: true,
            });
        remote_runtime
            .events
            .push_back(OnlineLobbyEvent::PeerAuthenticated {
                user: host_user,
                peer_id: host_peer.peer_id,
                reconnect: true,
            });
        host_runtime
            .endpoints
            .push_back(first_reconnect_host_endpoint);
        remote_runtime
            .endpoints
            .push_back(first_reconnect_remote_endpoint);
        remote_runtime.set_screen(NativeOnlineScreen::Loading);

        let reconnect_deadline = Instant::now() + Duration::from_secs(10);
        loop {
            pump_lifecycle_frame(
                &mut host_application,
                &mut host_runtime,
                &mut remote_application,
                &mut remote_runtime,
                &mut first_transport,
                host_projection.world_mut(),
                remote_projection.world_mut(),
                &mut now_ms,
            );
            let remote_phase = remote_application
                .active
                .as_ref()
                .map(|active| active.client().status().phase);
            if remote_phase == Some(RemoteOnlineClientPhase::Fighting) {
                break;
            }
            assert!(
                Instant::now() < reconnect_deadline,
                "replacement worker did not apply reconnect snapshot: remote={remote_phase:?}/{:?}",
                remote_application
                    .active
                    .as_ref()
                    .and_then(|active| active.client().terminal())
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(remote_runtime.view.screen, NativeOnlineScreen::Fighting);
        assert!(remote_application.coordinator_content_marked);
        assert!(remote_application.initial_sync_marked);
        assert_eq!(host_runtime.lifecycle.manifests_accepted, 1);
        assert_eq!(remote_runtime.lifecycle.manifests_accepted, 1);
        assert_eq!(host_runtime.lifecycle.content_loaded, 1);
        assert_eq!(remote_runtime.lifecycle.content_loaded, 1);
        assert_eq!(host_runtime.lifecycle.initial_sync_completed, 1);
        assert_eq!(remote_runtime.lifecycle.initial_sync_completed, 2);
        assert_eq!(host_runtime.lifecycle.countdowns_begun, 1);
        assert_eq!(remote_runtime.lifecycle.countdowns_begun, 1);
        assert_eq!(host_runtime.lifecycle.fighting_marked, 1);
        assert_eq!(remote_runtime.lifecycle.fighting_marked, 1);

        let (host_result, remote_result, host_worker_metrics, remote_worker_metrics) =
            drive_lifecycle_to_confirmed_result(
                &mut host_application,
                &mut host_runtime,
                &mut remote_application,
                &mut remote_runtime,
                &mut first_transport,
                host_projection.world_mut(),
                remote_projection.world_mut(),
                &mut now_ms,
            );
        assert_eq!(host_result, remote_result);
        assert!(host_result.final_tick > first_config.manifest.agreed_start_tick);
        assert_ne!(host_result.result_id, 0);
        assert!(host_worker_metrics.input_commands_submitted > 0);
        assert!(remote_worker_metrics.input_commands_submitted > 0);
        assert!(host_worker_metrics.input_ticks_submitted > 0);
        assert!(remote_worker_metrics.input_ticks_submitted > 0);
        assert_eq!(
            host_runtime.view.outcome,
            Some(OnlineMatchOutcome::Confirmed)
        );
        assert_eq!(
            remote_runtime.view.outcome,
            Some(OnlineMatchOutcome::Confirmed)
        );
        assert_eq!(host_runtime.lifecycle.result_confirmations_begun, 1);
        assert_eq!(remote_runtime.lifecycle.result_confirmations_begun, 1);
        assert_eq!(host_runtime.lifecycle.results_confirmed, 1);
        assert_eq!(remote_runtime.lifecycle.results_confirmed, 1);

        // Results workers retain both Steam endpoint owners. Pump well beyond
        // the transport's 50 ms endpoint-drop quiet window: neither side may
        // manufacture EndpointDropped merely because Completed was published.
        let host_iterations_at_result = host_worker_metrics.worker_iterations;
        let remote_iterations_at_result = remote_worker_metrics.worker_iterations;
        for _ in 0..80 {
            pump_lifecycle_frame(
                &mut host_application,
                &mut host_runtime,
                &mut remote_application,
                &mut remote_runtime,
                &mut first_transport,
                host_projection.world_mut(),
                remote_projection.world_mut(),
                &mut now_ms,
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            first_transport.host.metrics().endpoint_drop_drains_started,
            0
        );
        assert_eq!(
            first_transport
                .remote
                .metrics()
                .endpoint_drop_drains_started,
            0
        );
        for (application, runtime, prior_iterations) in [
            (&host_application, &host_runtime, host_iterations_at_result),
            (
                &remote_application,
                &remote_runtime,
                remote_iterations_at_result,
            ),
        ] {
            let client = application.active.as_ref().unwrap().client();
            assert_eq!(client.status().phase, RemoteOnlineClientPhase::Results);
            assert!(client.metrics().worker_iterations > prior_iterations);
            assert!(matches!(
                client.terminal(),
                Some(RemoteOnlineTerminal::Completed(_))
            ));
            assert_eq!(runtime.view.screen, NativeOnlineScreen::Results);
            assert_eq!(runtime.view.failure, None);
        }

        // The owner may act first. Its authority endpoint then drains and
        // closes, but the already-confirmed remote remains in Results until its
        // own between-match transition is observed/selected.
        host_application
            .dispatch(&mut host_runtime, NativeOnlineUiAction::Rematch, now_ms)
            .unwrap();
        assert!(host_application.active.is_some());
        assert_eq!(
            host_application.listen_shutdown,
            Some(ListenShutdownAction::Rematch)
        );
        let draining_snapshot = host_application.ui_snapshot(&host_runtime, true);
        assert_eq!(
            draining_snapshot.screen,
            NativeOnlineScreen::ReturningToLobby
        );
        assert_eq!(draining_snapshot.actions, NativeOnlineActions::default());
        drive_listen_shutdown_to_completion(
            &mut host_application,
            &mut host_runtime,
            &mut remote_application,
            &mut remote_runtime,
            &mut first_transport,
            &mut now_ms,
        );
        assert!(host_runtime.commands.iter().any(|command| {
            *command
                == RecordedCommand::AuthorityTerminalDrained {
                    user: remote_user,
                    peer_id: remote_peer.peer_id,
                    connection: first_reconnect_host_connection,
                    retry: None,
                }
        }));
        assert_eq!(
            host_runtime
                .commands
                .iter()
                .filter(|command| matches!(
                    command,
                    RecordedCommand::AuthorityTerminalDrained { .. }
                ))
                .count(),
            1
        );
        for _ in 0..80 {
            first_transport.pump();
            remote_application
                .pump(&mut remote_runtime, now_ms)
                .unwrap();
            remote_application
                .project_latest(remote_projection.world_mut())
                .unwrap();
            now_ms = now_ms.saturating_add(1);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(first_transport.host.metrics().endpoint_drop_drains_started > 0);
        assert!(
            first_transport
                .host
                .metrics()
                .endpoint_drop_drains_quiet_completed
                > 0
        );
        let remote_after_owner_return = remote_application
            .active
            .as_ref()
            .expect("remote retains confirmed Results until its safe reset")
            .client();
        assert_eq!(
            remote_after_owner_return.status().phase,
            RemoteOnlineClientPhase::Results
        );
        assert_eq!(
            remote_after_owner_return.terminal(),
            Some(RemoteOnlineTerminal::Completed(remote_result))
        );
        assert_eq!(remote_runtime.view.screen, NativeOnlineScreen::Results);
        assert_eq!(
            remote_runtime.view.outcome,
            Some(OnlineMatchOutcome::Confirmed)
        );
        assert_eq!(remote_runtime.view.failure, None);

        remote_application
            .dispatch(&mut remote_runtime, NativeOnlineUiAction::Rematch, now_ms)
            .unwrap();
        assert!(host_application.active.is_none());
        assert!(remote_application.active.is_none());
        assert!(host_application.pending_authority_commands.is_empty());
        assert!(remote_application.pending_authority_commands.is_empty());
        assert!(host_application.staged_endpoints.is_empty());
        assert!(remote_application.staged_endpoints.is_empty());
        assert!(host_application.release_render_world);
        assert!(remote_application.release_render_world);
        assert_eq!(host_application.metrics.sessions_stopped, 1);
        assert_eq!(remote_application.metrics.sessions_stopped, 1);
        assert!(
            diagnostics.path.exists(),
            "joining the listen worker persists terminal diagnostics under the per-test root"
        );
        assert!(!host_application.coordinator_content_marked);
        assert!(!remote_application.coordinator_content_marked);
        assert!(!host_application.initial_sync_marked);
        assert!(!remote_application.initial_sync_marked);
        assert!(!host_application.result_confirmation_started);
        assert!(!remote_application.result_confirmation_started);
        assert!(!host_application.result_confirmed);
        assert!(!remote_application.result_confirmed);
        host_application.release_render_world = false;
        remote_application.release_render_world = false;
        drop(first_transport);

        host_application
            .dispatch(
                &mut host_runtime,
                NativeOnlineUiAction::StartMatch,
                now_ms.saturating_add(1),
            )
            .unwrap();
        let second_options = host_runtime
            .committed_options
            .expect("rematch commits fresh manifest options");
        assert_ne!(first_options.match_id, second_options.match_id);
        let second_config = build_lifecycle_config(host_member, remote_member, second_options);
        let (mut second_transport, second_host_endpoint, second_remote_endpoint) =
            PumpedFakeSteamPair::admitted(
                lobby,
                host_user,
                remote_user,
                host_peer.peer_id,
                remote_peer.peer_id,
                200_000,
            );
        let second_host_connection = second_host_endpoint.admitted.connection;
        stage_lifecycle_session(
            &mut host_runtime,
            &mut remote_runtime,
            second_config,
            host_peer,
            remote_peer,
            host_user,
            remote_user,
            second_host_endpoint,
            second_remote_endpoint,
            now_ms.saturating_add(2),
        );
        now_ms = now_ms.saturating_add(3);
        drive_lifecycle_to_fighting(
            &mut host_application,
            &mut host_runtime,
            &mut remote_application,
            &mut remote_runtime,
            &mut second_transport,
            host_projection.world_mut(),
            remote_projection.world_mut(),
            &mut now_ms,
        );
        assert_eq!(host_application.metrics.sessions_started, 2);
        assert_eq!(remote_application.metrics.sessions_started, 2);

        host_application
            .dispatch(
                &mut host_runtime,
                NativeOnlineUiAction::ReturnToLobby,
                now_ms,
            )
            .unwrap();
        remote_application
            .dispatch(
                &mut remote_runtime,
                NativeOnlineUiAction::ReturnToLobby,
                now_ms,
            )
            .unwrap();
        assert!(host_application.active.is_some());
        assert!(remote_application.active.is_none());
        drive_listen_shutdown_to_completion(
            &mut host_application,
            &mut host_runtime,
            &mut remote_application,
            &mut remote_runtime,
            &mut second_transport,
            &mut now_ms,
        );
        assert!(host_runtime.commands.iter().any(|command| {
            *command
                == RecordedCommand::AuthorityTerminalDrained {
                    user: remote_user,
                    peer_id: remote_peer.peer_id,
                    connection: second_host_connection,
                    retry: Some(RetryDisposition::MatchEndedNoContest),
                }
        }));
        assert_eq!(host_application.metrics.sessions_stopped, 2);
        assert_eq!(remote_application.metrics.sessions_stopped, 2);
        assert!(host_application.release_render_world);
        assert!(remote_application.release_render_world);

        host_application
            .dispatch(
                &mut host_runtime,
                NativeOnlineUiAction::LeaveOnline,
                now_ms.saturating_add(1),
            )
            .unwrap();
        remote_application
            .dispatch(
                &mut remote_runtime,
                NativeOnlineUiAction::LeaveOnline,
                now_ms.saturating_add(1),
            )
            .unwrap();
        assert_eq!(
            host_runtime.lifecycle,
            LifecycleCommandCounts {
                manifests_accepted: 2,
                content_loaded: 2,
                initial_sync_completed: 2,
                countdowns_begun: 2,
                fighting_marked: 2,
                result_confirmations_begun: 1,
                results_confirmed: 1,
                rematches: 1,
                returns_to_lobby: 1,
                leaves: 1,
            }
        );
        assert_eq!(
            remote_runtime.lifecycle,
            LifecycleCommandCounts {
                initial_sync_completed: 3,
                ..host_runtime.lifecycle
            }
        );
        assert_eq!(
            host_application.metrics(),
            NativeOnlineApplicationMetrics {
                runtime_events: 4,
                endpoints_staged: 3,
                sessions_started: 2,
                sessions_stopped: 2,
                conservative_full_resyncs: 1,
                authority_terminal_marks: 2,
                graceful_shutdowns_started: 2,
                graceful_shutdowns_completed: 2,
                ..default()
            }
        );
        assert_eq!(
            remote_application.metrics(),
            NativeOnlineApplicationMetrics {
                runtime_events: 5,
                endpoints_staged: 3,
                sessions_started: 2,
                sessions_stopped: 2,
                ..default()
            }
        );
        assert!(host_application.active.is_none());
        assert!(remote_application.active.is_none());
        assert!(host_application.staged_endpoints.is_empty());
        assert!(remote_application.staged_endpoints.is_empty());
        assert!(host_application.pending_authority_commands.is_empty());
        assert!(remote_application.pending_authority_commands.is_empty());
        assert!(host_application.local_peer_id.is_none());
        assert!(remote_application.local_peer_id.is_none());
        assert!(host_application.bindings.iter().all(Option::is_none));
        assert!(remote_application.bindings.iter().all(Option::is_none));
        assert_eq!(host_runtime.view.screen, NativeOnlineScreen::OnlineMenu);
        assert_eq!(remote_runtime.view.screen, NativeOnlineScreen::OnlineMenu);
        drop(second_transport);
    }

    #[test]
    fn steam_quality_forwards_only_to_the_remote_clients_owner_link() {
        let owner = SteamUserId::new(92_001).unwrap();
        let unrelated = SteamUserId::new(92_003).unwrap();
        let (mut remote_application, mut remote_runtime, _remote_transport) =
            active_remote_failure_fixture();
        remote_application.remote_quality_user = Some(owner);
        let submitted = |application: &NativeOnlineApplication| match &application.active {
            Some(ActiveNativeOnlineMatch::Remote(client)) => {
                client.metrics().quality_commands_submitted
            }
            _ => panic!("remote quality fixture must own a remote client"),
        };
        let baseline = submitted(&remote_application);
        let quality = NetworkQualitySnapshot {
            quality: NetworkQuality::Degraded,
            sample_count: 3,
            average_rtt_ms: 180,
            average_loss_bps: 350,
            peak_rtt_ms: 220,
            peak_loss_bps: 500,
        };
        remote_application
            .handle_runtime_event(
                &mut remote_runtime,
                OnlineLobbyEvent::QualityChanged {
                    user: unrelated,
                    quality,
                },
                1,
            )
            .unwrap();
        assert_eq!(submitted(&remote_application), baseline);
        remote_application
            .handle_runtime_event(
                &mut remote_runtime,
                OnlineLobbyEvent::QualityChanged {
                    user: owner,
                    quality,
                },
                2,
            )
            .unwrap();
        assert_eq!(submitted(&remote_application), baseline + 1);
        remote_application.clear_active_match();

        let (
            mut listen_application,
            mut listen_runtime,
            mut listen_transport,
            remote_endpoint,
            remote_user,
            _remote_peer,
        ) = active_listen_security_fixture();
        let listen_baseline = match &listen_application.active {
            Some(ActiveNativeOnlineMatch::Listen(online_match)) => {
                online_match
                    .host_client
                    .metrics()
                    .quality_commands_submitted
            }
            _ => panic!("listen quality fixture must own a listen match"),
        };
        listen_application
            .handle_runtime_event(
                &mut listen_runtime,
                OnlineLobbyEvent::QualityChanged {
                    user: remote_user,
                    quality,
                },
                3,
            )
            .unwrap();
        let listen_submitted = match &listen_application.active {
            Some(ActiveNativeOnlineMatch::Listen(online_match)) => {
                online_match
                    .host_client
                    .metrics()
                    .quality_commands_submitted
            }
            _ => panic!("listen quality fixture must remain active"),
        };
        assert_eq!(listen_submitted, listen_baseline);
        listen_application.clear_active_match();
        drop(remote_endpoint);
        listen_transport.pump();
    }

    #[test]
    fn fatal_terminal_worker_synchronously_clears_match_before_error_recovery() {
        let (mut application, mut runtime, _transport) = active_remote_failure_fixture();
        let recoverable = OnlineFailure {
            code: OnlineFailureCode::ConnectionTimedOut,
            severity: OnlineFailureSeverity::Recoverable,
            recovery: OnlineRecoveryAction::Reconnect,
            detail_code: 61,
        };
        application.observe_terminal_worker(
            Some(RemoteOnlineTerminal::Failed(recoverable)),
            NativeOnlineScreen::Reconnecting,
        );
        assert!(application.active.is_some());
        assert!(application.failure_override.is_none());

        let fatal = OnlineFailure {
            code: OnlineFailureCode::SynchronizationFailed,
            severity: OnlineFailureSeverity::Fatal,
            recovery: OnlineRecoveryAction::ReturnToLobby,
            detail_code: 62,
        };
        application.observe_terminal_worker(
            Some(RemoteOnlineTerminal::Failed(fatal)),
            NativeOnlineScreen::Reconnecting,
        );

        assert!(application.active.is_none());
        assert!(application.staged_endpoints.is_empty());
        assert!(application.pending_authority_commands.is_empty());
        assert!(application.release_render_world);
        assert_eq!(application.metrics.sessions_stopped, 1);
        assert_eq!(application.failure_override, Some(fatal));
        let snapshot = application.ui_snapshot(&runtime, true);
        assert_eq!(snapshot.screen, NativeOnlineScreen::Error);
        assert_eq!(snapshot.failure, Some(fatal));

        application
            .dispatch(&mut runtime, NativeOnlineUiAction::ReturnToMenu, 1)
            .unwrap();
        assert!(application.active.is_none());
        assert_eq!(application.metrics.sessions_stopped, 1);
        assert!(application.failure_override.is_none());
        assert!(application.request_leave_user_mode);
    }

    fn disconnect_for_active_remote(
        application: &NativeOnlineApplication,
        retry: RetryDisposition,
        detail_code: u16,
    ) -> RemoteAuthorityDisconnect {
        let Some(ActiveNativeOnlineMatch::Remote(client)) = application.active.as_ref() else {
            panic!("typed disconnect fixture requires a remote client");
        };
        RemoteAuthorityDisconnect {
            generation: client.generation(),
            message: crate::network_protocol::DisconnectMessage {
                match_id: Some(client.manifest().match_id),
                code: crate::network_protocol::DisconnectCode::ServerShutdown,
                retry,
                detail_code,
                last_confirmed_tick: Some(SimTick(700)),
            },
            local_confirmed_tick: Some(SimTick(650)),
        }
    }

    #[test]
    fn typed_authority_disconnect_recovery_matrix_controls_worker_and_ui_actions() {
        for (retry, expected_screen, retain_worker) in [
            (
                RetryDisposition::ReconnectAllowed,
                NativeOnlineScreen::Reconnecting,
                true,
            ),
            (
                RetryDisposition::ReturnToLobby,
                NativeOnlineScreen::Error,
                false,
            ),
            (
                RetryDisposition::MatchEndedNoContest,
                NativeOnlineScreen::Results,
                false,
            ),
            (RetryDisposition::Fatal, NativeOnlineScreen::Error, false),
        ] {
            let (mut application, mut runtime, _transport) = active_remote_failure_fixture();
            let disconnect = disconnect_for_active_remote(&application, retry, 0xAFC);
            application
                .apply_authority_disconnect(&mut runtime, disconnect, 1)
                .unwrap();
            assert_eq!(application.active.is_some(), retain_worker, "{retry:?}");
            assert_eq!(application.authority_disconnect, Some(disconnect));

            let snapshot = application.ui_snapshot(&runtime, true);
            assert_eq!(snapshot.screen, expected_screen, "{retry:?}");
            assert_eq!(snapshot.authority_disconnect, Some(disconnect));
            assert_eq!(
                snapshot.failure,
                Some(OnlineFailure::from_disconnect(disconnect.message))
            );
            assert_eq!(
                native_online_action_available(&snapshot, NativeOnlineUiAction::Retry),
                false,
                "{retry:?}"
            );
            assert_eq!(
                snapshot.actions.return_to_lobby,
                matches!(
                    retry,
                    RetryDisposition::ReturnToLobby | RetryDisposition::MatchEndedNoContest
                ),
                "{retry:?}"
            );
            assert_eq!(
                snapshot.actions.return_to_menu,
                retry == RetryDisposition::Fatal,
                "{retry:?}"
            );
            assert_eq!(
                native_online_back_action(&snapshot),
                match retry {
                    RetryDisposition::ReconnectAllowed => NativeOnlineUiAction::RequestLeave,
                    RetryDisposition::ReturnToLobby | RetryDisposition::MatchEndedNoContest => {
                        NativeOnlineUiAction::ReturnToLobby
                    }
                    RetryDisposition::Fatal => NativeOnlineUiAction::ReturnToMenu,
                },
                "{retry:?}"
            );
            assert!(
                !native_online_details(&snapshot)
                    .contains(&disconnect.message.detail_code.to_string())
            );
            assert!(
                !native_online_details(&snapshot).contains(
                    &disconnect
                        .message
                        .last_confirmed_tick
                        .unwrap()
                        .get()
                        .to_string()
                )
            );
        }
    }

    #[test]
    fn typed_authority_disconnect_is_match_generation_bound_first_wins_and_order_independent() {
        let generic = OnlineFailure {
            code: OnlineFailureCode::ConnectionTimedOut,
            severity: OnlineFailureSeverity::Recoverable,
            recovery: OnlineRecoveryAction::Reconnect,
            detail_code: 44,
        };

        for generic_first in [false, true] {
            let (mut application, mut runtime, _transport) = active_remote_failure_fixture();
            let closed_connection = application
                .staged_endpoints
                .front()
                .expect("failure fixture retains a Steam endpoint")
                .admitted
                .connection;
            let disconnect =
                disconnect_for_active_remote(&application, RetryDisposition::ReconnectAllowed, 91);
            if generic_first {
                application.observe_terminal_worker(
                    Some(RemoteOnlineTerminal::Failed(generic)),
                    NativeOnlineScreen::Fighting,
                );
                application
                    .handle_runtime_event(
                        &mut runtime,
                        OnlineLobbyEvent::PeerDisconnected {
                            connection: closed_connection,
                            user: SteamUserId::new(92_001).unwrap(),
                            peer_id: PeerId::new(51_001).unwrap(),
                            reconnect_allowed: true,
                        },
                        1,
                    )
                    .unwrap();
            }

            application
                .apply_authority_disconnect(&mut runtime, disconnect, 2)
                .unwrap();
            if !generic_first {
                application.observe_terminal_worker(
                    Some(RemoteOnlineTerminal::Failed(generic)),
                    NativeOnlineScreen::Reconnecting,
                );
                application
                    .handle_runtime_event(
                        &mut runtime,
                        OnlineLobbyEvent::PeerDisconnected {
                            connection: closed_connection,
                            user: SteamUserId::new(92_001).unwrap(),
                            peer_id: PeerId::new(51_001).unwrap(),
                            reconnect_allowed: false,
                        },
                        3,
                    )
                    .unwrap();
            }

            let commands = runtime.commands.len();
            application
                .apply_authority_disconnect(
                    &mut runtime,
                    RemoteAuthorityDisconnect {
                        message: crate::network_protocol::DisconnectMessage {
                            retry: RetryDisposition::Fatal,
                            detail_code: 92,
                            ..disconnect.message
                        },
                        ..disconnect
                    },
                    4,
                )
                .unwrap();
            assert_eq!(runtime.commands.len(), commands);
            assert_eq!(application.authority_disconnect, Some(disconnect));
            assert_eq!(
                application.ui_snapshot(&runtime, true).failure,
                Some(OnlineFailure::from_disconnect(disconnect.message))
            );
        }

        let (mut application, mut runtime, _transport) = active_remote_failure_fixture();
        let exact =
            disconnect_for_active_remote(&application, RetryDisposition::ReconnectAllowed, 93);
        assert_eq!(
            application.apply_authority_disconnect(
                &mut runtime,
                RemoteAuthorityDisconnect {
                    generation: exact.generation + 1,
                    ..exact
                },
                1,
            ),
            Err(NativeOnlineApplicationError::InvalidAction)
        );
        assert_eq!(
            application.apply_authority_disconnect(
                &mut runtime,
                RemoteAuthorityDisconnect {
                    message: crate::network_protocol::DisconnectMessage {
                        match_id: Some(MatchId::new([0xDD; 16]).unwrap()),
                        ..exact.message
                    },
                    ..exact
                },
                2,
            ),
            Err(NativeOnlineApplicationError::InvalidAction)
        );
        assert!(application.authority_disconnect.is_none());
    }

    #[test]
    fn fatal_runtime_pump_cleanup_releases_workers_handoffs_and_exposes_safe_actions() {
        let (mut application, runtime, _transport) = active_remote_failure_fixture();
        let failure = OnlineFailure {
            code: OnlineFailureCode::ConnectionTimedOut,
            severity: OnlineFailureSeverity::Fatal,
            recovery: OnlineRecoveryAction::ReturnToMenu,
            detail_code: 73,
        };
        assert!(application.active.is_some());
        assert!(!application.staged_endpoints.is_empty());
        assert!(!application.pending_authority_commands.is_empty());

        application.handle_fatal_runtime_pump(failure);

        assert!(application.active.is_none());
        assert!(!application.accepts_gameplay_input());
        assert!(application.staged_endpoints.is_empty());
        assert!(application.pending_authority_commands.is_empty());
        assert!(application.bindings.iter().all(Option::is_none));
        assert!(application.release_render_world);
        assert_eq!(application.failure_override, Some(failure));
        assert_eq!(application.metrics.sessions_stopped, 1);
        let snapshot = application.ui_snapshot(&runtime, true);
        assert_eq!(snapshot.screen, NativeOnlineScreen::Error);
        assert!(snapshot.actions.return_to_menu);
        assert!(!snapshot.actions.create_private);
        assert!(!snapshot.actions.rematch);
        assert!(!snapshot.actions.toggle_ready);
    }

    #[test]
    fn menu_and_retry_cleanup_are_local_even_when_native_leave_fails() {
        let (mut menu_application, mut menu_runtime, _menu_transport) =
            active_remote_failure_fixture();
        menu_runtime.reject_leave = true;
        menu_application
            .dispatch(&mut menu_runtime, NativeOnlineUiAction::ReturnToMenu, 1)
            .unwrap();
        assert!(menu_application.active.is_none());
        assert!(menu_application.staged_endpoints.is_empty());
        assert!(menu_application.pending_authority_commands.is_empty());
        assert!(menu_application.request_leave_user_mode);
        assert!(menu_application.release_render_world);
        assert_eq!(menu_runtime.lifecycle.leaves, 1);

        let (mut retry_application, mut retry_runtime, _retry_transport) =
            active_remote_failure_fixture();
        retry_runtime.reject_leave = true;
        retry_application.failure_override = Some(OnlineFailure {
            code: OnlineFailureCode::InternalFailure,
            severity: OnlineFailureSeverity::Fatal,
            recovery: OnlineRecoveryAction::Retry,
            detail_code: 74,
        });
        retry_application
            .dispatch(&mut retry_runtime, NativeOnlineUiAction::Retry, 2)
            .unwrap();
        assert!(retry_application.active.is_none());
        assert!(retry_application.staged_endpoints.is_empty());
        assert!(retry_application.pending_authority_commands.is_empty());
        assert!(retry_application.failure_override.is_none());
        assert!(retry_application.request_online_focus);
        assert!(retry_application.release_render_world);
        assert_eq!(retry_runtime.lifecycle.leaves, 1);
    }

    #[test]
    fn application_capacity_failure_synchronously_clears_match_and_retry_is_idempotent() {
        let (mut application, mut runtime, _transport) = active_remote_failure_fixture();
        for _ in 0..=crate::native_online::MAX_NATIVE_ONLINE_EVENTS {
            runtime.events.push_back(OnlineLobbyEvent::StateChanged {
                from: crate::online_lobby::OnlineLobbyPhase::Fighting,
                to: crate::online_lobby::OnlineLobbyPhase::Fighting,
            });
        }

        assert_eq!(
            application.pump(&mut runtime, 1),
            Err(NativeOnlineApplicationError::EndpointCapacity)
        );
        let expected_failure = OnlineFailure {
            code: OnlineFailureCode::InternalCapacity,
            severity: OnlineFailureSeverity::Fatal,
            recovery: OnlineRecoveryAction::ReturnToMenu,
            detail_code: 0,
        };
        assert!(application.active.is_none());
        assert!(application.staged_endpoints.is_empty());
        assert!(application.pending_authority_commands.is_empty());
        assert!(application.release_render_world);
        assert_eq!(application.metrics.sessions_stopped, 1);
        assert_eq!(application.failure_override, Some(expected_failure));
        let snapshot = application.ui_snapshot(&runtime, true);
        assert_eq!(snapshot.screen, NativeOnlineScreen::Error);
        assert_eq!(snapshot.failure, Some(expected_failure));

        application
            .dispatch(&mut runtime, NativeOnlineUiAction::Retry, 2)
            .unwrap();
        assert!(application.active.is_none());
        assert_eq!(application.metrics.sessions_stopped, 1);
        assert!(application.failure_override.is_none());
        assert!(application.staged_endpoints.is_empty());
        assert!(application.pending_authority_commands.is_empty());
    }

    #[test]
    fn trusted_and_dedicated_actions_fail_closed_without_runtime_commands() {
        let mut runtime = FakeRuntime::available();
        let mut application = NativeOnlineApplication::default();
        let snapshot = application.ui_snapshot(&runtime, true);
        assert!(!native_online_action_available(
            &snapshot,
            NativeOnlineUiAction::RequestTrusted
        ));
        assert!(!native_online_action_available(
            &snapshot,
            NativeOnlineUiAction::RequestDedicated
        ));
        assert_eq!(
            application.dispatch(&mut runtime, NativeOnlineUiAction::RequestTrusted, 1),
            Err(NativeOnlineApplicationError::TrustedResultsDisabled)
        );
        assert_eq!(runtime.commands.len(), 0);
        assert_eq!(
            application.failure_override.map(|failure| failure.code),
            Some(OnlineFailureCode::PublicPlayDisabled)
        );
        assert_eq!(
            application.dispatch(&mut runtime, NativeOnlineUiAction::RequestDedicated, 2),
            Err(NativeOnlineApplicationError::DedicatedModeDisabled)
        );
        assert_eq!(runtime.commands.len(), 0);
        assert_eq!(
            application.failure_override.map(|failure| failure.code),
            Some(OnlineFailureCode::DedicatedUnavailable)
        );
    }

    #[test]
    fn unavailable_state_uses_stable_localization_key_and_no_backend_text() {
        let mut snapshot = NativeOnlineUiSnapshot::default();
        snapshot.visible = true;
        snapshot.availability = NativeOnlineAvailability::Unavailable(
            NativeOnlineUnavailableReason::UnsupportedPlatform,
        );
        snapshot.screen = NativeOnlineScreen::Unavailable;
        assert_eq!(
            native_online_details(&snapshot),
            "Online play is not supported on this platform."
        );
        assert_eq!(
            online_localized(snapshot.availability.message_key()),
            "Online play is not supported on this platform."
        );
    }

    #[test]
    fn overlay_notice_toast_is_standalone_when_online_panel_is_hidden_and_dismisses() {
        let mut application = NativeOnlineApplication::default();
        application.observe_overlay_request(
            OverlayUnavailableSurface::ControllerBindings,
            SteamOverlayRequestStatus::Unavailable,
            10,
        );
        let mut snapshot = NativeOnlineUiSnapshot::default();
        snapshot.visible = false;
        snapshot.overlay_notice = application.overlay_notice();

        let mut app = App::new();
        app.insert_non_send_resource(application)
            .insert_resource(snapshot)
            .init_resource::<MatchPresentationPolicy>()
            .add_systems(Startup, setup_native_online_ui)
            .add_systems(
                Update,
                (
                    handle_overlay_unavailable_notice_dismiss,
                    update_native_online_ui,
                )
                    .chain(),
            );
        app.update();

        let world = app.world_mut();
        let notice_display = world
            .query_filtered::<&Node, With<OverlayUnavailableNoticeRoot>>()
            .single(world)
            .unwrap()
            .display;
        let panel_display = world
            .query_filtered::<&Node, With<NativeOnlineUiRoot>>()
            .single(world)
            .unwrap()
            .display;
        assert_eq!(notice_display, Display::Flex);
        assert_eq!(panel_display, Display::None);
        let dismiss = world
            .query_filtered::<Entity, With<DismissOverlayUnavailableNotice>>()
            .single(world)
            .unwrap();
        world.entity_mut(dismiss).insert(Interaction::Pressed);

        app.update();
        assert!(
            app.world()
                .non_send_resource::<NativeOnlineApplication>()
                .overlay_notice()
                .is_none()
        );
    }

    #[test]
    fn local_couch_ordinals_are_sampled_independently_of_global_protocol_seats() {
        let keys = ButtonInput::<KeyCode>::default();
        let sample = sample_native_online_keyboard(
            &keys,
            0.0,
            PlayerControlBindings::player_three_default(),
        );
        assert_eq!(sample, RenderInputSample::default());
        let editor = CouchSeatEditor::default();
        assert_eq!(editor.seat_count, 1);
        assert_eq!(LocalSeatId::new(0).unwrap().index(), 0);
    }

    #[test]
    fn inactive_online_sampler_preserves_offline_input_and_gesture_history() {
        let seat = LocalSeatId::new(0).unwrap();
        let mut app = App::new();
        app.insert_non_send_resource(NativeOnlineApplication::default())
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<GameplayCameraControl>()
            .init_resource::<PlayerKeyBindings>()
            .init_resource::<LocalTickInputState>()
            .add_systems(Update, sample_native_online_render_input);
        app.world_mut()
            .resource_mut::<LocalTickInputState>()
            .merge_render_sample(
                seat,
                RenderInputSample {
                    held: InputMask::LEFT,
                    pressed: InputMask::LIGHT,
                    ..default()
                },
            );

        app.update();

        let frame = app
            .world_mut()
            .resource_mut::<LocalTickInputState>()
            .drain_for_tick(seat, 1);
        assert_eq!(frame.held, InputMask::LEFT);
        assert_eq!(frame.pressed, InputMask::LIGHT);
    }

    fn steam_input_with_menu(
        local_ordinal: usize,
        menu_held: SteamMenuInputMask,
    ) -> SteamInputSnapshot {
        let mut snapshot = SteamInputSnapshot::default();
        snapshot.controllers[local_ordinal] = SteamInputControllerSnapshot {
            controller_id: crate::steam_platform::SteamInputControllerId::new(
                local_ordinal as u64 + 100,
            ),
            menu_held,
            ..default()
        };
        snapshot
    }

    #[test]
    fn controller_menu_navigation_accept_and_edges_are_latched_once() {
        let mut ui = NativeOnlineUiSnapshot::default();
        ui.screen = NativeOnlineScreen::OnlineMenu;
        ui.actions.create_private = true;
        ui.actions.create_friends = true;
        let mut application = NativeOnlineApplication::default();
        application.controller_menu_intent(&ui, SteamInputSnapshot::default());

        let mut down = SteamMenuInputMask::NONE;
        down.insert(SteamMenuAction::Down);
        assert_eq!(
            application.controller_menu_intent(&ui, steam_input_with_menu(0, down)),
            ControllerMenuIntent::None
        );
        assert_eq!(
            application.controller_selected_action,
            Some(NativeOnlineUiAction::CreateFriends)
        );

        // Release between actions, then accept the focused friends-only item.
        application.controller_menu_intent(&ui, SteamInputSnapshot::default());
        let mut accept = SteamMenuInputMask::NONE;
        accept.insert(SteamMenuAction::Accept);
        assert_eq!(
            application.controller_menu_intent(&ui, steam_input_with_menu(0, accept)),
            ControllerMenuIntent::Dispatch(NativeOnlineUiAction::CreateFriends)
        );
        assert_eq!(
            application.controller_menu_intent(&ui, steam_input_with_menu(0, accept)),
            ControllerMenuIntent::None
        );
    }

    #[test]
    fn controller_back_and_binding_panel_intents_work_from_any_local_ordinal() {
        let mut ui = NativeOnlineUiSnapshot::default();
        ui.screen = NativeOnlineScreen::JoinPrompt;
        ui.actions.accept_join = true;
        ui.actions.decline_join = true;
        let mut application = NativeOnlineApplication::default();
        application.controller_menu_intent(&ui, SteamInputSnapshot::default());
        let mut back = SteamMenuInputMask::NONE;
        back.insert(SteamMenuAction::Back);
        assert_eq!(
            application.controller_menu_intent(&ui, steam_input_with_menu(3, back)),
            ControllerMenuIntent::Dispatch(NativeOnlineUiAction::DeclineJoin)
        );

        application.controller_menu_intent(&ui, SteamInputSnapshot::default());
        let mut bindings = SteamMenuInputMask::NONE;
        bindings.insert(SteamMenuAction::OpenBindings);
        assert_eq!(
            application.controller_menu_intent(&ui, steam_input_with_menu(2, bindings)),
            ControllerMenuIntent::OpenBindings(2)
        );
    }

    #[test]
    fn held_gameplay_face_button_does_not_accept_a_new_results_screen() {
        let mut fighting = NativeOnlineUiSnapshot::default();
        fighting.screen = NativeOnlineScreen::Fighting;
        fighting.actions.leave = true;
        let mut application = NativeOnlineApplication::default();
        application.controller_menu_intent(&fighting, SteamInputSnapshot::default());

        let mut results = NativeOnlineUiSnapshot::default();
        results.screen = NativeOnlineScreen::Results;
        results.actions.rematch = true;
        let mut accept = SteamMenuInputMask::NONE;
        accept.insert(SteamMenuAction::Accept);
        assert_eq!(
            application.controller_menu_intent(&results, steam_input_with_menu(0, accept)),
            ControllerMenuIntent::None
        );

        application.controller_menu_intent(&results, SteamInputSnapshot::default());
        assert_eq!(
            application.controller_menu_intent(&results, steam_input_with_menu(0, accept)),
            ControllerMenuIntent::Dispatch(NativeOnlineUiAction::Rematch)
        );
    }

    #[test]
    fn steam_analog_and_dpad_movement_share_camera_relative_mapping() {
        let analog = SteamInputControllerSnapshot {
            controller_id: crate::steam_platform::SteamInputControllerId::new(1),
            movement: QuantizedMovement::new(127, 0),
            gameplay_held: InputMask::JUMP,
            ..default()
        };
        let (movement, held) = sample_native_steam_controller(analog, 0.0);
        assert_eq!(movement, QuantizedMovement::new(127, 0));
        assert_eq!(held, InputMask::JUMP);

        let dpad = SteamInputControllerSnapshot {
            controller_id: crate::steam_platform::SteamInputControllerId::new(2),
            gameplay_held: InputMask::UP | InputMask::HEAVY,
            ..default()
        };
        let (movement, held) = sample_native_steam_controller(dpad, 0.0);
        assert_eq!(movement, QuantizedMovement::new(0, -127));
        assert_eq!(held, InputMask::UP | InputMask::HEAVY);
    }
}

#[derive(Resource, Clone, Debug)]
pub struct NativeOnlineUiSnapshot {
    pub visible: bool,
    pub availability: NativeOnlineAvailability,
    pub screen: NativeOnlineScreen,
    pub actions: NativeOnlineActions,
    pub role: Option<OnlineLobbyRole>,
    pub lobby_members: u8,
    pub total_seats: u8,
    pub local_seats: u8,
    pub local_ready: bool,
    pub all_members_ready: bool,
    pub network_quality: NetworkQualitySnapshot,
    pub input_delay_calibration: InputDelayCalibrationSnapshot,
    pub countdown_start_tick: Option<SimTick>,
    pub outcome: Option<OnlineMatchOutcome>,
    pub failure: Option<OnlineFailure>,
    /// Structured authority terminal retained for diagnostics and recovery
    /// policy. Player-facing text must use only `failure.message_key()`.
    pub authority_disconnect: Option<RemoteAuthorityDisconnect>,
    pub selected_seat: u8,
    pub selected_loadout: OnlineSeatSelection,
    pub selected_arena: DefinitionId,
    pub selected_rules: DefinitionId,
    pub session_kind: Option<NativeOnlineSessionKind>,
    pub worker_phase: Option<RemoteOnlineClientPhase>,
    pub worker_tick: Option<SimTick>,
    pub authority_tick: Option<SimTick>,
    pub controller_selected_action: Option<NativeOnlineUiAction>,
    pub metrics: NativeOnlineApplicationMetrics,
    pub leave_confirmation_open: bool,
    pub overlay_notice: Option<OverlayUnavailableNotice>,
    pub confirmed_match: Option<ConfirmedMatchPresentation>,
}

impl Default for NativeOnlineUiSnapshot {
    fn default() -> Self {
        Self {
            visible: false,
            availability: NativeOnlineAvailability::Unavailable(
                NativeOnlineUnavailableReason::SteamFeatureDisabled,
            ),
            screen: NativeOnlineScreen::Unavailable,
            actions: NativeOnlineActions::default(),
            role: None,
            lobby_members: 0,
            total_seats: 0,
            local_seats: 1,
            local_ready: false,
            all_members_ready: false,
            network_quality: NetworkQualitySnapshot::default(),
            input_delay_calibration: InputDelayCalibrationSnapshot::default(),
            countdown_start_tick: None,
            outcome: None,
            failure: None,
            authority_disconnect: None,
            selected_seat: 0,
            selected_loadout: CouchSeatEditor::default().selected(),
            selected_arena: CouchSeatEditor::default().arena,
            selected_rules: CouchSeatEditor::default().rules,
            session_kind: None,
            worker_phase: None,
            worker_tick: None,
            authority_tick: None,
            controller_selected_action: None,
            metrics: NativeOnlineApplicationMetrics::default(),
            leave_confirmation_open: false,
            overlay_notice: None,
            confirmed_match: None,
        }
    }
}

impl NativeOnlineApplication {
    pub fn ui_snapshot<R: NativeOnlineRuntimePort>(
        &self,
        runtime: &R,
        visible: bool,
    ) -> NativeOnlineUiSnapshot {
        let native = runtime.view_model();
        let typed_failure = self
            .authority_disconnect
            .map(|disconnect| OnlineFailure::from_disconnect(disconnect.message));
        let failure = typed_failure.or(self.failure_override).or(native.failure);
        let shutdown_pending = self.listen_shutdown.is_some() || self.awaiting_transport_retirement;
        let screen = if shutdown_pending {
            NativeOnlineScreen::ReturningToLobby
        } else if let Some(disconnect) = self.authority_disconnect {
            match disconnect.message.retry {
                RetryDisposition::ReconnectAllowed => NativeOnlineScreen::Reconnecting,
                RetryDisposition::MatchEndedNoContest => NativeOnlineScreen::Results,
                RetryDisposition::ReturnToLobby | RetryDisposition::Fatal => {
                    NativeOnlineScreen::Error
                }
            }
        } else if self.failure_override.is_some() {
            NativeOnlineScreen::Error
        } else {
            native.screen
        };
        let actions = if shutdown_pending {
            NativeOnlineActions::default()
        } else if let Some(failure) = failure {
            actions_for_recovery(failure.recovery)
        } else {
            native.actions
        };
        let (worker_phase, worker_tick) = self
            .active
            .as_ref()
            .map(|active| {
                let status = active.client().status();
                (Some(status.phase), Some(status.network_tick))
            })
            .unwrap_or((None, None));
        let authority_tick = match &self.active {
            Some(ActiveNativeOnlineMatch::Listen(online_match)) => {
                Some(online_match.authority.status().authority_tick)
            }
            _ => None,
        };
        NativeOnlineUiSnapshot {
            visible,
            availability: native.availability,
            screen,
            actions,
            role: native.role,
            lobby_members: native.lobby_members,
            total_seats: native.total_seats,
            local_seats: self.editor.seat_count,
            local_ready: native.local_ready,
            all_members_ready: native.all_members_ready,
            network_quality: native.network_quality,
            input_delay_calibration: native.input_delay_calibration,
            countdown_start_tick: native.countdown_start_tick,
            outcome: native.outcome,
            failure,
            authority_disconnect: self.authority_disconnect,
            selected_seat: self.editor.selected_seat,
            selected_loadout: self.editor.selected(),
            selected_arena: self.editor.arena,
            selected_rules: self.editor.rules,
            session_kind: self.active_session_kind(),
            worker_phase,
            worker_tick,
            authority_tick,
            controller_selected_action: self.controller_selected_action,
            metrics: self.metrics,
            leave_confirmation_open: self.leave_confirmation_open && !shutdown_pending,
            overlay_notice: self.overlay_notice,
            confirmed_match: None,
        }
    }
}

/// Derives the only online presentation policy consumed by overlay, HUD, and
/// audio systems. The resource is updated only when its semantic value changes.
pub fn derive_match_presentation_policy(
    snapshot: Res<NativeOnlineUiSnapshot>,
    active_arena: Res<ActiveArena>,
    mut policy: ResMut<MatchPresentationPolicy>,
) {
    let next = presentation_policy_for(&snapshot, active_arena.index());
    if *policy != next {
        *policy = next;
    }
}

fn presentation_policy_for(
    snapshot: &NativeOnlineUiSnapshot,
    arena_index: usize,
) -> MatchPresentationPolicy {
    if !snapshot.visible {
        MatchPresentationPolicy::default()
    } else if snapshot.leave_confirmation_open {
        MatchPresentationPolicy {
            phase: PresentationPhase::LeaveConfirmation,
            panel: OnlinePanelMode::LeaveConfirmation,
            gameplay_hud_visible: matches!(
                snapshot.screen,
                NativeOnlineScreen::Countdown
                    | NativeOnlineScreen::Fighting
                    | NativeOnlineScreen::Reconnecting
                    | NativeOnlineScreen::ConfirmingResult
            ),
            music: online_music_for_screen(snapshot.screen, arena_index),
            result_sfx: None,
        }
    } else {
        let (phase, panel, gameplay_hud_visible) = match snapshot.screen {
            NativeOnlineScreen::OnlineMenu
            | NativeOnlineScreen::JoinPrompt
            | NativeOnlineScreen::CreatingLobby
            | NativeOnlineScreen::JoiningLobby
            | NativeOnlineScreen::Lobby
            | NativeOnlineScreen::Connecting
            | NativeOnlineScreen::Authenticating
            | NativeOnlineScreen::ManifestAgreement
            | NativeOnlineScreen::Loading
            | NativeOnlineScreen::Ready
            | NativeOnlineScreen::ReturningToLobby
            | NativeOnlineScreen::Unavailable => {
                (PresentationPhase::Menu, OnlinePanelMode::Full, false)
            }
            NativeOnlineScreen::Countdown => (
                PresentationPhase::Countdown,
                OnlinePanelMode::CountdownStrip,
                true,
            ),
            NativeOnlineScreen::Fighting => (
                PresentationPhase::Fighting,
                OnlinePanelMode::FightStrip,
                true,
            ),
            NativeOnlineScreen::Reconnecting => (
                PresentationPhase::Reconnecting,
                OnlinePanelMode::ReconnectStrip,
                true,
            ),
            NativeOnlineScreen::ConfirmingResult => (
                PresentationPhase::ConfirmingResult,
                OnlinePanelMode::ConfirmingStrip,
                true,
            ),
            NativeOnlineScreen::Results => {
                (PresentationPhase::Results, OnlinePanelMode::Results, false)
            }
            NativeOnlineScreen::Error => (PresentationPhase::Error, OnlinePanelMode::Full, false),
        };
        let result_sfx = snapshot.confirmed_match.as_ref().and_then(|result| {
            let kind = match result.local_outcome {
                PresentedLocalOutcome::Victory | PresentedLocalOutcome::Mixed => {
                    PresentationResultSfx::Victory
                }
                PresentedLocalOutcome::Defeat => PresentationResultSfx::Defeat,
                PresentedLocalOutcome::Draw | PresentedLocalOutcome::NoContest => return None,
            };
            Some((result.key, kind))
        });
        MatchPresentationPolicy {
            phase,
            panel,
            gameplay_hud_visible,
            music: online_music_for_screen(snapshot.screen, arena_index),
            result_sfx,
        }
    }
}

fn online_music_for_screen(
    screen: NativeOnlineScreen,
    arena_index: usize,
) -> PresentationMusicTrack {
    match screen {
        NativeOnlineScreen::Countdown
        | NativeOnlineScreen::Fighting
        | NativeOnlineScreen::Reconnecting
        | NativeOnlineScreen::ConfirmingResult => PresentationMusicTrack::Arena(arena_index),
        NativeOnlineScreen::Results | NativeOnlineScreen::Error => PresentationMusicTrack::None,
        _ => PresentationMusicTrack::Menu,
    }
}

#[derive(Component)]
pub struct NativeOnlineUiRoot;

#[derive(Component)]
pub(crate) struct NativeOnlineUiPanel;

#[derive(Component)]
pub(crate) struct NativeOnlineUiTitle;

#[derive(Component)]
pub(crate) struct NativeOnlineUiDetails;

#[derive(Component)]
pub(crate) struct NativeOnlineUiFooter;

#[derive(Component)]
pub(crate) struct OverlayUnavailableNoticeRoot;

#[derive(Component)]
pub(crate) struct OverlayUnavailableNoticeText;

#[derive(Component)]
pub(crate) struct DismissOverlayUnavailableNotice;

fn native_online_button(label: &'static str, action: NativeOnlineUiAction) -> impl Bundle {
    (
        Button,
        action,
        Node {
            display: Display::None,
            min_width: Val::Px(150.0),
            height: Val::Px(42.0),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            border: UiRect::all(Val::Px(2.0)),
            padding: UiRect::axes(Val::Px(12.0), Val::Px(5.0)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.055, 0.055, 0.07, 0.96)),
        BorderColor::all(Color::srgb(0.38, 0.42, 0.48)),
        children![(
            Text::new(label),
            TextFont {
                font_size: 17.0,
                ..default()
            },
            TextColor(Color::srgb(0.93, 0.88, 0.77)),
            TextLayout::new_with_justify(Justify::Center),
        )],
    )
}

pub fn setup_native_online_ui(mut commands: Commands, ui_cameras: Query<Entity, With<UiCamera>>) {
    let mut root = commands.spawn((
        NativeOnlineUiRoot,
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
        Pickable::IGNORE,
    ));
    root.with_children(|root| {
        root.spawn((
            NativeOnlineUiPanel,
            Node {
                width: Val::Percent(86.0),
                max_width: Val::Px(920.0),
                min_height: Val::Px(280.0),
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
                NativeOnlineUiTitle,
                Text::new("ONLINE"),
                TextFont {
                    font_size: 38.0,
                    ..default()
                },
                TextColor(Color::srgb(0.93, 0.79, 0.52)),
                TextLayout::new_with_justify(Justify::Center),
            ));
            panel.spawn((
                NativeOnlineUiDetails,
                Text::new(""),
                TextFont {
                    font_size: 19.0,
                    ..default()
                },
                TextColor(Color::srgb(0.82, 0.84, 0.87)),
                TextLayout::new_with_justify(Justify::Center),
                Node {
                    min_height: Val::Px(72.0),
                    ..default()
                },
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
                        ("CREATE PRIVATE", NativeOnlineUiAction::CreatePrivate),
                        ("CREATE FRIENDS", NativeOnlineUiAction::CreateFriends),
                        ("ACCEPT JOIN", NativeOnlineUiAction::AcceptJoin),
                        ("DECLINE", NativeOnlineUiAction::DeclineJoin),
                        ("INVITE FRIENDS", NativeOnlineUiAction::InviteFriends),
                        ("ADD COUCH SEAT", NativeOnlineUiAction::AddSeat),
                        ("REMOVE SEAT", NativeOnlineUiAction::RemoveSeat),
                        ("PREVIOUS SEAT", NativeOnlineUiAction::PreviousSeat),
                        ("NEXT SEAT", NativeOnlineUiAction::NextSeat),
                        ("CHARACTER -", NativeOnlineUiAction::PreviousCharacter),
                        ("CHARACTER +", NativeOnlineUiAction::NextCharacter),
                        ("STYLE -", NativeOnlineUiAction::PreviousStyle),
                        ("STYLE +", NativeOnlineUiAction::NextStyle),
                        ("EQUIPMENT -", NativeOnlineUiAction::PreviousEquipment),
                        ("EQUIPMENT +", NativeOnlineUiAction::NextEquipment),
                        ("TOGGLE TEAM", NativeOnlineUiAction::ToggleTeam),
                        ("ARENA -", NativeOnlineUiAction::PreviousArena),
                        ("ARENA +", NativeOnlineUiAction::NextArena),
                        ("RULES -", NativeOnlineUiAction::PreviousRules),
                        ("RULES +", NativeOnlineUiAction::NextRules),
                        ("READY / NOT READY", NativeOnlineUiAction::ToggleReady),
                        ("START MATCH", NativeOnlineUiAction::StartMatch),
                        ("REMATCH", NativeOnlineUiAction::Rematch),
                        ("RETURN TO LOBBY", NativeOnlineUiAction::ReturnToLobby),
                        ("LEAVE ONLINE", NativeOnlineUiAction::RequestLeave),
                        ("CANCEL", NativeOnlineUiAction::CancelLeave),
                        ("CONFIRM LEAVE", NativeOnlineUiAction::LeaveOnline),
                        ("BACK", NativeOnlineUiAction::ReturnToMenu),
                        ("RETRY", NativeOnlineUiAction::Retry),
                        ("DISMISS", NativeOnlineUiAction::DismissError),
                    ] {
                        buttons.spawn(native_online_button(label, action));
                    }
                });
            panel.spawn((
                NativeOnlineUiFooter,
                Text::new(""),
                TextFont {
                    font_size: 15.0,
                    ..default()
                },
                TextColor(Color::srgb(0.62, 0.66, 0.72)),
                TextLayout::new_with_justify(Justify::Center),
            ));
        });
    });
    if let Some(camera) = ui_cameras.iter().next() {
        root.insert(UiTargetCamera(camera));
    }

    let mut notice_root = commands.spawn((
        OverlayUnavailableNoticeRoot,
        Node {
            display: Display::None,
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: Val::Px(18.0),
            width: Val::Percent(100.0),
            justify_content: JustifyContent::Center,
            padding: UiRect::horizontal(Val::Px(18.0)),
            ..default()
        },
        GlobalZIndex(1_000),
        Pickable::IGNORE,
    ));
    notice_root.with_children(|root| {
        root.spawn((
            Node {
                max_width: Val::Px(680.0),
                min_height: Val::Px(54.0),
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: Val::Px(12.0),
                padding: UiRect::axes(Val::Px(16.0), Val::Px(10.0)),
                border: UiRect::all(Val::Px(2.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.12, 0.075, 0.025, 0.96)),
            BorderColor::all(Color::srgb(0.92, 0.62, 0.22)),
            Pickable::IGNORE,
        ))
        .with_children(|notice| {
            notice.spawn((
                OverlayUnavailableNoticeText,
                Text::new(""),
                TextFont {
                    font_size: 17.0,
                    ..default()
                },
                TextColor(Color::srgb(0.98, 0.9, 0.72)),
                Node {
                    flex_grow: 1.0,
                    ..default()
                },
                Pickable::IGNORE,
            ));
            notice
                .spawn((
                    Button,
                    DismissOverlayUnavailableNotice,
                    Node {
                        width: Val::Px(34.0),
                        height: Val::Px(34.0),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        border: UiRect::all(Val::Px(1.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.08, 0.055, 0.025, 0.98)),
                    BorderColor::all(Color::srgb(0.82, 0.7, 0.5)),
                ))
                .with_child((
                    Text::new("×"),
                    TextFont {
                        font_size: 22.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.98, 0.9, 0.72)),
                    Pickable::IGNORE,
                ));
        });
    });
    if let Some(camera) = ui_cameras.iter().next() {
        notice_root.insert(UiTargetCamera(camera));
    }
}

pub(crate) fn update_native_online_ui(
    snapshot: Res<NativeOnlineUiSnapshot>,
    policy: Res<MatchPresentationPolicy>,
    mut roots: Query<(&mut Node, &mut BackgroundColor), With<NativeOnlineUiRoot>>,
    mut panels: Query<
        (&mut Node, &mut BackgroundColor),
        (With<NativeOnlineUiPanel>, Without<NativeOnlineUiRoot>),
    >,
    mut titles: Query<&mut Text, With<NativeOnlineUiTitle>>,
    mut details: Query<&mut Text, (With<NativeOnlineUiDetails>, Without<NativeOnlineUiTitle>)>,
    mut footers: Query<
        &mut Text,
        (
            With<NativeOnlineUiFooter>,
            Without<NativeOnlineUiTitle>,
            Without<NativeOnlineUiDetails>,
        ),
    >,
    mut buttons: Query<
        (&NativeOnlineUiAction, &mut Node),
        (
            With<Button>,
            Without<NativeOnlineUiRoot>,
            Without<NativeOnlineUiPanel>,
            Without<OverlayUnavailableNoticeRoot>,
        ),
    >,
    mut notice_roots: Query<
        &mut Node,
        (
            With<OverlayUnavailableNoticeRoot>,
            Without<NativeOnlineUiRoot>,
            Without<NativeOnlineUiPanel>,
            Without<NativeOnlineUiAction>,
        ),
    >,
    mut notice_texts: Query<
        &mut Text,
        (
            With<OverlayUnavailableNoticeText>,
            Without<NativeOnlineUiTitle>,
            Without<NativeOnlineUiDetails>,
            Without<NativeOnlineUiFooter>,
        ),
    >,
) {
    for (mut node, mut background) in &mut roots {
        node.display = if snapshot.visible {
            Display::Flex
        } else {
            Display::None
        };
        let alpha = match snapshot.screen {
            NativeOnlineScreen::Fighting => 0.0,
            NativeOnlineScreen::Reconnecting => 0.28,
            NativeOnlineScreen::Countdown => 0.42,
            _ => 0.96,
        };
        *background = BackgroundColor(Color::srgba(0.006, 0.008, 0.014, alpha));
        let compact = matches!(
            policy.panel,
            OnlinePanelMode::CountdownStrip
                | OnlinePanelMode::FightStrip
                | OnlinePanelMode::ReconnectStrip
                | OnlinePanelMode::ConfirmingStrip
        );
        node.align_items = if compact {
            AlignItems::FlexStart
        } else {
            AlignItems::Center
        };
        node.justify_content = if compact {
            JustifyContent::FlexEnd
        } else {
            JustifyContent::Center
        };
    }
    for (mut node, mut background) in &mut panels {
        let compact = matches!(
            policy.panel,
            OnlinePanelMode::CountdownStrip
                | OnlinePanelMode::FightStrip
                | OnlinePanelMode::ReconnectStrip
                | OnlinePanelMode::ConfirmingStrip
        );
        node.width = if compact {
            Val::Percent(ONLINE_COMPACT_PANEL_WIDTH_PERCENT)
        } else {
            Val::Percent(86.0)
        };
        node.max_width = if compact {
            Val::Px(ONLINE_COMPACT_PANEL_MAX_WIDTH)
        } else {
            Val::Px(920.0)
        };
        node.min_height = if compact {
            Val::Px(0.0)
        } else {
            Val::Px(280.0)
        };
        node.padding = UiRect::all(Val::Px(if compact { 10.0 } else { 22.0 }));
        node.row_gap = Val::Px(if compact { 5.0 } else { 14.0 });
        let alpha = if compact { 0.76 } else { 0.94 };
        *background = BackgroundColor(Color::srgba(0.025, 0.028, 0.04, alpha));
    }
    for mut title in &mut titles {
        **title = native_online_title(&snapshot).to_owned();
    }
    for mut detail in &mut details {
        **detail = native_online_details(&snapshot);
    }
    for mut footer in &mut footers {
        **footer = native_online_footer(&snapshot).to_owned();
    }
    for (action, mut node) in &mut buttons {
        let compact = matches!(
            policy.panel,
            OnlinePanelMode::CountdownStrip
                | OnlinePanelMode::FightStrip
                | OnlinePanelMode::ReconnectStrip
                | OnlinePanelMode::ConfirmingStrip
        );
        node.display = if !compact && native_online_action_available(&snapshot, *action) {
            Display::Flex
        } else {
            Display::None
        };
    }
    for mut node in &mut notice_roots {
        node.display = if snapshot.overlay_notice.is_some() {
            Display::Flex
        } else {
            Display::None
        };
    }
    for mut text in &mut notice_texts {
        **text = snapshot
            .overlay_notice
            .map(|notice| online_localized(notice.failure.message_key()))
            .unwrap_or_default()
            .to_owned();
    }
}

pub(crate) fn handle_overlay_unavailable_notice_dismiss(
    interactions: Query<
        &Interaction,
        (
            Changed<Interaction>,
            With<Button>,
            With<DismissOverlayUnavailableNotice>,
        ),
    >,
    mut application: NonSendMut<NativeOnlineApplication>,
) {
    if interactions
        .iter()
        .any(|interaction| *interaction == Interaction::Pressed)
    {
        application.dismiss_overlay_notice();
    }
}

pub fn update_native_online_button_styles(
    snapshot: Res<NativeOnlineUiSnapshot>,
    mut buttons: Query<
        (
            &Interaction,
            &NativeOnlineUiAction,
            &mut BackgroundColor,
            &mut BorderColor,
        ),
        (With<Button>, With<NativeOnlineUiAction>),
    >,
) {
    for (interaction, action, mut background, mut border) in &mut buttons {
        let (fill, edge) = match interaction {
            Interaction::Pressed => (Color::srgb(0.38, 0.27, 0.11), Color::srgb(1.0, 0.82, 0.4)),
            Interaction::Hovered => (Color::srgb(0.14, 0.17, 0.22), Color::srgb(0.76, 0.82, 0.9)),
            Interaction::None if snapshot.controller_selected_action == Some(*action) => (
                Color::srgb(0.19, 0.14, 0.075),
                Color::srgb(0.93, 0.68, 0.27),
            ),
            Interaction::None => (
                Color::srgba(0.055, 0.055, 0.07, 0.96),
                Color::srgb(0.38, 0.42, 0.48),
            ),
        };
        *background = BackgroundColor(fill);
        *border = BorderColor::all(edge);
    }
}

fn native_online_title(snapshot: &NativeOnlineUiSnapshot) -> &'static str {
    if snapshot.leave_confirmation_open {
        return "LEAVE ONLINE MATCH?";
    }
    match snapshot.screen {
        NativeOnlineScreen::Unavailable => "ONLINE UNAVAILABLE",
        NativeOnlineScreen::OnlineMenu => "ONLINE",
        NativeOnlineScreen::JoinPrompt => "JOIN INVITATION",
        NativeOnlineScreen::CreatingLobby => "CREATING LOBBY",
        NativeOnlineScreen::JoiningLobby => "JOINING LOBBY",
        NativeOnlineScreen::Lobby => "ONLINE LOBBY",
        NativeOnlineScreen::Connecting => "CONNECTING",
        NativeOnlineScreen::Authenticating => "AUTHENTICATING",
        NativeOnlineScreen::ManifestAgreement => "VERIFYING MATCH",
        NativeOnlineScreen::Loading => "LOADING",
        NativeOnlineScreen::Ready => "READY",
        NativeOnlineScreen::Countdown => "GET READY",
        NativeOnlineScreen::Fighting => "ONLINE MATCH",
        NativeOnlineScreen::Reconnecting => "RECONNECTING",
        NativeOnlineScreen::ConfirmingResult => "VERIFYING RESULT",
        NativeOnlineScreen::Results => "RESULTS",
        NativeOnlineScreen::ReturningToLobby => "RETURNING TO LOBBY",
        NativeOnlineScreen::Error => "ONLINE ERROR",
    }
}

fn native_online_details(snapshot: &NativeOnlineUiSnapshot) -> String {
    if snapshot.leave_confirmation_open {
        return "Your fighter will keep sending neutral input until you confirm or cancel."
            .to_owned();
    }
    if snapshot.screen == NativeOnlineScreen::Unavailable {
        return online_localized(snapshot.availability.message_key()).to_owned();
    }
    if snapshot.screen == NativeOnlineScreen::Error {
        return snapshot
            .failure
            .map(|failure| online_localized(failure.message_key()).to_owned())
            .unwrap_or_else(|| online_localized("online.error.internal").to_owned());
    }
    let loadout = snapshot.selected_loadout;
    let quality = quality_label(snapshot.network_quality.quality);
    match snapshot.screen {
        NativeOnlineScreen::OnlineMenu => format!(
            "Couch seats: {}  |  Selected seat: {}\n{} / {} / {} / Team {}\nArena: {}  |  Rules: {}\nCasual private and friends-only listen matches. Results are untrusted.",
            snapshot.local_seats,
            snapshot.selected_seat + 1,
            character_label_for_definition(loadout.character),
            style_label_for_definition(loadout.style),
            equipment_label_for_definition(loadout.equipment),
            loadout.team.get() + 1,
            arena_definitions()[usize::from(snapshot.selected_arena.get())].name,
            RULE_PRESETS[usize::from(snapshot.selected_rules.get())].label,
        ),
        NativeOnlineScreen::JoinPrompt => format!(
            "Couch seats: {}  |  Selected seat: {}\n{} / {} / {} / Team {}\nThe host's arena and rules become immutable when you accept.",
            snapshot.local_seats,
            snapshot.selected_seat + 1,
            character_label_for_definition(loadout.character),
            style_label_for_definition(loadout.style),
            equipment_label_for_definition(loadout.equipment),
            loadout.team.get() + 1,
        ),
        NativeOnlineScreen::Lobby => format!(
            "Members: {}  |  Fighters: {}  |  Your couch seats: {}\nReady: {}  |  Everyone ready: {}  |  Network: {}\n{}\nSeat {}: {} / {} / {} / Team {}",
            snapshot.lobby_members,
            snapshot.total_seats,
            snapshot.local_seats,
            yes_no(snapshot.local_ready),
            yes_no(snapshot.all_members_ready),
            quality,
            input_delay_calibration_text(snapshot.input_delay_calibration),
            snapshot.selected_seat + 1,
            character_label_for_definition(loadout.character),
            style_label_for_definition(loadout.style),
            equipment_label_for_definition(loadout.equipment),
            loadout.team.get() + 1,
        ),
        NativeOnlineScreen::Countdown => format!(
            "Authority start tick: {}  |  Local network tick: {}\nNetwork: {} ({} ms / {}.{:02}% loss)",
            snapshot.countdown_start_tick.map_or(0, SimTick::get),
            snapshot.worker_tick.map_or(0, SimTick::get),
            quality,
            snapshot.network_quality.average_rtt_ms,
            snapshot.network_quality.average_loss_bps / 100,
            snapshot.network_quality.average_loss_bps % 100,
        ),
        NativeOnlineScreen::Fighting => format!(
            "{}  |  Tick {}  |  Network {}\nEsc opens the leave action; gameplay remains authority-driven.",
            match snapshot.session_kind {
                Some(NativeOnlineSessionKind::ListenOwner) => "LISTEN HOST",
                Some(NativeOnlineSessionKind::RemoteClient) => "REMOTE CLIENT",
                None => "ONLINE",
            },
            snapshot.worker_tick.map_or(0, SimTick::get),
            quality,
        ),
        NativeOnlineScreen::Reconnecting => format!(
            "Your seats are reserved during the reconnect grace period.\nConservative full resync attempts: {}  |  Network: {}",
            snapshot.metrics.conservative_full_resyncs, quality,
        ),
        NativeOnlineScreen::Results => confirmed_result_details(snapshot),
        _ => format!(
            "{}\nWorker: {:?}  |  Network: {}",
            online_localized(snapshot.screen.message_key()),
            snapshot.worker_phase,
            quality,
        ),
    }
}

fn confirmed_result_details(snapshot: &NativeOnlineUiSnapshot) -> String {
    if snapshot.outcome == Some(OnlineMatchOutcome::NoContestHostLost) {
        return "NO CONTEST — HOST LOST\nThe listen authority disconnected.".to_owned();
    }
    let Some(result) = snapshot.confirmed_match.as_ref() else {
        return "VERIFYING FINAL RESULT…".to_owned();
    };
    let headline = match result.local_outcome {
        PresentedLocalOutcome::Victory => "VICTORY",
        PresentedLocalOutcome::Defeat => "DEFEAT",
        PresentedLocalOutcome::Draw => "DRAW",
        PresentedLocalOutcome::Mixed => "MIXED COUCH RESULT",
        PresentedLocalOutcome::NoContest => "NO CONTEST",
    };
    let outcome = match result.outcome {
        PresentedMatchOutcome::FighterWinner(fighter) => {
            format!("Fighter {} wins", fighter.get() + 1)
        }
        PresentedMatchOutcome::TeamWinner(team) => format!("Team {} wins", team.get() + 1),
        PresentedMatchOutcome::Draw => "Draw".to_owned(),
        PresentedMatchOutcome::Aborted(PresentedAbortReason::HostLost) => "Host lost".to_owned(),
        PresentedMatchOutcome::Aborted(PresentedAbortReason::SessionFailure) => {
            "Session ended".to_owned()
        }
        PresentedMatchOutcome::Aborted(PresentedAbortReason::Authority(code)) => {
            format!("Authority no-contest code {code}")
        }
    };
    let mut rows = Vec::new();
    for fighter in result.fighters.iter().flatten() {
        rows.push(format!(
            "P{}{}  KOs {}  Deaths {}  Damage {}",
            fighter.fighter.get() + 1,
            if fighter.locally_owned { " (YOU)" } else { "" },
            fighter.stats.knockouts,
            fighter.stats.deaths,
            fighter.stats.damage_dealt,
        ));
    }
    format!(
        "{headline} — {outcome}\n{}\nResult {} confirmed at tick {}",
        rows.join("\n"),
        result.key.result_id,
        result.final_tick.get()
    )
}

fn native_online_footer(snapshot: &NativeOnlineUiSnapshot) -> &'static str {
    if snapshot.leave_confirmation_open {
        return "Confirm leave  |  Cancel to resume control";
    }
    match snapshot.screen {
        NativeOnlineScreen::OnlineMenu => {
            "1 private  |  2 friends  |  Controller: navigate / accept / back"
        }
        NativeOnlineScreen::JoinPrompt => "Enter accept  |  Esc decline  |  Controller supported",
        NativeOnlineScreen::Lobby => {
            "R ready  |  I invite  |  Controller: navigate / accept / layout"
        }
        NativeOnlineScreen::Results => "Enter rematch  |  Esc lobby  |  Controller supported",
        NativeOnlineScreen::Error | NativeOnlineScreen::Unavailable => {
            "Choose a recovery action  |  Controller layout action opens Steam bindings"
        }
        _ => "Steam authentication and SDR transport remain outside canonical simulation",
    }
}

fn native_online_action_available(
    snapshot: &NativeOnlineUiSnapshot,
    action: NativeOnlineUiAction,
) -> bool {
    if snapshot.leave_confirmation_open {
        return matches!(
            action,
            NativeOnlineUiAction::CancelLeave | NativeOnlineUiAction::LeaveOnline
        );
    }
    let editable = matches!(
        snapshot.screen,
        NativeOnlineScreen::OnlineMenu | NativeOnlineScreen::JoinPrompt | NativeOnlineScreen::Lobby
    );
    match action {
        NativeOnlineUiAction::CreatePrivate => snapshot.actions.create_private,
        NativeOnlineUiAction::CreateFriends => snapshot.actions.create_friends,
        NativeOnlineUiAction::AcceptJoin => snapshot.actions.accept_join,
        NativeOnlineUiAction::DeclineJoin => snapshot.actions.decline_join,
        NativeOnlineUiAction::InviteFriends => snapshot.actions.invite_friends,
        NativeOnlineUiAction::AddSeat => {
            editable
                && snapshot.local_seats < MAX_LOCAL_SEATS
                && snapshot.total_seats < ONLINE_SEAT_CAPACITY
        }
        NativeOnlineUiAction::RemoveSeat => editable && snapshot.local_seats > 1,
        NativeOnlineUiAction::PreviousSeat
        | NativeOnlineUiAction::NextSeat
        | NativeOnlineUiAction::PreviousCharacter
        | NativeOnlineUiAction::NextCharacter
        | NativeOnlineUiAction::PreviousStyle
        | NativeOnlineUiAction::NextStyle
        | NativeOnlineUiAction::PreviousEquipment
        | NativeOnlineUiAction::NextEquipment
        | NativeOnlineUiAction::ToggleTeam => editable,
        NativeOnlineUiAction::PreviousArena
        | NativeOnlineUiAction::NextArena
        | NativeOnlineUiAction::PreviousRules
        | NativeOnlineUiAction::NextRules => snapshot.screen == NativeOnlineScreen::OnlineMenu,
        NativeOnlineUiAction::ToggleReady => snapshot.actions.toggle_ready,
        NativeOnlineUiAction::StartMatch => {
            snapshot.screen == NativeOnlineScreen::Lobby
                && snapshot.role == Some(OnlineLobbyRole::ListenAuthority)
                && snapshot.all_members_ready
                && snapshot.input_delay_calibration.state == InputDelayCalibrationState::Ready
        }
        NativeOnlineUiAction::Rematch => snapshot.actions.rematch,
        NativeOnlineUiAction::ReturnToLobby => snapshot.actions.return_to_lobby,
        NativeOnlineUiAction::RequestLeave => snapshot.actions.leave,
        NativeOnlineUiAction::CancelLeave | NativeOnlineUiAction::LeaveOnline => false,
        NativeOnlineUiAction::ReturnToMenu => snapshot.actions.return_to_menu,
        NativeOnlineUiAction::Retry => snapshot
            .failure
            .is_some_and(|failure| failure.recovery == OnlineRecoveryAction::Retry),
        NativeOnlineUiAction::DismissError => snapshot
            .failure
            .is_some_and(|failure| failure.recovery == OnlineRecoveryAction::Dismiss),
        NativeOnlineUiAction::RequestTrusted | NativeOnlineUiAction::RequestDedicated => false,
    }
}

fn actions_for_recovery(recovery: OnlineRecoveryAction) -> NativeOnlineActions {
    match recovery {
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
}

fn quality_label(quality: NetworkQuality) -> &'static str {
    match quality {
        NetworkQuality::Healthy => "GOOD",
        NetworkQuality::Warning => "WARNING",
        NetworkQuality::Degraded => "DEGRADED",
        NetworkQuality::Reject => "UNPLAYABLE",
    }
}

fn input_delay_calibration_text(calibration: InputDelayCalibrationSnapshot) -> String {
    match calibration.state {
        InputDelayCalibrationState::NotAuthority => {
            "Input delay: authority is calibrating".to_owned()
        }
        InputDelayCalibrationState::Calibrating => format!(
            "Latency calibration: {}/{} peers",
            calibration.calibrated_peer_count, calibration.remote_peer_count
        ),
        InputDelayCalibrationState::Ready => format!(
            "Latency p95: {} ms  |  Input delay: {} ticks  |  Rollback required: {}",
            calibration.worst_p95_rtt_ms.unwrap_or_default(),
            calibration.selected_input_delay_ticks.unwrap_or_default(),
            calibration.required_rollback_ticks.unwrap_or_default(),
        ),
        InputDelayCalibrationState::Unplayable => format!(
            "Latency p95: {} ms  |  Match cannot start",
            calibration.worst_p95_rtt_ms.unwrap_or_default(),
        ),
        InputDelayCalibrationState::Committed => format!(
            "Input delay committed: {} ticks",
            calibration.selected_input_delay_ticks.unwrap_or_default(),
        ),
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "YES" } else { "NO" }
}

fn character_label_for_definition(id: DefinitionId) -> &'static str {
    match id.get() {
        0 => "CAT",
        1 => "PIG",
        2 => "DOG",
        3 => "FOX",
        4 => "PANDA",
        5 => "BEE",
        6 => "PENGUIN",
        7 => "CHICK",
        _ => "UNKNOWN",
    }
}

fn style_label_for_definition(id: DefinitionId) -> &'static str {
    match id.get() {
        0 => "ANCHOR",
        1 => "VECTOR",
        2 => "CATALYST",
        _ => "UNKNOWN",
    }
}

fn equipment_label_for_definition(id: DefinitionId) -> &'static str {
    match id.get() {
        0 => "DASH COIL",
        1 => "AERIAL SPUR",
        2 => "COUNTER CELL",
        3 => "HEAVY SEAL",
        _ => "UNKNOWN",
    }
}

/// English fallback table keyed by the stable localization contract. A future
/// locale bundle can replace these values without changing runtime state or
/// exposing backend strings.
pub fn online_localized(key: &str) -> &'static str {
    match key {
        "online.available" => "Online play is available.",
        "online.unavailable.steam_feature_disabled" => {
            "Online play requires the Steam networking build."
        }
        "online.unavailable.unsupported_platform" => {
            "Online play is not supported on this platform."
        }
        "online.unavailable.app_id_missing" => "The Steam App ID is not configured.",
        "online.unavailable.app_id_invalid" => "The Steam App ID configuration is invalid.",
        "online.unavailable.spacewar_requires_explicit_opt_in" => {
            "Steam App ID 480 is development-only and requires explicit opt-in."
        }
        "online.unavailable.steam_initialization_failed" => {
            "Steam could not initialize. Check that Steam is running and you are signed in."
        }
        "online.screen.menu" => "Choose a private or friends-only online match.",
        "online.screen.join_prompt" => "A Steam lobby invitation is waiting.",
        "online.screen.creating_lobby" => "Creating the Steam lobby…",
        "online.screen.joining_lobby" => "Joining the Steam lobby…",
        "online.screen.lobby" => "Configure couch seats and ready up.",
        "online.screen.connecting" => "Opening the Steam relay connection…",
        "online.screen.authenticating" => "Authenticating lobby ownership and licenses…",
        "online.screen.manifest_agreement" => "Verifying versions, content, roster, and rules…",
        "online.screen.loading" => "Loading the agreed gameplay content…",
        "online.screen.ready" => "Waiting for the authority countdown…",
        "online.screen.countdown" => "The authority countdown is active.",
        "online.screen.fighting" => "The online match is active.",
        "online.screen.reconnecting" => "Reconnecting to the same authenticated match…",
        "online.screen.confirming_result" => "Confirming the final authoritative result…",
        "online.screen.results" => "The online match has ended.",
        "online.screen.returning_to_lobby" => "Returning everyone to the lobby…",
        "online.error.steam_unavailable" => "Steam online services are unavailable.",
        "online.error.steam_disconnected" => "Steam disconnected.",
        "online.error.overlay_unavailable" => {
            "Steam Overlay is unavailable. Enable it in Steam, then try again."
        }
        "online.error.release_configuration" => "The online release configuration is invalid.",
        "online.error.lobby_unavailable" => "That lobby is no longer available.",
        "online.error.lobby_full" => "That lobby is full.",
        "online.error.lobby_closed" => "The lobby was closed.",
        "online.error.invite_required" => "An invitation is required for that lobby.",
        "online.error.friend_required" => "That lobby is limited to friends.",
        "online.error.not_lobby_owner" => "Only the lobby owner can do that.",
        "online.error.public_play_disabled" => "Ranked and trusted online play are not enabled.",
        "online.error.invalid_seat_count" => "The couch seat selection is invalid.",
        "online.error.incompatible_version" => {
            "The players have incompatible game content or versions."
        }
        "online.error.authentication_failed" => "Steam authentication failed.",
        "online.error.ownership_failed" => "A player does not own or cannot access the game.",
        "online.error.platform_banned" => "A platform or publisher ban prevents this session.",
        "online.error.authentication_timeout" => "Steam authentication timed out.",
        "online.error.connection_timeout" => "The network connection timed out.",
        "online.error.network_quality_rejected" => {
            "The connection did not meet the sustained network-quality requirement."
        }
        "online.error.loading_timeout" => "A player took too long to load.",
        "online.error.synchronization_failed" => "The match could not synchronize safely.",
        "online.error.clock_sync_failed" => "The match clocks could not synchronize.",
        "online.error.invalid_input" => "Invalid gameplay input was rejected.",
        "online.error.malformed_traffic" => "Malformed network traffic was rejected.",
        "online.error.rate_limited" => "A player exceeded the network rate limit.",
        "online.error.kicked" => "You were removed from the match.",
        "online.error.authority_lost" => "The listen host was lost; the match is a no contest.",
        "online.error.server_shutdown" => "The authority shut down.",
        "online.error.dedicated_unavailable" => {
            "Dedicated servers are not enabled for this release."
        }
        "online.error.capacity" => "An internal online capacity limit was reached.",
        "online.error.internal" | _ => "The online session ended because of an internal error.",
    }
}

fn mix_online_identity(mut value: u64) -> u64 {
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn generate_online_match_nonce_with<E>(
    fill: impl FnOnce(&mut [u8]) -> Result<(), E>,
) -> Result<OnlineMatchNonce, NativeOnlineApplicationError> {
    let mut bytes = [0_u8; 32];
    fill(&mut bytes).map_err(|_| NativeOnlineApplicationError::EntropyUnavailable)?;
    Ok(OnlineMatchNonce(bytes))
}

fn derive_online_identity(
    user: AuthenticatedUserId,
    nonce: OnlineMatchNonce,
    counter: u64,
    now_ms: u64,
    domain: u64,
) -> u64 {
    let mut state = mix_online_identity(domain ^ user.get());
    for (index, chunk) in nonce.0.chunks_exact(8).enumerate() {
        let word = u64::from_le_bytes(
            chunk
                .try_into()
                .expect("eight-byte nonce chunks are structurally fixed"),
        );
        state = mix_online_identity(
            state
                ^ word.rotate_left((index as u32).wrapping_mul(13))
                ^ (index as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15),
        );
    }
    state = mix_online_identity(state ^ counter ^ 0xd1b5_4a32_d192_ed03);
    mix_online_identity(state ^ now_ms.rotate_left(23) ^ domain.rotate_right(11))
}

fn online_match_id(
    user: AuthenticatedUserId,
    nonce: OnlineMatchNonce,
    counter: u64,
    now_ms: u64,
) -> Result<MatchId, NativeOnlineApplicationError> {
    let mut bytes = [0_u8; 16];
    bytes[..8].copy_from_slice(
        &derive_online_identity(user, nonce, counter, now_ms, 0x4146_432d_4d41_5443).to_le_bytes(),
    );
    bytes[8..].copy_from_slice(
        &derive_online_identity(user, nonce, counter, now_ms, 0x4849_442d_4f4e_4c49).to_le_bytes(),
    );
    // MatchId reserves the all-zero bit pattern. Keep generation total and
    // deterministic even for an injected all-zero nonce or a theoretical hash
    // collision onto the reserved value.
    if bytes.iter().all(|byte| *byte == 0) {
        bytes[0] = 1;
    }
    MatchId::new(bytes).map_err(|_| NativeOnlineApplicationError::TimelineExhausted)
}

fn online_master_gameplay_seed(
    user: AuthenticatedUserId,
    nonce: OnlineMatchNonce,
    counter: u64,
    now_ms: u64,
) -> u64 {
    derive_online_identity(user, nonce, counter, now_ms, 0x5345_4544_2d41_4643)
}

impl NativeOnlineApplication {
    /// Consumes all bounded runtime handoffs, services worker state, and drives
    /// guarded coordinator transitions. Call once after pumping the native
    /// platform/runtime for the current application frame.
    pub fn pump<R: NativeOnlineRuntimePort>(
        &mut self,
        runtime: &mut R,
        now_ms: u64,
    ) -> Result<(), NativeOnlineApplicationError> {
        if self.awaiting_transport_retirement && !runtime.transport_retirement_pending_port() {
            self.awaiting_transport_retirement = false;
        }

        let mut event_count = 0_usize;
        while let Some(event) = runtime.poll_event_port() {
            event_count += 1;
            if event_count > crate::native_online::MAX_NATIVE_ONLINE_EVENTS {
                return self.fail(NativeOnlineApplicationError::EndpointCapacity);
            }
            self.metrics.runtime_events = self.metrics.runtime_events.saturating_add(1);
            self.handle_runtime_event(runtime, event, now_ms)?;
        }

        while let Some(endpoint) = runtime.take_endpoint_port() {
            if self.listen_shutdown.is_some() || self.awaiting_transport_retirement {
                // A shutdown fence has already been raised. Dropping the
                // application endpoint closes this late generation; it must
                // never be attached to the retiring or next match.
                continue;
            }
            if self.staged_endpoints.len() >= ONLINE_MAX_STAGED_ENDPOINTS {
                return self.fail(NativeOnlineApplicationError::EndpointCapacity);
            }
            self.staged_endpoints.push_back(endpoint);
            self.metrics.endpoints_staged = self.metrics.endpoints_staged.saturating_add(1);
        }

        self.try_start_session(runtime)?;
        self.retry_authority_commands()?;
        self.drain_authority_update(runtime, now_ms)?;
        self.service_pending_authority_revocations()?;
        self.service_pending_authority_detaches()?;
        self.route_staged_endpoints()?;
        if self.listen_shutdown.is_none() {
            self.drive_coordinator_from_worker(runtime, now_ms)?;
        }
        if self.awaiting_transport_retirement && !runtime.transport_retirement_pending_port() {
            self.awaiting_transport_retirement = false;
        }
        Ok(())
    }

    fn fail<T>(
        &mut self,
        error: NativeOnlineApplicationError,
    ) -> Result<T, NativeOnlineApplicationError> {
        if matches!(
            error,
            NativeOnlineApplicationError::EndpointCapacity
                | NativeOnlineApplicationError::AuthorityCommandCapacity
        ) {
            // Capacity exhaustion is fatal and must not strand an online
            // worker or any endpoint/authority retry handoff behind Error.
            self.clear_active_match();
        }
        self.observe_application_error(&error);
        Err(error)
    }

    fn handle_runtime_event<R: NativeOnlineRuntimePort>(
        &mut self,
        runtime: &mut R,
        event: OnlineLobbyEvent,
        now_ms: u64,
    ) -> Result<(), NativeOnlineApplicationError> {
        match event {
            OnlineLobbyEvent::JoinRequested(intent) => {
                self.pending_join = Some(intent);
                self.request_online_focus = true;
            }
            OnlineLobbyEvent::PeerAuthenticated { user, peer_id, .. } => {
                self.install_peer_binding(user, peer_id)?
            }
            OnlineLobbyEvent::PeerAuthenticationRejected {
                user,
                connection,
                failure,
            } => {
                let remote_listen_peer = runtime.view_model().role
                    == Some(OnlineLobbyRole::ListenAuthority)
                    && runtime
                        .local_authenticated_user_port()
                        .is_some_and(|local| local.get() != user.get());
                if remote_listen_peer {
                    self.forward_authentication_failure(user, connection, failure)?;
                    self.isolate_remote_peer_state(user);
                } else {
                    // A client rejected by its authority, or a failure
                    // attributable to the local listen owner, ends this
                    // process's online session. Remote listen peers are
                    // isolated above without poisoning the host UI.
                    self.isolate_remote_peer_state(user);
                    self.clear_active_match();
                    self.failure_override = Some(failure);
                }
            }
            OnlineLobbyEvent::PeerDisconnected {
                connection,
                peer_id,
                reconnect_allowed,
                ..
            } => {
                if self.active_session_kind() == Some(NativeOnlineSessionKind::ListenOwner)
                    && !self.pending_authority_detaches.iter().any(|pending| {
                        pending.peer_id == peer_id && pending.steam_connection == connection
                    })
                {
                    if self.pending_authority_detaches.len() >= MAX_STEAM_LOBBY_MEMBERS {
                        return self.fail(NativeOnlineApplicationError::EndpointCapacity);
                    }
                    // A generation-specific terminal may already be queued by
                    // the authority. Defer this peer-only transport signal so
                    // the exact terminal event gets the first chance to win.
                    self.pending_authority_detaches
                        .push_back(PendingAuthorityDetach {
                            peer_id,
                            steam_connection: connection,
                            defer_once: true,
                        });
                }
                if !reconnect_allowed
                    && runtime.view_model().role == Some(OnlineLobbyRole::Client)
                    && self.authority_disconnect.is_none()
                {
                    self.failure_override = Some(OnlineFailure {
                        code: OnlineFailureCode::AuthorityLost,
                        severity: OnlineFailureSeverity::MatchEnded,
                        recovery: OnlineRecoveryAction::MatchEndedNoContest,
                        detail_code: 0,
                    });
                }
            }
            OnlineLobbyEvent::QualityChanged { user, quality } => {
                if runtime.view_model().role == Some(OnlineLobbyRole::Client)
                    && self.remote_quality_user == Some(user)
                    && let Some(ActiveNativeOnlineMatch::Remote(client)) = &self.active
                {
                    let _ = client.submit_quality_sample(NetworkQualitySample {
                        rtt_ms: quality.average_rtt_ms,
                        loss_bps: quality.average_loss_bps,
                    });
                }
            }
            OnlineLobbyEvent::RosterChanged { live_bindings, .. } => {
                self.reconcile_peer_bindings(live_bindings);
            }
            OnlineLobbyEvent::DropGameplayEndpoints => {
                if self.active_session_kind() == Some(NativeOnlineSessionKind::ListenOwner)
                    && self.listen_shutdown.is_none()
                {
                    self.begin_listen_shutdown(
                        runtime,
                        ListenShutdownAction::CoordinatorDrop,
                        now_ms,
                    )?;
                } else if self.listen_shutdown.is_none() {
                    self.clear_active_match();
                }
            }
            OnlineLobbyEvent::ReturnedToLobby { .. } => {
                self.clear_active_match();
                self.reset_match_gates();
            }
            OnlineLobbyEvent::MatchEnded(OnlineMatchOutcome::NoContestHostLost) => {
                self.clear_active_match();
            }
            OnlineLobbyEvent::Failure(failure) => {
                if self.authority_disconnect.is_none() {
                    self.failure_override = Some(failure);
                }
            }
            OnlineLobbyEvent::StateChanged { to, .. }
                if to == crate::online_lobby::OnlineLobbyPhase::OfflineMenu =>
            {
                self.clear_active_match();
                self.failure_override = None;
            }
            OnlineLobbyEvent::LobbyEntered { .. }
            | OnlineLobbyEvent::TransportRequested(_)
            | OnlineLobbyEvent::AuthenticationRequired { .. }
            | OnlineLobbyEvent::AuthTicketReady { .. }
            | OnlineLobbyEvent::EndpointReady { .. }
            | OnlineLobbyEvent::ManifestCommitted(_)
            | OnlineLobbyEvent::MatchEnded(OnlineMatchOutcome::Confirmed)
            | OnlineLobbyEvent::RichPresenceUnavailable
            | OnlineLobbyEvent::StateChanged { .. } => {}
        }
        Ok(())
    }

    fn reconcile_peer_bindings(
        &mut self,
        live_bindings: [Option<OnlinePeerIdentity>; MAX_STEAM_LOBBY_MEMBERS],
    ) {
        let is_live = |user: SteamUserId, peer_id: PeerId| {
            live_bindings
                .iter()
                .flatten()
                .any(|identity| identity.user == user && identity.peer_id == peer_id)
        };
        let mut removed = [None; MAX_STEAM_LOBBY_MEMBERS];
        let mut removed_len = 0_usize;
        for binding in self.bindings.iter().flatten().copied() {
            if !is_live(binding.user, binding.peer_id) {
                removed[removed_len] = Some(binding.user);
                removed_len += 1;
            }
        }
        for user in removed[..removed_len].iter().flatten().copied() {
            self.isolate_remote_peer_state(user);
        }
        if self.remote_quality_user.is_some_and(|user| {
            !live_bindings
                .iter()
                .flatten()
                .any(|identity| identity.user == user)
        }) {
            self.remote_quality_user = None;
        }
    }

    fn install_peer_binding(
        &mut self,
        user: SteamUserId,
        peer_id: PeerId,
    ) -> Result<(), NativeOnlineApplicationError> {
        if let Some(binding) = self
            .bindings
            .iter_mut()
            .flatten()
            .find(|binding| binding.user == user || binding.peer_id == peer_id)
        {
            if binding.user != user || binding.peer_id != peer_id {
                return Err(NativeOnlineApplicationError::Runtime);
            }
            return Ok(());
        }
        let Some(slot) = self.bindings.iter_mut().find(|slot| slot.is_none()) else {
            return self.fail(NativeOnlineApplicationError::EndpointCapacity);
        };
        *slot = Some(AuthenticatedPeerBinding { user, peer_id });
        Ok(())
    }

    fn forward_authentication_failure(
        &mut self,
        user: SteamUserId,
        steam_connection: Option<SteamConnectionId>,
        failure: OnlineFailure,
    ) -> Result<(), NativeOnlineApplicationError> {
        if self.active_session_kind() != Some(NativeOnlineSessionKind::ListenOwner) {
            return Ok(());
        }
        let peer_id = self
            .bindings
            .iter()
            .flatten()
            .find(|binding| binding.user == user)
            .map(|binding| binding.peer_id);
        if failure.code == OnlineFailureCode::PlatformBanned {
            self.mark_authority_user_terminal_expected(user, peer_id);
            self.metrics.platform_bans_forwarded =
                self.metrics.platform_bans_forwarded.saturating_add(1);
            let Some(ActiveNativeOnlineMatch::Listen(online_match)) = &self.active else {
                return Ok(());
            };
            let outcome = online_match
                .authority
                .try_enforce_platform_ban(user.authenticated());
            return self.retain_authority_outcome(outcome);
        }

        let Some(steam_connection) = steam_connection else {
            // Rejections before an endpoint generation exists require no
            // authority-side revoke. The staged/queued attach is removed by
            // the normal identity-isolation path.
            return Ok(());
        };
        if self
            .pending_authority_revocations
            .iter()
            .any(|pending| pending.user == user && pending.steam_connection == steam_connection)
        {
            return Ok(());
        }
        if self.pending_authority_revocations.len() >= MAX_STEAM_LOBBY_MEMBERS {
            return self.fail(NativeOnlineApplicationError::EndpointCapacity);
        }
        self.pending_authority_revocations
            .push_back(PendingAuthorityRevocation {
                user,
                steam_connection,
            });
        self.metrics.authentication_revocations_forwarded = self
            .metrics
            .authentication_revocations_forwarded
            .saturating_add(1);
        Ok(())
    }

    fn mark_authority_user_terminal_expected(
        &mut self,
        user: SteamUserId,
        peer_id: Option<PeerId>,
    ) {
        for record in self.authority_endpoints.iter_mut().flatten() {
            if record.user != user || peer_id.is_some_and(|peer_id| record.peer_id != peer_id) {
                continue;
            }
            match &mut record.state {
                AuthorityEndpointState::Submitted {
                    terminal_expected, ..
                }
                | AuthorityEndpointState::Attached {
                    terminal_expected, ..
                } => *terminal_expected = true,
                AuthorityEndpointState::TerminalDrained { .. } => {}
            }
        }
    }

    fn isolate_remote_peer_state(&mut self, user: SteamUserId) {
        let peer_id = self
            .bindings
            .iter()
            .flatten()
            .find(|binding| binding.user == user)
            .map(|binding| binding.peer_id);
        for slot in &mut self.bindings {
            if slot.is_some_and(|binding| binding.user == user) {
                *slot = None;
            }
        }
        if self.remote_quality_user == Some(user) {
            self.remote_quality_user = None;
        }
        self.staged_endpoints
            .retain(|endpoint| endpoint.admitted.remote_user != user);
        let retained_before = self.pending_authority_commands.len();
        self.pending_authority_commands
            .retain(|command| !match command {
                ListenAuthorityCommand::AttachInitial {
                    peer_id: command_peer,
                    user_id,
                    ..
                } => {
                    user_id.get() == user.get()
                        || peer_id.is_some_and(|peer_id| peer_id == *command_peer)
                }
                ListenAuthorityCommand::AttachReconnect { user_id, .. } => {
                    user_id.get() == user.get()
                }
                _ => false,
            });
        if self.pending_authority_commands.len() != retained_before {
            for record in &mut self.authority_endpoints {
                if record.is_some_and(|record| {
                    record.user == user
                        && matches!(record.state, AuthorityEndpointState::Submitted { .. })
                }) {
                    *record = None;
                }
            }
        }
    }

    fn try_start_session<R: NativeOnlineRuntimePort>(
        &mut self,
        runtime: &R,
    ) -> Result<(), NativeOnlineApplicationError> {
        if self.active.is_some()
            || self.listen_shutdown.is_some()
            || self.awaiting_transport_retirement
        {
            return Ok(());
        }
        let view = runtime.view_model();
        let Some(config) = runtime.match_config_port() else {
            return Ok(());
        };
        match view.role {
            Some(OnlineLobbyRole::ListenAuthority) => {
                let committed = runtime
                    .committed_peers_port()
                    .ok_or(NativeOnlineApplicationError::MissingCommittedRoster)?;
                let local_user = runtime
                    .local_authenticated_user_port()
                    .ok_or(NativeOnlineApplicationError::MissingLocalIdentity)?;
                let host = committed
                    .iter()
                    .find(|peer| peer.user_id == local_user)
                    .copied()
                    .ok_or(NativeOnlineApplicationError::MissingListenHost)?;
                let roster =
                    ListenAuthenticatedRoster::new(&config, host, committed.iter().copied())
                        .map_err(|_| NativeOnlineApplicationError::MissingCommittedRoster)?;
                #[cfg(test)]
                let online_match = if let Some(root) = self.listen_diagnostics_root.clone() {
                    ListenOnlineMatch::spawn_with_diagnostics_root(
                        config,
                        roster,
                        ListenAuthorityConfig::default(),
                        RemoteOnlineClientConfig::default(),
                        root,
                    )
                } else {
                    ListenOnlineMatch::spawn(
                        config,
                        roster,
                        ListenAuthorityConfig::default(),
                        RemoteOnlineClientConfig::default(),
                    )
                }
                .map_err(|_| NativeOnlineApplicationError::ListenStart)?;
                #[cfg(not(test))]
                let online_match = ListenOnlineMatch::spawn(
                    config,
                    roster,
                    ListenAuthorityConfig::default(),
                    RemoteOnlineClientConfig::default(),
                )
                .map_err(|_| NativeOnlineApplicationError::ListenStart)?;
                if self.content_ready {
                    online_match.host_client.mark_content_loaded();
                }
                self.active = Some(ActiveNativeOnlineMatch::Listen(online_match));
                self.metrics.sessions_started = self.metrics.sessions_started.saturating_add(1);
            }
            Some(OnlineLobbyRole::Client) => {
                let Some(index) = self
                    .staged_endpoints
                    .iter()
                    .position(|endpoint| !endpoint.reconnect)
                else {
                    return Ok(());
                };
                let handoff = self
                    .staged_endpoints
                    .remove(index)
                    .expect("located staged endpoint still exists");
                let remote_quality_user = handoff.admitted.remote_user;
                let local_peer = self
                    .local_peer_id
                    .ok_or(NativeOnlineApplicationError::MissingLocalPeer)?;
                let client = RemoteOnlineClient::spawn(
                    handoff.admitted.endpoint,
                    config,
                    local_peer,
                    RemoteOnlineClientConfig::default(),
                )?;
                if self.content_ready {
                    client.mark_content_loaded();
                }
                self.active = Some(ActiveNativeOnlineMatch::Remote(client));
                self.remote_quality_user = Some(remote_quality_user);
                self.metrics.sessions_started = self.metrics.sessions_started.saturating_add(1);
            }
            None => {}
        }
        Ok(())
    }

    fn route_staged_endpoints(&mut self) -> Result<(), NativeOnlineApplicationError> {
        if self.listen_shutdown.is_some() || self.awaiting_transport_retirement {
            self.staged_endpoints.clear();
            return Ok(());
        }
        let count = self.staged_endpoints.len();
        for _ in 0..count {
            let handoff = self
                .staged_endpoints
                .pop_front()
                .expect("bounded endpoint count came from queue length");
            let Some(active_kind) = self.active_session_kind() else {
                self.staged_endpoints.push_front(handoff);
                break;
            };
            match active_kind {
                NativeOnlineSessionKind::ListenOwner => {
                    self.attach_listen_endpoint(handoff)?;
                }
                NativeOnlineSessionKind::RemoteClient => {
                    if !handoff.reconnect {
                        return Err(NativeOnlineApplicationError::InvalidAction);
                    }
                    if self.remote_quality_user != Some(handoff.admitted.remote_user) {
                        return Err(NativeOnlineApplicationError::Runtime);
                    }
                    let ActiveNativeOnlineMatch::Remote(client) =
                        self.active.as_mut().expect("active kind was just checked")
                    else {
                        unreachable!("active kind and active match agree");
                    };
                    client.reconnect(handoff.admitted.endpoint)?;
                    self.authority_disconnect = None;
                    self.failure_override = None;
                    // The manifest and content gates remain satisfied across a
                    // same-match reconnect, but the coordinator must observe the
                    // replacement transport's freshly applied authoritative
                    // snapshot before it can resume Fighting.
                    self.initial_sync_marked = false;
                    self.projected_confirmed_result = None;
                    if self.content_ready {
                        client.mark_content_loaded();
                    }
                }
            }
        }
        Ok(())
    }

    fn attach_listen_endpoint(
        &mut self,
        handoff: NativeOnlineEndpoint,
    ) -> Result<(), NativeOnlineApplicationError> {
        let record = AuthorityEndpointRecord {
            user: handoff.admitted.remote_user,
            peer_id: handoff.peer_id,
            steam_connection: handoff.admitted.connection,
            state: AuthorityEndpointState::Submitted {
                reconnect: handoff.reconnect,
                terminal_expected: false,
            },
        };
        self.retain_authority_endpoint_submission(record)?;
        let ActiveNativeOnlineMatch::Listen(online_match) = self
            .active
            .as_ref()
            .expect("listen endpoint is routed only with an active match")
        else {
            return Err(NativeOnlineApplicationError::InvalidAction);
        };
        let authenticated_user = handoff.admitted.admission.authenticated_user;
        let outcome = if handoff.reconnect {
            // The remote client's last-confirmed value is an optimization hint,
            // not trusted authority state. Tick zero is always conservative;
            // the hub validates identity/grace and transfers its current
            // retained snapshot plus canonical input tail.
            let claim = ReconnectClaim {
                match_id: online_match.host_client.manifest().match_id,
                peer_id: handoff.peer_id,
                last_confirmed_tick: SimTick::ZERO,
            };
            self.metrics.conservative_full_resyncs =
                self.metrics.conservative_full_resyncs.saturating_add(1);
            online_match.authority.try_attach_reconnect(
                authenticated_user,
                claim,
                handoff.admitted.endpoint,
            )
        } else {
            online_match.authority.try_attach_initial(
                handoff.peer_id,
                authenticated_user,
                handoff.admitted.endpoint,
            )
        };
        if let Err(error) = self.retain_authority_outcome(outcome) {
            self.remove_submitted_authority_endpoint(
                record.peer_id,
                handoff.reconnect,
                Some(record.steam_connection),
            );
            return Err(error);
        }
        Ok(())
    }

    fn retain_authority_endpoint_submission(
        &mut self,
        record: AuthorityEndpointRecord,
    ) -> Result<(), NativeOnlineApplicationError> {
        if record.user.get()
            != self
                .bindings
                .iter()
                .flatten()
                .find(|binding| binding.peer_id == record.peer_id)
                .map_or(record.user.get(), |binding| binding.user.get())
        {
            return Err(NativeOnlineApplicationError::Runtime);
        }
        if self.authority_endpoints.iter().flatten().any(|existing| {
            existing.steam_connection == record.steam_connection
                || (existing.peer_id == record.peer_id
                    && matches!(existing.state, AuthorityEndpointState::Submitted { .. }))
        }) {
            return Err(NativeOnlineApplicationError::Runtime);
        }
        let Some(slot) = self
            .authority_endpoints
            .iter_mut()
            .find(|slot| slot.is_none())
        else {
            return Err(NativeOnlineApplicationError::EndpointCapacity);
        };
        *slot = Some(record);
        Ok(())
    }

    fn remove_submitted_authority_endpoint(
        &mut self,
        peer_id: PeerId,
        reconnect: bool,
        steam_connection: Option<SteamConnectionId>,
    ) {
        let matches: ArrayVec<usize, ONLINE_MAX_AUTHORITY_ENDPOINT_RECORDS> = self
            .authority_endpoints
            .iter()
            .enumerate()
            .filter_map(|(index, record)| {
                record
                    .is_some_and(|record| {
                        record.peer_id == peer_id
                            && steam_connection
                                .is_none_or(|connection| record.steam_connection == connection)
                            && matches!(
                                record.state,
                                AuthorityEndpointState::Submitted {
                                    reconnect: existing,
                                    ..
                                } if existing == reconnect
                            )
                    })
                    .then_some(index)
            })
            .collect();
        if matches.len() == 1 {
            self.authority_endpoints[matches[0]] = None;
        }
    }

    fn promote_authority_endpoint(
        &mut self,
        peer_id: PeerId,
        connection: AuthorityConnectionId,
        reconnect: bool,
    ) -> Result<(), NativeOnlineApplicationError> {
        let mut match_index = None;
        for (index, record) in self.authority_endpoints.iter().enumerate() {
            if record.is_some_and(|record| {
                record.peer_id == peer_id
                    && matches!(
                        record.state,
                        AuthorityEndpointState::Submitted {
                            reconnect: existing,
                            ..
                        } if existing == reconnect
                    )
            }) {
                if match_index.is_some() {
                    return Err(NativeOnlineApplicationError::Runtime);
                }
                match_index = Some(index);
            }
        }
        let Some(index) = match_index else {
            // A close/rejection may have won before the worker published its
            // attach. Without the exact submitted Steam generation, never
            // guess or bind this authority generation to a replacement.
            return Ok(());
        };
        let terminal_expected = match self.authority_endpoints[index]
            .expect("matched authority endpoint record")
            .state
        {
            AuthorityEndpointState::Submitted {
                terminal_expected, ..
            } => terminal_expected || self.listen_shutdown.is_some(),
            _ => unreachable!("only submitted records are promoted"),
        };
        self.authority_endpoints[index]
            .as_mut()
            .expect("matched authority endpoint record")
            .state = AuthorityEndpointState::Attached {
            connection,
            terminal_expected,
        };
        Ok(())
    }

    fn mark_authority_endpoint_terminal_expected(
        &mut self,
        peer_id: PeerId,
        user_id: AuthenticatedUserId,
        connection: AuthorityConnectionId,
    ) {
        for record in self.authority_endpoints.iter_mut().flatten() {
            if record.peer_id == peer_id
                && record.user.get() == user_id.get()
                && matches!(
                    record.state,
                    AuthorityEndpointState::Attached {
                        connection: existing,
                        ..
                    } if existing == connection
                )
            {
                record.state = AuthorityEndpointState::Attached {
                    connection,
                    terminal_expected: true,
                };
            }
        }
    }

    fn retain_authority_terminal_drained(
        &mut self,
        peer_id: PeerId,
        user_id: AuthenticatedUserId,
        connection: AuthorityConnectionId,
        retry: Option<RetryDisposition>,
    ) {
        let mut match_index = None;
        for (index, record) in self.authority_endpoints.iter().enumerate() {
            if record.is_some_and(|record| {
                record.peer_id == peer_id
                    && record.user.get() == user_id.get()
                    && matches!(
                        record.state,
                        AuthorityEndpointState::Attached {
                            connection: existing,
                            ..
                        } if existing == connection
                    )
            }) {
                if match_index.is_some() {
                    self.metrics.stale_authority_terminal_events = self
                        .metrics
                        .stale_authority_terminal_events
                        .saturating_add(1);
                    return;
                }
                match_index = Some(index);
            }
        }
        let Some(index) = match_index else {
            self.metrics.stale_authority_terminal_events = self
                .metrics
                .stale_authority_terminal_events
                .saturating_add(1);
            return;
        };
        self.authority_endpoints[index]
            .as_mut()
            .expect("matched authority endpoint record")
            .state = AuthorityEndpointState::TerminalDrained { connection, retry };
    }

    fn retry_authority_terminal_marks<R: NativeOnlineRuntimePort>(
        &mut self,
        runtime: &mut R,
        now_ms: u64,
    ) -> Result<(), NativeOnlineApplicationError> {
        for index in 0..ONLINE_MAX_AUTHORITY_ENDPOINT_RECORDS {
            let Some(record) = self.authority_endpoints[index] else {
                continue;
            };
            let AuthorityEndpointState::TerminalDrained { retry, .. } = record.state else {
                continue;
            };
            runtime.execute_port(
                NativeOnlineCommand::MarkAuthorityTerminalDrained {
                    user: record.user,
                    peer_id: record.peer_id,
                    connection: record.steam_connection,
                    retry,
                },
                now_ms,
            )?;
            self.authority_endpoints[index] = None;
            self.metrics.authority_terminal_marks =
                self.metrics.authority_terminal_marks.saturating_add(1);
        }
        Ok(())
    }

    fn observe_authority_detached(&mut self, peer_id: PeerId, connection: AuthorityConnectionId) {
        let mut candidate = None;
        for (index, record) in self.authority_endpoints.iter().enumerate() {
            if record.is_some_and(|record| {
                record.peer_id == peer_id
                    && matches!(
                        record.state,
                        AuthorityEndpointState::Attached {
                            connection: existing,
                            ..
                        } if existing == connection
                    )
            }) {
                if candidate.is_some() {
                    return;
                }
                candidate = Some(index);
            }
        }
        if let Some(index) = candidate {
            self.authority_endpoints[index] = None;
        }
    }

    fn retain_authority_outcome(
        &mut self,
        outcome: ListenAuthoritySubmitOutcome,
    ) -> Result<(), NativeOnlineApplicationError> {
        match outcome {
            ListenAuthoritySubmitOutcome::Queued => Ok(()),
            ListenAuthoritySubmitOutcome::Full(command) => {
                if self.pending_authority_commands.len() >= ONLINE_MAX_PENDING_AUTHORITY_COMMANDS {
                    return self.fail(NativeOnlineApplicationError::AuthorityCommandCapacity);
                }
                self.pending_authority_commands.push_back(command);
                Ok(())
            }
            ListenAuthoritySubmitOutcome::Disconnected(_) => {
                Err(NativeOnlineApplicationError::AuthorityDisconnected)
            }
        }
    }

    fn retry_authority_commands(&mut self) -> Result<(), NativeOnlineApplicationError> {
        let count = self.pending_authority_commands.len();
        for _ in 0..count {
            let command = self
                .pending_authority_commands
                .pop_front()
                .expect("bounded retry count came from queue length");
            let Some(ActiveNativeOnlineMatch::Listen(online_match)) = &self.active else {
                return Err(NativeOnlineApplicationError::AuthorityDisconnected);
            };
            self.metrics.authority_commands_retried =
                self.metrics.authority_commands_retried.saturating_add(1);
            match online_match.authority.try_submit(command) {
                ListenAuthoritySubmitOutcome::Queued => {}
                ListenAuthoritySubmitOutcome::Full(command) => {
                    self.pending_authority_commands.push_back(command);
                    break;
                }
                ListenAuthoritySubmitOutcome::Disconnected(_) => {
                    return Err(NativeOnlineApplicationError::AuthorityDisconnected);
                }
            }
        }
        Ok(())
    }

    fn service_pending_authority_revocations(
        &mut self,
    ) -> Result<(), NativeOnlineApplicationError> {
        if self.listen_shutdown.is_some() {
            self.pending_authority_revocations.clear();
            return Ok(());
        }
        let count = self.pending_authority_revocations.len();
        for _ in 0..count {
            let pending = self
                .pending_authority_revocations
                .pop_front()
                .expect("bounded revocation count came from queue length");
            let matches: ArrayVec<usize, ONLINE_MAX_AUTHORITY_ENDPOINT_RECORDS> = self
                .authority_endpoints
                .iter()
                .enumerate()
                .filter_map(|(index, record)| {
                    record
                        .as_ref()
                        .is_some_and(|record| {
                            record.user == pending.user
                                && record.steam_connection == pending.steam_connection
                        })
                        .then_some(index)
                })
                .collect();
            let [index] = matches.as_slice() else {
                if matches.is_empty() {
                    continue;
                }
                return Err(NativeOnlineApplicationError::Runtime);
            };
            let record = self.authority_endpoints[*index].expect("matched endpoint record");
            match record.state {
                AuthorityEndpointState::Submitted {
                    terminal_expected: false,
                    ..
                } => self.pending_authority_revocations.push_back(pending),
                AuthorityEndpointState::Attached {
                    connection,
                    terminal_expected: false,
                } => {
                    self.authority_endpoints[*index]
                        .as_mut()
                        .expect("matched endpoint record")
                        .state = AuthorityEndpointState::Attached {
                        connection,
                        terminal_expected: true,
                    };
                    let outcome = match &self.active {
                        Some(ActiveNativeOnlineMatch::Listen(online_match)) => {
                            online_match.authority.try_revoke_authentication(connection)
                        }
                        _ => continue,
                    };
                    self.retain_authority_outcome(outcome)?;
                }
                AuthorityEndpointState::Submitted {
                    terminal_expected: true,
                    ..
                }
                | AuthorityEndpointState::Attached {
                    terminal_expected: true,
                    ..
                }
                | AuthorityEndpointState::TerminalDrained { .. } => {}
            }
        }
        Ok(())
    }

    fn service_pending_authority_detaches(&mut self) -> Result<(), NativeOnlineApplicationError> {
        if self.listen_shutdown.is_some() {
            self.pending_authority_detaches.clear();
            return Ok(());
        }
        let count = self.pending_authority_detaches.len();
        for _ in 0..count {
            let mut pending = self
                .pending_authority_detaches
                .pop_front()
                .expect("bounded detach count came from queue length");
            if pending.defer_once {
                pending.defer_once = false;
                self.pending_authority_detaches.push_back(pending);
                continue;
            }
            let matches: ArrayVec<usize, ONLINE_MAX_AUTHORITY_ENDPOINT_RECORDS> = self
                .authority_endpoints
                .iter()
                .enumerate()
                .filter_map(|(index, record)| {
                    record
                        .as_ref()
                        .is_some_and(|record| {
                            record.peer_id == pending.peer_id
                                && record.steam_connection == pending.steam_connection
                        })
                        .then_some(index)
                })
                .collect();
            let [index] = matches.as_slice() else {
                if matches.is_empty() {
                    continue;
                }
                return Err(NativeOnlineApplicationError::Runtime);
            };
            let record = self.authority_endpoints[*index].expect("matched endpoint record");
            match record.state {
                AuthorityEndpointState::Submitted {
                    terminal_expected: false,
                    ..
                } => self.pending_authority_detaches.push_back(pending),
                AuthorityEndpointState::Attached {
                    connection,
                    terminal_expected: false,
                } => {
                    let outcome = match &self.active {
                        Some(ActiveNativeOnlineMatch::Listen(online_match)) => {
                            online_match.authority.try_detach(connection)
                        }
                        _ => continue,
                    };
                    self.retain_authority_outcome(outcome)?;
                    // The queued command now owns the exact authority
                    // generation. Freeing this mapping cannot redirect the
                    // command to a reconnect replacement.
                    self.authority_endpoints[*index] = None;
                }
                AuthorityEndpointState::Submitted {
                    terminal_expected: true,
                    ..
                }
                | AuthorityEndpointState::Attached {
                    terminal_expected: true,
                    ..
                }
                | AuthorityEndpointState::TerminalDrained { .. } => {}
            }
        }
        Ok(())
    }

    fn drain_authority_update<R: NativeOnlineRuntimePort>(
        &mut self,
        runtime: &mut R,
        now_ms: u64,
    ) -> Result<(), NativeOnlineApplicationError> {
        let Some(ActiveNativeOnlineMatch::Listen(online_match)) = &mut self.active else {
            return Ok(());
        };
        let update = online_match.authority.drain_update();
        if let Some(failure) = update.status.failure {
            self.failure_override = Some(failure);
        }
        for event in update.events {
            match event {
                ListenAuthorityEvent::InitialAttached {
                    peer_id,
                    connection,
                } => self.promote_authority_endpoint(peer_id, connection, false)?,
                ListenAuthorityEvent::ReconnectAttached {
                    peer_id,
                    connection,
                } => self.promote_authority_endpoint(peer_id, connection, true)?,
                ListenAuthorityEvent::Detached {
                    peer_id,
                    connection,
                } => self.observe_authority_detached(peer_id, connection),
                ListenAuthorityEvent::AuthenticationRevoked {
                    peer_id,
                    user_id,
                    connection,
                } => self.mark_authority_endpoint_terminal_expected(peer_id, user_id, connection),
                ListenAuthorityEvent::PlatformBanEnforced {
                    user_id,
                    peer_id: Some(peer_id),
                    connection: Some(connection),
                } => self.mark_authority_endpoint_terminal_expected(peer_id, user_id, connection),
                ListenAuthorityEvent::PlatformBanEnforced { .. } => {}
                ListenAuthorityEvent::TerminalDrained {
                    peer_id,
                    user_id,
                    connection,
                    disconnect,
                    ..
                } => self.retain_authority_terminal_drained(
                    peer_id,
                    user_id,
                    connection,
                    disconnect.map(|message| message.retry),
                ),
                ListenAuthorityEvent::CommandRejected {
                    operation, peer_id, ..
                } => {
                    self.metrics.authority_command_rejections =
                        self.metrics.authority_command_rejections.saturating_add(1);
                    match operation {
                        ListenAuthorityOperation::AttachInitial => {
                            self.remove_submitted_authority_endpoint(peer_id, false, None);
                        }
                        ListenAuthorityOperation::AttachReconnect => {
                            self.remove_submitted_authority_endpoint(peer_id, true, None);
                        }
                        ListenAuthorityOperation::Detach => {
                            return Err(NativeOnlineApplicationError::AuthorityDisconnected);
                        }
                        ListenAuthorityOperation::RevokeAuthentication
                        | ListenAuthorityOperation::EnforcePlatformBan
                        | ListenAuthorityOperation::BeginShutdown => {
                            return Err(NativeOnlineApplicationError::AuthorityDisconnected);
                        }
                    }
                    if !matches!(operation, ListenAuthorityOperation::Detach) {
                        // Attach rejection is scoped to that exact pending
                        // handoff; it is not an authority-worker terminal.
                        if !matches!(
                            operation,
                            ListenAuthorityOperation::AttachInitial
                                | ListenAuthorityOperation::AttachReconnect
                        ) {
                            return Err(NativeOnlineApplicationError::AuthorityDisconnected);
                        }
                    }
                }
                ListenAuthorityEvent::SecurityCommandRejected { operation, .. } => {
                    self.metrics.authority_command_rejections =
                        self.metrics.authority_command_rejections.saturating_add(1);
                    if !matches!(operation, ListenAuthorityOperation::Detach) {
                        return Err(NativeOnlineApplicationError::AuthorityDisconnected);
                    }
                }
                ListenAuthorityEvent::ShutdownStarted | ListenAuthorityEvent::ShutdownDrained => {}
            }
        }
        self.retry_authority_terminal_marks(runtime, now_ms)?;

        if let Some(terminal) = update.terminal {
            let graceful = terminal.status.phase == ListenAuthorityPhase::Drained
                && self.listen_shutdown.is_some();
            let joined = match &mut self.active {
                Some(ActiveNativeOnlineMatch::Listen(online_match)) => {
                    online_match.authority.join().ok()
                }
                _ => None,
            };
            if graceful
                && joined.is_some_and(|joined| joined.status.phase == ListenAuthorityPhase::Drained)
            {
                self.complete_listen_shutdown(runtime, now_ms)?;
            } else {
                self.metrics.graceful_shutdown_emergency_fallbacks = self
                    .metrics
                    .graceful_shutdown_emergency_fallbacks
                    .saturating_add(1);
                self.clear_online_session();
                let _ = runtime.execute_port(NativeOnlineCommand::LeaveOnline, now_ms);
                self.awaiting_transport_retirement = runtime.transport_retirement_pending_port();
                self.failure_override = Some(OnlineFailure {
                    code: OnlineFailureCode::AuthorityLost,
                    severity: OnlineFailureSeverity::Fatal,
                    recovery: OnlineRecoveryAction::ReturnToMenu,
                    detail_code: 1,
                });
            }
        }
        Ok(())
    }

    fn drive_coordinator_from_worker<R: NativeOnlineRuntimePort>(
        &mut self,
        runtime: &mut R,
        now_ms: u64,
    ) -> Result<(), NativeOnlineApplicationError> {
        let authority_disconnect =
            self.active
                .as_ref()
                .and_then(|active| match active.client().terminal() {
                    Some(RemoteOnlineTerminal::AuthorityDisconnected(disconnect)) => {
                        Some(disconnect)
                    }
                    _ => None,
                });
        if let Some(disconnect) = authority_disconnect {
            self.apply_authority_disconnect(runtime, disconnect, now_ms)?;
        }
        let Some(active) = &self.active else {
            return Ok(());
        };
        if self.content_ready {
            active.client().mark_content_loaded();
        }
        let status = active.client().status();
        let mut view = runtime.view_model();

        if view.screen == NativeOnlineScreen::Loading
            && self.content_ready
            && !self.coordinator_content_marked
        {
            runtime.execute_port(NativeOnlineCommand::ContentLoaded, now_ms)?;
            self.coordinator_content_marked = true;
            view = runtime.view_model();
        }

        if view.screen == NativeOnlineScreen::Loading
            && self.coordinator_content_marked
            && !self.initial_sync_marked
            && matches!(
                status.phase,
                RemoteOnlineClientPhase::Ready
                    | RemoteOnlineClientPhase::Countdown
                    | RemoteOnlineClientPhase::Fighting
                    | RemoteOnlineClientPhase::ConfirmingResult
                    | RemoteOnlineClientPhase::Results
            )
        {
            runtime.execute_port(NativeOnlineCommand::InitialSyncComplete, now_ms)?;
            self.initial_sync_marked = true;
            view = runtime.view_model();
        }

        if view.screen == NativeOnlineScreen::Ready {
            if let Some(start_tick) = status.countdown_start_tick {
                runtime.execute_port(NativeOnlineCommand::BeginCountdown(start_tick), now_ms)?;
                view = runtime.view_model();
            }
        }

        if view.screen == NativeOnlineScreen::Countdown
            && matches!(
                status.phase,
                RemoteOnlineClientPhase::Fighting
                    | RemoteOnlineClientPhase::ConfirmingResult
                    | RemoteOnlineClientPhase::Results
            )
        {
            runtime.execute_port(
                NativeOnlineCommand::MarkFighting(status.network_tick),
                now_ms,
            )?;
            view = runtime.view_model();
        }

        let terminal = active.client().terminal();
        let current_match_id = active.client().manifest().match_id;
        let projected_terminal = terminal_is_projected_for_match(
            terminal,
            current_match_id,
            self.projected_confirmed_result,
        );
        if view.screen == NativeOnlineScreen::Fighting
            && projected_terminal
            && !self.result_confirmation_started
        {
            runtime.execute_port(NativeOnlineCommand::BeginResultConfirmation, now_ms)?;
            self.result_confirmation_started = true;
            view = runtime.view_model();
        }
        if view.screen == NativeOnlineScreen::ConfirmingResult
            && projected_terminal
            && !self.result_confirmed
        {
            runtime.execute_port(NativeOnlineCommand::ConfirmResult, now_ms)?;
            self.result_confirmed = true;
        }

        self.observe_terminal_worker(terminal, runtime.view_model().screen);
        Ok(())
    }

    pub fn project_latest(
        &mut self,
        target: &mut World,
    ) -> Result<(), NativeOnlineApplicationError> {
        let Some(active) = &mut self.active else {
            return Ok(());
        };
        let match_id = active.client().manifest().match_id;
        let update = active.client_mut().project_latest(target)?;
        if let Some(result) = update.projected_confirmed_result {
            let projected = ProjectedConfirmedTerminal { match_id, result };
            if self
                .projected_confirmed_result
                .is_some_and(|existing| existing != projected)
            {
                return Err(NativeOnlineApplicationError::Presentation);
            }
            self.projected_confirmed_result = Some(projected);
        }
        Ok(())
    }

    pub fn submit_local_inputs(
        &mut self,
        local_inputs: &mut LocalTickInputState,
    ) -> Result<(), NativeOnlineApplicationError> {
        if !self.accepts_gameplay_input() {
            return Ok(());
        }
        let Some(active) = &mut self.active else {
            return Ok(());
        };
        match active
            .client_mut()
            .sample_local_inputs(local_inputs)
            .map_err(|_| NativeOnlineApplicationError::Runtime)?
        {
            RemoteCommandSubmitOutcome::Queued => Ok(()),
            RemoteCommandSubmitOutcome::Full => {
                self.metrics.local_input_backpressure =
                    self.metrics.local_input_backpressure.saturating_add(1);
                Ok(())
            }
            RemoteCommandSubmitOutcome::Disconnected => {
                Err(NativeOnlineApplicationError::AuthorityDisconnected)
            }
        }
    }
}
