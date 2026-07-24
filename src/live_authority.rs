//! Production bridge from committed network input to the live Bevy simulation.
//!
//! This driver owns an already-configured simulation [`App`]. It adds only the
//! input application boundary; construction of the headless/live app remains a
//! separate bootstrap concern. Every step validates the complete fighter/input
//! mapping before running exactly one `FixedUpdate` schedule.

use bevy::prelude::*;
use std::error::Error;
use std::fmt;

use crate::arena::ArenaHazardState;
use crate::arena_defs::ActiveArena;
use crate::authority::AuthoritySimulation;
use crate::authority_input::{
    AuthorityInputOrigin, AuthorityInputRecord, AuthorityInputStatus, CommittedTickInputs,
};
use crate::bot::{AuthorityBotInputError, AuthorityBotInputGenerator, ExternallyStagedBotInput};
use crate::components::{Fighter, FighterInput};
use crate::determinism::{CanonicalHash64, FighterId};
use crate::game_state::{MatchPhase, MatchState};
use crate::live_input::network_input_to_fighter_input;
use crate::live_world_snapshot::LiveWorldSnapshotAdapter;
use crate::network_protocol::{
    InputFrame, MAX_SEATS, PeerId, ProtocolValidationError, SeatId, SeatOwner, SeatOwnership,
    SimTick,
};
use crate::replay::{
    AuthorityMatchResult, HeadlessReplayTarget, ReplayInputSource, ReplayTickInputs,
    authority_result_from_snapshot,
};
use crate::rollback::RollbackWorld;
use crate::simulation::SimulationSet;
use crate::snapshot::CanonicalSnapshot;
use crate::snapshot_ecs::{EcsSnapshotError, EcsSnapshotRestoreReport};

#[derive(Resource, Clone, Copy, Debug, Default)]
struct StagedCommittedInputs {
    tick: Option<SimTick>,
    by_fighter: [Option<InputFrame>; MAX_SEATS],
    expected_mask: u8,
    observed_tick: Option<SimTick>,
    applied_mask: u8,
}

impl StagedCommittedInputs {
    fn stage(&mut self, tick: SimTick, prepared: PreparedInputs) {
        *self = Self {
            tick: Some(tick),
            by_fighter: prepared.by_fighter,
            expected_mask: prepared.expected_mask,
            observed_tick: None,
            applied_mask: 0,
        };
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}

#[derive(Clone, Copy, Debug)]
struct PreparedInputs {
    by_fighter: [Option<InputFrame>; MAX_SEATS],
    expected_mask: u8,
}

/// Applies the staged, already-validated frames at the canonical input boundary.
///
/// It is ordered after legacy local/bot producers so a live authority remains
/// authoritative even when driven with the current full game schedule. Gameplay
/// modifiers such as drunk-direction inversion still run after this system.
fn apply_staged_committed_inputs(
    tick: Res<SimTick>,
    mut staged: ResMut<StagedCommittedInputs>,
    mut fighters: Query<(&Fighter, &mut FighterInput)>,
) {
    let Some(expected_tick) = staged.tick else {
        return;
    };
    staged.observed_tick = Some(*tick);
    if *tick != expected_tick {
        return;
    }

    let frames = staged.by_fighter;
    let mut applied_mask = 0_u8;
    for (fighter, mut input) in &mut fighters {
        let Some(id) = FighterId::from_index(fighter.id) else {
            continue;
        };
        let Some(frame) = frames[id.index()] else {
            continue;
        };
        *input = network_input_to_fighter_input(frame);
        applied_mask |= 1 << id.get();
    }
    staged.applied_mask = applied_mask;
}

/// Fail-closed errors at the live authority/rollback ECS boundary.
#[derive(Debug, PartialEq, Eq)]
pub enum LiveSimulationError {
    MissingResource(&'static str),
    Protocol(ProtocolValidationError),
    TickGap {
        expected: SimTick,
        found: SimTick,
    },
    InvalidFighterEntityId {
        entity: Entity,
        found: usize,
    },
    DuplicateFighterEntity {
        fighter: FighterId,
        first: Entity,
        duplicate: Entity,
    },
    MissingFighterEntity(FighterId),
    MissingFighterInputComponent {
        fighter: FighterId,
        entity: Entity,
    },
    InvalidInput {
        seat: SeatId,
        source: ProtocolValidationError,
    },
    InputFrameTickMismatch {
        seat: SeatId,
        expected: SimTick,
        found: SimTick,
    },
    InputFrameSeatMismatch {
        array_seat: SeatId,
        frame_seat: SeatId,
    },
    UncommittedInput(SeatId),
    UnexpectedSeatInput(SeatId),
    InputFighterMismatch {
        seat: SeatId,
        expected: FighterId,
        found: FighterId,
    },
    DuplicateFighterInput(FighterId),
    ReplayFighterArrayMismatch {
        index: usize,
        found: FighterId,
    },
    DuplicateReplaySeat(SeatId),
    MissingFighterInput(FighterId),
    InactiveSlotInput(FighterId),
    ScheduleTickMismatch {
        expected: SimTick,
        found: SimTick,
    },
    InputApplicationMismatch {
        tick: SimTick,
        observed_tick: Option<SimTick>,
        expected_mask: u8,
        applied_mask: u8,
    },
    AuthorityBotInput(AuthorityBotInputError),
    Snapshot(EcsSnapshotError),
}

impl From<ProtocolValidationError> for LiveSimulationError {
    fn from(error: ProtocolValidationError) -> Self {
        Self::Protocol(error)
    }
}

impl From<EcsSnapshotError> for LiveSimulationError {
    fn from(error: EcsSnapshotError) -> Self {
        Self::Snapshot(error)
    }
}

impl From<AuthorityBotInputError> for LiveSimulationError {
    fn from(error: AuthorityBotInputError) -> Self {
        Self::AuthorityBotInput(error)
    }
}

impl fmt::Display for LiveSimulationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "live simulation driver failed: {self:?}")
    }
}

impl Error for LiveSimulationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Protocol(source) => Some(source),
            Self::AuthorityBotInput(source) => Some(source),
            Self::Snapshot(source) => Some(source),
            _ => None,
        }
    }
}

/// Shared production world driver for authority and client rollback.
pub struct LiveSimulationDriver {
    app: App,
    ownership: SeatOwnership,
    seat_to_fighter: [Option<FighterId>; MAX_SEATS],
    snapshots: LiveWorldSnapshotAdapter,
    authority_bot_inputs: Option<AuthorityBotInputGenerator>,
}

impl LiveSimulationDriver {
    /// Attaches the network-input boundary to an already-configured live app.
    ///
    /// The caller must have installed the canonical fixed schedule and populated
    /// match resources/entities. Headless app construction intentionally lives
    /// outside this driver.
    pub fn new(mut app: App, ownership: SeatOwnership) -> Result<Self, LiveSimulationError> {
        ownership.validate()?;
        if !app.world().contains_resource::<SimTick>() {
            return Err(LiveSimulationError::MissingResource("SimTick"));
        }
        if !app.world().contains_resource::<MatchState>() {
            return Err(LiveSimulationError::MissingResource("MatchState"));
        }

        let mut seat_to_fighter = [None; MAX_SEATS];
        for assignment in ownership.as_slice() {
            seat_to_fighter[usize::from(assignment.seat.get())] = Some(assignment.fighter);
        }

        let has_authority_bot = ownership
            .as_slice()
            .iter()
            .any(|assignment| assignment.owner == SeatOwner::AuthorityBot);
        if has_authority_bot && !app.world().contains_resource::<ActiveArena>() {
            return Err(LiveSimulationError::MissingResource("ActiveArena"));
        }
        if has_authority_bot && !app.world().contains_resource::<ArenaHazardState>() {
            return Err(LiveSimulationError::MissingResource("ArenaHazardState"));
        }
        let authority_bot_inputs =
            has_authority_bot.then(|| AuthorityBotInputGenerator::new(app.world_mut()));

        app.init_resource::<StagedCommittedInputs>()
            .init_resource::<ExternallyStagedBotInput>()
            .add_systems(
                FixedUpdate,
                apply_staged_committed_inputs
                    .in_set(SimulationSet::Input)
                    .after(crate::bot::bot_input)
                    .before(crate::fighter::apply_drunk_input_modifier),
            );

        Ok(Self {
            app,
            ownership,
            seat_to_fighter,
            snapshots: LiveWorldSnapshotAdapter::new(),
            authority_bot_inputs,
        })
    }

    pub fn app(&self) -> &App {
        &self.app
    }

    pub fn world(&self) -> &World {
        self.app.world()
    }

    /// Mutable access for explicit composition boundaries such as a predicted
    /// client bootstrap. Normal authority stepping should use [`Self::step_committed`].
    pub fn world_mut(&mut self) -> &mut World {
        self.app.world_mut()
    }

    pub fn ownership(&self) -> SeatOwnership {
        self.ownership
    }

    pub fn current_sim_tick(&self) -> SimTick {
        *self
            .world()
            .get_resource::<SimTick>()
            .expect("LiveSimulationDriver construction requires SimTick")
    }

    pub fn capture_live_snapshot(&self) -> Result<CanonicalSnapshot, LiveSimulationError> {
        self.capture_live_snapshot_reusing(None)
    }

    pub fn capture_live_snapshot_reusing(
        &self,
        reusable: Option<CanonicalSnapshot>,
    ) -> Result<CanonicalSnapshot, LiveSimulationError> {
        self.snapshots
            .capture_reusing(self.world(), reusable)
            .map_err(Into::into)
    }

    pub fn restore_live_snapshot(
        &mut self,
        snapshot: &CanonicalSnapshot,
    ) -> Result<EcsSnapshotRestoreReport, LiveSimulationError> {
        self.world_mut()
            .resource_mut::<StagedCommittedInputs>()
            .clear();
        let report = self
            .snapshots
            .restore(self.app.world_mut(), snapshot)
            .map_err(LiveSimulationError::Snapshot)?;
        discard_live_presentation_after(self.app.world_mut(), snapshot.header.tick);
        let found = self.current_sim_tick();
        if found != snapshot.header.tick {
            return Err(LiveSimulationError::ScheduleTickMismatch {
                expected: snapshot.header.tick,
                found,
            });
        }
        Ok(report)
    }

    pub fn state_hash(&self) -> Result<u64, LiveSimulationError> {
        self.capture_live_snapshot()?
            .canonical_hash()
            .map_err(EcsSnapshotError::Snapshot)
            .map_err(LiveSimulationError::Snapshot)
    }

    /// Returns the stable, non-zero identifier for a completed canonical result.
    ///
    /// The identity intentionally excludes the current snapshot tick and phase
    /// timer. A result packet can therefore be resent over an unreliable channel
    /// without acquiring a new semantic ID while the results phase advances.
    pub fn final_result_id(&self) -> Result<Option<u64>, LiveSimulationError> {
        let phase = self
            .world()
            .get_resource::<MatchState>()
            .ok_or(LiveSimulationError::MissingResource("MatchState"))?
            .phase;
        if !matches!(phase, MatchPhase::Results | MatchPhase::Resetting) {
            return Ok(None);
        }
        let snapshot = self.capture_live_snapshot()?;
        let Some(result) = authority_result_from_snapshot(&snapshot) else {
            return Ok(None);
        };

        let mut hash = CanonicalHash64::new();
        hash.write_str("afc-authority-result-v1")
            .write_u32(snapshot.header.protocol_version)
            .write_u32(snapshot.header.simulation_version)
            .write_u64(snapshot.header.gameplay_content_hash)
            .write_bytes(&snapshot.header.match_id)
            .write_u64(snapshot.header.master_seed);
        match result {
            AuthorityMatchResult::Draw => {
                hash.write_u8(0);
            }
            AuthorityMatchResult::FighterWinner(fighter) => {
                hash.write_u8(1).write_fighter_id(fighter);
            }
            AuthorityMatchResult::TeamWinner(team) => {
                hash.write_u8(2).write_u8(team);
            }
            AuthorityMatchResult::Aborted(reason) => {
                hash.write_u8(3).write_u16(reason);
            }
        }
        let result_id = hash.finish();
        // The wire protocol reserves zero. This fixed remap is deterministic and
        // only affects the single preimage whose 64-bit digest is zero.
        Ok(Some(if result_id == 0 {
            0x4146_4352_4553_554c
        } else {
            result_id
        }))
    }

    pub fn step_committed(
        &mut self,
        inputs: &CommittedTickInputs,
    ) -> Result<(), LiveSimulationError> {
        let expected_tick = self.current_sim_tick().next();
        if inputs.tick != expected_tick {
            return Err(LiveSimulationError::TickGap {
                expected: expected_tick,
                found: inputs.tick,
            });
        }
        let prepared = self.prepare_committed_inputs(inputs)?;
        self.world_mut()
            .resource_mut::<StagedCommittedInputs>()
            .stage(inputs.tick, prepared);

        // This is deliberately the only schedule advance in the driver.
        self.app.world_mut().run_schedule(FixedUpdate);

        let found_tick = self.current_sim_tick();
        let (observed_tick, expected_mask, applied_mask) = {
            let mut staged = self.app.world_mut().resource_mut::<StagedCommittedInputs>();
            let status = (
                staged.observed_tick,
                staged.expected_mask,
                staged.applied_mask,
            );
            staged.clear();
            status
        };
        // `run_schedule` is the standalone-ECS path and, unlike `App::update`,
        // does not retire change/removal trackers. Flush them once the complete
        // canonical tick has observed them so despawn-heavy matches reuse
        // Bevy's removal buffers instead of growing them for the match lifetime.
        self.app.world_mut().clear_trackers();
        if found_tick != inputs.tick {
            return Err(LiveSimulationError::ScheduleTickMismatch {
                expected: inputs.tick,
                found: found_tick,
            });
        }
        if observed_tick != Some(inputs.tick) || applied_mask != expected_mask {
            return Err(LiveSimulationError::InputApplicationMismatch {
                tick: inputs.tick,
                observed_tick,
                expected_mask,
                applied_mask,
            });
        }
        Ok(())
    }

    fn prepare_committed_inputs(
        &self,
        inputs: &CommittedTickInputs,
    ) -> Result<PreparedInputs, LiveSimulationError> {
        let world = self.world();
        let match_state = world
            .get_resource::<MatchState>()
            .ok_or(LiveSimulationError::MissingResource("MatchState"))?;
        validate_fighter_entities(world)?;

        let mut by_fighter = [None; MAX_SEATS];
        let mut input_mask = 0_u8;
        for (seat_index, record) in inputs.by_seat.iter().enumerate() {
            let Some(record) = record else {
                continue;
            };
            let array_seat = SeatId::new(seat_index as u8)
                .expect("the fixed input array contains only valid seats");
            record
                .frame
                .validate()
                .map_err(|source| LiveSimulationError::InvalidInput {
                    seat: array_seat,
                    source,
                })?;
            if record.frame.tick != inputs.tick {
                return Err(LiveSimulationError::InputFrameTickMismatch {
                    seat: array_seat,
                    expected: inputs.tick,
                    found: record.frame.tick,
                });
            }
            if record.frame.seat != array_seat {
                return Err(LiveSimulationError::InputFrameSeatMismatch {
                    array_seat,
                    frame_seat: record.frame.seat,
                });
            }
            if record.status != AuthorityInputStatus::Committed {
                return Err(LiveSimulationError::UncommittedInput(array_seat));
            }
            if !match_state.active_slots[record.fighter.index()] {
                return Err(LiveSimulationError::InactiveSlotInput(record.fighter));
            }
            let fighter_bit = 1 << record.fighter.get();
            if input_mask & fighter_bit != 0 {
                return Err(LiveSimulationError::DuplicateFighterInput(record.fighter));
            }
            input_mask |= fighter_bit;

            let expected_fighter = self.seat_to_fighter[seat_index]
                .ok_or(LiveSimulationError::UnexpectedSeatInput(array_seat))?;
            if record.fighter != expected_fighter {
                return Err(LiveSimulationError::InputFighterMismatch {
                    seat: array_seat,
                    expected: expected_fighter,
                    found: record.fighter,
                });
            }
            by_fighter[record.fighter.index()] = Some(record.frame);
        }

        let mut expected_mask = 0_u8;
        for fighter in FighterId::ALL {
            if !match_state.active_slots[fighter.index()] {
                continue;
            }
            expected_mask |= 1 << fighter.get();
            if by_fighter[fighter.index()].is_none() {
                return Err(LiveSimulationError::MissingFighterInput(fighter));
            }
        }
        Ok(PreparedInputs {
            by_fighter,
            expected_mask,
        })
    }

    fn committed_from_rollback_frames(
        &self,
        tick: SimTick,
        frames: &[InputFrame; MAX_SEATS],
    ) -> Result<CommittedTickInputs, LiveSimulationError> {
        let mut committed = CommittedTickInputs {
            tick,
            by_seat: [None; MAX_SEATS],
        };
        for (seat_index, frame) in frames.iter().copied().enumerate() {
            let seat = SeatId::new(seat_index as u8)
                .expect("the fixed rollback input array contains only valid seats");
            frame
                .validate()
                .map_err(|source| LiveSimulationError::InvalidInput { seat, source })?;
            if frame.tick != tick {
                return Err(LiveSimulationError::InputFrameTickMismatch {
                    seat,
                    expected: tick,
                    found: frame.tick,
                });
            }
            if frame.seat != seat {
                return Err(LiveSimulationError::InputFrameSeatMismatch {
                    array_seat: seat,
                    frame_seat: frame.seat,
                });
            }
            let Some(fighter) = self.seat_to_fighter[seat_index] else {
                continue;
            };
            committed.by_seat[seat_index] = Some(AuthorityInputRecord {
                frame,
                fighter,
                // Rollback already resolved known/predicted frame selection.
                // Origin is not gameplay state; use the non-substituted local
                // authority marker so diagnostics do not count it as a miss.
                origin: AuthorityInputOrigin::AuthorityBot,
                status: AuthorityInputStatus::Committed,
            });
        }
        Ok(committed)
    }

    fn committed_from_replay_inputs(
        &self,
        inputs: &ReplayTickInputs,
    ) -> Result<CommittedTickInputs, LiveSimulationError> {
        let mut committed = CommittedTickInputs {
            tick: inputs.tick,
            by_seat: [None; MAX_SEATS],
        };
        for (index, accepted) in inputs.fighters.iter().copied().enumerate() {
            let expected_fighter = FighterId::from_index(index)
                .expect("the replay input array has exactly four fighter slots");
            if accepted.fighter != expected_fighter {
                return Err(LiveSimulationError::ReplayFighterArrayMismatch {
                    index,
                    found: accepted.fighter,
                });
            }
            if matches!(accepted.source, ReplayInputSource::Inactive) {
                continue;
            }

            let seat = accepted.frame.seat;
            let seat_index = usize::from(seat.get());
            if committed.by_seat[seat_index].is_some() {
                return Err(LiveSimulationError::DuplicateReplaySeat(seat));
            }
            let expected = self.seat_to_fighter[seat_index]
                .ok_or(LiveSimulationError::UnexpectedSeatInput(seat))?;
            if expected != accepted.fighter {
                return Err(LiveSimulationError::InputFighterMismatch {
                    seat,
                    expected,
                    found: accepted.fighter,
                });
            }
            let origin = match accepted.source {
                ReplayInputSource::Inactive => unreachable!("inactive inputs were skipped"),
                ReplayInputSource::Peer => AuthorityInputOrigin::Peer(
                    PeerId::new(u64::from(seat.get()) + 1)
                        .expect("seat-derived replay diagnostic peer IDs are non-zero"),
                ),
                ReplayInputSource::AuthorityBot => AuthorityInputOrigin::AuthorityBot,
                ReplayInputSource::AuthoritySubstitution => AuthorityInputOrigin::MissingSubstitute,
            };
            committed.by_seat[seat_index] = Some(AuthorityInputRecord {
                frame: accepted.frame,
                fighter: accepted.fighter,
                origin,
                status: AuthorityInputStatus::Committed,
            });
        }
        Ok(committed)
    }
}

/// Removes speculative event and render-intent work after a restored tick.
///
/// These resources are optional because a dedicated authority intentionally
/// owns only the canonical event journal, while a predicted client may install
/// additional lightweight intent journals. Presented-ID history is preserved
/// by `PresentationEventRouter`: replaying a corrected tick must not replay an
/// already consumed one-shot.
fn discard_live_presentation_after(world: &mut World, retained_through: SimTick) {
    if let Some(mut journal) = world.get_resource_mut::<crate::sim_event::SimEventJournal>() {
        journal.discard_after(retained_through);
    }
    if let Some(mut cursor) = world.get_resource_mut::<crate::sim_event::PresentationEventCursor>()
    {
        crate::rollback::RollbackEventDiscard::discard_after(&mut *cursor, retained_through);
    }
    if let Some(mut router) = world.get_resource_mut::<crate::sim_event::PresentationEventRouter>()
    {
        crate::rollback::RollbackEventDiscard::discard_after(&mut *router, retained_through);
    }
    if let Some(mut intents) =
        world.get_resource_mut::<crate::combat::CombatPresentationIntentJournal>()
    {
        intents.discard_after(retained_through);
    }
    if let Some(mut intents) =
        world.get_resource_mut::<crate::fighter::FighterPresentationIntentJournal>()
    {
        intents.discard_after(retained_through);
    }
    if let Some(mut intents) =
        world.get_resource_mut::<crate::items::ItemPresentationIntentJournal>()
    {
        intents.discard_after(retained_through);
    }
    if let Some(mut intents) =
        world.get_resource_mut::<crate::arena::ArenaPresentationIntentJournal>()
    {
        intents.discard_after(retained_through);
    }
    if let Some(mut intents) =
        world.get_resource_mut::<crate::specials::SpecialPresentationIntentJournal>()
    {
        intents.discard_after(retained_through);
    }
    if let Some(mut intents) =
        world.get_resource_mut::<crate::bee_skills::BeePresentationIntentJournal>()
    {
        intents.discard_after(retained_through);
    }
    if let Some(mut intents) =
        world.get_resource_mut::<crate::chick_skills::ChickPresentationIntentJournal>()
    {
        intents.discard_after(retained_through);
    }
    if let Some(mut intents) =
        world.get_resource_mut::<crate::penguin_skills::PenguinPresentationIntentJournal>()
    {
        intents.discard_after(retained_through);
    }
}

fn validate_fighter_entities(world: &World) -> Result<[Entity; MAX_SEATS], LiveSimulationError> {
    let mut fighters = [None; MAX_SEATS];
    for archetype in world.archetypes().iter() {
        for entry in archetype.entities() {
            let entity = entry.id();
            let Some(fighter) = world.get::<Fighter>(entity) else {
                continue;
            };
            let Some(id) = FighterId::from_index(fighter.id) else {
                return Err(LiveSimulationError::InvalidFighterEntityId {
                    entity,
                    found: fighter.id,
                });
            };
            if let Some(first) = fighters[id.index()].replace(entity) {
                return Err(LiveSimulationError::DuplicateFighterEntity {
                    fighter: id,
                    first,
                    duplicate: entity,
                });
            }
            if world.get::<FighterInput>(entity).is_none() {
                return Err(LiveSimulationError::MissingFighterInputComponent {
                    fighter: id,
                    entity,
                });
            }
        }
    }
    for fighter in FighterId::ALL {
        if fighters[fighter.index()].is_none() {
            return Err(LiveSimulationError::MissingFighterEntity(fighter));
        }
    }
    Ok(fighters.map(|entity| entity.expect("every fixed fighter slot was validated")))
}

impl AuthoritySimulation for LiveSimulationDriver {
    type Snapshot = CanonicalSnapshot;
    type Error = LiveSimulationError;

    fn current_tick(&self) -> SimTick {
        self.current_sim_tick()
    }

    fn step(&mut self, inputs: &CommittedTickInputs) -> Result<(), Self::Error> {
        self.step_committed(inputs)
    }

    fn capture_snapshot(&self) -> Result<Self::Snapshot, Self::Error> {
        self.capture_live_snapshot()
    }

    fn capture_snapshot_reusing(
        &self,
        reusable: Option<Self::Snapshot>,
    ) -> Result<Self::Snapshot, Self::Error> {
        self.capture_live_snapshot_reusing(reusable)
    }

    fn generate_authority_bot_frames(
        &mut self,
        tick: SimTick,
    ) -> Result<Option<[Option<InputFrame>; MAX_SEATS]>, Self::Error> {
        let Self {
            app,
            ownership,
            authority_bot_inputs,
            ..
        } = self;
        let Some(generator) = authority_bot_inputs.as_mut() else {
            return Ok(None);
        };
        generator
            .generate(app.world_mut(), *ownership, tick)
            .map(Some)
            .map_err(Into::into)
    }

    fn final_result_id(&self) -> Option<u64> {
        // AuthorityMatch has just captured and validated this same immutable
        // completed tick, so this projection cannot newly fail in between calls.
        LiveSimulationDriver::final_result_id(self).ok().flatten()
    }
}

impl HeadlessReplayTarget for LiveSimulationDriver {
    type Error = LiveSimulationError;

    fn restore_snapshot(&mut self, snapshot: &CanonicalSnapshot) -> Result<(), Self::Error> {
        self.restore_live_snapshot(snapshot).map(|_| ())
    }

    fn step(&mut self, inputs: &ReplayTickInputs) -> Result<(), Self::Error> {
        let committed = self.committed_from_replay_inputs(inputs)?;
        self.step_committed(&committed)
    }

    fn state_hash(&self) -> Result<crate::network_protocol::StateHash, Self::Error> {
        LiveSimulationDriver::state_hash(self).map(crate::network_protocol::StateHash)
    }

    fn final_result(&self) -> Result<Option<AuthorityMatchResult>, Self::Error> {
        let snapshot = self.capture_live_snapshot()?;
        Ok(authority_result_from_snapshot(&snapshot))
    }
}

impl RollbackWorld for LiveSimulationDriver {
    type Snapshot = CanonicalSnapshot;
    type Error = LiveSimulationError;

    fn current_tick(&self) -> SimTick {
        self.current_sim_tick()
    }

    fn capture_snapshot(&self) -> Result<Self::Snapshot, Self::Error> {
        self.capture_live_snapshot()
    }

    fn capture_snapshot_reusing(
        &self,
        reusable: Option<Self::Snapshot>,
    ) -> Result<Self::Snapshot, Self::Error> {
        self.capture_live_snapshot_reusing(reusable)
    }

    fn restore_snapshot(&mut self, snapshot: &Self::Snapshot) -> Result<(), Self::Error> {
        self.restore_live_snapshot(snapshot).map(|_| ())
    }

    fn step(&mut self, tick: SimTick, inputs: &[InputFrame; MAX_SEATS]) -> Result<(), Self::Error> {
        let committed = self.committed_from_rollback_frames(tick, inputs)?;
        self.step_committed(&committed)
    }

    fn state_hash(&self) -> Result<u64, Self::Error> {
        LiveSimulationDriver::state_hash(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::arena::{
        ArenaHazardState, ArenaImpactAccent, ArenaPipeState, ArenaPresentationIntent,
        ArenaPresentationIntentJournal, PowderKegCannonState,
    };
    use crate::arena_defs::ActiveArena;
    use crate::authority::AuthorityMatch;
    use crate::authority_input::AuthorityInputConfig;
    use crate::bee_skills::{
        BeePresentationIntent, BeePresentationIntentJournal, BeePresentationKind,
    };
    use crate::characters::{CHARACTER_KINDS, CharacterKind, FighterCharacter};
    use crate::chick_skills::{
        ChickPresentationIntent, ChickPresentationIntentJournal, ChickPresentationKind,
    };
    use crate::combat::{
        CombatPresentationCueIntent, CombatPresentationCueKind, CombatPresentationIntent,
        CombatPresentationIntentJournal, ImpactOutcome, ImpactPresentation, ImpactSource,
        ImpactVisualPresentation,
    };
    use crate::combat_sfx::CombatSfxKind;
    use crate::components::{
        BotBehaviorMode, BotBrain, BotMovementPlan, DrunkStatus, FighterAction, FighterActionState,
        FighterGrabState, FighterInventory, FighterMotor, FighterSpecialState, FighterStats,
        FighterUltimateState, LocalInputAssignment, ParticipantKind, SimPosition,
    };
    use crate::determinism::{SimEntityId, SimEntityKind};
    use crate::ecs_identity::{SIM_ENTITY_POOL_CAPACITIES, SimulationIdentityAllocator};
    use crate::effects::HitImpactEffectId;
    use crate::equipment::{EquipmentKind, FighterEquipment};
    use crate::fighter::{
        FighterPresentationIntent, FighterPresentationIntentJournal, FighterPresentationKind,
    };
    use crate::game_state::{Hitstop, LocalSetup, MatchPhase, MatchTelemetry, TeamId};
    use crate::headless::{
        HeadlessMatchConfig, build_headless_simulation, snapshot_contract_for_manifest,
    };
    use crate::items::{
        ItemKind, ItemPresentationIntent, ItemPresentationIntentJournal, ItemPresentationKind,
    };
    use crate::network_protocol::{
        AuthorityKind, BuildId, CompatibilityId, DefinitionId, FighterSlotConfig,
        GameplayContentHash, InputButtons, InputSequence, MAX_FIGHTERS, MAX_NORMAL_ROLLBACK_TICKS,
        MIN_SNAPSHOT_HISTORY_TICKS, ManifestHash, MatchId, MatchManifest, ProtocolVersion,
        QuantizedAxis, ReplayFormatVersion, SIMULATION_HZ, SeatAssignment, SeatOwner,
        SimulationVersion, TeamId as ProtocolTeamId,
    };
    use crate::penguin_skills::{
        PenguinPresentationIntent, PenguinPresentationIntentJournal, PenguinPresentationKind,
    };
    use crate::reactions::ReactionFamilyId;
    use crate::replay::{AcceptedFighterInput, ReplayInputSource};
    use crate::sim_event::{AbilityLifecycleEvent, SimEventId, SimEventSource};
    use crate::simulation::{ElapsedTicks, SimTick, TickTimer, advance_sim_tick};
    use crate::snapshot_ecs::SnapshotContract;
    use crate::specials::{
        SpecialPresentationIntent, SpecialPresentationIntentJournal, SpecialPresentationKind,
    };
    use crate::styles::{FighterStyle, FighterStyleKind};
    use crate::techniques::DamageElement;

    const SEED: u64 = 0xAFC0_5566_7788_9900;
    const NAMES: [&str; MAX_SEATS] = ["cat", "pig", "fox", "panda"];

    #[derive(Resource, Default)]
    struct FixedScheduleRuns(u32);

    fn count_fixed_schedule(mut runs: ResMut<FixedScheduleRuns>) {
        runs.0 += 1;
    }

    fn ownership() -> SeatOwnership {
        SeatOwnership::from_assignments(&[
            SeatAssignment {
                seat: SeatId::new(0).unwrap(),
                fighter: FighterId::new(0).unwrap(),
                owner: SeatOwner::AuthorityBot,
            },
            SeatAssignment {
                seat: SeatId::new(1).unwrap(),
                fighter: FighterId::new(1).unwrap(),
                owner: SeatOwner::AuthorityBot,
            },
        ])
        .unwrap()
    }

    fn headless_bot_config() -> HeadlessMatchConfig {
        let ownership = ownership();
        let mut local_setup = LocalSetup::default();
        local_setup.replay_seed = SEED;
        local_setup.slots[0].participant = ParticipantKind::Bot;
        local_setup.slots[0].input = LocalInputAssignment::Unassigned;

        let mut slots = [FighterSlotConfig::default(); MAX_FIGHTERS];
        for fighter in FighterId::ALL.into_iter().take(2) {
            let setup = &local_setup.slots[fighter.index()];
            let character = CHARACTER_KINDS
                .iter()
                .position(|kind| *kind == setup.character)
                .expect("fixture characters are in the canonical catalog")
                as u16;
            let style = match setup.style {
                FighterStyleKind::Anchor => 0,
                FighterStyleKind::Vector => 1,
                FighterStyleKind::Catalyst => 2,
            };
            let equipment = match setup.equipment {
                EquipmentKind::DashCoil => 0,
                EquipmentKind::AerialSpur => 1,
                EquipmentKind::CounterCell => 2,
                EquipmentKind::HeavySeal => 3,
            };
            let team = match setup.team {
                TeamId::Red => 0,
                TeamId::Blue => 1,
            };
            slots[fighter.index()] = FighterSlotConfig {
                occupied: true,
                fighter,
                team: ProtocolTeamId::new(team).unwrap(),
                character: DefinitionId::new(character).unwrap(),
                style: DefinitionId::new(style).unwrap(),
                equipment: DefinitionId::new(equipment).unwrap(),
            };
        }

        let manifest = MatchManifest {
            compatibility: CompatibilityId {
                protocol: ProtocolVersion::new(1).unwrap(),
                simulation: SimulationVersion::new(2).unwrap(),
                replay: ReplayFormatVersion::new(1).unwrap(),
                build: BuildId::new([0xB2; 16]).unwrap(),
                gameplay_content: GameplayContentHash::new([0xD7; 32]).unwrap(),
            },
            manifest_hash: ManifestHash(0xAFC0_7711),
            match_id: MatchId::new(*b"bot-live-driver!").unwrap(),
            authority: AuthorityKind::Dedicated,
            trusted_results: true,
            arena: DefinitionId::new(local_setup.arena_index as u16).unwrap(),
            rules: DefinitionId::new(local_setup.rule_index as u16).unwrap(),
            slots,
            ownership,
            master_gameplay_seed: SEED,
            rng_scheme_version: 1,
            tick_rate_hz: SIMULATION_HZ,
            input_delay_ticks: 2,
            rollback_limit_ticks: MAX_NORMAL_ROLLBACK_TICKS,
            snapshot_history_ticks: MIN_SNAPSHOT_HISTORY_TICKS,
            agreed_start_tick: SimTick(120),
        };
        HeadlessMatchConfig {
            snapshot_contract: snapshot_contract_for_manifest(&manifest),
            manifest,
            local_setup,
        }
    }

    fn fixture_app(order: [usize; MAX_SEATS], advance_tick: bool) -> App {
        let mut app = App::new();
        app.configure_sets(
            FixedUpdate,
            (
                SimulationSet::TickStart,
                SimulationSet::Match,
                SimulationSet::Input,
                SimulationSet::Action,
                SimulationSet::Movement,
                SimulationSet::Combat,
                SimulationSet::Items,
                SimulationSet::Respawn,
                SimulationSet::TickEnd,
            )
                .chain(),
        );
        if advance_tick {
            app.add_systems(
                FixedUpdate,
                advance_sim_tick.in_set(SimulationSet::TickStart),
            );
        }
        app.init_resource::<FixedScheduleRuns>().add_systems(
            FixedUpdate,
            count_fixed_schedule.in_set(SimulationSet::Action),
        );
        app.add_systems(
            FixedUpdate,
            crate::canonical_state::canonicalize_authoritative_state.in_set(SimulationSet::TickEnd),
        );

        let active_arena = ActiveArena::new(3);
        let mut match_state = MatchState::default();
        match_state.phase = MatchPhase::Fighting;
        match_state.set_active_slots([true, true, false, false]);
        match_state.arena_index = active_arena.index();
        match_state.replay_seed = SEED;
        app.insert_resource(SimTick::ZERO)
            .insert_resource(SnapshotContract {
                simulation_version: 2,
                protocol_version: 1,
                gameplay_content_hash: 0x1122_3344_5566_7788,
                match_id: *b"live-driver-test",
                master_seed: SEED,
                pool_capacities: SIM_ENTITY_POOL_CAPACITIES,
            })
            .insert_resource(active_arena)
            .insert_resource(match_state)
            .insert_resource(MatchTelemetry {
                replay_seed: SEED,
                ..MatchTelemetry::default()
            })
            .insert_resource(Hitstop::default())
            .insert_resource(ArenaHazardState::new(
                active_arena.index(),
                active_arena.definition().hazards.len(),
            ))
            .insert_resource(ArenaPipeState::new(active_arena.index()))
            .insert_resource(PowderKegCannonState::new(active_arena.index()))
            .insert_resource(SimulationIdentityAllocator::default());

        for index in order {
            app.world_mut().spawn((
                Fighter {
                    id: index,
                    name: NAMES[index],
                    color: Color::WHITE,
                    spawn: Vec3::new(index as f32, 0.0, 0.0),
                },
                SimPosition::new(Vec3::new(index as f32, 0.0, 0.0)),
                FighterInput::default(),
                FighterStats::default(),
                FighterMotor::default(),
                FighterActionState::default(),
                DrunkStatus::default(),
                FighterInventory::default(),
                FighterGrabState::default(),
                FighterUltimateState::default(),
                FighterSpecialState::default(),
                FighterCharacter::new(CharacterKind::Cat),
                FighterStyle {
                    kind: FighterStyleKind::Anchor,
                },
                FighterEquipment {
                    kind: EquipmentKind::DashCoil,
                    cooldown: crate::simulation::TickTimer::ZERO,
                },
            ));
        }
        app
    }

    fn driver(order: [usize; MAX_SEATS]) -> LiveSimulationDriver {
        LiveSimulationDriver::new(fixture_app(order, true), ownership()).unwrap()
    }

    fn input_frame(tick: u64, seat: u8, movement: i8, buttons: u16) -> InputFrame {
        InputFrame {
            tick: SimTick(tick),
            seat: SeatId::new(seat).unwrap(),
            movement_x: QuantizedAxis::new(movement).unwrap(),
            movement_y: QuantizedAxis::default(),
            held_buttons: InputButtons::new(buttons).unwrap(),
            pressed_buttons: InputButtons::new(buttons).unwrap(),
            released_buttons: InputButtons::default(),
            sequence: InputSequence(tick as u16),
        }
    }

    fn record(tick: u64, seat: u8, fighter: u8, movement: i8) -> AuthorityInputRecord {
        AuthorityInputRecord {
            frame: input_frame(tick, seat, movement, InputButtons::LIGHT),
            fighter: FighterId::new(fighter).unwrap(),
            origin: AuthorityInputOrigin::AuthorityBot,
            status: AuthorityInputStatus::Committed,
        }
    }

    fn committed(tick: u64) -> CommittedTickInputs {
        let mut inputs = CommittedTickInputs {
            tick: SimTick(tick),
            by_seat: [None; MAX_SEATS],
        };
        inputs.by_seat[0] = Some(record(tick, 0, 0, 96));
        inputs.by_seat[1] = Some(record(tick, 1, 1, -64));
        inputs
    }

    fn fighter_input(world: &World, fighter_id: usize) -> &FighterInput {
        let entity = fighter_entity(world, fighter_id);
        world.get::<FighterInput>(entity).unwrap()
    }

    fn fighter_entity(world: &World, fighter_id: usize) -> Entity {
        for archetype in world.archetypes().iter() {
            for entry in archetype.entities() {
                let entity = entry.id();
                if world
                    .get::<Fighter>(entity)
                    .is_some_and(|fighter| fighter.id == fighter_id)
                {
                    return entity;
                }
            }
        }
        panic!("fixture fighter {fighter_id} is missing")
    }

    fn bot_brain_state(
        world: &World,
        fighter_id: usize,
    ) -> (
        BotBehaviorMode,
        TickTimer,
        TickTimer,
        TickTimer,
        TickTimer,
        u32,
        BotMovementPlan,
    ) {
        let brain = world
            .get::<BotBrain>(fighter_entity(world, fighter_id))
            .expect("authority bot has a brain");
        (
            brain.behavior,
            brain.decision_timer,
            brain.movement_plan_timer,
            brain.dash_timer,
            brain.attack_timer,
            brain.strafe_sign.to_bits(),
            brain.movement_plan,
        )
    }

    fn committed_bot_frames(
        ownership: SeatOwnership,
        tick: SimTick,
        frames: [Option<InputFrame>; MAX_SEATS],
    ) -> CommittedTickInputs {
        let mut committed = CommittedTickInputs {
            tick,
            by_seat: [None; MAX_SEATS],
        };
        for assignment in ownership.as_slice() {
            let seat_index = usize::from(assignment.seat.get());
            committed.by_seat[seat_index] = Some(AuthorityInputRecord {
                frame: frames[seat_index].expect("authority bot emitted its owned seat"),
                fighter: assignment.fighter,
                origin: AuthorityInputOrigin::AuthorityBot,
                status: AuthorityInputStatus::Committed,
            });
        }
        committed
    }

    fn replay_bot_inputs(ownership: SeatOwnership, tick: SimTick) -> ReplayTickInputs {
        let mut inputs = ReplayTickInputs::all_inactive(tick);
        for assignment in ownership.as_slice() {
            inputs.fighters[assignment.fighter.index()] = AcceptedFighterInput {
                fighter: assignment.fighter,
                source: ReplayInputSource::AuthorityBot,
                frame: input_frame(tick.get(), assignment.seat.get(), 0, 0),
            };
        }
        inputs
    }

    #[test]
    fn reversed_fighter_creation_order_places_inputs_identically_and_hashes_equal() {
        let mut ordered = driver([0, 1, 2, 3]);
        let mut reversed = driver([3, 2, 1, 0]);
        let inputs = committed(1);

        AuthoritySimulation::step(&mut ordered, &inputs).unwrap();
        AuthoritySimulation::step(&mut reversed, &inputs).unwrap();

        assert_eq!(ordered.current_sim_tick(), SimTick(1));
        assert_eq!(reversed.current_sim_tick(), SimTick(1));
        assert_eq!(ordered.world().resource::<FixedScheduleRuns>().0, 1);
        assert_eq!(reversed.world().resource::<FixedScheduleRuns>().0, 1);
        assert!(fighter_input(ordered.world(), 0).movement.x > 0.0);
        assert!(fighter_input(ordered.world(), 1).movement.x < 0.0);
        assert_eq!(
            fighter_input(ordered.world(), 0).movement,
            fighter_input(reversed.world(), 0).movement
        );
        assert_eq!(
            fighter_input(ordered.world(), 1).movement,
            fighter_input(reversed.world(), 1).movement
        );
        assert_eq!(
            ordered.state_hash().unwrap(),
            reversed.state_hash().unwrap()
        );
    }

    #[test]
    fn snapshot_restore_returns_to_the_exact_prior_live_tick() {
        let mut simulation = driver([2, 0, 3, 1]);
        simulation.step_committed(&committed(1)).unwrap();
        let tick_one = simulation.capture_live_snapshot().unwrap();

        let mut tick_two_inputs = committed(2);
        tick_two_inputs.by_seat[0] = Some(record(2, 0, 0, -100));
        simulation.step_committed(&tick_two_inputs).unwrap();
        assert_eq!(simulation.current_sim_tick(), SimTick(2));

        simulation.restore_live_snapshot(&tick_one).unwrap();
        assert_eq!(simulation.current_sim_tick(), SimTick(1));
        assert_eq!(simulation.capture_live_snapshot().unwrap(), tick_one);
    }

    #[test]
    fn invalid_inputs_and_fighter_rosters_fail_before_fixed_schedule_runs() {
        let mut simulation = driver([0, 1, 2, 3]);
        assert_eq!(
            simulation.step_committed(&committed(2)),
            Err(LiveSimulationError::TickGap {
                expected: SimTick(1),
                found: SimTick(2),
            })
        );

        let mut missing = committed(1);
        missing.by_seat[1] = None;
        assert_eq!(
            simulation.step_committed(&missing),
            Err(LiveSimulationError::MissingFighterInput(
                FighterId::new(1).unwrap()
            ))
        );

        let mut inactive = committed(1);
        inactive.by_seat[2] = Some(record(1, 2, 2, 1));
        assert_eq!(
            simulation.step_committed(&inactive),
            Err(LiveSimulationError::InactiveSlotInput(
                FighterId::new(2).unwrap()
            ))
        );

        let mut duplicate = committed(1);
        duplicate.by_seat[1] = Some(record(1, 1, 0, -64));
        assert_eq!(
            simulation.step_committed(&duplicate),
            Err(LiveSimulationError::DuplicateFighterInput(
                FighterId::new(0).unwrap()
            ))
        );
        assert_eq!(simulation.current_sim_tick(), SimTick::ZERO);
        assert_eq!(simulation.world().resource::<FixedScheduleRuns>().0, 0);

        simulation.world_mut().spawn((
            Fighter {
                id: 0,
                name: "duplicate",
                color: Color::WHITE,
                spawn: Vec3::ZERO,
            },
            FighterInput::default(),
        ));
        assert!(matches!(
            simulation.step_committed(&committed(1)),
            Err(LiveSimulationError::DuplicateFighterEntity {
                fighter: FighterId::ZERO,
                ..
            })
        ));
        assert_eq!(simulation.world().resource::<FixedScheduleRuns>().0, 0);

        let mut missing_fighter = driver([0, 1, 2, 3]);
        let entity = fighter_entity(missing_fighter.world(), 1);
        missing_fighter
            .world_mut()
            .entity_mut(entity)
            .remove::<Fighter>();
        assert_eq!(
            missing_fighter.step_committed(&committed(1)),
            Err(LiveSimulationError::MissingFighterEntity(
                FighterId::new(1).unwrap()
            ))
        );
        assert_eq!(missing_fighter.world().resource::<FixedScheduleRuns>().0, 0);
    }

    #[test]
    fn schedule_tick_mismatch_and_snapshot_failure_are_typed() {
        let mut no_clock =
            LiveSimulationDriver::new(fixture_app([0, 1, 2, 3], false), ownership()).unwrap();
        assert_eq!(
            no_clock.step_committed(&committed(1)),
            Err(LiveSimulationError::ScheduleTickMismatch {
                expected: SimTick(1),
                found: SimTick::ZERO,
            })
        );
        assert_eq!(no_clock.world().resource::<FixedScheduleRuns>().0, 1);

        let mut broken_snapshot = driver([0, 1, 2, 3]);
        broken_snapshot
            .world_mut()
            .remove_resource::<SnapshotContract>();
        assert!(matches!(
            broken_snapshot.capture_live_snapshot(),
            Err(LiveSimulationError::Snapshot(_))
        ));
    }

    #[test]
    fn rollback_world_uses_the_session_seat_to_fighter_mapping() {
        let mut simulation = driver([3, 2, 1, 0]);
        let frames = std::array::from_fn(|seat| {
            input_frame(1, seat as u8, if seat == 0 { 100 } else { -20 }, 0)
        });

        RollbackWorld::step(&mut simulation, SimTick(1), &frames).unwrap();
        assert_eq!(simulation.current_sim_tick(), SimTick(1));
        assert!(fighter_input(simulation.world(), 0).movement.x > 0.0);
        assert!(fighter_input(simulation.world(), 1).movement.x < 0.0);
    }

    #[test]
    fn replay_target_steps_the_same_committed_frames_as_live_authority() {
        let mut authority = driver([0, 1, 2, 3]);
        let mut replay_target = driver([3, 2, 1, 0]);
        let committed = committed(1);
        let mut replay_inputs = ReplayTickInputs::all_inactive(SimTick(1));
        replay_inputs.fighters[0] = AcceptedFighterInput {
            fighter: FighterId::new(0).unwrap(),
            source: ReplayInputSource::AuthorityBot,
            frame: committed.by_seat[0].unwrap().frame,
        };
        replay_inputs.fighters[1] = AcceptedFighterInput {
            fighter: FighterId::new(1).unwrap(),
            source: ReplayInputSource::AuthorityBot,
            frame: committed.by_seat[1].unwrap().frame,
        };

        authority.step_committed(&committed).unwrap();
        HeadlessReplayTarget::step(&mut replay_target, &replay_inputs).unwrap();

        assert_eq!(
            authority.current_sim_tick(),
            replay_target.current_sim_tick()
        );
        assert_eq!(
            authority.state_hash().unwrap(),
            replay_target.state_hash().unwrap()
        );
    }

    #[test]
    fn live_bot_generator_same_tick_retry_is_cached_and_snapshot_neutral() {
        let mut simulation = build_headless_simulation(headless_bot_config()).unwrap();
        assert!(
            simulation
                .world()
                .contains_resource::<ExternallyStagedBotInput>()
        );
        let canonical_before = simulation.capture_live_snapshot().unwrap();
        let brains_before = [
            bot_brain_state(simulation.world(), 0),
            bot_brain_state(simulation.world(), 1),
        ];

        let first = AuthoritySimulation::generate_authority_bot_frames(&mut simulation, SimTick(1))
            .unwrap()
            .unwrap();
        let brains_after_first = [
            bot_brain_state(simulation.world(), 0),
            bot_brain_state(simulation.world(), 1),
        ];
        assert_ne!(brains_before, brains_after_first);
        assert_eq!(
            simulation.capture_live_snapshot().unwrap(),
            canonical_before,
            "authority AI may not write canonical input before commitment"
        );

        let retry = AuthoritySimulation::generate_authority_bot_frames(&mut simulation, SimTick(1))
            .unwrap()
            .unwrap();
        assert_eq!(retry, first);
        assert_eq!(
            [
                bot_brain_state(simulation.world(), 0),
                bot_brain_state(simulation.world(), 1),
            ],
            brains_after_first,
            "a retried authority deadline must not advance AI twice"
        );
    }

    #[test]
    fn independent_headless_authorities_emit_identical_bot_tapes_and_hashes() {
        let config = headless_bot_config();
        let first_driver = build_headless_simulation(config.clone()).unwrap();
        let second_driver = build_headless_simulation(config.clone()).unwrap();
        let mut first = AuthorityMatch::new(
            config.manifest,
            first_driver,
            AuthorityInputConfig::default(),
        )
        .unwrap();
        let mut second = AuthorityMatch::new(
            config.manifest,
            second_driver,
            AuthorityInputConfig::default(),
        )
        .unwrap();

        for expected_tick in 1..=48 {
            let first_report = first.step().unwrap();
            let second_report = second.step().unwrap();
            assert_eq!(first_report, second_report);
            assert_eq!(first_report.tick, SimTick(expected_tick));
            assert_eq!(first_report.committed_inputs.len(), 2);
            assert!(first_report.committed_inputs.iter().all(|record| {
                record.origin == AuthorityInputOrigin::AuthorityBot
                    && record.status == AuthorityInputStatus::Committed
                    && record.frame.sequence == InputSequence(expected_tick as u16)
            }));
        }
    }

    #[test]
    fn authority_bot_hitstop_repeats_continuous_state_and_clears_edges() {
        let config = headless_bot_config();
        let ownership = config.manifest.ownership;
        let mut simulation = build_headless_simulation(config).unwrap();
        {
            let fighter = fighter_entity(simulation.world(), 0);
            let mut action = simulation
                .world_mut()
                .get_mut::<FighterActionState>(fighter)
                .unwrap();
            action.action = FighterAction::LightAttack1;
            action.elapsed = ElapsedTicks::from_ticks(10);
        }

        let tick_one =
            AuthoritySimulation::generate_authority_bot_frames(&mut simulation, SimTick(1))
                .unwrap()
                .unwrap();
        let first = tick_one[0].unwrap();
        assert_ne!(first.pressed_buttons.bits() & InputButtons::LIGHT, 0);
        AuthoritySimulation::step(
            &mut simulation,
            &committed_bot_frames(ownership, SimTick(1), tick_one),
        )
        .unwrap();

        simulation
            .world_mut()
            .resource_mut::<Hitstop>()
            .trigger(0.1);
        let brains_before_pause = [
            bot_brain_state(simulation.world(), 0),
            bot_brain_state(simulation.world(), 1),
        ];
        let tick_two =
            AuthoritySimulation::generate_authority_bot_frames(&mut simulation, SimTick(2))
                .unwrap()
                .unwrap();
        let repeated = tick_two[0].unwrap();

        assert_eq!(repeated.movement_x, first.movement_x);
        assert_eq!(repeated.movement_y, first.movement_y);
        assert_eq!(repeated.held_buttons, first.held_buttons);
        assert_eq!(repeated.pressed_buttons, InputButtons::default());
        assert_eq!(repeated.released_buttons, InputButtons::default());
        assert_eq!(repeated.tick, SimTick(2));
        assert_eq!(repeated.sequence, InputSequence(2));
        assert_eq!(
            [
                bot_brain_state(simulation.world(), 0),
                bot_brain_state(simulation.world(), 1),
            ],
            brains_before_pause,
            "hitstop cannot consume an authority AI decision tick"
        );
    }

    #[test]
    fn rollback_and_replay_steps_never_advance_authority_ai() {
        let config = headless_bot_config();
        let ownership = config.manifest.ownership;
        let mut rollback = build_headless_simulation(config.clone()).unwrap();
        let rollback_brains = [
            bot_brain_state(rollback.world(), 0),
            bot_brain_state(rollback.world(), 1),
        ];
        let frames = std::array::from_fn(|seat| input_frame(1, seat as u8, 0, 0));
        RollbackWorld::step(&mut rollback, SimTick(1), &frames).unwrap();
        assert_eq!(
            [
                bot_brain_state(rollback.world(), 0),
                bot_brain_state(rollback.world(), 1),
            ],
            rollback_brains
        );

        let mut replay = build_headless_simulation(config).unwrap();
        let replay_brains = [
            bot_brain_state(replay.world(), 0),
            bot_brain_state(replay.world(), 1),
        ];
        HeadlessReplayTarget::step(&mut replay, &replay_bot_inputs(ownership, SimTick(1))).unwrap();
        assert_eq!(
            [
                bot_brain_state(replay.world(), 0),
                bot_brain_state(replay.world(), 1),
            ],
            replay_brains
        );
    }

    fn presentation_event_id(tick: SimTick, source: SimEventSource) -> SimEventId {
        SimEventId {
            tick,
            source,
            ordinal: 0,
        }
    }

    fn presentation_test_impact() -> ImpactOutcome {
        ImpactOutcome {
            guarded: false,
            committed_damage: 1.0,
            resolved_reaction: Some(ReactionFamilyId::ShortStandingStagger),
            presentation: ImpactPresentation {
                position: Vec3::ZERO,
                direction: Vec3::X,
                source: ImpactSource::FighterStrike,
                feedback_cue: "rollback_discard_fixture",
                feedback_priority: 1,
                reaction: None,
                side_effect_cue: None,
                side_effect_priority: 0,
                visual: ImpactVisualPresentation::Hit {
                    element: DamageElement::Neutral,
                    heavy_spark: false,
                    spark_scale: 1.0,
                    hit_effect: HitImpactEffectId::GenericLight,
                    hit_effects_enabled: false,
                    include_skill_accent: false,
                },
                combat_sfx: CombatSfxKind::LightHit,
                combat_sfx_priority: 1,
                hud_flash: 0.0,
                reaction_visual_side: 1.0,
                camera_shake: 0.0,
            },
        }
    }

    fn seed_all_predicted_presentation_journals(world: &mut World, tick: SimTick) {
        let victim = FighterId::ZERO;
        let hitbox = SimEntityId::new(SimEntityKind::Hitbox, 0, 1);
        let combat_event = presentation_event_id(tick, SimEventSource::Entity(hitbox));
        let impact = presentation_test_impact();
        {
            let mut journal = world.resource_mut::<CombatPresentationIntentJournal>();
            journal
                .record(CombatPresentationIntent {
                    event_id: combat_event,
                    victim,
                    outcome: impact,
                })
                .unwrap();
            journal
                .record_cue(CombatPresentationCueIntent {
                    event_id: combat_event,
                    kind: CombatPresentationCueKind::AttackSurfaceDespawn { entity: hitbox },
                })
                .unwrap();
        }

        let fighter_event = presentation_event_id(tick, SimEventSource::Fighter(FighterId::ZERO));
        world
            .resource_mut::<FighterPresentationIntentJournal>()
            .record(FighterPresentationIntent {
                event_id: fighter_event,
                fighter: FighterId::ZERO,
                fighter_name: "rollback fixture",
                kind: FighterPresentationKind::RecoveryCompleted,
            })
            .unwrap();

        let item = SimEntityId::new(SimEntityKind::Item, 0, 1);
        let item_event = presentation_event_id(tick, SimEventSource::Entity(item));
        world
            .resource_mut::<ItemPresentationIntentJournal>()
            .record(ItemPresentationIntent {
                event_id: item_event,
                item,
                item_kind: ItemKind::Apple,
                fighter: None,
                fighter_name: None,
                kind: ItemPresentationKind::Broken {
                    position: Vec3::ZERO,
                },
            })
            .unwrap();

        let arena_event = presentation_event_id(tick, SimEventSource::Arena);
        world
            .resource_mut::<ArenaPresentationIntentJournal>()
            .record(ArenaPresentationIntent {
                event_id: arena_event,
                victim,
                outcome: impact,
                accent: ArenaImpactAccent::None,
            })
            .unwrap();

        let special = SimEntityId::new(SimEntityKind::Special, 0, 1);
        let special_event = presentation_event_id(tick, SimEventSource::Entity(special));
        world
            .resource_mut::<SpecialPresentationIntentJournal>()
            .record(SpecialPresentationIntent {
                event_id: special_event,
                entity: special,
                kind: SpecialPresentationKind::Lifecycle {
                    event: AbilityLifecycleEvent::Spawned,
                    position: Vec3::ZERO,
                    direction: Vec3::X,
                    package: None,
                    cue: None,
                    source: ImpactSource::Projectile,
                    priority: 1,
                },
            })
            .unwrap();

        let bee = SimEntityId::new(SimEntityKind::BeeSkill, 0, 1);
        let bee_event = presentation_event_id(tick, SimEventSource::Entity(bee));
        world
            .resource_mut::<BeePresentationIntentJournal>()
            .record(BeePresentationIntent {
                event_id: bee_event,
                entity: bee,
                kind: BeePresentationKind::Lifecycle {
                    event: AbilityLifecycleEvent::Spawned,
                    position: Vec3::ZERO,
                    direction: Vec3::X,
                    package: None,
                    cue: None,
                    source: ImpactSource::Projectile,
                    priority: 1,
                },
            })
            .unwrap();

        let chick = SimEntityId::new(SimEntityKind::ChickSkill, 0, 1);
        let chick_event = presentation_event_id(tick, SimEventSource::Entity(chick));
        world
            .resource_mut::<ChickPresentationIntentJournal>()
            .record(ChickPresentationIntent {
                event_id: chick_event,
                entity: chick,
                kind: ChickPresentationKind::Lifecycle {
                    event: AbilityLifecycleEvent::Spawned,
                    position: Vec3::ZERO,
                    direction: Vec3::X,
                    package: None,
                    cue: None,
                    source: ImpactSource::Projectile,
                    priority: 1,
                    hud_flash: None,
                },
            })
            .unwrap();

        let penguin = SimEntityId::new(SimEntityKind::PenguinSkill, 0, 1);
        let penguin_event = presentation_event_id(tick, SimEventSource::Entity(penguin));
        world
            .resource_mut::<PenguinPresentationIntentJournal>()
            .record(PenguinPresentationIntent {
                event_id: penguin_event,
                entity: penguin,
                kind: PenguinPresentationKind::Lifecycle {
                    event: AbilityLifecycleEvent::Spawned,
                    position: Vec3::ZERO,
                    direction: Vec3::X,
                    package: None,
                    cue: None,
                    source: ImpactSource::Projectile,
                    priority: 1,
                    hud_flash: None,
                },
            })
            .unwrap();
    }

    fn assert_predicted_presentation_tick(world: &World, tick: SimTick, retained: bool) {
        let hitbox = SimEntityId::new(SimEntityKind::Hitbox, 0, 1);
        let combat_event = presentation_event_id(tick, SimEventSource::Entity(hitbox));
        let fighter_event = presentation_event_id(tick, SimEventSource::Fighter(FighterId::ZERO));
        let item = SimEntityId::new(SimEntityKind::Item, 0, 1);
        let special = SimEntityId::new(SimEntityKind::Special, 0, 1);
        let bee = SimEntityId::new(SimEntityKind::BeeSkill, 0, 1);
        let chick = SimEntityId::new(SimEntityKind::ChickSkill, 0, 1);
        let penguin = SimEntityId::new(SimEntityKind::PenguinSkill, 0, 1);

        assert_eq!(
            world
                .resource::<CombatPresentationIntentJournal>()
                .get(combat_event)
                .is_some(),
            retained
        );
        assert_eq!(
            world
                .resource::<CombatPresentationIntentJournal>()
                .cue(combat_event)
                .is_some(),
            retained
        );
        assert_eq!(
            world
                .resource::<FighterPresentationIntentJournal>()
                .get(fighter_event)
                .is_some(),
            retained
        );
        assert_eq!(
            world
                .resource::<ItemPresentationIntentJournal>()
                .get(presentation_event_id(tick, SimEventSource::Entity(item)))
                .is_some(),
            retained
        );
        assert_eq!(
            world
                .resource::<ArenaPresentationIntentJournal>()
                .get(presentation_event_id(tick, SimEventSource::Arena))
                .is_some(),
            retained
        );
        assert_eq!(
            world
                .resource::<SpecialPresentationIntentJournal>()
                .get(presentation_event_id(tick, SimEventSource::Entity(special)))
                .is_some(),
            retained
        );
        assert_eq!(
            world
                .resource::<BeePresentationIntentJournal>()
                .get(presentation_event_id(tick, SimEventSource::Entity(bee)))
                .is_some(),
            retained
        );
        assert_eq!(
            world
                .resource::<ChickPresentationIntentJournal>()
                .get(presentation_event_id(tick, SimEventSource::Entity(chick)))
                .is_some(),
            retained
        );
        assert_eq!(
            world
                .resource::<PenguinPresentationIntentJournal>()
                .get(presentation_event_id(tick, SimEventSource::Entity(penguin)))
                .is_some(),
            retained
        );
    }

    #[test]
    fn live_restore_discards_future_entries_from_every_predicted_presentation_journal() {
        let retained_through = SimTick(40);
        let speculative_tick = SimTick(41);
        let mut world = World::new();
        world.init_resource::<CombatPresentationIntentJournal>();
        world.init_resource::<FighterPresentationIntentJournal>();
        world.init_resource::<ItemPresentationIntentJournal>();
        world.init_resource::<ArenaPresentationIntentJournal>();
        world.init_resource::<SpecialPresentationIntentJournal>();
        world.init_resource::<BeePresentationIntentJournal>();
        world.init_resource::<ChickPresentationIntentJournal>();
        world.init_resource::<PenguinPresentationIntentJournal>();
        seed_all_predicted_presentation_journals(&mut world, retained_through);
        seed_all_predicted_presentation_journals(&mut world, speculative_tick);

        discard_live_presentation_after(&mut world, retained_through);

        assert_predicted_presentation_tick(&world, retained_through, true);
        assert_predicted_presentation_tick(&world, speculative_tick, false);
    }

    #[test]
    fn completed_result_identity_is_nonzero_and_stable_across_result_ticks() {
        let mut simulation = driver([0, 1, 2, 3]);
        assert_eq!(simulation.final_result_id().unwrap(), None);
        assert_eq!(
            HeadlessReplayTarget::final_result(&simulation).unwrap(),
            None
        );

        {
            let mut state = simulation.world_mut().resource_mut::<MatchState>();
            state.phase = MatchPhase::Results;
            state.stocks = [1, 1, 0, 0];
        }
        let first = simulation.final_result_id().unwrap().unwrap();
        assert_ne!(first, 0);
        assert_eq!(
            HeadlessReplayTarget::final_result(&simulation).unwrap(),
            Some(AuthorityMatchResult::Draw)
        );

        *simulation.world_mut().resource_mut::<SimTick>() = SimTick(99);
        simulation
            .world_mut()
            .resource_mut::<MatchState>()
            .phase_timer_ticks = 77;
        assert_eq!(simulation.final_result_id().unwrap(), Some(first));
        assert_eq!(
            AuthoritySimulation::final_result_id(&simulation),
            Some(first)
        );
    }
}
