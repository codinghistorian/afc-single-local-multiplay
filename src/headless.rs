//! Render-free composition root for the production live simulation.
//!
//! The authority owns the same canonical fixed-step systems as the client, but
//! this module deliberately installs no Bevy plugins and no frame schedules.
//! In particular, constructing a headless simulation cannot create an asset
//! server, renderer, audio output, UI tree, or window.

use bevy::ecs::schedule::ExecutorKind;
use bevy::prelude::*;
use std::error::Error;
use std::fmt;

use crate::arena;
use crate::arena_defs::arena_definitions;
use crate::characters::{CHARACTER_KINDS, CHARACTER_MOVE_CATALOG_PATH, CharacterMoveCatalog};
use crate::components::ParticipantKind;
use crate::determinism::{CanonicalHash64, FighterId};
use crate::ecs_identity::{SIM_ENTITY_POOL_CAPACITIES, SimulationIdentityAllocator};
use crate::equipment::EquipmentKind;
use crate::feel::{COMBAT_FEEL_PATH, CombatFeelTuning};
use crate::game_state::{
    Hitstop, LocalSetup, MatchPhase, MatchState, MatchTelemetry, RULE_PRESETS, TeamId,
};
use crate::items;
use crate::live_authority::{LiveSimulationDriver, LiveSimulationError};
use crate::network_protocol::{
    GameplayContentHash, MatchManifest, ProtocolValidationError, SeatOwner,
};
use crate::sim_event::{SimEventJournal, TickEventBuffer};
use crate::simulation::{self, SimTick, SimulationSet};
use crate::snapshot_ecs::SnapshotContract;
use crate::styles::FighterStyleKind;

/// Fully agreed inputs needed to construct one canonical simulation world.
///
/// `LocalSetup` is retained as the existing gameplay bootstrap format, but it
/// is accepted only when every simulation-relevant value agrees with the wire
/// manifest. Raw local-device assignments are intentionally ignored here.
#[derive(Clone)]
pub struct HeadlessMatchConfig {
    pub manifest: MatchManifest,
    pub snapshot_contract: SnapshotContract,
    pub local_setup: LocalSetup,
}

impl HeadlessMatchConfig {
    pub fn validate(&self) -> Result<(), HeadlessBuildError> {
        self.manifest
            .validate()
            .map_err(HeadlessBuildError::Manifest)?;
        validate_snapshot_contract(&self.manifest, self.snapshot_contract)?;
        validate_local_setup(&self.manifest, &self.local_setup)
    }
}

/// Fail-closed bootstrap errors. No partially constructed driver is returned.
#[derive(Debug)]
pub enum HeadlessBuildError {
    Manifest(ProtocolValidationError),
    ContractMismatch(&'static str),
    SetupMismatch {
        field: &'static str,
        fighter: Option<FighterId>,
    },
    ItemCapacity {
        requested: usize,
        rejected: usize,
    },
    InvalidEmbeddedGameplayContent {
        asset: &'static str,
        reason: String,
    },
    LiveSimulation(LiveSimulationError),
}

impl fmt::Display for HeadlessBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEmbeddedGameplayContent { asset, reason } => write!(
                formatter,
                "headless simulation bootstrap failed: invalid embedded gameplay asset \
                 {asset}: {reason}"
            ),
            _ => write!(formatter, "headless simulation bootstrap failed: {self:?}"),
        }
    }
}

impl Error for HeadlessBuildError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Manifest(error) => Some(error),
            Self::LiveSimulation(error) => Some(error),
            _ => None,
        }
    }
}

impl From<LiveSimulationError> for HeadlessBuildError {
    fn from(error: LiveSimulationError) -> Self {
        Self::LiveSimulation(error)
    }
}

/// Stable 64-bit projection used by the current snapshot-v2 header.
///
/// The session protocol retains the full 256-bit content identity. Snapshot-v2
/// has a legacy 64-bit field, so this bridge hashes all 32 bytes rather than
/// truncating them. A future snapshot schema can carry the full value directly.
pub fn snapshot_gameplay_content_hash(content: GameplayContentHash) -> u64 {
    let mut hash = CanonicalHash64::new();
    hash.write_bytes(content.as_bytes());
    hash.finish()
}

/// Produces the exact snapshot compatibility contract implied by a manifest.
pub fn snapshot_contract_for_manifest(manifest: &MatchManifest) -> SnapshotContract {
    SnapshotContract {
        simulation_version: u32::from(manifest.compatibility.simulation.get()),
        protocol_version: u32::from(manifest.compatibility.protocol.get()),
        gameplay_content_hash: snapshot_gameplay_content_hash(
            manifest.compatibility.gameplay_content,
        ),
        match_id: *manifest.match_id.as_bytes(),
        master_seed: manifest.master_gameplay_seed,
        pool_capacities: SIM_ENTITY_POOL_CAPACITIES,
    }
}

/// Builds a production live-simulation driver with no client plugins or assets.
pub fn build_headless_simulation(
    config: HeadlessMatchConfig,
) -> Result<LiveSimulationDriver, HeadlessBuildError> {
    config.validate()?;

    let mut app = App::new();
    configure_canonical_fixed_schedule(&mut app);
    initialize_headless_resources(&mut app, &config)?;
    bootstrap_canonical_world(&mut app, &config)?;

    LiveSimulationDriver::new(app, config.manifest.ownership).map_err(Into::into)
}

/// Builds the render-free world used by a network client's prediction engine.
///
/// It runs the identical canonical schedule as a dedicated authority, but also
/// installs the bounded, renderer-facing intent journals produced alongside
/// semantic simulation events. Assets, windows, audio, cameras, and actual
/// presentation entities remain absent. A presentation projector copies these
/// intents into the rendered world after prediction/rollback has settled.
pub fn build_predicted_simulation(
    config: HeadlessMatchConfig,
) -> Result<LiveSimulationDriver, HeadlessBuildError> {
    config.validate()?;

    let mut app = App::new();
    configure_canonical_fixed_schedule(&mut app);
    initialize_headless_resources(&mut app, &config)?;
    app.init_resource::<crate::combat::CombatPresentationIntentJournal>()
        .init_resource::<crate::fighter::FighterPresentationIntentJournal>()
        .init_resource::<crate::items::ItemPresentationIntentJournal>()
        .init_resource::<crate::arena::ArenaPresentationIntentJournal>()
        .init_resource::<crate::specials::SpecialPresentationIntentJournal>()
        .init_resource::<crate::bee_skills::BeePresentationIntentJournal>()
        .init_resource::<crate::chick_skills::ChickPresentationIntentJournal>()
        .init_resource::<crate::penguin_skills::PenguinPresentationIntentJournal>();
    bootstrap_canonical_world(&mut app, &config)?;

    LiveSimulationDriver::new(app, config.manifest.ownership).map_err(Into::into)
}

fn configure_canonical_fixed_schedule(app: &mut App) {
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
    )
    .add_systems(
        FixedUpdate,
        (
            simulation::advance_sim_tick,
            crate::sim_event::begin_sim_event_tick,
            arena::sync_active_arena_from_match_state,
            crate::ecs_identity::reclaim_orphaned_sim_entities,
        )
            .chain()
            .in_set(SimulationSet::TickStart),
    )
    .add_systems(
        FixedUpdate,
        (
            crate::game_state::tick_hitstop,
            crate::game_state::tick_match_timer,
            crate::fighter::update_drunk_status,
        )
            .chain()
            .in_set(SimulationSet::Match),
    )
    .add_systems(
        FixedUpdate,
        crate::fighter::apply_drunk_input_modifier.in_set(SimulationSet::Input),
    )
    .add_systems(
        FixedUpdate,
        (
            crate::fighter::apply_aim_assist,
            items::handle_item_inputs,
            crate::specials::handle_special_inputs,
            crate::specials::tick_special_cooldowns,
            crate::equipment::tick_equipment_cooldowns,
            crate::fighter::update_fighter_state,
            crate::fighter::update_grab_holds,
            crate::fighter::update_ultimate_locks,
            crate::combat::spawn_attack_hitboxes,
            items::spawn_item_hitboxes,
        )
            .chain()
            .in_set(SimulationSet::Action)
            .run_if(crate::game_state::match_accepts_gameplay),
    )
    .add_systems(
        FixedUpdate,
        (
            crate::fighter::apply_fighter_movement,
            arena::update_arena_pipe_transits,
            crate::fighter::separate_fighters,
        )
            .chain()
            .in_set(SimulationSet::Movement)
            .run_if(crate::game_state::match_accepts_gameplay),
    )
    .add_systems(
        FixedUpdate,
        (
            crate::combat::begin_contact_collection,
            crate::combat::update_hitboxes,
            crate::combat::collect_hitbox_contacts,
            crate::specials::collect_special_contacts,
            crate::bee_skills::collect_bee_skill_contacts,
            crate::chick_skills::collect_chick_skill_contacts,
            crate::penguin_skills::collect_penguin_skill_contacts,
            crate::penguin_skills::update_penguin_surfaces,
        )
            .chain()
            .in_set(SimulationSet::Combat)
            .run_if(crate::game_state::match_accepts_gameplay),
    )
    .add_systems(
        FixedUpdate,
        (
            items::drop_items_from_disabled_fighters,
            items::update_items,
            items::advance_moving_items_and_collect_contacts,
            arena::advance_arena_hazards_and_collect_contacts,
            arena::update_crank_yard_machinery,
            arena::advance_powder_keg_cannons_and_collect_contacts,
            crate::combat::resolve_contacts,
            crate::combat::apply_hitbox_contact_outcomes,
            items::apply_item_contact_outcomes,
            crate::bee_skills::apply_bee_skill_contact_outcomes,
            crate::chick_skills::apply_chick_skill_contact_outcomes,
            crate::penguin_skills::apply_penguin_skill_contact_outcomes,
            crate::specials::apply_special_contact_outcomes,
            arena::apply_powder_keg_contact_outcomes,
            arena::apply_arena_hazard_contact_outcomes,
        )
            .chain()
            .in_set(SimulationSet::Items)
            .run_if(crate::game_state::match_accepts_gameplay),
    )
    .add_systems(
        FixedUpdate,
        crate::fighter::ringout_and_respawn
            .in_set(SimulationSet::Respawn)
            .run_if(crate::game_state::match_accepts_gameplay),
    )
    .add_systems(
        FixedUpdate,
        (
            crate::canonical_state::canonicalize_authoritative_state,
            crate::sim_event::commit_sim_event_tick,
        )
            .chain()
            .in_set(SimulationSet::TickEnd),
    );
    // Every canonical phase is explicitly chained, so the multi-threaded
    // executor cannot expose useful parallelism here. Its per-run task graph
    // bookkeeping also allocates on every rollback/authority tick.
    app.edit_schedule(FixedUpdate, |schedule| {
        schedule.set_executor_kind(ExecutorKind::SingleThreaded);
    });
}

fn initialize_headless_resources(
    app: &mut App,
    config: &HeadlessMatchConfig,
) -> Result<(), HeadlessBuildError> {
    // Parse both baked assets before mutating the world. Online authority and
    // prediction may never inherit the rendered developer sandbox's mutable
    // native loose-file defaults or file-watcher state.
    let character_moves = CharacterMoveCatalog::from_embedded_gameplay().map_err(|reason| {
        HeadlessBuildError::InvalidEmbeddedGameplayContent {
            asset: CHARACTER_MOVE_CATALOG_PATH,
            reason,
        }
    })?;
    let combat_feel = CombatFeelTuning::from_embedded_gameplay().map_err(|reason| {
        HeadlessBuildError::InvalidEmbeddedGameplayContent {
            asset: COMBAT_FEEL_PATH,
            reason,
        }
    })?;

    let setup = config.local_setup.clone();
    let mut match_state = MatchState::default();
    match_state.rule_index = setup.rule_index;
    match_state.rules = setup.active_rule();
    match_state.arena_index = setup.arena_index;
    match_state.replay_seed = config.manifest.master_gameplay_seed;
    match_state.apply_local_setup(&setup);
    match_state.reset_for_new_match();
    debug_assert_eq!(match_state.phase, MatchPhase::Fighting);

    app.insert_resource(Time::<Fixed>::from_hz(simulation::SIM_HZ))
        .insert_resource(config.snapshot_contract)
        .insert_resource(setup)
        .insert_resource(match_state)
        .insert_resource(MatchTelemetry {
            replay_seed: config.manifest.master_gameplay_seed,
            ..MatchTelemetry::default()
        })
        .init_resource::<Hitstop>()
        .insert_resource(SimTick::ZERO)
        .init_resource::<TickEventBuffer>()
        .init_resource::<SimEventJournal>()
        .init_resource::<SimulationIdentityAllocator>()
        .init_resource::<crate::contact_arbitration::ContactBuffer>()
        .init_resource::<crate::items::ItemContactFrame>()
        .init_resource::<crate::arena::ArenaOrdnanceContactFrame>()
        .insert_resource(character_moves)
        .insert_resource(combat_feel);
    Ok(())
}

fn bootstrap_canonical_world(
    app: &mut App,
    config: &HeadlessMatchConfig,
) -> Result<(), HeadlessBuildError> {
    let world = app.world_mut();
    let active_arena =
        arena::bootstrap_canonical_arena_runtime(world, config.local_setup.arena_index);
    let fighters = crate::fighter::bootstrap_canonical_fighters(
        world,
        &config.local_setup,
        active_arena.definition(),
    );
    // Fighter construction still supports a local training-dummy environment
    // switch. An authority manifest is immutable, so explicitly normalize its
    // bot slots for the separate bot-frame producer and never inherit that
    // process-local switch.
    for assignment in config.manifest.ownership.as_slice() {
        if assignment.owner != SeatOwner::AuthorityBot {
            continue;
        }
        let mut brain = world
            .get_mut::<crate::components::BotBrain>(fighters[assignment.fighter.index()])
            .expect("validated authority-bot slot has a canonical BotBrain");
        crate::bot::start_bot_combat_ai(&mut brain);
    }
    let item_report = items::reset_canonical_items_for_arena(world, active_arena.definition());
    if item_report.rejected_anchors != 0 {
        return Err(HeadlessBuildError::ItemCapacity {
            requested: active_arena.definition().item_anchors.len(),
            rejected: item_report.rejected_anchors,
        });
    }
    Ok(())
}

fn validate_snapshot_contract(
    manifest: &MatchManifest,
    contract: SnapshotContract,
) -> Result<(), HeadlessBuildError> {
    let expected = snapshot_contract_for_manifest(manifest);
    for (matches, field) in [
        (
            contract.simulation_version == expected.simulation_version,
            "simulation version",
        ),
        (
            contract.protocol_version == expected.protocol_version,
            "protocol version",
        ),
        (
            contract.gameplay_content_hash == expected.gameplay_content_hash,
            "gameplay content hash",
        ),
        (contract.match_id == expected.match_id, "match ID"),
        (contract.master_seed == expected.master_seed, "master seed"),
        (
            contract.pool_capacities == expected.pool_capacities,
            "simulation entity pool capacities",
        ),
    ] {
        if !matches {
            return Err(HeadlessBuildError::ContractMismatch(field));
        }
    }
    Ok(())
}

fn validate_local_setup(
    manifest: &MatchManifest,
    setup: &LocalSetup,
) -> Result<(), HeadlessBuildError> {
    if setup.arena_index != usize::from(manifest.arena.get())
        || setup.arena_index >= arena_definitions().len()
    {
        return Err(setup_mismatch("arena", None));
    }
    if setup.rule_index != usize::from(manifest.rules.get())
        || setup.rule_index >= RULE_PRESETS.len()
    {
        return Err(setup_mismatch("rules", None));
    }
    if setup.replay_seed != manifest.master_gameplay_seed {
        return Err(setup_mismatch("master gameplay seed", None));
    }

    for fighter in FighterId::ALL {
        let slot = &setup.slots[fighter.index()];
        let manifest_slot = manifest.slots[fighter.index()];
        let expected_participant = if !manifest_slot.occupied {
            ParticipantKind::Closed
        } else {
            match manifest
                .ownership
                .assignment_for_fighter(fighter)
                .expect("validated occupied manifest slot has an owner")
                .owner
            {
                SeatOwner::Peer(_) => ParticipantKind::Human,
                SeatOwner::AuthorityBot => ParticipantKind::Bot,
            }
        };
        if slot.participant != expected_participant {
            return Err(setup_mismatch("participant", Some(fighter)));
        }
        if !manifest_slot.occupied {
            continue;
        }
        if usize::from(manifest_slot.character.get()) >= CHARACTER_KINDS.len()
            || CHARACTER_KINDS[usize::from(manifest_slot.character.get())] != slot.character
        {
            return Err(setup_mismatch("character", Some(fighter)));
        }
        if style_definition_id(slot.style) != manifest_slot.style.get() {
            return Err(setup_mismatch("style", Some(fighter)));
        }
        if equipment_definition_id(slot.equipment) != manifest_slot.equipment.get() {
            return Err(setup_mismatch("equipment", Some(fighter)));
        }
        if team_definition_id(slot.team) != manifest_slot.team.get() {
            return Err(setup_mismatch("team", Some(fighter)));
        }
    }
    Ok(())
}

const fn style_definition_id(style: FighterStyleKind) -> u16 {
    match style {
        FighterStyleKind::Anchor => 0,
        FighterStyleKind::Vector => 1,
        FighterStyleKind::Catalyst => 2,
    }
}

const fn equipment_definition_id(equipment: EquipmentKind) -> u16 {
    match equipment {
        EquipmentKind::DashCoil => 0,
        EquipmentKind::AerialSpur => 1,
        EquipmentKind::CounterCell => 2,
        EquipmentKind::HeavySeal => 3,
    }
}

const fn team_definition_id(team: TeamId) -> u8 {
    match team {
        TeamId::Red => 0,
        TeamId::Blue => 1,
    }
}

const fn setup_mismatch(field: &'static str, fighter: Option<FighterId>) -> HeadlessBuildError {
    HeadlessBuildError::SetupMismatch { field, fighter }
}

#[cfg(test)]
mod tests {
    use super::*;

    use bevy::audio::{AudioPlayer, AudioSource};
    use bevy::pbr::StandardMaterial;
    use bevy::prelude::{AssetServer, Assets, ChildOf, Font, Image, Mesh, Node, Scene, Window};

    use crate::arena::{ArenaGeometry, ArenaHazardState, ArenaPipeState, PowderKegCannonState};
    use crate::arena_defs::ActiveArena;
    use crate::authority::AuthoritySimulation;
    use crate::authority_input::{
        AuthorityInputOrigin, AuthorityInputRecord, AuthorityInputStatus, CommittedTickInputs,
    };
    use crate::characters::FighterCharacter;
    use crate::components::{
        BotBehaviorMode, BotBrain, DrunkStatus, Fighter, FighterAction, FighterActionState,
        FighterInput, FighterMotor, FighterStats, FighterVisualRoot, LocalInputAssignment,
        SimPosition,
    };
    use crate::determinism::{DEFAULT_F32_QUANTIZATION, canonicalize_f32};
    use crate::ecs_identity::StableSimEntity;
    use crate::effects::VisualEffect;
    use crate::equipment::{EQUIPMENT_KINDS, FighterEquipment};
    use crate::fighter::{FighterPresentationIntentJournal, FighterPresentationKind};
    use crate::items::{ArenaItem, ItemPresentationIntentJournal};
    use crate::match_config::{
        DEFAULT_INPUT_DELAY_TICKS, DEFAULT_ROLLBACK_LIMIT_TICKS, DEFAULT_SNAPSHOT_HISTORY_TICKS,
        MatchBuildOptions, build_headless_match_config, canonical_manifest_hash,
    };
    use crate::network_protocol::{
        AuthorityKind, BuildId, CompatibilityId, DefinitionId, FighterSlotConfig,
        GameplayContentHash, InputButtons, InputFrame, InputSequence, MAX_FIGHTERS,
        MAX_NORMAL_ROLLBACK_TICKS, MIN_SNAPSHOT_HISTORY_TICKS, ManifestHash, MatchId, PeerId,
        ProtocolVersion, QuantizedAxis, ReplayFormatVersion, SIMULATION_HZ, SeatAssignment, SeatId,
        SeatOwnership, SimulationVersion, TeamId as ProtocolTeamId,
    };
    use crate::sim_event::{
        AbilityLifecycleEvent, FighterLifecycleEvent, PresentationEventCursor,
        PresentationEventRouter, PresentationPolicy, SimEvent, SimEventKind, SimEventSource,
    };
    use crate::simulation::{ElapsedTicks, TickTimer, seconds_to_ticks_ceil};
    use crate::snapshot::MatchResultSnapshot;
    use crate::styles::{FIGHTER_STYLE_KINDS, FighterStyle};

    const SEED: u64 = 0xAFC0_5EED_1234_5678;

    fn fixture() -> HeadlessMatchConfig {
        fixture_for_arena(0)
    }

    fn fixture_for_arena(arena_index: usize) -> HeadlessMatchConfig {
        let peer = PeerId::new(77).unwrap();
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
        let mut slots = [FighterSlotConfig::default(); MAX_FIGHTERS];
        let mut setup = LocalSetup::default();
        setup.arena_index = arena_index;
        for index in 0..2 {
            slots[index] = FighterSlotConfig {
                occupied: true,
                fighter: FighterId::from_index(index).unwrap(),
                team: ProtocolTeamId::new(team_definition_id(setup.slots[index].team)).unwrap(),
                character: DefinitionId::new(
                    CHARACTER_KINDS
                        .iter()
                        .position(|kind| *kind == setup.slots[index].character)
                        .unwrap() as u16,
                )
                .unwrap(),
                style: DefinitionId::new(style_definition_id(setup.slots[index].style)).unwrap(),
                equipment: DefinitionId::new(equipment_definition_id(setup.slots[index].equipment))
                    .unwrap(),
            };
        }
        let manifest = MatchManifest {
            compatibility: CompatibilityId {
                protocol: ProtocolVersion::new(1).unwrap(),
                simulation: SimulationVersion::new(crate::match_config::CURRENT_SIMULATION_VERSION)
                    .unwrap(),
                replay: ReplayFormatVersion::new(1).unwrap(),
                build: BuildId::new([0xB1; 16]).unwrap(),
                gameplay_content: GameplayContentHash::new([0xC7; 32]).unwrap(),
            },
            manifest_hash: ManifestHash(0xAFC0),
            match_id: MatchId::new(*b"headless-fixture").unwrap(),
            authority: AuthorityKind::Dedicated,
            trusted_results: true,
            arena: DefinitionId::new(setup.arena_index as u16).unwrap(),
            rules: DefinitionId::new(setup.rule_index as u16).unwrap(),
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
        let mut local_setup = setup;
        local_setup.replay_seed = SEED;
        HeadlessMatchConfig {
            snapshot_contract: snapshot_contract_for_manifest(&manifest),
            manifest,
            local_setup,
        }
    }

    fn compact_content_fixture(arena_index: usize) -> HeadlessMatchConfig {
        let mut setup = LocalSetup::default();
        setup.arena_index = arena_index;
        setup.replay_seed = SEED ^ (arena_index as u64).wrapping_mul(0x9e37_79b9);
        let mut human_owners = [None; MAX_FIGHTERS];
        for fighter in 0..MAX_FIGHTERS {
            let slot = &mut setup.slots[fighter];
            slot.participant = ParticipantKind::Human;
            slot.input = LocalInputAssignment::Unassigned;
            slot.character =
                CHARACTER_KINDS[(arena_index * MAX_FIGHTERS + fighter) % CHARACTER_KINDS.len()];
            slot.style = FIGHTER_STYLE_KINDS[(arena_index + fighter) % FIGHTER_STYLE_KINDS.len()];
            slot.equipment = EQUIPMENT_KINDS[(arena_index + fighter) % EQUIPMENT_KINDS.len()];
            slot.team = if fighter.is_multiple_of(2) {
                TeamId::Red
            } else {
                TeamId::Blue
            };
            human_owners[fighter] =
                Some(PeerId::new(20_000 + arena_index as u64 * 10 + fighter as u64).unwrap());
        }

        let options = MatchBuildOptions {
            match_id: MatchId::new([0x40 + arena_index as u8; 16]).unwrap(),
            authority: AuthorityKind::Dedicated,
            trusted_results: true,
            human_owners,
            agreed_start_tick: SimTick(120),
            input_delay_ticks: DEFAULT_INPUT_DELAY_TICKS,
            rollback_limit_ticks: DEFAULT_ROLLBACK_LIMIT_TICKS,
            snapshot_history_ticks: DEFAULT_SNAPSHOT_HISTORY_TICKS,
        };
        let mut config = build_headless_match_config(&setup, options).unwrap();
        // Keep this cross-profile content tape independent of the build
        // metadata and whole-source content identity. Gameplay evolution still
        // changes the resulting canonical hashes, while debug/release and all
        // operating systems start from one frozen test contract.
        config.manifest.compatibility.build = BuildId::new([0xB4; 16]).unwrap();
        config.manifest.compatibility.gameplay_content =
            GameplayContentHash::new([0xC4; 32]).unwrap();
        config.manifest.manifest_hash = canonical_manifest_hash(&config.manifest);
        config.snapshot_contract = snapshot_contract_for_manifest(&config.manifest);
        config
    }

    fn neutral_inputs(config: &HeadlessMatchConfig, tick: SimTick) -> CommittedTickInputs {
        let mut committed = CommittedTickInputs {
            tick,
            by_seat: [None; crate::network_protocol::MAX_SEATS],
        };
        for assignment in config.manifest.ownership.as_slice() {
            let frame = InputFrame {
                tick,
                seat: assignment.seat,
                movement_x: QuantizedAxis::default(),
                movement_y: QuantizedAxis::default(),
                held_buttons: Default::default(),
                pressed_buttons: Default::default(),
                released_buttons: Default::default(),
                sequence: InputSequence(tick.get() as u16),
            };
            committed.by_seat[usize::from(assignment.seat.get())] = Some(AuthorityInputRecord {
                frame,
                fighter: assignment.fighter,
                origin: match assignment.owner {
                    SeatOwner::Peer(peer) => AuthorityInputOrigin::Peer(peer),
                    SeatOwner::AuthorityBot => AuthorityInputOrigin::AuthorityBot,
                },
                status: AuthorityInputStatus::Committed,
            });
        }
        committed
    }

    fn outward_ringout_inputs(config: &HeadlessMatchConfig, tick: SimTick) -> CommittedTickInputs {
        let mut committed = neutral_inputs(config, tick);
        for (seat, axis) in [(0_u8, -127_i8), (1_u8, 127_i8)] {
            let record = committed.by_seat[usize::from(seat)]
                .as_mut()
                .expect("golden fixture owns both active seats");
            record.frame.movement_x = QuantizedAxis::new(axis).unwrap();
            record.frame.held_buttons = InputButtons::default();
        }
        committed
    }

    fn compact_special_inputs(config: &HeadlessMatchConfig, tick: SimTick) -> CommittedTickInputs {
        let mut committed = neutral_inputs(config, tick);
        let set_buttons = |record: &mut AuthorityInputRecord, held: u16, pressed: u16| {
            record.frame.held_buttons = InputButtons::new(held).unwrap();
            record.frame.pressed_buttons = InputButtons::new(pressed).unwrap();
        };
        if tick == SimTick(1) {
            set_buttons(
                committed.by_seat[0].as_mut().unwrap(),
                0,
                InputButtons::SPECIAL,
            );
            set_buttons(
                committed.by_seat[1].as_mut().unwrap(),
                InputButtons::GUARD,
                InputButtons::SPECIAL,
            );
            set_buttons(
                committed.by_seat[2].as_mut().unwrap(),
                0,
                InputButtons::SPECIAL | InputButtons::AIM_GRAB,
            );
            set_buttons(
                committed.by_seat[3].as_mut().unwrap(),
                InputButtons::HEAVY,
                InputButtons::SPECIAL | InputButtons::HEAVY,
            );
        }
        committed
    }

    fn compact_item_inputs(config: &HeadlessMatchConfig, tick: SimTick) -> CommittedTickInputs {
        let mut committed = neutral_inputs(config, tick);
        if tick == SimTick(1) {
            committed.by_seat[1].as_mut().unwrap().frame.pressed_buttons =
                InputButtons::new(InputButtons::LIGHT).unwrap();
        }
        committed
    }

    fn arrange_compact_item_world(driver: &mut LiveSimulationDriver, arena_index: usize) {
        let arena = &arena_definitions()[arena_index];
        let item_position = arena
            .item_anchors
            .first()
            .expect("every shipping arena has authored item content")
            .position;
        let item_fighter = fighter_entity(driver.world_mut(), FighterId::new(1).unwrap());
        let item_position = Vec3::new(
            canonicalize_f32(item_position.x, DEFAULT_F32_QUANTIZATION),
            canonicalize_f32(item_position.y, DEFAULT_F32_QUANTIZATION),
            canonicalize_f32(item_position.z, DEFAULT_F32_QUANTIZATION),
        );
        if let Some(mut transform) = driver.world_mut().get_mut::<Transform>(item_fighter) {
            transform.translation = item_position;
        }
        driver
            .world_mut()
            .get_mut::<SimPosition>(item_fighter)
            .unwrap()
            .translation = item_position;
    }

    fn arrange_compact_hazard_world(driver: &mut LiveSimulationDriver, arena_index: usize) {
        let arena = &arena_definitions()[arena_index];
        if let Some(hazard) = arena.hazards.first() {
            let hazard_fighter = fighter_entity(driver.world_mut(), FighterId::ZERO);
            let hazard_position = Vec3::new(
                canonicalize_f32(hazard.center.x, DEFAULT_F32_QUANTIZATION),
                canonicalize_f32(hazard.center.y, DEFAULT_F32_QUANTIZATION),
                canonicalize_f32(hazard.center.z, DEFAULT_F32_QUANTIZATION),
            );
            if let Some(mut transform) = driver.world_mut().get_mut::<Transform>(hazard_fighter) {
                transform.translation = hazard_position;
            }
            driver
                .world_mut()
                .get_mut::<SimPosition>(hazard_fighter)
                .unwrap()
                .translation = hazard_position;
        }
    }

    fn component_count<T: Component>(world: &World) -> usize {
        world
            .archetypes()
            .iter()
            .flat_map(|archetype| archetype.entities())
            .filter(|entry| world.get::<T>(entry.id()).is_some())
            .count()
    }

    fn fighter_entity(world: &mut World, fighter_id: FighterId) -> Entity {
        let mut fighters = world.query::<(Entity, &Fighter)>();
        fighters
            .iter(world)
            .find_map(|(entity, fighter)| (fighter.id == fighter_id.index()).then_some(entity))
            .expect("canonical fixture contains every fighter slot")
    }

    fn route_committed_events(
        driver: &mut LiveSimulationDriver,
        confirmed_through: SimTick,
    ) -> Vec<SimEvent> {
        let world = driver.world_mut();
        let mut cursor = world
            .remove_resource::<PresentationEventCursor>()
            .expect("test installs presentation cursor");
        let mut router = world
            .remove_resource::<PresentationEventRouter>()
            .expect("test installs presentation router");
        let mut dispatched = Vec::new();
        cursor
            .route_available(
                world.resource::<SimEventJournal>(),
                &mut router,
                Some(confirmed_through),
                |event| dispatched.push(event),
            )
            .unwrap();
        world.insert_resource(cursor);
        world.insert_resource(router);
        dispatched
    }

    fn mutate_excluded_fighter_presentation(
        world: &mut World,
        entities: [Entity; 2],
        branch_b: bool,
    ) {
        let (name, color, hud_flash, reaction_side, rotation, scale, cue) = if branch_b {
            (
                "rollback-branch-b",
                Color::srgb(0.8, 0.1, 0.6),
                0.8125,
                -0.875,
                Quat::from_rotation_y(1.25),
                Vec3::splat(1.75),
                "mutated-excluded-cue-b",
            )
        } else {
            (
                "rollback-branch-a",
                Color::srgb(0.1, 0.7, 0.2),
                0.1875,
                0.625,
                Quat::from_rotation_x(0.5),
                Vec3::splat(1.25),
                "mutated-excluded-cue-a",
            )
        };

        for entity in entities {
            {
                let mut fighter = world.get_mut::<Fighter>(entity).unwrap();
                fighter.name = name;
                fighter.color = color;
            }
            world.get_mut::<FighterStats>(entity).unwrap().hud_flash = hud_flash;
            world
                .get_mut::<FighterActionState>(entity)
                .unwrap()
                .reaction_visual_side = reaction_side;
            let mut transform = world.get_mut::<Transform>(entity).unwrap();
            transform.rotation = rotation;
            transform.scale = scale;
        }
        if let Some(aftermath) = world
            .get_mut::<FighterMotor>(entities[0])
            .unwrap()
            .landing_aftermath
            .as_mut()
        {
            aftermath.cue = cue;
        }
    }

    #[test]
    fn bootstrap_is_complete_canonical_and_contains_no_client_runtime() {
        let config = fixture();
        let arena_index = config.local_setup.arena_index;
        let expected_anchors = arena_definitions()[arena_index].item_anchors.len();
        let driver = build_headless_simulation(config).unwrap();

        driver.capture_live_snapshot().unwrap();
        assert_eq!(component_count::<Fighter>(driver.world()), 4);
        assert_eq!(component_count::<FighterInput>(driver.world()), 4);
        assert_eq!(
            component_count::<ArenaItem>(driver.world()),
            expected_anchors
        );
        assert!(driver.world().contains_resource::<ActiveArena>());
        assert!(driver.world().contains_resource::<ArenaHazardState>());
        assert!(driver.world().contains_resource::<ArenaPipeState>());
        assert!(driver.world().contains_resource::<PowderKegCannonState>());
        assert_eq!(driver.world().resource::<ActiveArena>().index(), 0);

        let mut item_rows = driver
            .world()
            .archetypes()
            .iter()
            .flat_map(|archetype| archetype.entities())
            .filter_map(|entry| {
                Some((
                    driver.world().get::<StableSimEntity>(entry.id())?.id(),
                    driver.world().get::<ArenaItem>(entry.id())?,
                ))
            })
            .collect::<Vec<_>>();
        item_rows.sort_unstable_by_key(|(id, _)| *id);
        for ((_, item), anchor) in item_rows
            .iter()
            .zip(arena_definitions()[arena_index].item_anchors)
        {
            let canonical_position = Vec3::new(
                canonicalize_f32(anchor.position.x, DEFAULT_F32_QUANTIZATION),
                canonicalize_f32(anchor.position.y, DEFAULT_F32_QUANTIZATION),
                canonicalize_f32(anchor.position.z, DEFAULT_F32_QUANTIZATION),
            );
            assert_eq!(item.kind, anchor.kind);
            assert_eq!(item.position, canonical_position);
            assert_eq!(item.anchor, canonical_position);
        }

        let authority_bot = driver
            .world()
            .archetypes()
            .iter()
            .flat_map(|archetype| archetype.entities())
            .find_map(|entry| {
                let fighter = driver.world().get::<Fighter>(entry.id())?;
                (fighter.id == 1).then(|| driver.world().get::<BotBrain>(entry.id()).unwrap())
            })
            .unwrap();
        assert_eq!(authority_bot.behavior, BotBehaviorMode::Combatant);

        assert!(!driver.world().contains_resource::<AssetServer>());
        assert!(!driver.world().contains_resource::<Assets<Mesh>>());
        assert!(!driver.world().contains_resource::<Assets<Scene>>());
        assert!(!driver.world().contains_resource::<Assets<Image>>());
        assert!(!driver.world().contains_resource::<Assets<Font>>());
        assert!(
            !driver
                .world()
                .contains_resource::<Assets<StandardMaterial>>()
        );
        assert!(!driver.world().contains_resource::<Assets<AudioSource>>());
        assert!(
            !driver
                .world()
                .contains_resource::<crate::effects::EffectAssets>()
        );
        assert!(
            !driver
                .world()
                .contains_resource::<crate::combat::CombatVisualAssets>()
        );
        assert!(
            !driver
                .world()
                .contains_resource::<FighterPresentationIntentJournal>()
        );
        assert!(
            !driver
                .world()
                .contains_resource::<ItemPresentationIntentJournal>()
        );
        assert!(
            !driver
                .world()
                .contains_resource::<crate::game_state::MatchAnnouncements>()
        );
        assert!(
            !driver
                .world()
                .contains_resource::<crate::combat::HitEffects>()
        );
        assert!(
            !driver
                .world()
                .contains_resource::<crate::bee_skills::BeeSkillAssets>()
        );
        assert!(
            !driver
                .world()
                .contains_resource::<crate::chick_skills::ChickSkillAssets>()
        );
        assert!(
            !driver
                .world()
                .contains_resource::<crate::penguin_skills::PenguinSkillAssets>()
        );
        assert!(
            !driver
                .world()
                .contains_resource::<crate::specials::SpecialAssets>()
        );
        assert!(
            !driver
                .world()
                .contains_resource::<crate::items::ItemAssets>()
        );
        assert!(
            !driver
                .world()
                .contains_resource::<crate::arena::ArenaOrdnanceAssets>()
        );
        assert_eq!(component_count::<Window>(driver.world()), 0);
        assert_eq!(component_count::<Node>(driver.world()), 0);
        assert_eq!(
            component_count::<AudioPlayer<AudioSource>>(driver.world()),
            0
        );
        assert_eq!(component_count::<ArenaGeometry>(driver.world()), 0);
        assert_eq!(component_count::<FighterVisualRoot>(driver.world()), 0);
        assert_eq!(component_count::<VisualEffect>(driver.world()), 0);
        assert_eq!(component_count::<ChildOf>(driver.world()), 0);
        assert_eq!(
            driver
                .world()
                .archetypes()
                .iter()
                .map(|archetype| archetype.entities().len())
                .sum::<usize>(),
            4 + expected_anchors
        );
    }

    #[test]
    fn authority_and_prediction_use_the_same_frozen_embedded_authored_data() {
        let config = fixture();
        let authority = build_headless_simulation(config.clone()).unwrap();
        let prediction = build_predicted_simulation(config).unwrap();
        let expected_moves = CharacterMoveCatalog::from_embedded_gameplay().unwrap();
        let expected_feel = CombatFeelTuning::from_embedded_gameplay().unwrap();

        for driver in [&authority, &prediction] {
            let moves = driver.world().resource::<CharacterMoveCatalog>();
            let feel = driver.world().resource::<CombatFeelTuning>();
            assert_eq!(
                moves.authored_file_for_test(),
                expected_moves.authored_file_for_test()
            );
            assert_eq!(
                feel.authored_file_for_test(),
                expected_feel.authored_file_for_test()
            );
            assert_eq!(moves.last_error(), None);
            assert_eq!(feel.last_error(), None);
        }
    }

    #[test]
    fn committed_step_advances_exactly_once_and_independent_worlds_hash_identically() {
        let config = fixture();
        let mut first = build_headless_simulation(config.clone()).unwrap();
        let mut second = build_headless_simulation(config.clone()).unwrap();
        assert_eq!(first.state_hash().unwrap(), second.state_hash().unwrap());

        for expected in 1..=4 {
            let tick = SimTick(expected);
            let committed = neutral_inputs(&config, tick);
            AuthoritySimulation::step(&mut first, &committed).unwrap();
            AuthoritySimulation::step(&mut second, &committed).unwrap();
            assert_eq!(first.current_sim_tick(), tick);
            assert_eq!(second.current_sim_tick(), tick);
            assert_eq!(first.state_hash().unwrap(), second.state_hash().unwrap());
        }
    }

    #[test]
    fn rollback_replay_event_stream_is_independent_of_excluded_presentation_state() {
        let config = fixture();
        let mut driver = build_predicted_simulation(config.clone()).unwrap();
        driver
            .world_mut()
            .insert_resource(PresentationEventCursor::default());
        driver
            .world_mut()
            .insert_resource(PresentationEventRouter::default());

        let prime_tick = SimTick(1);
        AuthoritySimulation::step(&mut driver, &neutral_inputs(&config, prime_tick)).unwrap();
        let _ = route_committed_events(&mut driver, prime_tick);

        let arena = &arena_definitions()[config.local_setup.arena_index];
        let (dash_landing_fighter, drunk_ringout_fighter) = {
            let world = driver.world_mut();
            (
                fighter_entity(world, FighterId::ZERO),
                fighter_entity(world, FighterId::new(1).unwrap()),
            )
        };
        let entities = [dash_landing_fighter, drunk_ringout_fighter];
        {
            let world = driver.world_mut();
            for entity in entities {
                let translation = world
                    .get::<SimPosition>(entity)
                    .expect("predicted fighter has canonical position")
                    .translation;
                world
                    .entity_mut(entity)
                    .insert(Transform::from_translation(translation));
            }
            let mut aftermath = crate::reactions::reaction_family_definition(
                crate::reactions::ReactionFamilyId::GroundBounceDown,
            )
            .landing_aftermath
            .unwrap();
            aftermath.horizontal_damping =
                canonicalize_f32(aftermath.horizontal_damping, DEFAULT_F32_QUANTIZATION);
            {
                let mut motor = world.get_mut::<FighterMotor>(dash_landing_fighter).unwrap();
                motor.grounded = false;
                motor.velocity = Vec3::new(0.0, -3.0, 0.0);
                motor.landing_aftermath = Some(aftermath);
            }
            {
                let mut action = world
                    .get_mut::<FighterActionState>(dash_landing_fighter)
                    .unwrap();
                action.action = FighterAction::Dashing;
                action.elapsed = ElapsedTicks::from_ticks(
                    seconds_to_ticks_ceil(crate::constants::DASH_TRAIL_REPEAT) - 1,
                );
            }
            let spawn = arena.spawn_points[0];
            world
                .get_mut::<SimPosition>(dash_landing_fighter)
                .unwrap()
                .translation = Vec3::new(
                canonicalize_f32(spawn.x, DEFAULT_F32_QUANTIZATION),
                canonicalize_f32(spawn.y, DEFAULT_F32_QUANTIZATION),
                canonicalize_f32(spawn.z, DEFAULT_F32_QUANTIZATION),
            );

            world
                .get_mut::<DrunkStatus>(drunk_ringout_fighter)
                .unwrap()
                .remaining = TickTimer::from_seconds_ceil(crate::constants::DRUNK_DURATION);
            world
                .get_mut::<SimPosition>(drunk_ringout_fighter)
                .unwrap()
                .translation = Vec3::new(
                100.0,
                canonicalize_f32(arena.spawn_points[1].y, DEFAULT_F32_QUANTIZATION),
                0.0,
            );
            mutate_excluded_fighter_presentation(world, entities, false);
        }
        let rollback_snapshot = driver.capture_live_snapshot().unwrap();

        let replay_tick = SimTick(2);
        let mut replay_inputs = neutral_inputs(&config, replay_tick);
        replay_inputs.by_seat[0].as_mut().unwrap().frame.movement_x =
            QuantizedAxis::new(127).unwrap();
        AuthoritySimulation::step(&mut driver, &replay_inputs).unwrap();

        let first_events = driver
            .world()
            .resource::<SimEventJournal>()
            .iter_at(replay_tick)
            .copied()
            .collect::<Vec<_>>();
        for (ordinal, event) in first_events.iter().enumerate() {
            assert_eq!(usize::from(event.id.ordinal), ordinal);
        }
        let event_index = |predicate: fn(&SimEventKind) -> bool| {
            first_events
                .iter()
                .position(|event| predicate(&event.kind))
                .expect("expected event kind is present")
        };
        let drunk_index = event_index(|kind| {
            matches!(
                kind,
                SimEventKind::FighterLifecycle {
                    event: FighterLifecycleEvent::DrunkBubble,
                    ..
                }
            )
        });
        let dash_index = event_index(|kind| {
            matches!(
                kind,
                SimEventKind::FighterLifecycle {
                    event: FighterLifecycleEvent::DashTrail,
                    ..
                }
            )
        });
        let aftermath_index = event_index(|kind| {
            matches!(
                kind,
                SimEventKind::FighterLifecycle {
                    event: FighterLifecycleEvent::LandingAftermath,
                    ..
                }
            )
        });
        let stock_index = event_index(|kind| matches!(kind, SimEventKind::StockLost { .. }));
        assert!(drunk_index < stock_index);
        assert!(dash_index < stock_index);
        assert!(aftermath_index < stock_index);

        let first_intents = {
            let intents = driver
                .world()
                .resource::<FighterPresentationIntentJournal>();
            first_events
                .iter()
                .filter_map(|event| intents.get(event.id).map(|intent| (event.id, intent.kind)))
                .collect::<Vec<_>>()
        };
        assert!(first_intents.iter().any(|(_, kind)| {
            matches!(
                kind,
                FighterPresentationKind::LandingAftermath {
                    cue: "reaction_bounce_down",
                    ..
                }
            )
        }));
        assert!(first_intents.iter().any(|(_, kind)| {
            matches!(
                kind,
                FighterPresentationKind::DrunkBubble { phase, .. } if *phase == 0.0
            )
        }));

        let first_dispatched = route_committed_events(&mut driver, replay_tick);
        assert_eq!(first_dispatched, first_events);

        mutate_excluded_fighter_presentation(driver.world_mut(), entities, true);
        driver.restore_live_snapshot(&rollback_snapshot).unwrap();
        {
            let world = driver.world_mut();
            for entity in entities {
                assert_eq!(
                    world.get::<Fighter>(entity).unwrap().name,
                    "rollback-branch-b"
                );
                assert_eq!(
                    world.get::<Fighter>(entity).unwrap().color,
                    Color::srgb(0.8, 0.1, 0.6)
                );
                assert_eq!(world.get::<FighterStats>(entity).unwrap().hud_flash, 0.8125);
                assert_eq!(
                    world
                        .get::<FighterActionState>(entity)
                        .unwrap()
                        .reaction_visual_side,
                    -0.875
                );
                let transform = world.get::<Transform>(entity).unwrap();
                assert_eq!(transform.rotation, Quat::from_rotation_y(1.25));
                assert_eq!(transform.scale, Vec3::splat(1.75));
            }
            world
                .get_mut::<FighterMotor>(dash_landing_fighter)
                .unwrap()
                .landing_aftermath
                .as_mut()
                .unwrap()
                .cue = "post-restore-mutated-excluded-cue";
        }

        AuthoritySimulation::step(&mut driver, &replay_inputs).unwrap();
        let replayed_events = driver
            .world()
            .resource::<SimEventJournal>()
            .iter_at(replay_tick)
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(replayed_events, first_events);
        let replayed_intents = {
            let intents = driver
                .world()
                .resource::<FighterPresentationIntentJournal>();
            replayed_events
                .iter()
                .filter_map(|event| intents.get(event.id).map(|intent| (event.id, intent.kind)))
                .collect::<Vec<_>>()
        };
        assert_eq!(replayed_intents, first_intents);

        let replayed_dispatches = route_committed_events(&mut driver, replay_tick);
        assert!(
            replayed_dispatches
                .iter()
                .all(|event| { event.kind.presentation_policy() == PresentationPolicy::Predicted })
        );
        for irreversible in first_events
            .iter()
            .filter(|event| event.kind.presentation_policy() != PresentationPolicy::Predicted)
        {
            assert!(
                !replayed_dispatches
                    .iter()
                    .any(|event| event.id == irreversible.id)
            );
        }
        assert!(
            driver
                .world()
                .resource::<PresentationEventRouter>()
                .metrics()
                .duplicate_events_suppressed
                > 0
        );
    }

    #[test]
    fn cross_platform_golden_stock_ringout_tape_matches_frozen_hashes_and_result() {
        const EXPECTED_CHECKPOINTS: [(u64, u64); 6] = [
            (1, 0x5cb7_9acd_3a84_77b9),
            (120, 0xfb2f_bbca_96e5_0ed0),
            (240, 0xbbee_3e67_295a_0e73),
            (360, 0x8735_f205_1de7_af5a),
            (480, 0x436a_842f_eaed_79f0),
            (600, 0x3420_1466_fca4_0adc),
        ];
        const HISTORICAL_V4_CHECKPOINTS: [(u64, u64); 6] = [
            (1, 0x0114_c86d_5060_830c),
            (120, 0x57c7_c8ca_b49b_e405),
            (240, 0x65b9_2dec_2377_722a),
            (360, 0x51e4_071d_e3fe_06ef),
            (480, 0xe018_fe6e_3896_65cd),
            (600, 0xa404_842d_3686_b979),
        ];
        const EXPECTED_FINAL_TICK: SimTick = SimTick(709);
        const EXPECTED_FINAL_HASH: u64 = 0xdea5_b6eb_6275_a281;
        const HISTORICAL_V4_FINAL_HASH: u64 = 0xa567_7c44_0896_53d6;

        let config = fixture();
        assert_eq!(
            config.manifest.compatibility.simulation.get(),
            crate::match_config::CURRENT_SIMULATION_VERSION
        );
        let mut driver = build_headless_simulation(config.clone()).unwrap();
        let mut checkpoints = Vec::new();
        let mut historical_v4_checkpoints = Vec::new();

        for raw_tick in 1..=2_400 {
            let tick = SimTick(raw_tick);
            AuthoritySimulation::step(&mut driver, &outward_ringout_inputs(&config, tick)).unwrap();
            if raw_tick == 1 || raw_tick % 120 == 0 {
                let current = driver.capture_live_snapshot().unwrap();
                checkpoints.push((raw_tick, current.canonical_hash().unwrap()));
                // This tape never uses AIM_GRAB. Rewriting only the snapshot's
                // version discriminator must therefore reproduce the reviewed
                // v4 hash at every checkpoint; any other state divergence is
                // an unreviewed gameplay change, not a v5 header refresh.
                let mut historical_v4 = current;
                historical_v4.header.simulation_version = 4;
                historical_v4_checkpoints.push((raw_tick, historical_v4.canonical_hash().unwrap()));
            }
            if driver.world().resource::<MatchState>().phase == MatchPhase::Results {
                break;
            }
        }

        let snapshot = driver.capture_live_snapshot().unwrap();
        assert_eq!(checkpoints, EXPECTED_CHECKPOINTS);
        assert_eq!(historical_v4_checkpoints, HISTORICAL_V4_CHECKPOINTS);
        assert_eq!(snapshot.header.tick, EXPECTED_FINAL_TICK);
        assert_eq!(snapshot.canonical_hash().unwrap(), EXPECTED_FINAL_HASH);
        let mut historical_v4_final = snapshot.clone();
        historical_v4_final.header.simulation_version = 4;
        assert_eq!(
            historical_v4_final.canonical_hash().unwrap(),
            HISTORICAL_V4_FINAL_HASH
        );
        assert_eq!(
            snapshot.match_state.result,
            MatchResultSnapshot::TeamWinner {
                team: 1,
                decided_tick: EXPECTED_FINAL_TICK,
            }
        );
    }

    #[test]
    fn every_authored_arena_is_snapshot_ready_at_bootstrap() {
        for arena_index in 0..arena_definitions().len() {
            let expected_items = arena_definitions()[arena_index].item_anchors.len();
            let driver = build_headless_simulation(fixture_for_arena(arena_index)).unwrap();
            let snapshot = driver.capture_live_snapshot().unwrap();
            assert_eq!(snapshot.match_state.rules.arena_id, arena_index as u32);
            assert_eq!(snapshot.dynamic_objects.len(), expected_items);
            assert_eq!(component_count::<ArenaItem>(driver.world()), expected_items);
            assert_eq!(
                driver.world().resource::<ActiveArena>().index(),
                arena_index
            );
        }
    }

    #[test]
    fn compact_all_content_matrix_matches_frozen_hashes() {
        // Each arena freezes the independent special/hazard and item branches
        // after semantic review.
        const EXPECTED_FINAL_HASHES: [[u64; 2]; 10] = [
            [0xe210_c317_79aa_8715, 0x0cea_48c8_4636_5c7e],
            [0x1c7c_ddaa_5d78_d085, 0x0424_21e2_fd13_c809],
            [0x107a_0d21_ef6a_1ee5, 0x36ba_eb37_10df_9e64],
            [0x64ba_5b47_f0a1_5c66, 0xec86_de29_dfc8_f4bd],
            [0x2d7c_86bd_46e7_ae5d, 0x4b0d_50bd_b65c_c76d],
            [0x763b_f076_7aef_3d90, 0xfff3_a5f4_0036_34f3],
            [0xa438_d82d_1c47_9e3b, 0x6981_7e7d_d7ab_1c81],
            [0x515e_09ef_df42_281d, 0x0c6c_1b2a_a670_c364],
            [0x54fd_d40e_3e45_aa68, 0x6c18_f183_8552_66da],
            [0x6ca7_d27c_fb69_210b, 0x6097_f41c_5364_25a2],
        ];

        assert_eq!(arena_definitions().len(), 10);
        assert_eq!(CHARACTER_KINDS.len(), 8);
        assert_eq!(FIGHTER_STYLE_KINDS.len(), 3);
        assert_eq!(EQUIPMENT_KINDS.len(), 4);

        let mut character_coverage = [false; 8];
        let mut style_coverage = [false; 3];
        let mut equipment_coverage = [false; 4];
        let mut final_hashes = Vec::with_capacity(arena_definitions().len());
        let mut ability_spawns = 0_usize;
        let mut item_pickups = 0_usize;
        let mut hazard_contacts = 0_usize;

        for arena_index in 0..arena_definitions().len() {
            let config = compact_content_fixture(arena_index);
            let mut special_first = build_headless_simulation(config.clone()).unwrap();
            let mut special_second = build_headless_simulation(config.clone()).unwrap();
            arrange_compact_hazard_world(&mut special_first, arena_index);
            arrange_compact_hazard_world(&mut special_second, arena_index);
            let mut arena_ability_spawns = 0_usize;

            {
                let world = special_first.world_mut();
                let mut loadouts =
                    world.query::<(&FighterCharacter, &FighterStyle, &FighterEquipment)>();
                for (character, style, equipment) in loadouts.iter(world) {
                    character_coverage[CHARACTER_KINDS
                        .iter()
                        .position(|kind| *kind == character.kind)
                        .unwrap()] = true;
                    style_coverage[FIGHTER_STYLE_KINDS
                        .iter()
                        .position(|kind| *kind == style.kind)
                        .unwrap()] = true;
                    equipment_coverage[EQUIPMENT_KINDS
                        .iter()
                        .position(|kind| *kind == equipment.kind)
                        .unwrap()] = true;
                }
            }

            for raw_tick in 1..=120 {
                let tick = SimTick(raw_tick);
                let inputs = compact_special_inputs(&config, tick);
                AuthoritySimulation::step(&mut special_first, &inputs).unwrap();
                AuthoritySimulation::step(&mut special_second, &inputs).unwrap();
                assert_eq!(
                    special_first.state_hash().unwrap(),
                    special_second.state_hash().unwrap(),
                    "arena {arena_index} special/hazard branch diverged at tick {raw_tick}"
                );
                for event in special_first
                    .world()
                    .resource::<SimEventJournal>()
                    .iter_at(tick)
                {
                    if matches!(
                        event.kind,
                        SimEventKind::AbilityLifecycle {
                            event: AbilityLifecycleEvent::Spawned,
                            ..
                        }
                    ) {
                        ability_spawns += 1;
                        arena_ability_spawns += 1;
                    }
                    if matches!(event.id.source, SimEventSource::ArenaHazard { .. }) {
                        hazard_contacts += 1;
                    }
                }
            }
            assert!(
                arena_ability_spawns >= MAX_FIGHTERS,
                "arena {arena_index} did not execute all four special requests"
            );

            let special_final_hash = special_first.state_hash().unwrap();
            let mut item_first = build_headless_simulation(config.clone()).unwrap();
            let mut item_second = build_headless_simulation(config.clone()).unwrap();
            arrange_compact_item_world(&mut item_first, arena_index);
            arrange_compact_item_world(&mut item_second, arena_index);
            let mut arena_item_pickups = 0_usize;
            for raw_tick in 1..=4 {
                let tick = SimTick(raw_tick);
                let inputs = compact_item_inputs(&config, tick);
                AuthoritySimulation::step(&mut item_first, &inputs).unwrap();
                AuthoritySimulation::step(&mut item_second, &inputs).unwrap();
                assert_eq!(
                    item_first.state_hash().unwrap(),
                    item_second.state_hash().unwrap(),
                    "arena {arena_index} item branch diverged at tick {raw_tick}"
                );
                for event in item_first
                    .world()
                    .resource::<SimEventJournal>()
                    .iter_at(tick)
                {
                    if matches!(
                        event.kind,
                        SimEventKind::ItemLifecycle {
                            event: crate::sim_event::ItemLifecycleEvent::PickedUp,
                            ..
                        }
                    ) {
                        item_pickups += 1;
                        arena_item_pickups += 1;
                    }
                }
            }
            assert!(
                arena_item_pickups >= 1,
                "arena {arena_index} did not execute its authored portable-item content"
            );
            final_hashes.push([special_final_hash, item_first.state_hash().unwrap()]);
        }

        assert!(character_coverage.into_iter().all(|covered| covered));
        assert!(style_coverage.into_iter().all(|covered| covered));
        assert!(equipment_coverage.into_iter().all(|covered| covered));
        assert!(
            ability_spawns >= arena_definitions().len() * MAX_FIGHTERS,
            "all four special variants must spawn in every arena"
        );
        assert!(
            item_pickups >= arena_definitions().len(),
            "every arena must execute its authored portable-item content"
        );
        assert!(
            hazard_contacts > 0,
            "the matrix must execute at least one authored static hazard contact"
        );
        assert_eq!(final_hashes, EXPECTED_FINAL_HASHES);
    }

    #[test]
    #[ignore = "release-candidate production-state soak; executes 200,000 fixed schedule steps"]
    fn production_bevy_state_repeated_hash_soak_100000_ticks() {
        const SOAK_TICKS: u64 = 100_000;
        const HASH_INTERVAL: u64 = 1_000;

        let config = compact_content_fixture(0);
        let mut first = build_headless_simulation(config.clone()).unwrap();
        let mut second = build_headless_simulation(config.clone()).unwrap();
        let initial_hash = first.state_hash().unwrap();
        assert_eq!(initial_hash, second.state_hash().unwrap());

        for raw_tick in 1..=SOAK_TICKS {
            let tick = SimTick(raw_tick);
            let inputs = neutral_inputs(&config, tick);
            AuthoritySimulation::step(&mut first, &inputs).unwrap();
            AuthoritySimulation::step(&mut second, &inputs).unwrap();
            if raw_tick % HASH_INTERVAL == 0 || raw_tick == SOAK_TICKS {
                assert_eq!(
                    first.state_hash().unwrap(),
                    second.state_hash().unwrap(),
                    "production Bevy worlds diverged at repeated-hash checkpoint {raw_tick}"
                );
            }
        }

        assert_eq!(first.current_sim_tick(), SimTick(SOAK_TICKS));
        assert_eq!(second.current_sim_tick(), SimTick(SOAK_TICKS));
        assert_ne!(first.state_hash().unwrap(), initial_hash);
    }

    #[test]
    fn contract_and_setup_disagreement_fail_before_world_construction() {
        let mut bad_contract = fixture();
        bad_contract.snapshot_contract.master_seed ^= 1;
        assert!(matches!(
            build_headless_simulation(bad_contract),
            Err(HeadlessBuildError::ContractMismatch("master seed"))
        ));

        let mut bad_setup = fixture();
        bad_setup.local_setup.slots[0].input = LocalInputAssignment::Keyboard(9);
        // Raw device assignments are client-local and therefore do not enter
        // authority validation.
        assert!(bad_setup.validate().is_ok());
        bad_setup.local_setup.slots[0].participant = ParticipantKind::Bot;
        assert!(matches!(
            build_headless_simulation(bad_setup),
            Err(HeadlessBuildError::SetupMismatch {
                field: "participant",
                fighter: Some(FighterId::ZERO),
            })
        ));
    }
}

#[cfg(test)]
#[path = "../tests/support/behavior_fixtures.rs"]
mod behavior_fixtures;
