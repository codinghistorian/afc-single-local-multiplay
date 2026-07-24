//! Versioned offline replay files and a transport-independent headless verifier.
//!
//! Replay files are archival simulation inputs, not gameplay packets. Their manual
//! codec is intentionally separate from `network_codec`: it stores one complete,
//! authority-accepted input set for every fighter and tick, optional seek
//! keyframes, and diagnostic hashes under strict file-level bounds.

use crate::authority::AuthorityTickReport;
use crate::authority_input::{AuthorityInputOrigin, AuthorityInputStatus};
use crate::determinism::{CanonicalHash64, FIGHTER_CAPACITY, FighterId, SimTick};
use crate::network_protocol::{
    BuildId, CompatibilityId, GameplayContentHash, InputButtons, InputFrame, InputSequence,
    ManifestHash, MatchId, MatchManifest, ProtocolValidationError, ProtocolVersion, QuantizedAxis,
    ReplayFormatVersion, SeatId, SeatOwner, SimulationVersion, StateHash,
};
use crate::snapshot::{CanonicalSnapshot, MAX_SNAPSHOT_BYTES, MatchResultSnapshot, SnapshotError};
use std::error::Error;
use std::fmt;

pub const REPLAY_MAGIC: [u8; 4] = *b"AFCR";
pub const REPLAY_SCHEMA_VERSION: u16 = 1;

/// Two hours at 60 Hz. Longer sessions require a deliberate schema-cap revision.
pub const MAX_REPLAY_TICKS: usize = 60 * 60 * 60 * 2;
pub const MAX_REPLAY_HASH_CHECKPOINTS: usize = 65_536;
pub const MAX_REPLAY_KEYFRAMES: usize = 256;
pub const MAX_REPLAY_BYTES: usize = 64 * 1_024 * 1_024;
pub const AUTHORITY_RESULT_ID_BYTES: usize = 16;

const ENCODED_ACCEPTED_INPUT_BYTES: usize = 1 + 1 + 1 + 1 + 1 + 2 + 2 + 2 + 2;
const ENCODED_TICK_INPUT_BYTES: usize =
    8 + ENCODED_ACCEPTED_INPUT_BYTES * FIGHTER_CAPACITY as usize;
const ENCODED_HASH_CHECKPOINT_BYTES: usize = 8 + 8;
const MIN_ENCODED_KEYFRAME_BYTES: usize = 8 + 8 + 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayHeader {
    pub schema_version: u16,
    pub compatibility: CompatibilityId,
    pub match_id: MatchId,
    pub manifest_hash: ManifestHash,
    /// Stable digest of rules, arena, slot/loadout configuration, and tuning IDs.
    pub match_config_hash: u64,
    pub master_seed: u64,
}

impl ReplayHeader {
    pub const fn new(
        compatibility: CompatibilityId,
        match_id: MatchId,
        manifest_hash: ManifestHash,
        match_config_hash: u64,
        master_seed: u64,
    ) -> Self {
        Self {
            schema_version: REPLAY_SCHEMA_VERSION,
            compatibility,
            match_id,
            manifest_hash,
            match_config_hash,
            master_seed,
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayInputSource {
    Inactive = 0,
    Peer = 1,
    AuthorityBot = 2,
    AuthoritySubstitution = 3,
}

impl ReplayInputSource {
    const fn code(self) -> u8 {
        self as u8
    }

    const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Inactive),
            1 => Some(Self::Peer),
            2 => Some(Self::AuthorityBot),
            3 => Some(Self::AuthoritySubstitution),
            _ => None,
        }
    }

    pub const fn is_active(self) -> bool {
        !matches!(self, Self::Inactive)
    }
}

/// The final input actually accepted by the authority for one fighter slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcceptedFighterInput {
    pub fighter: FighterId,
    pub source: ReplayInputSource,
    pub frame: InputFrame,
}

impl AcceptedFighterInput {
    pub fn inactive(tick: SimTick, fighter: FighterId) -> Self {
        Self {
            fighter,
            source: ReplayInputSource::Inactive,
            frame: InputFrame {
                tick,
                seat: SeatId::new(fighter.get()).expect("fighter slots are valid seat indices"),
                movement_x: QuantizedAxis::default(),
                movement_y: QuantizedAxis::default(),
                held_buttons: InputButtons::default(),
                pressed_buttons: InputButtons::default(),
                released_buttons: InputButtons::default(),
                sequence: InputSequence(0),
            },
        }
    }
}

/// Fixed four-slot authority input at one simulation tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayTickInputs {
    pub tick: SimTick,
    pub fighters: [AcceptedFighterInput; FIGHTER_CAPACITY as usize],
}

impl ReplayTickInputs {
    pub fn all_inactive(tick: SimTick) -> Self {
        Self {
            tick,
            fighters: FighterId::ALL.map(|fighter| AcceptedFighterInput::inactive(tick, fighter)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplayHashCheckpoint {
    pub tick: SimTick,
    pub state_hash: StateHash,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayKeyframe {
    pub tick: SimTick,
    pub state_hash: StateHash,
    pub snapshot: CanonicalSnapshot,
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AuthorityResultId([u8; AUTHORITY_RESULT_ID_BYTES]);

impl AuthorityResultId {
    pub fn new(bytes: [u8; AUTHORITY_RESULT_ID_BYTES]) -> Result<Self, ReplayError> {
        if bytes.iter().all(|byte| *byte == 0) {
            Err(ReplayError::InvalidValue {
                field: "authority result ID",
                value: 0,
            })
        } else {
            Ok(Self(bytes))
        }
    }

    pub const fn as_bytes(&self) -> &[u8; AUTHORITY_RESULT_ID_BYTES] {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorityMatchResult {
    Draw,
    FighterWinner(FighterId),
    TeamWinner(u8),
    Aborted(u16),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FinalAuthorityResult {
    pub result_id: AuthorityResultId,
    pub confirmed_tick: SimTick,
    pub state_hash: StateHash,
    pub result: AuthorityMatchResult,
}

/// A complete authority history for one match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replay {
    pub header: ReplayHeader,
    pub initial_snapshot: CanonicalSnapshot,
    /// Contiguous records beginning at `initial_snapshot.tick + 1`.
    pub inputs: Vec<ReplayTickInputs>,
    /// Strict simulation-order checkpoints, without duplicate ticks.
    pub hash_checkpoints: Vec<ReplayHashCheckpoint>,
    /// Strict simulation-order seek points after the initial snapshot.
    pub keyframes: Vec<ReplayKeyframe>,
    pub final_result: FinalAuthorityResult,
}

/// Incremental, bounded recorder for the authority's actual committed input tape.
///
/// Callers hand this type the same [`AuthorityTickReport`] and immutable snapshot
/// that were produced by `AuthorityMatch::step`. This prevents replay files from
/// being reconstructed from a client's predicted or packet-received view.
pub struct AuthorityReplayRecorder {
    manifest: MatchManifest,
    header: ReplayHeader,
    initial_snapshot: CanonicalSnapshot,
    inputs: Vec<ReplayTickInputs>,
    hash_checkpoints: Vec<ReplayHashCheckpoint>,
    keyframes: Vec<ReplayKeyframe>,
}

impl AuthorityReplayRecorder {
    pub fn new(
        manifest: MatchManifest,
        initial_snapshot: CanonicalSnapshot,
    ) -> Result<Self, ReplayError> {
        manifest.validate()?;
        let header = ReplayHeader::new(
            manifest.compatibility,
            manifest.match_id,
            manifest.manifest_hash,
            initial_snapshot.header.gameplay_content_hash,
            manifest.master_gameplay_seed,
        );
        validate_bound_snapshot(&header, &initial_snapshot)?;
        if initial_snapshot.header.master_seed != manifest.master_gameplay_seed {
            return Err(ReplayError::InvariantViolation(
                "initial snapshot seed differs from match manifest",
            ));
        }
        let occupied_mask = manifest
            .slots
            .iter()
            .enumerate()
            .fold(0_u8, |mask, (index, slot)| {
                if slot.occupied {
                    mask | (1 << index)
                } else {
                    mask
                }
            });
        if initial_snapshot.match_state.active_slots_mask != occupied_mask {
            return Err(ReplayError::InvariantViolation(
                "initial snapshot active roster differs from match manifest",
            ));
        }
        let initial_hash = StateHash(initial_snapshot.canonical_hash()?);
        let initial_tick = initial_snapshot.header.tick;
        Ok(Self {
            manifest,
            header,
            initial_snapshot,
            inputs: Vec::new(),
            hash_checkpoints: vec![ReplayHashCheckpoint {
                tick: initial_tick,
                state_hash: initial_hash,
            }],
            keyframes: Vec::new(),
        })
    }

    pub const fn recorded_ticks(&self) -> usize {
        self.inputs.len()
    }

    pub fn record_tick(
        &mut self,
        report: &AuthorityTickReport,
        snapshot: &CanonicalSnapshot,
        checkpoint: bool,
        keyframe: bool,
    ) -> Result<(), ReplayError> {
        enforce_cap("input tick count", self.inputs.len() + 1, MAX_REPLAY_TICKS)?;
        let expected_tick = self
            .initial_snapshot
            .header
            .tick
            .wrapping_add(self.inputs.len() as u64 + 1);
        if report.tick != expected_tick
            || report.committed_inputs.tick != expected_tick
            || snapshot.header.tick != expected_tick
        {
            return Err(ReplayError::NonCanonicalOrder {
                field: "recorded authority ticks",
            });
        }
        validate_bound_snapshot(&self.header, snapshot)?;
        if snapshot.match_state.active_slots_mask
            != self.initial_snapshot.match_state.active_slots_mask
        {
            return Err(ReplayError::InvariantViolation(
                "fighter activity changed during replay recording",
            ));
        }
        let snapshot_hash = StateHash(snapshot.canonical_hash()?);
        if snapshot_hash != report.state_hash {
            return Err(ReplayError::InvariantViolation(
                "authority report hash differs from recorded snapshot",
            ));
        }

        let mut recorded = ReplayTickInputs::all_inactive(expected_tick);
        let mut fighter_mask = 0_u8;
        for (seat_index, authority_record) in report.committed_inputs.by_seat.iter().enumerate() {
            let Some(authority_record) = authority_record else {
                continue;
            };
            if authority_record.status != AuthorityInputStatus::Committed
                || authority_record.frame.tick != expected_tick
                || usize::from(authority_record.frame.seat.get()) != seat_index
            {
                return Err(ReplayError::InvariantViolation(
                    "authority report contains a malformed committed input",
                ));
            }
            let assignment = self
                .manifest
                .ownership
                .assignment_for_seat(authority_record.frame.seat)
                .ok_or(ReplayError::InvariantViolation(
                    "authority report input has no manifest owner",
                ))?;
            if assignment.fighter != authority_record.fighter {
                return Err(ReplayError::InvariantViolation(
                    "authority report fighter differs from seat ownership",
                ));
            }
            let origin_matches_owner = match (assignment.owner, authority_record.origin) {
                (SeatOwner::Peer(expected), AuthorityInputOrigin::Peer(actual)) => {
                    expected == actual
                }
                (SeatOwner::Peer(_), AuthorityInputOrigin::MissingSubstitute)
                | (SeatOwner::Peer(_), AuthorityInputOrigin::DisconnectedBot(_))
                | (SeatOwner::AuthorityBot, AuthorityInputOrigin::AuthorityBot) => true,
                _ => false,
            };
            if !origin_matches_owner {
                return Err(ReplayError::InvariantViolation(
                    "authority input origin differs from manifest ownership",
                ));
            }
            let bit = 1 << authority_record.fighter.get();
            if fighter_mask & bit != 0 {
                return Err(ReplayError::InvariantViolation(
                    "authority report contains duplicate fighter input",
                ));
            }
            fighter_mask |= bit;
            let source = match authority_record.origin {
                AuthorityInputOrigin::Peer(_) => ReplayInputSource::Peer,
                AuthorityInputOrigin::AuthorityBot | AuthorityInputOrigin::DisconnectedBot(_) => {
                    ReplayInputSource::AuthorityBot
                }
                AuthorityInputOrigin::MissingSubstitute => ReplayInputSource::AuthoritySubstitution,
            };
            recorded.fighters[authority_record.fighter.index()] = AcceptedFighterInput {
                fighter: authority_record.fighter,
                source,
                frame: authority_record.frame,
            };
        }
        if fighter_mask != self.initial_snapshot.match_state.active_slots_mask {
            return Err(ReplayError::InvariantViolation(
                "authority report is missing an active fighter input",
            ));
        }

        self.inputs.push(recorded);
        if checkpoint {
            self.push_checkpoint(expected_tick, snapshot_hash)?;
        }
        if keyframe {
            enforce_cap(
                "keyframe count",
                self.keyframes.len() + 1,
                MAX_REPLAY_KEYFRAMES,
            )?;
            self.keyframes.push(ReplayKeyframe {
                tick: expected_tick,
                state_hash: snapshot_hash,
                snapshot: snapshot.clone(),
            });
        }
        Ok(())
    }

    pub fn finish(
        mut self,
        final_snapshot: &CanonicalSnapshot,
        result_id: u64,
    ) -> Result<Replay, ReplayError> {
        if result_id == 0 {
            return Err(ReplayError::InvalidValue {
                field: "authority result ID",
                value: 0,
            });
        }
        let final_tick = self
            .inputs
            .last()
            .map_or(self.initial_snapshot.header.tick, |inputs| inputs.tick);
        if final_snapshot.header.tick != final_tick {
            return Err(ReplayError::InvariantViolation(
                "final snapshot tick differs from recorded input history",
            ));
        }
        validate_bound_snapshot(&self.header, final_snapshot)?;
        let state_hash = StateHash(final_snapshot.canonical_hash()?);
        self.push_checkpoint(final_tick, state_hash)?;
        let result = authority_result_from_snapshot(final_snapshot).ok_or(
            ReplayError::InvariantViolation("final snapshot has no canonical match result"),
        )?;
        let replay_result_id =
            authority_result_id_from_wire(result_id, self.header.match_id.as_bytes())?;

        let replay = Replay {
            header: self.header,
            initial_snapshot: self.initial_snapshot,
            inputs: self.inputs,
            hash_checkpoints: self.hash_checkpoints,
            keyframes: self.keyframes,
            final_result: FinalAuthorityResult {
                result_id: replay_result_id,
                confirmed_tick: final_tick,
                state_hash,
                result,
            },
        };
        replay.validate()?;
        Ok(replay)
    }

    fn push_checkpoint(&mut self, tick: SimTick, state_hash: StateHash) -> Result<(), ReplayError> {
        if let Some(previous) = self.hash_checkpoints.last() {
            if previous.tick == tick {
                if previous.state_hash != state_hash {
                    return Err(ReplayError::InvariantViolation(
                        "duplicate replay checkpoint has a conflicting hash",
                    ));
                }
                return Ok(());
            }
            if previous.tick > tick {
                return Err(ReplayError::NonCanonicalOrder {
                    field: "hash checkpoints",
                });
            }
        }
        enforce_cap(
            "hash checkpoint count",
            self.hash_checkpoints.len() + 1,
            MAX_REPLAY_HASH_CHECKPOINTS,
        )?;
        self.hash_checkpoints
            .push(ReplayHashCheckpoint { tick, state_hash });
        Ok(())
    }
}

fn authority_result_id_from_wire(
    result_id: u64,
    match_id: &[u8; 16],
) -> Result<AuthorityResultId, ReplayError> {
    let mut bytes = [0_u8; AUTHORITY_RESULT_ID_BYTES];
    bytes[..8].copy_from_slice(&result_id.to_le_bytes());
    let mut hash = CanonicalHash64::new();
    hash.write_str("afc-replay-result-id-v1")
        .write_bytes(match_id)
        .write_u64(result_id);
    bytes[8..].copy_from_slice(&hash.finish().to_le_bytes());
    AuthorityResultId::new(bytes)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplayError {
    InvalidMagic([u8; 4]),
    UnsupportedSchemaVersion {
        found: u16,
        supported: u16,
    },
    DeclaredLengthMismatch {
        declared: usize,
        actual: usize,
    },
    UnexpectedEnd {
        offset: usize,
        needed: usize,
        remaining: usize,
    },
    LimitExceeded {
        field: &'static str,
        value: usize,
        max: usize,
    },
    InvalidValue {
        field: &'static str,
        value: u64,
    },
    NonCanonicalOrder {
        field: &'static str,
    },
    InvariantViolation(&'static str),
    Protocol(ProtocolValidationError),
    Snapshot(SnapshotError),
}

impl fmt::Display for ReplayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMagic(found) => write!(formatter, "invalid replay magic {found:?}"),
            Self::UnsupportedSchemaVersion { found, supported } => write!(
                formatter,
                "unsupported replay schema version {found}; supported version is {supported}"
            ),
            Self::DeclaredLengthMismatch { declared, actual } => write!(
                formatter,
                "replay declares {declared} bytes but contains {actual} bytes"
            ),
            Self::UnexpectedEnd {
                offset,
                needed,
                remaining,
            } => write!(
                formatter,
                "replay ended at byte {offset}: needed {needed} bytes, {remaining} remain"
            ),
            Self::LimitExceeded { field, value, max } => {
                write!(
                    formatter,
                    "{field} value {value} exceeds replay limit {max}"
                )
            }
            Self::InvalidValue { field, value } => {
                write!(formatter, "invalid replay {field} value {value}")
            }
            Self::NonCanonicalOrder { field } => {
                write!(
                    formatter,
                    "replay {field} is not in strict simulation order"
                )
            }
            Self::InvariantViolation(message) => {
                write!(formatter, "replay invariant violated: {message}")
            }
            Self::Protocol(error) => write!(formatter, "replay protocol field is invalid: {error}"),
            Self::Snapshot(error) => write!(formatter, "replay snapshot is invalid: {error}"),
        }
    }
}

impl Error for ReplayError {}

impl From<ProtocolValidationError> for ReplayError {
    fn from(value: ProtocolValidationError) -> Self {
        Self::Protocol(value)
    }
}

impl From<SnapshotError> for ReplayError {
    fn from(value: SnapshotError) -> Self {
        Self::Snapshot(value)
    }
}

fn enforce_cap(field: &'static str, value: usize, max: usize) -> Result<(), ReplayError> {
    if value > max {
        Err(ReplayError::LimitExceeded { field, value, max })
    } else {
        Ok(())
    }
}

impl Replay {
    pub fn validate(&self) -> Result<(), ReplayError> {
        if self.header.schema_version != REPLAY_SCHEMA_VERSION {
            return Err(ReplayError::UnsupportedSchemaVersion {
                found: self.header.schema_version,
                supported: REPLAY_SCHEMA_VERSION,
            });
        }
        self.header.compatibility.validate()?;
        if self.header.compatibility.replay.get() != REPLAY_SCHEMA_VERSION {
            return Err(ReplayError::UnsupportedSchemaVersion {
                found: self.header.compatibility.replay.get(),
                supported: REPLAY_SCHEMA_VERSION,
            });
        }
        self.header.match_id.validate()?;
        if self.header.manifest_hash.0 == 0 {
            return Err(ReplayError::InvalidValue {
                field: "manifest hash",
                value: 0,
            });
        }
        if self.header.match_config_hash == 0 {
            return Err(ReplayError::InvalidValue {
                field: "match configuration hash",
                value: 0,
            });
        }

        validate_bound_snapshot(&self.header, &self.initial_snapshot)?;
        enforce_cap("input tick count", self.inputs.len(), MAX_REPLAY_TICKS)?;

        let active_mask = self.initial_snapshot.match_state.active_slots_mask;
        let mut expected_tick = self.initial_snapshot.header.tick.next();
        let mut previous_frames: [Option<InputFrame>; FIGHTER_CAPACITY as usize] =
            [None; FIGHTER_CAPACITY as usize];
        let mut bot_owned: [Option<bool>; FIGHTER_CAPACITY as usize] =
            [None; FIGHTER_CAPACITY as usize];

        for record in &self.inputs {
            if record.tick != expected_tick {
                return Err(ReplayError::NonCanonicalOrder {
                    field: "input ticks",
                });
            }
            expected_tick = expected_tick.next();

            let mut used_seats = [false; FIGHTER_CAPACITY as usize];
            for (index, accepted) in record.fighters.iter().enumerate() {
                let expected_fighter = FighterId::from_index(index)
                    .expect("replay fighter array has exactly four slots");
                if accepted.fighter != expected_fighter {
                    return Err(ReplayError::InvariantViolation(
                        "accepted-input array index does not match FighterId",
                    ));
                }
                if accepted.frame.tick != record.tick {
                    return Err(ReplayError::InvariantViolation(
                        "accepted input frame tick differs from record tick",
                    ));
                }
                accepted.frame.validate()?;

                let fighter_active = active_mask & (1 << index) != 0;
                if accepted.source.is_active() != fighter_active {
                    return Err(ReplayError::InvariantViolation(
                        "input source activity differs from initial fighter activity",
                    ));
                }
                if !fighter_active {
                    if *accepted != AcceptedFighterInput::inactive(record.tick, expected_fighter) {
                        return Err(ReplayError::InvariantViolation(
                            "inactive fighter input is not canonical neutral padding",
                        ));
                    }
                    continue;
                }

                let seat_index = accepted.frame.seat.get() as usize;
                if used_seats[seat_index] {
                    return Err(ReplayError::InvariantViolation(
                        "two active fighters use the same seat in one tick",
                    ));
                }
                used_seats[seat_index] = true;

                let current_bot_owned = accepted.source == ReplayInputSource::AuthorityBot;
                match bot_owned[index] {
                    None => bot_owned[index] = Some(current_bot_owned),
                    Some(expected) if expected != current_bot_owned => {
                        return Err(ReplayError::InvariantViolation(
                            "authority-bot ownership changes during replay",
                        ));
                    }
                    Some(_) => {}
                }

                if let Some(previous) = previous_frames[index] {
                    if previous.seat != accepted.frame.seat {
                        return Err(ReplayError::InvariantViolation(
                            "fighter seat changes during replay",
                        ));
                    }
                    if accepted.frame.sequence != InputSequence(previous.sequence.0.wrapping_add(1))
                    {
                        return Err(ReplayError::NonCanonicalOrder {
                            field: "accepted input sequences",
                        });
                    }
                }
                previous_frames[index] = Some(accepted.frame);
            }
        }

        enforce_cap(
            "hash checkpoint count",
            self.hash_checkpoints.len(),
            MAX_REPLAY_HASH_CHECKPOINTS,
        )?;
        if self.hash_checkpoints.is_empty() {
            return Err(ReplayError::InvariantViolation(
                "replay must contain initial and final hash checkpoints",
            ));
        }
        let mut previous_position = None;
        for checkpoint in &self.hash_checkpoints {
            let position =
                self.tick_position(checkpoint.tick)
                    .ok_or(ReplayError::InvariantViolation(
                        "hash checkpoint tick is outside input history",
                    ))?;
            if previous_position.is_some_and(|previous| position <= previous) {
                return Err(ReplayError::NonCanonicalOrder {
                    field: "hash checkpoints",
                });
            }
            previous_position = Some(position);
        }
        if self
            .hash_checkpoints
            .first()
            .map(|checkpoint| checkpoint.tick)
            != Some(self.initial_snapshot.header.tick)
            || self
                .hash_checkpoints
                .last()
                .map(|checkpoint| checkpoint.tick)
                != Some(self.final_tick())
        {
            return Err(ReplayError::InvariantViolation(
                "hash checkpoints must cover initial and final ticks",
            ));
        }
        if let Some(initial_checkpoint) = self
            .hash_checkpoints
            .iter()
            .find(|checkpoint| checkpoint.tick == self.initial_snapshot.header.tick)
            && initial_checkpoint.state_hash != StateHash(self.initial_snapshot.canonical_hash()?)
        {
            return Err(ReplayError::InvariantViolation(
                "initial checkpoint differs from canonical initial snapshot hash",
            ));
        }

        enforce_cap("keyframe count", self.keyframes.len(), MAX_REPLAY_KEYFRAMES)?;
        previous_position = None;
        for keyframe in &self.keyframes {
            let position =
                self.tick_position(keyframe.tick)
                    .ok_or(ReplayError::InvariantViolation(
                        "keyframe tick is outside input history",
                    ))?;
            if position == 0 {
                return Err(ReplayError::InvariantViolation(
                    "initial snapshot must not be duplicated as a keyframe",
                ));
            }
            if previous_position.is_some_and(|previous| position <= previous) {
                return Err(ReplayError::NonCanonicalOrder { field: "keyframes" });
            }
            previous_position = Some(position);

            validate_bound_snapshot(&self.header, &keyframe.snapshot)?;
            if keyframe.snapshot.header.tick != keyframe.tick {
                return Err(ReplayError::InvariantViolation(
                    "keyframe tick differs from nested snapshot tick",
                ));
            }
            let canonical_hash = keyframe.snapshot.canonical_hash()?;
            if keyframe.state_hash != StateHash(canonical_hash) {
                return Err(ReplayError::InvariantViolation(
                    "keyframe hash differs from canonical snapshot hash",
                ));
            }
            if let Some(checkpoint) = self
                .hash_checkpoints
                .iter()
                .find(|checkpoint| checkpoint.tick == keyframe.tick)
                && checkpoint.state_hash != keyframe.state_hash
            {
                return Err(ReplayError::InvariantViolation(
                    "keyframe and checkpoint hashes disagree",
                ));
            }
        }

        AuthorityResultId::new(*self.final_result.result_id.as_bytes())?;
        let final_tick = self.final_tick();
        if self.final_result.confirmed_tick != final_tick {
            return Err(ReplayError::InvariantViolation(
                "authority result tick is not the final replay tick",
            ));
        }
        match self.final_result.result {
            AuthorityMatchResult::FighterWinner(fighter) => {
                if !self.initial_snapshot.fighters[fighter.index()].occupied {
                    return Err(ReplayError::InvariantViolation(
                        "authority result names an unoccupied fighter",
                    ));
                }
            }
            AuthorityMatchResult::TeamWinner(team) if team >= FIGHTER_CAPACITY => {
                return Err(ReplayError::InvalidValue {
                    field: "authority winning team",
                    value: u64::from(team),
                });
            }
            _ => {}
        }

        if let Some(checkpoint) = self
            .hash_checkpoints
            .iter()
            .find(|checkpoint| checkpoint.tick == final_tick)
            && checkpoint.state_hash != self.final_result.state_hash
        {
            return Err(ReplayError::InvariantViolation(
                "final authority hash differs from final checkpoint",
            ));
        }
        if let Some(keyframe) = self
            .keyframes
            .iter()
            .find(|keyframe| keyframe.tick == final_tick)
        {
            if keyframe.state_hash != self.final_result.state_hash {
                return Err(ReplayError::InvariantViolation(
                    "final authority hash differs from final keyframe",
                ));
            }
            if let Some(snapshot_result) = authority_result_from_snapshot(&keyframe.snapshot)
                && snapshot_result != self.final_result.result
            {
                return Err(ReplayError::InvariantViolation(
                    "final keyframe result differs from authority result",
                ));
            }
        } else if self.inputs.is_empty()
            && let Some(snapshot_result) = authority_result_from_snapshot(&self.initial_snapshot)
            && snapshot_result != self.final_result.result
        {
            return Err(ReplayError::InvariantViolation(
                "initial/final snapshot result differs from authority result",
            ));
        }

        Ok(())
    }

    /// Validates both the replay's internal bindings and exact local playback
    /// compatibility. Decoding may retain an older replay for archival or
    /// diagnostics, but current simulation code must call this gate before
    /// attempting playback.
    pub fn validate_against(
        &self,
        expected_compatibility: &CompatibilityId,
    ) -> Result<(), ReplayError> {
        self.validate()?;
        self.header
            .compatibility
            .validate_against(expected_compatibility)?;
        Ok(())
    }

    pub fn final_tick(&self) -> SimTick {
        self.inputs
            .last()
            .map_or(self.initial_snapshot.header.tick, |record| record.tick)
    }

    /// Returns zero for the initial snapshot and `n + 1` for input record `n`.
    pub fn tick_position(&self, tick: SimTick) -> Option<usize> {
        let initial_tick = self.initial_snapshot.header.tick;
        let delta = tick.get().wrapping_sub(initial_tick.get());
        if delta == 0 {
            return Some(0);
        }
        let position = usize::try_from(delta).ok()?;
        let input_index = position.checked_sub(1)?;
        if input_index < self.inputs.len() && self.inputs[input_index].tick == tick {
            Some(position)
        } else {
            None
        }
    }
}

fn validate_bound_snapshot(
    header: &ReplayHeader,
    snapshot: &CanonicalSnapshot,
) -> Result<(), ReplayError> {
    snapshot.validate()?;
    if snapshot.header.protocol_version != u32::from(header.compatibility.protocol.get()) {
        return Err(ReplayError::InvariantViolation(
            "snapshot protocol version differs from replay compatibility",
        ));
    }
    if snapshot.header.simulation_version != u32::from(header.compatibility.simulation.get()) {
        return Err(ReplayError::InvariantViolation(
            "snapshot simulation version differs from replay compatibility",
        ));
    }
    if snapshot.header.match_id != *header.match_id.as_bytes() {
        return Err(ReplayError::InvariantViolation(
            "snapshot match ID differs from replay match ID",
        ));
    }
    if snapshot.header.gameplay_content_hash != header.match_config_hash {
        return Err(ReplayError::InvariantViolation(
            "snapshot configuration hash differs from replay header",
        ));
    }
    if snapshot.header.master_seed != header.master_seed {
        return Err(ReplayError::InvariantViolation(
            "snapshot master seed differs from replay header",
        ));
    }
    Ok(())
}

pub(crate) fn authority_result_from_snapshot(
    snapshot: &CanonicalSnapshot,
) -> Option<AuthorityMatchResult> {
    match snapshot.match_state.result {
        MatchResultSnapshot::Pending => None,
        MatchResultSnapshot::Draw { .. } => Some(AuthorityMatchResult::Draw),
        MatchResultSnapshot::FighterWinner { fighter, .. } => {
            Some(AuthorityMatchResult::FighterWinner(fighter))
        }
        MatchResultSnapshot::TeamWinner { team, .. } => {
            Some(AuthorityMatchResult::TeamWinner(team))
        }
        MatchResultSnapshot::Aborted { reason, .. } => Some(AuthorityMatchResult::Aborted(reason)),
    }
}

impl Replay {
    /// Encodes the replay file. This codec is deliberately not reused for live
    /// packet framing or transport messages.
    pub fn encode(&self) -> Result<Vec<u8>, ReplayError> {
        self.validate()?;
        let mut encoder = ReplayEncoder::new();
        encoder.write_bytes(&REPLAY_MAGIC)?;
        encoder.write_u16(self.header.schema_version)?;
        let length_offset = encoder.len();
        encoder.write_u32(0)?;

        encode_compatibility(&mut encoder, &self.header.compatibility)?;
        encoder.write_bytes(self.header.match_id.as_bytes())?;
        encoder.write_u64(self.header.manifest_hash.0)?;
        encoder.write_u64(self.header.match_config_hash)?;
        encoder.write_u64(self.header.master_seed)?;

        let initial_snapshot = self.initial_snapshot.encode()?;
        encoder.write_u32(initial_snapshot.len() as u32)?;
        encoder.write_bytes(&initial_snapshot)?;

        encoder.write_u32(self.inputs.len() as u32)?;
        for record in &self.inputs {
            encode_tick_inputs(&mut encoder, record)?;
        }

        encoder.write_u32(self.hash_checkpoints.len() as u32)?;
        for checkpoint in &self.hash_checkpoints {
            encoder.write_u64(checkpoint.tick.get())?;
            encoder.write_u64(checkpoint.state_hash.0)?;
        }

        encoder.write_u32(self.keyframes.len() as u32)?;
        for keyframe in &self.keyframes {
            encoder.write_u64(keyframe.tick.get())?;
            encoder.write_u64(keyframe.state_hash.0)?;
            let snapshot = keyframe.snapshot.encode()?;
            encoder.write_u32(snapshot.len() as u32)?;
            encoder.write_bytes(&snapshot)?;
        }

        encoder.write_bytes(self.final_result.result_id.as_bytes())?;
        encoder.write_u64(self.final_result.confirmed_tick.get())?;
        encoder.write_u64(self.final_result.state_hash.0)?;
        encode_authority_result(&mut encoder, self.final_result.result)?;

        let len = encoder.len();
        enforce_cap("encoded replay bytes", len, MAX_REPLAY_BYTES)?;
        encoder.patch_u32(length_offset, len as u32);
        Ok(encoder.finish())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ReplayError> {
        enforce_cap("encoded replay bytes", bytes.len(), MAX_REPLAY_BYTES)?;
        let mut decoder = ReplayDecoder::new(bytes);
        let magic = decoder.read_array::<4>()?;
        if magic != REPLAY_MAGIC {
            return Err(ReplayError::InvalidMagic(magic));
        }
        let schema_version = decoder.read_u16()?;
        if schema_version != REPLAY_SCHEMA_VERSION {
            return Err(ReplayError::UnsupportedSchemaVersion {
                found: schema_version,
                supported: REPLAY_SCHEMA_VERSION,
            });
        }
        let declared_length = decoder.read_u32()? as usize;
        enforce_cap("declared replay bytes", declared_length, MAX_REPLAY_BYTES)?;
        if declared_length != bytes.len() {
            return Err(ReplayError::DeclaredLengthMismatch {
                declared: declared_length,
                actual: bytes.len(),
            });
        }

        let compatibility = decode_compatibility(&mut decoder)?;
        let match_id = MatchId::new(decoder.read_array::<16>()?)?;
        let manifest_hash = ManifestHash(decoder.read_u64()?);
        let match_config_hash = decoder.read_u64()?;
        let master_seed = decoder.read_u64()?;
        let header = ReplayHeader {
            schema_version,
            compatibility,
            match_id,
            manifest_hash,
            match_config_hash,
            master_seed,
        };

        let initial_len =
            decoder.read_bounded_len_u32("initial snapshot bytes", MAX_SNAPSHOT_BYTES)?;
        if initial_len == 0 {
            return Err(ReplayError::InvalidValue {
                field: "initial snapshot bytes",
                value: 0,
            });
        }
        let initial_snapshot = CanonicalSnapshot::decode(decoder.read_slice(initial_len)?)?;

        let input_count = decoder.read_bounded_len_u32("input tick count", MAX_REPLAY_TICKS)?;
        decoder.require_fixed_records(
            "input tick records",
            input_count,
            ENCODED_TICK_INPUT_BYTES,
        )?;
        let mut inputs = Vec::with_capacity(input_count);
        for _ in 0..input_count {
            inputs.push(decode_tick_inputs(&mut decoder)?);
        }

        let hash_count =
            decoder.read_bounded_len_u32("hash checkpoint count", MAX_REPLAY_HASH_CHECKPOINTS)?;
        decoder.require_fixed_records(
            "hash checkpoint records",
            hash_count,
            ENCODED_HASH_CHECKPOINT_BYTES,
        )?;
        let mut hash_checkpoints = Vec::with_capacity(hash_count);
        for _ in 0..hash_count {
            hash_checkpoints.push(ReplayHashCheckpoint {
                tick: SimTick(decoder.read_u64()?),
                state_hash: StateHash(decoder.read_u64()?),
            });
        }

        let keyframe_count =
            decoder.read_bounded_len_u32("keyframe count", MAX_REPLAY_KEYFRAMES)?;
        decoder.require_fixed_records(
            "minimum keyframe records",
            keyframe_count,
            MIN_ENCODED_KEYFRAME_BYTES,
        )?;
        let mut keyframes = Vec::with_capacity(keyframe_count);
        for _ in 0..keyframe_count {
            let tick = SimTick(decoder.read_u64()?);
            let state_hash = StateHash(decoder.read_u64()?);
            let snapshot_len =
                decoder.read_bounded_len_u32("keyframe snapshot bytes", MAX_SNAPSHOT_BYTES)?;
            if snapshot_len == 0 {
                return Err(ReplayError::InvalidValue {
                    field: "keyframe snapshot bytes",
                    value: 0,
                });
            }
            let snapshot = CanonicalSnapshot::decode(decoder.read_slice(snapshot_len)?)?;
            keyframes.push(ReplayKeyframe {
                tick,
                state_hash,
                snapshot,
            });
        }

        let result_id = AuthorityResultId::new(decoder.read_array::<AUTHORITY_RESULT_ID_BYTES>()?)?;
        let confirmed_tick = SimTick(decoder.read_u64()?);
        let state_hash = StateHash(decoder.read_u64()?);
        let result = decode_authority_result(&mut decoder)?;
        if decoder.remaining() != 0 {
            return Err(ReplayError::InvariantViolation(
                "canonical replay contains trailing bytes",
            ));
        }

        let replay = Self {
            header,
            initial_snapshot,
            inputs,
            hash_checkpoints,
            keyframes,
            final_result: FinalAuthorityResult {
                result_id,
                confirmed_tick,
                state_hash,
                result,
            },
        };
        replay.validate()?;
        Ok(replay)
    }
}

struct ReplayEncoder {
    bytes: Vec<u8>,
}

impl ReplayEncoder {
    fn new() -> Self {
        Self {
            bytes: Vec::with_capacity(16 * 1_024),
        }
    }

    fn len(&self) -> usize {
        self.bytes.len()
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), ReplayError> {
        let attempted =
            self.bytes
                .len()
                .checked_add(bytes.len())
                .ok_or(ReplayError::LimitExceeded {
                    field: "encoded replay bytes",
                    value: usize::MAX,
                    max: MAX_REPLAY_BYTES,
                })?;
        enforce_cap("encoded replay bytes", attempted, MAX_REPLAY_BYTES)?;
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn write_u8(&mut self, value: u8) -> Result<(), ReplayError> {
        self.write_bytes(&[value])
    }

    fn write_i8(&mut self, value: i8) -> Result<(), ReplayError> {
        self.write_bytes(&value.to_le_bytes())
    }

    fn write_u16(&mut self, value: u16) -> Result<(), ReplayError> {
        self.write_bytes(&value.to_le_bytes())
    }

    fn write_u32(&mut self, value: u32) -> Result<(), ReplayError> {
        self.write_bytes(&value.to_le_bytes())
    }

    fn write_u64(&mut self, value: u64) -> Result<(), ReplayError> {
        self.write_bytes(&value.to_le_bytes())
    }

    fn patch_u32(&mut self, offset: usize, value: u32) {
        self.bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
}

struct ReplayDecoder<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> ReplayDecoder<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.offset
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N], ReplayError> {
        let slice = self.read_slice(N)?;
        let mut result = [0; N];
        result.copy_from_slice(slice);
        Ok(result)
    }

    fn read_slice(&mut self, len: usize) -> Result<&'a [u8], ReplayError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(ReplayError::UnexpectedEnd {
                offset: self.offset,
                needed: len,
                remaining: self.remaining(),
            })?;
        if end > self.bytes.len() {
            return Err(ReplayError::UnexpectedEnd {
                offset: self.offset,
                needed: len,
                remaining: self.remaining(),
            });
        }
        let slice = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(slice)
    }

    fn read_u8(&mut self) -> Result<u8, ReplayError> {
        Ok(self.read_array::<1>()?[0])
    }

    fn read_i8(&mut self) -> Result<i8, ReplayError> {
        Ok(i8::from_le_bytes(self.read_array()?))
    }

    fn read_u16(&mut self) -> Result<u16, ReplayError> {
        Ok(u16::from_le_bytes(self.read_array()?))
    }

    fn read_u32(&mut self) -> Result<u32, ReplayError> {
        Ok(u32::from_le_bytes(self.read_array()?))
    }

    fn read_u64(&mut self) -> Result<u64, ReplayError> {
        Ok(u64::from_le_bytes(self.read_array()?))
    }

    fn read_bounded_len_u32(
        &mut self,
        field: &'static str,
        max: usize,
    ) -> Result<usize, ReplayError> {
        let len = self.read_u32()? as usize;
        enforce_cap(field, len, max)?;
        Ok(len)
    }

    fn require_fixed_records(
        &self,
        field: &'static str,
        count: usize,
        encoded_width: usize,
    ) -> Result<(), ReplayError> {
        let needed = count
            .checked_mul(encoded_width)
            .ok_or(ReplayError::LimitExceeded {
                field,
                value: usize::MAX,
                max: self.remaining(),
            })?;
        if needed > self.remaining() {
            return Err(ReplayError::UnexpectedEnd {
                offset: self.offset,
                needed,
                remaining: self.remaining(),
            });
        }
        Ok(())
    }
}

fn encode_compatibility(
    encoder: &mut ReplayEncoder,
    compatibility: &CompatibilityId,
) -> Result<(), ReplayError> {
    encoder.write_u16(compatibility.protocol.get())?;
    encoder.write_u16(compatibility.simulation.get())?;
    encoder.write_u16(compatibility.replay.get())?;
    encoder.write_bytes(compatibility.build.as_bytes())?;
    encoder.write_bytes(compatibility.gameplay_content.as_bytes())
}

fn decode_compatibility(decoder: &mut ReplayDecoder<'_>) -> Result<CompatibilityId, ReplayError> {
    Ok(CompatibilityId {
        protocol: ProtocolVersion::new(decoder.read_u16()?)?,
        simulation: SimulationVersion::new(decoder.read_u16()?)?,
        replay: ReplayFormatVersion::new(decoder.read_u16()?)?,
        build: BuildId::new(decoder.read_array::<16>()?)?,
        gameplay_content: GameplayContentHash::new(decoder.read_array::<32>()?)?,
    })
}

fn encode_tick_inputs(
    encoder: &mut ReplayEncoder,
    record: &ReplayTickInputs,
) -> Result<(), ReplayError> {
    encoder.write_u64(record.tick.get())?;
    for accepted in &record.fighters {
        encoder.write_u8(accepted.fighter.get())?;
        encoder.write_u8(accepted.source.code())?;
        encoder.write_u8(accepted.frame.seat.get())?;
        encoder.write_i8(accepted.frame.movement_x.get())?;
        encoder.write_i8(accepted.frame.movement_y.get())?;
        encoder.write_u16(accepted.frame.held_buttons.bits())?;
        encoder.write_u16(accepted.frame.pressed_buttons.bits())?;
        encoder.write_u16(accepted.frame.released_buttons.bits())?;
        encoder.write_u16(accepted.frame.sequence.0)?;
    }
    Ok(())
}

fn decode_tick_inputs(decoder: &mut ReplayDecoder<'_>) -> Result<ReplayTickInputs, ReplayError> {
    let tick = SimTick(decoder.read_u64()?);
    let mut fighters = FighterId::ALL.map(|fighter| AcceptedFighterInput::inactive(tick, fighter));
    for accepted in &mut fighters {
        let fighter_value = decoder.read_u8()?;
        let fighter = FighterId::new(fighter_value).ok_or(ReplayError::InvalidValue {
            field: "accepted fighter ID",
            value: u64::from(fighter_value),
        })?;
        let source_value = decoder.read_u8()?;
        let source =
            ReplayInputSource::from_code(source_value).ok_or(ReplayError::InvalidValue {
                field: "accepted input source",
                value: u64::from(source_value),
            })?;
        let frame = InputFrame {
            tick,
            seat: SeatId::new(decoder.read_u8()?)?,
            movement_x: QuantizedAxis::new(decoder.read_i8()?)?,
            movement_y: QuantizedAxis::new(decoder.read_i8()?)?,
            held_buttons: InputButtons::new(decoder.read_u16()?)?,
            pressed_buttons: InputButtons::new(decoder.read_u16()?)?,
            released_buttons: InputButtons::new(decoder.read_u16()?)?,
            sequence: InputSequence(decoder.read_u16()?),
        };
        *accepted = AcceptedFighterInput {
            fighter,
            source,
            frame,
        };
    }
    Ok(ReplayTickInputs { tick, fighters })
}

fn encode_authority_result(
    encoder: &mut ReplayEncoder,
    result: AuthorityMatchResult,
) -> Result<(), ReplayError> {
    match result {
        AuthorityMatchResult::Draw => encoder.write_u8(0),
        AuthorityMatchResult::FighterWinner(fighter) => {
            encoder.write_u8(1)?;
            encoder.write_u8(fighter.get())
        }
        AuthorityMatchResult::TeamWinner(team) => {
            encoder.write_u8(2)?;
            encoder.write_u8(team)
        }
        AuthorityMatchResult::Aborted(reason) => {
            encoder.write_u8(3)?;
            encoder.write_u16(reason)
        }
    }
}

fn decode_authority_result(
    decoder: &mut ReplayDecoder<'_>,
) -> Result<AuthorityMatchResult, ReplayError> {
    match decoder.read_u8()? {
        0 => Ok(AuthorityMatchResult::Draw),
        1 => {
            let value = decoder.read_u8()?;
            let fighter = FighterId::new(value).ok_or(ReplayError::InvalidValue {
                field: "authority winning fighter",
                value: u64::from(value),
            })?;
            Ok(AuthorityMatchResult::FighterWinner(fighter))
        }
        2 => Ok(AuthorityMatchResult::TeamWinner(decoder.read_u8()?)),
        3 => Ok(AuthorityMatchResult::Aborted(decoder.read_u16()?)),
        value => Err(ReplayError::InvalidValue {
            field: "authority result tag",
            value: u64::from(value),
        }),
    }
}

/// Minimal boundary required by the offline verifier. Implementations own a
/// headless simulation instance and must not sleep or consult presentation time.
pub trait HeadlessReplayTarget {
    type Error;

    fn restore_snapshot(&mut self, snapshot: &CanonicalSnapshot) -> Result<(), Self::Error>;

    fn step(&mut self, inputs: &ReplayTickInputs) -> Result<(), Self::Error>;

    fn state_hash(&self) -> Result<StateHash, Self::Error>;

    fn final_result(&self) -> Result<Option<AuthorityMatchResult>, Self::Error>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayTargetOperation {
    Restore,
    Step,
    Hash,
    Result,
}

#[derive(Debug)]
pub enum ReplayVerificationError<E> {
    InvalidReplay(ReplayError),
    UnknownStartTick(SimTick),
    Target {
        tick: SimTick,
        operation: ReplayTargetOperation,
        source: E,
    },
    HashDivergence {
        tick: SimTick,
        expected: StateHash,
        actual: StateHash,
    },
    FinalResultDivergence {
        tick: SimTick,
        expected: AuthorityMatchResult,
        actual: Option<AuthorityMatchResult>,
    },
}

impl<E: fmt::Display> fmt::Display for ReplayVerificationError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidReplay(error) => write!(formatter, "replay is invalid: {error}"),
            Self::UnknownStartTick(tick) => {
                write!(
                    formatter,
                    "replay does not contain requested tick {}",
                    tick.get()
                )
            }
            Self::Target {
                tick,
                operation,
                source,
            } => write!(
                formatter,
                "headless target {operation:?} failed at tick {}: {source}",
                tick.get()
            ),
            Self::HashDivergence {
                tick,
                expected,
                actual,
            } => write!(
                formatter,
                "first replay hash divergence at tick {}: expected {:016x}, got {:016x}",
                tick.get(),
                expected.0,
                actual.0
            ),
            Self::FinalResultDivergence {
                tick,
                expected,
                actual,
            } => write!(
                formatter,
                "final replay result diverged at tick {}: expected {expected:?}, got {actual:?}",
                tick.get()
            ),
        }
    }
}

impl<E: Error + 'static> Error for ReplayVerificationError<E> {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplayVerificationReport {
    pub requested_tick: SimTick,
    pub restored_tick: SimTick,
    pub final_tick: SimTick,
    pub stepped_ticks: usize,
    pub verified_checkpoints: usize,
    pub final_hash: StateHash,
    pub authority_result_id: AuthorityResultId,
}

/// Runs deterministic simulation steps as quickly as the target permits. It has no
/// wall-clock pacing and stops at the first checkpoint/final divergence.
#[derive(Clone, Copy, Debug, Default)]
pub struct HeadlessReplayRunner;

impl HeadlessReplayRunner {
    pub fn verify<T: HeadlessReplayTarget>(
        replay: &Replay,
        target: &mut T,
    ) -> Result<ReplayVerificationReport, ReplayVerificationError<T::Error>> {
        Self::verify_from(replay, replay.initial_snapshot.header.tick, target)
    }

    /// Restores the latest keyframe at or before `requested_tick`, then replays all
    /// later accepted input through the authority result.
    pub fn verify_from<T: HeadlessReplayTarget>(
        replay: &Replay,
        requested_tick: SimTick,
        target: &mut T,
    ) -> Result<ReplayVerificationReport, ReplayVerificationError<T::Error>> {
        replay
            .validate()
            .map_err(ReplayVerificationError::InvalidReplay)?;
        let requested_position = replay
            .tick_position(requested_tick)
            .ok_or(ReplayVerificationError::UnknownStartTick(requested_tick))?;

        let mut restored_snapshot = &replay.initial_snapshot;
        let mut restored_hash = StateHash(
            replay
                .initial_snapshot
                .canonical_hash()
                .map_err(|error| ReplayVerificationError::InvalidReplay(error.into()))?,
        );
        let mut restored_position = 0;
        for keyframe in &replay.keyframes {
            let position = replay
                .tick_position(keyframe.tick)
                .expect("validated keyframe ticks are in replay history");
            if position > requested_position {
                break;
            }
            restored_snapshot = &keyframe.snapshot;
            restored_hash = keyframe.state_hash;
            restored_position = position;
        }
        let restored_tick = restored_snapshot.header.tick;

        target
            .restore_snapshot(restored_snapshot)
            .map_err(|source| ReplayVerificationError::Target {
                tick: restored_tick,
                operation: ReplayTargetOperation::Restore,
                source,
            })?;
        let actual_restored_hash =
            target
                .state_hash()
                .map_err(|source| ReplayVerificationError::Target {
                    tick: restored_tick,
                    operation: ReplayTargetOperation::Hash,
                    source,
                })?;
        if actual_restored_hash != restored_hash {
            return Err(ReplayVerificationError::HashDivergence {
                tick: restored_tick,
                expected: restored_hash,
                actual: actual_restored_hash,
            });
        }

        let mut verified_checkpoints = 0;
        if let Some(checkpoint) = replay
            .hash_checkpoints
            .iter()
            .find(|checkpoint| checkpoint.tick == restored_tick)
        {
            if checkpoint.state_hash != actual_restored_hash {
                return Err(ReplayVerificationError::HashDivergence {
                    tick: restored_tick,
                    expected: checkpoint.state_hash,
                    actual: actual_restored_hash,
                });
            }
            verified_checkpoints += 1;
        }

        let mut checkpoint_index = replay
            .hash_checkpoints
            .iter()
            .position(|checkpoint| {
                replay
                    .tick_position(checkpoint.tick)
                    .expect("validated checkpoint ticks are in replay history")
                    > restored_position
            })
            .unwrap_or(replay.hash_checkpoints.len());

        let mut stepped_ticks = 0;
        for record in replay.inputs.iter().skip(restored_position) {
            target
                .step(record)
                .map_err(|source| ReplayVerificationError::Target {
                    tick: record.tick,
                    operation: ReplayTargetOperation::Step,
                    source,
                })?;
            stepped_ticks += 1;

            if let Some(checkpoint) = replay.hash_checkpoints.get(checkpoint_index)
                && checkpoint.tick == record.tick
            {
                let actual =
                    target
                        .state_hash()
                        .map_err(|source| ReplayVerificationError::Target {
                            tick: record.tick,
                            operation: ReplayTargetOperation::Hash,
                            source,
                        })?;
                if actual != checkpoint.state_hash {
                    return Err(ReplayVerificationError::HashDivergence {
                        tick: record.tick,
                        expected: checkpoint.state_hash,
                        actual,
                    });
                }
                verified_checkpoints += 1;
                checkpoint_index += 1;
            }
        }

        let final_tick = replay.final_tick();
        let final_hash = target
            .state_hash()
            .map_err(|source| ReplayVerificationError::Target {
                tick: final_tick,
                operation: ReplayTargetOperation::Hash,
                source,
            })?;
        if final_hash != replay.final_result.state_hash {
            return Err(ReplayVerificationError::HashDivergence {
                tick: final_tick,
                expected: replay.final_result.state_hash,
                actual: final_hash,
            });
        }

        let actual_result =
            target
                .final_result()
                .map_err(|source| ReplayVerificationError::Target {
                    tick: final_tick,
                    operation: ReplayTargetOperation::Result,
                    source,
                })?;
        if actual_result != Some(replay.final_result.result) {
            return Err(ReplayVerificationError::FinalResultDivergence {
                tick: final_tick,
                expected: replay.final_result.result,
                actual: actual_result,
            });
        }

        Ok(ReplayVerificationReport {
            requested_tick,
            restored_tick,
            final_tick,
            stepped_ticks,
            verified_checkpoints,
            final_hash,
            authority_result_id: replay.final_result.result_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority_input::{
        AuthorityInputOrigin, AuthorityInputRecord, AuthorityInputStatus, CommittedTickInputs,
    };
    use crate::network_protocol::{
        AuthorityKind, DefinitionId, FighterSlotConfig, PeerId, SIMULATION_HZ, SeatAssignment,
        SeatOwnership, TeamId,
    };
    use crate::snapshot::{
        ArenaRuntimeSnapshot, FighterInputSnapshot, FighterSnapshot, MatchPhaseSnapshot,
        MatchRulesSnapshot, MatchStateSnapshot, MatchStatsSnapshot, PoolAllocatorSnapshot,
        SnapshotHeader,
    };

    const INITIAL_TICK: u64 = 100;
    const FINAL_TICK: u64 = 108;
    const CONFIG_HASH: u64 = 0x1122_3344_5566_7788;
    const MASTER_SEED: u64 = 0x8877_6655_4433_2211;
    const MATCH_BYTES: [u8; 16] = [0x44; 16];

    fn compatibility() -> CompatibilityId {
        CompatibilityId {
            protocol: ProtocolVersion::new(3).unwrap(),
            simulation: SimulationVersion::new(3).unwrap(),
            replay: ReplayFormatVersion::new(REPLAY_SCHEMA_VERSION).unwrap(),
            build: BuildId::new([0x22; 16]).unwrap(),
            gameplay_content: GameplayContentHash::new([0x33; 32]).unwrap(),
        }
    }

    fn initial_snapshot() -> CanonicalSnapshot {
        let mut fighters = FighterId::ALL.map(FighterSnapshot::empty);
        for fighter in fighters.iter_mut().take(2) {
            fighter.occupied = true;
            fighter.active = true;
            fighter.health = 400_000;
            fighter.stamina = 100_000;
        }
        let allocators = crate::determinism::SimEntityKind::ALL
            .into_iter()
            .map(|kind| PoolAllocatorSnapshot::empty(kind, 2).unwrap())
            .collect();
        CanonicalSnapshot {
            header: SnapshotHeader::new(
                3,
                3,
                CONFIG_HASH,
                MATCH_BYTES,
                SimTick(INITIAL_TICK),
                MASTER_SEED,
            ),
            match_state: MatchStateSnapshot {
                phase: MatchPhaseSnapshot::Fight,
                phase_ticks: 10,
                match_ticks_remaining: 1_000,
                active_slots_mask: 0b0011,
                teams: [0, 1, 0, 0],
                stocks: [3, 3, 0, 0],
                rules: MatchRulesSnapshot {
                    ruleset_id: 5,
                    arena_id: 9,
                    duration_ticks: 1_200,
                    starting_stocks: 3,
                    score_limit: 10,
                    team_mode: false,
                    friendly_fire: false,
                },
                ..Default::default()
            },
            fighters,
            arena: ArenaRuntimeSnapshot::default(),
            allocators,
            dynamic_objects: Vec::new(),
            rng_streams: Vec::new(),
            stats: MatchStatsSnapshot::default(),
        }
    }

    fn recorder_manifest() -> MatchManifest {
        let peer = PeerId::new(7).unwrap();
        let ownership = SeatOwnership::from_assignments(&[
            SeatAssignment {
                seat: SeatId::new(0).unwrap(),
                fighter: FighterId::new(0).unwrap(),
                owner: SeatOwner::Peer(peer),
            },
            SeatAssignment {
                seat: SeatId::new(1).unwrap(),
                fighter: FighterId::new(1).unwrap(),
                owner: SeatOwner::AuthorityBot,
            },
        ])
        .unwrap();
        let mut slots = [FighterSlotConfig::default(); FIGHTER_CAPACITY as usize];
        for (index, slot) in slots.iter_mut().take(2).enumerate() {
            *slot = FighterSlotConfig {
                occupied: true,
                fighter: FighterId::from_index(index).unwrap(),
                team: TeamId::new(index as u8).unwrap(),
                character: DefinitionId::new(index as u16).unwrap(),
                style: DefinitionId::new(0).unwrap(),
                equipment: DefinitionId::new(0).unwrap(),
            };
        }
        MatchManifest {
            compatibility: compatibility(),
            manifest_hash: ManifestHash(0xaabb_ccdd_eeff_1122),
            match_id: MatchId::new(MATCH_BYTES).unwrap(),
            authority: AuthorityKind::Listen,
            trusted_results: false,
            arena: DefinitionId::new(9).unwrap(),
            rules: DefinitionId::new(5).unwrap(),
            slots,
            ownership,
            master_gameplay_seed: MASTER_SEED,
            rng_scheme_version: 1,
            tick_rate_hz: SIMULATION_HZ,
            input_delay_ticks: 2,
            rollback_limit_ticks: 8,
            snapshot_history_ticks: 32,
            agreed_start_tick: SimTick(INITIAL_TICK + 10),
        }
    }

    fn authority_report(
        tick: u64,
        sequence: u16,
        peer_origin: AuthorityInputOrigin,
        state_hash: StateHash,
        final_result_id: Option<u64>,
    ) -> AuthorityTickReport {
        let tape = input_record(tick, sequence);
        let mut committed = CommittedTickInputs {
            tick: SimTick(tick),
            by_seat: [None; FIGHTER_CAPACITY as usize],
        };
        committed.by_seat[0] = Some(AuthorityInputRecord {
            frame: tape.fighters[0].frame,
            fighter: FighterId::new(0).unwrap(),
            origin: peer_origin,
            status: AuthorityInputStatus::Committed,
        });
        committed.by_seat[1] = Some(AuthorityInputRecord {
            frame: tape.fighters[1].frame,
            fighter: FighterId::new(1).unwrap(),
            origin: AuthorityInputOrigin::AuthorityBot,
            status: AuthorityInputStatus::Committed,
        });
        AuthorityTickReport {
            tick: SimTick(tick),
            state_hash,
            committed_inputs: committed,
            substituted_inputs: u8::from(matches!(
                peer_origin,
                AuthorityInputOrigin::MissingSubstitute
            )),
            final_result_id,
        }
    }

    fn accepted_input(
        tick: SimTick,
        fighter: FighterId,
        sequence: u16,
        source: ReplayInputSource,
        axis: i8,
    ) -> AcceptedFighterInput {
        AcceptedFighterInput {
            fighter,
            source,
            frame: InputFrame {
                tick,
                seat: SeatId::new(fighter.get()).unwrap(),
                movement_x: QuantizedAxis::new(axis).unwrap(),
                movement_y: QuantizedAxis::new(-axis).unwrap(),
                held_buttons: InputButtons::new(InputButtons::LIGHT).unwrap(),
                pressed_buttons: InputButtons::new(if sequence % 2 == 0 {
                    InputButtons::JUMP
                } else {
                    0
                })
                .unwrap(),
                released_buttons: InputButtons::new(0).unwrap(),
                sequence: InputSequence(sequence),
            },
        }
    }

    fn input_record(tick: u64, sequence: u16) -> ReplayTickInputs {
        let tick = SimTick(tick);
        let mut record = ReplayTickInputs::all_inactive(tick);
        record.fighters[0] = accepted_input(
            tick,
            FighterId::new(0).unwrap(),
            sequence,
            ReplayInputSource::Peer,
            sequence as i8,
        );
        record.fighters[1] = accepted_input(
            tick,
            FighterId::new(1).unwrap(),
            sequence,
            ReplayInputSource::AuthorityBot,
            -(sequence as i8),
        );
        record
    }

    #[derive(Clone, Debug)]
    struct TestTarget {
        snapshot: Option<CanonicalSnapshot>,
        final_tick: SimTick,
        hash_bias_tick: Option<SimTick>,
        result_override: Option<AuthorityMatchResult>,
    }

    impl TestTarget {
        fn new(final_tick: SimTick) -> Self {
            Self {
                snapshot: None,
                final_tick,
                hash_bias_tick: None,
                result_override: None,
            }
        }

        fn snapshot(&self) -> &CanonicalSnapshot {
            self.snapshot.as_ref().unwrap()
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct TestTargetError(&'static str);

    impl fmt::Display for TestTargetError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(self.0)
        }
    }

    impl Error for TestTargetError {}

    impl HeadlessReplayTarget for TestTarget {
        type Error = TestTargetError;

        fn restore_snapshot(&mut self, snapshot: &CanonicalSnapshot) -> Result<(), Self::Error> {
            self.snapshot = Some(snapshot.clone());
            Ok(())
        }

        fn step(&mut self, inputs: &ReplayTickInputs) -> Result<(), Self::Error> {
            let snapshot = self
                .snapshot
                .as_mut()
                .ok_or(TestTargetError("step before restore"))?;
            if inputs.tick != snapshot.header.tick.next() {
                return Err(TestTargetError("noncontiguous test input"));
            }

            let mut delta = 0_u64;
            for (index, accepted) in inputs.fighters.iter().enumerate() {
                if accepted.source.is_active() {
                    delta = delta
                        .wrapping_add(accepted.frame.movement_x.get().unsigned_abs() as u64)
                        .wrapping_add(u64::from(accepted.frame.held_buttons.bits()))
                        .wrapping_add(u64::from(accepted.frame.pressed_buttons.bits()));
                    snapshot.fighters[index].input = FighterInputSnapshot {
                        move_x: i16::from(accepted.frame.movement_x.get()),
                        move_y: i16::from(accepted.frame.movement_y.get()),
                        held_buttons: u32::from(accepted.frame.held_buttons.bits()),
                        pressed_latches: u32::from(accepted.frame.pressed_buttons.bits()),
                        released_latches: u32::from(accepted.frame.released_buttons.bits()),
                    };
                }
            }
            snapshot.header.tick = inputs.tick;
            snapshot.stats.gameplay_ticks = snapshot.stats.gameplay_ticks.wrapping_add(delta);
            snapshot.stats.emitted_events = snapshot.stats.emitted_events.wrapping_add(1);
            snapshot.match_state.phase_ticks = snapshot.match_state.phase_ticks.wrapping_add(1);
            snapshot.match_state.match_ticks_remaining =
                snapshot.match_state.match_ticks_remaining.saturating_sub(1);

            if inputs.tick == self.final_tick {
                snapshot.match_state.phase = MatchPhaseSnapshot::Result;
                snapshot.match_state.result = MatchResultSnapshot::FighterWinner {
                    fighter: FighterId::new(0).unwrap(),
                    decided_tick: inputs.tick,
                };
            }
            Ok(())
        }

        fn state_hash(&self) -> Result<StateHash, Self::Error> {
            let snapshot = self
                .snapshot
                .as_ref()
                .ok_or(TestTargetError("hash before restore"))?;
            let mut hash = snapshot
                .canonical_hash()
                .map_err(|_| TestTargetError("snapshot hash failed"))?;
            if self.hash_bias_tick == Some(snapshot.header.tick) {
                hash ^= 1;
            }
            Ok(StateHash(hash))
        }

        fn final_result(&self) -> Result<Option<AuthorityMatchResult>, Self::Error> {
            if let Some(result) = self.result_override {
                return Ok(Some(result));
            }
            Ok(authority_result_from_snapshot(self.snapshot()))
        }
    }

    fn fixture() -> Replay {
        fixture_for_simulation_version(3)
    }

    fn fixture_for_simulation_version(simulation_version: u16) -> Replay {
        let mut initial_snapshot = initial_snapshot();
        initial_snapshot.header.simulation_version = u32::from(simulation_version);
        let inputs = (INITIAL_TICK + 1..=FINAL_TICK)
            .enumerate()
            .map(|(index, tick)| input_record(tick, index as u16 + 1))
            .collect::<Vec<_>>();

        let mut generator = TestTarget::new(SimTick(FINAL_TICK));
        generator.restore_snapshot(&initial_snapshot).unwrap();
        let mut hash_checkpoints = vec![ReplayHashCheckpoint {
            tick: initial_snapshot.header.tick,
            state_hash: generator.state_hash().unwrap(),
        }];
        let mut keyframes = Vec::new();
        for record in &inputs {
            generator.step(record).unwrap();
            if record.tick.get() % 2 == 0 {
                let state_hash = generator.state_hash().unwrap();
                hash_checkpoints.push(ReplayHashCheckpoint {
                    tick: record.tick,
                    state_hash,
                });
                if record.tick == SimTick(104) {
                    keyframes.push(ReplayKeyframe {
                        tick: record.tick,
                        state_hash,
                        snapshot: generator.snapshot().clone(),
                    });
                }
            }
        }
        let final_hash = generator.state_hash().unwrap();

        Replay {
            header: ReplayHeader::new(
                CompatibilityId {
                    simulation: SimulationVersion::new(simulation_version).unwrap(),
                    ..compatibility()
                },
                MatchId::new(MATCH_BYTES).unwrap(),
                ManifestHash(0xaabb_ccdd_eeff_1122),
                CONFIG_HASH,
                MASTER_SEED,
            ),
            initial_snapshot,
            inputs,
            hash_checkpoints,
            keyframes,
            final_result: FinalAuthorityResult {
                result_id: AuthorityResultId::new([0x66; AUTHORITY_RESULT_ID_BYTES]).unwrap(),
                confirmed_tick: SimTick(FINAL_TICK),
                state_hash: final_hash,
                result: AuthorityMatchResult::FighterWinner(FighterId::new(0).unwrap()),
            },
        }
    }

    fn decoder_after_initial_snapshot(encoded: &[u8]) -> ReplayDecoder<'_> {
        let mut decoder = ReplayDecoder::new(encoded);
        decoder.read_array::<4>().unwrap();
        decoder.read_u16().unwrap();
        decoder.read_u32().unwrap();
        decode_compatibility(&mut decoder).unwrap();
        decoder.read_array::<16>().unwrap();
        decoder.read_u64().unwrap();
        decoder.read_u64().unwrap();
        decoder.read_u64().unwrap();
        let snapshot_len = decoder.read_u32().unwrap() as usize;
        decoder.read_slice(snapshot_len).unwrap();
        decoder
    }

    #[test]
    fn authority_recorder_preserves_peer_bot_and_substitution_origins() {
        let initial = initial_snapshot();
        let mut recorder =
            AuthorityReplayRecorder::new(recorder_manifest(), initial.clone()).unwrap();

        let mut tick_one = initial.clone();
        tick_one.header.tick = SimTick(INITIAL_TICK + 1);
        let tick_one_hash = StateHash(tick_one.canonical_hash().unwrap());
        let first = authority_report(
            INITIAL_TICK + 1,
            1,
            AuthorityInputOrigin::Peer(PeerId::new(7).unwrap()),
            tick_one_hash,
            None,
        );
        recorder
            .record_tick(&first, &tick_one, false, false)
            .unwrap();

        let mut final_snapshot = tick_one.clone();
        final_snapshot.header.tick = SimTick(INITIAL_TICK + 2);
        final_snapshot.match_state.phase = MatchPhaseSnapshot::Result;
        final_snapshot.match_state.result = MatchResultSnapshot::FighterWinner {
            fighter: FighterId::new(0).unwrap(),
            decided_tick: final_snapshot.header.tick,
        };
        let final_hash = StateHash(final_snapshot.canonical_hash().unwrap());
        let final_report = authority_report(
            INITIAL_TICK + 2,
            2,
            AuthorityInputOrigin::MissingSubstitute,
            final_hash,
            Some(0x8877),
        );
        recorder
            .record_tick(&final_report, &final_snapshot, true, true)
            .unwrap();
        let replay = recorder.finish(&final_snapshot, 0x8877).unwrap();

        assert_eq!(replay.inputs.len(), 2);
        assert_eq!(replay.inputs[0].fighters[0].source, ReplayInputSource::Peer);
        assert_eq!(
            replay.inputs[1].fighters[0].source,
            ReplayInputSource::AuthoritySubstitution
        );
        assert!(
            replay
                .inputs
                .iter()
                .all(|tick| { tick.fighters[1].source == ReplayInputSource::AuthorityBot })
        );
        assert_eq!(
            replay.final_result.confirmed_tick,
            SimTick(INITIAL_TICK + 2)
        );
        replay.validate().unwrap();
    }

    #[test]
    fn replay_round_trip_preserves_manifest_inputs_bots_hashes_keyframes_and_result() {
        let replay = fixture();
        let encoded = replay.encode().unwrap();
        let restored = Replay::decode(&encoded).unwrap();

        assert_eq!(restored, replay);
        assert_eq!(restored.encode().unwrap(), encoded);
        assert_eq!(
            restored.inputs[0].fighters[1].source,
            ReplayInputSource::AuthorityBot
        );
        assert_eq!(restored.keyframes[0].tick, SimTick(104));
        assert!(encoded.len() < MAX_REPLAY_BYTES);
    }

    #[test]
    fn replay_envelope_uses_frozen_magic_version_and_declared_length() {
        let encoded = fixture().encode().unwrap();
        assert_eq!(&encoded[0..4], &REPLAY_MAGIC);
        assert_eq!(
            u16::from_le_bytes(encoded[4..6].try_into().unwrap()),
            REPLAY_SCHEMA_VERSION
        );
        assert_eq!(
            u32::from_le_bytes(encoded[6..10].try_into().unwrap()) as usize,
            encoded.len()
        );
    }

    #[test]
    fn all_final_authority_result_variants_round_trip() {
        let variants = [
            AuthorityMatchResult::Draw,
            AuthorityMatchResult::FighterWinner(FighterId::new(1).unwrap()),
            AuthorityMatchResult::TeamWinner(2),
            AuthorityMatchResult::Aborted(77),
        ];
        for result in variants {
            let mut replay = fixture();
            replay.final_result.result = result;
            let restored = Replay::decode(&replay.encode().unwrap()).unwrap();
            assert_eq!(restored.final_result.result, result);
        }
    }

    #[test]
    fn replay_validation_rejects_gaps_duplicates_and_reordered_diagnostics() {
        let mut replay = fixture();
        replay.inputs[2].tick = replay.inputs[1].tick;
        let duplicate_tick = replay.inputs[2].tick;
        for accepted in &mut replay.inputs[2].fighters {
            accepted.frame.tick = duplicate_tick;
        }
        assert!(matches!(
            replay.validate(),
            Err(ReplayError::NonCanonicalOrder {
                field: "input ticks"
            })
        ));

        let mut replay = fixture();
        replay
            .hash_checkpoints
            .insert(1, replay.hash_checkpoints[0]);
        assert!(matches!(
            replay.validate(),
            Err(ReplayError::NonCanonicalOrder {
                field: "hash checkpoints"
            })
        ));

        let mut replay = fixture();
        replay.keyframes.push(replay.keyframes[0].clone());
        assert!(matches!(
            replay.validate(),
            Err(ReplayError::NonCanonicalOrder { field: "keyframes" })
        ));
    }

    #[test]
    fn replay_validation_rejects_incomplete_slot_and_sequence_history() {
        let mut replay = fixture();
        replay.inputs[0].fighters[0].fighter = FighterId::new(1).unwrap();
        assert!(replay.validate().is_err());

        let mut replay = fixture();
        replay.inputs[1].fighters[0].frame.sequence = InputSequence(99);
        assert!(matches!(
            replay.validate(),
            Err(ReplayError::NonCanonicalOrder {
                field: "accepted input sequences"
            })
        ));

        let mut replay = fixture();
        replay.inputs[1].fighters[2].frame.sequence = InputSequence(1);
        assert!(replay.validate().is_err());
    }

    #[test]
    fn peer_substitution_is_allowed_but_bot_ownership_cannot_change() {
        let mut replay = fixture();
        replay.inputs[2].fighters[0].source = ReplayInputSource::AuthoritySubstitution;
        replay.validate().unwrap();

        replay.inputs[2].fighters[1].source = ReplayInputSource::Peer;
        assert!(replay.validate().is_err());
    }

    #[test]
    fn replay_binding_rejects_compatibility_match_config_and_seed_mismatches() {
        let mut replay = fixture();
        replay.header.master_seed ^= 1;
        assert!(replay.validate().is_err());

        let mut replay = fixture();
        replay.header.match_config_hash ^= 1;
        assert!(replay.validate().is_err());

        let mut replay = fixture();
        replay.header.match_id = MatchId::new([0x99; 16]).unwrap();
        assert!(replay.validate().is_err());

        let mut replay = fixture();
        replay.header.compatibility.simulation = SimulationVersion::new(5).unwrap();
        assert!(replay.validate().is_err());
    }

    #[test]
    fn v4_replay_is_rejected_by_v5_playback_compatibility() {
        let replay = fixture_for_simulation_version(4);
        let expected = CompatibilityId {
            simulation: SimulationVersion::new(5).unwrap(),
            ..replay.header.compatibility
        };

        assert_eq!(replay.header.compatibility.simulation.get(), 4);
        assert_eq!(expected.simulation.get(), 5);
        let result = replay.validate_against(&expected);
        assert!(
            matches!(
                &result,
                Err(ReplayError::Protocol(
                    ProtocolValidationError::SimulationVersionMismatch
                ))
            ),
            "unexpected compatibility result: {result:?}"
        );
    }

    #[test]
    fn keyframe_hash_and_final_hash_must_match_canonical_diagnostics() {
        let mut replay = fixture();
        replay.keyframes[0].state_hash.0 ^= 1;
        assert!(replay.validate().is_err());

        let mut replay = fixture();
        replay.final_result.state_hash.0 ^= 1;
        assert!(replay.validate().is_err());
    }

    #[test]
    fn decoder_rejects_magic_version_length_and_all_truncated_prefixes() {
        let encoded = fixture().encode().unwrap();

        let mut bad_magic = encoded.clone();
        bad_magic[0] ^= 1;
        assert!(matches!(
            Replay::decode(&bad_magic),
            Err(ReplayError::InvalidMagic(_))
        ));

        let mut bad_version = encoded.clone();
        bad_version[4..6].copy_from_slice(&(REPLAY_SCHEMA_VERSION + 1).to_le_bytes());
        assert!(matches!(
            Replay::decode(&bad_version),
            Err(ReplayError::UnsupportedSchemaVersion { .. })
        ));

        let mut bad_length = encoded.clone();
        bad_length[6..10].copy_from_slice(&((encoded.len() + 1) as u32).to_le_bytes());
        assert!(matches!(
            Replay::decode(&bad_length),
            Err(ReplayError::DeclaredLengthMismatch { .. })
        ));

        let mut over_cap = encoded.clone();
        over_cap[6..10].copy_from_slice(&((MAX_REPLAY_BYTES + 1) as u32).to_le_bytes());
        assert!(matches!(
            Replay::decode(&over_cap),
            Err(ReplayError::LimitExceeded {
                field: "declared replay bytes",
                ..
            })
        ));

        for len in 0..encoded.len() {
            assert!(
                Replay::decode(&encoded[..len]).is_err(),
                "decoded prefix {len}"
            );
        }
    }

    #[test]
    fn decoder_rejects_corrupt_input_count_before_allocating() {
        let original = fixture().encode().unwrap();
        let decoder = decoder_after_initial_snapshot(&original);
        let count_offset = decoder.offset;
        let mut encoded = original.clone();
        encoded[count_offset..count_offset + 4]
            .copy_from_slice(&((MAX_REPLAY_TICKS + 1) as u32).to_le_bytes());

        assert!(matches!(
            Replay::decode(&encoded),
            Err(ReplayError::LimitExceeded {
                field: "input tick count",
                ..
            })
        ));

        let mut impossible_for_file = original;
        impossible_for_file[count_offset..count_offset + 4]
            .copy_from_slice(&1_000_u32.to_le_bytes());
        assert!(matches!(
            Replay::decode(&impossible_for_file),
            Err(ReplayError::UnexpectedEnd { .. })
        ));
    }

    #[test]
    fn decoder_rejects_invalid_fighter_axis_buttons_and_nested_lengths() {
        let encoded = fixture().encode().unwrap();
        let decoder = decoder_after_initial_snapshot(&encoded);
        let input_count_offset = decoder.offset;
        let first_record_offset = input_count_offset + 4;

        let mut bad_fighter = encoded.clone();
        bad_fighter[first_record_offset + 8] = FIGHTER_CAPACITY;
        assert!(matches!(
            Replay::decode(&bad_fighter),
            Err(ReplayError::InvalidValue {
                field: "accepted fighter ID",
                ..
            })
        ));

        let mut bad_axis = encoded.clone();
        bad_axis[first_record_offset + 8 + 3] = i8::MIN as u8;
        assert!(matches!(
            Replay::decode(&bad_axis),
            Err(ReplayError::Protocol(_))
        ));

        let mut bad_buttons = encoded.clone();
        let held_offset = first_record_offset + 8 + 5;
        bad_buttons[held_offset..held_offset + 2].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(matches!(
            Replay::decode(&bad_buttons),
            Err(ReplayError::Protocol(_))
        ));

        let mut bad_initial_len = encoded;
        let mut header_decoder = ReplayDecoder::new(&bad_initial_len);
        header_decoder.read_array::<4>().unwrap();
        header_decoder.read_u16().unwrap();
        header_decoder.read_u32().unwrap();
        decode_compatibility(&mut header_decoder).unwrap();
        header_decoder.read_array::<16>().unwrap();
        header_decoder.read_u64().unwrap();
        header_decoder.read_u64().unwrap();
        header_decoder.read_u64().unwrap();
        let length_offset = header_decoder.offset;
        bad_initial_len[length_offset..length_offset + 4]
            .copy_from_slice(&((MAX_SNAPSHOT_BYTES + 1) as u32).to_le_bytes());
        assert!(matches!(
            Replay::decode(&bad_initial_len),
            Err(ReplayError::LimitExceeded {
                field: "initial snapshot bytes",
                ..
            })
        ));

        let mut bad_keyframe_len = fixture().encode().unwrap();
        let mut keyframe_decoder = decoder_after_initial_snapshot(&bad_keyframe_len);
        let input_count = keyframe_decoder.read_u32().unwrap();
        for _ in 0..input_count {
            decode_tick_inputs(&mut keyframe_decoder).unwrap();
        }
        let hash_count = keyframe_decoder.read_u32().unwrap();
        for _ in 0..hash_count {
            keyframe_decoder.read_u64().unwrap();
            keyframe_decoder.read_u64().unwrap();
        }
        assert_eq!(keyframe_decoder.read_u32().unwrap(), 1);
        keyframe_decoder.read_u64().unwrap();
        keyframe_decoder.read_u64().unwrap();
        let keyframe_length_offset = keyframe_decoder.offset;
        bad_keyframe_len[keyframe_length_offset..keyframe_length_offset + 4]
            .copy_from_slice(&((MAX_SNAPSHOT_BYTES + 1) as u32).to_le_bytes());
        assert!(matches!(
            Replay::decode(&bad_keyframe_len),
            Err(ReplayError::LimitExceeded {
                field: "keyframe snapshot bytes",
                ..
            })
        ));
    }

    #[test]
    fn arbitrary_bytes_never_panic_or_bypass_validation() {
        let mut state = 0x1234_5678_9abc_def0_u64;
        for len in 0..256 {
            let mut bytes = vec![0; len];
            for byte in &mut bytes {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *byte = state as u8;
            }
            let decoded = std::panic::catch_unwind(|| Replay::decode(&bytes));
            assert!(
                decoded.is_ok(),
                "decoder panicked for {len} arbitrary bytes"
            );
            assert!(decoded.unwrap().is_err());
        }
    }

    #[test]
    fn authority_result_id_and_collection_caps_are_strict() {
        assert!(AuthorityResultId::new([0; AUTHORITY_RESULT_ID_BYTES]).is_err());

        let mut replay = fixture();
        replay.hash_checkpoints = vec![
            ReplayHashCheckpoint {
                tick: replay.initial_snapshot.header.tick,
                state_hash: StateHash(1),
            };
            MAX_REPLAY_HASH_CHECKPOINTS + 1
        ];
        assert!(matches!(
            replay.validate(),
            Err(ReplayError::LimitExceeded {
                field: "hash checkpoint count",
                ..
            })
        ));

        let over_cap_keyframes = ((MAX_REPLAY_KEYFRAMES + 1) as u32).to_le_bytes();
        let mut decoder = ReplayDecoder::new(&over_cap_keyframes);
        assert!(matches!(
            decoder.read_bounded_len_u32("keyframe count", MAX_REPLAY_KEYFRAMES),
            Err(ReplayError::LimitExceeded {
                field: "keyframe count",
                ..
            })
        ));
    }

    #[test]
    fn headless_runner_verifies_full_replay_faster_than_realtime() {
        let replay = fixture();
        let mut target = TestTarget::new(SimTick(FINAL_TICK));
        let report = HeadlessReplayRunner::verify(&replay, &mut target).unwrap();

        assert_eq!(report.requested_tick, SimTick(INITIAL_TICK));
        assert_eq!(report.restored_tick, SimTick(INITIAL_TICK));
        assert_eq!(report.final_tick, SimTick(FINAL_TICK));
        assert_eq!(report.stepped_ticks, (FINAL_TICK - INITIAL_TICK) as usize);
        assert_eq!(report.verified_checkpoints, replay.hash_checkpoints.len());
        assert_eq!(report.final_hash, replay.final_result.state_hash);
        assert_eq!(report.authority_result_id, replay.final_result.result_id);
    }

    #[test]
    fn restore_keyframe_then_replay_reproduces_later_hashes_and_result() {
        let replay = fixture();
        let mut target = TestTarget::new(SimTick(FINAL_TICK));
        let report = HeadlessReplayRunner::verify_from(&replay, SimTick(105), &mut target).unwrap();

        assert_eq!(report.requested_tick, SimTick(105));
        assert_eq!(report.restored_tick, SimTick(104));
        assert_eq!(report.stepped_ticks, 4);
        assert_eq!(report.verified_checkpoints, 3);
        assert_eq!(report.final_hash, replay.final_result.state_hash);
    }

    #[test]
    fn runner_reports_the_first_checkpoint_divergence() {
        let replay = fixture();
        let mut target = TestTarget::new(SimTick(FINAL_TICK));
        target.hash_bias_tick = Some(SimTick(106));

        assert!(matches!(
            HeadlessReplayRunner::verify(&replay, &mut target),
            Err(ReplayVerificationError::HashDivergence {
                tick: SimTick(106),
                ..
            })
        ));
    }

    #[test]
    fn runner_reports_final_result_divergence_and_unknown_seek_tick() {
        let replay = fixture();
        let mut target = TestTarget::new(SimTick(FINAL_TICK));
        target.result_override = Some(AuthorityMatchResult::Draw);
        assert!(matches!(
            HeadlessReplayRunner::verify(&replay, &mut target),
            Err(ReplayVerificationError::FinalResultDivergence { .. })
        ));

        let mut target = TestTarget::new(SimTick(FINAL_TICK));
        assert!(matches!(
            HeadlessReplayRunner::verify_from(&replay, SimTick(99), &mut target),
            Err(ReplayVerificationError::UnknownStartTick(SimTick(99)))
        ));
    }

    #[test]
    fn tick_positions_remain_correct_across_u64_wrap() {
        let mut replay = fixture();
        replay.initial_snapshot.header.tick = SimTick(u64::MAX - 1);
        replay.inputs.clear();
        replay.inputs.push(input_record(u64::MAX, 1));
        replay.inputs.push(input_record(0, 2));
        let initial_hash = StateHash(replay.initial_snapshot.canonical_hash().unwrap());
        replay.keyframes.clear();
        replay.final_result.confirmed_tick = SimTick(0);

        let mut target = TestTarget::new(SimTick(0));
        target.restore_snapshot(&replay.initial_snapshot).unwrap();
        for record in &replay.inputs {
            target.step(record).unwrap();
        }
        replay.final_result.state_hash = target.state_hash().unwrap();
        replay.hash_checkpoints = vec![
            ReplayHashCheckpoint {
                tick: replay.initial_snapshot.header.tick,
                state_hash: initial_hash,
            },
            ReplayHashCheckpoint {
                tick: SimTick(0),
                state_hash: replay.final_result.state_hash,
            },
        ];
        replay.validate().unwrap();
        assert_eq!(replay.tick_position(SimTick(u64::MAX - 1)), Some(0));
        assert_eq!(replay.tick_position(SimTick(u64::MAX)), Some(1));
        assert_eq!(replay.tick_position(SimTick(0)), Some(2));
    }
}
