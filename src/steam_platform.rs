//! Steam lobby, invitation, presence, and authentication boundary.
//!
//! This module deliberately does not own gameplay transport or simulation state.
//! [`SteamPlatform`] is the sole callback-pump owner and converts asynchronous
//! backend callbacks into a bounded, validated event stream. The default build
//! includes the deterministic [`FakeSteamBackend`]; the native Steam client
//! implementation is available only with `steam-net`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex};

use crate::match_config::current_compatibility;
use crate::network_protocol::{
    AuthorityKind, CompatibilityId, DefinitionId, MAX_LOCAL_SEATS, ProtocolValidationError,
};
use crate::online_roster::{
    MAX_ONLINE_MEMBER_DECLARATION_BYTES, OnlineMemberDeclarationSummary,
    validate_member_declaration,
};
use crate::reconnect::AuthenticatedUserId;
#[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
use crate::tick_input::RawInputButton;
use crate::tick_input::{InputMask, QuantizedMovement};

pub const SPACEWAR_APP_ID: u32 = 480;
pub const MAX_STEAM_LOBBY_MEMBERS: usize = 4;
pub const MAX_STEAM_EVENTS: usize = 128;
pub const DEFAULT_STEAM_EVENT_CAPACITY: usize = 64;
pub const MAX_STEAM_AUTH_TICKET_BYTES: usize = 1_024;
pub const MAX_REGION_CODE_BYTES: usize = 16;
pub const MAX_CONNECT_COMMAND_BYTES: usize = 96;
pub const DEFAULT_JOIN_INTENT_TTL_MS: u64 = 30_000;
pub const DEFAULT_AUTH_INTENT_TTL_MS: u64 = 15_000;
pub const MAX_STEAM_INPUT_CONTROLLERS: usize = MAX_LOCAL_SEATS as usize;

#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
const MAX_STEAM_INPUT_DISCOVERED_CONTROLLERS: usize = 16;

const MAX_LOBBY_METADATA_PAIRS: usize = 16;
const MAX_MEMBER_METADATA_PAIRS: usize = 3;
const MAX_RICH_PRESENCE_PAIRS: usize = 20;

const KEY_SCHEMA: &str = "afc_schema";
const KEY_BUILD: &str = "afc_build";
const KEY_PROTOCOL: &str = "afc_protocol";
const KEY_SIMULATION: &str = "afc_sim";
const KEY_REPLAY: &str = "afc_replay";
const KEY_CONTENT: &str = "afc_content";
const KEY_AUTHORITY: &str = "afc_authority";
const KEY_VISIBILITY: &str = "afc_visibility";
const KEY_REGION: &str = "afc_region";
const KEY_RULES: &str = "afc_rules";
const KEY_ARENA: &str = "afc_arena";
const KEY_SEATS: &str = "afc_seats";
const KEY_ADMISSION: &str = "afc_admission";
const KEY_OPEN: &str = "afc_open";
const MEMBER_KEY_READY: &str = "afc_ready";
const MEMBER_KEY_SEATS: &str = "afc_local_seats";
const MEMBER_KEY_LOADOUT: &str = "afc_loadout";
const LOBBY_SCHEMA_VERSION: u16 = 2;

/// A process-local Steam Input device identity. This value is used only to
/// preserve controller-to-couch-ordinal assignment; it is never serialized or
/// included in gameplay protocol frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SteamInputControllerId(u64);

impl SteamInputControllerId {
    pub const fn new(value: u64) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SteamInputDeviceKind {
    #[default]
    Unknown,
    SteamController,
    Xbox360,
    XboxOne,
    GenericGamepad,
    PlayStation3,
    PlayStation4,
    PlayStation5,
    SwitchJoyConPair,
    SwitchJoyConSingle,
    SwitchPro,
    AppleMfi,
    Android,
    MobileTouch,
    SteamDeck,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SteamInputActionSet {
    Gameplay,
    #[default]
    Menu,
}

/// Result of a user-requested Steam Overlay surface.
///
/// `Submitted` deliberately does not claim that an invite dialog became
/// visible: Steam's invite activation API returns no completion value. The
/// binding-panel API does return a boolean, which is projected into the same
/// conservative status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SteamOverlayRequestStatus {
    Submitted,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SteamMenuAction {
    Accept = 0,
    Back = 1,
    Up = 2,
    Down = 3,
    Left = 4,
    Right = 5,
    OpenBindings = 6,
}

impl SteamMenuAction {
    pub const ALL: [Self; 7] = [
        Self::Accept,
        Self::Back,
        Self::Up,
        Self::Down,
        Self::Left,
        Self::Right,
        Self::OpenBindings,
    ];

    pub const fn mask(self) -> SteamMenuInputMask {
        SteamMenuInputMask(1_u16 << self as u8)
    }
}

/// Current menu-action values from Steam Input. Edges are intentionally
/// derived by the application so several controllers can share menu focus
/// without sending device identities into the simulation protocol.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SteamMenuInputMask(u16);

impl SteamMenuInputMask {
    pub const NONE: Self = Self(0);

    pub const fn from_bits(bits: u16) -> Self {
        Self(bits & ((1_u16 << SteamMenuAction::ALL.len()) - 1))
    }

    pub const fn bits(self) -> u16 {
        self.0
    }

    pub const fn contains(self, action: SteamMenuAction) -> bool {
        self.0 & action.mask().0 != 0
    }

    pub fn insert(&mut self, action: SteamMenuAction) {
        self.0 |= action.mask().0;
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }
}

/// Action-level values for one stable local controller ordinal.
///
/// No Steam action-set/action handles appear here: the platform backend owns
/// those implementation details, while the rest of the game sees the same
/// compact actions used by keyboard and network input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SteamInputControllerSnapshot {
    pub controller_id: Option<SteamInputControllerId>,
    pub device_kind: SteamInputDeviceKind,
    pub movement: QuantizedMovement,
    pub gameplay_held: InputMask,
    pub menu_held: SteamMenuInputMask,
}

impl SteamInputControllerSnapshot {
    pub const fn connected(self) -> bool {
        self.controller_id.is_some()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SteamInputSnapshot {
    pub controllers: [SteamInputControllerSnapshot; MAX_STEAM_INPUT_CONTROLLERS],
}

impl Default for SteamInputSnapshot {
    fn default() -> Self {
        Self {
            controllers: [SteamInputControllerSnapshot::default(); MAX_STEAM_INPUT_CONTROLLERS],
        }
    }
}

impl SteamInputSnapshot {
    pub const fn controller(self, local_ordinal: usize) -> Option<SteamInputControllerSnapshot> {
        if local_ordinal >= MAX_STEAM_INPUT_CONTROLLERS {
            return None;
        }
        let controller = self.controllers[local_ordinal];
        if controller.connected() {
            Some(controller)
        } else {
            None
        }
    }

    pub fn aggregate_menu(self) -> SteamMenuInputMask {
        let mut aggregate = SteamMenuInputMask::NONE;
        for controller in self.controllers {
            aggregate = aggregate.union(controller.menu_held);
        }
        aggregate
    }
}

/// Fixed-capacity assignment table shared by the fake and real backends.
/// Existing connected handles retain their couch ordinal even when Steam
/// returns the connected list in a different order on a later frame.
#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SteamInputAssignments {
    handles: [Option<SteamInputControllerId>; MAX_STEAM_INPUT_CONTROLLERS],
}

#[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
impl SteamInputAssignments {
    fn reconcile(&mut self, connected: &[u64]) {
        for assignment in &mut self.handles {
            if assignment.is_some_and(|assigned| !connected.contains(&assigned.get())) {
                *assignment = None;
            }
        }

        let mut sorted = [0_u64; MAX_STEAM_INPUT_DISCOVERED_CONTROLLERS];
        let mut sorted_len = 0;
        for raw in connected.iter().copied().filter(|raw| *raw != 0) {
            if sorted[..sorted_len].contains(&raw) {
                continue;
            }
            let mut insert_at = sorted_len;
            while insert_at > 0 && sorted[insert_at - 1] > raw {
                sorted[insert_at] = sorted[insert_at - 1];
                insert_at -= 1;
            }
            sorted[insert_at] = raw;
            sorted_len += 1;
            if sorted_len == sorted.len() {
                break;
            }
        }

        for raw in &sorted[..sorted_len] {
            let id =
                SteamInputControllerId::new(*raw).expect("zero controller handles are skipped");
            if self.handles.contains(&Some(id)) {
                continue;
            }
            let Some(vacant) = self.handles.iter_mut().find(|slot| slot.is_none()) else {
                break;
            };
            *vacant = Some(id);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SteamAppId(u32);

impl SteamAppId {
    pub fn new(value: u32) -> Result<Self, SteamPlatformError> {
        if value == 0 {
            Err(SteamPlatformError::ZeroIdentifier)
        } else {
            Ok(Self(value))
        }
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SteamLobbyId(u64);

impl SteamLobbyId {
    pub fn new(value: u64) -> Result<Self, SteamPlatformError> {
        if value == 0 {
            Err(SteamPlatformError::ZeroIdentifier)
        } else {
            Ok(Self(value))
        }
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Monotonic identity for one asynchronous lobby create/join operation.
///
/// Backends must echo this value in their completion event. The platform uses
/// it to retire callbacks from canceled operations without attributing them to
/// a later create or join.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SteamOperationId(u64);

impl SteamOperationId {
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SteamUserId(u64);

impl SteamUserId {
    pub fn new(value: u64) -> Result<Self, SteamPlatformError> {
        if value == 0 {
            Err(SteamPlatformError::ZeroIdentifier)
        } else {
            Ok(Self(value))
        }
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn authenticated(self) -> AuthenticatedUserId {
        AuthenticatedUserId::new(self.0).expect("SteamUserId is always non-zero")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SteamEnvironment {
    Production,
    Development { allow_spacewar_480: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SteamClientConfig {
    pub app_id: SteamAppId,
    pub environment: SteamEnvironment,
    pub allow_public_lobbies: bool,
    pub event_capacity: usize,
    pub join_intent_ttl_ms: u64,
    pub auth_intent_ttl_ms: u64,
}

impl SteamClientConfig {
    pub fn production(app_id: SteamAppId) -> Self {
        Self {
            app_id,
            environment: SteamEnvironment::Production,
            allow_public_lobbies: false,
            event_capacity: DEFAULT_STEAM_EVENT_CAPACITY,
            join_intent_ttl_ms: DEFAULT_JOIN_INTENT_TTL_MS,
            auth_intent_ttl_ms: DEFAULT_AUTH_INTENT_TTL_MS,
        }
    }

    pub fn development(app_id: SteamAppId, allow_spacewar_480: bool) -> Self {
        Self {
            environment: SteamEnvironment::Development { allow_spacewar_480 },
            ..Self::production(app_id)
        }
    }

    pub fn validate(self) -> Result<(), SteamPlatformError> {
        if self.event_capacity == 0 || self.event_capacity > MAX_STEAM_EVENTS {
            return Err(SteamPlatformError::InvalidEventCapacity);
        }
        if self.join_intent_ttl_ms == 0 || self.auth_intent_ttl_ms == 0 {
            return Err(SteamPlatformError::InvalidTimeout);
        }
        if self.app_id.get() == SPACEWAR_APP_ID {
            match self.environment {
                SteamEnvironment::Development {
                    allow_spacewar_480: true,
                } => {}
                SteamEnvironment::Development {
                    allow_spacewar_480: false,
                } => return Err(SteamPlatformError::SpacewarRequiresExplicitOptIn),
                SteamEnvironment::Production => {
                    return Err(SteamPlatformError::SpacewarForbiddenInProduction);
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LobbyVisibility {
    Private,
    FriendsOnly,
    Public,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegionCode(String);

impl RegionCode {
    pub fn new(value: impl Into<String>) -> Result<Self, SteamPlatformError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= MAX_REGION_CODE_BYTES
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        if !valid {
            return Err(SteamPlatformError::InvalidRegion);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LobbyMetadata {
    pub compatibility: CompatibilityId,
    pub authority: AuthorityKind,
    pub visibility: LobbyVisibility,
    pub region: RegionCode,
    pub rules: DefinitionId,
    pub arena: DefinitionId,
    pub seat_capacity: u8,
}

impl LobbyMetadata {
    pub fn new(
        compatibility: CompatibilityId,
        authority: AuthorityKind,
        visibility: LobbyVisibility,
        region: RegionCode,
        rules: DefinitionId,
        arena: DefinitionId,
        seat_capacity: u8,
    ) -> Result<Self, SteamPlatformError> {
        compatibility
            .validate()
            .map_err(SteamPlatformError::Protocol)?;
        rules.validate().map_err(SteamPlatformError::Protocol)?;
        arena.validate().map_err(SteamPlatformError::Protocol)?;
        if authority == AuthorityKind::Offline {
            return Err(SteamPlatformError::InvalidAuthority);
        }
        if seat_capacity == 0 || seat_capacity > MAX_LOCAL_SEATS {
            return Err(SteamPlatformError::InvalidSeatCount);
        }
        Ok(Self {
            compatibility,
            authority,
            visibility,
            region,
            rules,
            arena,
            seat_capacity,
        })
    }

    pub fn current(
        authority: AuthorityKind,
        visibility: LobbyVisibility,
        region: RegionCode,
        rules: DefinitionId,
        arena: DefinitionId,
        seat_capacity: u8,
    ) -> Result<Self, SteamPlatformError> {
        Self::new(
            current_compatibility(),
            authority,
            visibility,
            region,
            rules,
            arena,
            seat_capacity,
        )
    }

    /// Validates the lobby metadata permitted by the first player-facing
    /// Steam release. This is intentionally stricter than the metadata codec,
    /// which retains dedicated/public variants for future protocol evolution
    /// and low-level tests.
    pub fn validate_first_release_player_scope(&self) -> Result<(), SteamPlatformError> {
        match self.authority {
            AuthorityKind::Listen => {}
            AuthorityKind::Dedicated => return Err(SteamPlatformError::DedicatedSdrUnavailable),
            AuthorityKind::Offline => return Err(SteamPlatformError::InvalidAuthority),
        }
        match self.visibility {
            LobbyVisibility::Private | LobbyVisibility::FriendsOnly => Ok(()),
            LobbyVisibility::Public => Err(SteamPlatformError::PublicLobbiesDisabled),
        }
    }

    fn validate_for_local_client(
        &self,
        config: SteamClientConfig,
    ) -> Result<(), SteamPlatformError> {
        self.compatibility
            .validate_against(&current_compatibility())
            .map_err(SteamPlatformError::Protocol)?;
        if self.visibility == LobbyVisibility::Public && !config.allow_public_lobbies {
            return Err(SteamPlatformError::PublicLobbiesDisabled);
        }
        Ok(())
    }

    fn pairs(
        &self,
        admission_enabled: bool,
        effective_joinable: bool,
    ) -> [(&'static str, String); 14] {
        [
            (KEY_SCHEMA, LOBBY_SCHEMA_VERSION.to_string()),
            (KEY_BUILD, encode_hex(self.compatibility.build.as_bytes())),
            (KEY_PROTOCOL, self.compatibility.protocol.get().to_string()),
            (
                KEY_SIMULATION,
                self.compatibility.simulation.get().to_string(),
            ),
            (KEY_REPLAY, self.compatibility.replay.get().to_string()),
            (
                KEY_CONTENT,
                encode_hex(self.compatibility.gameplay_content.as_bytes()),
            ),
            (KEY_AUTHORITY, authority_text(self.authority).to_owned()),
            (KEY_VISIBILITY, visibility_text(self.visibility).to_owned()),
            (KEY_REGION, self.region.as_str().to_owned()),
            (KEY_RULES, self.rules.get().to_string()),
            (KEY_ARENA, self.arena.get().to_string()),
            (KEY_SEATS, self.seat_capacity.to_string()),
            (KEY_ADMISSION, bool_text(admission_enabled).to_owned()),
            (KEY_OPEN, bool_text(effective_joinable).to_owned()),
        ]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LobbyCreateRequest {
    pub visibility: LobbyVisibility,
    pub maximum_peers: u8,
    pub local_seats: u8,
}

impl LobbyCreateRequest {
    fn validate(
        self,
        metadata: &LobbyMetadata,
        config: SteamClientConfig,
    ) -> Result<(), SteamPlatformError> {
        if self.visibility != metadata.visibility {
            return Err(SteamPlatformError::VisibilityMismatch);
        }
        if self.maximum_peers == 0 || usize::from(self.maximum_peers) > MAX_STEAM_LOBBY_MEMBERS {
            return Err(SteamPlatformError::LobbyCapacityExceeded);
        }
        validate_local_seats(self.local_seats)?;
        if self.local_seats > metadata.seat_capacity {
            return Err(SteamPlatformError::LobbyCapacityExceeded);
        }
        metadata.validate_for_local_client(config)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinOrigin {
    SteamInvite { friend: Option<SteamUserId> },
    LaunchCommand,
    RichPresence { friend: Option<SteamUserId> },
    FriendsList,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LobbyJoinIntent {
    pub lobby: SteamLobbyId,
    pub origin: JoinOrigin,
    pub expires_at_ms: u64,
}

impl LobbyJoinIntent {
    pub fn friends_list(
        lobby: SteamLobbyId,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<Self, SteamPlatformError> {
        Ok(Self {
            lobby,
            origin: JoinOrigin::FriendsList,
            expires_at_ms: deadline(now_ms, ttl_ms)?,
        })
    }

    fn validate(self, now_ms: u64) -> Result<(), SteamPlatformError> {
        if now_ms >= self.expires_at_ms {
            Err(SteamPlatformError::JoinIntentExpired)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberReadiness {
    Pending,
    Declared { ready: bool, local_seats: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LobbyMember {
    pub user: SteamUserId,
    pub readiness: MemberReadiness,
    pub loadout: Option<MemberLoadoutDeclaration>,
}

/// Canonical, allocation-free copy of the bounded online roster declaration
/// published in Steam member metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemberLoadoutDeclaration {
    bytes: [u8; MAX_ONLINE_MEMBER_DECLARATION_BYTES],
    len: u8,
    summary: OnlineMemberDeclarationSummary,
}

impl MemberLoadoutDeclaration {
    pub fn new(encoded: &str) -> Result<Self, SteamPlatformError> {
        let summary = validate_member_declaration(encoded)
            .map_err(|_| SteamPlatformError::InvalidMetadata)?;
        let mut bytes = [0_u8; MAX_ONLINE_MEMBER_DECLARATION_BYTES];
        bytes[..encoded.len()].copy_from_slice(encoded.as_bytes());
        Ok(Self {
            bytes,
            len: encoded.len() as u8,
            summary,
        })
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..usize::from(self.len)])
            .expect("validated online member declarations are lowercase ASCII")
    }

    pub const fn revision(self) -> u16 {
        self.summary.revision
    }

    pub const fn seat_count(self) -> u8 {
        self.summary.seat_count
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LobbyMembershipChange {
    Entered,
    Left,
    Disconnected,
    Kicked,
    Banned,
}

/// The Steam subject carried by `LobbyDataUpdate`.
///
/// Steam uses the lobby Steam ID itself for lobby-data changes and a user
/// Steam ID for member-data changes. Keeping that distinction through the
/// backend boundary prevents hostile member metadata from being interpreted as
/// an immutable lobby-contract update (and vice versa).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LobbyDataSubject {
    Lobby,
    Member(SteamUserId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberDeclarationRejection {
    Malformed,
    RevisionRegression,
    RevisionConflict,
    LobbyCapacityExceeded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberDataOutcome {
    Staging,
    Accepted,
    Rejected(MemberDeclarationRejection),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionPurpose {
    Initial,
    Reconnect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AuthenticatedSteamPeer {
    pub lobby: SteamLobbyId,
    pub user: SteamUserId,
    /// Steam ID that owns the app license used by `user`. This is normally
    /// equal to `user`, but may differ for a Steam Families borrower. It is
    /// never the lobby or gameplay authority identity.
    pub license_owner_user: SteamUserId,
    pub authenticated_user: AuthenticatedUserId,
    pub local_seats: u8,
    pub purpose: AdmissionPurpose,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuthTicketHandle(u32);

impl AuthTicketHandle {
    pub const fn get(self) -> u32 {
        self.0
    }

    #[cfg(test)]
    pub(crate) const fn for_test(value: u32) -> Self {
        Self(value)
    }
}

#[derive(PartialEq, Eq)]
pub struct IssuedAuthTicket {
    pub handle: AuthTicketHandle,
    pub remote_user: SteamUserId,
    bytes: Vec<u8>,
}

impl IssuedAuthTicket {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[cfg(any(test, all(feature = "steam-net", not(target_arch = "wasm32"))))]
    pub(crate) fn into_parts(mut self) -> (AuthTicketHandle, SteamUserId, Vec<u8>) {
        (
            self.handle,
            self.remote_user,
            std::mem::take(&mut self.bytes),
        )
    }
}

impl fmt::Debug for IssuedAuthTicket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IssuedAuthTicket")
            .field("handle", &self.handle)
            .field("remote_user", &self.remote_user)
            .field("ticket_len", &self.bytes.len())
            .field("bytes", &"<redacted>")
            .finish()
    }
}

impl Drop for IssuedAuthTicket {
    fn drop(&mut self) {
        zeroize_ticket_bytes(&mut self.bytes);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthValidationFailure {
    UserNotConnected,
    NoLicenseOrExpired,
    VacBanned,
    LoggedInElsewhere,
    VacCheckTimedOut,
    TicketCancelled,
    TicketAlreadyUsed,
    TicketInvalid,
    PublisherBan,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LicenseStatus {
    HasLicense,
    DoesNotHaveLicense,
    NoAuthentication,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerAuthenticationRejection {
    Validation(AuthValidationFailure),
    DoesNotHaveLicense,
    NoAuthentication,
    IntentExpired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SteamBackendError {
    InitializationFailed,
    AlreadyInitialized,
    AppIdMismatch,
    NotLoggedOn,
    OperationFailed,
    InvalidData,
    CapacityExceeded,
    AuthenticationFailed,
    CallbackQueueOverflow,
    IntegrityFailure,
    SteamInputInitializationFailed,
    SteamInputManifestInvalid,
    SteamInputActionMissing,
}

impl fmt::Display for SteamBackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Steam backend failure: {self:?}")
    }
}

impl std::error::Error for SteamBackendError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SteamPlatformError {
    Backend(SteamBackendError),
    Protocol(ProtocolValidationError),
    ZeroIdentifier,
    InvalidEventCapacity,
    InvalidTimeout,
    SpacewarRequiresExplicitOptIn,
    SpacewarForbiddenInProduction,
    PublicLobbiesDisabled,
    InvalidRegion,
    InvalidAuthority,
    InvalidSeatCount,
    InvalidMetadata,
    MetadataMissing,
    MetadataMismatch,
    VisibilityMismatch,
    LobbyCapacityExceeded,
    InvalidState,
    UnexpectedLobby,
    JoinIntentExpired,
    PrivateLobbyRequiresInvite,
    FriendsRelationshipRequired,
    NotLobbyOwner,
    MemberNotInExpectedLobby,
    MemberMetadataPending,
    EventQueueOverflow,
    InvalidConnectCommand,
    ConnectCommandTooLong,
    DuplicateConnectLobby,
    AuthTicketEmpty,
    AuthTicketTooLarge,
    AuthCapacityExceeded,
    AuthIntentExpired,
    AuthenticationPending,
    AuthenticationRejected,
    AdmissionAlreadyConsumed,
    DedicatedSdrUnavailable,
    Faulted,
}

impl fmt::Display for SteamPlatformError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Steam platform operation failed: {self:?}")
    }
}

impl std::error::Error for SteamPlatformError {}

impl From<SteamBackendError> for SteamPlatformError {
    fn from(value: SteamBackendError) -> Self {
        Self::Backend(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LobbyExitReason {
    Requested,
    JoinRejected,
    SteamDisconnected,
    Removed,
    AuthorityLost,
    ValidationFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SteamPlatformEvent {
    LobbyJoinRequested(LobbyJoinIntent),
    LobbyEntered {
        lobby: SteamLobbyId,
        owner: SteamUserId,
    },
    LobbyCreateFailed(SteamPlatformError),
    LobbyJoinRejected {
        lobby: SteamLobbyId,
        reason: SteamPlatformError,
    },
    LobbyRosterChanged {
        lobby: SteamLobbyId,
    },
    LobbyMetadataChanged {
        lobby: SteamLobbyId,
    },
    LobbyMemberDataChanged {
        lobby: SteamLobbyId,
        user: SteamUserId,
        outcome: MemberDataOutcome,
    },
    LobbyLeft {
        lobby: SteamLobbyId,
        reason: LobbyExitReason,
    },
    AuthorityLost {
        lobby: SteamLobbyId,
        previous_authority: SteamUserId,
        successor: SteamUserId,
    },
    RichPresenceUnavailable,
    AuthTicketReady {
        handle: AuthTicketHandle,
    },
    AuthTicketRejected {
        handle: AuthTicketHandle,
        remote_user: SteamUserId,
    },
    PeerAuthenticated {
        lobby: SteamLobbyId,
        user: SteamUserId,
    },
    PeerAuthenticationRejected {
        lobby: SteamLobbyId,
        user: SteamUserId,
        reason: PeerAuthenticationRejection,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SteamBackendEvent {
    LobbyCreated {
        operation_id: SteamOperationId,
        result: Result<SteamLobbyId, SteamBackendError>,
    },
    LobbyJoined {
        operation_id: SteamOperationId,
        requested: SteamLobbyId,
        result: Result<SteamLobbyId, SteamBackendError>,
    },
    LobbyMembershipChanged {
        lobby: SteamLobbyId,
        user: SteamUserId,
        change: LobbyMembershipChange,
    },
    /// The native callback mailbox observed more distinct peer departures
    /// than its bounded per-user slots can retain. The event remains scoped to
    /// one active lobby and conservatively retires every remote auth capability
    /// before rebuilding the authoritative roster.
    LobbyMembershipResync {
        lobby: SteamLobbyId,
    },
    LobbyDataChanged {
        lobby: SteamLobbyId,
        subject: LobbyDataSubject,
    },
    LobbyJoinRequested {
        lobby: SteamLobbyId,
        friend: Option<SteamUserId>,
    },
    RichPresenceJoinRequested {
        friend: Option<SteamUserId>,
        connect: String,
    },
    LaunchParametersChanged,
    AuthTicketReady {
        handle: AuthTicketHandle,
        success: bool,
    },
    AuthSessionValidated {
        user: SteamUserId,
        license_owner_user: SteamUserId,
        result: Result<(), AuthValidationFailure>,
    },
    SteamDisconnected,
    IntegrityFailure,
}

#[derive(PartialEq, Eq)]
pub struct BackendIssuedAuthTicket {
    pub handle: AuthTicketHandle,
    pub bytes: Vec<u8>,
}

impl BackendIssuedAuthTicket {
    fn into_parts(mut self) -> (AuthTicketHandle, Vec<u8>) {
        (self.handle, std::mem::take(&mut self.bytes))
    }
}

impl fmt::Debug for BackendIssuedAuthTicket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BackendIssuedAuthTicket")
            .field("handle", &self.handle)
            .field("ticket_len", &self.bytes.len())
            .field("bytes", &"<redacted>")
            .finish()
    }
}

impl Drop for BackendIssuedAuthTicket {
    fn drop(&mut self) {
        zeroize_ticket_bytes(&mut self.bytes);
    }
}

fn zeroize_ticket_bytes(bytes: &mut [u8]) {
    bytes.fill(0);
    // Keep the overwrite observable at the optimization boundary before the
    // allocation is released.
    std::hint::black_box(bytes);
}

/// Backend operations used by the policy/state-machine layer.
///
/// Implementations must not expose another callback pump. `pump_callbacks` is
/// called only by [`SteamPlatform::pump`].
pub trait SteamBackend {
    fn configured_app_id(&self) -> Result<SteamAppId, SteamBackendError>;
    fn local_user(&self) -> Result<SteamUserId, SteamBackendError>;
    fn pump_callbacks(&mut self) -> Result<(), SteamBackendError>;
    fn poll_event(&mut self) -> Option<SteamBackendEvent>;
    fn take_callback_overflow(&mut self) -> bool;

    /// Narrows level-triggered native lobby callbacks to the joining/active
    /// lobby. Backends without a native asynchronous callback mailbox can
    /// ignore this hint.
    fn set_callback_lobby_scope(&mut self, _lobby: Option<SteamLobbyId>) {}

    /// Retires a canceled create/join operation. Native backends use this to
    /// free the bounded live-operation slot while retaining monotonic knowledge
    /// that a delayed completion is stale and cleanup-only.
    fn retire_lobby_operation(&mut self, _operation_id: SteamOperationId) {}

    fn steam_input_snapshot(&self) -> SteamInputSnapshot {
        SteamInputSnapshot::default()
    }

    /// Returns current overlay readiness. Implementations must query current
    /// state rather than cache startup readiness because Steam may need several
    /// seconds to hook the process.
    fn is_overlay_enabled(&self) -> bool {
        false
    }

    /// Latest coalesced `GameOverlayActivated` state. This is presentation and
    /// input-routing state, never a gameplay or protocol event.
    fn is_overlay_active(&self) -> bool {
        false
    }

    fn set_steam_input_action_set(
        &mut self,
        _action_set: SteamInputActionSet,
    ) -> Result<(), SteamBackendError> {
        Ok(())
    }

    fn show_steam_input_binding_panel(
        &mut self,
        _local_ordinal: usize,
    ) -> Result<bool, SteamBackendError> {
        Ok(false)
    }

    fn create_lobby(
        &mut self,
        operation_id: SteamOperationId,
        visibility: LobbyVisibility,
        maximum_peers: u8,
    ) -> Result<(), SteamBackendError>;
    fn join_lobby(
        &mut self,
        operation_id: SteamOperationId,
        lobby: SteamLobbyId,
    ) -> Result<(), SteamBackendError>;
    fn leave_lobby(&mut self, lobby: SteamLobbyId);
    fn set_lobby_joinable(
        &mut self,
        lobby: SteamLobbyId,
        joinable: bool,
    ) -> Result<(), SteamBackendError>;
    fn set_lobby_data(
        &mut self,
        lobby: SteamLobbyId,
        key: &'static str,
        value: &str,
    ) -> Result<(), SteamBackendError>;
    fn lobby_data(
        &self,
        lobby: SteamLobbyId,
        key: &'static str,
    ) -> Result<Option<String>, SteamBackendError>;
    fn set_member_data(
        &mut self,
        lobby: SteamLobbyId,
        key: &'static str,
        value: &str,
    ) -> Result<(), SteamBackendError>;
    fn member_data(
        &self,
        lobby: SteamLobbyId,
        user: SteamUserId,
        key: &'static str,
    ) -> Result<Option<String>, SteamBackendError>;
    fn lobby_owner(&self, lobby: SteamLobbyId) -> Result<SteamUserId, SteamBackendError>;
    fn lobby_members(&self, lobby: SteamLobbyId) -> Result<Vec<SteamUserId>, SteamBackendError>;
    fn is_friend(&self, user: SteamUserId) -> Result<bool, SteamBackendError>;
    fn open_invite_overlay(&mut self, lobby: SteamLobbyId) -> Result<(), SteamBackendError>;
    fn launch_command_line(&self) -> Result<String, SteamBackendError>;
    fn clear_rich_presence(&mut self);
    fn set_rich_presence(
        &mut self,
        key: &'static str,
        value: Option<&str>,
    ) -> Result<(), SteamBackendError>;

    fn issue_auth_ticket(
        &mut self,
        remote_user: SteamUserId,
    ) -> Result<BackendIssuedAuthTicket, SteamBackendError>;
    fn cancel_auth_ticket(&mut self, handle: AuthTicketHandle);
    fn begin_auth_session(
        &mut self,
        user: SteamUserId,
        ticket: &[u8],
    ) -> Result<(), SteamBackendError>;
    fn end_auth_session(&mut self, user: SteamUserId);
    fn license_status(
        &self,
        user: SteamUserId,
        app_id: SteamAppId,
    ) -> Result<LicenseStatus, SteamBackendError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SteamPlatformState {
    Idle,
    Creating,
    Joining,
    InLobby(SteamLobbyId),
    Faulted,
}

#[derive(Clone, Debug)]
struct PendingCreate {
    operation_id: SteamOperationId,
    request: LobbyCreateRequest,
    metadata: LobbyMetadata,
}

#[derive(Clone, Debug)]
struct PendingJoin {
    operation_id: SteamOperationId,
    intent: LobbyJoinIntent,
    local_seats: u8,
    observed_metadata: Option<LobbyMetadata>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MemberCommitMarker {
    Staging { revision: u16 },
    Committed { revision: u16, ready: bool },
}

#[derive(Clone, Copy, Debug)]
struct CachedMemberDeclaration {
    user: SteamUserId,
    /// Last coherent declaration. It remains only as continuity history while
    /// `current_valid` is false and is never projected into the live roster in
    /// that state.
    accepted: Option<MemberLoadoutDeclaration>,
    current_valid: bool,
    current_ready: bool,
    provisional_seats: Option<u8>,
    invalid: bool,
    recovery_revision_floor: u16,
    last_rejection: Option<MemberDeclarationRejection>,
}

impl CachedMemberDeclaration {
    const fn new(user: SteamUserId, provisional_seats: Option<u8>) -> Self {
        Self {
            user,
            accepted: None,
            current_valid: false,
            current_ready: false,
            provisional_seats,
            invalid: false,
            recovery_revision_floor: 0,
            last_rejection: None,
        }
    }

    fn projected_member(self) -> LobbyMember {
        if self.current_valid {
            let declaration = self
                .accepted
                .expect("a valid cached declaration always has accepted content");
            LobbyMember {
                user: self.user,
                readiness: MemberReadiness::Declared {
                    ready: self.current_ready,
                    local_seats: declaration.seat_count(),
                },
                loadout: Some(declaration),
            }
        } else {
            LobbyMember {
                user: self.user,
                readiness: MemberReadiness::Pending,
                loadout: None,
            }
        }
    }
}

struct RawMemberDeclaration {
    user: SteamUserId,
    marker: Option<String>,
    seats: Option<String>,
    loadout: Option<String>,
}

struct RosterRefreshBatch {
    updates: [Option<(SteamUserId, MemberDataOutcome)>; MAX_STEAM_LOBBY_MEMBERS],
    len: usize,
}

impl Default for RosterRefreshBatch {
    fn default() -> Self {
        Self {
            updates: [None; MAX_STEAM_LOBBY_MEMBERS],
            len: 0,
        }
    }
}

impl RosterRefreshBatch {
    fn push(&mut self, user: SteamUserId, outcome: MemberDataOutcome) {
        if self.len < self.updates.len() {
            self.updates[self.len] = Some((user, outcome));
            self.len += 1;
        }
    }

    fn iter(&self) -> impl Iterator<Item = (SteamUserId, MemberDataOutcome)> + '_ {
        self.updates[..self.len].iter().flatten().copied()
    }
}

#[derive(Clone, Debug)]
struct ActiveLobby {
    id: SteamLobbyId,
    metadata: LobbyMetadata,
    authority_user: SteamUserId,
    admission_enabled: bool,
    effective_joinable: bool,
    roster: [Option<LobbyMember>; MAX_STEAM_LOBBY_MEMBERS],
    declarations: [Option<CachedMemberDeclaration>; MAX_STEAM_LOBBY_MEMBERS],
    roster_len: usize,
}

#[derive(Clone, Debug)]
enum InternalState {
    Idle,
    Creating(PendingCreate),
    Joining(PendingJoin),
    InLobby(ActiveLobby),
    Faulted,
}

#[derive(Clone, Copy, Debug)]
struct IssuedTicketRecord {
    handle: AuthTicketHandle,
    remote_user: SteamUserId,
    ready: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AdmissionStatus {
    Waiting,
    Approved {
        license_owner_user: SteamUserId,
        consumed: bool,
    },
    Rejected,
}

#[derive(Clone, Copy, Debug)]
struct PeerAdmissionRecord {
    lobby: SteamLobbyId,
    user: SteamUserId,
    local_seats: u8,
    purpose: AdmissionPurpose,
    expires_at_ms: u64,
    status: AdmissionStatus,
}

/// Steam platform service. This is intentionally not `Clone`: one instance owns
/// the process's one client callback pump, active auth tickets, and auth sessions.
pub struct SteamPlatform<B: SteamBackend> {
    config: SteamClientConfig,
    backend: B,
    local_user: SteamUserId,
    state: InternalState,
    public_events: VecDeque<SteamPlatformEvent>,
    issued_tickets: [Option<IssuedTicketRecord>; MAX_STEAM_LOBBY_MEMBERS],
    admissions: [Option<PeerAdmissionRecord>; MAX_STEAM_LOBBY_MEMBERS],
    next_operation_id: u64,
    last_fault: Option<SteamPlatformError>,
    last_now_ms: u64,
}

impl<B: SteamBackend> SteamPlatform<B> {
    pub fn new(
        config: SteamClientConfig,
        backend: B,
        now_ms: u64,
    ) -> Result<Self, SteamPlatformError> {
        config.validate()?;
        let backend_app_id = backend.configured_app_id()?;
        if backend_app_id != config.app_id {
            return Err(SteamPlatformError::Backend(
                SteamBackendError::AppIdMismatch,
            ));
        }
        let local_user = backend.local_user()?;
        let launch_command = backend.launch_command_line()?;
        let mut platform = Self {
            config,
            backend,
            local_user,
            state: InternalState::Idle,
            public_events: VecDeque::with_capacity(config.event_capacity),
            issued_tickets: [None; MAX_STEAM_LOBBY_MEMBERS],
            admissions: [None; MAX_STEAM_LOBBY_MEMBERS],
            next_operation_id: 1,
            last_fault: None,
            last_now_ms: now_ms,
        };
        platform.queue_launch_intent(&launch_command, now_ms)?;
        Ok(platform)
    }

    pub fn state(&self) -> SteamPlatformState {
        match &self.state {
            InternalState::Idle => SteamPlatformState::Idle,
            InternalState::Creating(_) => SteamPlatformState::Creating,
            InternalState::Joining(_) => SteamPlatformState::Joining,
            InternalState::InLobby(active) => SteamPlatformState::InLobby(active.id),
            InternalState::Faulted => SteamPlatformState::Faulted,
        }
    }

    pub const fn config(&self) -> SteamClientConfig {
        self.config
    }

    pub const fn local_user(&self) -> SteamUserId {
        self.local_user
    }

    pub fn last_fault(&self) -> Option<SteamPlatformError> {
        self.last_fault
    }

    pub fn poll_event(&mut self) -> Option<SteamPlatformEvent> {
        self.public_events.pop_front()
    }

    pub fn steam_input_snapshot(&self) -> SteamInputSnapshot {
        self.backend.steam_input_snapshot()
    }

    pub fn is_overlay_enabled(&self) -> bool {
        self.backend.is_overlay_enabled()
    }

    pub fn is_overlay_active(&self) -> bool {
        self.backend.is_overlay_active()
    }

    pub fn set_steam_input_action_set(
        &mut self,
        action_set: SteamInputActionSet,
    ) -> Result<(), SteamPlatformError> {
        self.backend
            .set_steam_input_action_set(action_set)
            .map_err(Into::into)
    }

    pub fn show_steam_input_binding_panel(
        &mut self,
        local_ordinal: usize,
    ) -> Result<SteamOverlayRequestStatus, SteamPlatformError> {
        if local_ordinal >= MAX_STEAM_INPUT_CONTROLLERS {
            return Err(SteamPlatformError::Backend(SteamBackendError::InvalidData));
        }
        if self
            .backend
            .steam_input_snapshot()
            .controller(local_ordinal)
            .is_none()
        {
            return Err(SteamPlatformError::Backend(SteamBackendError::InvalidData));
        }
        if !self.backend.is_overlay_enabled() {
            return Ok(SteamOverlayRequestStatus::Unavailable);
        }
        let shown = self
            .backend
            .show_steam_input_binding_panel(local_ordinal)
            .map_err(SteamPlatformError::from)?;
        Ok(if shown {
            SteamOverlayRequestStatus::Submitted
        } else {
            SteamOverlayRequestStatus::Unavailable
        })
    }

    pub fn roster(&self) -> &[Option<LobbyMember>; MAX_STEAM_LOBBY_MEMBERS] {
        match &self.state {
            InternalState::InLobby(active) => &active.roster,
            _ => &[None; MAX_STEAM_LOBBY_MEMBERS],
        }
    }

    pub fn roster_len(&self) -> usize {
        match &self.state {
            InternalState::InLobby(active) => active.roster_len,
            _ => 0,
        }
    }

    pub fn lobby_metadata(&self) -> Option<&LobbyMetadata> {
        match &self.state {
            InternalState::InLobby(active) => Some(&active.metadata),
            _ => None,
        }
    }

    pub fn lobby_owner(&self) -> Option<SteamUserId> {
        match &self.state {
            InternalState::InLobby(active) => Some(active.authority_user),
            _ => None,
        }
    }

    pub fn accepted_seat_total(&self) -> u8 {
        let InternalState::InLobby(active) = &self.state else {
            return 0;
        };
        active.roster[..active.roster_len]
            .iter()
            .flatten()
            .filter_map(|member| match member.readiness {
                MemberReadiness::Declared { local_seats, .. } => Some(local_seats),
                MemberReadiness::Pending => None,
            })
            .fold(0_u8, u8::saturating_add)
    }

    pub fn seat_capacity(&self) -> Option<u8> {
        match &self.state {
            InternalState::InLobby(active) => Some(active.metadata.seat_capacity),
            _ => None,
        }
    }

    pub fn effective_joinable(&self) -> bool {
        matches!(
            &self.state,
            InternalState::InLobby(ActiveLobby {
                effective_joinable: true,
                ..
            })
        )
    }

    pub fn all_members_ready(&self) -> bool {
        let InternalState::InLobby(active) = &self.state else {
            return false;
        };
        active.roster_len > 0
            && active.roster[..active.roster_len].iter().all(|member| {
                matches!(
                    member,
                    Some(LobbyMember {
                        readiness: MemberReadiness::Declared { ready: true, .. },
                        ..
                    })
                )
            })
    }

    /// Strong match-start gate: every Steam member is ready and has published
    /// a syntactically and semantically valid seat/loadout declaration whose
    /// seat count agrees with the separately bounded readiness metadata.
    pub fn all_members_match_ready(&self) -> bool {
        let InternalState::InLobby(active) = &self.state else {
            return false;
        };
        active.roster_len > 0
            && active.roster[..active.roster_len].iter().all(|member| {
                matches!(
                    member,
                    Some(LobbyMember {
                        readiness: MemberReadiness::Declared {
                            ready: true,
                            local_seats,
                        },
                        loadout: Some(loadout),
                        ..
                    }) if *local_seats == loadout.seat_count()
                )
            })
    }

    pub fn member_loadout(&self, user: SteamUserId) -> Option<MemberLoadoutDeclaration> {
        let InternalState::InLobby(active) = &self.state else {
            return None;
        };
        active.roster[..active.roster_len]
            .iter()
            .flatten()
            .find(|member| member.user == user)
            .and_then(|member| member.loadout)
    }

    pub fn create_lobby(
        &mut self,
        request: LobbyCreateRequest,
        metadata: LobbyMetadata,
    ) -> Result<(), SteamPlatformError> {
        self.require_operational_idle()?;
        request.validate(&metadata, self.config)?;
        if metadata.authority == AuthorityKind::Dedicated {
            return Err(SteamPlatformError::DedicatedSdrUnavailable);
        }
        let operation_id = self.allocate_operation_id()?;
        self.backend
            .create_lobby(operation_id, request.visibility, request.maximum_peers)?;
        self.state = InternalState::Creating(PendingCreate {
            operation_id,
            request,
            metadata,
        });
        Ok(())
    }

    pub fn join_lobby(
        &mut self,
        intent: LobbyJoinIntent,
        local_seats: u8,
        now_ms: u64,
    ) -> Result<(), SteamPlatformError> {
        self.require_operational_idle()?;
        self.advance_time(now_ms)?;
        intent.validate(now_ms)?;
        validate_local_seats(local_seats)?;
        let operation_id = self.allocate_operation_id()?;
        self.backend.set_callback_lobby_scope(Some(intent.lobby));
        if let Err(error) = self.backend.join_lobby(operation_id, intent.lobby) {
            self.backend.set_callback_lobby_scope(None);
            return Err(error.into());
        }
        self.state = InternalState::Joining(PendingJoin {
            operation_id,
            intent,
            local_seats,
            observed_metadata: None,
        });
        Ok(())
    }

    /// Retires an in-flight create/join without waiting for Steam's callback.
    ///
    /// The backend operation itself may already be completing. Its monotonic
    /// operation ID remains retired; a later success is cleaned up and a later
    /// failure is ignored, so this platform can immediately start a new
    /// operation without callback misattribution.
    pub fn cancel_pending_lobby_operation(&mut self) -> Result<bool, SteamPlatformError> {
        match self.state.clone() {
            InternalState::Creating(pending) => {
                self.backend.retire_lobby_operation(pending.operation_id);
                self.backend.set_callback_lobby_scope(None);
                self.state = InternalState::Idle;
                Ok(true)
            }
            InternalState::Joining(pending) => {
                self.backend.retire_lobby_operation(pending.operation_id);
                // Fake and some native implementations may have installed
                // membership before the completion callback becomes visible.
                // A stale success also repeats this cleanup when it arrives.
                self.backend.leave_lobby(pending.intent.lobby);
                self.backend.clear_rich_presence();
                self.backend.set_callback_lobby_scope(None);
                self.state = InternalState::Idle;
                Ok(true)
            }
            InternalState::Idle | InternalState::InLobby(_) => Ok(false),
            InternalState::Faulted => Err(SteamPlatformError::Faulted),
        }
    }

    pub fn leave_lobby(&mut self) -> Result<(), SteamPlatformError> {
        let lobby = self.active_lobby_id()?;
        self.teardown_lobby(lobby, true);
        self.state = InternalState::Idle;
        let queued = self.push_event(SteamPlatformEvent::LobbyLeft {
            lobby,
            reason: LobbyExitReason::Requested,
        });
        match queued {
            Ok(()) => Ok(()),
            Err(error) => self.fail_closed(error),
        }
    }

    pub fn set_readiness(
        &mut self,
        ready: bool,
        local_seats: u8,
    ) -> Result<(), SteamPlatformError> {
        validate_local_seats(local_seats)?;
        self.revalidate_active_lobby_metadata()?;
        let (lobby, revision) = match &self.state {
            InternalState::InLobby(active) => {
                let cache = active
                    .declarations
                    .iter()
                    .flatten()
                    .find(|cache| cache.user == self.local_user)
                    .filter(|cache| cache.current_valid && !cache.invalid)
                    .ok_or(SteamPlatformError::MemberMetadataPending)?;
                let declaration = cache
                    .accepted
                    .ok_or(SteamPlatformError::MemberMetadataPending)?;
                if declaration.seat_count() != local_seats {
                    return Err(SteamPlatformError::MetadataMismatch);
                }
                (active.id, declaration.revision())
            }
            _ => return Err(SteamPlatformError::InvalidState),
        };
        let update = (|| {
            let marker =
                encode_member_commit_marker(MemberCommitMarker::Committed { revision, ready });
            self.backend
                .set_member_data(lobby, MEMBER_KEY_READY, &marker)?;
            let batch = self.refresh_roster(Some(self.local_user))?;
            self.push_member_refresh_events(lobby, &batch)
        })();
        match update {
            Ok(()) => Ok(()),
            Err(error) => self.fail_closed(error),
        }
    }

    /// Publishes one complete local couch-seat declaration. Readiness is set to
    /// false before changing the declaration and set to the requested value
    /// last, so observers can never accept a mixed old/new loadout as ready.
    pub fn set_member_declaration(
        &mut self,
        declaration: MemberLoadoutDeclaration,
        ready: bool,
    ) -> Result<(), SteamPlatformError> {
        let local_seats = declaration.seat_count();
        validate_local_seats(local_seats)?;
        self.revalidate_active_lobby_metadata()?;
        let (lobby, seat_capacity, declared_by_other_members) = match &self.state {
            InternalState::InLobby(active) => {
                let declared_by_other_members = active.roster[..active.roster_len]
                    .iter()
                    .flatten()
                    .filter(|member| member.user != self.local_user)
                    .filter_map(|member| match member.readiness {
                        MemberReadiness::Pending => None,
                        MemberReadiness::Declared { local_seats, .. } => {
                            Some(u16::from(local_seats))
                        }
                    })
                    .sum::<u16>();
                (
                    active.id,
                    active.metadata.seat_capacity,
                    declared_by_other_members,
                )
            }
            _ => return Err(SteamPlatformError::InvalidState),
        };
        if declared_by_other_members + u16::from(local_seats) > u16::from(seat_capacity) {
            return Err(SteamPlatformError::LobbyCapacityExceeded);
        }
        self.validate_local_declaration_continuity(declaration)?;
        let update = (|| {
            let staging = encode_member_commit_marker(MemberCommitMarker::Staging {
                revision: declaration.revision(),
            });
            self.backend
                .set_member_data(lobby, MEMBER_KEY_READY, &staging)?;
            self.backend
                .set_member_data(lobby, MEMBER_KEY_SEATS, &local_seats.to_string())?;
            self.backend
                .set_member_data(lobby, MEMBER_KEY_LOADOUT, declaration.as_str())?;
            let committed = encode_member_commit_marker(MemberCommitMarker::Committed {
                revision: declaration.revision(),
                ready,
            });
            self.backend
                .set_member_data(lobby, MEMBER_KEY_READY, &committed)?;
            let batch = self.refresh_roster(Some(self.local_user))?;
            self.push_member_refresh_events(lobby, &batch)
        })();
        match update {
            Ok(()) => Ok(()),
            Err(error) => self.fail_closed(error),
        }
    }

    pub fn set_accepting_peers(&mut self, accepting: bool) -> Result<(), SteamPlatformError> {
        let (lobby, owner) = match &self.state {
            InternalState::InLobby(active) => (active.id, active.authority_user),
            _ => return Err(SteamPlatformError::InvalidState),
        };
        if owner != self.local_user {
            return Err(SteamPlatformError::NotLobbyOwner);
        }
        self.revalidate_active_lobby_metadata()?;
        let update = (|| {
            if !accepting {
                self.backend.set_lobby_joinable(lobby, false)?;
                self.backend.set_lobby_data(lobby, KEY_OPEN, "0")?;
            }
            if let InternalState::InLobby(active) = &mut self.state {
                active.effective_joinable &= accepting;
                active.admission_enabled = accepting;
            }
            self.backend
                .set_lobby_data(lobby, KEY_ADMISSION, bool_text(accepting))?;
            self.reconcile_owner_joinability()?;
            self.refresh_presence_or_warn()
        })();
        match update {
            Ok(()) => Ok(()),
            Err(error) => self.fail_closed(error),
        }
    }

    pub fn open_invite_overlay(&mut self) -> Result<SteamOverlayRequestStatus, SteamPlatformError> {
        let active = match &self.state {
            InternalState::InLobby(active) if active.effective_joinable => active,
            _ => return Err(SteamPlatformError::InvalidState),
        };
        if !self.backend.is_overlay_enabled() {
            return Ok(SteamOverlayRequestStatus::Unavailable);
        }
        self.backend.open_invite_overlay(active.id)?;
        Ok(SteamOverlayRequestStatus::Submitted)
    }

    pub fn issue_auth_ticket(
        &mut self,
        remote_user: SteamUserId,
    ) -> Result<IssuedAuthTicket, SteamPlatformError> {
        self.active_lobby_id()?;
        if self
            .issued_tickets
            .iter()
            .flatten()
            .any(|ticket| ticket.remote_user == remote_user)
        {
            return Err(SteamPlatformError::InvalidState);
        }
        let slot = self
            .issued_tickets
            .iter()
            .position(Option::is_none)
            .ok_or(SteamPlatformError::AuthCapacityExceeded)?;
        let issued = self.backend.issue_auth_ticket(remote_user)?;
        let issued_handle = issued.handle;
        if issued.bytes.is_empty() {
            self.backend.cancel_auth_ticket(issued_handle);
            return Err(SteamPlatformError::AuthTicketEmpty);
        }
        if issued.bytes.len() > MAX_STEAM_AUTH_TICKET_BYTES {
            self.backend.cancel_auth_ticket(issued_handle);
            return Err(SteamPlatformError::AuthTicketTooLarge);
        }
        let (handle, bytes) = issued.into_parts();
        self.issued_tickets[slot] = Some(IssuedTicketRecord {
            handle,
            remote_user,
            ready: false,
        });
        Ok(IssuedAuthTicket {
            handle,
            remote_user,
            bytes,
        })
    }

    pub fn auth_ticket_is_ready(&self, handle: AuthTicketHandle) -> bool {
        self.issued_tickets
            .iter()
            .flatten()
            .any(|ticket| ticket.handle == handle && ticket.ready)
    }

    pub fn cancel_auth_ticket(
        &mut self,
        handle: AuthTicketHandle,
    ) -> Result<(), SteamPlatformError> {
        let slot = self
            .issued_tickets
            .iter()
            .position(|ticket| ticket.is_some_and(|ticket| ticket.handle == handle))
            .ok_or(SteamPlatformError::InvalidState)?;
        self.backend.cancel_auth_ticket(handle);
        self.issued_tickets[slot] = None;
        Ok(())
    }

    pub fn begin_peer_authentication(
        &mut self,
        lobby: SteamLobbyId,
        user: SteamUserId,
        ticket: &[u8],
        purpose: AdmissionPurpose,
        now_ms: u64,
    ) -> Result<(), SteamPlatformError> {
        if ticket.is_empty() {
            return Err(SteamPlatformError::AuthTicketEmpty);
        }
        if ticket.len() > MAX_STEAM_AUTH_TICKET_BYTES {
            return Err(SteamPlatformError::AuthTicketTooLarge);
        }
        self.advance_time(now_ms)?;
        self.revalidate_active_lobby_metadata()?;
        self.refresh_lobby_flags()?;
        let active = match &self.state {
            InternalState::InLobby(active) if active.id == lobby => active,
            InternalState::InLobby(_) => return Err(SteamPlatformError::UnexpectedLobby),
            _ => return Err(SteamPlatformError::InvalidState),
        };
        if purpose == AdmissionPurpose::Initial && !active.admission_enabled {
            return Err(SteamPlatformError::InvalidState);
        }
        if active.metadata.visibility == LobbyVisibility::FriendsOnly
            && !self.backend.is_friend(user)?
        {
            return Err(SteamPlatformError::FriendsRelationshipRequired);
        }
        let member = active.roster[..active.roster_len]
            .iter()
            .flatten()
            .find(|member| member.user == user)
            .ok_or(SteamPlatformError::MemberNotInExpectedLobby)?;
        let MemberReadiness::Declared { local_seats, .. } = member.readiness else {
            return Err(SteamPlatformError::MemberMetadataPending);
        };
        if let Some(admission) = self
            .admissions
            .iter()
            .flatten()
            .find(|admission| admission.user == user)
        {
            // Reliable pre-game signaling may redeliver the same ticket before
            // or after Steam's validation callback. The existing session is
            // already at least as authoritative as the replay, so treat an
            // exact lobby/purpose replay as an idempotent no-op. A conflicting
            // purpose or cross-lobby reuse still fails closed.
            return if admission.lobby == lobby && admission.purpose == purpose {
                Ok(())
            } else {
                Err(SteamPlatformError::InvalidState)
            };
        }
        let slot = self
            .admissions
            .iter()
            .position(Option::is_none)
            .ok_or(SteamPlatformError::AuthCapacityExceeded)?;
        let expires_at_ms = deadline(now_ms, self.config.auth_intent_ttl_ms)?;
        self.backend.begin_auth_session(user, ticket)?;
        self.admissions[slot] = Some(PeerAdmissionRecord {
            lobby,
            user,
            local_seats,
            purpose,
            expires_at_ms,
            status: AdmissionStatus::Waiting,
        });
        Ok(())
    }

    pub fn consume_authenticated_admission(
        &mut self,
        lobby: SteamLobbyId,
        user: SteamUserId,
        now_ms: u64,
    ) -> Result<AuthenticatedSteamPeer, SteamPlatformError> {
        self.advance_time(now_ms)?;
        let admission = self
            .admissions
            .iter_mut()
            .flatten()
            .find(|admission| admission.user == user)
            .ok_or(SteamPlatformError::AuthenticationPending)?;
        if admission.lobby != lobby {
            return Err(SteamPlatformError::UnexpectedLobby);
        }
        let deadline_applies = !matches!(
            admission.status,
            AdmissionStatus::Approved { consumed: true, .. } | AdmissionStatus::Rejected
        );
        if deadline_applies && now_ms >= admission.expires_at_ms {
            return Err(SteamPlatformError::AuthIntentExpired);
        }
        match &mut admission.status {
            AdmissionStatus::Waiting => Err(SteamPlatformError::AuthenticationPending),
            AdmissionStatus::Rejected => Err(SteamPlatformError::AuthenticationRejected),
            AdmissionStatus::Approved {
                license_owner_user,
                consumed,
            } => {
                if *consumed {
                    return Err(SteamPlatformError::AdmissionAlreadyConsumed);
                }
                *consumed = true;
                Ok(AuthenticatedSteamPeer {
                    lobby,
                    user,
                    license_owner_user: *license_owner_user,
                    authenticated_user: user.authenticated(),
                    local_seats: admission.local_seats,
                    purpose: admission.purpose,
                })
            }
        }
    }

    pub fn end_peer_authentication(&mut self, user: SteamUserId) -> Result<(), SteamPlatformError> {
        let slot = self
            .admissions
            .iter()
            .position(|admission| admission.is_some_and(|entry| entry.user == user))
            .ok_or(SteamPlatformError::InvalidState)?;
        self.backend.end_auth_session(user);
        self.admissions[slot] = None;
        Ok(())
    }

    pub fn pump(&mut self, now_ms: u64) -> Result<(), SteamPlatformError> {
        if matches!(self.state, InternalState::Faulted) {
            return Err(SteamPlatformError::Faulted);
        }
        if let Err(error) = self.advance_time(now_ms) {
            return self.fail_closed(error);
        }
        if let Err(error) = self.backend.pump_callbacks() {
            return self.fail_closed(SteamPlatformError::Backend(error));
        }
        if self.backend.take_callback_overflow() {
            return self.fail_closed(SteamPlatformError::Backend(
                SteamBackendError::CallbackQueueOverflow,
            ));
        }
        while let Some(event) = self.backend.poll_event() {
            if let Err(error) = self.handle_backend_event(event, now_ms) {
                return self.fail_closed(error);
            }
        }
        self.expire_auth_intents(now_ms)?;
        Ok(())
    }

    pub const fn dedicated_hosted_sdr_support(&self) -> DedicatedSdrSupport {
        DedicatedSdrSupport::UnavailableInPinnedBinding
    }

    fn require_operational_idle(&self) -> Result<(), SteamPlatformError> {
        match self.state {
            InternalState::Idle => Ok(()),
            InternalState::Faulted => Err(SteamPlatformError::Faulted),
            _ => Err(SteamPlatformError::InvalidState),
        }
    }

    fn allocate_operation_id(&mut self) -> Result<SteamOperationId, SteamPlatformError> {
        let operation_id = SteamOperationId(self.next_operation_id);
        self.next_operation_id =
            self.next_operation_id
                .checked_add(1)
                .ok_or(SteamPlatformError::Backend(
                    SteamBackendError::IntegrityFailure,
                ))?;
        Ok(operation_id)
    }

    fn advance_time(&mut self, now_ms: u64) -> Result<(), SteamPlatformError> {
        if now_ms < self.last_now_ms {
            return Err(SteamPlatformError::InvalidTimeout);
        }
        self.last_now_ms = now_ms;
        Ok(())
    }

    fn active_lobby_id(&self) -> Result<SteamLobbyId, SteamPlatformError> {
        match &self.state {
            InternalState::InLobby(active) => Ok(active.id),
            InternalState::Faulted => Err(SteamPlatformError::Faulted),
            _ => Err(SteamPlatformError::InvalidState),
        }
    }

    fn handle_backend_event(
        &mut self,
        event: SteamBackendEvent,
        now_ms: u64,
    ) -> Result<(), SteamPlatformError> {
        match event {
            SteamBackendEvent::LobbyCreated {
                operation_id,
                result,
            } => self.handle_lobby_created(operation_id, result),
            SteamBackendEvent::LobbyJoined {
                operation_id,
                requested,
                result,
            } => self.handle_lobby_joined(operation_id, requested, result, now_ms),
            SteamBackendEvent::LobbyMembershipChanged {
                lobby,
                user,
                change,
            } => self.handle_membership_change(lobby, user, change),
            SteamBackendEvent::LobbyMembershipResync { lobby } => {
                self.handle_membership_resync(lobby)
            }
            SteamBackendEvent::LobbyDataChanged { lobby, subject } => {
                if self.active_lobby_id().ok() != Some(lobby) {
                    return Ok(());
                }
                match subject {
                    LobbyDataSubject::Lobby => {
                        self.revalidate_active_lobby_metadata()?;
                        self.refresh_lobby_flags()?;
                        self.refresh_presence_or_warn()?;
                        self.push_event(SteamPlatformEvent::LobbyMetadataChanged { lobby })
                    }
                    LobbyDataSubject::Member(user) => {
                        let batch = self.refresh_roster(Some(user))?;
                        self.push_member_refresh_events(lobby, &batch)
                    }
                }
            }
            SteamBackendEvent::LobbyJoinRequested { lobby, friend } => {
                let intent = LobbyJoinIntent {
                    lobby,
                    origin: JoinOrigin::SteamInvite { friend },
                    expires_at_ms: deadline(now_ms, self.config.join_intent_ttl_ms)?,
                };
                self.push_event(SteamPlatformEvent::LobbyJoinRequested(intent))
            }
            SteamBackendEvent::RichPresenceJoinRequested { friend, connect } => {
                let lobby = match parse_connect_lobby_command(&connect) {
                    Ok(Some(lobby)) => lobby,
                    Ok(None)
                    | Err(
                        SteamPlatformError::InvalidConnectCommand
                        | SteamPlatformError::ConnectCommandTooLong
                        | SteamPlatformError::DuplicateConnectLobby
                        | SteamPlatformError::ZeroIdentifier,
                    ) => return Ok(()),
                    Err(error) => return Err(error),
                };
                let intent = LobbyJoinIntent {
                    lobby,
                    origin: JoinOrigin::RichPresence { friend },
                    expires_at_ms: deadline(now_ms, self.config.join_intent_ttl_ms)?,
                };
                self.push_event(SteamPlatformEvent::LobbyJoinRequested(intent))
            }
            SteamBackendEvent::LaunchParametersChanged => {
                let command = self.backend.launch_command_line()?;
                match self.queue_launch_intent(&command, now_ms) {
                    Ok(()) => Ok(()),
                    Err(
                        SteamPlatformError::InvalidConnectCommand
                        | SteamPlatformError::ConnectCommandTooLong
                        | SteamPlatformError::DuplicateConnectLobby
                        | SteamPlatformError::ZeroIdentifier,
                    ) => Ok(()),
                    Err(error) => Err(error),
                }
            }
            SteamBackendEvent::AuthTicketReady { handle, success } => {
                self.handle_auth_ticket_ready(handle, success)
            }
            SteamBackendEvent::AuthSessionValidated {
                user,
                license_owner_user,
                result,
            } => self.handle_auth_validation(user, license_owner_user, result),
            SteamBackendEvent::SteamDisconnected => {
                self.backend.set_callback_lobby_scope(None);
                match self.state.clone() {
                    InternalState::InLobby(active) => {
                        self.teardown_lobby(active.id, true);
                        self.state = InternalState::Idle;
                        self.push_event(SteamPlatformEvent::LobbyLeft {
                            lobby: active.id,
                            reason: LobbyExitReason::SteamDisconnected,
                        })?;
                    }
                    InternalState::Creating(pending) => {
                        self.backend.retire_lobby_operation(pending.operation_id);
                        self.state = InternalState::Idle;
                        self.push_event(SteamPlatformEvent::LobbyCreateFailed(
                            SteamPlatformError::Backend(SteamBackendError::NotLoggedOn),
                        ))?;
                    }
                    InternalState::Joining(pending) => {
                        self.backend.retire_lobby_operation(pending.operation_id);
                        self.backend.leave_lobby(pending.intent.lobby);
                        self.state = InternalState::Idle;
                        self.push_event(SteamPlatformEvent::LobbyJoinRejected {
                            lobby: pending.intent.lobby,
                            reason: SteamPlatformError::Backend(SteamBackendError::NotLoggedOn),
                        })?;
                    }
                    InternalState::Idle | InternalState::Faulted => {}
                }
                Ok(())
            }
            SteamBackendEvent::IntegrityFailure => Err(SteamPlatformError::Backend(
                SteamBackendError::IntegrityFailure,
            )),
        }
    }

    fn handle_lobby_created(
        &mut self,
        operation_id: SteamOperationId,
        result: Result<SteamLobbyId, SteamBackendError>,
    ) -> Result<(), SteamPlatformError> {
        self.validate_completed_operation_id(operation_id)?;
        let pending = match self.state.clone() {
            InternalState::Creating(pending) => {
                if pending.operation_id != operation_id {
                    self.cleanup_stale_lobby_success(result);
                    return Ok(());
                }
                pending
            }
            InternalState::Joining(pending) if pending.operation_id == operation_id => {
                return Err(SteamPlatformError::Backend(
                    SteamBackendError::IntegrityFailure,
                ));
            }
            InternalState::Idle
            | InternalState::Joining(_)
            | InternalState::InLobby(_)
            | InternalState::Faulted => {
                self.cleanup_stale_lobby_success(result);
                return Ok(());
            }
        };
        let lobby = match result {
            Ok(lobby) => lobby,
            Err(error) => {
                self.state = InternalState::Idle;
                return self.push_event(SteamPlatformEvent::LobbyCreateFailed(error.into()));
            }
        };
        let result = self
            .backend
            .set_lobby_joinable(lobby, false)
            .map_err(SteamPlatformError::from)
            .and_then(|()| self.publish_lobby_metadata(lobby, &pending.metadata, true, false));
        if let Err(error) = result {
            self.backend.leave_lobby(lobby);
            self.state = InternalState::Idle;
            self.push_event(SteamPlatformEvent::LobbyCreateFailed(error))?;
            return Ok(());
        }
        self.enter_active_lobby(
            lobby,
            pending.metadata,
            self.local_user,
            true,
            false,
            Some(pending.request.local_seats),
        )?;
        self.push_event(SteamPlatformEvent::LobbyEntered {
            lobby,
            owner: self.local_user,
        })
    }

    fn handle_lobby_joined(
        &mut self,
        operation_id: SteamOperationId,
        requested: SteamLobbyId,
        result: Result<SteamLobbyId, SteamBackendError>,
        now_ms: u64,
    ) -> Result<(), SteamPlatformError> {
        self.validate_completed_operation_id(operation_id)?;
        let pending = match self.state.clone() {
            InternalState::Joining(pending) => {
                if pending.operation_id != operation_id {
                    self.cleanup_stale_join_success(requested, result);
                    return Ok(());
                }
                pending
            }
            InternalState::Creating(pending) if pending.operation_id == operation_id => {
                return Err(SteamPlatformError::Backend(
                    SteamBackendError::IntegrityFailure,
                ));
            }
            InternalState::Idle
            | InternalState::Creating(_)
            | InternalState::InLobby(_)
            | InternalState::Faulted => {
                self.cleanup_stale_join_success(requested, result);
                return Ok(());
            }
        };
        if requested != pending.intent.lobby {
            if let Ok(returned_lobby) = result {
                self.backend.leave_lobby(returned_lobby);
            }
            self.backend.leave_lobby(requested);
            self.backend.leave_lobby(pending.intent.lobby);
            self.backend.clear_rich_presence();
            return Err(SteamPlatformError::UnexpectedLobby);
        }
        let lobby = match result {
            Ok(lobby) if lobby == requested => lobby,
            Ok(other_lobby) => {
                self.backend.leave_lobby(other_lobby);
                self.backend.leave_lobby(requested);
                self.backend.clear_rich_presence();
                return Err(SteamPlatformError::UnexpectedLobby);
            }
            Err(error) => {
                self.backend.leave_lobby(requested);
                self.backend.clear_rich_presence();
                self.backend.set_callback_lobby_scope(None);
                self.state = InternalState::Idle;
                return self.push_event(SteamPlatformEvent::LobbyJoinRejected {
                    lobby: requested,
                    reason: error.into(),
                });
            }
        };
        let activation = (|| {
            self.validate_joined_lobby(&pending, now_ms)?;
            let metadata = read_lobby_metadata(&self.backend, lobby)?;
            if pending
                .observed_metadata
                .as_ref()
                .is_some_and(|observed| observed != &metadata)
            {
                return Err(SteamPlatformError::MetadataMismatch);
            }
            let owner = self.backend.lobby_owner(lobby)?;
            let admission_enabled = read_required_bool(&self.backend, lobby, KEY_ADMISSION)?;
            let effective_joinable = read_required_bool(&self.backend, lobby, KEY_OPEN)?;
            self.enter_active_lobby(
                lobby,
                metadata,
                owner,
                admission_enabled,
                effective_joinable,
                Some(pending.local_seats),
            )?;
            Ok(owner)
        })();
        let owner = match activation {
            Ok(owner) => owner,
            Err(reason) => {
                // Steam has already installed native membership. Every
                // fallible validation/read/activation step after that point
                // is one transaction: clean all capabilities and leave the
                // returned lobby before exposing a recoverable rejection.
                self.teardown_lobby(lobby, true);
                self.state = InternalState::Idle;
                self.push_event(SteamPlatformEvent::LobbyJoinRejected { lobby, reason })?;
                return Ok(());
            }
        };
        self.push_event(SteamPlatformEvent::LobbyEntered { lobby, owner })
    }

    fn validate_completed_operation_id(
        &self,
        operation_id: SteamOperationId,
    ) -> Result<(), SteamPlatformError> {
        if operation_id.get() == 0 || operation_id.get() >= self.next_operation_id {
            Err(SteamPlatformError::Backend(
                SteamBackendError::IntegrityFailure,
            ))
        } else {
            Ok(())
        }
    }

    fn cleanup_stale_lobby_success(&mut self, result: Result<SteamLobbyId, SteamBackendError>) {
        let Ok(lobby) = result else {
            return;
        };
        if self.active_lobby_id().ok() != Some(lobby) {
            self.backend.leave_lobby(lobby);
        }
    }

    fn cleanup_stale_join_success(
        &mut self,
        _requested: SteamLobbyId,
        result: Result<SteamLobbyId, SteamBackendError>,
    ) {
        let Ok(lobby) = result else {
            return;
        };
        // A mismatched success is still attributable to the retired
        // operation and must be left, but it is never accepted as `requested`.
        let current_join_targets_same_lobby = matches!(
            &self.state,
            InternalState::Joining(PendingJoin { intent, .. }) if intent.lobby == lobby
        );
        if self.active_lobby_id().ok() != Some(lobby) && !current_join_targets_same_lobby {
            self.backend.leave_lobby(lobby);
        }
    }

    fn validate_joined_lobby(
        &self,
        pending: &PendingJoin,
        now_ms: u64,
    ) -> Result<(), SteamPlatformError> {
        pending.intent.validate(now_ms)?;
        let metadata = read_lobby_metadata(&self.backend, pending.intent.lobby)?;
        metadata.validate_for_local_client(self.config)?;
        if metadata.authority == AuthorityKind::Dedicated {
            return Err(SteamPlatformError::DedicatedSdrUnavailable);
        }
        if pending.local_seats > metadata.seat_capacity {
            return Err(SteamPlatformError::LobbyCapacityExceeded);
        }
        if !read_required_bool(&self.backend, pending.intent.lobby, KEY_ADMISSION)? {
            return Err(SteamPlatformError::InvalidState);
        }
        let owner = self.backend.lobby_owner(pending.intent.lobby)?;
        match metadata.visibility {
            LobbyVisibility::Private => match pending.intent.origin {
                JoinOrigin::SteamInvite { .. }
                | JoinOrigin::LaunchCommand
                | JoinOrigin::RichPresence { .. } => {}
                JoinOrigin::FriendsList => {
                    return Err(SteamPlatformError::PrivateLobbyRequiresInvite);
                }
            },
            LobbyVisibility::FriendsOnly => {
                if !self.backend.is_friend(owner)? {
                    return Err(SteamPlatformError::FriendsRelationshipRequired);
                }
            }
            LobbyVisibility::Public => {
                if !self.config.allow_public_lobbies {
                    return Err(SteamPlatformError::PublicLobbiesDisabled);
                }
            }
        }
        let members = bounded_members(&self.backend, pending.intent.lobby)?;
        if !members.contains(&self.local_user) || !members.contains(&owner) {
            return Err(SteamPlatformError::MemberNotInExpectedLobby);
        }
        let mut accepted_other_seats = 0_u16;
        for member in members
            .iter()
            .copied()
            .filter(|member| *member != self.local_user)
        {
            if let Some(seats) =
                read_coherent_member_seat_count(&self.backend, pending.intent.lobby, member)?
            {
                accepted_other_seats = accepted_other_seats.saturating_add(u16::from(seats));
            }
        }
        if accepted_other_seats + u16::from(pending.local_seats) > u16::from(metadata.seat_capacity)
        {
            return Err(SteamPlatformError::LobbyCapacityExceeded);
        }
        Ok(())
    }

    fn enter_active_lobby(
        &mut self,
        lobby: SteamLobbyId,
        metadata: LobbyMetadata,
        authority_user: SteamUserId,
        admission_enabled: bool,
        effective_joinable: bool,
        local_provisional_seats: Option<u8>,
    ) -> Result<(), SteamPlatformError> {
        metadata.validate_for_local_client(self.config)?;
        self.backend.set_callback_lobby_scope(Some(lobby));
        let mut declarations = [None; MAX_STEAM_LOBBY_MEMBERS];
        declarations[0] = Some(CachedMemberDeclaration::new(
            self.local_user,
            local_provisional_seats,
        ));
        self.state = InternalState::InLobby(ActiveLobby {
            id: lobby,
            metadata,
            authority_user,
            admission_enabled,
            effective_joinable,
            roster: [None; MAX_STEAM_LOBBY_MEMBERS],
            declarations,
            roster_len: 0,
        });
        self.refresh_roster(None)?;
        self.reconcile_owner_joinability()?;
        self.refresh_presence_or_warn()?;
        Ok(())
    }

    fn handle_membership_change(
        &mut self,
        lobby: SteamLobbyId,
        user: SteamUserId,
        change: LobbyMembershipChange,
    ) -> Result<(), SteamPlatformError> {
        let (active_id, previous_authority, retained_metadata, previous_users) = match &self.state {
            InternalState::InLobby(active) => {
                let mut users = [None; MAX_STEAM_LOBBY_MEMBERS];
                for (index, member) in active.roster[..active.roster_len]
                    .iter()
                    .flatten()
                    .enumerate()
                {
                    users[index] = Some(member.user);
                }
                (
                    active.id,
                    active.authority_user,
                    active.metadata.clone(),
                    users,
                )
            }
            _ => return Ok(()),
        };
        if active_id != lobby {
            return Ok(());
        }
        if user == self.local_user
            && matches!(
                change,
                LobbyMembershipChange::Left
                    | LobbyMembershipChange::Disconnected
                    | LobbyMembershipChange::Kicked
                    | LobbyMembershipChange::Banned
            )
        {
            self.teardown_lobby(lobby, false);
            self.state = InternalState::Idle;
            return self.push_event(SteamPlatformEvent::LobbyLeft {
                lobby,
                reason: LobbyExitReason::Removed,
            });
        }
        if matches!(
            change,
            LobbyMembershipChange::Left
                | LobbyMembershipChange::Disconnected
                | LobbyMembershipChange::Kicked
                | LobbyMembershipChange::Banned
        ) {
            self.cleanup_peer_authentication(user);
        }
        let member_updates = self.refresh_roster(None)?;
        let current_users = match &self.state {
            InternalState::InLobby(active) => active.roster,
            _ => [None; MAX_STEAM_LOBBY_MEMBERS],
        };
        for departed in previous_users.iter().flatten().copied() {
            if departed != self.local_user
                && !current_users
                    .iter()
                    .flatten()
                    .any(|member| member.user == departed)
            {
                self.cleanup_peer_authentication(departed);
            }
        }
        self.push_member_refresh_events(lobby, &member_updates)?;
        let authority_present = match &self.state {
            InternalState::InLobby(active) => active.roster[..active.roster_len]
                .iter()
                .flatten()
                .any(|member| member.user == previous_authority),
            _ => false,
        };
        if !authority_present {
            let successor = self.backend.lobby_owner(lobby)?;
            let successor_present = match &self.state {
                InternalState::InLobby(active) => active.roster[..active.roster_len]
                    .iter()
                    .flatten()
                    .any(|member| member.user == successor),
                _ => false,
            };
            if successor == previous_authority || !successor_present {
                return Err(SteamPlatformError::Backend(
                    SteamBackendError::IntegrityFailure,
                ));
            }
            let observed_metadata = read_lobby_metadata(&self.backend, lobby)?;
            observed_metadata.validate_for_local_client(self.config)?;
            if observed_metadata != retained_metadata {
                return Err(SteamPlatformError::MetadataMismatch);
            }
            if let InternalState::InLobby(active) = &mut self.state {
                active.authority_user = successor;
            }
            let successor_updates = self.refresh_roster(None)?;
            self.push_member_refresh_events(lobby, &successor_updates)?;
            self.refresh_presence_or_warn()?;
            return self.push_event(SteamPlatformEvent::AuthorityLost {
                lobby,
                previous_authority,
                successor,
            });
        }
        self.refresh_presence_or_warn()?;
        self.push_event(SteamPlatformEvent::LobbyRosterChanged { lobby })
    }

    fn handle_membership_resync(&mut self, lobby: SteamLobbyId) -> Result<(), SteamPlatformError> {
        if self.active_lobby_id().ok() != Some(lobby) {
            return Ok(());
        }
        self.cleanup_auth();
        // A synthetic local "entered" signal runs the same authoritative
        // roster/owner reconciliation without treating callback pressure as an
        // identityless backend failure.
        self.handle_membership_change(lobby, self.local_user, LobbyMembershipChange::Entered)
    }

    fn refresh_roster(
        &mut self,
        subject: Option<SteamUserId>,
    ) -> Result<RosterRefreshBatch, SteamPlatformError> {
        let (lobby, capacity, authority_user, old_declarations) = match &self.state {
            InternalState::InLobby(active) => (
                active.id,
                active.metadata.seat_capacity,
                active.authority_user,
                active.declarations,
            ),
            _ => return Err(SteamPlatformError::InvalidState),
        };
        let users = bounded_members(&self.backend, lobby)?;
        if users.is_empty() || !users.contains(&self.local_user) {
            return Err(SteamPlatformError::MemberNotInExpectedLobby);
        }

        let mut raw: [Option<RawMemberDeclaration>; MAX_STEAM_LOBBY_MEMBERS] =
            std::array::from_fn(|_| None);
        for (index, user) in users.iter().copied().enumerate() {
            raw[index] = Some(RawMemberDeclaration {
                user,
                marker: self.backend.member_data(lobby, user, MEMBER_KEY_READY)?,
                seats: self.backend.member_data(lobby, user, MEMBER_KEY_SEATS)?,
                loadout: self.backend.member_data(lobby, user, MEMBER_KEY_LOADOUT)?,
            });
        }

        let mut declarations = [None; MAX_STEAM_LOBBY_MEMBERS];
        for (index, user) in users.iter().copied().enumerate() {
            declarations[index] = old_declarations
                .iter()
                .flatten()
                .find(|cached| cached.user == user)
                .copied()
                .or_else(|| Some(CachedMemberDeclaration::new(user, None)));
        }

        let mut outcomes = [MemberDataOutcome::Staging; MAX_STEAM_LOBBY_MEMBERS];
        for index in 0..users.len() {
            let cached = declarations[index]
                .as_mut()
                .expect("every bounded Steam member has a declaration cache");
            let member_raw = raw[index]
                .as_ref()
                .expect("every bounded Steam member has a raw metadata snapshot");
            outcomes[index] = evaluate_member_declaration(cached, member_raw);
        }

        // Seat-capacity arbitration cannot depend on callback arrival order:
        // every process admits the owner first, then ascending SteamUserId.
        let mut canonical_order = [0_usize; MAX_STEAM_LOBBY_MEMBERS];
        for index in 0..users.len() {
            canonical_order[index] = index;
        }
        canonical_order[..users.len()].sort_by_key(|index| {
            let user = users[*index];
            (user != authority_user, user.get())
        });
        let mut accepted_seats = 0_u16;
        for index in canonical_order[..users.len()].iter().copied() {
            let cached = declarations[index]
                .as_mut()
                .expect("canonical members retain declaration caches");
            if !cached.current_valid {
                continue;
            }
            let seats = cached
                .accepted
                .expect("valid declarations have coherent content")
                .seat_count();
            if accepted_seats + u16::from(seats) <= u16::from(capacity) {
                accepted_seats += u16::from(seats);
                if cached.last_rejection == Some(MemberDeclarationRejection::LobbyCapacityExceeded)
                {
                    cached.last_rejection = None;
                }
            } else {
                outcomes[index] = reject_member_capacity(cached);
            }
        }

        let mut roster = [None; MAX_STEAM_LOBBY_MEMBERS];
        for index in 0..users.len() {
            roster[index] = declarations[index].map(CachedMemberDeclaration::projected_member);
        }
        if let InternalState::InLobby(active) = &mut self.state {
            active.roster = roster;
            active.declarations = declarations;
            active.roster_len = users.len();
        }
        self.reconcile_owner_joinability()?;

        let mut batch = RosterRefreshBatch::default();
        for index in canonical_order[..users.len()].iter().copied() {
            let user = users[index];
            let cached = declarations[index].expect("canonical members retain declaration caches");
            let previous = old_declarations
                .iter()
                .flatten()
                .find(|previous| previous.user == user)
                .copied();
            let projection_changed = previous.is_none_or(|previous| {
                previous.current_valid != cached.current_valid
                    || previous.current_ready != cached.current_ready
                    || (cached.current_valid && previous.accepted != cached.accepted)
                    || previous.invalid != cached.invalid
            });
            if subject == Some(user)
                || matches!(outcomes[index], MemberDataOutcome::Rejected(_))
                || projection_changed
            {
                batch.push(user, outcomes[index]);
            }
        }
        Ok(batch)
    }

    fn push_member_refresh_events(
        &mut self,
        lobby: SteamLobbyId,
        batch: &RosterRefreshBatch,
    ) -> Result<(), SteamPlatformError> {
        for (user, outcome) in batch.iter() {
            self.push_event(SteamPlatformEvent::LobbyMemberDataChanged {
                lobby,
                user,
                outcome,
            })?;
        }
        Ok(())
    }

    pub fn revalidate_active_lobby_metadata(&mut self) -> Result<(), SteamPlatformError> {
        let (lobby, retained) = match &self.state {
            InternalState::InLobby(active) => (active.id, active.metadata.clone()),
            _ => return Err(SteamPlatformError::InvalidState),
        };
        let observed = read_lobby_metadata(&self.backend, lobby)?;
        observed.validate_for_local_client(self.config)?;
        if observed != retained {
            return Err(SteamPlatformError::MetadataMismatch);
        }
        Ok(())
    }

    fn refresh_lobby_flags(&mut self) -> Result<(), SteamPlatformError> {
        let lobby = self.active_lobby_id()?;
        let admission_enabled = read_required_bool(&self.backend, lobby, KEY_ADMISSION)?;
        let effective_joinable = read_required_bool(&self.backend, lobby, KEY_OPEN)?;
        if let InternalState::InLobby(active) = &mut self.state {
            active.admission_enabled = admission_enabled;
            active.effective_joinable = effective_joinable;
        }
        Ok(())
    }

    fn reconcile_owner_joinability(&mut self) -> Result<(), SteamPlatformError> {
        let (lobby, should_join, is_joinable, is_owner) = match &self.state {
            InternalState::InLobby(active) => {
                let every_member_coherent = active.roster_len > 0
                    && active.roster[..active.roster_len].iter().all(|member| {
                        matches!(
                            member,
                            Some(LobbyMember {
                                readiness: MemberReadiness::Declared { .. },
                                loadout: Some(_),
                                ..
                            })
                        )
                    });
                let accepted_seats = active.roster[..active.roster_len]
                    .iter()
                    .flatten()
                    .filter_map(|member| match member.readiness {
                        MemberReadiness::Declared { local_seats, .. } => {
                            Some(u16::from(local_seats))
                        }
                        MemberReadiness::Pending => None,
                    })
                    .sum::<u16>();
                (
                    active.id,
                    active.admission_enabled
                        && active.roster_len < MAX_STEAM_LOBBY_MEMBERS
                        && every_member_coherent
                        && accepted_seats < u16::from(active.metadata.seat_capacity),
                    active.effective_joinable,
                    active.authority_user == self.local_user,
                )
            }
            _ => return Err(SteamPlatformError::InvalidState),
        };
        if !is_owner || should_join == is_joinable {
            return Ok(());
        }
        if should_join {
            self.backend.set_lobby_data(lobby, KEY_OPEN, "1")?;
            self.backend.set_lobby_joinable(lobby, true)?;
        } else {
            self.backend.set_lobby_joinable(lobby, false)?;
            self.backend.set_lobby_data(lobby, KEY_OPEN, "0")?;
        }
        if let InternalState::InLobby(active) = &mut self.state {
            active.effective_joinable = should_join;
        }
        Ok(())
    }

    fn validate_local_declaration_continuity(
        &self,
        declaration: MemberLoadoutDeclaration,
    ) -> Result<(), SteamPlatformError> {
        let InternalState::InLobby(active) = &self.state else {
            return Err(SteamPlatformError::InvalidState);
        };
        let Some(cached) = active
            .declarations
            .iter()
            .flatten()
            .find(|cached| cached.user == self.local_user)
        else {
            return Ok(());
        };
        if cached.invalid && declaration.revision() <= cached.recovery_revision_floor {
            return Err(SteamPlatformError::MetadataMismatch);
        }
        let Some(accepted) = cached.accepted else {
            return Ok(());
        };
        if declaration.revision() < accepted.revision()
            || (declaration.revision() == accepted.revision() && declaration != accepted)
        {
            return Err(SteamPlatformError::MetadataMismatch);
        }
        Ok(())
    }

    fn publish_lobby_metadata(
        &mut self,
        lobby: SteamLobbyId,
        metadata: &LobbyMetadata,
        admission_enabled: bool,
        effective_joinable: bool,
    ) -> Result<(), SteamPlatformError> {
        for (key, value) in metadata.pairs(admission_enabled, effective_joinable) {
            validate_metadata_value(&value)?;
            self.backend.set_lobby_data(lobby, key, &value)?;
        }
        Ok(())
    }

    fn refresh_presence_or_warn(&mut self) -> Result<(), SteamPlatformError> {
        if self.update_rich_presence().is_err() {
            self.backend.clear_rich_presence();
            self.push_event(SteamPlatformEvent::RichPresenceUnavailable)?;
        }
        Ok(())
    }

    fn update_rich_presence(&mut self) -> Result<(), SteamPlatformError> {
        let (lobby, visibility, effective_joinable, roster_len) = match &self.state {
            InternalState::InLobby(active) => (
                active.id,
                active.metadata.visibility,
                active.effective_joinable,
                active.roster_len,
            ),
            _ => return Err(SteamPlatformError::InvalidState),
        };
        self.backend.clear_rich_presence();
        let status = match visibility {
            LobbyVisibility::Private => "In a private lobby",
            LobbyVisibility::FriendsOnly => "In a friends lobby",
            LobbyVisibility::Public => "In an online lobby",
        };
        self.backend.set_rich_presence("status", Some(status))?;
        self.backend
            .set_rich_presence("steam_player_group", Some(&lobby.get().to_string()))?;
        self.backend
            .set_rich_presence("steam_player_group_size", Some(&roster_len.to_string()))?;
        if effective_joinable {
            self.backend
                .set_rich_presence("connect", Some(&connect_lobby_command(lobby)))?;
        }
        Ok(())
    }

    fn queue_launch_intent(
        &mut self,
        command: &str,
        now_ms: u64,
    ) -> Result<(), SteamPlatformError> {
        let Some(lobby) = parse_connect_lobby_command(command)? else {
            return Ok(());
        };
        let intent = LobbyJoinIntent {
            lobby,
            origin: JoinOrigin::LaunchCommand,
            expires_at_ms: deadline(now_ms, self.config.join_intent_ttl_ms)?,
        };
        self.push_event(SteamPlatformEvent::LobbyJoinRequested(intent))
    }

    fn handle_auth_ticket_ready(
        &mut self,
        handle: AuthTicketHandle,
        success: bool,
    ) -> Result<(), SteamPlatformError> {
        let Some(slot) = self
            .issued_tickets
            .iter()
            .position(|ticket| ticket.is_some_and(|ticket| ticket.handle == handle))
        else {
            // Steam may deliver AuthSessionTicketResponse after local
            // cancellation retired the handle. Unknown retired handles carry
            // no live capability and are therefore benign.
            return Ok(());
        };
        let record = self.issued_tickets[slot].expect("slot was just found");
        let was_ready = record.ready;
        if was_ready {
            return if success {
                Ok(())
            } else {
                Err(SteamPlatformError::Backend(
                    SteamBackendError::IntegrityFailure,
                ))
            };
        }
        if success {
            if let Some(ticket) = &mut self.issued_tickets[slot] {
                ticket.ready = true;
            }
            self.push_event(SteamPlatformEvent::AuthTicketReady { handle })
        } else {
            self.backend.cancel_auth_ticket(handle);
            self.issued_tickets[slot] = None;
            self.push_event(SteamPlatformEvent::AuthTicketRejected {
                handle,
                remote_user: record.remote_user,
            })
        }
    }

    fn handle_auth_validation(
        &mut self,
        user: SteamUserId,
        license_owner_user: SteamUserId,
        result: Result<(), AuthValidationFailure>,
    ) -> Result<(), SteamPlatformError> {
        let Some(slot) = self
            .admissions
            .iter()
            .position(|admission| admission.is_some_and(|entry| entry.user == user))
        else {
            return Ok(());
        };
        let lobby = self.admissions[slot].expect("slot was just found").lobby;
        let prior_status = self.admissions[slot].expect("slot was just found").status;
        let rejection = match result {
            Err(error) => Some(PeerAuthenticationRejection::Validation(error)),
            Ok(()) => match self.backend.license_status(user, self.config.app_id)? {
                LicenseStatus::HasLicense => None,
                LicenseStatus::DoesNotHaveLicense => {
                    Some(PeerAuthenticationRejection::DoesNotHaveLicense)
                }
                LicenseStatus::NoAuthentication => {
                    Some(PeerAuthenticationRejection::NoAuthentication)
                }
            },
        };
        if rejection.is_none() {
            match prior_status {
                AdmissionStatus::Waiting => {
                    if let Some(admission) = &mut self.admissions[slot] {
                        admission.status = AdmissionStatus::Approved {
                            license_owner_user,
                            consumed: false,
                        };
                    }
                    self.push_event(SteamPlatformEvent::PeerAuthenticated { lobby, user })
                }
                AdmissionStatus::Approved {
                    license_owner_user: prior_owner,
                    ..
                } if prior_owner == license_owner_user => Ok(()),
                AdmissionStatus::Approved { .. } | AdmissionStatus::Rejected => Err(
                    SteamPlatformError::Backend(SteamBackendError::IntegrityFailure),
                ),
            }
        } else {
            if prior_status == AdmissionStatus::Rejected {
                return Ok(());
            }
            let reason = rejection.expect("the rejected branch has a reason");
            self.backend.end_auth_session(user);
            if let Some(admission) = &mut self.admissions[slot] {
                admission.status = AdmissionStatus::Rejected;
            }
            self.push_event(SteamPlatformEvent::PeerAuthenticationRejected {
                lobby,
                user,
                reason,
            })
        }
    }

    fn expire_auth_intents(&mut self, now_ms: u64) -> Result<(), SteamPlatformError> {
        for index in 0..self.admissions.len() {
            let Some(admission) = self.admissions[index] else {
                continue;
            };
            let expires = matches!(
                admission.status,
                AdmissionStatus::Waiting
                    | AdmissionStatus::Approved {
                        consumed: false,
                        ..
                    }
            );
            if expires && now_ms >= admission.expires_at_ms {
                self.backend.end_auth_session(admission.user);
                self.admissions[index] = None;
                self.push_event(SteamPlatformEvent::PeerAuthenticationRejected {
                    lobby: admission.lobby,
                    user: admission.user,
                    reason: PeerAuthenticationRejection::IntentExpired,
                })?;
            }
        }
        Ok(())
    }

    fn cleanup_peer_authentication(&mut self, user: SteamUserId) {
        if let Some(slot) = self
            .admissions
            .iter()
            .position(|admission| admission.is_some_and(|entry| entry.user == user))
        {
            self.backend.end_auth_session(user);
            self.admissions[slot] = None;
        }
        for index in 0..self.issued_tickets.len() {
            if self.issued_tickets[index].is_some_and(|ticket| ticket.remote_user == user) {
                let ticket = self.issued_tickets[index]
                    .take()
                    .expect("slot was just found");
                self.backend.cancel_auth_ticket(ticket.handle);
            }
        }
    }

    fn cleanup_auth(&mut self) {
        for index in 0..self.issued_tickets.len() {
            if let Some(ticket) = self.issued_tickets[index].take() {
                self.backend.cancel_auth_ticket(ticket.handle);
            }
        }
        for index in 0..self.admissions.len() {
            if let Some(admission) = self.admissions[index].take() {
                self.backend.end_auth_session(admission.user);
            }
        }
    }

    fn teardown_lobby(&mut self, lobby: SteamLobbyId, call_leave: bool) {
        self.cleanup_auth();
        self.backend.clear_rich_presence();
        self.backend.set_callback_lobby_scope(None);
        if call_leave {
            self.backend.leave_lobby(lobby);
        }
    }

    fn push_event(&mut self, event: SteamPlatformEvent) -> Result<(), SteamPlatformError> {
        if self.public_events.len() >= self.config.event_capacity {
            return Err(SteamPlatformError::EventQueueOverflow);
        }
        self.public_events.push_back(event);
        Ok(())
    }

    fn fail_closed<T>(&mut self, error: SteamPlatformError) -> Result<T, SteamPlatformError> {
        if let Ok(lobby) = self.active_lobby_id() {
            self.teardown_lobby(lobby, true);
        } else {
            if let InternalState::Joining(pending) = &self.state {
                self.backend.leave_lobby(pending.intent.lobby);
            }
            self.cleanup_auth();
            self.backend.clear_rich_presence();
            self.backend.set_callback_lobby_scope(None);
        }
        self.public_events.clear();
        self.state = InternalState::Faulted;
        self.last_fault = Some(error);
        Err(error)
    }
}

impl<B: SteamBackend> Drop for SteamPlatform<B> {
    fn drop(&mut self) {
        if let InternalState::InLobby(active) = &self.state {
            let lobby = active.id;
            self.teardown_lobby(lobby, true);
        } else {
            self.cleanup_auth();
            self.backend.clear_rich_presence();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DedicatedSdrSupport {
    /// `steamworks` 0.12.2 exposes the desktop client API used here but this AFC
    /// adapter does not expose the Steam GameServer hosted-address/listen-socket
    /// and game-coordinator ticket flow required for hosted dedicated SDR.
    UnavailableInPinnedBinding,
}

pub fn connect_lobby_command(lobby: SteamLobbyId) -> String {
    format!("+connect_lobby {}", lobby.get())
}

pub fn parse_connect_lobby_command(
    command: &str,
) -> Result<Option<SteamLobbyId>, SteamPlatformError> {
    if command.len() > MAX_CONNECT_COMMAND_BYTES {
        return Err(SteamPlatformError::ConnectCommandTooLong);
    }
    if command.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(SteamPlatformError::InvalidConnectCommand);
    }
    let tokens: Vec<&str> = command.split_ascii_whitespace().collect();
    let mut lobby = None;
    let mut index = 0;
    while index < tokens.len() {
        if tokens[index] == "+connect_lobby" {
            if lobby.is_some() {
                return Err(SteamPlatformError::DuplicateConnectLobby);
            }
            let raw = tokens
                .get(index + 1)
                .ok_or(SteamPlatformError::InvalidConnectCommand)?
                .parse::<u64>()
                .map_err(|_| SteamPlatformError::InvalidConnectCommand)?;
            lobby = Some(SteamLobbyId::new(raw)?);
            index += 2;
        } else {
            index += 1;
        }
    }
    Ok(lobby)
}

fn read_lobby_metadata<B: SteamBackend>(
    backend: &B,
    lobby: SteamLobbyId,
) -> Result<LobbyMetadata, SteamPlatformError> {
    let schema = read_required(backend, lobby, KEY_SCHEMA)?;
    if parse_u16(&schema)? != LOBBY_SCHEMA_VERSION {
        return Err(SteamPlatformError::MetadataMismatch);
    }
    let build = decode_hex::<16>(&read_required(backend, lobby, KEY_BUILD)?)?;
    let content = decode_hex::<32>(&read_required(backend, lobby, KEY_CONTENT)?)?;
    let protocol = crate::network_protocol::ProtocolVersion::new(parse_u16(&read_required(
        backend,
        lobby,
        KEY_PROTOCOL,
    )?)?)
    .map_err(SteamPlatformError::Protocol)?;
    let simulation = crate::network_protocol::SimulationVersion::new(parse_u16(&read_required(
        backend,
        lobby,
        KEY_SIMULATION,
    )?)?)
    .map_err(SteamPlatformError::Protocol)?;
    let replay = crate::network_protocol::ReplayFormatVersion::new(parse_u16(&read_required(
        backend, lobby, KEY_REPLAY,
    )?)?)
    .map_err(SteamPlatformError::Protocol)?;
    let compatibility = CompatibilityId {
        protocol,
        simulation,
        replay,
        build: crate::network_protocol::BuildId::new(build)
            .map_err(SteamPlatformError::Protocol)?,
        gameplay_content: crate::network_protocol::GameplayContentHash::new(content)
            .map_err(SteamPlatformError::Protocol)?,
    };
    let authority = parse_authority(&read_required(backend, lobby, KEY_AUTHORITY)?)?;
    let visibility = parse_visibility(&read_required(backend, lobby, KEY_VISIBILITY)?)?;
    let region = RegionCode::new(read_required(backend, lobby, KEY_REGION)?)?;
    let rules = DefinitionId::new(parse_u16(&read_required(backend, lobby, KEY_RULES)?)?)
        .map_err(SteamPlatformError::Protocol)?;
    let arena = DefinitionId::new(parse_u16(&read_required(backend, lobby, KEY_ARENA)?)?)
        .map_err(SteamPlatformError::Protocol)?;
    let seat_capacity = parse_u8(&read_required(backend, lobby, KEY_SEATS)?)?;
    LobbyMetadata::new(
        compatibility,
        authority,
        visibility,
        region,
        rules,
        arena,
        seat_capacity,
    )
}

fn read_required<B: SteamBackend>(
    backend: &B,
    lobby: SteamLobbyId,
    key: &'static str,
) -> Result<String, SteamPlatformError> {
    let value = backend
        .lobby_data(lobby, key)?
        .filter(|value| !value.is_empty())
        .ok_or(SteamPlatformError::MetadataMissing)?;
    validate_metadata_value(&value)?;
    Ok(value)
}

fn read_required_bool<B: SteamBackend>(
    backend: &B,
    lobby: SteamLobbyId,
    key: &'static str,
) -> Result<bool, SteamPlatformError> {
    parse_bool(&read_required(backend, lobby, key)?)
}

fn read_coherent_member_seat_count<B: SteamBackend>(
    backend: &B,
    lobby: SteamLobbyId,
    user: SteamUserId,
) -> Result<Option<u8>, SteamPlatformError> {
    let Some(marker) = backend
        .member_data(lobby, user, MEMBER_KEY_READY)?
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let Ok(MemberCommitMarker::Committed { revision, .. }) = parse_member_commit_marker(&marker)
    else {
        return Ok(None);
    };
    let Some(seats) = backend
        .member_data(lobby, user, MEMBER_KEY_SEATS)?
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let Some(loadout) = backend
        .member_data(lobby, user, MEMBER_KEY_LOADOUT)?
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let Ok(seats) = parse_u8(&seats) else {
        return Ok(None);
    };
    if validate_local_seats(seats).is_err() {
        return Ok(None);
    }
    let Ok(loadout) = MemberLoadoutDeclaration::new(&loadout) else {
        return Ok(None);
    };
    if loadout.revision() != revision || loadout.seat_count() != seats {
        return Ok(None);
    }
    Ok(Some(seats))
}

fn bounded_members<B: SteamBackend>(
    backend: &B,
    lobby: SteamLobbyId,
) -> Result<Vec<SteamUserId>, SteamPlatformError> {
    let members = backend.lobby_members(lobby)?;
    if members.is_empty() || members.len() > MAX_STEAM_LOBBY_MEMBERS {
        return Err(SteamPlatformError::LobbyCapacityExceeded);
    }
    for (index, member) in members.iter().enumerate() {
        if members[..index].contains(member) {
            return Err(SteamPlatformError::InvalidMetadata);
        }
    }
    Ok(members)
}

fn validate_local_seats(value: u8) -> Result<(), SteamPlatformError> {
    if value == 0 || value > MAX_LOCAL_SEATS {
        Err(SteamPlatformError::InvalidSeatCount)
    } else {
        Ok(())
    }
}

fn encode_member_commit_marker(marker: MemberCommitMarker) -> String {
    match marker {
        MemberCommitMarker::Staging { revision } => format!("s:{revision}"),
        MemberCommitMarker::Committed { revision, ready } => {
            format!("c:{revision}:{}", u8::from(ready))
        }
    }
}

fn parse_member_commit_marker(value: &str) -> Result<MemberCommitMarker, SteamPlatformError> {
    let mut parts = value.split(':');
    let phase = parts.next().ok_or(SteamPlatformError::InvalidMetadata)?;
    let revision = parse_u16(parts.next().ok_or(SteamPlatformError::InvalidMetadata)?)?;
    if revision == 0 {
        return Err(SteamPlatformError::InvalidMetadata);
    }
    match phase {
        "s" if parts.next().is_none() => Ok(MemberCommitMarker::Staging { revision }),
        "c" => {
            let ready = parse_bool(parts.next().ok_or(SteamPlatformError::InvalidMetadata)?)?;
            if parts.next().is_some() {
                return Err(SteamPlatformError::InvalidMetadata);
            }
            Ok(MemberCommitMarker::Committed { revision, ready })
        }
        _ => Err(SteamPlatformError::InvalidMetadata),
    }
}

fn reject_member_declaration(
    cached: &mut CachedMemberDeclaration,
    reason: MemberDeclarationRejection,
    offending_revision: Option<u16>,
) -> MemberDataOutcome {
    cached.current_valid = false;
    cached.current_ready = false;
    cached.provisional_seats = None;
    cached.invalid = true;
    cached.recovery_revision_floor = cached
        .recovery_revision_floor
        .max(
            cached
                .accepted
                .map_or(0, MemberLoadoutDeclaration::revision),
        )
        .max(offending_revision.unwrap_or(0));
    if cached.last_rejection == Some(reason) {
        MemberDataOutcome::Staging
    } else {
        cached.last_rejection = Some(reason);
        MemberDataOutcome::Rejected(reason)
    }
}

fn mark_member_staging(
    cached: &mut CachedMemberDeclaration,
    provisional_seats: Option<u8>,
) -> MemberDataOutcome {
    cached.current_valid = false;
    cached.current_ready = false;
    cached.provisional_seats = provisional_seats;
    MemberDataOutcome::Staging
}

fn reject_member_capacity(cached: &mut CachedMemberDeclaration) -> MemberDataOutcome {
    cached.current_valid = false;
    cached.current_ready = false;
    cached.provisional_seats = None;
    if cached.last_rejection == Some(MemberDeclarationRejection::LobbyCapacityExceeded) {
        MemberDataOutcome::Staging
    } else {
        cached.last_rejection = Some(MemberDeclarationRejection::LobbyCapacityExceeded);
        MemberDataOutcome::Rejected(MemberDeclarationRejection::LobbyCapacityExceeded)
    }
}

fn evaluate_member_declaration(
    cached: &mut CachedMemberDeclaration,
    raw: &RawMemberDeclaration,
) -> MemberDataOutcome {
    debug_assert_eq!(cached.user, raw.user);
    let marker_text = raw.marker.as_deref().filter(|value| !value.is_empty());
    let Some(marker_text) = marker_text else {
        return mark_member_staging(cached, parse_provisional_seats(raw.seats.as_deref()));
    };
    let marker = match parse_member_commit_marker(marker_text) {
        Ok(marker) => marker,
        Err(_) => {
            return reject_member_declaration(cached, MemberDeclarationRejection::Malformed, None);
        }
    };
    let marker_revision = match marker {
        MemberCommitMarker::Staging { revision }
        | MemberCommitMarker::Committed { revision, .. } => revision,
    };
    if matches!(marker, MemberCommitMarker::Staging { .. }) {
        return mark_member_staging(cached, parse_provisional_seats(raw.seats.as_deref()));
    }

    let seats_text = raw.seats.as_deref().filter(|value| !value.is_empty());
    let loadout_text = raw.loadout.as_deref().filter(|value| !value.is_empty());
    let (Some(seats_text), Some(loadout_text)) = (seats_text, loadout_text) else {
        return mark_member_staging(cached, parse_provisional_seats(raw.seats.as_deref()));
    };
    let local_seats = match parse_u8(seats_text).and_then(|seats| {
        validate_local_seats(seats)?;
        Ok(seats)
    }) {
        Ok(seats) => seats,
        Err(_) => {
            return reject_member_declaration(
                cached,
                MemberDeclarationRejection::Malformed,
                Some(marker_revision),
            );
        }
    };
    let declaration = match MemberLoadoutDeclaration::new(loadout_text) {
        Ok(declaration) => declaration,
        Err(_) => {
            return reject_member_declaration(
                cached,
                MemberDeclarationRejection::Malformed,
                Some(marker_revision),
            );
        }
    };
    if declaration.revision() != marker_revision || declaration.seat_count() != local_seats {
        return mark_member_staging(cached, Some(local_seats));
    }
    let MemberCommitMarker::Committed { ready, .. } = marker else {
        unreachable!("the staging marker returned above")
    };

    if cached.invalid && marker_revision <= cached.recovery_revision_floor {
        return reject_member_declaration(
            cached,
            MemberDeclarationRejection::RevisionRegression,
            Some(marker_revision),
        );
    }
    if let Some(accepted) = cached.accepted {
        if marker_revision < accepted.revision() {
            return reject_member_declaration(
                cached,
                MemberDeclarationRejection::RevisionRegression,
                Some(marker_revision),
            );
        }
        if marker_revision == accepted.revision() && declaration != accepted {
            return reject_member_declaration(
                cached,
                MemberDeclarationRejection::RevisionConflict,
                Some(marker_revision),
            );
        }
    }
    cached.accepted = Some(declaration);
    cached.current_valid = true;
    cached.current_ready = ready;
    cached.provisional_seats = None;
    cached.invalid = false;
    cached.recovery_revision_floor = marker_revision;
    if cached.last_rejection != Some(MemberDeclarationRejection::LobbyCapacityExceeded) {
        cached.last_rejection = None;
    }
    MemberDataOutcome::Accepted
}

fn parse_provisional_seats(value: Option<&str>) -> Option<u8> {
    let value = value?;
    if value.is_empty() {
        return None;
    }
    let seats = parse_u8(value).ok()?;
    validate_local_seats(seats).ok()?;
    Some(seats)
}

fn validate_metadata_value(value: &str) -> Result<(), SteamPlatformError> {
    if value.is_empty() || value.len() > 128 || value.bytes().any(|byte| byte == 0) {
        Err(SteamPlatformError::InvalidMetadata)
    } else {
        Ok(())
    }
}

fn deadline(now_ms: u64, ttl_ms: u64) -> Result<u64, SteamPlatformError> {
    now_ms
        .checked_add(ttl_ms)
        .ok_or(SteamPlatformError::InvalidTimeout)
}

const fn bool_text(value: bool) -> &'static str {
    if value { "1" } else { "0" }
}

fn parse_bool(value: &str) -> Result<bool, SteamPlatformError> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(SteamPlatformError::InvalidMetadata),
    }
}

fn parse_u8(value: &str) -> Result<u8, SteamPlatformError> {
    if value.is_empty() || value.len() > 3 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(SteamPlatformError::InvalidMetadata);
    }
    value
        .parse()
        .map_err(|_| SteamPlatformError::InvalidMetadata)
}

fn parse_u16(value: &str) -> Result<u16, SteamPlatformError> {
    if value.is_empty() || value.len() > 5 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(SteamPlatformError::InvalidMetadata);
    }
    value
        .parse()
        .map_err(|_| SteamPlatformError::InvalidMetadata)
}

const fn authority_text(value: AuthorityKind) -> &'static str {
    match value {
        AuthorityKind::Offline => "offline",
        AuthorityKind::Listen => "listen",
        AuthorityKind::Dedicated => "dedicated",
    }
}

fn parse_authority(value: &str) -> Result<AuthorityKind, SteamPlatformError> {
    match value {
        "listen" => Ok(AuthorityKind::Listen),
        "dedicated" => Ok(AuthorityKind::Dedicated),
        _ => Err(SteamPlatformError::InvalidAuthority),
    }
}

const fn visibility_text(value: LobbyVisibility) -> &'static str {
    match value {
        LobbyVisibility::Private => "private",
        LobbyVisibility::FriendsOnly => "friends",
        LobbyVisibility::Public => "public",
    }
}

fn parse_visibility(value: &str) -> Result<LobbyVisibility, SteamPlatformError> {
    match value {
        "private" => Ok(LobbyVisibility::Private),
        "friends" => Ok(LobbyVisibility::FriendsOnly),
        "public" => Ok(LobbyVisibility::Public),
        _ => Err(SteamPlatformError::InvalidMetadata),
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[usize::from(byte >> 4)] as char);
        output.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    output
}

fn decode_hex<const N: usize>(value: &str) -> Result<[u8; N], SteamPlatformError> {
    if value.len() != N * 2 {
        return Err(SteamPlatformError::InvalidMetadata);
    }
    let mut output = [0_u8; N];
    for (index, byte) in output.iter_mut().enumerate() {
        let high = decode_nibble(value.as_bytes()[index * 2])?;
        let low = decode_nibble(value.as_bytes()[index * 2 + 1])?;
        *byte = (high << 4) | low;
    }
    Ok(output)
}

fn decode_nibble(value: u8) -> Result<u8, SteamPlatformError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(SteamPlatformError::InvalidMetadata),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FakeLobbyMemberSeed {
    pub user: SteamUserId,
    pub readiness: Option<MemberReadiness>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FakeAuthOutcome {
    pub license_owner_user: SteamUserId,
    pub validation: Result<(), AuthValidationFailure>,
    pub license: LicenseStatus,
}

impl FakeAuthOutcome {
    pub const fn accepted(user: SteamUserId) -> Self {
        Self {
            license_owner_user: user,
            validation: Ok(()),
            license: LicenseStatus::HasLicense,
        }
    }
}

fn fake_seed_member_declaration(seats: u8) -> Result<MemberLoadoutDeclaration, SteamBackendError> {
    let mut encoded = format!("010100{seats:02x}");
    for index in 0..seats {
        encoded.push_str(&format!("{index:02x}000000000000"));
    }
    MemberLoadoutDeclaration::new(&encoded).map_err(|_| SteamBackendError::InvalidData)
}

#[derive(Clone, Debug)]
struct FakeLobby {
    owner: SteamUserId,
    maximum_peers: u8,
    joinable: bool,
    members: Vec<SteamUserId>,
    data: BTreeMap<&'static str, String>,
    member_data: BTreeMap<(SteamUserId, &'static str), String>,
}

#[derive(Debug)]
struct FakeSteamState {
    app_id: SteamAppId,
    local_user: SteamUserId,
    next_lobby_id: u64,
    next_ticket_handle: u32,
    launch_command: String,
    events: VecDeque<SteamBackendEvent>,
    callback_overflow: bool,
    lobbies: BTreeMap<SteamLobbyId, FakeLobby>,
    lobby_data_read_counts: BTreeMap<(SteamLobbyId, &'static str), u32>,
    fail_lobby_data_read: Option<(SteamLobbyId, &'static str, u32)>,
    friends: BTreeSet<SteamUserId>,
    rich_presence: BTreeMap<&'static str, String>,
    issued_tickets: BTreeMap<AuthTicketHandle, SteamUserId>,
    cancelled_tickets: BTreeSet<AuthTicketHandle>,
    active_auth_sessions: BTreeSet<SteamUserId>,
    ended_auth_sessions: BTreeSet<SteamUserId>,
    auth_outcomes: BTreeMap<SteamUserId, FakeAuthOutcome>,
    overlay_enabled: bool,
    overlay_active: bool,
    overlay_enabled_query_count: u32,
    callback_pump_count: u32,
    invite_overlay_open_count: u32,
    steam_input_snapshot: SteamInputSnapshot,
    steam_input_action_set: SteamInputActionSet,
    steam_input_binding_panel_open_count: u32,
    last_steam_input_binding_ordinal: Option<usize>,
}

impl FakeSteamState {
    fn push_event(&mut self, event: SteamBackendEvent) {
        if self.events.len() >= MAX_STEAM_EVENTS {
            self.callback_overflow = true;
        } else {
            self.events.push_back(event);
        }
    }
}

/// Injection/inspection handle for deterministic tests. It cannot pump
/// callbacks, so the associated [`SteamPlatform`] remains the only pump owner.
#[derive(Clone)]
pub struct FakeSteamControl {
    shared: Arc<Mutex<FakeSteamState>>,
}

impl FakeSteamControl {
    /// Copies one fake lobby into another independent fake backend. This is a
    /// test-only process-fabric seam: the source lock is released before the
    /// target lock is acquired, so two simulated Steam clients never share a
    /// backend or participate in a lock-order cycle.
    #[cfg(test)]
    pub(crate) fn mirror_lobby_shell_to(
        &self,
        target: &FakeSteamControl,
        lobby: SteamLobbyId,
    ) -> Result<(), SteamBackendError> {
        let snapshot = {
            let source = self
                .shared
                .lock()
                .map_err(|_| SteamBackendError::IntegrityFailure)?;
            source
                .lobbies
                .get(&lobby)
                .cloned()
                .ok_or(SteamBackendError::InvalidData)?
        };
        let mut target = target
            .shared
            .lock()
            .map_err(|_| SteamBackendError::IntegrityFailure)?;
        target.lobbies.insert(lobby, snapshot);
        Ok(())
    }

    /// Mirrors only owner-authored lobby state. Member declarations remain
    /// independently authored by their corresponding fake process.
    #[cfg(test)]
    pub(crate) fn mirror_lobby_owner_state_to(
        &self,
        target: &FakeSteamControl,
        lobby: SteamLobbyId,
    ) -> Result<(), SteamBackendError> {
        let (owner, maximum_peers, joinable, data) = {
            let source = self
                .shared
                .lock()
                .map_err(|_| SteamBackendError::IntegrityFailure)?;
            let lobby = source
                .lobbies
                .get(&lobby)
                .ok_or(SteamBackendError::InvalidData)?;
            (
                lobby.owner,
                lobby.maximum_peers,
                lobby.joinable,
                lobby.data.clone(),
            )
        };
        let mut target = target
            .shared
            .lock()
            .map_err(|_| SteamBackendError::IntegrityFailure)?;
        let record = target
            .lobbies
            .get_mut(&lobby)
            .ok_or(SteamBackendError::InvalidData)?;
        let changed = record.owner != owner
            || record.maximum_peers != maximum_peers
            || record.joinable != joinable
            || record.data != data;
        record.owner = owner;
        record.maximum_peers = maximum_peers;
        record.joinable = joinable;
        record.data = data;
        if changed {
            target.push_event(SteamBackendEvent::LobbyDataChanged {
                lobby,
                subject: LobbyDataSubject::Lobby,
            });
        }
        Ok(())
    }

    /// Mirrors one user's presence and declaration into another independent
    /// fake backend and emits the same bounded callbacks the target process
    /// would receive from Steam.
    #[cfg(test)]
    pub(crate) fn mirror_lobby_member_to(
        &self,
        target: &FakeSteamControl,
        lobby: SteamLobbyId,
        user: SteamUserId,
    ) -> Result<(), SteamBackendError> {
        let (present, member_data) = {
            let source = self
                .shared
                .lock()
                .map_err(|_| SteamBackendError::IntegrityFailure)?;
            let lobby = source
                .lobbies
                .get(&lobby)
                .ok_or(SteamBackendError::InvalidData)?;
            (
                lobby.members.contains(&user),
                lobby
                    .member_data
                    .iter()
                    .filter_map(|((candidate, key), value)| {
                        (*candidate == user).then_some((*key, value.clone()))
                    })
                    .collect::<Vec<_>>(),
            )
        };
        let mut target = target
            .shared
            .lock()
            .map_err(|_| SteamBackendError::IntegrityFailure)?;
        let record = target
            .lobbies
            .get_mut(&lobby)
            .ok_or(SteamBackendError::InvalidData)?;
        let was_present = record.members.contains(&user);
        let prior_member_data = record
            .member_data
            .iter()
            .filter_map(|((candidate, key), value)| {
                (*candidate == user).then_some((*key, value.clone()))
            })
            .collect::<Vec<_>>();
        let data_changed = prior_member_data != member_data;
        if present && !was_present {
            if record.members.len() >= usize::from(record.maximum_peers) {
                return Err(SteamBackendError::CapacityExceeded);
            }
            record.members.push(user);
        } else if !present && was_present {
            record.members.retain(|candidate| *candidate != user);
        }
        record
            .member_data
            .retain(|(candidate, _), _| *candidate != user);
        if present {
            for (key, value) in member_data {
                record.member_data.insert((user, key), value);
            }
        }
        if present != was_present {
            target.push_event(SteamBackendEvent::LobbyMembershipChanged {
                lobby,
                user,
                change: if present {
                    LobbyMembershipChange::Entered
                } else {
                    LobbyMembershipChange::Left
                },
            });
        }
        if present && data_changed {
            target.push_event(SteamBackendEvent::LobbyDataChanged {
                lobby,
                subject: LobbyDataSubject::Member(user),
            });
        }
        Ok(())
    }

    pub fn set_overlay_enabled(&self, enabled: bool) -> Result<(), SteamBackendError> {
        let mut state = self
            .shared
            .lock()
            .map_err(|_| SteamBackendError::IntegrityFailure)?;
        state.overlay_enabled = enabled;
        Ok(())
    }

    /// Coalesces activity to the latest callback value, matching the native
    /// backend's `GameOverlayActivated` storage.
    pub fn set_overlay_active(&self, active: bool) -> Result<(), SteamBackendError> {
        let mut state = self
            .shared
            .lock()
            .map_err(|_| SteamBackendError::IntegrityFailure)?;
        state.overlay_active = active;
        Ok(())
    }

    pub fn overlay_enabled_query_count(&self) -> u32 {
        self.shared
            .lock()
            .map(|state| state.overlay_enabled_query_count)
            .unwrap_or(0)
    }

    pub fn callback_pump_count(&self) -> u32 {
        self.shared
            .lock()
            .map(|state| state.callback_pump_count)
            .unwrap_or(0)
    }

    /// Injects one complete action-level controller sample. This deterministic
    /// hook intentionally accepts no Steam action handles, matching the public
    /// platform boundary used by the application.
    pub fn set_steam_input_controller(
        &self,
        local_ordinal: usize,
        controller: Option<SteamInputControllerSnapshot>,
    ) -> Result<(), SteamBackendError> {
        if local_ordinal >= MAX_STEAM_INPUT_CONTROLLERS
            || controller.is_some_and(|controller| !controller.connected())
        {
            return Err(SteamBackendError::InvalidData);
        }
        let mut state = self
            .shared
            .lock()
            .map_err(|_| SteamBackendError::IntegrityFailure)?;
        state.steam_input_snapshot.controllers[local_ordinal] = controller.unwrap_or_default();
        Ok(())
    }

    pub fn steam_input_binding_panel_open_count(&self) -> u32 {
        self.shared
            .lock()
            .map(|state| state.steam_input_binding_panel_open_count)
            .unwrap_or(0)
    }

    pub fn last_steam_input_binding_ordinal(&self) -> Option<usize> {
        self.shared
            .lock()
            .ok()
            .and_then(|state| state.last_steam_input_binding_ordinal)
    }

    pub fn steam_input_action_set(&self) -> Option<SteamInputActionSet> {
        self.shared
            .lock()
            .ok()
            .map(|state| state.steam_input_action_set)
    }

    pub fn seed_lobby(
        &self,
        lobby: SteamLobbyId,
        metadata: &LobbyMetadata,
        accepting: bool,
        owner: SteamUserId,
        members: &[FakeLobbyMemberSeed],
    ) -> Result<(), SteamBackendError> {
        if members.is_empty() || members.len() > MAX_STEAM_LOBBY_MEMBERS {
            return Err(SteamBackendError::CapacityExceeded);
        }
        if !members.iter().any(|member| member.user == owner) {
            return Err(SteamBackendError::InvalidData);
        }
        for (index, member) in members.iter().enumerate() {
            if members[..index]
                .iter()
                .any(|prior| prior.user == member.user)
            {
                return Err(SteamBackendError::InvalidData);
            }
        }
        let mut data = BTreeMap::new();
        for (key, value) in metadata.pairs(accepting, accepting) {
            data.insert(key, value);
        }
        let mut member_data = BTreeMap::new();
        for member in members {
            if let Some(MemberReadiness::Declared { ready, local_seats }) = member.readiness {
                validate_local_seats(local_seats).map_err(|_| SteamBackendError::InvalidData)?;
                let marker = encode_member_commit_marker(MemberCommitMarker::Committed {
                    revision: 1,
                    ready,
                });
                let declaration = fake_seed_member_declaration(local_seats)?;
                member_data.insert((member.user, MEMBER_KEY_READY), marker);
                member_data.insert((member.user, MEMBER_KEY_SEATS), local_seats.to_string());
                member_data.insert(
                    (member.user, MEMBER_KEY_LOADOUT),
                    declaration.as_str().to_owned(),
                );
            }
        }
        let mut state = self
            .shared
            .lock()
            .map_err(|_| SteamBackendError::IntegrityFailure)?;
        if state.lobbies.len() >= MAX_STEAM_EVENTS && !state.lobbies.contains_key(&lobby) {
            return Err(SteamBackendError::CapacityExceeded);
        }
        state.lobbies.insert(
            lobby,
            FakeLobby {
                owner,
                maximum_peers: MAX_STEAM_LOBBY_MEMBERS as u8,
                joinable: accepting,
                members: members.iter().map(|member| member.user).collect(),
                data,
                member_data,
            },
        );
        Ok(())
    }

    pub fn set_friend(&self, user: SteamUserId, is_friend: bool) -> Result<(), SteamBackendError> {
        let mut state = self
            .shared
            .lock()
            .map_err(|_| SteamBackendError::IntegrityFailure)?;
        if is_friend {
            if state.friends.len() >= MAX_STEAM_LOBBY_MEMBERS && !state.friends.contains(&user) {
                return Err(SteamBackendError::CapacityExceeded);
            }
            state.friends.insert(user);
        } else {
            state.friends.remove(&user);
        }
        Ok(())
    }

    pub fn set_member_readiness(
        &self,
        lobby: SteamLobbyId,
        user: SteamUserId,
        readiness: MemberReadiness,
    ) -> Result<(), SteamBackendError> {
        let mut state = self
            .shared
            .lock()
            .map_err(|_| SteamBackendError::IntegrityFailure)?;
        let record = state
            .lobbies
            .get_mut(&lobby)
            .ok_or(SteamBackendError::InvalidData)?;
        if !record.members.contains(&user) {
            return Err(SteamBackendError::InvalidData);
        }
        record
            .member_data
            .retain(|(member, key), _| *member != user || *key == MEMBER_KEY_LOADOUT);
        if let MemberReadiness::Declared { ready, local_seats } = readiness {
            validate_local_seats(local_seats).map_err(|_| SteamBackendError::InvalidData)?;
            let revision = record
                .member_data
                .get(&(user, MEMBER_KEY_LOADOUT))
                .and_then(|encoded| MemberLoadoutDeclaration::new(encoded).ok())
                .map_or(1, MemberLoadoutDeclaration::revision);
            record.member_data.insert(
                (user, MEMBER_KEY_READY),
                encode_member_commit_marker(MemberCommitMarker::Committed { revision, ready }),
            );
            record
                .member_data
                .insert((user, MEMBER_KEY_SEATS), local_seats.to_string());
        }
        Ok(())
    }

    pub fn set_member_loadout(
        &self,
        lobby: SteamLobbyId,
        user: SteamUserId,
        declaration: MemberLoadoutDeclaration,
    ) -> Result<(), SteamBackendError> {
        let mut state = self
            .shared
            .lock()
            .map_err(|_| SteamBackendError::IntegrityFailure)?;
        let record = state
            .lobbies
            .get_mut(&lobby)
            .ok_or(SteamBackendError::InvalidData)?;
        if !record.members.contains(&user) {
            return Err(SteamBackendError::InvalidData);
        }
        record
            .member_data
            .insert((user, MEMBER_KEY_LOADOUT), declaration.as_str().to_owned());
        Ok(())
    }

    pub fn set_lobby_data_raw(
        &self,
        lobby: SteamLobbyId,
        key: &'static str,
        value: &str,
    ) -> Result<(), SteamBackendError> {
        let mut state = self
            .shared
            .lock()
            .map_err(|_| SteamBackendError::IntegrityFailure)?;
        let record = state
            .lobbies
            .get_mut(&lobby)
            .ok_or(SteamBackendError::InvalidData)?;
        if record.data.len() >= MAX_LOBBY_METADATA_PAIRS && !record.data.contains_key(key) {
            return Err(SteamBackendError::CapacityExceeded);
        }
        record.data.insert(key, value.to_owned());
        Ok(())
    }

    pub fn fail_lobby_data_read_on_occurrence(
        &self,
        lobby: SteamLobbyId,
        key: &'static str,
        occurrence: u32,
    ) -> Result<(), SteamBackendError> {
        if occurrence == 0 {
            return Err(SteamBackendError::InvalidData);
        }
        let mut state = self
            .shared
            .lock()
            .map_err(|_| SteamBackendError::IntegrityFailure)?;
        if !state.lobbies.contains_key(&lobby) {
            return Err(SteamBackendError::InvalidData);
        }
        state.lobby_data_read_counts.insert((lobby, key), 0);
        state.fail_lobby_data_read = Some((lobby, key, occurrence));
        Ok(())
    }

    pub fn set_member_data_raw(
        &self,
        lobby: SteamLobbyId,
        user: SteamUserId,
        key: &'static str,
        value: &str,
    ) -> Result<(), SteamBackendError> {
        let mut state = self
            .shared
            .lock()
            .map_err(|_| SteamBackendError::IntegrityFailure)?;
        let record = state
            .lobbies
            .get_mut(&lobby)
            .ok_or(SteamBackendError::InvalidData)?;
        if !record.members.contains(&user) {
            return Err(SteamBackendError::InvalidData);
        }
        let pair_count = record
            .member_data
            .keys()
            .filter(|(member, _)| *member == user)
            .count();
        if pair_count >= MAX_MEMBER_METADATA_PAIRS && !record.member_data.contains_key(&(user, key))
        {
            return Err(SteamBackendError::CapacityExceeded);
        }
        record.member_data.insert((user, key), value.to_owned());
        Ok(())
    }

    pub fn member_data_raw(
        &self,
        lobby: SteamLobbyId,
        user: SteamUserId,
        key: &'static str,
    ) -> Option<String> {
        self.shared.lock().ok().and_then(|state| {
            state
                .lobbies
                .get(&lobby)
                .and_then(|record| record.member_data.get(&(user, key)).cloned())
        })
    }

    pub fn lobby_is_joinable(&self, lobby: SteamLobbyId) -> Option<bool> {
        self.shared
            .lock()
            .ok()
            .and_then(|state| state.lobbies.get(&lobby).map(|record| record.joinable))
    }

    pub fn set_launch_command(&self, command: &str) -> Result<(), SteamBackendError> {
        if command.len() > 256 {
            return Err(SteamBackendError::InvalidData);
        }
        let mut state = self
            .shared
            .lock()
            .map_err(|_| SteamBackendError::IntegrityFailure)?;
        state.launch_command = command.to_owned();
        Ok(())
    }

    pub fn set_auth_outcome(
        &self,
        user: SteamUserId,
        outcome: FakeAuthOutcome,
    ) -> Result<(), SteamBackendError> {
        let mut state = self
            .shared
            .lock()
            .map_err(|_| SteamBackendError::IntegrityFailure)?;
        if state.auth_outcomes.len() >= MAX_STEAM_LOBBY_MEMBERS
            && !state.auth_outcomes.contains_key(&user)
        {
            return Err(SteamBackendError::CapacityExceeded);
        }
        state.auth_outcomes.insert(user, outcome);
        Ok(())
    }

    pub fn emit_join_request(
        &self,
        lobby: SteamLobbyId,
        friend: Option<SteamUserId>,
    ) -> Result<(), SteamBackendError> {
        self.emit(SteamBackendEvent::LobbyJoinRequested { lobby, friend })
    }

    pub fn emit_rich_presence_join(
        &self,
        friend: Option<SteamUserId>,
        connect: &str,
    ) -> Result<(), SteamBackendError> {
        if connect.len() > MAX_CONNECT_COMMAND_BYTES {
            return Err(SteamBackendError::InvalidData);
        }
        self.emit(SteamBackendEvent::RichPresenceJoinRequested {
            friend,
            connect: connect.to_owned(),
        })
    }

    pub fn emit_launch_parameters_changed(&self) -> Result<(), SteamBackendError> {
        self.emit(SteamBackendEvent::LaunchParametersChanged)
    }

    pub fn emit_membership_change(
        &self,
        lobby: SteamLobbyId,
        user: SteamUserId,
        change: LobbyMembershipChange,
    ) -> Result<(), SteamBackendError> {
        {
            let mut state = self
                .shared
                .lock()
                .map_err(|_| SteamBackendError::IntegrityFailure)?;
            if let Some(record) = state.lobbies.get_mut(&lobby) {
                match change {
                    LobbyMembershipChange::Entered => {
                        if record.members.len() < usize::from(record.maximum_peers)
                            && !record.members.contains(&user)
                        {
                            record.members.push(user);
                        }
                    }
                    LobbyMembershipChange::Left
                    | LobbyMembershipChange::Disconnected
                    | LobbyMembershipChange::Kicked
                    | LobbyMembershipChange::Banned => {
                        record.members.retain(|member| *member != user);
                        record.member_data.retain(|(member, _), _| *member != user);
                        if record.owner == user {
                            record.owner = *record
                                .members
                                .first()
                                .ok_or(SteamBackendError::InvalidData)?;
                        }
                    }
                }
            }
            state.push_event(SteamBackendEvent::LobbyMembershipChanged {
                lobby,
                user,
                change,
            });
        }
        Ok(())
    }

    pub fn emit_lobby_data_changed(&self, lobby: SteamLobbyId) -> Result<(), SteamBackendError> {
        self.emit(SteamBackendEvent::LobbyDataChanged {
            lobby,
            subject: LobbyDataSubject::Lobby,
        })
    }

    pub fn emit_member_data_changed(
        &self,
        lobby: SteamLobbyId,
        user: SteamUserId,
    ) -> Result<(), SteamBackendError> {
        self.emit(SteamBackendEvent::LobbyDataChanged {
            lobby,
            subject: LobbyDataSubject::Member(user),
        })
    }

    pub fn emit_auth_validation(
        &self,
        user: SteamUserId,
        license_owner_user: SteamUserId,
        result: Result<(), AuthValidationFailure>,
    ) -> Result<(), SteamBackendError> {
        self.emit(SteamBackendEvent::AuthSessionValidated {
            user,
            license_owner_user,
            result,
        })
    }

    pub fn set_queued_auth_ticket_result(
        &self,
        handle: AuthTicketHandle,
        success: bool,
    ) -> Result<(), SteamBackendError> {
        let mut state = self
            .shared
            .lock()
            .map_err(|_| SteamBackendError::IntegrityFailure)?;
        let event = state
            .events
            .iter_mut()
            .find(|event| {
                matches!(
                    event,
                    SteamBackendEvent::AuthTicketReady {
                        handle: queued,
                        ..
                    } if *queued == handle
                )
            })
            .ok_or(SteamBackendError::InvalidData)?;
        *event = SteamBackendEvent::AuthTicketReady { handle, success };
        Ok(())
    }

    pub fn set_queued_lobby_join_result(
        &self,
        operation_id: SteamOperationId,
        result: Result<SteamLobbyId, SteamBackendError>,
    ) -> Result<(), SteamBackendError> {
        let mut state = self
            .shared
            .lock()
            .map_err(|_| SteamBackendError::IntegrityFailure)?;
        let event = state
            .events
            .iter_mut()
            .find(|event| {
                matches!(
                    event,
                    SteamBackendEvent::LobbyJoined {
                        operation_id: queued,
                        ..
                    } if *queued == operation_id
                )
            })
            .ok_or(SteamBackendError::InvalidData)?;
        let requested = match *event {
            SteamBackendEvent::LobbyJoined { requested, .. } => requested,
            _ => unreachable!("the matching event was just selected"),
        };
        *event = SteamBackendEvent::LobbyJoined {
            operation_id,
            requested,
            result,
        };
        Ok(())
    }

    pub fn set_queued_lobby_join_callback(
        &self,
        operation_id: SteamOperationId,
        requested: SteamLobbyId,
        result: Result<SteamLobbyId, SteamBackendError>,
    ) -> Result<(), SteamBackendError> {
        let mut state = self
            .shared
            .lock()
            .map_err(|_| SteamBackendError::IntegrityFailure)?;
        let event = state
            .events
            .iter_mut()
            .find(|event| {
                matches!(
                    event,
                    SteamBackendEvent::LobbyJoined {
                        operation_id: queued,
                        ..
                    } if *queued == operation_id
                )
            })
            .ok_or(SteamBackendError::InvalidData)?;
        *event = SteamBackendEvent::LobbyJoined {
            operation_id,
            requested,
            result,
        };
        Ok(())
    }

    pub fn emit_disconnect(&self) -> Result<(), SteamBackendError> {
        self.emit(SteamBackendEvent::SteamDisconnected)
    }

    pub fn cancelled_ticket(&self, handle: AuthTicketHandle) -> bool {
        self.shared
            .lock()
            .map(|state| state.cancelled_tickets.contains(&handle))
            .unwrap_or(false)
    }

    pub fn ended_auth_session(&self, user: SteamUserId) -> bool {
        self.shared
            .lock()
            .map(|state| state.ended_auth_sessions.contains(&user))
            .unwrap_or(false)
    }

    pub fn lobby_contains_member(&self, lobby: SteamLobbyId, user: SteamUserId) -> bool {
        self.shared
            .lock()
            .ok()
            .and_then(|state| {
                state
                    .lobbies
                    .get(&lobby)
                    .map(|record| record.members.contains(&user))
            })
            .unwrap_or(false)
    }

    pub fn rich_presence(&self, key: &'static str) -> Option<String> {
        self.shared
            .lock()
            .ok()
            .and_then(|state| state.rich_presence.get(key).cloned())
    }

    pub fn invite_overlay_open_count(&self) -> u32 {
        self.shared
            .lock()
            .map(|state| state.invite_overlay_open_count)
            .unwrap_or(0)
    }

    fn emit(&self, event: SteamBackendEvent) -> Result<(), SteamBackendError> {
        let mut state = self
            .shared
            .lock()
            .map_err(|_| SteamBackendError::IntegrityFailure)?;
        state.push_event(event);
        Ok(())
    }
}

/// Deterministic, Steam-free backend used by ordinary CI and platform tests.
pub struct FakeSteamBackend {
    shared: Arc<Mutex<FakeSteamState>>,
}

impl FakeSteamBackend {
    pub fn new(app_id: SteamAppId, local_user: SteamUserId) -> (Self, FakeSteamControl) {
        let shared = Arc::new(Mutex::new(FakeSteamState {
            app_id,
            local_user,
            next_lobby_id: 10_000,
            next_ticket_handle: 1,
            launch_command: String::new(),
            events: VecDeque::with_capacity(MAX_STEAM_EVENTS),
            callback_overflow: false,
            lobbies: BTreeMap::new(),
            lobby_data_read_counts: BTreeMap::new(),
            fail_lobby_data_read: None,
            friends: BTreeSet::new(),
            rich_presence: BTreeMap::new(),
            issued_tickets: BTreeMap::new(),
            cancelled_tickets: BTreeSet::new(),
            active_auth_sessions: BTreeSet::new(),
            ended_auth_sessions: BTreeSet::new(),
            auth_outcomes: BTreeMap::new(),
            overlay_enabled: false,
            overlay_active: false,
            overlay_enabled_query_count: 0,
            callback_pump_count: 0,
            invite_overlay_open_count: 0,
            steam_input_snapshot: SteamInputSnapshot::default(),
            steam_input_action_set: SteamInputActionSet::default(),
            steam_input_binding_panel_open_count: 0,
            last_steam_input_binding_ordinal: None,
        }));
        (
            Self {
                shared: shared.clone(),
            },
            FakeSteamControl { shared },
        )
    }

    fn with_state<T>(
        &self,
        action: impl FnOnce(&FakeSteamState) -> Result<T, SteamBackendError>,
    ) -> Result<T, SteamBackendError> {
        let state = self
            .shared
            .lock()
            .map_err(|_| SteamBackendError::IntegrityFailure)?;
        action(&state)
    }

    fn with_state_mut<T>(
        &self,
        action: impl FnOnce(&mut FakeSteamState) -> Result<T, SteamBackendError>,
    ) -> Result<T, SteamBackendError> {
        let mut state = self
            .shared
            .lock()
            .map_err(|_| SteamBackendError::IntegrityFailure)?;
        action(&mut state)
    }
}

impl SteamBackend for FakeSteamBackend {
    fn configured_app_id(&self) -> Result<SteamAppId, SteamBackendError> {
        self.with_state(|state| Ok(state.app_id))
    }

    fn local_user(&self) -> Result<SteamUserId, SteamBackendError> {
        self.with_state(|state| Ok(state.local_user))
    }

    fn pump_callbacks(&mut self) -> Result<(), SteamBackendError> {
        self.with_state_mut(|state| {
            state.callback_pump_count = state.callback_pump_count.saturating_add(1);
            Ok(())
        })
    }

    fn poll_event(&mut self) -> Option<SteamBackendEvent> {
        let mut state = self.shared.lock().ok()?;
        loop {
            let event = state.events.pop_front()?;
            if let SteamBackendEvent::AuthTicketReady { handle, .. } = &event
                && !state.issued_tickets.contains_key(handle)
            {
                // Mirror the native callback translator: a canceled ticket's
                // queued response is retired before it reaches the platform.
                continue;
            }
            return Some(event);
        }
    }

    fn take_callback_overflow(&mut self) -> bool {
        self.shared
            .lock()
            .map(|mut state| std::mem::take(&mut state.callback_overflow))
            .unwrap_or(true)
    }

    fn steam_input_snapshot(&self) -> SteamInputSnapshot {
        self.shared
            .lock()
            .map(|state| state.steam_input_snapshot)
            .unwrap_or_default()
    }

    fn is_overlay_enabled(&self) -> bool {
        self.shared
            .lock()
            .map(|mut state| {
                state.overlay_enabled_query_count =
                    state.overlay_enabled_query_count.saturating_add(1);
                state.overlay_enabled
            })
            .unwrap_or(false)
    }

    fn is_overlay_active(&self) -> bool {
        self.shared
            .lock()
            .map(|state| state.overlay_active)
            .unwrap_or(false)
    }

    fn set_steam_input_action_set(
        &mut self,
        action_set: SteamInputActionSet,
    ) -> Result<(), SteamBackendError> {
        self.with_state_mut(|state| {
            state.steam_input_action_set = action_set;
            Ok(())
        })
    }

    fn show_steam_input_binding_panel(
        &mut self,
        local_ordinal: usize,
    ) -> Result<bool, SteamBackendError> {
        self.with_state_mut(|state| {
            let Some(controller) = state
                .steam_input_snapshot
                .controllers
                .get(local_ordinal)
                .copied()
            else {
                return Err(SteamBackendError::InvalidData);
            };
            if !controller.connected() {
                return Ok(false);
            }
            state.steam_input_binding_panel_open_count =
                state.steam_input_binding_panel_open_count.saturating_add(1);
            state.last_steam_input_binding_ordinal = Some(local_ordinal);
            Ok(true)
        })
    }

    fn create_lobby(
        &mut self,
        operation_id: SteamOperationId,
        _visibility: LobbyVisibility,
        maximum_peers: u8,
    ) -> Result<(), SteamBackendError> {
        if maximum_peers == 0 || usize::from(maximum_peers) > MAX_STEAM_LOBBY_MEMBERS {
            return Err(SteamBackendError::CapacityExceeded);
        }
        self.with_state_mut(|state| {
            let lobby = SteamLobbyId::new(state.next_lobby_id)
                .map_err(|_| SteamBackendError::IntegrityFailure)?;
            state.next_lobby_id = state.next_lobby_id.saturating_add(1);
            state.lobbies.insert(
                lobby,
                FakeLobby {
                    owner: state.local_user,
                    maximum_peers,
                    joinable: true,
                    members: vec![state.local_user],
                    data: BTreeMap::new(),
                    member_data: BTreeMap::new(),
                },
            );
            state.push_event(SteamBackendEvent::LobbyCreated {
                operation_id,
                result: Ok(lobby),
            });
            Ok(())
        })
    }

    fn join_lobby(
        &mut self,
        operation_id: SteamOperationId,
        lobby: SteamLobbyId,
    ) -> Result<(), SteamBackendError> {
        self.with_state_mut(|state| {
            let local_user = state.local_user;
            let result = match state.lobbies.get_mut(&lobby) {
                Some(record)
                    if record.joinable
                        && record.members.len() < usize::from(record.maximum_peers) =>
                {
                    if !record.members.contains(&local_user) {
                        record.members.push(local_user);
                    }
                    Ok(lobby)
                }
                _ => Err(SteamBackendError::OperationFailed),
            };
            state.push_event(SteamBackendEvent::LobbyJoined {
                operation_id,
                requested: lobby,
                result,
            });
            Ok(())
        })
    }

    fn leave_lobby(&mut self, lobby: SteamLobbyId) {
        let _ = self.with_state_mut(|state| {
            let local_user = state.local_user;
            if let Some(record) = state.lobbies.get_mut(&lobby) {
                record.members.retain(|member| *member != local_user);
                record
                    .member_data
                    .retain(|(user, _), _| *user != local_user);
                if record.owner == local_user
                    && let Some(successor) = record.members.first().copied()
                {
                    record.owner = successor;
                }
            }
            Ok(())
        });
    }

    fn set_lobby_joinable(
        &mut self,
        lobby: SteamLobbyId,
        joinable: bool,
    ) -> Result<(), SteamBackendError> {
        self.with_state_mut(|state| {
            let record = state
                .lobbies
                .get_mut(&lobby)
                .ok_or(SteamBackendError::InvalidData)?;
            if record.owner != state.local_user {
                return Err(SteamBackendError::OperationFailed);
            }
            record.joinable = joinable;
            Ok(())
        })
    }

    fn set_lobby_data(
        &mut self,
        lobby: SteamLobbyId,
        key: &'static str,
        value: &str,
    ) -> Result<(), SteamBackendError> {
        self.with_state_mut(|state| {
            let record = state
                .lobbies
                .get_mut(&lobby)
                .ok_or(SteamBackendError::InvalidData)?;
            if record.owner != state.local_user {
                return Err(SteamBackendError::OperationFailed);
            }
            if record.data.len() >= MAX_LOBBY_METADATA_PAIRS && !record.data.contains_key(key) {
                return Err(SteamBackendError::CapacityExceeded);
            }
            record.data.insert(key, value.to_owned());
            Ok(())
        })
    }

    fn lobby_data(
        &self,
        lobby: SteamLobbyId,
        key: &'static str,
    ) -> Result<Option<String>, SteamBackendError> {
        self.with_state_mut(|state| {
            let reads = state
                .lobby_data_read_counts
                .entry((lobby, key))
                .or_insert(0);
            *reads = reads.saturating_add(1);
            if state.fail_lobby_data_read == Some((lobby, key, *reads)) {
                state.fail_lobby_data_read = None;
                return Err(SteamBackendError::OperationFailed);
            }
            Ok(state
                .lobbies
                .get(&lobby)
                .and_then(|record| record.data.get(key).cloned()))
        })
    }

    fn set_member_data(
        &mut self,
        lobby: SteamLobbyId,
        key: &'static str,
        value: &str,
    ) -> Result<(), SteamBackendError> {
        self.with_state_mut(|state| {
            let local_user = state.local_user;
            let record = state
                .lobbies
                .get_mut(&lobby)
                .ok_or(SteamBackendError::InvalidData)?;
            if !record.members.contains(&local_user) {
                return Err(SteamBackendError::OperationFailed);
            }
            let local_pair_count = record
                .member_data
                .keys()
                .filter(|(user, _)| *user == local_user)
                .count();
            if local_pair_count >= MAX_MEMBER_METADATA_PAIRS
                && !record.member_data.contains_key(&(local_user, key))
            {
                return Err(SteamBackendError::CapacityExceeded);
            }
            record
                .member_data
                .insert((local_user, key), value.to_owned());
            Ok(())
        })
    }

    fn member_data(
        &self,
        lobby: SteamLobbyId,
        user: SteamUserId,
        key: &'static str,
    ) -> Result<Option<String>, SteamBackendError> {
        self.with_state(|state| {
            Ok(state
                .lobbies
                .get(&lobby)
                .and_then(|record| record.member_data.get(&(user, key)).cloned()))
        })
    }

    fn lobby_owner(&self, lobby: SteamLobbyId) -> Result<SteamUserId, SteamBackendError> {
        self.with_state(|state| {
            state
                .lobbies
                .get(&lobby)
                .map(|record| record.owner)
                .ok_or(SteamBackendError::InvalidData)
        })
    }

    fn lobby_members(&self, lobby: SteamLobbyId) -> Result<Vec<SteamUserId>, SteamBackendError> {
        self.with_state(|state| {
            let members = state
                .lobbies
                .get(&lobby)
                .ok_or(SteamBackendError::InvalidData)?
                .members
                .clone();
            if members.len() > MAX_STEAM_LOBBY_MEMBERS {
                Err(SteamBackendError::CapacityExceeded)
            } else {
                Ok(members)
            }
        })
    }

    fn is_friend(&self, user: SteamUserId) -> Result<bool, SteamBackendError> {
        self.with_state(|state| Ok(state.friends.contains(&user)))
    }

    fn open_invite_overlay(&mut self, lobby: SteamLobbyId) -> Result<(), SteamBackendError> {
        self.with_state_mut(|state| {
            if !state.lobbies.contains_key(&lobby) {
                return Err(SteamBackendError::InvalidData);
            }
            state.invite_overlay_open_count = state.invite_overlay_open_count.saturating_add(1);
            Ok(())
        })
    }

    fn launch_command_line(&self) -> Result<String, SteamBackendError> {
        self.with_state(|state| Ok(state.launch_command.clone()))
    }

    fn clear_rich_presence(&mut self) {
        let _ = self.with_state_mut(|state| {
            state.rich_presence.clear();
            Ok(())
        });
    }

    fn set_rich_presence(
        &mut self,
        key: &'static str,
        value: Option<&str>,
    ) -> Result<(), SteamBackendError> {
        self.with_state_mut(|state| {
            if let Some(value) = value {
                if state.rich_presence.len() >= MAX_RICH_PRESENCE_PAIRS
                    && !state.rich_presence.contains_key(key)
                {
                    return Err(SteamBackendError::CapacityExceeded);
                }
                state.rich_presence.insert(key, value.to_owned());
            } else {
                state.rich_presence.remove(key);
            }
            Ok(())
        })
    }

    fn issue_auth_ticket(
        &mut self,
        remote_user: SteamUserId,
    ) -> Result<BackendIssuedAuthTicket, SteamBackendError> {
        self.with_state_mut(|state| {
            if state.issued_tickets.len() >= MAX_STEAM_LOBBY_MEMBERS {
                return Err(SteamBackendError::CapacityExceeded);
            }
            let handle = AuthTicketHandle(state.next_ticket_handle);
            state.next_ticket_handle = state.next_ticket_handle.saturating_add(1);
            state.issued_tickets.insert(handle, remote_user);
            let mut bytes = Vec::with_capacity(16);
            bytes.extend_from_slice(&state.local_user.get().to_le_bytes());
            bytes.extend_from_slice(&remote_user.get().to_le_bytes());
            state.push_event(SteamBackendEvent::AuthTicketReady {
                handle,
                success: true,
            });
            Ok(BackendIssuedAuthTicket { handle, bytes })
        })
    }

    fn cancel_auth_ticket(&mut self, handle: AuthTicketHandle) {
        let _ = self.with_state_mut(|state| {
            state.issued_tickets.remove(&handle);
            state.cancelled_tickets.insert(handle);
            Ok(())
        });
    }

    fn begin_auth_session(
        &mut self,
        user: SteamUserId,
        _ticket: &[u8],
    ) -> Result<(), SteamBackendError> {
        self.with_state_mut(|state| {
            if state.active_auth_sessions.len() >= MAX_STEAM_LOBBY_MEMBERS
                || !state.active_auth_sessions.insert(user)
            {
                return Err(SteamBackendError::AuthenticationFailed);
            }
            let outcome = state
                .auth_outcomes
                .get(&user)
                .copied()
                .unwrap_or_else(|| FakeAuthOutcome::accepted(user));
            state.push_event(SteamBackendEvent::AuthSessionValidated {
                user,
                license_owner_user: outcome.license_owner_user,
                result: outcome.validation,
            });
            Ok(())
        })
    }

    fn end_auth_session(&mut self, user: SteamUserId) {
        let _ = self.with_state_mut(|state| {
            state.active_auth_sessions.remove(&user);
            state.ended_auth_sessions.insert(user);
            Ok(())
        });
    }

    fn license_status(
        &self,
        user: SteamUserId,
        _app_id: SteamAppId,
    ) -> Result<LicenseStatus, SteamBackendError> {
        self.with_state(|state| {
            Ok(state
                .auth_outcomes
                .get(&user)
                .map(|outcome| outcome.license)
                .unwrap_or(LicenseStatus::HasLicense))
        })
    }
}

#[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
mod real {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    const REAL_CALLBACK_EVENTS_PER_PUMP: usize = 16;
    const REAL_OPERATION_CALLBACK_CAPACITY: usize = 4;
    const STEAM_INPUT_GAMEPLAY_ACTION_SET: &str = "Gameplay";
    const STEAM_INPUT_MENU_ACTION_SET: &str = "Menu";
    const STEAM_INPUT_MOVE_ACTION: &str = "Move";
    const STEAM_INPUT_GAMEPLAY_ACTIONS: [&str; RawInputButton::ALL.len()] = [
        "Left", "Right", "Up", "Down", "AimGrab", "Heavy", "Light", "Jump",
    ];
    const STEAM_INPUT_MENU_ACTIONS: [&str; SteamMenuAction::ALL.len()] = [
        "MenuAccept",
        "MenuBack",
        "MenuUp",
        "MenuDown",
        "MenuLeft",
        "MenuRight",
        "MenuBindings",
    ];
    const STEAM_INPUT_MANIFEST_ENV: &str = "AFC_STEAM_INPUT_MANIFEST";
    const STEAM_INPUT_MANIFEST_RELATIVE_PATH: &str = "steam_input/action_manifest.vdf";
    const STEAM_INPUT_CONFIGURATION_FILES: [&str; 2] =
        ["steam_deck_default.vdf", "generic_gamepad_default.vdf"];
    const MAX_STEAM_INPUT_MANIFEST_BYTES: u64 = 64 * 1_024;
    static REAL_CLIENT_OWNED: AtomicBool = AtomicBool::new(false);

    #[derive(Clone, Copy)]
    struct RealTicketRecord {
        platform_handle: AuthTicketHandle,
        steam_handle: steamworks::AuthTicket,
    }

    struct RealOperationCallback {
        operation_id: SteamOperationId,
        event: Option<SteamBackendEvent>,
    }

    struct RealTrackedAuthSession {
        user: SteamUserId,
        seen_callback: bool,
        retired_awaiting_callback: bool,
        pending: Option<SteamBackendEvent>,
    }

    /// Bounded semantic mailbox for native Steam callbacks.
    ///
    /// Level-triggered lobby chatter is represented as dirty state, while
    /// security terminals and per-capability completions remain sticky. No
    /// remotely attributable callback rate can turn into a process-global
    /// queue-overflow fault.
    struct RealCallbackMailbox {
        local_user: SteamUserId,
        lobby_scope: Option<SteamLobbyId>,
        operations: Vec<RealOperationCallback>,
        retired_operation_high_water: u64,
        integrity_failure: bool,
        steam_disconnected: bool,
        local_departure: Option<SteamBackendEvent>,
        peer_departures: [Option<SteamBackendEvent>; MAX_STEAM_LOBBY_MEMBERS],
        membership_cleanup_all: bool,
        membership_dirty: Option<SteamBackendEvent>,
        lobby_data_dirty: Option<SteamBackendEvent>,
        member_data_dirty: [Option<SteamBackendEvent>; MAX_STEAM_LOBBY_MEMBERS],
        auth_sessions: [Option<RealTrackedAuthSession>; MAX_STEAM_LOBBY_MEMBERS],
        auth_tickets: [Option<SteamBackendEvent>; MAX_STEAM_LOBBY_MEMBERS],
        join_intent: Option<SteamBackendEvent>,
        launch_parameters_dirty: bool,
    }

    impl RealCallbackMailbox {
        fn new(local_user: SteamUserId) -> Self {
            Self {
                local_user,
                lobby_scope: None,
                operations: Vec::with_capacity(REAL_OPERATION_CALLBACK_CAPACITY),
                retired_operation_high_water: 0,
                integrity_failure: false,
                steam_disconnected: false,
                local_departure: None,
                peer_departures: std::array::from_fn(|_| None),
                membership_cleanup_all: false,
                membership_dirty: None,
                lobby_data_dirty: None,
                member_data_dirty: std::array::from_fn(|_| None),
                auth_sessions: std::array::from_fn(|_| None),
                auth_tickets: std::array::from_fn(|_| None),
                join_intent: None,
                launch_parameters_dirty: false,
            }
        }

        fn set_lobby_scope(&mut self, lobby: Option<SteamLobbyId>) {
            if self.lobby_scope == lobby {
                return;
            }
            self.lobby_scope = lobby;
            self.local_departure = None;
            self.peer_departures = std::array::from_fn(|_| None);
            self.membership_cleanup_all = false;
            self.membership_dirty = None;
            self.lobby_data_dirty = None;
            self.member_data_dirty = std::array::from_fn(|_| None);
        }

        fn register_operation(
            &mut self,
            operation_id: SteamOperationId,
        ) -> Result<(), SteamBackendError> {
            if operation_id.get() == 0
                || self
                    .operations
                    .iter()
                    .any(|operation| operation.operation_id == operation_id)
            {
                return Err(SteamBackendError::IntegrityFailure);
            }
            if self.operations.len() >= REAL_OPERATION_CALLBACK_CAPACITY {
                return Err(SteamBackendError::CapacityExceeded);
            }
            self.operations.push(RealOperationCallback {
                operation_id,
                event: None,
            });
            Ok(())
        }

        fn retire_operation(
            &mut self,
            operation_id: SteamOperationId,
        ) -> Result<Option<SteamLobbyId>, SteamBackendError> {
            let Some(index) = self
                .operations
                .iter()
                .position(|operation| operation.operation_id == operation_id)
            else {
                return if operation_id.get() <= self.retired_operation_high_water {
                    Ok(None)
                } else {
                    Err(SteamBackendError::IntegrityFailure)
                };
            };
            let retired = self.operations.swap_remove(index);
            self.retired_operation_high_water =
                self.retired_operation_high_water.max(operation_id.get());
            Ok(retired.event.as_ref().and_then(callback_success_lobby))
        }

        /// Returns a stale successful lobby that must be left immediately.
        fn complete_operation(&mut self, event: SteamBackendEvent) -> Option<SteamLobbyId> {
            let Some(operation_id) = callback_operation_id(&event) else {
                self.integrity_failure = true;
                return None;
            };
            let Some(operation) = self
                .operations
                .iter_mut()
                .find(|operation| operation.operation_id == operation_id)
            else {
                if operation_id.get() <= self.retired_operation_high_water {
                    return callback_success_lobby(&event)
                        .filter(|lobby| self.lobby_scope != Some(*lobby));
                }
                self.integrity_failure = true;
                return None;
            };
            match &operation.event {
                None => operation.event = Some(event),
                Some(prior) if prior == &event => {}
                Some(_) => self.integrity_failure = true,
            }
            None
        }

        fn begin_auth_session(&mut self, user: SteamUserId) -> Result<(), SteamBackendError> {
            if self
                .auth_sessions
                .iter()
                .flatten()
                .any(|session| session.user == user)
            {
                // A callback has no Steam auth-session generation. Never let a
                // delayed callback from a retired generation authenticate its
                // replacement.
                return Err(SteamBackendError::AuthenticationFailed);
            }
            let slot = self
                .auth_sessions
                .iter_mut()
                .find(|slot| slot.is_none())
                .ok_or(SteamBackendError::CapacityExceeded)?;
            *slot = Some(RealTrackedAuthSession {
                user,
                seen_callback: false,
                retired_awaiting_callback: false,
                pending: None,
            });
            Ok(())
        }

        fn abort_auth_session_start(&mut self, user: SteamUserId) {
            if let Some(slot) = self.auth_sessions.iter_mut().find(|slot| {
                slot.as_ref().is_some_and(|session| {
                    session.user == user
                        && !session.seen_callback
                        && !session.retired_awaiting_callback
                })
            }) {
                *slot = None;
            }
        }

        fn retire_auth_session(&mut self, user: SteamUserId) {
            let Some(slot) = self
                .auth_sessions
                .iter_mut()
                .find(|slot| slot.as_ref().is_some_and(|session| session.user == user))
            else {
                return;
            };
            let session = slot.as_mut().expect("tracked auth slot exists");
            session.pending = None;
            if session.seen_callback {
                *slot = None;
            } else {
                session.retired_awaiting_callback = true;
            }
        }

        fn retire_auth_ticket(&mut self, handle: AuthTicketHandle) {
            if let Some(slot) = self.auth_tickets.iter_mut().find(|slot| {
                slot.as_ref()
                    .is_some_and(|event| callback_ticket_handle(event) == Some(handle))
            }) {
                *slot = None;
            }
        }

        fn enqueue(&mut self, event: SteamBackendEvent) {
            match event {
                SteamBackendEvent::LobbyCreated { .. } | SteamBackendEvent::LobbyJoined { .. } => {
                    let _ = self.complete_operation(event);
                }
                SteamBackendEvent::IntegrityFailure => self.integrity_failure = true,
                SteamBackendEvent::SteamDisconnected => self.steam_disconnected = true,
                SteamBackendEvent::LobbyMembershipChanged {
                    lobby,
                    user,
                    change,
                } if self.lobby_scope == Some(lobby) => {
                    let event = SteamBackendEvent::LobbyMembershipChanged {
                        lobby,
                        user,
                        change,
                    };
                    if is_membership_departure(change) {
                        if user == self.local_user {
                            self.local_departure = Some(event.clone());
                        } else if let Some(slot) = self.peer_departures.iter_mut().find(|slot| {
                            slot.as_ref()
                                .is_some_and(|prior| callback_membership_user(prior) == Some(user))
                        }) {
                            *slot = Some(event.clone());
                        } else if let Some(slot) =
                            self.peer_departures.iter_mut().find(|slot| slot.is_none())
                        {
                            *slot = Some(event.clone());
                        } else {
                            self.membership_cleanup_all = true;
                        }
                    }
                    self.membership_dirty = Some(event);
                }
                SteamBackendEvent::LobbyMembershipChanged { .. }
                | SteamBackendEvent::LobbyMembershipResync { .. } => {}
                SteamBackendEvent::LobbyDataChanged { lobby, subject }
                    if self.lobby_scope == Some(lobby) =>
                {
                    let event = SteamBackendEvent::LobbyDataChanged { lobby, subject };
                    match subject {
                        LobbyDataSubject::Lobby => self.lobby_data_dirty = Some(event),
                        LobbyDataSubject::Member(user) => {
                            if let Some(slot) = self.member_data_dirty.iter_mut().find(|slot| {
                                slot.as_ref().is_some_and(|prior| {
                                    matches!(
                                        prior,
                                        SteamBackendEvent::LobbyDataChanged {
                                            subject: LobbyDataSubject::Member(prior_user),
                                            ..
                                        } if *prior_user == user
                                    )
                                })
                            }) {
                                *slot = Some(event);
                            } else if let Some(slot) = self
                                .member_data_dirty
                                .iter_mut()
                                .find(|slot| slot.is_none())
                            {
                                *slot = Some(event);
                            } else {
                                // One member-data dirty event causes a full
                                // canonical roster reread. Retaining the latest
                                // key is therefore a safe pressure fallback.
                                self.member_data_dirty[0] = Some(event);
                            }
                        }
                    }
                }
                SteamBackendEvent::LobbyDataChanged { .. } => {}
                SteamBackendEvent::AuthSessionValidated { user, .. } => {
                    let Some(slot) = self
                        .auth_sessions
                        .iter_mut()
                        .find(|slot| slot.as_ref().is_some_and(|session| session.user == user))
                    else {
                        return;
                    };
                    let session = slot.as_mut().expect("tracked auth slot exists");
                    if session.retired_awaiting_callback {
                        *slot = None;
                        return;
                    }
                    session.seen_callback = true;
                    match session.pending.as_ref() {
                        None => session.pending = Some(event),
                        Some(prior) => {
                            match (callback_auth_result(prior), callback_auth_result(&event)) {
                                (Some(Err(_)), _) => {}
                                (Some(Ok(_)), Some(Err(_))) => session.pending = Some(event),
                                (Some(Ok(prior_owner)), Some(Ok(owner)))
                                    if prior_owner == owner => {}
                                (Some(Ok(_)), Some(Ok(_))) | (None, _) | (_, None) => {
                                    self.integrity_failure = true;
                                }
                            }
                        }
                    }
                }
                SteamBackendEvent::AuthTicketReady { handle, success } => {
                    if let Some(slot) = self.auth_tickets.iter_mut().find(|slot| {
                        slot.as_ref()
                            .is_some_and(|prior| callback_ticket_handle(prior) == Some(handle))
                    }) {
                        let prior_failed = slot
                            .as_ref()
                            .is_some_and(|prior| callback_ticket_failed(prior));
                        if !prior_failed && !success {
                            *slot = Some(SteamBackendEvent::AuthTicketReady { handle, success });
                        }
                    } else if let Some(slot) =
                        self.auth_tickets.iter_mut().find(|slot| slot.is_none())
                    {
                        *slot = Some(SteamBackendEvent::AuthTicketReady { handle, success });
                    } else {
                        self.integrity_failure = true;
                    }
                }
                SteamBackendEvent::LobbyJoinRequested { .. }
                | SteamBackendEvent::RichPresenceJoinRequested { .. } => {
                    self.join_intent = Some(event);
                }
                SteamBackendEvent::LaunchParametersChanged => {
                    self.launch_parameters_dirty = true;
                }
            }
        }

        fn pop_event(&mut self) -> Option<SteamBackendEvent> {
            if let Some((index, _)) = self
                .operations
                .iter()
                .enumerate()
                .filter(|(_, operation)| operation.event.is_some())
                .max_by_key(|(_, operation)| operation.operation_id.get())
            {
                return self.operations.swap_remove(index).event;
            }
            if self.integrity_failure {
                self.integrity_failure = false;
                self.clear_non_operation_callbacks();
                return Some(SteamBackendEvent::IntegrityFailure);
            }
            if self.steam_disconnected {
                self.steam_disconnected = false;
                self.clear_non_operation_callbacks();
                return Some(SteamBackendEvent::SteamDisconnected);
            }
            if let Some(event) = self.local_departure.take() {
                return Some(event);
            }
            if self.membership_cleanup_all {
                self.membership_cleanup_all = false;
                self.peer_departures = std::array::from_fn(|_| None);
                self.membership_dirty = None;
                return self
                    .lobby_scope
                    .map(|lobby| SteamBackendEvent::LobbyMembershipResync { lobby });
            }
            if let Some(slot) = self.peer_departures.iter_mut().find(|slot| slot.is_some()) {
                let event = slot.take();
                if self.membership_dirty == event {
                    self.membership_dirty = None;
                }
                return event;
            }
            if let Some(event) = take_pending_auth_callback(&mut self.auth_sessions, true) {
                return Some(event);
            }
            if let Some(event) = take_auth_ticket_callback(&mut self.auth_tickets, true) {
                return Some(event);
            }
            if let Some(event) = take_pending_auth_callback(&mut self.auth_sessions, false) {
                return Some(event);
            }
            if let Some(event) = take_auth_ticket_callback(&mut self.auth_tickets, false) {
                return Some(event);
            }
            self.membership_dirty
                .take()
                .or_else(|| self.lobby_data_dirty.take())
                .or_else(|| self.member_data_dirty.iter_mut().find_map(Option::take))
                .or_else(|| {
                    if self.launch_parameters_dirty {
                        self.launch_parameters_dirty = false;
                        Some(SteamBackendEvent::LaunchParametersChanged)
                    } else {
                        None
                    }
                })
                .or_else(|| self.join_intent.take())
        }

        fn clear_non_operation_callbacks(&mut self) {
            self.local_departure = None;
            self.peer_departures = std::array::from_fn(|_| None);
            self.membership_cleanup_all = false;
            self.membership_dirty = None;
            self.lobby_data_dirty = None;
            self.member_data_dirty = std::array::from_fn(|_| None);
            for session in self.auth_sessions.iter_mut().flatten() {
                session.pending = None;
            }
            self.auth_tickets = std::array::from_fn(|_| None);
            self.join_intent = None;
            self.launch_parameters_dirty = false;
        }

        #[cfg(test)]
        fn pending_event_count(&self) -> usize {
            self.operations
                .iter()
                .filter(|operation| operation.event.is_some())
                .count()
                + usize::from(self.integrity_failure)
                + usize::from(self.steam_disconnected)
                + usize::from(self.local_departure.is_some())
                + self.peer_departures.iter().flatten().count()
                + usize::from(self.membership_cleanup_all)
                + usize::from(self.membership_dirty.is_some())
                + usize::from(self.lobby_data_dirty.is_some())
                + self.member_data_dirty.iter().flatten().count()
                + self
                    .auth_sessions
                    .iter()
                    .flatten()
                    .filter(|session| session.pending.is_some())
                    .count()
                + self.auth_tickets.iter().flatten().count()
                + usize::from(self.join_intent.is_some())
                + usize::from(self.launch_parameters_dirty)
        }
    }

    fn callback_operation_id(event: &SteamBackendEvent) -> Option<SteamOperationId> {
        match event {
            SteamBackendEvent::LobbyCreated { operation_id, .. }
            | SteamBackendEvent::LobbyJoined { operation_id, .. } => Some(*operation_id),
            _ => None,
        }
    }

    fn callback_success_lobby(event: &SteamBackendEvent) -> Option<SteamLobbyId> {
        match event {
            SteamBackendEvent::LobbyCreated {
                result: Ok(lobby), ..
            }
            | SteamBackendEvent::LobbyJoined {
                result: Ok(lobby), ..
            } => Some(*lobby),
            _ => None,
        }
    }

    fn callback_ticket_handle(event: &SteamBackendEvent) -> Option<AuthTicketHandle> {
        match event {
            SteamBackendEvent::AuthTicketReady { handle, .. } => Some(*handle),
            _ => None,
        }
    }

    fn callback_ticket_failed(event: &SteamBackendEvent) -> bool {
        matches!(
            event,
            SteamBackendEvent::AuthTicketReady { success: false, .. }
        )
    }

    fn callback_membership_user(event: &SteamBackendEvent) -> Option<SteamUserId> {
        match event {
            SteamBackendEvent::LobbyMembershipChanged { user, .. } => Some(*user),
            _ => None,
        }
    }

    fn callback_auth_result(
        event: &SteamBackendEvent,
    ) -> Option<Result<SteamUserId, AuthValidationFailure>> {
        match event {
            SteamBackendEvent::AuthSessionValidated {
                license_owner_user,
                result,
                ..
            } => Some(match result {
                Ok(()) => Ok(*license_owner_user),
                Err(error) => Err(*error),
            }),
            _ => None,
        }
    }

    fn is_membership_departure(change: LobbyMembershipChange) -> bool {
        matches!(
            change,
            LobbyMembershipChange::Left
                | LobbyMembershipChange::Disconnected
                | LobbyMembershipChange::Kicked
                | LobbyMembershipChange::Banned
        )
    }

    fn take_pending_auth_callback(
        sessions: &mut [Option<RealTrackedAuthSession>; MAX_STEAM_LOBBY_MEMBERS],
        failure: bool,
    ) -> Option<SteamBackendEvent> {
        let slot = sessions.iter_mut().find(|slot| {
            slot.as_ref()
                .and_then(|session| session.pending.as_ref())
                .is_some_and(|event| {
                    callback_auth_result(event).is_some_and(|result| result.is_err() == failure)
                })
        })?;
        slot.as_mut()?.pending.take()
    }

    fn take_auth_ticket_callback(
        tickets: &mut [Option<SteamBackendEvent>; MAX_STEAM_LOBBY_MEMBERS],
        failure: bool,
    ) -> Option<SteamBackendEvent> {
        let slot = tickets.iter_mut().find(|slot| {
            slot.as_ref()
                .is_some_and(|event| callback_ticket_failed(event) == failure)
        })?;
        slot.take()
    }

    fn enqueue_real_callback(
        mailbox: &Arc<Mutex<RealCallbackMailbox>>,
        integrity_failure: &AtomicBool,
        event: SteamBackendEvent,
    ) {
        match mailbox.lock() {
            Ok(mut mailbox) => mailbox.enqueue(event),
            Err(_) => integrity_failure.store(true, Ordering::Release),
        }
    }

    fn enqueue_real_operation_callback(
        mailbox: &Arc<Mutex<RealCallbackMailbox>>,
        integrity_failure: &AtomicBool,
        client: &steamworks::Client,
        event: SteamBackendEvent,
    ) {
        let stale_lobby = match mailbox.lock() {
            Ok(mut mailbox) => mailbox.complete_operation(event),
            Err(_) => {
                integrity_failure.store(true, Ordering::Release);
                None
            }
        };
        if let Some(lobby) = stale_lobby {
            client
                .matchmaking()
                .leave_lobby(steamworks::LobbyId::from_raw(lobby.get()));
        }
    }

    pub(super) fn coalesce_overlay_activity(
        state: &AtomicBool,
        event: steamworks::GameOverlayActivated,
    ) {
        state.store(event.active, Ordering::Release);
    }

    struct RealSteamInputState {
        gameplay_action_set: u64,
        menu_action_set: u64,
        desired_action_set: SteamInputActionSet,
        movement_action: u64,
        gameplay_actions: [u64; RawInputButton::ALL.len()],
        menu_actions: [u64; SteamMenuAction::ALL.len()],
        assignments: SteamInputAssignments,
        snapshot: SteamInputSnapshot,
    }

    impl RealSteamInputState {
        fn initialize(client: &steamworks::Client) -> Result<Self, SteamBackendError> {
            let manifest = resolve_steam_input_manifest()?;
            validate_steam_input_manifest(&manifest)?;
            let manifest = manifest
                .to_str()
                .ok_or(SteamBackendError::SteamInputManifestInvalid)?;
            let input = client.input();
            if !input.init(true) {
                return Err(SteamBackendError::SteamInputInitializationFailed);
            }
            if !input.set_input_action_manifest_file_path(manifest) {
                input.shutdown();
                return Err(SteamBackendError::SteamInputManifestInvalid);
            }
            input.run_frame();

            let gameplay_action_set = input.get_action_set_handle(STEAM_INPUT_GAMEPLAY_ACTION_SET);
            let menu_action_set = input.get_action_set_handle(STEAM_INPUT_MENU_ACTION_SET);
            let movement_action = input.get_analog_action_handle(STEAM_INPUT_MOVE_ACTION);
            let gameplay_actions = std::array::from_fn(|index| {
                input.get_digital_action_handle(STEAM_INPUT_GAMEPLAY_ACTIONS[index])
            });
            let menu_actions = std::array::from_fn(|index| {
                input.get_digital_action_handle(STEAM_INPUT_MENU_ACTIONS[index])
            });
            if gameplay_action_set == 0
                || menu_action_set == 0
                || movement_action == 0
                || gameplay_actions.contains(&0)
                || menu_actions.contains(&0)
            {
                input.shutdown();
                return Err(SteamBackendError::SteamInputActionMissing);
            }

            Ok(Self {
                gameplay_action_set,
                menu_action_set,
                desired_action_set: SteamInputActionSet::Menu,
                movement_action,
                gameplay_actions,
                menu_actions,
                assignments: SteamInputAssignments::default(),
                snapshot: SteamInputSnapshot::default(),
            })
        }
    }

    pub(crate) struct RealClientOwnershipGuard {
        next_transport_connection_id: AtomicU32,
    }

    impl RealClientOwnershipGuard {
        pub(crate) fn allocate_transport_connection_id(&self) -> Option<u32> {
            self.next_transport_connection_id
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    current.checked_add(1)
                })
                .ok()
        }
    }

    impl Drop for RealClientOwnershipGuard {
        fn drop(&mut self) {
            REAL_CLIENT_OWNED.store(false, Ordering::Release);
        }
    }

    /// Desktop Steam client backend for `steamworks` 0.12.2.
    ///
    /// Construction is intentionally available only through
    /// [`SteamPlatform::initialize_steam_client`], keeping the `steamworks::Client`
    /// and its callback pump private.
    pub struct RealSteamBackend {
        client: steamworks::Client,
        steam_input: RealSteamInputState,
        overlay_active: Arc<AtomicBool>,
        callback_mailbox: Arc<Mutex<RealCallbackMailbox>>,
        callback_integrity_failure: Arc<AtomicBool>,
        callback_events_remaining: usize,
        _callback_handles: Vec<steamworks::CallbackHandle>,
        tickets: Arc<Mutex<Vec<RealTicketRecord>>>,
        next_ticket_handle: u32,
        callback_owner_alive: Arc<AtomicBool>,
        ownership: Arc<RealClientOwnershipGuard>,
    }

    impl RealSteamBackend {
        fn initialize(app_id: SteamAppId) -> Result<Self, SteamBackendError> {
            if REAL_CLIENT_OWNED
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return Err(SteamBackendError::AlreadyInitialized);
            }
            let result = Self::initialize_owned(app_id);
            if result.is_err() {
                REAL_CLIENT_OWNED.store(false, Ordering::Release);
            }
            result
        }

        fn initialize_owned(app_id: SteamAppId) -> Result<Self, SteamBackendError> {
            let client = steamworks::Client::init_app(app_id.get())
                .map_err(|_| SteamBackendError::InitializationFailed)?;
            if client.utils().app_id().0 != app_id.get() {
                return Err(SteamBackendError::AppIdMismatch);
            }
            if !client.user().logged_on() {
                return Err(SteamBackendError::NotLoggedOn);
            }
            let local_user = backend_user(client.user().steam_id())?;
            let steam_input = RealSteamInputState::initialize(&client)?;

            let callback_mailbox = Arc::new(Mutex::new(RealCallbackMailbox::new(local_user)));
            let callback_integrity_failure = Arc::new(AtomicBool::new(false));
            let overlay_active = Arc::new(AtomicBool::new(false));
            let tickets = Arc::new(Mutex::new(Vec::<RealTicketRecord>::with_capacity(
                MAX_STEAM_LOBBY_MEMBERS,
            )));
            let mut callback_handles = Vec::with_capacity(10);

            {
                let active = overlay_active.clone();
                callback_handles.push(client.register_callback(
                    move |event: steamworks::GameOverlayActivated| {
                        // Activity is latest-value presentation state. It must
                        // never consume a semantic mailbox or public event slot.
                        coalesce_overlay_activity(&active, event);
                    },
                ));
            }
            {
                let mailbox = callback_mailbox.clone();
                let integrity = callback_integrity_failure.clone();
                callback_handles.push(client.register_callback(
                    move |event: steamworks::GameLobbyJoinRequested| {
                        let translated = backend_lobby(event.lobby_steam_id)
                            .map(|lobby| SteamBackendEvent::LobbyJoinRequested {
                                lobby,
                                friend: optional_backend_user(event.friend_steam_id),
                            })
                            .unwrap_or(SteamBackendEvent::IntegrityFailure);
                        enqueue_real_callback(&mailbox, &integrity, translated);
                    },
                ));
            }
            {
                let mailbox = callback_mailbox.clone();
                let integrity = callback_integrity_failure.clone();
                callback_handles.push(client.register_callback(
                    move |event: steamworks::GameRichPresenceJoinRequested| {
                        // Rich-presence commands are untrusted UX intents. A
                        // malformed command is ignored before it can consume a
                        // mailbox slot or fault an active match.
                        if parse_connect_lobby_command(&event.connect)
                            .ok()
                            .flatten()
                            .is_some()
                        {
                            enqueue_real_callback(
                                &mailbox,
                                &integrity,
                                SteamBackendEvent::RichPresenceJoinRequested {
                                    friend: optional_backend_user(event.friend_steam_id),
                                    connect: event.connect,
                                },
                            );
                        }
                    },
                ));
            }
            {
                let mailbox = callback_mailbox.clone();
                let integrity = callback_integrity_failure.clone();
                callback_handles.push(client.register_callback(
                    move |_event: steamworks::NewUrlLaunchParameters| {
                        enqueue_real_callback(
                            &mailbox,
                            &integrity,
                            SteamBackendEvent::LaunchParametersChanged,
                        );
                    },
                ));
            }
            {
                let mailbox = callback_mailbox.clone();
                let integrity = callback_integrity_failure.clone();
                callback_handles.push(client.register_callback(
                    move |event: steamworks::LobbyChatUpdate| {
                        let translated = backend_lobby(event.lobby)
                            .and_then(|lobby| {
                                Ok(SteamBackendEvent::LobbyMembershipChanged {
                                    lobby,
                                    user: backend_user(event.user_changed)?,
                                    change: match event.member_state_change {
                                        steamworks::ChatMemberStateChange::Entered => {
                                            LobbyMembershipChange::Entered
                                        }
                                        steamworks::ChatMemberStateChange::Left => {
                                            LobbyMembershipChange::Left
                                        }
                                        steamworks::ChatMemberStateChange::Disconnected => {
                                            LobbyMembershipChange::Disconnected
                                        }
                                        steamworks::ChatMemberStateChange::Kicked => {
                                            LobbyMembershipChange::Kicked
                                        }
                                        steamworks::ChatMemberStateChange::Banned => {
                                            LobbyMembershipChange::Banned
                                        }
                                    },
                                })
                            })
                            .unwrap_or(SteamBackendEvent::IntegrityFailure);
                        enqueue_real_callback(&mailbox, &integrity, translated);
                    },
                ));
            }
            {
                let mailbox = callback_mailbox.clone();
                let integrity = callback_integrity_failure.clone();
                callback_handles.push(client.register_callback(
                    move |event: steamworks::LobbyDataUpdate| {
                        let translated = translate_lobby_data_update(event)
                            .unwrap_or(SteamBackendEvent::IntegrityFailure);
                        enqueue_real_callback(&mailbox, &integrity, translated);
                    },
                ));
            }
            {
                let mailbox = callback_mailbox.clone();
                let integrity = callback_integrity_failure.clone();
                let ticket_map = tickets.clone();
                callback_handles.push(client.register_callback(
                    move |event: steamworks::AuthSessionTicketResponse| {
                        let translated = match ticket_map.lock() {
                            Ok(records) => records
                                .iter()
                                .find(|record| record.steam_handle == event.ticket)
                                .map(|record| SteamBackendEvent::AuthTicketReady {
                                    handle: record.platform_handle,
                                    success: event.result.is_ok(),
                                }),
                            Err(_) => Some(SteamBackendEvent::IntegrityFailure),
                        };
                        // Cancel removes the native/platform handle mapping.
                        // A response racing that retirement is benign; lock
                        // failure remains identityless infrastructure failure.
                        if let Some(translated) = translated {
                            enqueue_real_callback(&mailbox, &integrity, translated);
                        }
                    },
                ));
            }
            {
                let mailbox = callback_mailbox.clone();
                let integrity = callback_integrity_failure.clone();
                callback_handles.push(client.register_callback(
                    move |event: steamworks::ValidateAuthTicketResponse| {
                        let translated = translate_auth_validation(event)
                            .unwrap_or(SteamBackendEvent::IntegrityFailure);
                        enqueue_real_callback(&mailbox, &integrity, translated);
                    },
                ));
            }
            {
                let mailbox = callback_mailbox.clone();
                let integrity = callback_integrity_failure.clone();
                callback_handles.push(client.register_callback(
                    move |_event: steamworks::SteamServersDisconnected| {
                        enqueue_real_callback(
                            &mailbox,
                            &integrity,
                            SteamBackendEvent::SteamDisconnected,
                        );
                    },
                ));
            }
            {
                let mailbox = callback_mailbox.clone();
                let integrity = callback_integrity_failure.clone();
                callback_handles.push(client.register_callback(
                    move |event: steamworks::SteamServerConnectFailure| {
                        if !event.still_retrying {
                            enqueue_real_callback(
                                &mailbox,
                                &integrity,
                                SteamBackendEvent::SteamDisconnected,
                            );
                        }
                    },
                ));
            }

            Ok(Self {
                client,
                steam_input,
                overlay_active,
                callback_mailbox,
                callback_integrity_failure,
                callback_events_remaining: 0,
                _callback_handles: callback_handles,
                tickets,
                next_ticket_handle: 1,
                callback_owner_alive: Arc::new(AtomicBool::new(true)),
                ownership: Arc::new(RealClientOwnershipGuard {
                    next_transport_connection_id: AtomicU32::new(1),
                }),
            })
        }

        fn next_platform_ticket_handle(&mut self) -> Result<AuthTicketHandle, SteamBackendError> {
            let handle = self.next_ticket_handle;
            self.next_ticket_handle = self
                .next_ticket_handle
                .checked_add(1)
                .ok_or(SteamBackendError::CapacityExceeded)?;
            if handle == 0 {
                return Err(SteamBackendError::IntegrityFailure);
            }
            Ok(AuthTicketHandle(handle))
        }
    }

    impl SteamBackend for RealSteamBackend {
        fn configured_app_id(&self) -> Result<SteamAppId, SteamBackendError> {
            SteamAppId::new(self.client.utils().app_id().0)
                .map_err(|_| SteamBackendError::AppIdMismatch)
        }

        fn local_user(&self) -> Result<SteamUserId, SteamBackendError> {
            if !self.client.user().logged_on() {
                return Err(SteamBackendError::NotLoggedOn);
            }
            backend_user(self.client.user().steam_id())
        }

        fn pump_callbacks(&mut self) -> Result<(), SteamBackendError> {
            self.client.run_callbacks();
            if self
                .callback_integrity_failure
                .swap(false, Ordering::AcqRel)
            {
                return Err(SteamBackendError::IntegrityFailure);
            }
            self.refresh_steam_input();
            self.callback_events_remaining = REAL_CALLBACK_EVENTS_PER_PUMP;
            Ok(())
        }

        fn poll_event(&mut self) -> Option<SteamBackendEvent> {
            if self.callback_events_remaining == 0 {
                return None;
            }
            let event = match self.callback_mailbox.lock() {
                Ok(mut mailbox) => mailbox.pop_event(),
                Err(_) => Some(SteamBackendEvent::IntegrityFailure),
            };
            if event.is_some() {
                self.callback_events_remaining -= 1;
            } else {
                self.callback_events_remaining = 0;
            }
            event
        }

        fn take_callback_overflow(&mut self) -> bool {
            false
        }

        fn set_callback_lobby_scope(&mut self, lobby: Option<SteamLobbyId>) {
            match self.callback_mailbox.lock() {
                Ok(mut mailbox) => mailbox.set_lobby_scope(lobby),
                Err(_) => self
                    .callback_integrity_failure
                    .store(true, Ordering::Release),
            }
        }

        fn retire_lobby_operation(&mut self, operation_id: SteamOperationId) {
            let retired_success = match self.callback_mailbox.lock() {
                Ok(mut mailbox) => mailbox.retire_operation(operation_id),
                Err(_) => Err(SteamBackendError::IntegrityFailure),
            };
            match retired_success {
                Ok(Some(lobby)) => self
                    .client
                    .matchmaking()
                    .leave_lobby(steamworks::LobbyId::from_raw(lobby.get())),
                Ok(None) => {}
                Err(_) => self
                    .callback_integrity_failure
                    .store(true, Ordering::Release),
            }
        }

        fn steam_input_snapshot(&self) -> SteamInputSnapshot {
            self.steam_input.snapshot
        }

        fn is_overlay_enabled(&self) -> bool {
            self.client.utils().is_overlay_enabled()
        }

        fn is_overlay_active(&self) -> bool {
            self.overlay_active.load(Ordering::Acquire)
        }

        fn set_steam_input_action_set(
            &mut self,
            action_set: SteamInputActionSet,
        ) -> Result<(), SteamBackendError> {
            if self.steam_input.desired_action_set == action_set {
                return Ok(());
            }
            self.steam_input.desired_action_set = action_set;
            let action_set_handle = match action_set {
                SteamInputActionSet::Gameplay => self.steam_input.gameplay_action_set,
                SteamInputActionSet::Menu => self.steam_input.menu_action_set,
            };
            {
                let input = self.client.input();
                for controller in self.steam_input.assignments.handles.iter().flatten() {
                    input.activate_action_set_handle(controller.get(), action_set_handle);
                }
            }
            // Re-sample only on a context transition so the first gameplay or
            // menu render frame does not inherit one frame of the old set.
            self.refresh_steam_input();
            Ok(())
        }

        fn show_steam_input_binding_panel(
            &mut self,
            local_ordinal: usize,
        ) -> Result<bool, SteamBackendError> {
            let Some(controller) = self
                .steam_input
                .assignments
                .handles
                .get(local_ordinal)
                .copied()
                .flatten()
            else {
                return Ok(false);
            };
            Ok(self.client.input().show_binding_panel(controller.get()))
        }

        fn create_lobby(
            &mut self,
            operation_id: SteamOperationId,
            visibility: LobbyVisibility,
            maximum_peers: u8,
        ) -> Result<(), SteamBackendError> {
            if maximum_peers == 0 || usize::from(maximum_peers) > MAX_STEAM_LOBBY_MEMBERS {
                return Err(SteamBackendError::CapacityExceeded);
            }
            let lobby_type = match visibility {
                LobbyVisibility::Private => steamworks::LobbyType::Private,
                LobbyVisibility::FriendsOnly => steamworks::LobbyType::FriendsOnly,
                LobbyVisibility::Public => steamworks::LobbyType::Public,
            };
            self.callback_mailbox
                .lock()
                .map_err(|_| SteamBackendError::IntegrityFailure)?
                .register_operation(operation_id)?;
            let mailbox = self.callback_mailbox.clone();
            let integrity = self.callback_integrity_failure.clone();
            let cleanup_client = self.client.clone();
            self.client.matchmaking().create_lobby(
                lobby_type,
                u32::from(maximum_peers),
                move |result| {
                    let translated = result
                        .map_err(|_| SteamBackendError::OperationFailed)
                        .and_then(backend_lobby);
                    enqueue_real_operation_callback(
                        &mailbox,
                        &integrity,
                        &cleanup_client,
                        SteamBackendEvent::LobbyCreated {
                            operation_id,
                            result: translated,
                        },
                    );
                },
            );
            Ok(())
        }

        fn join_lobby(
            &mut self,
            operation_id: SteamOperationId,
            lobby: SteamLobbyId,
        ) -> Result<(), SteamBackendError> {
            self.callback_mailbox
                .lock()
                .map_err(|_| SteamBackendError::IntegrityFailure)?
                .register_operation(operation_id)?;
            let mailbox = self.callback_mailbox.clone();
            let integrity = self.callback_integrity_failure.clone();
            let cleanup_client = self.client.clone();
            self.client.matchmaking().join_lobby(
                steamworks::LobbyId::from_raw(lobby.get()),
                move |result| {
                    let translated = result
                        .map_err(|()| SteamBackendError::OperationFailed)
                        .and_then(backend_lobby);
                    enqueue_real_operation_callback(
                        &mailbox,
                        &integrity,
                        &cleanup_client,
                        SteamBackendEvent::LobbyJoined {
                            operation_id,
                            requested: lobby,
                            result: translated,
                        },
                    );
                },
            );
            Ok(())
        }

        fn leave_lobby(&mut self, lobby: SteamLobbyId) {
            self.client
                .matchmaking()
                .leave_lobby(steamworks::LobbyId::from_raw(lobby.get()));
        }

        fn set_lobby_joinable(
            &mut self,
            lobby: SteamLobbyId,
            joinable: bool,
        ) -> Result<(), SteamBackendError> {
            if self
                .client
                .matchmaking()
                .set_lobby_joinable(steamworks::LobbyId::from_raw(lobby.get()), joinable)
            {
                Ok(())
            } else {
                Err(SteamBackendError::OperationFailed)
            }
        }

        fn set_lobby_data(
            &mut self,
            lobby: SteamLobbyId,
            key: &'static str,
            value: &str,
        ) -> Result<(), SteamBackendError> {
            validate_real_string(key, 255)?;
            validate_real_string(value, 128)?;
            if self.client.matchmaking().set_lobby_data(
                steamworks::LobbyId::from_raw(lobby.get()),
                key,
                value,
            ) {
                Ok(())
            } else {
                Err(SteamBackendError::OperationFailed)
            }
        }

        fn lobby_data(
            &self,
            lobby: SteamLobbyId,
            key: &'static str,
        ) -> Result<Option<String>, SteamBackendError> {
            validate_real_string(key, 255)?;
            Ok(self
                .client
                .matchmaking()
                .lobby_data(steamworks::LobbyId::from_raw(lobby.get()), key))
        }

        fn set_member_data(
            &mut self,
            lobby: SteamLobbyId,
            key: &'static str,
            value: &str,
        ) -> Result<(), SteamBackendError> {
            validate_real_string(key, 255)?;
            validate_real_string(value, 128)?;
            self.client.matchmaking().set_lobby_member_data(
                steamworks::LobbyId::from_raw(lobby.get()),
                key,
                value,
            );
            Ok(())
        }

        fn member_data(
            &self,
            lobby: SteamLobbyId,
            user: SteamUserId,
            key: &'static str,
        ) -> Result<Option<String>, SteamBackendError> {
            validate_real_string(key, 255)?;
            Ok(self.client.matchmaking().get_lobby_member_data(
                steamworks::LobbyId::from_raw(lobby.get()),
                steamworks::SteamId::from_raw(user.get()),
                key,
            ))
        }

        fn lobby_owner(&self, lobby: SteamLobbyId) -> Result<SteamUserId, SteamBackendError> {
            backend_user(
                self.client
                    .matchmaking()
                    .lobby_owner(steamworks::LobbyId::from_raw(lobby.get())),
            )
        }

        fn lobby_members(
            &self,
            lobby: SteamLobbyId,
        ) -> Result<Vec<SteamUserId>, SteamBackendError> {
            let matchmaking = self.client.matchmaking();
            let steam_lobby = steamworks::LobbyId::from_raw(lobby.get());
            if matchmaking.lobby_member_count(steam_lobby) > MAX_STEAM_LOBBY_MEMBERS {
                return Err(SteamBackendError::CapacityExceeded);
            }
            let members = matchmaking.lobby_members(steam_lobby);
            if members.len() > MAX_STEAM_LOBBY_MEMBERS {
                return Err(SteamBackendError::CapacityExceeded);
            }
            members.into_iter().map(backend_user).collect()
        }

        fn is_friend(&self, user: SteamUserId) -> Result<bool, SteamBackendError> {
            Ok(self
                .client
                .friends()
                .get_friend(steamworks::SteamId::from_raw(user.get()))
                .has_friend(steamworks::FriendFlags::IMMEDIATE))
        }

        fn open_invite_overlay(&mut self, lobby: SteamLobbyId) -> Result<(), SteamBackendError> {
            self.client
                .friends()
                .activate_invite_dialog(steamworks::LobbyId::from_raw(lobby.get()));
            Ok(())
        }

        fn launch_command_line(&self) -> Result<String, SteamBackendError> {
            Ok(self.client.apps().launch_command_line())
        }

        fn clear_rich_presence(&mut self) {
            self.client.friends().clear_rich_presence();
        }

        fn set_rich_presence(
            &mut self,
            key: &'static str,
            value: Option<&str>,
        ) -> Result<(), SteamBackendError> {
            validate_real_string(key, 64)?;
            if let Some(value) = value {
                validate_real_string(value, 256)?;
            }
            if self.client.friends().set_rich_presence(key, value) {
                Ok(())
            } else {
                Err(SteamBackendError::OperationFailed)
            }
        }

        fn issue_auth_ticket(
            &mut self,
            remote_user: SteamUserId,
        ) -> Result<BackendIssuedAuthTicket, SteamBackendError> {
            let platform_handle = self.next_platform_ticket_handle()?;
            let (steam_handle, bytes) = self
                .client
                .user()
                .authentication_session_ticket_with_steam_id(steamworks::SteamId::from_raw(
                    remote_user.get(),
                ));
            if bytes.is_empty() {
                self.client
                    .user()
                    .cancel_authentication_ticket(steam_handle);
                return Err(SteamBackendError::AuthenticationFailed);
            }
            let mut tickets = self
                .tickets
                .lock()
                .map_err(|_| SteamBackendError::IntegrityFailure)?;
            if tickets.len() >= MAX_STEAM_LOBBY_MEMBERS {
                self.client
                    .user()
                    .cancel_authentication_ticket(steam_handle);
                return Err(SteamBackendError::CapacityExceeded);
            }
            tickets.push(RealTicketRecord {
                platform_handle,
                steam_handle,
            });
            Ok(BackendIssuedAuthTicket {
                handle: platform_handle,
                bytes,
            })
        }

        fn cancel_auth_ticket(&mut self, handle: AuthTicketHandle) {
            let record = self.tickets.lock().ok().and_then(|mut tickets| {
                tickets
                    .iter()
                    .position(|record| record.platform_handle == handle)
                    .map(|index| tickets.swap_remove(index))
            });
            if let Some(record) = record {
                self.client
                    .user()
                    .cancel_authentication_ticket(record.steam_handle);
            }
            match self.callback_mailbox.lock() {
                Ok(mut mailbox) => mailbox.retire_auth_ticket(handle),
                Err(_) => self
                    .callback_integrity_failure
                    .store(true, Ordering::Release),
            }
        }

        fn begin_auth_session(
            &mut self,
            user: SteamUserId,
            ticket: &[u8],
        ) -> Result<(), SteamBackendError> {
            self.callback_mailbox
                .lock()
                .map_err(|_| SteamBackendError::IntegrityFailure)?
                .begin_auth_session(user)?;
            let result = self
                .client
                .user()
                .begin_authentication_session(steamworks::SteamId::from_raw(user.get()), ticket)
                .map_err(|_| SteamBackendError::AuthenticationFailed);
            if result.is_err() {
                match self.callback_mailbox.lock() {
                    Ok(mut mailbox) => mailbox.abort_auth_session_start(user),
                    Err(_) => self
                        .callback_integrity_failure
                        .store(true, Ordering::Release),
                }
            }
            result
        }

        fn end_auth_session(&mut self, user: SteamUserId) {
            self.client
                .user()
                .end_authentication_session(steamworks::SteamId::from_raw(user.get()));
            match self.callback_mailbox.lock() {
                Ok(mut mailbox) => mailbox.retire_auth_session(user),
                Err(_) => self
                    .callback_integrity_failure
                    .store(true, Ordering::Release),
            }
        }

        fn license_status(
            &self,
            user: SteamUserId,
            app_id: SteamAppId,
        ) -> Result<LicenseStatus, SteamBackendError> {
            Ok(
                match self.client.user().user_has_license_for_app(
                    steamworks::SteamId::from_raw(user.get()),
                    steamworks::AppId(app_id.get()),
                ) {
                    steamworks::UserHasLicense::HasLicense => LicenseStatus::HasLicense,
                    steamworks::UserHasLicense::DoesNotHaveLicense => {
                        LicenseStatus::DoesNotHaveLicense
                    }
                    steamworks::UserHasLicense::NoAuth => LicenseStatus::NoAuthentication,
                },
            )
        }
    }

    impl RealSteamBackend {
        fn refresh_steam_input(&mut self) {
            let input = self.client.input();
            input.run_frame();
            let mut connected = [0_u64; MAX_STEAM_INPUT_DISCOVERED_CONTROLLERS];
            let connected_len = input
                .get_connected_controllers_slice(&mut connected)
                .min(connected.len());
            self.steam_input
                .assignments
                .reconcile(&connected[..connected_len]);

            let mut snapshot = SteamInputSnapshot::default();
            for (local_ordinal, controller) in
                self.steam_input.assignments.handles.iter().enumerate()
            {
                let Some(controller) = *controller else {
                    continue;
                };
                let raw = controller.get();
                let action_set_handle = match self.steam_input.desired_action_set {
                    SteamInputActionSet::Gameplay => self.steam_input.gameplay_action_set,
                    SteamInputActionSet::Menu => self.steam_input.menu_action_set,
                };
                input.activate_action_set_handle(raw, action_set_handle);
                let analog = input.get_analog_action_data(raw, self.steam_input.movement_action);
                let movement = if analog.bActive && analog.x.is_finite() && analog.y.is_finite() {
                    // Steam's joystick convention is +Y up; the game binding
                    // layer uses -Y for screen/world-forward before applying
                    // the gameplay camera rotation.
                    QuantizedMovement::from_unit_axes(analog.x, -analog.y)
                } else {
                    QuantizedMovement::ZERO
                };

                let mut gameplay_held = InputMask::NONE;
                for (button, action) in RawInputButton::ALL
                    .into_iter()
                    .zip(self.steam_input.gameplay_actions)
                {
                    let data = input.get_digital_action_data(raw, action);
                    if data.bActive && data.bState {
                        gameplay_held.insert(button.mask());
                    }
                }

                let mut menu_held = SteamMenuInputMask::NONE;
                for (action, handle) in SteamMenuAction::ALL
                    .into_iter()
                    .zip(self.steam_input.menu_actions)
                {
                    let data = input.get_digital_action_data(raw, handle);
                    if data.bActive && data.bState {
                        menu_held.insert(action);
                    }
                }
                snapshot.controllers[local_ordinal] = SteamInputControllerSnapshot {
                    controller_id: Some(controller),
                    device_kind: map_steam_input_type(input.get_input_type_for_handle(raw)),
                    movement,
                    gameplay_held,
                    menu_held,
                };
            }
            self.steam_input.snapshot = snapshot;
        }
    }

    impl Drop for RealSteamBackend {
        fn drop(&mut self) {
            self.callback_owner_alive.store(false, Ordering::Release);
            if let Ok(mut mailbox) = self.callback_mailbox.lock() {
                for operation in mailbox.operations.drain(..) {
                    let lobby = match operation.event {
                        Some(SteamBackendEvent::LobbyCreated {
                            result: Ok(lobby), ..
                        })
                        | Some(SteamBackendEvent::LobbyJoined {
                            result: Ok(lobby), ..
                        }) => Some(lobby),
                        _ => None,
                    };
                    if let Some(lobby) = lobby {
                        self.client
                            .matchmaking()
                            .leave_lobby(steamworks::LobbyId::from_raw(lobby.get()));
                    }
                }
            }
            if let Ok(mut tickets) = self.tickets.lock() {
                for record in tickets.drain(..) {
                    self.client
                        .user()
                        .cancel_authentication_ticket(record.steam_handle);
                }
            }
            self.client.friends().clear_rich_presence();
            self.client.input().shutdown();
        }
    }

    fn resolve_steam_input_manifest() -> Result<PathBuf, SteamBackendError> {
        if let Some(configured) = std::env::var_os(STEAM_INPUT_MANIFEST_ENV) {
            let configured = PathBuf::from(configured);
            return configured
                .is_file()
                .then_some(configured)
                .ok_or(SteamBackendError::SteamInputManifestInvalid);
        }

        let executable_directory = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(Path::to_path_buf));
        let candidates = [
            executable_directory
                .as_ref()
                .map(|directory| directory.join(STEAM_INPUT_MANIFEST_RELATIVE_PATH)),
            executable_directory.as_ref().map(|directory| {
                directory
                    .join("assets")
                    .join(STEAM_INPUT_MANIFEST_RELATIVE_PATH)
            }),
            executable_directory.as_ref().map(|directory| {
                directory
                    .parent()
                    .unwrap_or(directory)
                    .join("Resources")
                    .join(STEAM_INPUT_MANIFEST_RELATIVE_PATH)
            }),
            Some(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("assets")
                    .join(STEAM_INPUT_MANIFEST_RELATIVE_PATH),
            ),
        ];
        candidates
            .into_iter()
            .flatten()
            .find(|candidate| candidate.is_file())
            .ok_or(SteamBackendError::SteamInputManifestInvalid)
    }

    pub(super) fn validate_steam_input_manifest(path: &Path) -> Result<(), SteamBackendError> {
        let metadata =
            std::fs::metadata(path).map_err(|_| SteamBackendError::SteamInputManifestInvalid)?;
        if !metadata.is_file()
            || metadata.len() == 0
            || metadata.len() > MAX_STEAM_INPUT_MANIFEST_BYTES
        {
            return Err(SteamBackendError::SteamInputManifestInvalid);
        }
        let text = std::fs::read_to_string(path)
            .map_err(|_| SteamBackendError::SteamInputManifestInvalid)?;
        validate_vdf_structure(&text)?;
        if text.bytes().any(|byte| byte == 0)
            || !text.contains("\"Action Manifest\"")
            || !text.contains("\"configurations\"")
            || !text.contains("\"actions\"")
            || !text.contains("\"localization\"")
        {
            return Err(SteamBackendError::SteamInputManifestInvalid);
        }
        for required in [
            STEAM_INPUT_GAMEPLAY_ACTION_SET,
            STEAM_INPUT_MENU_ACTION_SET,
            STEAM_INPUT_MOVE_ACTION,
        ]
        .into_iter()
        .chain(STEAM_INPUT_GAMEPLAY_ACTIONS)
        .chain(STEAM_INPUT_MENU_ACTIONS)
        {
            let quoted = format!("\"{required}\"");
            if !text.contains(&quoted) {
                return Err(SteamBackendError::SteamInputManifestInvalid);
            }
        }
        let directory = path
            .parent()
            .ok_or(SteamBackendError::SteamInputManifestInvalid)?;
        for configuration in STEAM_INPUT_CONFIGURATION_FILES {
            if !text.contains(&format!("\"{configuration}\"")) {
                return Err(SteamBackendError::SteamInputManifestInvalid);
            }
            validate_steam_input_configuration(&directory.join(configuration))?;
        }
        Ok(())
    }

    fn validate_steam_input_configuration(path: &Path) -> Result<(), SteamBackendError> {
        let metadata =
            std::fs::metadata(path).map_err(|_| SteamBackendError::SteamInputManifestInvalid)?;
        if !metadata.is_file()
            || metadata.len() == 0
            || metadata.len() > MAX_STEAM_INPUT_MANIFEST_BYTES
        {
            return Err(SteamBackendError::SteamInputManifestInvalid);
        }
        let text = std::fs::read_to_string(path)
            .map_err(|_| SteamBackendError::SteamInputManifestInvalid)?;
        validate_vdf_structure(&text)?;
        for required in [
            "\"controller_mappings\"",
            "\"controller_type\"",
            "\"Gameplay\"",
            "\"Menu\"",
            "\"gameactions\"",
            "\"Move\"",
            "game_action Gameplay Jump",
            "game_action Menu MenuAccept",
            "game_action Menu MenuBack",
            "game_action Menu MenuBindings",
        ] {
            if !text.contains(required) {
                return Err(SteamBackendError::SteamInputManifestInvalid);
            }
        }
        Ok(())
    }

    pub(super) fn validate_vdf_structure(text: &str) -> Result<(), SteamBackendError> {
        let bytes = text.as_bytes();
        let mut index = 0;
        let mut depth = 0_u16;
        let mut saw_root = false;
        let mut in_string = false;
        let mut escaped = false;
        let mut line_comment = false;
        while index < bytes.len() {
            let byte = bytes[index];
            if line_comment {
                if byte == b'\n' {
                    line_comment = false;
                }
                index += 1;
                continue;
            }
            if in_string {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    in_string = false;
                } else if byte == 0 || (byte < b' ' && !matches!(byte, b'\t' | b'\n' | b'\r')) {
                    return Err(SteamBackendError::SteamInputManifestInvalid);
                }
                index += 1;
                continue;
            }
            if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
                line_comment = true;
                index += 2;
                continue;
            }
            match byte {
                b'"' => in_string = true,
                b'{' => {
                    saw_root = true;
                    depth = depth
                        .checked_add(1)
                        .ok_or(SteamBackendError::SteamInputManifestInvalid)?;
                }
                b'}' => {
                    depth = depth
                        .checked_sub(1)
                        .ok_or(SteamBackendError::SteamInputManifestInvalid)?;
                }
                0 => return Err(SteamBackendError::SteamInputManifestInvalid),
                control if control < b' ' && !matches!(control, b'\t' | b'\n' | b'\r') => {
                    return Err(SteamBackendError::SteamInputManifestInvalid);
                }
                _ => {}
            }
            index += 1;
        }
        if !saw_root || depth != 0 || in_string || escaped {
            return Err(SteamBackendError::SteamInputManifestInvalid);
        }
        Ok(())
    }

    fn map_steam_input_type(value: steamworks::InputType) -> SteamInputDeviceKind {
        match value {
            steamworks::InputType::Unknown => SteamInputDeviceKind::Unknown,
            steamworks::InputType::SteamController => SteamInputDeviceKind::SteamController,
            steamworks::InputType::XBox360Controller => SteamInputDeviceKind::Xbox360,
            steamworks::InputType::XBoxOneController => SteamInputDeviceKind::XboxOne,
            steamworks::InputType::GenericGamepad => SteamInputDeviceKind::GenericGamepad,
            steamworks::InputType::PS4Controller => SteamInputDeviceKind::PlayStation4,
            steamworks::InputType::AppleMFiController => SteamInputDeviceKind::AppleMfi,
            steamworks::InputType::AndroidController => SteamInputDeviceKind::Android,
            steamworks::InputType::SwitchJoyConPair => SteamInputDeviceKind::SwitchJoyConPair,
            steamworks::InputType::SwitchJoyConSingle => SteamInputDeviceKind::SwitchJoyConSingle,
            steamworks::InputType::SwitchProController => SteamInputDeviceKind::SwitchPro,
            steamworks::InputType::MobileTouch => SteamInputDeviceKind::MobileTouch,
            steamworks::InputType::PS3Controller => SteamInputDeviceKind::PlayStation3,
            steamworks::InputType::PS5Controller => SteamInputDeviceKind::PlayStation5,
            steamworks::InputType::SteamDeckController => SteamInputDeviceKind::SteamDeck,
        }
    }

    fn backend_lobby(value: steamworks::LobbyId) -> Result<SteamLobbyId, SteamBackendError> {
        SteamLobbyId::new(value.raw()).map_err(|_| SteamBackendError::InvalidData)
    }

    pub(super) fn translate_lobby_data_update(
        event: steamworks::LobbyDataUpdate,
    ) -> Result<SteamBackendEvent, SteamBackendError> {
        // `success == false` is still attributable to this lobby/subject. Keep
        // it as a bounded dirty signal and let the platform re-read its
        // authoritative cache; callback delivery failure is not an
        // identityless process-integrity failure.
        let lobby = backend_lobby(event.lobby)?;
        let subject = if event.member.raw() == event.lobby.raw() {
            LobbyDataSubject::Lobby
        } else {
            LobbyDataSubject::Member(backend_user(event.member)?)
        };
        Ok(SteamBackendEvent::LobbyDataChanged { lobby, subject })
    }

    pub(super) fn translate_auth_validation(
        event: steamworks::ValidateAuthTicketResponse,
    ) -> Result<SteamBackendEvent, SteamBackendError> {
        Ok(SteamBackendEvent::AuthSessionValidated {
            // `steam_id` is the ticket provider and therefore the identity
            // bound to BeginAuthSession. `owner_steam_id` is only the app's
            // license owner and may differ for Steam Families borrowing.
            user: backend_user(event.steam_id)?,
            license_owner_user: backend_user(event.owner_steam_id)?,
            result: event.response.map_err(map_auth_validation_error),
        })
    }

    fn backend_user(value: steamworks::SteamId) -> Result<SteamUserId, SteamBackendError> {
        if value.is_invalid() {
            return Err(SteamBackendError::InvalidData);
        }
        SteamUserId::new(value.raw()).map_err(|_| SteamBackendError::InvalidData)
    }

    fn optional_backend_user(value: steamworks::SteamId) -> Option<SteamUserId> {
        if value.is_invalid() {
            None
        } else {
            SteamUserId::new(value.raw()).ok()
        }
    }

    fn map_auth_validation_error(
        value: steamworks::AuthSessionValidateError,
    ) -> AuthValidationFailure {
        match value {
            steamworks::AuthSessionValidateError::UserNotConnectedToSteam => {
                AuthValidationFailure::UserNotConnected
            }
            steamworks::AuthSessionValidateError::NoLicenseOrExpired => {
                AuthValidationFailure::NoLicenseOrExpired
            }
            steamworks::AuthSessionValidateError::VACBanned => AuthValidationFailure::VacBanned,
            steamworks::AuthSessionValidateError::LoggedInElseWhere => {
                AuthValidationFailure::LoggedInElsewhere
            }
            steamworks::AuthSessionValidateError::VACCheckTimedOut => {
                AuthValidationFailure::VacCheckTimedOut
            }
            steamworks::AuthSessionValidateError::AuthTicketCancelled => {
                AuthValidationFailure::TicketCancelled
            }
            steamworks::AuthSessionValidateError::AuthTicketInvalidAlreadyUsed => {
                AuthValidationFailure::TicketAlreadyUsed
            }
            steamworks::AuthSessionValidateError::AuthTicketInvalid => {
                AuthValidationFailure::TicketInvalid
            }
            steamworks::AuthSessionValidateError::PublisherIssuedBan => {
                AuthValidationFailure::PublisherBan
            }
        }
    }

    fn validate_real_string(value: &str, maximum: usize) -> Result<(), SteamBackendError> {
        if value.is_empty() || value.len() > maximum || value.bytes().any(|byte| byte == 0) {
            Err(SteamBackendError::InvalidData)
        } else {
            Ok(())
        }
    }

    #[cfg(test)]
    mod callback_mailbox_tests {
        use super::*;

        fn user(value: u64) -> SteamUserId {
            SteamUserId::new(value).unwrap()
        }

        fn lobby(value: u64) -> SteamLobbyId {
            SteamLobbyId::new(value).unwrap()
        }

        #[test]
        fn callback_chatter_coalesces_while_auth_and_ticket_failures_dominate() {
            let local = user(91_001);
            let remote = user(91_002);
            let active_lobby = lobby(92_001);
            let mut mailbox = RealCallbackMailbox::new(local);
            mailbox.set_lobby_scope(Some(active_lobby));
            mailbox.begin_auth_session(remote).unwrap();

            for index in 0..10_000_u64 {
                mailbox.enqueue(SteamBackendEvent::LobbyMembershipChanged {
                    lobby: active_lobby,
                    user: remote,
                    change: LobbyMembershipChange::Entered,
                });
                mailbox.enqueue(SteamBackendEvent::LobbyDataChanged {
                    lobby: active_lobby,
                    subject: LobbyDataSubject::Lobby,
                });
                mailbox.enqueue(SteamBackendEvent::LobbyDataChanged {
                    lobby: active_lobby,
                    subject: LobbyDataSubject::Member(remote),
                });
                mailbox.enqueue(SteamBackendEvent::LobbyJoinRequested {
                    lobby: lobby(100_000 + index),
                    friend: Some(remote),
                });
            }
            mailbox.enqueue(SteamBackendEvent::AuthSessionValidated {
                user: remote,
                license_owner_user: remote,
                result: Ok(()),
            });
            mailbox.enqueue(SteamBackendEvent::AuthSessionValidated {
                user: remote,
                license_owner_user: remote,
                result: Err(AuthValidationFailure::TicketCancelled),
            });
            mailbox.enqueue(SteamBackendEvent::AuthSessionValidated {
                user: remote,
                license_owner_user: remote,
                result: Ok(()),
            });
            let handle = AuthTicketHandle(7);
            mailbox.enqueue(SteamBackendEvent::AuthTicketReady {
                handle,
                success: true,
            });
            mailbox.enqueue(SteamBackendEvent::AuthTicketReady {
                handle,
                success: false,
            });
            mailbox.enqueue(SteamBackendEvent::AuthTicketReady {
                handle,
                success: true,
            });

            assert!(mailbox.pending_event_count() <= 8);
            assert_eq!(
                mailbox.pop_event(),
                Some(SteamBackendEvent::AuthSessionValidated {
                    user: remote,
                    license_owner_user: remote,
                    result: Err(AuthValidationFailure::TicketCancelled),
                })
            );
            assert_eq!(
                mailbox.pop_event(),
                Some(SteamBackendEvent::AuthTicketReady {
                    handle,
                    success: false,
                })
            );
            assert!(!matches!(
                mailbox.pop_event(),
                Some(SteamBackendEvent::IntegrityFailure)
            ));
        }

        #[test]
        fn operation_completions_drain_before_sticky_terminal_callbacks() {
            let mut mailbox = RealCallbackMailbox::new(user(91_011));
            let first = SteamOperationId(1);
            let second = SteamOperationId(2);
            mailbox.register_operation(first).unwrap();
            mailbox.register_operation(second).unwrap();
            mailbox.enqueue(SteamBackendEvent::LobbyCreated {
                operation_id: first,
                result: Ok(lobby(92_011)),
            });
            mailbox.enqueue(SteamBackendEvent::LobbyJoined {
                operation_id: second,
                requested: lobby(92_012),
                result: Ok(lobby(92_012)),
            });
            mailbox.enqueue(SteamBackendEvent::SteamDisconnected);
            mailbox.enqueue(SteamBackendEvent::IntegrityFailure);

            assert!(matches!(
                mailbox.pop_event(),
                Some(SteamBackendEvent::LobbyJoined {
                    operation_id,
                    ..
                }) if operation_id == second
            ));
            assert!(matches!(
                mailbox.pop_event(),
                Some(SteamBackendEvent::LobbyCreated {
                    operation_id,
                    ..
                }) if operation_id == first
            ));
            assert_eq!(
                mailbox.pop_event(),
                Some(SteamBackendEvent::IntegrityFailure)
            );
        }

        #[test]
        fn canceled_operations_release_capacity_and_delayed_successes_are_cleanup_only() {
            let mut mailbox = RealCallbackMailbox::new(user(91_016));
            for value in 1..=10_u64 {
                let operation_id = SteamOperationId(value);
                mailbox.register_operation(operation_id).unwrap();
                assert_eq!(mailbox.retire_operation(operation_id), Ok(None));
            }

            let completed_before_cancel = SteamOperationId(11);
            let completed_lobby = lobby(92_111);
            mailbox.register_operation(completed_before_cancel).unwrap();
            assert_eq!(
                mailbox.complete_operation(SteamBackendEvent::LobbyCreated {
                    operation_id: completed_before_cancel,
                    result: Ok(completed_lobby),
                }),
                None
            );
            assert_eq!(
                mailbox.retire_operation(completed_before_cancel),
                Ok(Some(completed_lobby))
            );

            let live = SteamOperationId(12);
            mailbox.register_operation(live).unwrap();
            for value in 1..=11_u64 {
                let stale_lobby = lobby(92_100 + value);
                assert_eq!(
                    mailbox.complete_operation(SteamBackendEvent::LobbyJoined {
                        operation_id: SteamOperationId(value),
                        requested: stale_lobby,
                        result: Ok(stale_lobby),
                    }),
                    Some(stale_lobby)
                );
            }
            let live_lobby = lobby(92_112);
            assert_eq!(
                mailbox.complete_operation(SteamBackendEvent::LobbyJoined {
                    operation_id: live,
                    requested: live_lobby,
                    result: Ok(live_lobby),
                }),
                None
            );
            mailbox.enqueue(SteamBackendEvent::SteamDisconnected);
            assert!(matches!(
                mailbox.pop_event(),
                Some(SteamBackendEvent::LobbyJoined {
                    operation_id,
                    ..
                }) if operation_id == live
            ));
            assert_eq!(
                mailbox.pop_event(),
                Some(SteamBackendEvent::SteamDisconnected)
            );
        }

        #[test]
        fn departed_auth_generation_is_a_barrier_and_pressure_resyncs_without_faulting() {
            let local = user(91_021);
            let remote = user(91_022);
            let active_lobby = lobby(92_021);
            let mut mailbox = RealCallbackMailbox::new(local);
            mailbox.set_lobby_scope(Some(active_lobby));
            mailbox.begin_auth_session(remote).unwrap();
            mailbox.retire_auth_session(remote);
            assert_eq!(
                mailbox.begin_auth_session(remote),
                Err(SteamBackendError::AuthenticationFailed)
            );
            mailbox.enqueue(SteamBackendEvent::AuthSessionValidated {
                user: remote,
                license_owner_user: remote,
                result: Ok(()),
            });
            mailbox.begin_auth_session(remote).unwrap();

            for offset in 0..=MAX_STEAM_LOBBY_MEMBERS {
                mailbox.enqueue(SteamBackendEvent::LobbyMembershipChanged {
                    lobby: active_lobby,
                    user: user(91_100 + offset as u64),
                    change: LobbyMembershipChange::Left,
                });
            }
            assert_eq!(
                mailbox.pop_event(),
                Some(SteamBackendEvent::LobbyMembershipResync {
                    lobby: active_lobby,
                })
            );
            assert!(!matches!(
                mailbox.pop_event(),
                Some(SteamBackendEvent::IntegrityFailure)
            ));
        }
    }

    impl SteamPlatform<RealSteamBackend> {
        pub fn initialize_steam_client(
            config: SteamClientConfig,
            now_ms: u64,
        ) -> Result<Self, SteamPlatformError> {
            config.validate()?;
            let backend = RealSteamBackend::initialize(config.app_id)?;
            SteamPlatform::new(config, backend, now_ms)
        }

        pub(crate) fn steam_transport_client_access(
            &self,
        ) -> (
            steamworks::Client,
            Arc<AtomicBool>,
            Arc<RealClientOwnershipGuard>,
        ) {
            (
                self.backend.client.clone(),
                self.backend.callback_owner_alive.clone(),
                self.backend.ownership.clone(),
            )
        }
    }
}

#[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
pub(crate) use real::RealClientOwnershipGuard;
#[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
pub use real::RealSteamBackend;

#[cfg(test)]
mod tests {
    use super::*;

    const NOW_MS: u64 = 10_000;

    fn app_id() -> SteamAppId {
        SteamAppId::new(SPACEWAR_APP_ID).unwrap()
    }

    fn user(value: u64) -> SteamUserId {
        SteamUserId::new(value).unwrap()
    }

    fn lobby(value: u64) -> SteamLobbyId {
        SteamLobbyId::new(value).unwrap()
    }

    fn config() -> SteamClientConfig {
        SteamClientConfig::development(app_id(), true)
    }

    #[test]
    fn auth_ticket_debug_is_redacted_and_zeroization_overwrites_secret_bytes() {
        let issued = IssuedAuthTicket {
            handle: AuthTicketHandle(71),
            remote_user: user(72),
            bytes: vec![17, 34, 51, 68],
        };
        let issued_debug = format!("{issued:?}");
        assert!(issued_debug.contains("<redacted>"));
        assert!(issued_debug.contains("ticket_len: 4"));
        assert!(!issued_debug.contains("[17, 34, 51, 68]"));

        let backend = BackendIssuedAuthTicket {
            handle: AuthTicketHandle(73),
            bytes: vec![85, 102, 119, 136],
        };
        let backend_debug = format!("{backend:?}");
        assert!(backend_debug.contains("<redacted>"));
        assert!(backend_debug.contains("ticket_len: 4"));
        assert!(!backend_debug.contains("[85, 102, 119, 136]"));

        let mut secret = vec![0xA5; MAX_STEAM_AUTH_TICKET_BYTES];
        zeroize_ticket_bytes(&mut secret);
        assert!(secret.iter().all(|byte| *byte == 0));
    }

    #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
    #[test]
    fn bundled_production_steam_input_manifest_and_configurations_validate() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("steam_input")
            .join("action_manifest.vdf");
        real::validate_steam_input_manifest(&path).unwrap();
        assert_eq!(
            real::validate_vdf_structure("\"Action Manifest\" { \"actions\" { }"),
            Err(SteamBackendError::SteamInputManifestInvalid)
        );
        assert_eq!(
            real::validate_vdf_structure("\"Action Manifest { }"),
            Err(SteamBackendError::SteamInputManifestInvalid)
        );
    }

    #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
    #[test]
    fn real_auth_callback_keeps_ticket_user_distinct_from_license_owner() {
        let ticket_user_raw = 76_561_198_000_000_001;
        let family_owner_raw = 76_561_198_000_000_002;
        let ticket_user = SteamUserId::new(ticket_user_raw).unwrap();

        for license_owner_raw in [ticket_user_raw, family_owner_raw] {
            let translated =
                real::translate_auth_validation(steamworks::ValidateAuthTicketResponse {
                    steam_id: steamworks::SteamId::from_raw(ticket_user_raw),
                    response: Ok(()),
                    owner_steam_id: steamworks::SteamId::from_raw(license_owner_raw),
                })
                .unwrap();
            assert_eq!(
                translated,
                SteamBackendEvent::AuthSessionValidated {
                    user: ticket_user,
                    license_owner_user: SteamUserId::new(license_owner_raw).unwrap(),
                    result: Ok(()),
                }
            );
        }
    }

    #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
    #[test]
    fn real_overlay_activity_callback_is_latest_value_not_fifo() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let active = AtomicBool::new(false);
        for value in [true, false, true, true, false] {
            real::coalesce_overlay_activity(
                &active,
                steamworks::GameOverlayActivated { active: value },
            );
        }
        assert!(!active.load(Ordering::Acquire));
    }

    #[test]
    fn steam_input_assignments_survive_callback_order_changes() {
        let mut assignments = SteamInputAssignments::default();
        assignments.reconcile(&[91, 27, 63]);
        assert_eq!(
            assignments.handles,
            [
                SteamInputControllerId::new(27),
                SteamInputControllerId::new(63),
                SteamInputControllerId::new(91),
                None,
            ]
        );

        assignments.reconcile(&[63, 91, 27]);
        assert_eq!(
            assignments.handles,
            [
                SteamInputControllerId::new(27),
                SteamInputControllerId::new(63),
                SteamInputControllerId::new(91),
                None,
            ]
        );

        // A disconnected ordinal becomes available, but surviving devices do
        // not move merely because a new lower-valued handle appears.
        assignments.reconcile(&[91, 11, 27]);
        assert_eq!(assignments.handles[0], SteamInputControllerId::new(27));
        assert_eq!(assignments.handles[1], SteamInputControllerId::new(11));
        assert_eq!(assignments.handles[2], SteamInputControllerId::new(91));
    }

    #[test]
    fn steam_input_binding_panel_checks_target_then_overlay_and_preserves_exact_ordinal() {
        let (mut platform, control) = platform(user(7));
        let mut menu = SteamMenuInputMask::NONE;
        menu.insert(SteamMenuAction::Accept);
        menu.insert(SteamMenuAction::OpenBindings);
        let injected = SteamInputControllerSnapshot {
            controller_id: SteamInputControllerId::new(400),
            device_kind: SteamInputDeviceKind::SteamDeck,
            movement: QuantizedMovement::new(64, -127),
            gameplay_held: InputMask::RIGHT | InputMask::JUMP,
            menu_held: menu,
        };
        control
            .set_steam_input_controller(2, Some(injected))
            .unwrap();

        assert_eq!(
            platform.steam_input_snapshot().controller(2),
            Some(injected)
        );
        assert_eq!(platform.steam_input_snapshot().controller(0), None);
        platform
            .set_steam_input_action_set(SteamInputActionSet::Gameplay)
            .unwrap();
        assert_eq!(
            control.steam_input_action_set(),
            Some(SteamInputActionSet::Gameplay)
        );

        assert_eq!(
            platform.show_steam_input_binding_panel(2).unwrap(),
            SteamOverlayRequestStatus::Unavailable
        );
        assert_eq!(control.steam_input_binding_panel_open_count(), 0);
        assert_eq!(control.overlay_enabled_query_count(), 1);

        control.set_overlay_enabled(true).unwrap();
        assert_eq!(
            platform.show_steam_input_binding_panel(2).unwrap(),
            SteamOverlayRequestStatus::Submitted
        );
        assert_eq!(control.steam_input_binding_panel_open_count(), 1);
        assert_eq!(control.last_steam_input_binding_ordinal(), Some(2));
        let readiness_queries = control.overlay_enabled_query_count();
        assert!(platform.show_steam_input_binding_panel(0).is_err());
        assert_eq!(
            control.overlay_enabled_query_count(),
            readiness_queries,
            "a disconnected target is rejected before overlay readiness"
        );
        assert!(
            platform
                .show_steam_input_binding_panel(MAX_STEAM_INPUT_CONTROLLERS)
                .is_err()
        );
        assert_eq!(control.overlay_enabled_query_count(), readiness_queries);

        assert!(!platform.is_overlay_active());
        control.set_overlay_active(true).unwrap();
        assert!(platform.is_overlay_active());
        let pumps = control.callback_pump_count();
        platform.pump(NOW_MS + 1).unwrap();
        assert_eq!(control.callback_pump_count(), pumps + 1);
    }

    #[cfg(all(feature = "steam-net", not(target_arch = "wasm32")))]
    #[test]
    fn real_lobby_data_update_preserves_lobby_and_member_subjects() {
        let raw_lobby = 77_u64;
        let lobby_update = steamworks::LobbyDataUpdate {
            lobby: steamworks::LobbyId::from_raw(raw_lobby),
            member: steamworks::SteamId::from_raw(raw_lobby),
            success: true,
        };
        assert_eq!(
            real::translate_lobby_data_update(lobby_update).unwrap(),
            SteamBackendEvent::LobbyDataChanged {
                lobby: lobby(raw_lobby),
                subject: LobbyDataSubject::Lobby,
            }
        );

        let raw_user = 76_561_197_960_287_930_u64;
        let member_update = steamworks::LobbyDataUpdate {
            lobby: steamworks::LobbyId::from_raw(raw_lobby),
            member: steamworks::SteamId::from_raw(raw_user),
            success: true,
        };
        assert_eq!(
            real::translate_lobby_data_update(member_update).unwrap(),
            SteamBackendEvent::LobbyDataChanged {
                lobby: lobby(raw_lobby),
                subject: LobbyDataSubject::Member(user(raw_user)),
            }
        );
        assert_eq!(
            real::translate_lobby_data_update(steamworks::LobbyDataUpdate {
                lobby: steamworks::LobbyId::from_raw(raw_lobby),
                member: steamworks::SteamId::from_raw(raw_user),
                success: false,
            })
            .unwrap(),
            SteamBackendEvent::LobbyDataChanged {
                lobby: lobby(raw_lobby),
                subject: LobbyDataSubject::Member(user(raw_user)),
            }
        );
        assert!(
            real::translate_lobby_data_update(steamworks::LobbyDataUpdate {
                lobby: steamworks::LobbyId::from_raw(raw_lobby),
                member: steamworks::SteamId::from_raw(0),
                success: true,
            })
            .is_err()
        );
    }

    fn metadata(visibility: LobbyVisibility, seats: u8) -> LobbyMetadata {
        LobbyMetadata::current(
            AuthorityKind::Listen,
            visibility,
            RegionCode::new("kr-seoul").unwrap(),
            DefinitionId::new(0).unwrap(),
            DefinitionId::new(1).unwrap(),
            seats,
        )
        .unwrap()
    }

    fn platform(local_user: SteamUserId) -> (SteamPlatform<FakeSteamBackend>, FakeSteamControl) {
        let (backend, control) = FakeSteamBackend::new(app_id(), local_user);
        (
            SteamPlatform::new(config(), backend, NOW_MS).unwrap(),
            control,
        )
    }

    fn member_declaration(revision: u8, seats: u8) -> MemberLoadoutDeclaration {
        let mut encoded = format!("01{revision:02x}00{seats:02x}");
        for index in 0..seats {
            // team + character/style/equipment IDs, all encoded little-endian.
            encoded.push_str(&format!("{index:02x}000000000000"));
        }
        MemberLoadoutDeclaration::new(&encoded).unwrap()
    }

    fn member_declaration_variant(revision: u8, seats: u8) -> MemberLoadoutDeclaration {
        let mut encoded = member_declaration(revision, seats).as_str().to_owned();
        let final_digit = encoded
            .pop()
            .expect("a declaration always has encoded seat content");
        encoded.push(if final_digit == '0' { '1' } else { '0' });
        MemberLoadoutDeclaration::new(&encoded).unwrap()
    }

    fn set_raw_member_declaration(
        control: &FakeSteamControl,
        lobby: SteamLobbyId,
        user: SteamUserId,
        marker: MemberCommitMarker,
        declaration: MemberLoadoutDeclaration,
    ) {
        control
            .set_member_data_raw(
                lobby,
                user,
                MEMBER_KEY_READY,
                &encode_member_commit_marker(marker),
            )
            .unwrap();
        control
            .set_member_data_raw(
                lobby,
                user,
                MEMBER_KEY_SEATS,
                &declaration.seat_count().to_string(),
            )
            .unwrap();
        control
            .set_member_data_raw(lobby, user, MEMBER_KEY_LOADOUT, declaration.as_str())
            .unwrap();
    }

    fn create_test_host(
        local: SteamUserId,
        local_seats: u8,
        capacity: u8,
    ) -> (
        SteamPlatform<FakeSteamBackend>,
        FakeSteamControl,
        SteamLobbyId,
    ) {
        let (mut platform, control) = platform(local);
        platform
            .create_lobby(
                LobbyCreateRequest {
                    visibility: LobbyVisibility::Private,
                    maximum_peers: MAX_STEAM_LOBBY_MEMBERS as u8,
                    local_seats,
                },
                metadata(LobbyVisibility::Private, capacity),
            )
            .unwrap();
        platform.pump(NOW_MS + 1).unwrap();
        let SteamPlatformState::InLobby(lobby) = platform.state() else {
            panic!("test host did not enter its lobby")
        };
        drain_events(&mut platform);
        platform
            .set_member_declaration(member_declaration(1, local_seats), false)
            .unwrap();
        drain_events(&mut platform);
        (platform, control, lobby)
    }

    fn drain_events(platform: &mut SteamPlatform<FakeSteamBackend>) -> Vec<SteamPlatformEvent> {
        std::iter::from_fn(|| platform.poll_event()).collect()
    }

    #[test]
    fn invite_overlay_checks_lobby_eligibility_before_current_overlay_readiness() {
        let local = user(90);
        let (mut idle, idle_control) = platform(local);
        assert_eq!(
            idle.open_invite_overlay(),
            Err(SteamPlatformError::InvalidState)
        );
        assert_eq!(idle_control.overlay_enabled_query_count(), 0);

        let (mut platform, control, _) = create_test_host(local, 1, 4);
        assert_eq!(
            platform.open_invite_overlay().unwrap(),
            SteamOverlayRequestStatus::Unavailable
        );
        assert_eq!(control.invite_overlay_open_count(), 0);
        assert_eq!(control.overlay_enabled_query_count(), 1);
        assert!(matches!(platform.state(), SteamPlatformState::InLobby(_)));

        control.set_overlay_enabled(true).unwrap();
        assert_eq!(
            platform.open_invite_overlay().unwrap(),
            SteamOverlayRequestStatus::Submitted
        );
        assert_eq!(control.invite_overlay_open_count(), 1);
        assert!(matches!(platform.state(), SteamPlatformState::InLobby(_)));

        platform.set_accepting_peers(false).unwrap();
        let readiness_queries = control.overlay_enabled_query_count();
        assert_eq!(
            platform.open_invite_overlay(),
            Err(SteamPlatformError::InvalidState)
        );
        assert_eq!(control.invite_overlay_open_count(), 1);
        assert_eq!(control.overlay_enabled_query_count(), readiness_queries);
    }

    #[test]
    fn spacewar_requires_an_explicit_development_opt_in() {
        let app_id = app_id();
        assert_eq!(
            SteamClientConfig::production(app_id).validate(),
            Err(SteamPlatformError::SpacewarForbiddenInProduction)
        );
        assert_eq!(
            SteamClientConfig::development(app_id, false).validate(),
            Err(SteamPlatformError::SpacewarRequiresExplicitOptIn)
        );
        assert!(
            SteamClientConfig::development(app_id, true)
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn dedicated_hosted_sdr_is_an_explicit_unavailable_capability() {
        let local = user(91);
        let (mut platform, _control) = platform(local);
        let dedicated = LobbyMetadata::current(
            AuthorityKind::Dedicated,
            LobbyVisibility::Private,
            RegionCode::new("kr-seoul").unwrap(),
            DefinitionId::new(0).unwrap(),
            DefinitionId::new(0).unwrap(),
            2,
        )
        .unwrap();
        assert_eq!(
            platform.create_lobby(
                LobbyCreateRequest {
                    visibility: LobbyVisibility::Private,
                    maximum_peers: 2,
                    local_seats: 1,
                },
                dedicated,
            ),
            Err(SteamPlatformError::DedicatedSdrUnavailable)
        );
        assert_eq!(
            platform.dedicated_hosted_sdr_support(),
            DedicatedSdrSupport::UnavailableInPinnedBinding
        );
    }

    #[test]
    fn first_release_player_metadata_allows_only_private_or_friends_listen_lobbies() {
        for visibility in [LobbyVisibility::Private, LobbyVisibility::FriendsOnly] {
            assert!(
                metadata(visibility, 4)
                    .validate_first_release_player_scope()
                    .is_ok()
            );
        }

        assert_eq!(
            metadata(LobbyVisibility::Public, 4).validate_first_release_player_scope(),
            Err(SteamPlatformError::PublicLobbiesDisabled)
        );
        let dedicated = LobbyMetadata::current(
            AuthorityKind::Dedicated,
            LobbyVisibility::Private,
            RegionCode::new("kr-seoul").unwrap(),
            DefinitionId::new(0).unwrap(),
            DefinitionId::new(0).unwrap(),
            2,
        )
        .unwrap();
        assert_eq!(
            dedicated.validate_first_release_player_scope(),
            Err(SteamPlatformError::DedicatedSdrUnavailable)
        );
    }

    #[test]
    fn connect_lobby_parser_is_exact_bounded_and_non_ambiguous() {
        assert_eq!(
            parse_connect_lobby_command("-silent +connect_lobby 12345").unwrap(),
            Some(lobby(12345))
        );
        assert_eq!(parse_connect_lobby_command("-silent").unwrap(), None);
        assert_eq!(
            parse_connect_lobby_command("+connect_lobby 0"),
            Err(SteamPlatformError::ZeroIdentifier)
        );
        assert_eq!(
            parse_connect_lobby_command("+connect_lobby 1 +connect_lobby 1"),
            Err(SteamPlatformError::DuplicateConnectLobby)
        );
        assert_eq!(
            parse_connect_lobby_command("+connect_lobby not-a-number"),
            Err(SteamPlatformError::InvalidConnectCommand)
        );
        assert_eq!(
            parse_connect_lobby_command("+connect_lobby\n123"),
            Err(SteamPlatformError::InvalidConnectCommand)
        );
        assert_eq!(
            parse_connect_lobby_command(&"x".repeat(MAX_CONNECT_COMMAND_BYTES + 1)),
            Err(SteamPlatformError::ConnectCommandTooLong)
        );
    }

    #[test]
    fn malformed_external_join_intents_do_not_fault_the_platform() {
        let local = user(100);
        let (mut platform, control) = platform(local);
        control
            .emit_rich_presence_join(Some(user(101)), "+connect_lobby not-a-number")
            .unwrap();
        control.set_launch_command("+connect_lobby 0").unwrap();
        control.emit_launch_parameters_changed().unwrap();

        platform.pump(NOW_MS + 1).unwrap();
        assert_eq!(platform.state(), SteamPlatformState::Idle);
        assert_eq!(platform.poll_event(), None);
    }

    #[test]
    fn create_publishes_exact_metadata_roster_readiness_and_presence() {
        let local = user(101);
        let (mut platform, control) = platform(local);
        let metadata = metadata(LobbyVisibility::Private, 4);
        platform
            .create_lobby(
                LobbyCreateRequest {
                    visibility: LobbyVisibility::Private,
                    maximum_peers: 4,
                    local_seats: 2,
                },
                metadata.clone(),
            )
            .unwrap();
        platform.pump(NOW_MS + 1).unwrap();

        let SteamPlatformState::InLobby(created_lobby) = platform.state() else {
            panic!("lobby was not entered")
        };
        assert_eq!(platform.lobby_metadata(), Some(&metadata));
        assert_eq!(platform.roster_len(), 1);
        assert_eq!(
            platform.roster()[0],
            Some(LobbyMember {
                user: local,
                readiness: MemberReadiness::Pending,
                loadout: None,
            })
        );
        assert_eq!(control.rich_presence("connect"), None);
        assert_eq!(
            drain_events(&mut platform),
            vec![SteamPlatformEvent::LobbyEntered {
                lobby: created_lobby,
                owner: local,
            }]
        );

        let declaration = member_declaration(1, 2);
        platform.set_member_declaration(declaration, false).unwrap();
        assert!(!platform.all_members_ready());
        platform.set_readiness(true, 2).unwrap();
        assert!(platform.all_members_match_ready());
        assert_eq!(platform.member_loadout(local), Some(declaration));
        platform.set_accepting_peers(false).unwrap();
        assert_eq!(control.rich_presence("connect"), None);
        assert_eq!(
            control.rich_presence("steam_player_group_size"),
            Some("1".into())
        );
    }

    #[test]
    fn member_updates_stage_every_partial_order_and_preserve_legal_ready_toggles() {
        let local = user(901);
        let remote = user(902);
        let (mut platform, control, lobby) = create_test_host(local, 1, 4);
        control
            .emit_membership_change(lobby, remote, LobbyMembershipChange::Entered)
            .unwrap();
        platform.pump(NOW_MS + 2).unwrap();
        drain_events(&mut platform);

        let first = member_declaration(1, 1);
        set_raw_member_declaration(
            &control,
            lobby,
            remote,
            MemberCommitMarker::Committed {
                revision: 1,
                ready: false,
            },
            first,
        );
        control.emit_member_data_changed(lobby, remote).unwrap();
        platform.pump(NOW_MS + 3).unwrap();
        assert!(matches!(
            platform
                .roster()
                .iter()
                .flatten()
                .find(|member| member.user == remote)
                .map(|member| member.readiness),
            Some(MemberReadiness::Declared {
                ready: false,
                local_seats: 1
            })
        ));
        drain_events(&mut platform);

        // New content arriving before its staging marker is mixed but merely
        // projects Pending; it cannot poison the lobby.
        let second = member_declaration(2, 2);
        control
            .set_member_data_raw(lobby, remote, MEMBER_KEY_LOADOUT, second.as_str())
            .unwrap();
        control.emit_member_data_changed(lobby, remote).unwrap();
        platform.pump(NOW_MS + 4).unwrap();
        assert_eq!(platform.state(), SteamPlatformState::InLobby(lobby));
        assert_eq!(
            platform
                .roster()
                .iter()
                .flatten()
                .find(|member| member.user == remote)
                .map(|member| member.readiness),
            Some(MemberReadiness::Pending)
        );

        control
            .set_member_data_raw(
                lobby,
                remote,
                MEMBER_KEY_READY,
                &encode_member_commit_marker(MemberCommitMarker::Staging { revision: 2 }),
            )
            .unwrap();
        control.emit_member_data_changed(lobby, remote).unwrap();
        platform.pump(NOW_MS + 5).unwrap();
        assert!(!platform.all_members_match_ready());

        control
            .set_member_data_raw(lobby, remote, MEMBER_KEY_SEATS, "2")
            .unwrap();
        control.emit_member_data_changed(lobby, remote).unwrap();
        platform.pump(NOW_MS + 6).unwrap();
        assert_eq!(
            platform
                .roster()
                .iter()
                .flatten()
                .find(|member| member.user == remote)
                .map(|member| member.readiness),
            Some(MemberReadiness::Pending)
        );

        control
            .set_member_data_raw(
                lobby,
                remote,
                MEMBER_KEY_READY,
                &encode_member_commit_marker(MemberCommitMarker::Committed {
                    revision: 2,
                    ready: false,
                }),
            )
            .unwrap();
        control.emit_member_data_changed(lobby, remote).unwrap();
        platform.pump(NOW_MS + 7).unwrap();
        assert_eq!(
            platform
                .roster()
                .iter()
                .flatten()
                .find(|member| member.user == remote)
                .map(|member| member.readiness),
            Some(MemberReadiness::Declared {
                ready: false,
                local_seats: 2,
            })
        );

        // Same revision + identical content is exactly the legal ready toggle.
        control
            .set_member_data_raw(
                lobby,
                remote,
                MEMBER_KEY_READY,
                &encode_member_commit_marker(MemberCommitMarker::Committed {
                    revision: 2,
                    ready: true,
                }),
            )
            .unwrap();
        control.emit_member_data_changed(lobby, remote).unwrap();
        platform.pump(NOW_MS + 8).unwrap();
        assert_eq!(
            platform
                .roster()
                .iter()
                .flatten()
                .find(|member| member.user == remote)
                .map(|member| member.readiness),
            Some(MemberReadiness::Declared {
                ready: true,
                local_seats: 2,
            })
        );
    }

    #[test]
    fn continuity_violations_are_deduped_pending_and_require_a_higher_revision() {
        let local = user(911);
        let remote = user(912);
        let (mut platform, control, lobby) = create_test_host(local, 1, 4);
        control
            .emit_membership_change(lobby, remote, LobbyMembershipChange::Entered)
            .unwrap();
        platform.pump(NOW_MS + 2).unwrap();
        drain_events(&mut platform);

        set_raw_member_declaration(
            &control,
            lobby,
            remote,
            MemberCommitMarker::Committed {
                revision: 2,
                ready: true,
            },
            member_declaration(2, 1),
        );
        control.emit_member_data_changed(lobby, remote).unwrap();
        platform.pump(NOW_MS + 3).unwrap();
        drain_events(&mut platform);

        set_raw_member_declaration(
            &control,
            lobby,
            remote,
            MemberCommitMarker::Committed {
                revision: 1,
                ready: true,
            },
            member_declaration(1, 1),
        );
        control.emit_member_data_changed(lobby, remote).unwrap();
        platform.pump(NOW_MS + 4).unwrap();
        let first_rejection = drain_events(&mut platform);
        assert_eq!(
            first_rejection
                .iter()
                .filter(|event| matches!(
                    event,
                    SteamPlatformEvent::LobbyMemberDataChanged {
                        user,
                        outcome: MemberDataOutcome::Rejected(
                            MemberDeclarationRejection::RevisionRegression
                        ),
                        ..
                    } if *user == remote
                ))
                .count(),
            1
        );
        assert_eq!(
            platform
                .roster()
                .iter()
                .flatten()
                .find(|member| member.user == remote)
                .map(|member| member.readiness),
            Some(MemberReadiness::Pending)
        );

        control.emit_member_data_changed(lobby, remote).unwrap();
        platform.pump(NOW_MS + 5).unwrap();
        assert!(!drain_events(&mut platform).iter().any(|event| matches!(
            event,
            SteamPlatformEvent::LobbyMemberDataChanged {
                outcome: MemberDataOutcome::Rejected(_),
                ..
            }
        )));

        // Same-revision content cannot reactivate an invalid member.
        set_raw_member_declaration(
            &control,
            lobby,
            remote,
            MemberCommitMarker::Committed {
                revision: 2,
                ready: false,
            },
            member_declaration_variant(2, 1),
        );
        control.emit_member_data_changed(lobby, remote).unwrap();
        platform.pump(NOW_MS + 6).unwrap();
        assert_eq!(
            platform
                .roster()
                .iter()
                .flatten()
                .find(|member| member.user == remote)
                .map(|member| member.readiness),
            Some(MemberReadiness::Pending)
        );

        set_raw_member_declaration(
            &control,
            lobby,
            remote,
            MemberCommitMarker::Committed {
                revision: 3,
                ready: false,
            },
            member_declaration(3, 1),
        );
        control.emit_member_data_changed(lobby, remote).unwrap();
        platform.pump(NOW_MS + 7).unwrap();
        assert!(matches!(
            platform
                .roster()
                .iter()
                .flatten()
                .find(|member| member.user == remote)
                .map(|member| member.readiness),
            Some(MemberReadiness::Declared {
                ready: false,
                local_seats: 1
            })
        ));

        set_raw_member_declaration(
            &control,
            lobby,
            remote,
            MemberCommitMarker::Committed {
                revision: 3,
                ready: true,
            },
            member_declaration_variant(3, 1),
        );
        control.emit_member_data_changed(lobby, remote).unwrap();
        platform.pump(NOW_MS + 8).unwrap();
        assert!(drain_events(&mut platform).iter().any(|event| matches!(
            event,
            SteamPlatformEvent::LobbyMemberDataChanged {
                user,
                outcome: MemberDataOutcome::Rejected(
                    MemberDeclarationRejection::RevisionConflict
                ),
                ..
            } if *user == remote
        )));
        assert_eq!(
            platform
                .roster()
                .iter()
                .flatten()
                .find(|member| member.user == remote)
                .map(|member| member.readiness),
            Some(MemberReadiness::Pending)
        );
    }

    #[test]
    fn malformed_member_is_deduped_and_lobby_subject_alone_revalidates_contract() {
        let local = user(921);
        let remote = user(922);
        let (mut platform, control, lobby) = create_test_host(local, 1, 4);
        control
            .emit_membership_change(lobby, remote, LobbyMembershipChange::Entered)
            .unwrap();
        platform.pump(NOW_MS + 2).unwrap();
        drain_events(&mut platform);
        set_raw_member_declaration(
            &control,
            lobby,
            remote,
            MemberCommitMarker::Committed {
                revision: 1,
                ready: false,
            },
            member_declaration(1, 1),
        );
        control.emit_member_data_changed(lobby, remote).unwrap();
        platform.pump(NOW_MS + 3).unwrap();
        drain_events(&mut platform);

        control
            .set_member_data_raw(lobby, remote, MEMBER_KEY_READY, "not-a-marker")
            .unwrap();
        control.emit_member_data_changed(lobby, remote).unwrap();
        platform.pump(NOW_MS + 4).unwrap();
        let rejected = drain_events(&mut platform);
        assert_eq!(
            rejected
                .iter()
                .filter(|event| matches!(
                    event,
                    SteamPlatformEvent::LobbyMemberDataChanged {
                        user,
                        outcome: MemberDataOutcome::Rejected(
                            MemberDeclarationRejection::Malformed
                        ),
                        ..
                    } if *user == remote
                ))
                .count(),
            1
        );
        assert_eq!(platform.state(), SteamPlatformState::InLobby(lobby));
        assert!(!platform.effective_joinable());
        control.emit_member_data_changed(lobby, remote).unwrap();
        platform.pump(NOW_MS + 5).unwrap();
        assert!(!drain_events(&mut platform).iter().any(|event| matches!(
            event,
            SteamPlatformEvent::LobbyMemberDataChanged {
                outcome: MemberDataOutcome::Rejected(_),
                ..
            }
        )));

        // A member-subject callback cannot be conflated with a lobby-contract
        // callback, even if immutable data is concurrently hostile.
        control.set_lobby_data_raw(lobby, KEY_RULES, "2").unwrap();
        control.emit_member_data_changed(lobby, remote).unwrap();
        platform.pump(NOW_MS + 6).unwrap();
        assert_eq!(platform.state(), SteamPlatformState::InLobby(lobby));
        control.emit_lobby_data_changed(lobby).unwrap();
        assert_eq!(
            platform.pump(NOW_MS + 7),
            Err(SteamPlatformError::MetadataMismatch)
        );
        assert_eq!(platform.state(), SteamPlatformState::Faulted);
    }

    #[test]
    fn pending_and_full_members_close_joinability_then_reduction_reopens_it() {
        let local = user(931);
        let remote = user(932);
        let (mut platform, control, lobby) = create_test_host(local, 2, 4);
        assert!(platform.effective_joinable());
        assert_eq!(control.lobby_is_joinable(lobby), Some(true));

        control
            .emit_membership_change(lobby, remote, LobbyMembershipChange::Entered)
            .unwrap();
        platform.pump(NOW_MS + 2).unwrap();
        assert!(!platform.effective_joinable());
        assert_eq!(control.lobby_is_joinable(lobby), Some(false));
        drain_events(&mut platform);

        set_raw_member_declaration(
            &control,
            lobby,
            remote,
            MemberCommitMarker::Committed {
                revision: 1,
                ready: false,
            },
            member_declaration(1, 1),
        );
        control.emit_member_data_changed(lobby, remote).unwrap();
        platform.pump(NOW_MS + 3).unwrap();
        assert!(platform.effective_joinable());

        set_raw_member_declaration(
            &control,
            lobby,
            remote,
            MemberCommitMarker::Committed {
                revision: 2,
                ready: false,
            },
            member_declaration(2, 2),
        );
        control.emit_member_data_changed(lobby, remote).unwrap();
        platform.pump(NOW_MS + 4).unwrap();
        assert_eq!(platform.accepted_seat_total(), 4);
        assert!(!platform.effective_joinable());

        set_raw_member_declaration(
            &control,
            lobby,
            remote,
            MemberCommitMarker::Committed {
                revision: 3,
                ready: false,
            },
            member_declaration(3, 1),
        );
        control.emit_member_data_changed(lobby, remote).unwrap();
        platform.pump(NOW_MS + 5).unwrap();
        assert_eq!(platform.accepted_seat_total(), 3);
        assert!(platform.effective_joinable());
        assert_eq!(control.lobby_is_joinable(lobby), Some(true));

        set_raw_member_declaration(
            &control,
            lobby,
            remote,
            MemberCommitMarker::Committed {
                revision: 4,
                ready: false,
            },
            member_declaration(4, 2),
        );
        control.emit_member_data_changed(lobby, remote).unwrap();
        platform.pump(NOW_MS + 6).unwrap();
        assert!(!platform.effective_joinable());
        control
            .emit_membership_change(lobby, remote, LobbyMembershipChange::Left)
            .unwrap();
        platform.pump(NOW_MS + 7).unwrap();
        assert!(platform.effective_joinable());
        assert_eq!(platform.accepted_seat_total(), 2);
    }

    fn canonical_capacity_result(
        first_callback: SteamUserId,
    ) -> (Vec<SteamUserId>, Vec<SteamUserId>) {
        let owner = user(999);
        let lower = user(100);
        let higher = user(200);
        let (mut platform, control, lobby) = create_test_host(owner, 2, 4);
        for remote in [lower, higher] {
            control
                .emit_membership_change(lobby, remote, LobbyMembershipChange::Entered)
                .unwrap();
            platform.pump(NOW_MS + 2 + remote.get()).unwrap();
            drain_events(&mut platform);
        }
        set_raw_member_declaration(
            &control,
            lobby,
            lower,
            MemberCommitMarker::Committed {
                revision: 1,
                ready: false,
            },
            member_declaration(1, 2),
        );
        set_raw_member_declaration(
            &control,
            lobby,
            higher,
            MemberCommitMarker::Committed {
                revision: 1,
                ready: false,
            },
            member_declaration(1, 1),
        );
        control
            .emit_member_data_changed(lobby, first_callback)
            .unwrap();
        platform.pump(NOW_MS + 500).unwrap();
        let events = drain_events(&mut platform);
        let accepted = platform
            .roster()
            .iter()
            .flatten()
            .filter_map(|member| match member.readiness {
                MemberReadiness::Declared { .. } => Some(member.user),
                MemberReadiness::Pending => None,
            })
            .collect::<Vec<_>>();
        let rejected =
            events
                .iter()
                .filter_map(|event| match event {
                    SteamPlatformEvent::LobbyMemberDataChanged {
                        user,
                        outcome:
                            MemberDataOutcome::Rejected(
                                MemberDeclarationRejection::LobbyCapacityExceeded,
                            ),
                        ..
                    } => Some(*user),
                    _ => None,
                })
                .collect::<Vec<_>>();
        (accepted, rejected)
    }

    #[test]
    fn capacity_arbitration_is_owner_then_user_id_independent_of_callback_order() {
        let lower_first = canonical_capacity_result(user(100));
        let higher_first = canonical_capacity_result(user(200));
        assert_eq!(lower_first, higher_first);
        assert_eq!(lower_first.0, vec![user(999), user(100)]);
        assert_eq!(lower_first.1, vec![user(200)]);
    }

    #[test]
    fn cancelled_create_can_retry_immediately_and_retires_late_callbacks() {
        let local = user(111);
        let (mut platform, control) = platform(local);
        let request = LobbyCreateRequest {
            visibility: LobbyVisibility::Private,
            maximum_peers: 2,
            local_seats: 1,
        };

        platform
            .create_lobby(request, metadata(LobbyVisibility::Private, 2))
            .unwrap();
        assert!(platform.cancel_pending_lobby_operation().unwrap());
        assert_eq!(platform.state(), SteamPlatformState::Idle);
        platform
            .create_lobby(request, metadata(LobbyVisibility::Private, 2))
            .unwrap();
        platform.pump(NOW_MS + 1).unwrap();

        let retired_lobby = lobby(10_000);
        let active_lobby = lobby(10_001);
        assert_eq!(platform.state(), SteamPlatformState::InLobby(active_lobby));
        assert!(!control.lobby_contains_member(retired_lobby, local));
        assert!(control.lobby_contains_member(active_lobby, local));
        assert_eq!(
            drain_events(&mut platform),
            vec![SteamPlatformEvent::LobbyEntered {
                lobby: active_lobby,
                owner: local,
            }]
        );

        control
            .emit(SteamBackendEvent::LobbyCreated {
                operation_id: SteamOperationId(1),
                result: Err(SteamBackendError::OperationFailed),
            })
            .unwrap();
        platform.pump(NOW_MS + 2).unwrap();
        assert_eq!(platform.state(), SteamPlatformState::InLobby(active_lobby));
        assert_eq!(platform.poll_event(), None);
    }

    #[test]
    fn cancelled_join_can_retry_immediately_and_retires_late_callbacks() {
        let local = user(121);
        let owner = user(122);
        let retired_lobby = lobby(11_001);
        let active_lobby = lobby(11_002);
        let (mut platform, control) = platform(local);
        for target in [retired_lobby, active_lobby] {
            control
                .seed_lobby(
                    target,
                    &metadata(LobbyVisibility::Private, 2),
                    true,
                    owner,
                    &[FakeLobbyMemberSeed {
                        user: owner,
                        readiness: Some(MemberReadiness::Declared {
                            ready: false,
                            local_seats: 1,
                        }),
                    }],
                )
                .unwrap();
        }
        let join_intent = |target| LobbyJoinIntent {
            lobby: target,
            origin: JoinOrigin::SteamInvite {
                friend: Some(owner),
            },
            expires_at_ms: NOW_MS + DEFAULT_JOIN_INTENT_TTL_MS,
        };

        platform
            .join_lobby(join_intent(retired_lobby), 1, NOW_MS)
            .unwrap();
        assert!(platform.cancel_pending_lobby_operation().unwrap());
        assert_eq!(platform.state(), SteamPlatformState::Idle);
        platform
            .join_lobby(join_intent(active_lobby), 1, NOW_MS)
            .unwrap();
        platform.pump(NOW_MS + 1).unwrap();

        assert_eq!(platform.state(), SteamPlatformState::InLobby(active_lobby));
        assert!(!control.lobby_contains_member(retired_lobby, local));
        assert!(control.lobby_contains_member(active_lobby, local));
        assert_eq!(
            drain_events(&mut platform),
            vec![SteamPlatformEvent::LobbyEntered {
                lobby: active_lobby,
                owner,
            }]
        );

        control
            .emit(SteamBackendEvent::LobbyJoined {
                operation_id: SteamOperationId(1),
                requested: retired_lobby,
                result: Err(SteamBackendError::OperationFailed),
            })
            .unwrap();
        platform.pump(NOW_MS + 2).unwrap();
        assert_eq!(platform.state(), SteamPlatformState::InLobby(active_lobby));
        assert_eq!(platform.poll_event(), None);
    }

    #[test]
    fn cancelled_same_lobby_join_retry_survives_old_success_and_failure() {
        let local = user(125);
        let owner = user(126);
        let target = lobby(11_101);
        let (mut platform, control) = platform(local);
        control
            .seed_lobby(
                target,
                &metadata(LobbyVisibility::Private, 2),
                true,
                owner,
                &[FakeLobbyMemberSeed {
                    user: owner,
                    readiness: Some(MemberReadiness::Declared {
                        ready: false,
                        local_seats: 1,
                    }),
                }],
            )
            .unwrap();
        let intent = || LobbyJoinIntent {
            lobby: target,
            origin: JoinOrigin::SteamInvite {
                friend: Some(owner),
            },
            expires_at_ms: NOW_MS + DEFAULT_JOIN_INTENT_TTL_MS,
        };

        platform.join_lobby(intent(), 1, NOW_MS).unwrap();
        assert!(platform.cancel_pending_lobby_operation().unwrap());
        platform.join_lobby(intent(), 1, NOW_MS).unwrap();
        control
            .emit(SteamBackendEvent::LobbyJoined {
                operation_id: SteamOperationId(1),
                requested: target,
                result: Err(SteamBackendError::OperationFailed),
            })
            .unwrap();
        platform.pump(NOW_MS + 1).unwrap();

        assert_eq!(platform.state(), SteamPlatformState::InLobby(target));
        assert!(control.lobby_contains_member(target, local));
        assert_eq!(
            drain_events(&mut platform),
            vec![SteamPlatformEvent::LobbyEntered {
                lobby: target,
                owner,
            }]
        );
    }

    #[test]
    fn matching_join_success_for_wrong_lobby_leaves_returned_membership() {
        let local = user(151);
        let owner = user(152);
        let requested = lobby(11_201);
        let returned = lobby(11_202);
        let (mut platform, control) = platform(local);
        control
            .seed_lobby(
                requested,
                &metadata(LobbyVisibility::Private, 3),
                true,
                owner,
                &[FakeLobbyMemberSeed {
                    user: owner,
                    readiness: Some(MemberReadiness::Declared {
                        ready: false,
                        local_seats: 1,
                    }),
                }],
            )
            .unwrap();
        control
            .seed_lobby(
                returned,
                &metadata(LobbyVisibility::Private, 3),
                true,
                owner,
                &[
                    FakeLobbyMemberSeed {
                        user: owner,
                        readiness: Some(MemberReadiness::Declared {
                            ready: false,
                            local_seats: 1,
                        }),
                    },
                    FakeLobbyMemberSeed {
                        user: local,
                        readiness: Some(MemberReadiness::Declared {
                            ready: false,
                            local_seats: 1,
                        }),
                    },
                ],
            )
            .unwrap();
        platform
            .join_lobby(
                LobbyJoinIntent {
                    lobby: requested,
                    origin: JoinOrigin::SteamInvite {
                        friend: Some(owner),
                    },
                    expires_at_ms: NOW_MS + DEFAULT_JOIN_INTENT_TTL_MS,
                },
                1,
                NOW_MS,
            )
            .unwrap();
        control
            .set_queued_lobby_join_result(SteamOperationId(1), Ok(returned))
            .unwrap();

        assert_eq!(
            platform.pump(NOW_MS + 1),
            Err(SteamPlatformError::UnexpectedLobby)
        );
        assert!(!control.lobby_contains_member(returned, local));
        assert!(!control.lobby_contains_member(requested, local));
        assert_eq!(platform.state(), SteamPlatformState::Faulted);
    }

    #[test]
    fn matching_join_operation_with_corrupted_requested_id_leaves_every_membership() {
        let local = user(161);
        let owner = user(162);
        let intended = lobby(11_301);
        let corrupted = lobby(11_302);
        let (mut platform, control) = platform(local);
        control
            .seed_lobby(
                intended,
                &metadata(LobbyVisibility::Private, 3),
                true,
                owner,
                &[FakeLobbyMemberSeed {
                    user: owner,
                    readiness: Some(MemberReadiness::Declared {
                        ready: false,
                        local_seats: 1,
                    }),
                }],
            )
            .unwrap();
        control
            .seed_lobby(
                corrupted,
                &metadata(LobbyVisibility::Private, 3),
                true,
                owner,
                &[
                    FakeLobbyMemberSeed {
                        user: owner,
                        readiness: Some(MemberReadiness::Declared {
                            ready: false,
                            local_seats: 1,
                        }),
                    },
                    FakeLobbyMemberSeed {
                        user: local,
                        readiness: Some(MemberReadiness::Declared {
                            ready: false,
                            local_seats: 1,
                        }),
                    },
                ],
            )
            .unwrap();
        platform
            .join_lobby(
                LobbyJoinIntent {
                    lobby: intended,
                    origin: JoinOrigin::SteamInvite {
                        friend: Some(owner),
                    },
                    expires_at_ms: NOW_MS + DEFAULT_JOIN_INTENT_TTL_MS,
                },
                1,
                NOW_MS,
            )
            .unwrap();
        control
            .set_queued_lobby_join_callback(SteamOperationId(1), corrupted, Ok(corrupted))
            .unwrap();

        assert_eq!(
            platform.pump(NOW_MS + 1),
            Err(SteamPlatformError::UnexpectedLobby)
        );
        assert!(!control.lobby_contains_member(intended, local));
        assert!(!control.lobby_contains_member(corrupted, local));
        assert_eq!(platform.state(), SteamPlatformState::Faulted);
    }

    #[test]
    fn cancelled_auth_ticket_ignores_queued_and_late_responses() {
        let local = user(131);
        let remote = user(132);
        let (mut platform, control) = platform(local);
        platform
            .create_lobby(
                LobbyCreateRequest {
                    visibility: LobbyVisibility::Private,
                    maximum_peers: 2,
                    local_seats: 1,
                },
                metadata(LobbyVisibility::Private, 2),
            )
            .unwrap();
        platform.pump(NOW_MS + 1).unwrap();
        drain_events(&mut platform);

        let ticket = platform.issue_auth_ticket(remote).unwrap();
        platform.cancel_auth_ticket(ticket.handle).unwrap();
        assert!(control.cancelled_ticket(ticket.handle));
        control
            .emit(SteamBackendEvent::AuthTicketReady {
                handle: ticket.handle,
                success: false,
            })
            .unwrap();
        platform.pump(NOW_MS + 2).unwrap();

        assert!(matches!(platform.state(), SteamPlatformState::InLobby(_)));
        assert!(!platform.auth_ticket_is_ready(ticket.handle));
        assert_eq!(platform.poll_event(), None);
    }

    #[test]
    fn unissued_lobby_operation_callback_faults_closed() {
        let local = user(141);
        let (mut platform, control) = platform(local);
        control
            .emit(SteamBackendEvent::LobbyCreated {
                operation_id: SteamOperationId(1),
                result: Err(SteamBackendError::OperationFailed),
            })
            .unwrap();

        assert_eq!(
            platform.pump(NOW_MS + 1),
            Err(SteamPlatformError::Backend(
                SteamBackendError::IntegrityFailure
            ))
        );
        assert_eq!(platform.state(), SteamPlatformState::Faulted);
    }

    #[test]
    fn private_and_friends_join_policies_fail_closed() {
        let local = user(201);
        let owner = user(202);
        let private_lobby = lobby(5_001);
        let (mut platform, control) = platform(local);
        control
            .seed_lobby(
                private_lobby,
                &metadata(LobbyVisibility::Private, 4),
                true,
                owner,
                &[FakeLobbyMemberSeed {
                    user: owner,
                    readiness: Some(MemberReadiness::Declared {
                        ready: false,
                        local_seats: 1,
                    }),
                }],
            )
            .unwrap();
        platform
            .join_lobby(
                LobbyJoinIntent::friends_list(private_lobby, NOW_MS, DEFAULT_JOIN_INTENT_TTL_MS)
                    .unwrap(),
                1,
                NOW_MS,
            )
            .unwrap();
        platform.pump(NOW_MS + 1).unwrap();
        assert_eq!(platform.state(), SteamPlatformState::Idle);
        assert_eq!(
            drain_events(&mut platform),
            vec![SteamPlatformEvent::LobbyJoinRejected {
                lobby: private_lobby,
                reason: SteamPlatformError::PrivateLobbyRequiresInvite,
            }]
        );

        let friends_lobby = lobby(5_002);
        control
            .seed_lobby(
                friends_lobby,
                &metadata(LobbyVisibility::FriendsOnly, 4),
                true,
                owner,
                &[FakeLobbyMemberSeed {
                    user: owner,
                    readiness: Some(MemberReadiness::Declared {
                        ready: false,
                        local_seats: 1,
                    }),
                }],
            )
            .unwrap();
        let intent = LobbyJoinIntent {
            lobby: friends_lobby,
            origin: JoinOrigin::SteamInvite {
                friend: Some(owner),
            },
            expires_at_ms: NOW_MS + DEFAULT_JOIN_INTENT_TTL_MS,
        };
        platform.join_lobby(intent, 1, NOW_MS + 2).unwrap();
        platform.pump(NOW_MS + 3).unwrap();
        assert_eq!(platform.state(), SteamPlatformState::Idle);
        assert_eq!(
            drain_events(&mut platform),
            vec![SteamPlatformEvent::LobbyJoinRejected {
                lobby: friends_lobby,
                reason: SteamPlatformError::FriendsRelationshipRequired,
            }]
        );
    }

    #[test]
    fn invited_friend_join_validates_metadata_before_entering_gameplay() {
        let local = user(301);
        let owner = user(302);
        let expected_lobby = lobby(6_001);
        let (mut platform, control) = platform(local);
        control.set_friend(owner, true).unwrap();
        control
            .seed_lobby(
                expected_lobby,
                &metadata(LobbyVisibility::FriendsOnly, 4),
                true,
                owner,
                &[FakeLobbyMemberSeed {
                    user: owner,
                    readiness: Some(MemberReadiness::Declared {
                        ready: true,
                        local_seats: 2,
                    }),
                }],
            )
            .unwrap();
        control
            .emit_join_request(expected_lobby, Some(owner))
            .unwrap();
        platform.pump(NOW_MS + 1).unwrap();
        let Some(SteamPlatformEvent::LobbyJoinRequested(intent)) = platform.poll_event() else {
            panic!("missing join intent")
        };
        platform.join_lobby(intent, 2, NOW_MS + 1).unwrap();
        platform.pump(NOW_MS + 2).unwrap();
        assert_eq!(
            platform.state(),
            SteamPlatformState::InLobby(expected_lobby)
        );
        assert_eq!(platform.roster_len(), 2);
        assert!(!platform.all_members_ready());

        platform
            .set_member_declaration(member_declaration(1, 2), true)
            .unwrap();
        assert!(platform.all_members_ready());
    }

    #[test]
    fn readiness_rejects_overbooking_before_mutating_lobby_state() {
        let local = user(351);
        let owner = user(352);
        let expected_lobby = lobby(6_501);
        let (mut platform, control) = platform(local);
        control
            .seed_lobby(
                expected_lobby,
                &metadata(LobbyVisibility::Private, 4),
                true,
                owner,
                &[FakeLobbyMemberSeed {
                    user: owner,
                    readiness: Some(MemberReadiness::Declared {
                        ready: true,
                        local_seats: 3,
                    }),
                }],
            )
            .unwrap();
        platform
            .join_lobby(
                LobbyJoinIntent {
                    lobby: expected_lobby,
                    origin: JoinOrigin::LaunchCommand,
                    expires_at_ms: NOW_MS + DEFAULT_JOIN_INTENT_TTL_MS,
                },
                1,
                NOW_MS,
            )
            .unwrap();
        platform.pump(NOW_MS + 1).unwrap();
        drain_events(&mut platform);

        let before = [
            control.member_data_raw(expected_lobby, local, MEMBER_KEY_READY),
            control.member_data_raw(expected_lobby, local, MEMBER_KEY_SEATS),
            control.member_data_raw(expected_lobby, local, MEMBER_KEY_LOADOUT),
        ];
        assert_eq!(
            platform.set_member_declaration(member_declaration(1, 2), true),
            Err(SteamPlatformError::LobbyCapacityExceeded)
        );
        assert_eq!(
            [
                control.member_data_raw(expected_lobby, local, MEMBER_KEY_READY),
                control.member_data_raw(expected_lobby, local, MEMBER_KEY_SEATS),
                control.member_data_raw(expected_lobby, local, MEMBER_KEY_LOADOUT),
            ],
            before
        );
        assert_eq!(
            platform.state(),
            SteamPlatformState::InLobby(expected_lobby)
        );
        assert!(platform.roster().iter().flatten().any(|member| {
            member.user == local && member.readiness == MemberReadiness::Pending
        }));
    }

    #[test]
    fn successful_native_join_is_left_when_coherent_seats_overbook_capacity() {
        let local = user(361);
        let owner = user(362);
        let expected_lobby = lobby(6_601);
        let (mut platform, control) = platform(local);
        control
            .seed_lobby(
                expected_lobby,
                &metadata(LobbyVisibility::Private, 4),
                true,
                owner,
                &[FakeLobbyMemberSeed {
                    user: owner,
                    readiness: Some(MemberReadiness::Declared {
                        ready: false,
                        local_seats: 3,
                    }),
                }],
            )
            .unwrap();
        platform
            .join_lobby(
                LobbyJoinIntent {
                    lobby: expected_lobby,
                    origin: JoinOrigin::LaunchCommand,
                    expires_at_ms: NOW_MS + DEFAULT_JOIN_INTENT_TTL_MS,
                },
                2,
                NOW_MS,
            )
            .unwrap();
        platform.pump(NOW_MS + 1).unwrap();
        assert_eq!(platform.state(), SteamPlatformState::Idle);
        assert!(!control.lobby_contains_member(expected_lobby, local));
        assert!(matches!(
            platform.poll_event(),
            Some(SteamPlatformEvent::LobbyJoinRejected {
                lobby,
                reason: SteamPlatformError::LobbyCapacityExceeded,
            }) if lobby == expected_lobby
        ));
    }

    #[test]
    fn incompatible_lobby_is_left_before_transport_admission() {
        let local = user(401);
        let owner = user(402);
        let expected_lobby = lobby(7_001);
        let (mut platform, control) = platform(local);
        control
            .seed_lobby(
                expected_lobby,
                &metadata(LobbyVisibility::Private, 2),
                true,
                owner,
                &[FakeLobbyMemberSeed {
                    user: owner,
                    readiness: Some(MemberReadiness::Declared {
                        ready: false,
                        local_seats: 1,
                    }),
                }],
            )
            .unwrap();
        control
            .set_lobby_data_raw(expected_lobby, KEY_PROTOCOL, "65535")
            .unwrap();
        let intent = LobbyJoinIntent {
            lobby: expected_lobby,
            origin: JoinOrigin::LaunchCommand,
            expires_at_ms: NOW_MS + 10,
        };
        platform.join_lobby(intent, 1, NOW_MS).unwrap();
        platform.pump(NOW_MS + 1).unwrap();
        assert_eq!(platform.state(), SteamPlatformState::Idle);
        assert!(matches!(
            platform.poll_event(),
            Some(SteamPlatformEvent::LobbyJoinRejected {
                lobby: value,
                reason: SteamPlatformError::Protocol(
                    ProtocolValidationError::ProtocolVersionMismatch
                ),
            }) if value == expected_lobby
        ));
    }

    #[test]
    fn post_validation_join_read_failure_leaves_native_membership_transactionally() {
        let local = user(411);
        let owner = user(412);
        let expected_lobby = lobby(7_011);
        let (mut platform, control) = platform(local);
        control
            .seed_lobby(
                expected_lobby,
                &metadata(LobbyVisibility::Private, 2),
                true,
                owner,
                &[FakeLobbyMemberSeed {
                    user: owner,
                    readiness: Some(MemberReadiness::Declared {
                        ready: false,
                        local_seats: 1,
                    }),
                }],
            )
            .unwrap();
        platform
            .join_lobby(
                LobbyJoinIntent {
                    lobby: expected_lobby,
                    origin: JoinOrigin::SteamInvite {
                        friend: Some(owner),
                    },
                    expires_at_ms: NOW_MS + 10,
                },
                1,
                NOW_MS,
            )
            .unwrap();
        // The first schema read belongs to validate_joined_lobby. Inject the
        // failure into the exact re-read that follows successful native join
        // validation but precedes active-state installation.
        control
            .fail_lobby_data_read_on_occurrence(expected_lobby, KEY_SCHEMA, 2)
            .unwrap();

        platform.pump(NOW_MS + 1).unwrap();

        assert_eq!(platform.state(), SteamPlatformState::Idle);
        assert!(!control.lobby_contains_member(expected_lobby, local));
        assert_eq!(control.rich_presence("connect"), None);
        assert!(matches!(
            platform.poll_event(),
            Some(SteamPlatformEvent::LobbyJoinRejected {
                lobby,
                reason: SteamPlatformError::Backend(SteamBackendError::OperationFailed),
            }) if lobby == expected_lobby
        ));
    }

    #[test]
    fn expected_lobby_authentication_license_and_ticket_replay_are_safe_for_admission() {
        let local = user(501);
        let remote = user(502);
        let family_license_owner = user(503);
        let (mut platform, control) = platform(local);
        platform
            .create_lobby(
                LobbyCreateRequest {
                    visibility: LobbyVisibility::Private,
                    maximum_peers: 4,
                    local_seats: 1,
                },
                metadata(LobbyVisibility::Private, 4),
            )
            .unwrap();
        platform.pump(NOW_MS + 1).unwrap();
        drain_events(&mut platform);
        let SteamPlatformState::InLobby(active_lobby) = platform.state() else {
            panic!("lobby missing")
        };
        control
            .emit_membership_change(active_lobby, remote, LobbyMembershipChange::Entered)
            .unwrap();
        set_raw_member_declaration(
            &control,
            active_lobby,
            remote,
            MemberCommitMarker::Committed {
                revision: 1,
                ready: true,
            },
            member_declaration(1, 2),
        );
        platform.pump(NOW_MS + 2).unwrap();
        control
            .set_auth_outcome(
                remote,
                FakeAuthOutcome {
                    license_owner_user: family_license_owner,
                    validation: Ok(()),
                    license: LicenseStatus::HasLicense,
                },
            )
            .unwrap();

        platform
            .begin_peer_authentication(
                active_lobby,
                remote,
                &[1, 2, 3],
                AdmissionPurpose::Initial,
                NOW_MS + 2,
            )
            .unwrap();
        // Reliable signaling can redeliver the exact ticket while validation
        // is pending. It must reuse the live auth session rather than invoking
        // BeginAuthSession twice or faulting the lobby.
        platform
            .begin_peer_authentication(
                active_lobby,
                remote,
                &[1, 2, 3],
                AdmissionPurpose::Initial,
                NOW_MS + 2,
            )
            .unwrap();
        assert_eq!(
            platform.consume_authenticated_admission(active_lobby, remote, NOW_MS + 2),
            Err(SteamPlatformError::AuthenticationPending)
        );
        platform.pump(NOW_MS + 3).unwrap();
        let admission = platform
            .consume_authenticated_admission(active_lobby, remote, NOW_MS + 3)
            .unwrap();
        assert_eq!(admission.user, remote);
        assert_eq!(admission.license_owner_user, family_license_owner);
        assert_eq!(admission.authenticated_user, remote.authenticated());
        assert_eq!(admission.local_seats, 2);
        // A delayed replay after one-time admission consumption is also
        // idempotent; it cannot reset the consumed capability.
        platform
            .begin_peer_authentication(
                active_lobby,
                remote,
                &[1, 2, 3],
                AdmissionPurpose::Initial,
                NOW_MS + 3,
            )
            .unwrap();
        assert_eq!(
            platform.consume_authenticated_admission(active_lobby, remote, NOW_MS + 3),
            Err(SteamPlatformError::AdmissionAlreadyConsumed)
        );
        let after_auth_deadline = NOW_MS + DEFAULT_AUTH_INTENT_TTL_MS + 10;
        platform.pump(after_auth_deadline).unwrap();
        assert!(!control.ended_auth_session(remote));

        let ticket = platform.issue_auth_ticket(remote).unwrap();
        assert!(!platform.auth_ticket_is_ready(ticket.handle));
        platform.pump(after_auth_deadline + 1).unwrap();
        assert!(platform.auth_ticket_is_ready(ticket.handle));
        control
            .emit_membership_change(active_lobby, remote, LobbyMembershipChange::Left)
            .unwrap();
        platform.pump(after_auth_deadline + 2).unwrap();
        assert!(control.cancelled_ticket(ticket.handle));
        assert!(control.ended_auth_session(remote));
        assert_eq!(platform.state(), SteamPlatformState::InLobby(active_lobby));
        platform.leave_lobby().unwrap();
    }

    #[test]
    fn license_failure_rejects_and_ends_authentication() {
        let local = user(601);
        let remote = user(602);
        let (mut platform, control) = platform(local);
        platform
            .create_lobby(
                LobbyCreateRequest {
                    visibility: LobbyVisibility::Private,
                    maximum_peers: 2,
                    local_seats: 1,
                },
                metadata(LobbyVisibility::Private, 2),
            )
            .unwrap();
        platform.pump(NOW_MS + 1).unwrap();
        drain_events(&mut platform);
        let SteamPlatformState::InLobby(active_lobby) = platform.state() else {
            panic!("lobby missing")
        };
        control
            .emit_membership_change(active_lobby, remote, LobbyMembershipChange::Entered)
            .unwrap();
        set_raw_member_declaration(
            &control,
            active_lobby,
            remote,
            MemberCommitMarker::Committed {
                revision: 1,
                ready: false,
            },
            member_declaration(1, 1),
        );
        control
            .set_auth_outcome(
                remote,
                FakeAuthOutcome {
                    license_owner_user: remote,
                    validation: Ok(()),
                    license: LicenseStatus::DoesNotHaveLicense,
                },
            )
            .unwrap();
        platform.pump(NOW_MS + 2).unwrap();
        platform
            .begin_peer_authentication(
                active_lobby,
                remote,
                &[9],
                AdmissionPurpose::Initial,
                NOW_MS + 2,
            )
            .unwrap();
        platform.pump(NOW_MS + 3).unwrap();
        assert_eq!(
            platform.consume_authenticated_admission(active_lobby, remote, NOW_MS + 3),
            Err(SteamPlatformError::AuthenticationRejected)
        );
        assert!(control.ended_auth_session(remote));
        assert!(drain_events(&mut platform).contains(
            &SteamPlatformEvent::PeerAuthenticationRejected {
                lobby: active_lobby,
                user: remote,
                reason: PeerAuthenticationRejection::DoesNotHaveLicense,
            }
        ));
    }

    #[test]
    fn bounded_event_overflow_faults_and_clears_joinability_state() {
        let local = user(701);
        let (backend, control) = FakeSteamBackend::new(app_id(), local);
        let mut tiny_config = config();
        tiny_config.event_capacity = 2;
        let mut platform = SteamPlatform::new(tiny_config, backend, NOW_MS).unwrap();
        for value in 8_001..=8_003 {
            control.emit_join_request(lobby(value), None).unwrap();
        }
        assert_eq!(
            platform.pump(NOW_MS + 1),
            Err(SteamPlatformError::EventQueueOverflow)
        );
        assert_eq!(platform.state(), SteamPlatformState::Faulted);
        assert_eq!(
            platform.last_fault(),
            Some(SteamPlatformError::EventQueueOverflow)
        );
        assert_eq!(platform.poll_event(), None);
    }

    #[test]
    fn backend_callback_overflow_faults_before_unbounded_drain() {
        let local = user(751);
        let (mut platform, control) = platform(local);
        for offset in 0..=MAX_STEAM_EVENTS {
            control
                .emit_join_request(lobby(20_000 + offset as u64), None)
                .unwrap();
        }
        assert_eq!(
            platform.pump(NOW_MS + 1),
            Err(SteamPlatformError::Backend(
                SteamBackendError::CallbackQueueOverflow
            ))
        );
        assert_eq!(platform.state(), SteamPlatformState::Faulted);
        assert_eq!(platform.poll_event(), None);
    }

    #[test]
    fn platform_time_regression_faults_closed() {
        let local = user(761);
        let (mut platform, _control) = platform(local);
        platform.pump(NOW_MS + 2).unwrap();
        assert_eq!(
            platform.pump(NOW_MS + 1),
            Err(SteamPlatformError::InvalidTimeout)
        );
        assert_eq!(platform.state(), SteamPlatformState::Faulted);
    }

    #[test]
    fn authority_departure_retains_lobby_and_reports_steam_owner_transfer() {
        let local = user(801);
        let owner = user(802);
        let expected_lobby = lobby(9_001);
        let (mut platform, control) = platform(local);
        control
            .seed_lobby(
                expected_lobby,
                &metadata(LobbyVisibility::Private, 2),
                true,
                owner,
                &[FakeLobbyMemberSeed {
                    user: owner,
                    readiness: Some(MemberReadiness::Declared {
                        ready: false,
                        local_seats: 1,
                    }),
                }],
            )
            .unwrap();
        platform
            .join_lobby(
                LobbyJoinIntent {
                    lobby: expected_lobby,
                    origin: JoinOrigin::SteamInvite {
                        friend: Some(owner),
                    },
                    expires_at_ms: NOW_MS + 10,
                },
                1,
                NOW_MS,
            )
            .unwrap();
        platform.pump(NOW_MS + 1).unwrap();
        drain_events(&mut platform);
        control
            .emit_membership_change(expected_lobby, owner, LobbyMembershipChange::Disconnected)
            .unwrap();
        platform.pump(NOW_MS + 2).unwrap();
        assert_eq!(
            platform.state(),
            SteamPlatformState::InLobby(expected_lobby)
        );
        assert_eq!(platform.lobby_owner(), Some(local));
        assert_eq!(platform.roster_len(), 1);
        assert_eq!(platform.roster()[0].map(|member| member.user), Some(local));
        assert_eq!(
            drain_events(&mut platform),
            vec![SteamPlatformEvent::AuthorityLost {
                lobby: expected_lobby,
                previous_authority: owner,
                successor: local,
            }]
        );
    }

    #[test]
    fn authority_transfer_faults_closed_if_immutable_lobby_metadata_changed() {
        let local = user(811);
        let owner = user(812);
        let expected_lobby = lobby(9_002);
        let (mut platform, control) = platform(local);
        control
            .seed_lobby(
                expected_lobby,
                &metadata(LobbyVisibility::Private, 2),
                true,
                owner,
                &[FakeLobbyMemberSeed {
                    user: owner,
                    readiness: Some(MemberReadiness::Declared {
                        ready: false,
                        local_seats: 1,
                    }),
                }],
            )
            .unwrap();
        platform
            .join_lobby(
                LobbyJoinIntent {
                    lobby: expected_lobby,
                    origin: JoinOrigin::SteamInvite {
                        friend: Some(owner),
                    },
                    expires_at_ms: NOW_MS + 10,
                },
                1,
                NOW_MS,
            )
            .unwrap();
        platform.pump(NOW_MS + 1).unwrap();
        drain_events(&mut platform);

        control
            .set_lobby_data_raw(expected_lobby, KEY_RULES, "2")
            .unwrap();
        control
            .emit_membership_change(expected_lobby, owner, LobbyMembershipChange::Disconnected)
            .unwrap();
        assert_eq!(
            platform.pump(NOW_MS + 2),
            Err(SteamPlatformError::MetadataMismatch)
        );
        assert_eq!(platform.state(), SteamPlatformState::Faulted);
        assert_eq!(
            platform.last_fault(),
            Some(SteamPlatformError::MetadataMismatch)
        );
        assert_eq!(platform.poll_event(), None);
    }
}
