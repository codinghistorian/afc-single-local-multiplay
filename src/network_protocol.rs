//! Transport-independent multiplayer protocol primitives.
//!
//! This module deliberately contains no Bevy, Lightyear, Steam, rendering, or raw
//! device-input types. Wire-facing values use fixed-capacity storage. A transport
//! adapter must deserialize, call the relevant `validate` method, and only then pass
//! a message to session or authority code.

use serde::{Deserialize, Serialize};

pub use crate::determinism::{FighterId, SimTick};

pub const SIMULATION_HZ: u16 = 60;
pub const MAX_FIGHTERS: usize = 4;
pub const MAX_SEATS: usize = MAX_FIGHTERS;
pub const MAX_LOCAL_SEATS: u8 = MAX_SEATS as u8;
pub const MAX_INPUT_REDUNDANCY: usize = 6;
pub const MAX_INPUT_FRAMES_PER_WINDOW: usize = MAX_INPUT_REDUNDANCY + 1;
pub const MIN_INPUT_DELAY_TICKS: u8 = 1;
pub const MAX_INPUT_DELAY_TICKS: u8 = 6;
const _: () = assert!(
    MAX_INPUT_DELAY_TICKS as usize + 1 <= MAX_INPUT_FRAMES_PER_WINDOW,
    "the maximum negotiated input lead must fit one complete redundancy window"
);
pub const MAX_NORMAL_ROLLBACK_TICKS: u8 = 12;
pub const MIN_SNAPSHOT_HISTORY_TICKS: u8 = 32;
pub const MAX_HIGH_FREQUENCY_PACKET_BYTES: usize = 1_200;

// Resync is reliable and may span packets, but every chunk still fits below the
// transport packet cap. The 128 KiB total covers the audited 92,157-byte maximum
// production-live snapshot for the fixed pool-capacity contract with 29.7%
// headroom; changing either side requires redoing that exact byte audit.
pub const RESYNC_BLOCK_BYTES: usize = 32;
pub const RESYNC_BLOCKS_PER_CHUNK: usize = 32;
pub const MAX_RESYNC_CHUNK_BYTES: usize = RESYNC_BLOCK_BYTES * RESYNC_BLOCKS_PER_CHUNK;
pub const MAX_RESYNC_SNAPSHOT_BYTES: usize = 128 * 1024;
pub const MAX_RESYNC_CHUNKS: usize = MAX_RESYNC_SNAPSHOT_BYTES / MAX_RESYNC_CHUNK_BYTES;
/// A hard-resync carries the exact continuous-input boundary for the snapshot
/// plus at most four preceding committed ticks.  This is deliberately smaller
/// than the normal seven-frame unreliable relay ceiling so the reliable seed is
/// a single bounded datagram.
pub const MAX_RESYNC_INPUT_TAIL_TICKS: usize = 5;

pub type ProtocolResult<T> = Result<T, ProtocolValidationError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolValidationError {
    ZeroIdentifier,
    ZeroVersion,
    InvalidDefinitionId,
    InvalidSeat,
    InvalidFighter,
    InvalidTeam,
    CapacityExceeded,
    DuplicateSeat,
    DuplicateFighter,
    MissingFighterOwner,
    OwnerForInactiveFighter,
    InvalidLocalSeatCount,
    InvalidAxis,
    UnsupportedButtons,
    EmptyInputWindow,
    InputWindowTooLarge,
    MixedSeatInputWindow,
    NonContiguousInputTicks,
    NonContiguousInputSequences,
    EmptyInputBatch,
    DuplicateInputSeat,
    MatchMismatch,
    PeerMismatch,
    UnownedSeat,
    SeatOwnedByDifferentPeer,
    AuthorityOwnedSeat,
    InvalidTickWindow,
    StaleInput,
    FutureInput,
    ProtocolVersionMismatch,
    SimulationVersionMismatch,
    BuildMismatch,
    ContentMismatch,
    ReplayVersionMismatch,
    InvalidPhaseTransition,
    ExpiredPhaseDeadline,
    InvalidTickRate,
    InvalidInputDelay,
    InvalidRollbackLimit,
    InvalidSnapshotHistory,
    InvalidStartTick,
    UntrustedAuthorityForTrustedResult,
    InvalidManifest,
    InvalidSnapshot,
    EmptySnapshot,
    SnapshotTooLarge,
    InvalidChunkCount,
    InvalidChunkIndex,
    InvalidChunkLength,
    NonCanonicalPadding,
    NonZeroChunkPadding,
    ResyncMismatch,
}

impl core::fmt::Display for ProtocolValidationError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "invalid multiplayer protocol value: {self:?}")
    }
}

impl std::error::Error for ProtocolValidationError {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProtocolVersion(u16);

impl ProtocolVersion {
    pub fn new(value: u16) -> ProtocolResult<Self> {
        let value = Self(value);
        value.validate()?;
        Ok(value)
    }

    pub const fn get(self) -> u16 {
        self.0
    }

    pub fn validate(self) -> ProtocolResult<()> {
        if self.0 == 0 {
            Err(ProtocolValidationError::ZeroVersion)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SimulationVersion(u16);

impl SimulationVersion {
    pub fn new(value: u16) -> ProtocolResult<Self> {
        let value = Self(value);
        value.validate()?;
        Ok(value)
    }

    pub const fn get(self) -> u16 {
        self.0
    }

    pub fn validate(self) -> ProtocolResult<()> {
        if self.0 == 0 {
            Err(ProtocolValidationError::ZeroVersion)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ReplayFormatVersion(u16);

impl ReplayFormatVersion {
    pub fn new(value: u16) -> ProtocolResult<Self> {
        let value = Self(value);
        value.validate()?;
        Ok(value)
    }

    pub const fn get(self) -> u16 {
        self.0
    }

    pub fn validate(self) -> ProtocolResult<()> {
        if self.0 == 0 {
            Err(ProtocolValidationError::ZeroVersion)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BuildId([u8; 16]);

impl BuildId {
    pub fn new(bytes: [u8; 16]) -> ProtocolResult<Self> {
        if bytes.iter().all(|byte| *byte == 0) {
            Err(ProtocolValidationError::ZeroIdentifier)
        } else {
            Ok(Self(bytes))
        }
    }

    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    pub fn validate(&self) -> ProtocolResult<()> {
        Self::new(self.0).map(|_| ())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GameplayContentHash([u8; 32]);

impl GameplayContentHash {
    pub fn new(bytes: [u8; 32]) -> ProtocolResult<Self> {
        if bytes.iter().all(|byte| *byte == 0) {
            Err(ProtocolValidationError::ZeroIdentifier)
        } else {
            Ok(Self(bytes))
        }
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn validate(&self) -> ProtocolResult<()> {
        Self::new(self.0).map(|_| ())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MatchId([u8; 16]);

impl MatchId {
    pub fn new(bytes: [u8; 16]) -> ProtocolResult<Self> {
        if bytes.iter().all(|byte| *byte == 0) {
            Err(ProtocolValidationError::ZeroIdentifier)
        } else {
            Ok(Self(bytes))
        }
    }

    pub fn validate(&self) -> ProtocolResult<()> {
        Self::new(self.0).map(|_| ())
    }

    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PeerId(u64);

impl PeerId {
    pub fn new(value: u64) -> ProtocolResult<Self> {
        if value == 0 {
            Err(ProtocolValidationError::ZeroIdentifier)
        } else {
            Ok(Self(value))
        }
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn validate(self) -> ProtocolResult<()> {
        Self::new(self.0).map(|_| ())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SeatId(u8);

impl SeatId {
    pub fn new(value: u8) -> ProtocolResult<Self> {
        if usize::from(value) < MAX_SEATS {
            Ok(Self(value))
        } else {
            Err(ProtocolValidationError::InvalidSeat)
        }
    }

    pub const fn get(self) -> u8 {
        self.0
    }

    pub fn validate(self) -> ProtocolResult<()> {
        Self::new(self.0).map(|_| ())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TeamId(u8);

impl TeamId {
    pub fn new(value: u8) -> ProtocolResult<Self> {
        if usize::from(value) < MAX_FIGHTERS {
            Ok(Self(value))
        } else {
            Err(ProtocolValidationError::InvalidTeam)
        }
    }

    pub const fn get(self) -> u8 {
        self.0
    }

    pub fn validate(self) -> ProtocolResult<()> {
        Self::new(self.0).map(|_| ())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DefinitionId(u16);

impl DefinitionId {
    pub const INVALID: u16 = u16::MAX;

    pub fn new(value: u16) -> ProtocolResult<Self> {
        if value == Self::INVALID {
            Err(ProtocolValidationError::InvalidDefinitionId)
        } else {
            Ok(Self(value))
        }
    }

    pub const fn get(self) -> u16 {
        self.0
    }

    pub fn validate(self) -> ProtocolResult<()> {
        Self::new(self.0).map(|_| ())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InputSequence(pub u16);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ManifestHash(pub u64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StateHash(pub u64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TransferId(u32);

impl TransferId {
    pub fn new(value: u32) -> ProtocolResult<Self> {
        if value == 0 {
            Err(ProtocolValidationError::ZeroIdentifier)
        } else {
            Ok(Self(value))
        }
    }

    pub const fn get(self) -> u32 {
        self.0
    }

    pub fn validate(self) -> ProtocolResult<()> {
        Self::new(self.0).map(|_| ())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ClockProbeId(u32);

impl ClockProbeId {
    pub fn new(value: u32) -> ProtocolResult<Self> {
        if value == 0 {
            Err(ProtocolValidationError::ZeroIdentifier)
        } else {
            Ok(Self(value))
        }
    }

    pub const fn get(self) -> u32 {
        self.0
    }

    pub fn validate(self) -> ProtocolResult<()> {
        Self::new(self.0).map(|_| ())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatibilityId {
    pub protocol: ProtocolVersion,
    pub simulation: SimulationVersion,
    pub replay: ReplayFormatVersion,
    pub build: BuildId,
    pub gameplay_content: GameplayContentHash,
}

impl CompatibilityId {
    pub fn validate(&self) -> ProtocolResult<()> {
        self.protocol.validate()?;
        self.simulation.validate()?;
        self.replay.validate()?;
        self.build.validate()?;
        self.gameplay_content.validate()
    }

    pub fn validate_against(&self, expected: &Self) -> ProtocolResult<()> {
        self.validate()?;
        expected.validate()?;
        if self.protocol != expected.protocol {
            return Err(ProtocolValidationError::ProtocolVersionMismatch);
        }
        if self.simulation != expected.simulation {
            return Err(ProtocolValidationError::SimulationVersionMismatch);
        }
        if self.build != expected.build {
            return Err(ProtocolValidationError::BuildMismatch);
        }
        if self.gameplay_content != expected.gameplay_content {
            return Err(ProtocolValidationError::ContentMismatch);
        }
        if self.replay != expected.replay {
            return Err(ProtocolValidationError::ReplayVersionMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProtocolChannel {
    Control,
    Input,
    State,
    Resync,
    Result,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Delivery {
    OrderedReliable,
    SequencedUnreliable,
    UnorderedReliable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    Bidirectional,
    AuthorityToClient,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelSpec {
    pub channel: ProtocolChannel,
    pub delivery: Delivery,
    pub direction: Direction,
}

pub const CHANNEL_SPECS: [ChannelSpec; 5] = [
    ChannelSpec {
        channel: ProtocolChannel::Control,
        delivery: Delivery::OrderedReliable,
        direction: Direction::Bidirectional,
    },
    ChannelSpec {
        channel: ProtocolChannel::Input,
        delivery: Delivery::SequencedUnreliable,
        direction: Direction::Bidirectional,
    },
    ChannelSpec {
        channel: ProtocolChannel::State,
        delivery: Delivery::SequencedUnreliable,
        direction: Direction::AuthorityToClient,
    },
    ChannelSpec {
        channel: ProtocolChannel::Resync,
        delivery: Delivery::UnorderedReliable,
        direction: Direction::AuthorityToClient,
    },
    ChannelSpec {
        channel: ProtocolChannel::Result,
        delivery: Delivery::OrderedReliable,
        direction: Direction::AuthorityToClient,
    },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionPhase {
    OfflineMenu,
    Lobby,
    Connecting,
    Authenticating,
    ManifestAgreement,
    Loading,
    InitialSync,
    Ready,
    Countdown,
    Fighting,
    ConfirmingResult,
    Results,
}

impl ConnectionPhase {
    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::OfflineMenu, Self::Lobby)
                | (Self::Lobby, Self::OfflineMenu)
                | (Self::Lobby, Self::Connecting)
                | (Self::Connecting, Self::Authenticating)
                | (Self::Authenticating, Self::ManifestAgreement)
                | (Self::ManifestAgreement, Self::Loading)
                | (Self::Loading, Self::InitialSync)
                | (Self::InitialSync, Self::Ready)
                | (Self::Ready, Self::Countdown)
                | (Self::Countdown, Self::Fighting)
                | (Self::Fighting, Self::ConfirmingResult)
                | (Self::ConfirmingResult, Self::Results)
                | (Self::Results, Self::Lobby)
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseTransition {
    pub from: ConnectionPhase,
    pub to: ConnectionPhase,
    pub deadline_tick: SimTick,
}

impl PhaseTransition {
    pub fn validate(&self, current_tick: SimTick) -> ProtocolResult<()> {
        if !self.from.can_transition_to(self.to) {
            return Err(ProtocolValidationError::InvalidPhaseTransition);
        }
        if self.deadline_tick.0 <= current_tick.0 {
            return Err(ProtocolValidationError::ExpiredPhaseDeadline);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SeatOwner {
    Peer(PeerId),
    AuthorityBot,
}

impl Default for SeatOwner {
    fn default() -> Self {
        Self::AuthorityBot
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatAssignment {
    pub seat: SeatId,
    pub fighter: FighterId,
    pub owner: SeatOwner,
}

impl SeatAssignment {
    pub fn validate(&self) -> ProtocolResult<()> {
        self.seat.validate()?;
        if let SeatOwner::Peer(peer) = self.owner {
            peer.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatOwnership {
    count: u8,
    assignments: [SeatAssignment; MAX_SEATS],
}

impl SeatOwnership {
    pub const fn empty() -> Self {
        Self {
            count: 0,
            assignments: [SeatAssignment {
                seat: SeatId(0),
                fighter: FighterId::ZERO,
                owner: SeatOwner::AuthorityBot,
            }; MAX_SEATS],
        }
    }

    pub fn from_assignments(assignments: &[SeatAssignment]) -> ProtocolResult<Self> {
        let mut ownership = Self::empty();
        for assignment in assignments {
            ownership.push(*assignment)?;
        }
        Ok(ownership)
    }

    pub fn push(&mut self, assignment: SeatAssignment) -> ProtocolResult<()> {
        assignment.validate()?;
        if self.len() >= MAX_SEATS {
            return Err(ProtocolValidationError::CapacityExceeded);
        }
        if self
            .as_slice()
            .iter()
            .any(|existing| existing.seat == assignment.seat)
        {
            return Err(ProtocolValidationError::DuplicateSeat);
        }
        if self
            .as_slice()
            .iter()
            .any(|existing| existing.fighter == assignment.fighter)
        {
            return Err(ProtocolValidationError::DuplicateFighter);
        }
        let next_index = self.len();
        self.assignments[next_index] = assignment;
        self.count += 1;
        Ok(())
    }

    pub const fn len(&self) -> usize {
        self.count as usize
    }

    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn as_slice(&self) -> &[SeatAssignment] {
        &self.assignments[..self.len().min(MAX_SEATS)]
    }

    pub fn validate(&self) -> ProtocolResult<()> {
        if self.len() > MAX_SEATS {
            return Err(ProtocolValidationError::CapacityExceeded);
        }
        for (index, assignment) in self.as_slice().iter().enumerate() {
            assignment.validate()?;
            if self.as_slice()[..index]
                .iter()
                .any(|prior| prior.seat == assignment.seat)
            {
                return Err(ProtocolValidationError::DuplicateSeat);
            }
            if self.as_slice()[..index]
                .iter()
                .any(|prior| prior.fighter == assignment.fighter)
            {
                return Err(ProtocolValidationError::DuplicateFighter);
            }
        }
        if self.assignments[self.len()..]
            .iter()
            .any(|assignment| *assignment != SeatAssignment::default())
        {
            return Err(ProtocolValidationError::NonCanonicalPadding);
        }
        Ok(())
    }

    pub fn assignment_for_seat(&self, seat: SeatId) -> Option<&SeatAssignment> {
        self.as_slice()
            .iter()
            .find(|assignment| assignment.seat == seat)
    }

    pub fn assignment_for_fighter(&self, fighter: FighterId) -> Option<&SeatAssignment> {
        self.as_slice()
            .iter()
            .find(|assignment| assignment.fighter == fighter)
    }

    pub fn validate_peer_input(&self, peer: PeerId, seat: SeatId) -> ProtocolResult<FighterId> {
        self.validate()?;
        peer.validate()?;
        seat.validate()?;
        let assignment = self
            .assignment_for_seat(seat)
            .ok_or(ProtocolValidationError::UnownedSeat)?;
        match assignment.owner {
            SeatOwner::Peer(owner) if owner == peer => Ok(assignment.fighter),
            SeatOwner::Peer(_) => Err(ProtocolValidationError::SeatOwnedByDifferentPeer),
            SeatOwner::AuthorityBot => Err(ProtocolValidationError::AuthorityOwnedSeat),
        }
    }

    pub fn peer_owns_any_seat(&self, peer: PeerId) -> bool {
        self.as_slice()
            .iter()
            .any(|assignment| assignment.owner == SeatOwner::Peer(peer))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuantizedAxis(i8);

impl QuantizedAxis {
    pub const MIN: i8 = -127;
    pub const MAX: i8 = 127;

    pub fn new(value: i8) -> ProtocolResult<Self> {
        if (Self::MIN..=Self::MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err(ProtocolValidationError::InvalidAxis)
        }
    }

    pub const fn get(self) -> i8 {
        self.0
    }

    pub fn validate(self) -> ProtocolResult<()> {
        Self::new(self.0).map(|_| ())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputButtons(u16);

impl InputButtons {
    pub const AIM_GRAB: u16 = 1 << 0;
    pub const LIGHT: u16 = 1 << 1;
    pub const HEAVY: u16 = 1 << 2;
    pub const JUMP: u16 = 1 << 3;
    pub const GUARD: u16 = 1 << 4;
    pub const ULTIMATE: u16 = 1 << 5;
    pub const SPECIAL: u16 = 1 << 6;
    pub const DASH: u16 = 1 << 7;
    /// Raw device-edge information retained separately from the delayed light
    /// action pulse produced by local chord recognition.
    pub const RAW_LIGHT: u16 = 1 << 8;
    /// Raw device-edge information retained separately from the delayed heavy
    /// action pulse produced by local chord recognition.
    pub const RAW_HEAVY: u16 = 1 << 9;
    pub const SUPPORTED_MASK: u16 = Self::AIM_GRAB
        | Self::LIGHT
        | Self::HEAVY
        | Self::JUMP
        | Self::GUARD
        | Self::ULTIMATE
        | Self::SPECIAL
        | Self::DASH
        | Self::RAW_LIGHT
        | Self::RAW_HEAVY;

    pub fn new(bits: u16) -> ProtocolResult<Self> {
        let buttons = Self(bits);
        buttons.validate()?;
        Ok(buttons)
    }

    pub const fn bits(self) -> u16 {
        self.0
    }

    pub fn validate(self) -> ProtocolResult<()> {
        if self.0 & !Self::SUPPORTED_MASK == 0 {
            Ok(())
        } else {
            Err(ProtocolValidationError::UnsupportedButtons)
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputFrame {
    pub tick: SimTick,
    pub seat: SeatId,
    pub movement_x: QuantizedAxis,
    pub movement_y: QuantizedAxis,
    pub held_buttons: InputButtons,
    /// Edge pulses latched by the render-rate sampler for this tick.
    ///
    /// This preserves a complete press/release occurring between simulation
    /// ticks. When the authority substitutes a missing frame, it repeats only
    /// movement and `held_buttons` and clears both edge fields.
    pub pressed_buttons: InputButtons,
    pub released_buttons: InputButtons,
    pub sequence: InputSequence,
}

impl InputFrame {
    // Fixed-width AFC codec intent, excluding outer message/channel framing.
    pub const FIXED_WIRE_BYTES: usize = 8 + 1 + 1 + 1 + 2 + 2 + 2 + 2;

    pub fn validate(&self) -> ProtocolResult<()> {
        self.seat.validate()?;
        self.movement_x.validate()?;
        self.movement_y.validate()?;
        self.held_buttons.validate()?;
        self.pressed_buttons.validate()?;
        self.released_buttons.validate()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatInputWindow {
    count: u8,
    frames: [InputFrame; MAX_INPUT_FRAMES_PER_WINDOW],
}

impl Default for SeatInputWindow {
    fn default() -> Self {
        Self {
            count: 0,
            frames: [InputFrame::default(); MAX_INPUT_FRAMES_PER_WINDOW],
        }
    }
}

impl SeatInputWindow {
    pub const MAX_WIRE_BYTES: usize =
        1 + InputFrame::FIXED_WIRE_BYTES * MAX_INPUT_FRAMES_PER_WINDOW;

    pub fn from_newest_first(frames: &[InputFrame]) -> ProtocolResult<Self> {
        if frames.is_empty() {
            return Err(ProtocolValidationError::EmptyInputWindow);
        }
        if frames.len() > MAX_INPUT_FRAMES_PER_WINDOW {
            return Err(ProtocolValidationError::InputWindowTooLarge);
        }
        let mut window = Self::default();
        window.count = frames.len() as u8;
        window.frames[..frames.len()].copy_from_slice(frames);
        window.validate()?;
        Ok(window)
    }

    pub const fn len(&self) -> usize {
        self.count as usize
    }

    pub fn as_slice(&self) -> &[InputFrame] {
        &self.frames[..self.len().min(MAX_INPUT_FRAMES_PER_WINDOW)]
    }

    pub fn newest(&self) -> Option<&InputFrame> {
        self.as_slice().first()
    }

    pub fn validate(&self) -> ProtocolResult<()> {
        if self.len() == 0 {
            return Err(ProtocolValidationError::EmptyInputWindow);
        }
        if self.len() > MAX_INPUT_FRAMES_PER_WINDOW {
            return Err(ProtocolValidationError::InputWindowTooLarge);
        }
        let newest = self.frames[0];
        newest.validate()?;
        for (offset, frame) in self.as_slice().iter().enumerate() {
            frame.validate()?;
            if frame.seat != newest.seat {
                return Err(ProtocolValidationError::MixedSeatInputWindow);
            }
            let expected_tick = newest
                .tick
                .0
                .checked_sub(offset as u64)
                .ok_or(ProtocolValidationError::NonContiguousInputTicks)?;
            if frame.tick.0 != expected_tick {
                return Err(ProtocolValidationError::NonContiguousInputTicks);
            }
            let expected_sequence = newest.sequence.0.wrapping_sub(offset as u16);
            if frame.sequence.0 != expected_sequence {
                return Err(ProtocolValidationError::NonContiguousInputSequences);
            }
        }
        if self.frames[self.len()..]
            .iter()
            .any(|frame| *frame != InputFrame::default())
        {
            return Err(ProtocolValidationError::NonCanonicalPadding);
        }
        Ok(())
    }
}

/// Provenance of an input after the authority has committed it. Clients never
/// infer this from ownership or acknowledgement metadata: the relay describes
/// the exact frame that was used for canonical simulation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommittedInputSource {
    Peer(PeerId),
    AuthorityBot,
    #[default]
    MissingSubstitute,
}

impl CommittedInputSource {
    pub fn validate(self) -> ProtocolResult<()> {
        match self {
            Self::Peer(peer) => peer.validate(),
            Self::AuthorityBot | Self::MissingSubstitute => Ok(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommittedInputRecord {
    pub frame: InputFrame,
    pub fighter: FighterId,
    pub source: CommittedInputSource,
}

impl CommittedInputRecord {
    /// The codec keeps source identity fixed-width so malformed variable-length
    /// payloads cannot change the parser's record boundaries.
    pub const FIXED_WIRE_BYTES: usize = InputFrame::FIXED_WIRE_BYTES + 1 + 1 + 8;

    pub fn validate(&self) -> ProtocolResult<()> {
        self.frame.validate()?;
        if self.fighter.index() >= MAX_FIGHTERS {
            return Err(ProtocolValidationError::InvalidFighter);
        }
        self.source.validate()
    }
}

/// Newest-first redundant committed inputs for one canonical seat/fighter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommittedSeatInputWindow {
    count: u8,
    records: [CommittedInputRecord; MAX_INPUT_FRAMES_PER_WINDOW],
}

impl Default for CommittedSeatInputWindow {
    fn default() -> Self {
        Self {
            count: 0,
            records: [CommittedInputRecord::default(); MAX_INPUT_FRAMES_PER_WINDOW],
        }
    }
}

impl CommittedSeatInputWindow {
    pub const MAX_WIRE_BYTES: usize =
        1 + CommittedInputRecord::FIXED_WIRE_BYTES * MAX_INPUT_FRAMES_PER_WINDOW;

    pub fn from_newest_first(records: &[CommittedInputRecord]) -> ProtocolResult<Self> {
        if records.is_empty() {
            return Err(ProtocolValidationError::EmptyInputWindow);
        }
        if records.len() > MAX_INPUT_FRAMES_PER_WINDOW {
            return Err(ProtocolValidationError::InputWindowTooLarge);
        }
        let mut window = Self::default();
        window.count = records.len() as u8;
        window.records[..records.len()].copy_from_slice(records);
        window.validate()?;
        Ok(window)
    }

    pub const fn len(&self) -> usize {
        self.count as usize
    }

    pub fn as_slice(&self) -> &[CommittedInputRecord] {
        &self.records[..self.len().min(MAX_INPUT_FRAMES_PER_WINDOW)]
    }

    pub fn newest(&self) -> Option<&CommittedInputRecord> {
        self.as_slice().first()
    }

    pub fn validate(&self) -> ProtocolResult<()> {
        if self.len() == 0 {
            return Err(ProtocolValidationError::EmptyInputWindow);
        }
        if self.len() > MAX_INPUT_FRAMES_PER_WINDOW {
            return Err(ProtocolValidationError::InputWindowTooLarge);
        }
        let newest = self.records[0];
        newest.validate()?;
        for (offset, record) in self.as_slice().iter().enumerate() {
            record.validate()?;
            if record.frame.seat != newest.frame.seat || record.fighter != newest.fighter {
                return Err(ProtocolValidationError::MixedSeatInputWindow);
            }
            let expected_tick = newest
                .frame
                .tick
                .0
                .checked_sub(offset as u64)
                .ok_or(ProtocolValidationError::NonContiguousInputTicks)?;
            if record.frame.tick.0 != expected_tick {
                return Err(ProtocolValidationError::NonContiguousInputTicks);
            }
        }
        if self.records[self.len()..]
            .iter()
            .any(|record| *record != CommittedInputRecord::default())
        {
            return Err(ProtocolValidationError::NonCanonicalPadding);
        }
        Ok(())
    }
}

/// Authority-to-client latest-wins relay of exact committed inputs. Redundant
/// per-seat windows tolerate loss on the sequenced-unreliable input channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommittedInputRelay {
    pub match_id: MatchId,
    pub authority_tick: SimTick,
    window_count: u8,
    windows: [CommittedSeatInputWindow; MAX_SEATS],
}

impl CommittedInputRelay {
    pub const MAX_WIRE_BYTES: usize =
        16 + 8 + 1 + MAX_SEATS * CommittedSeatInputWindow::MAX_WIRE_BYTES;

    pub fn new(
        match_id: MatchId,
        authority_tick: SimTick,
        windows: &[CommittedSeatInputWindow],
    ) -> ProtocolResult<Self> {
        if windows.is_empty() {
            return Err(ProtocolValidationError::EmptyInputBatch);
        }
        if windows.len() > MAX_SEATS {
            return Err(ProtocolValidationError::CapacityExceeded);
        }
        let mut relay = Self {
            match_id,
            authority_tick,
            window_count: windows.len() as u8,
            windows: [CommittedSeatInputWindow::default(); MAX_SEATS],
        };
        relay.windows[..windows.len()].copy_from_slice(windows);
        relay.validate()?;
        Ok(relay)
    }

    pub const fn len(&self) -> usize {
        self.window_count as usize
    }

    pub fn as_slice(&self) -> &[CommittedSeatInputWindow] {
        &self.windows[..self.len().min(MAX_SEATS)]
    }

    pub fn validate(&self) -> ProtocolResult<()> {
        self.match_id.validate()?;
        if self.len() == 0 {
            return Err(ProtocolValidationError::EmptyInputBatch);
        }
        if self.len() > MAX_SEATS {
            return Err(ProtocolValidationError::CapacityExceeded);
        }
        for (index, window) in self.as_slice().iter().enumerate() {
            window.validate()?;
            let newest = window
                .newest()
                .ok_or(ProtocolValidationError::EmptyInputWindow)?;
            if newest.frame.tick != self.authority_tick {
                return Err(ProtocolValidationError::InvalidTickWindow);
            }
            for prior in &self.as_slice()[..index] {
                let prior = prior
                    .newest()
                    .ok_or(ProtocolValidationError::EmptyInputWindow)?;
                if prior.frame.seat == newest.frame.seat {
                    return Err(ProtocolValidationError::DuplicateInputSeat);
                }
                if prior.fighter == newest.fighter {
                    return Err(ProtocolValidationError::DuplicateFighter);
                }
            }
        }
        if self.windows[self.len()..]
            .iter()
            .any(|window| *window != CommittedSeatInputWindow::default())
        {
            return Err(ProtocolValidationError::NonCanonicalPadding);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputTickWindow {
    pub oldest_retained: SimTick,
    pub first_uncommitted: SimTick,
    pub latest_acceptable: SimTick,
}

impl InputTickWindow {
    pub fn new(
        oldest_retained: SimTick,
        first_uncommitted: SimTick,
        latest_acceptable: SimTick,
    ) -> ProtocolResult<Self> {
        if oldest_retained.0 > first_uncommitted.0 || first_uncommitted.0 > latest_acceptable.0 {
            return Err(ProtocolValidationError::InvalidTickWindow);
        }
        Ok(Self {
            oldest_retained,
            first_uncommitted,
            latest_acceptable,
        })
    }

    pub fn validate_new_input_tick(&self, tick: SimTick) -> ProtocolResult<()> {
        if tick.0 < self.first_uncommitted.0 {
            Err(ProtocolValidationError::StaleInput)
        } else if tick.0 > self.latest_acceptable.0 {
            Err(ProtocolValidationError::FutureInput)
        } else {
            Ok(())
        }
    }

    pub fn validate_redundant_tick(&self, tick: SimTick) -> ProtocolResult<()> {
        if tick.0 < self.oldest_retained.0 {
            Err(ProtocolValidationError::StaleInput)
        } else if tick.0 > self.latest_acceptable.0 {
            Err(ProtocolValidationError::FutureInput)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateBaselineAck {
    pub tick: SimTick,
    pub hash: StateHash,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputBatch {
    pub match_id: MatchId,
    pub peer_id: PeerId,
    window_count: u8,
    windows: [SeatInputWindow; MAX_LOCAL_SEATS as usize],
    state_baseline_ack: Option<StateBaselineAck>,
}

impl InputBatch {
    pub const MAX_WIRE_BYTES: usize =
        16 + 8 + 1 + SeatInputWindow::MAX_WIRE_BYTES * MAX_LOCAL_SEATS as usize + 1 + 8 + 8;

    pub fn new(
        match_id: MatchId,
        peer_id: PeerId,
        windows: &[SeatInputWindow],
    ) -> ProtocolResult<Self> {
        if windows.is_empty() {
            return Err(ProtocolValidationError::EmptyInputBatch);
        }
        if windows.len() > MAX_LOCAL_SEATS as usize {
            return Err(ProtocolValidationError::CapacityExceeded);
        }
        let mut batch = Self {
            match_id,
            peer_id,
            window_count: windows.len() as u8,
            windows: [SeatInputWindow::default(); MAX_LOCAL_SEATS as usize],
            state_baseline_ack: None,
        };
        batch.windows[..windows.len()].copy_from_slice(windows);
        batch.validate_structure()?;
        Ok(batch)
    }

    pub const fn len(&self) -> usize {
        self.window_count as usize
    }

    pub fn as_slice(&self) -> &[SeatInputWindow] {
        &self.windows[..self.len().min(MAX_LOCAL_SEATS as usize)]
    }

    pub const fn state_baseline_ack(&self) -> Option<StateBaselineAck> {
        self.state_baseline_ack
    }

    pub fn with_state_baseline_ack(
        mut self,
        acknowledgement: StateBaselineAck,
    ) -> ProtocolResult<Self> {
        self.state_baseline_ack = Some(acknowledgement);
        self.validate_structure()?;
        Ok(self)
    }

    pub fn validate_structure(&self) -> ProtocolResult<()> {
        self.match_id.validate()?;
        self.peer_id.validate()?;
        if self.len() == 0 {
            return Err(ProtocolValidationError::EmptyInputBatch);
        }
        if self.len() > MAX_LOCAL_SEATS as usize {
            return Err(ProtocolValidationError::CapacityExceeded);
        }
        for (index, window) in self.as_slice().iter().enumerate() {
            window.validate()?;
            let seat = window
                .newest()
                .ok_or(ProtocolValidationError::EmptyInputWindow)?
                .seat;
            if self.as_slice()[..index]
                .iter()
                .filter_map(SeatInputWindow::newest)
                .any(|prior| prior.seat == seat)
            {
                return Err(ProtocolValidationError::DuplicateInputSeat);
            }
        }
        if self.windows[self.len()..]
            .iter()
            .any(|window| *window != SeatInputWindow::default())
        {
            return Err(ProtocolValidationError::NonCanonicalPadding);
        }
        Ok(())
    }

    pub fn validate_for(
        &self,
        expected_match: MatchId,
        connected_peer: PeerId,
        ownership: &SeatOwnership,
        ticks: &InputTickWindow,
    ) -> ProtocolResult<()> {
        self.validate_structure()?;
        expected_match.validate()?;
        connected_peer.validate()?;
        ownership.validate()?;
        if self.match_id != expected_match {
            return Err(ProtocolValidationError::MatchMismatch);
        }
        if self.peer_id != connected_peer {
            return Err(ProtocolValidationError::PeerMismatch);
        }
        for window in self.as_slice() {
            let newest = window
                .newest()
                .ok_or(ProtocolValidationError::EmptyInputWindow)?;
            ownership.validate_peer_input(connected_peer, newest.seat)?;
            ticks.validate_new_input_tick(newest.tick)?;
            for redundant in window.as_slice().iter().skip(1) {
                ticks.validate_redundant_tick(redundant.tick)?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthorityKind {
    Offline,
    Listen,
    Dedicated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconnectClaim {
    pub match_id: MatchId,
    pub peer_id: PeerId,
    pub last_confirmed_tick: SimTick,
}

impl ReconnectClaim {
    pub fn validate(&self) -> ProtocolResult<()> {
        self.match_id.validate()?;
        self.peer_id.validate()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LobbyJoinRequest {
    pub compatibility: CompatibilityId,
    pub requested_local_seats: u8,
    pub reconnect: Option<ReconnectClaim>,
}

impl LobbyJoinRequest {
    pub fn validate(&self, expected: &CompatibilityId) -> ProtocolResult<()> {
        self.compatibility.validate_against(expected)?;
        if self.requested_local_seats == 0 || self.requested_local_seats > MAX_LOCAL_SEATS {
            return Err(ProtocolValidationError::InvalidLocalSeatCount);
        }
        if let Some(reconnect) = self.reconnect {
            reconnect.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LobbyAdmission {
    pub peer_id: PeerId,
    pub authority: AuthorityKind,
    pub maximum_local_seats: u8,
}

impl LobbyAdmission {
    pub fn validate(&self) -> ProtocolResult<()> {
        self.peer_id.validate()?;
        if self.maximum_local_seats == 0 || self.maximum_local_seats > MAX_LOCAL_SEATS {
            return Err(ProtocolValidationError::InvalidLocalSeatCount);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LobbyReadiness {
    pub peer_id: PeerId,
    pub ready: bool,
}

impl LobbyReadiness {
    pub fn validate(&self) -> ProtocolResult<()> {
        self.peer_id.validate()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LobbyMessage {
    Join(LobbyJoinRequest),
    Admitted(LobbyAdmission),
    Ready(LobbyReadiness),
    Leave { peer_id: PeerId },
}

impl LobbyMessage {
    pub fn validate(&self, expected: &CompatibilityId) -> ProtocolResult<()> {
        match self {
            Self::Join(message) => message.validate(expected),
            Self::Admitted(message) => message.validate(),
            Self::Ready(message) => message.validate(),
            Self::Leave { peer_id } => peer_id.validate(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FighterSlotConfig {
    pub occupied: bool,
    pub fighter: FighterId,
    pub team: TeamId,
    pub character: DefinitionId,
    pub style: DefinitionId,
    pub equipment: DefinitionId,
}

impl FighterSlotConfig {
    pub fn validate(&self) -> ProtocolResult<()> {
        if !self.occupied {
            return Ok(());
        }
        self.team.validate()?;
        self.character.validate()?;
        self.style.validate()?;
        self.equipment.validate()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchManifest {
    pub compatibility: CompatibilityId,
    pub manifest_hash: ManifestHash,
    pub match_id: MatchId,
    pub authority: AuthorityKind,
    pub trusted_results: bool,
    pub arena: DefinitionId,
    pub rules: DefinitionId,
    pub slots: [FighterSlotConfig; MAX_FIGHTERS],
    pub ownership: SeatOwnership,
    pub master_gameplay_seed: u64,
    pub rng_scheme_version: u16,
    pub tick_rate_hz: u16,
    pub input_delay_ticks: u8,
    pub rollback_limit_ticks: u8,
    pub snapshot_history_ticks: u8,
    pub agreed_start_tick: SimTick,
}

impl MatchManifest {
    pub fn validate(&self) -> ProtocolResult<()> {
        self.compatibility.validate()?;
        self.match_id.validate()?;
        self.arena.validate()?;
        self.rules.validate()?;
        self.ownership.validate()?;
        if self.trusted_results && self.authority != AuthorityKind::Dedicated {
            return Err(ProtocolValidationError::UntrustedAuthorityForTrustedResult);
        }
        if self.rng_scheme_version == 0 {
            return Err(ProtocolValidationError::ZeroVersion);
        }
        if self.tick_rate_hz != SIMULATION_HZ {
            return Err(ProtocolValidationError::InvalidTickRate);
        }
        if !(MIN_INPUT_DELAY_TICKS..=MAX_INPUT_DELAY_TICKS).contains(&self.input_delay_ticks) {
            return Err(ProtocolValidationError::InvalidInputDelay);
        }
        if self.rollback_limit_ticks == 0 || self.rollback_limit_ticks > MAX_NORMAL_ROLLBACK_TICKS {
            return Err(ProtocolValidationError::InvalidRollbackLimit);
        }
        if self.snapshot_history_ticks < MIN_SNAPSHOT_HISTORY_TICKS
            || self.snapshot_history_ticks < self.rollback_limit_ticks
        {
            return Err(ProtocolValidationError::InvalidSnapshotHistory);
        }

        let mut occupied_count = 0;
        for (index, slot) in self.slots.iter().enumerate() {
            if slot.occupied {
                occupied_count += 1;
                slot.validate()?;
                if usize::from(slot.fighter.get()) != index {
                    return Err(ProtocolValidationError::InvalidManifest);
                }
                if self
                    .ownership
                    .assignment_for_fighter(slot.fighter)
                    .is_none()
                {
                    return Err(ProtocolValidationError::MissingFighterOwner);
                }
            } else {
                if *slot != FighterSlotConfig::default() {
                    return Err(ProtocolValidationError::NonCanonicalPadding);
                }
                if self
                    .ownership
                    .assignment_for_fighter(
                        FighterId::from_index(index)
                            .expect("manifest slot indices are bounded by fighter capacity"),
                    )
                    .is_some()
                {
                    return Err(ProtocolValidationError::OwnerForInactiveFighter);
                }
            }
        }
        if occupied_count != self.ownership.len() {
            return Err(ProtocolValidationError::InvalidManifest);
        }
        Ok(())
    }

    pub fn validate_for_start(&self, current_tick: SimTick) -> ProtocolResult<()> {
        self.validate()?;
        if self.agreed_start_tick.0 <= current_tick.0 {
            Err(ProtocolValidationError::InvalidStartTick)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StartMessage {
    Manifest(MatchManifest),
    ManifestAccepted {
        match_id: MatchId,
        peer_id: PeerId,
        manifest_hash: ManifestHash,
    },
    InitialSyncApplied {
        match_id: MatchId,
        peer_id: PeerId,
        snapshot_tick: SimTick,
        snapshot_hash: StateHash,
    },
    Ready {
        match_id: MatchId,
        peer_id: PeerId,
    },
    Countdown {
        match_id: MatchId,
        start_tick: SimTick,
    },
}

impl StartMessage {
    pub fn validate(&self) -> ProtocolResult<()> {
        match self {
            Self::Manifest(manifest) => manifest.validate(),
            Self::ManifestAccepted {
                match_id, peer_id, ..
            }
            | Self::InitialSyncApplied {
                match_id, peer_id, ..
            }
            | Self::Ready { match_id, peer_id } => {
                match_id.validate()?;
                peer_id.validate()
            }
            Self::Countdown {
                match_id,
                start_tick,
            } => {
                match_id.validate()?;
                if start_tick.0 == 0 {
                    Err(ProtocolValidationError::InvalidStartTick)
                } else {
                    Ok(())
                }
            }
        }
    }

    pub fn validate_against_manifest(&self, manifest: &MatchManifest) -> ProtocolResult<()> {
        self.validate()?;
        manifest.validate()?;
        match self {
            Self::Manifest(candidate) => {
                if candidate.match_id != manifest.match_id
                    || candidate.manifest_hash != manifest.manifest_hash
                {
                    Err(ProtocolValidationError::MatchMismatch)
                } else {
                    Ok(())
                }
            }
            Self::ManifestAccepted {
                match_id,
                peer_id,
                manifest_hash,
            } => {
                validate_match_and_peer(manifest, *match_id, *peer_id)?;
                if *manifest_hash != manifest.manifest_hash {
                    Err(ProtocolValidationError::InvalidManifest)
                } else {
                    Ok(())
                }
            }
            Self::InitialSyncApplied {
                match_id, peer_id, ..
            }
            | Self::Ready { match_id, peer_id } => {
                validate_match_and_peer(manifest, *match_id, *peer_id)
            }
            Self::Countdown {
                match_id,
                start_tick,
            } => {
                if *match_id != manifest.match_id {
                    return Err(ProtocolValidationError::MatchMismatch);
                }
                // The manifest value is the earliest acceptable boundary. The
                // authority chooses the actual future boundary only after every
                // peer has finished loading and declared readiness.
                if *start_tick < manifest.agreed_start_tick {
                    return Err(ProtocolValidationError::InvalidStartTick);
                }
                Ok(())
            }
        }
    }
}

/// Client-to-authority clock sample request. Local timestamps deliberately stay
/// out of the packet; the client records them beside this opaque probe ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockProbe {
    pub match_id: MatchId,
    pub peer_id: PeerId,
    pub probe_id: ClockProbeId,
}

impl ClockProbe {
    pub fn validate(&self) -> ProtocolResult<()> {
        self.match_id.validate()?;
        self.peer_id.validate()?;
        self.probe_id.validate()
    }
}

/// Authority response captured at a canonical network tick. A client combines
/// this value with its local send/receive instants to estimate tick phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockReply {
    pub match_id: MatchId,
    pub peer_id: PeerId,
    pub probe_id: ClockProbeId,
    pub authority_tick: SimTick,
}

impl ClockReply {
    pub fn validate(&self) -> ProtocolResult<()> {
        self.match_id.validate()?;
        self.peer_id.validate()?;
        self.probe_id.validate()
    }
}

fn validate_match_and_peer(
    manifest: &MatchManifest,
    match_id: MatchId,
    peer_id: PeerId,
) -> ProtocolResult<()> {
    if match_id != manifest.match_id {
        return Err(ProtocolValidationError::MatchMismatch);
    }
    if !manifest.ownership.peer_owns_any_seat(peer_id) {
        return Err(ProtocolValidationError::UnownedSeat);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResyncReason {
    InitialSync,
    Reconnect,
    HashMismatch,
    HistoryExpired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResyncRequest {
    pub match_id: MatchId,
    pub peer_id: PeerId,
    pub reason: ResyncReason,
    pub last_confirmed_tick: SimTick,
    pub last_confirmed_hash: StateHash,
}

impl ResyncRequest {
    pub fn validate(&self) -> ProtocolResult<()> {
        self.match_id.validate()?;
        self.peer_id.validate()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResyncBegin {
    pub match_id: MatchId,
    pub transfer_id: TransferId,
    pub snapshot_tick: SimTick,
    pub snapshot_hash: StateHash,
    pub snapshot_bytes: u32,
    pub chunk_count: u16,
    pub recent_input_start: SimTick,
    pub recent_input_end: SimTick,
}

impl ResyncBegin {
    pub fn validate(&self) -> ProtocolResult<()> {
        self.match_id.validate()?;
        self.transfer_id.validate()?;
        let snapshot_bytes = self.snapshot_bytes as usize;
        if snapshot_bytes == 0 {
            return Err(ProtocolValidationError::EmptySnapshot);
        }
        if snapshot_bytes > MAX_RESYNC_SNAPSHOT_BYTES {
            return Err(ProtocolValidationError::SnapshotTooLarge);
        }
        let expected_chunks = snapshot_bytes.div_ceil(MAX_RESYNC_CHUNK_BYTES);
        if usize::from(self.chunk_count) != expected_chunks
            || expected_chunks == 0
            || expected_chunks > MAX_RESYNC_CHUNKS
        {
            return Err(ProtocolValidationError::InvalidChunkCount);
        }
        if self.recent_input_start.0 > self.recent_input_end.0
            || self.recent_input_end != self.snapshot_tick
            || self
                .recent_input_end
                .0
                .saturating_sub(self.recent_input_start.0)
                >= MAX_RESYNC_INPUT_TAIL_TICKS as u64
        {
            return Err(ProtocolValidationError::InvalidTickWindow);
        }
        Ok(())
    }

    pub fn expected_chunk_len(&self, chunk_index: u16) -> ProtocolResult<usize> {
        self.validate()?;
        if chunk_index >= self.chunk_count {
            return Err(ProtocolValidationError::InvalidChunkIndex);
        }
        let offset = usize::from(chunk_index) * MAX_RESYNC_CHUNK_BYTES;
        Ok((self.snapshot_bytes as usize - offset).min(MAX_RESYNC_CHUNK_BYTES))
    }
}

/// Reliable, canonical input seed associated with one resync snapshot.
///
/// Every occupied seat has the same inclusive newest-first tick range, ending
/// exactly at `snapshot_tick`. Tick zero is represented by one explicit neutral
/// record per seat; an empty or implicit seed is never accepted. The fixed
/// backing arrays make hostile counts allocation-free and canonical padding is
/// checked by the reused committed-window type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResyncInputTail {
    pub match_id: MatchId,
    pub transfer_id: TransferId,
    pub snapshot_tick: SimTick,
    pub snapshot_hash: StateHash,
    pub recent_input_start: SimTick,
    pub recent_input_end: SimTick,
    window_count: u8,
    windows: [CommittedSeatInputWindow; MAX_SEATS],
}

impl ResyncInputTail {
    pub const MAX_WIRE_BYTES: usize = 16
        + 4
        + 8
        + 8
        + 8
        + 8
        + 1
        + MAX_SEATS * (1 + MAX_RESYNC_INPUT_TAIL_TICKS * CommittedInputRecord::FIXED_WIRE_BYTES);

    pub fn new(begin: &ResyncBegin, windows: &[CommittedSeatInputWindow]) -> ProtocolResult<Self> {
        begin.validate()?;
        Self::from_parts(
            begin.match_id,
            begin.transfer_id,
            begin.snapshot_tick,
            begin.snapshot_hash,
            begin.recent_input_start,
            begin.recent_input_end,
            windows,
        )
    }

    pub fn from_parts(
        match_id: MatchId,
        transfer_id: TransferId,
        snapshot_tick: SimTick,
        snapshot_hash: StateHash,
        recent_input_start: SimTick,
        recent_input_end: SimTick,
        windows: &[CommittedSeatInputWindow],
    ) -> ProtocolResult<Self> {
        if windows.is_empty() || windows.len() > MAX_SEATS {
            return Err(if windows.is_empty() {
                ProtocolValidationError::EmptyInputBatch
            } else {
                ProtocolValidationError::CapacityExceeded
            });
        }
        let mut tail = Self {
            match_id,
            transfer_id,
            snapshot_tick,
            snapshot_hash,
            recent_input_start,
            recent_input_end,
            window_count: windows.len() as u8,
            windows: [CommittedSeatInputWindow::default(); MAX_SEATS],
        };
        tail.windows[..windows.len()].copy_from_slice(windows);
        tail.validate()?;
        Ok(tail)
    }

    pub const fn len(&self) -> usize {
        self.window_count as usize
    }

    pub const fn is_empty(&self) -> bool {
        self.window_count == 0
    }

    pub fn as_slice(&self) -> &[CommittedSeatInputWindow] {
        &self.windows[..self.len().min(MAX_SEATS)]
    }

    pub fn validate(&self) -> ProtocolResult<()> {
        self.match_id.validate()?;
        self.transfer_id.validate()?;
        if self.is_empty() {
            return Err(ProtocolValidationError::EmptyInputBatch);
        }
        if self.len() > MAX_SEATS {
            return Err(ProtocolValidationError::CapacityExceeded);
        }
        if self.recent_input_end != self.snapshot_tick
            || self.recent_input_start > self.recent_input_end
        {
            return Err(ProtocolValidationError::InvalidTickWindow);
        }
        let range_len = self
            .recent_input_end
            .0
            .checked_sub(self.recent_input_start.0)
            .and_then(|span| span.checked_add(1))
            .ok_or(ProtocolValidationError::InvalidTickWindow)? as usize;
        if range_len == 0 || range_len > MAX_RESYNC_INPUT_TAIL_TICKS {
            return Err(ProtocolValidationError::InvalidTickWindow);
        }
        for (index, window) in self.as_slice().iter().enumerate() {
            window.validate()?;
            if window.len() != range_len {
                return Err(ProtocolValidationError::InvalidTickWindow);
            }
            let newest = window
                .newest()
                .ok_or(ProtocolValidationError::EmptyInputWindow)?;
            let oldest = window
                .as_slice()
                .last()
                .ok_or(ProtocolValidationError::EmptyInputWindow)?;
            if newest.frame.tick != self.recent_input_end
                || oldest.frame.tick != self.recent_input_start
            {
                return Err(ProtocolValidationError::InvalidTickWindow);
            }
            for previous in &self.as_slice()[..index] {
                let previous = previous
                    .newest()
                    .ok_or(ProtocolValidationError::EmptyInputWindow)?;
                if previous.frame.seat == newest.frame.seat {
                    return Err(ProtocolValidationError::DuplicateInputSeat);
                }
                if previous.fighter == newest.fighter {
                    return Err(ProtocolValidationError::DuplicateFighter);
                }
            }
        }
        if self.windows[self.len()..]
            .iter()
            .any(|window| *window != CommittedSeatInputWindow::default())
        {
            return Err(ProtocolValidationError::NonCanonicalPadding);
        }
        Ok(())
    }

    pub fn validate_against(&self, begin: &ResyncBegin) -> ProtocolResult<()> {
        self.validate()?;
        begin.validate()?;
        if self.match_id != begin.match_id
            || self.transfer_id != begin.transfer_id
            || self.snapshot_tick != begin.snapshot_tick
            || self.snapshot_hash != begin.snapshot_hash
            || self.recent_input_start != begin.recent_input_start
            || self.recent_input_end != begin.recent_input_end
        {
            return Err(ProtocolValidationError::ResyncMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResyncChunkPayload {
    blocks: [[u8; RESYNC_BLOCK_BYTES]; RESYNC_BLOCKS_PER_CHUNK],
}

impl Default for ResyncChunkPayload {
    fn default() -> Self {
        Self {
            blocks: [[0; RESYNC_BLOCK_BYTES]; RESYNC_BLOCKS_PER_CHUNK],
        }
    }
}

impl ResyncChunkPayload {
    pub fn from_bytes(bytes: &[u8]) -> ProtocolResult<(Self, u16)> {
        if bytes.is_empty() || bytes.len() > MAX_RESYNC_CHUNK_BYTES {
            return Err(ProtocolValidationError::InvalidChunkLength);
        }
        let mut payload = Self::default();
        for (index, byte) in bytes.iter().copied().enumerate() {
            payload.blocks[index / RESYNC_BLOCK_BYTES][index % RESYNC_BLOCK_BYTES] = byte;
        }
        Ok((payload, bytes.len() as u16))
    }

    pub fn copy_prefix_into(&self, length: u16, output: &mut [u8]) -> ProtocolResult<usize> {
        self.validate_padding(length)?;
        let length = usize::from(length);
        if output.len() < length {
            return Err(ProtocolValidationError::CapacityExceeded);
        }
        for index in 0..length {
            output[index] = self.blocks[index / RESYNC_BLOCK_BYTES][index % RESYNC_BLOCK_BYTES];
        }
        Ok(length)
    }

    pub fn validate_padding(&self, length: u16) -> ProtocolResult<()> {
        let length = usize::from(length);
        if length == 0 || length > MAX_RESYNC_CHUNK_BYTES {
            return Err(ProtocolValidationError::InvalidChunkLength);
        }
        for index in length..MAX_RESYNC_CHUNK_BYTES {
            if self.blocks[index / RESYNC_BLOCK_BYTES][index % RESYNC_BLOCK_BYTES] != 0 {
                return Err(ProtocolValidationError::NonZeroChunkPadding);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResyncChunk {
    pub match_id: MatchId,
    pub transfer_id: TransferId,
    pub snapshot_tick: SimTick,
    pub snapshot_hash: StateHash,
    pub chunk_index: u16,
    pub chunk_count: u16,
    pub payload_len: u16,
    pub payload: ResyncChunkPayload,
}

impl ResyncChunk {
    // Fixed-width AFC codec intent, excluding channel/transport framing.
    pub const MAX_WIRE_BYTES: usize = 16 + 4 + 8 + 8 + 2 + 2 + 2 + MAX_RESYNC_CHUNK_BYTES;

    pub fn validate(&self) -> ProtocolResult<()> {
        self.match_id.validate()?;
        self.transfer_id.validate()?;
        if self.chunk_count == 0 || usize::from(self.chunk_count) > MAX_RESYNC_CHUNKS {
            return Err(ProtocolValidationError::InvalidChunkCount);
        }
        if self.chunk_index >= self.chunk_count {
            return Err(ProtocolValidationError::InvalidChunkIndex);
        }
        self.payload.validate_padding(self.payload_len)
    }

    pub fn validate_against(&self, begin: &ResyncBegin) -> ProtocolResult<()> {
        self.validate()?;
        begin.validate()?;
        if self.match_id != begin.match_id
            || self.transfer_id != begin.transfer_id
            || self.snapshot_tick != begin.snapshot_tick
            || self.snapshot_hash != begin.snapshot_hash
            || self.chunk_count != begin.chunk_count
        {
            return Err(ProtocolValidationError::ResyncMismatch);
        }
        if usize::from(self.payload_len) != begin.expected_chunk_len(self.chunk_index)? {
            return Err(ProtocolValidationError::InvalidChunkLength);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResyncApplied {
    pub match_id: MatchId,
    pub transfer_id: TransferId,
    pub peer_id: PeerId,
    pub snapshot_tick: SimTick,
    pub snapshot_hash: StateHash,
}

impl ResyncApplied {
    pub fn validate(&self) -> ProtocolResult<()> {
        self.match_id.validate()?;
        self.transfer_id.validate()?;
        self.peer_id.validate()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResyncMessage {
    Begin(ResyncBegin),
    Chunk(ResyncChunk),
    InputTail(ResyncInputTail),
}

impl ResyncMessage {
    pub fn validate(&self) -> ProtocolResult<()> {
        match self {
            Self::Begin(message) => message.validate(),
            Self::Chunk(message) => message.validate(),
            Self::InputTail(message) => message.validate(),
        }
    }
}

/// Resync negotiation sent on the bidirectional Control channel. Bulk snapshot
/// delivery remains authority-to-client on the Resync channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResyncControlMessage {
    Request(ResyncRequest),
    Applied(ResyncApplied),
}

impl ResyncControlMessage {
    pub fn validate(&self) -> ProtocolResult<()> {
        match self {
            Self::Request(message) => message.validate(),
            Self::Applied(message) => message.validate(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DisconnectCode {
    ClientRequested,
    Timeout,
    AuthenticationFailed,
    OwnershipFailed,
    IncompatibleProtocol,
    IncompatibleSimulation,
    IncompatibleBuild,
    IncompatibleContent,
    InvalidInput,
    MalformedTraffic,
    RateLimited,
    Kicked,
    AuthorityLost,
    ServerShutdown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RetryDisposition {
    ReturnToLobby,
    ReconnectAllowed,
    MatchEndedNoContest,
    Fatal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisconnectMessage {
    pub match_id: Option<MatchId>,
    pub code: DisconnectCode,
    pub retry: RetryDisposition,
    // Stable localization/diagnostic lookup, never an unbounded wire string.
    pub detail_code: u16,
    pub last_confirmed_tick: Option<SimTick>,
}

impl DisconnectMessage {
    pub fn validate(&self) -> ProtocolResult<()> {
        if let Some(match_id) = self.match_id {
            match_id.validate()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compatibility() -> CompatibilityId {
        CompatibilityId {
            protocol: ProtocolVersion::new(1).unwrap(),
            simulation: SimulationVersion::new(1).unwrap(),
            replay: ReplayFormatVersion::new(1).unwrap(),
            build: BuildId::new([1; 16]).unwrap(),
            gameplay_content: GameplayContentHash::new([2; 32]).unwrap(),
        }
    }

    fn match_id() -> MatchId {
        MatchId::new([3; 16]).unwrap()
    }

    fn human_assignment(seat: u8, fighter: u8, peer: PeerId) -> SeatAssignment {
        SeatAssignment {
            seat: SeatId::new(seat).unwrap(),
            fighter: FighterId::new(fighter).unwrap(),
            owner: SeatOwner::Peer(peer),
        }
    }

    fn frame(tick: u64, seat: u8, sequence: u16) -> InputFrame {
        InputFrame {
            tick: SimTick(tick),
            seat: SeatId::new(seat).unwrap(),
            movement_x: QuantizedAxis::new(12).unwrap(),
            movement_y: QuantizedAxis::new(-12).unwrap(),
            held_buttons: InputButtons::new(InputButtons::LIGHT | InputButtons::JUMP).unwrap(),
            pressed_buttons: InputButtons::new(InputButtons::LIGHT).unwrap(),
            released_buttons: InputButtons::default(),
            sequence: InputSequence(sequence),
        }
    }

    fn full_window(newest_tick: u64, seat: u8, newest_sequence: u16) -> SeatInputWindow {
        let frames = std::array::from_fn::<_, MAX_INPUT_FRAMES_PER_WINDOW, _>(|offset| {
            frame(
                newest_tick - offset as u64,
                seat,
                newest_sequence.wrapping_sub(offset as u16),
            )
        });
        SeatInputWindow::from_newest_first(&frames).unwrap()
    }

    fn manifest(authority: AuthorityKind, trusted_results: bool) -> MatchManifest {
        let peer = PeerId::new(7).unwrap();
        let ownership = SeatOwnership::from_assignments(&[
            human_assignment(0, 0, peer),
            SeatAssignment {
                seat: SeatId::new(1).unwrap(),
                fighter: FighterId::new(1).unwrap(),
                owner: SeatOwner::AuthorityBot,
            },
        ])
        .unwrap();
        let mut slots = [FighterSlotConfig::default(); MAX_FIGHTERS];
        for index in 0..2 {
            slots[index] = FighterSlotConfig {
                occupied: true,
                fighter: FighterId::new(index as u8).unwrap(),
                team: TeamId::new(index as u8).unwrap(),
                character: DefinitionId::new(index as u16).unwrap(),
                style: DefinitionId::new(0).unwrap(),
                equipment: DefinitionId::new(0).unwrap(),
            };
        }
        MatchManifest {
            compatibility: compatibility(),
            manifest_hash: ManifestHash(9),
            match_id: match_id(),
            authority,
            trusted_results,
            arena: DefinitionId::new(0).unwrap(),
            rules: DefinitionId::new(0).unwrap(),
            slots,
            ownership,
            master_gameplay_seed: 42,
            rng_scheme_version: 1,
            tick_rate_hz: SIMULATION_HZ,
            input_delay_ticks: 2,
            rollback_limit_ticks: MAX_NORMAL_ROLLBACK_TICKS,
            snapshot_history_ticks: MIN_SNAPSHOT_HISTORY_TICKS,
            agreed_start_tick: SimTick(120),
        }
    }

    #[test]
    fn identifier_and_index_caps_fail_closed() {
        assert_eq!(
            ProtocolVersion::new(0),
            Err(ProtocolValidationError::ZeroVersion)
        );
        assert_eq!(
            BuildId::new([0; 16]),
            Err(ProtocolValidationError::ZeroIdentifier)
        );
        assert!(SeatId::new((MAX_SEATS - 1) as u8).is_ok());
        assert_eq!(
            SeatId::new(MAX_SEATS as u8),
            Err(ProtocolValidationError::InvalidSeat)
        );
        assert!(FighterId::new((MAX_FIGHTERS - 1) as u8).is_some());
        assert_eq!(FighterId::new(MAX_FIGHTERS as u8), None);
        assert!(TeamId::new((MAX_FIGHTERS - 1) as u8).is_ok());
        assert_eq!(
            TeamId::new(MAX_FIGHTERS as u8),
            Err(ProtocolValidationError::InvalidTeam)
        );
        assert_eq!(
            DefinitionId::new(DefinitionId::INVALID),
            Err(ProtocolValidationError::InvalidDefinitionId)
        );
    }

    #[test]
    fn connection_phases_follow_the_documented_state_machine() {
        assert!(ConnectionPhase::Lobby.can_transition_to(ConnectionPhase::Connecting));
        assert!(ConnectionPhase::Results.can_transition_to(ConnectionPhase::Lobby));
        assert!(!ConnectionPhase::Lobby.can_transition_to(ConnectionPhase::Fighting));
        assert_eq!(
            PhaseTransition {
                from: ConnectionPhase::Lobby,
                to: ConnectionPhase::Fighting,
                deadline_tick: SimTick(20),
            }
            .validate(SimTick(10)),
            Err(ProtocolValidationError::InvalidPhaseTransition)
        );
    }

    #[test]
    fn compatibility_rejects_every_matchmaking_boundary() {
        let expected = compatibility();
        let mut candidate = expected;
        candidate.protocol = ProtocolVersion::new(2).unwrap();
        assert_eq!(
            candidate.validate_against(&expected),
            Err(ProtocolValidationError::ProtocolVersionMismatch)
        );
        candidate = expected;
        candidate.simulation = SimulationVersion::new(2).unwrap();
        assert_eq!(
            candidate.validate_against(&expected),
            Err(ProtocolValidationError::SimulationVersionMismatch)
        );
        candidate = expected;
        candidate.build = BuildId::new([8; 16]).unwrap();
        assert_eq!(
            candidate.validate_against(&expected),
            Err(ProtocolValidationError::BuildMismatch)
        );
        candidate = expected;
        candidate.gameplay_content = GameplayContentHash::new([8; 32]).unwrap();
        assert_eq!(
            candidate.validate_against(&expected),
            Err(ProtocolValidationError::ContentMismatch)
        );
        candidate = expected;
        candidate.replay = ReplayFormatVersion::new(2).unwrap();
        assert_eq!(
            candidate.validate_against(&expected),
            Err(ProtocolValidationError::ReplayVersionMismatch)
        );
    }

    #[test]
    fn ownership_is_bounded_unique_and_authoritative() {
        let peer_a = PeerId::new(1).unwrap();
        let peer_b = PeerId::new(2).unwrap();
        let ownership = SeatOwnership::from_assignments(&[
            human_assignment(0, 0, peer_a),
            human_assignment(1, 1, peer_b),
            SeatAssignment {
                seat: SeatId::new(2).unwrap(),
                fighter: FighterId::new(2).unwrap(),
                owner: SeatOwner::AuthorityBot,
            },
        ])
        .unwrap();
        assert_eq!(
            ownership.validate_peer_input(peer_a, SeatId::new(0).unwrap()),
            Ok(FighterId::new(0).unwrap())
        );
        assert_eq!(
            ownership.validate_peer_input(peer_a, SeatId::new(1).unwrap()),
            Err(ProtocolValidationError::SeatOwnedByDifferentPeer)
        );
        assert_eq!(
            ownership.validate_peer_input(peer_a, SeatId::new(2).unwrap()),
            Err(ProtocolValidationError::AuthorityOwnedSeat)
        );
        assert_eq!(
            ownership.validate_peer_input(peer_a, SeatId::new(3).unwrap()),
            Err(ProtocolValidationError::UnownedSeat)
        );

        let duplicate = SeatOwnership::from_assignments(&[
            human_assignment(0, 0, peer_a),
            human_assignment(0, 1, peer_a),
        ]);
        assert_eq!(duplicate, Err(ProtocolValidationError::DuplicateSeat));

        // Wire deserialization bypasses constructors, so validation must reject a
        // forged count before slicing the backing array.
        let mut forged_over_capacity = ownership;
        forged_over_capacity.count = MAX_SEATS as u8 + 1;
        assert_eq!(
            forged_over_capacity.validate(),
            Err(ProtocolValidationError::CapacityExceeded)
        );
    }

    #[test]
    fn input_axis_button_and_redundancy_caps_are_validated() {
        assert!(QuantizedAxis::new(QuantizedAxis::MIN).is_ok());
        assert_eq!(
            QuantizedAxis::new(i8::MIN),
            Err(ProtocolValidationError::InvalidAxis)
        );
        assert_eq!(
            InputButtons::new(1 << 15),
            Err(ProtocolValidationError::UnsupportedButtons)
        );
        assert_eq!(
            SeatInputWindow::from_newest_first(&[]),
            Err(ProtocolValidationError::EmptyInputWindow)
        );
        let too_many = [frame(100, 0, 100); MAX_INPUT_FRAMES_PER_WINDOW + 1];
        assert_eq!(
            SeatInputWindow::from_newest_first(&too_many),
            Err(ProtocolValidationError::InputWindowTooLarge)
        );
        assert_eq!(full_window(100, 0, 100).len(), MAX_INPUT_FRAMES_PER_WINDOW);
    }

    #[test]
    fn stale_and_future_input_ticks_are_rejected() {
        let ticks = InputTickWindow::new(SimTick(90), SimTick(96), SimTick(104)).unwrap();
        assert_eq!(
            ticks.validate_new_input_tick(SimTick(95)),
            Err(ProtocolValidationError::StaleInput)
        );
        assert!(ticks.validate_new_input_tick(SimTick(96)).is_ok());
        assert!(ticks.validate_new_input_tick(SimTick(104)).is_ok());
        assert_eq!(
            ticks.validate_new_input_tick(SimTick(105)),
            Err(ProtocolValidationError::FutureInput)
        );
        // Committed redundancy is harmless while retained, but history older than
        // the retained frontier is rejected.
        assert!(ticks.validate_redundant_tick(SimTick(90)).is_ok());
        assert_eq!(
            ticks.validate_redundant_tick(SimTick(89)),
            Err(ProtocolValidationError::StaleInput)
        );
    }

    #[test]
    fn input_batch_checks_connected_peer_and_owned_seats() {
        let peer = PeerId::new(1).unwrap();
        let other = PeerId::new(2).unwrap();
        let ownership = SeatOwnership::from_assignments(&[human_assignment(0, 0, peer)]).unwrap();
        let batch = InputBatch::new(match_id(), peer, &[full_window(100, 0, 10)]).unwrap();
        let ticks = InputTickWindow::new(SimTick(90), SimTick(96), SimTick(104)).unwrap();
        assert!(
            batch
                .validate_for(match_id(), peer, &ownership, &ticks)
                .is_ok()
        );
        assert_eq!(
            batch.validate_for(match_id(), other, &ownership, &ticks),
            Err(ProtocolValidationError::PeerMismatch)
        );
        let mut forged_over_capacity = batch;
        forged_over_capacity.window_count = MAX_LOCAL_SEATS + 1;
        assert_eq!(
            forged_over_capacity.validate_structure(),
            Err(ProtocolValidationError::CapacityExceeded)
        );
    }

    #[test]
    fn lobby_local_seat_cap_is_enforced() {
        let expected = compatibility();
        for requested in 1..=MAX_LOCAL_SEATS {
            assert!(
                LobbyJoinRequest {
                    compatibility: expected,
                    requested_local_seats: requested,
                    reconnect: None,
                }
                .validate(&expected)
                .is_ok()
            );
        }
        for requested in [0, MAX_LOCAL_SEATS + 1] {
            assert_eq!(
                LobbyJoinRequest {
                    compatibility: expected,
                    requested_local_seats: requested,
                    reconnect: None,
                }
                .validate(&expected),
                Err(ProtocolValidationError::InvalidLocalSeatCount)
            );
        }
    }

    #[test]
    fn manifest_enforces_network_and_trust_caps() {
        let valid = manifest(AuthorityKind::Dedicated, true);
        assert!(valid.validate_for_start(SimTick(119)).is_ok());

        let mut invalid = valid;
        invalid.tick_rate_hz = SIMULATION_HZ + 1;
        assert_eq!(
            invalid.validate(),
            Err(ProtocolValidationError::InvalidTickRate)
        );
        invalid = valid;
        invalid.input_delay_ticks = MAX_INPUT_DELAY_TICKS + 1;
        assert_eq!(
            invalid.validate(),
            Err(ProtocolValidationError::InvalidInputDelay)
        );
        invalid = valid;
        invalid.input_delay_ticks = MIN_INPUT_DELAY_TICKS - 1;
        assert_eq!(
            invalid.validate(),
            Err(ProtocolValidationError::InvalidInputDelay)
        );
        invalid = valid;
        invalid.rollback_limit_ticks = MAX_NORMAL_ROLLBACK_TICKS + 1;
        assert_eq!(
            invalid.validate(),
            Err(ProtocolValidationError::InvalidRollbackLimit)
        );
        invalid = valid;
        invalid.rollback_limit_ticks = 0;
        assert_eq!(
            invalid.validate(),
            Err(ProtocolValidationError::InvalidRollbackLimit)
        );
        invalid = valid;
        invalid.snapshot_history_ticks = MIN_SNAPSHOT_HISTORY_TICKS - 1;
        assert_eq!(
            invalid.validate(),
            Err(ProtocolValidationError::InvalidSnapshotHistory)
        );
        assert_eq!(
            manifest(AuthorityKind::Listen, true).validate(),
            Err(ProtocolValidationError::UntrustedAuthorityForTrustedResult)
        );
        assert_eq!(
            valid.validate_for_start(SimTick(120)),
            Err(ProtocolValidationError::InvalidStartTick)
        );
        assert!(
            StartMessage::Countdown {
                match_id: valid.match_id,
                start_tick: SimTick(240),
            }
            .validate_against_manifest(&valid)
            .is_ok()
        );
        assert_eq!(
            StartMessage::Countdown {
                match_id: valid.match_id,
                start_tick: SimTick(119),
            }
            .validate_against_manifest(&valid),
            Err(ProtocolValidationError::InvalidStartTick)
        );
    }

    #[test]
    fn resync_payload_and_total_snapshot_caps_are_enforced() {
        assert_eq!(MAX_RESYNC_CHUNK_BYTES, 1_024);
        assert_eq!(MAX_RESYNC_SNAPSHOT_BYTES, 128 * 1_024);
        assert_eq!(MAX_RESYNC_CHUNKS, 128);

        let max_chunk = [7_u8; MAX_RESYNC_CHUNK_BYTES];
        let (payload, payload_len) = ResyncChunkPayload::from_bytes(&max_chunk).unwrap();
        assert_eq!(usize::from(payload_len), MAX_RESYNC_CHUNK_BYTES);
        let mut copied = [0_u8; MAX_RESYNC_CHUNK_BYTES];
        assert_eq!(
            payload.copy_prefix_into(payload_len, &mut copied).unwrap(),
            MAX_RESYNC_CHUNK_BYTES
        );
        assert_eq!(copied, max_chunk);
        assert_eq!(
            ResyncChunkPayload::from_bytes(&[]),
            Err(ProtocolValidationError::InvalidChunkLength)
        );
        let too_large = [0_u8; MAX_RESYNC_CHUNK_BYTES + 1];
        assert_eq!(
            ResyncChunkPayload::from_bytes(&too_large),
            Err(ProtocolValidationError::InvalidChunkLength)
        );

        let begin = ResyncBegin {
            match_id: match_id(),
            transfer_id: TransferId::new(1).unwrap(),
            snapshot_tick: SimTick(200),
            snapshot_hash: StateHash(99),
            snapshot_bytes: MAX_RESYNC_SNAPSHOT_BYTES as u32,
            chunk_count: MAX_RESYNC_CHUNKS as u16,
            recent_input_start: SimTick(196),
            recent_input_end: SimTick(200),
        };
        assert!(begin.validate().is_ok());
        assert_eq!(begin.expected_chunk_len(127), Ok(MAX_RESYNC_CHUNK_BYTES));
        assert_eq!(
            begin.expected_chunk_len(128),
            Err(ProtocolValidationError::InvalidChunkIndex)
        );
        let too_large_begin = ResyncBegin {
            snapshot_bytes: MAX_RESYNC_SNAPSHOT_BYTES as u32 + 1,
            ..begin
        };
        assert_eq!(
            too_large_begin.validate(),
            Err(ProtocolValidationError::SnapshotTooLarge)
        );
        let bad_index = ResyncChunk {
            match_id: begin.match_id,
            transfer_id: begin.transfer_id,
            snapshot_tick: begin.snapshot_tick,
            snapshot_hash: begin.snapshot_hash,
            chunk_index: begin.chunk_count,
            chunk_count: begin.chunk_count,
            payload_len,
            payload,
        };
        assert_eq!(
            bad_index.validate(),
            Err(ProtocolValidationError::InvalidChunkIndex)
        );

        let windows = std::array::from_fn::<_, 2, _>(|seat| {
            let records = std::array::from_fn::<_, MAX_RESYNC_INPUT_TAIL_TICKS, _>(|offset| {
                CommittedInputRecord {
                    frame: frame(200 - offset as u64, seat as u8, 50 - offset as u16),
                    fighter: FighterId::new(seat as u8).unwrap(),
                    source: CommittedInputSource::MissingSubstitute,
                }
            });
            CommittedSeatInputWindow::from_newest_first(&records).unwrap()
        });
        let tail = ResyncInputTail::new(&begin, &windows).unwrap();
        assert_eq!(tail.len(), 2);
        assert_eq!(tail.as_slice()[0].len(), MAX_RESYNC_INPUT_TAIL_TICKS);

        let mut forged_count = tail;
        forged_count.window_count = MAX_SEATS as u8 + 1;
        assert_eq!(
            forged_count.validate(),
            Err(ProtocolValidationError::CapacityExceeded)
        );
        let mut forged_padding = tail;
        forged_padding.windows[2] = windows[0];
        assert_eq!(
            forged_padding.validate(),
            Err(ProtocolValidationError::NonCanonicalPadding)
        );
        let short = CommittedSeatInputWindow::from_newest_first(&[CommittedInputRecord {
            frame: frame(200, 0, 50),
            fighter: FighterId::ZERO,
            source: CommittedInputSource::MissingSubstitute,
        }])
        .unwrap();
        assert_eq!(
            ResyncInputTail::new(&begin, &[short]),
            Err(ProtocolValidationError::InvalidTickWindow)
        );
        assert_eq!(
            ResyncBegin {
                recent_input_start: SimTick(195),
                ..begin
            }
            .validate(),
            Err(ProtocolValidationError::InvalidTickWindow)
        );
        assert_eq!(
            ResyncBegin {
                recent_input_end: SimTick(199),
                ..begin
            }
            .validate(),
            Err(ProtocolValidationError::InvalidTickWindow)
        );
    }

    #[test]
    fn fixed_codec_intent_leaves_transport_headroom() {
        assert!(InputBatch::MAX_WIRE_BYTES < MAX_HIGH_FREQUENCY_PACKET_BYTES);
        assert!(CommittedInputRelay::MAX_WIRE_BYTES < MAX_HIGH_FREQUENCY_PACKET_BYTES);
        assert!(ResyncChunk::MAX_WIRE_BYTES < MAX_HIGH_FREQUENCY_PACKET_BYTES);
        assert!(ResyncInputTail::MAX_WIRE_BYTES < MAX_HIGH_FREQUENCY_PACKET_BYTES);
        assert_eq!(
            MAX_RESYNC_CHUNKS * MAX_RESYNC_CHUNK_BYTES,
            MAX_RESYNC_SNAPSHOT_BYTES
        );
        assert_eq!(CHANNEL_SPECS.len(), 5);
        assert_eq!(
            CHANNEL_SPECS[1],
            ChannelSpec {
                channel: ProtocolChannel::Input,
                delivery: Delivery::SequencedUnreliable,
                direction: Direction::Bidirectional,
            }
        );
    }

    #[test]
    fn input_batch_carries_optional_authoritative_state_baseline_ack() {
        let frame = InputFrame {
            tick: SimTick(12),
            seat: SeatId::new(0).unwrap(),
            sequence: InputSequence(12),
            ..InputFrame::default()
        };
        let window = SeatInputWindow::from_newest_first(&[frame]).unwrap();
        let acknowledgement = StateBaselineAck {
            tick: SimTick(9),
            hash: StateHash(44),
        };
        let batch = InputBatch::new(
            MatchId::new([1; 16]).unwrap(),
            PeerId::new(2).unwrap(),
            &[window],
        )
        .unwrap()
        .with_state_baseline_ack(acknowledgement)
        .unwrap();
        assert_eq!(batch.state_baseline_ack(), Some(acknowledgement));
    }
}
