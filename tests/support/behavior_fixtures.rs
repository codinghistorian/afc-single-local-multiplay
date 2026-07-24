//! Versioned production-headless behavior fixtures.
//!
//! This module is included from `headless.rs` under `cfg(test)` so it can use
//! crate-internal production composition without creating a second public test
//! API. Fixture input is compiled completely before any simulation or restore:
//! local gesture state is intentionally not snapshot state.

use std::fs;
use std::path::{Path, PathBuf};

use bevy::prelude::{
    App, Entity, FixedUpdate, IntoScheduleConfigs, Name, Res, ResMut, Resource, Transform, Vec3,
    World,
};
use serde::{Deserialize, Serialize};

use crate::authority::AuthoritySimulation;
use crate::authority_input::{
    AuthorityInputOrigin, AuthorityInputRecord, AuthorityInputStatus, CommittedTickInputs,
};
use crate::characters::CHARACTER_KINDS;
use crate::components::{
    Fighter, FighterAction, FighterActionState, FighterGrabState, FighterMotor, FighterStats,
    Hitbox, LocalInputAssignment, ParticipantKind, SimPosition,
};
use crate::determinism::{
    DEFAULT_F32_QUANTIZATION, FighterId, SimEntityId, SimEntityKind, SimTick, dequantize_f32,
};
use crate::ecs_identity::StableSimEntity;
use crate::equipment::EquipmentKind;
use crate::game_state::{Hitstop, LocalSetup, TeamId};
use crate::headless::{HeadlessMatchConfig, build_headless_simulation};
use crate::items::{ArenaItem, ItemKind, ItemState};
use crate::live_authority::LiveSimulationDriver;
use crate::live_input::local_tick_to_network_input;
use crate::match_config::{
    DEFAULT_INPUT_DELAY_TICKS, DEFAULT_ROLLBACK_LIMIT_TICKS, DEFAULT_SNAPSHOT_HISTORY_TICKS,
    MatchBuildOptions, build_headless_match_config,
};
use crate::network_protocol::{
    AuthorityKind, InputButtons, InputFrame, InputSequence as NetworkInputSequence, MatchId,
    PeerId, SeatOwner,
};
use crate::sim_event::{SimEvent, SimEventJournal, SimEventKind, SimEventSource};
use crate::simulation::{ElapsedTicks, SimulationSet, TickTimer};
use crate::snapshot::{CanonicalSnapshot, MatchPhaseSnapshot};
use crate::styles::FighterStyleKind;
use crate::tick_input::{
    InputMask, InputSequence, LocalSeatId, QuantizedMovement, SeatGestureTrackers, TickInputFrame,
};

const FIXTURE_SCHEMA_VERSION: u16 = 1;
const CONTRACT_VERSION: u16 = 5;
const FIXTURE_DIRECTORY: &str = "tests/fixtures/behavior/v1";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BehaviorFixture {
    schema_version: u16,
    contract_version: u16,
    id: u8,
    name: String,
    classification: FixtureClassification,
    setup: FixtureSetup,
    #[serde(default, skip_serializing_if = "FixtureInitialState::is_empty")]
    initial_state: FixtureInitialState,
    #[serde(default, skip_serializing_if = "FixtureObservations::is_empty")]
    observations: FixtureObservations,
    duration_ticks: u64,
    restore_tick: u64,
    stop_on_result: bool,
    checkpoint_ticks: Vec<u64>,
    raw_spans: Vec<RawInputSpan>,
    raw_edges: Vec<RawInputEdge>,
    action_spans: Vec<ActionInputSpan>,
    action_edges: Vec<ActionInputEdge>,
    expected: Option<FixtureExpected>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum FixtureClassification {
    Preserve,
    AcceptedChange,
    UndefinedLegacyOrder,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureSetup {
    arena: usize,
    rules: usize,
    seed: u64,
    slots: Vec<FixtureSlot>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureSlot {
    occupied: bool,
    character: u8,
    style: u8,
    equipment: u8,
    team: u8,
}

/// Test-only canonical bootstrap overrides for interactions whose arbitration
/// requires exact same-tick placement. These are applied after the production
/// headless world is built and before tick one; every clean, perturbed, and
/// restore execution receives the same values.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureInitialState {
    fighters: Vec<FixtureInitialFighter>,
    items: Vec<FixtureInitialItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    grabs: Vec<FixtureInitialGrab>,
}

impl FixtureInitialState {
    fn is_empty(&self) -> bool {
        self.fighters.is_empty() && self.items.is_empty() && self.grabs.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureInitialFighter {
    fighter: u8,
    position_q12: [i32; 3],
    spawn_q12: Option<[i32; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    facing_q12: Option<[i32; 3]>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureInitialGrab {
    holder: u8,
    victim: u8,
    holder_elapsed_ticks: u32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureInitialItem {
    /// Ordinal after sorting authored item entities by stable simulation ID.
    ordinal: u8,
    position_q12: [i32; 3],
}

/// Selects extra human-readable checkpoint fields without rewriting unrelated
/// goldens. The canonical per-tick hash always covers the complete snapshot.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureObservations {
    #[serde(default, skip_serializing_if = "is_false")]
    extended_fighters: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    match_stats: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    item_ordinals: Vec<u8>,
}

impl FixtureObservations {
    fn is_empty(&self) -> bool {
        !self.extended_fighters && !self.match_stats && self.item_ordinals.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInputSpan {
    fighter: u8,
    start_tick: u64,
    end_tick: u64,
    movement: [i8; 2],
    held: u16,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInputEdge {
    fighter: u8,
    tick: u64,
    pressed: u16,
    released: u16,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionInputSpan {
    fighter: u8,
    start_tick: u64,
    end_tick: u64,
    held: u16,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionInputEdge {
    fighter: u8,
    tick: u64,
    pressed: u16,
    released: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureExpected {
    hashes: Vec<GoldenHash>,
    checkpoints: Vec<SemanticCheckpoint>,
    event_ticks: Vec<GoldenEventTick>,
    final_tick: u64,
    final_result: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GoldenHash {
    tick: u64,
    hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GoldenEventTick {
    tick: u64,
    events: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SemanticCheckpoint {
    tick: u64,
    phase: String,
    hitstop_ticks: u32,
    result: String,
    dynamic_objects: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    throws: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    item_hits: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    items: Vec<SemanticItem>,
    fighters: Vec<SemanticFighter>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SemanticItem {
    ordinal: u8,
    stable_index: u32,
    generation: u32,
    kind: String,
    state: String,
    owner: Option<u8>,
    position: [i32; 3],
    velocity: [i32; 3],
    durability: i32,
    max_durability: i32,
    respawn_ticks: u32,
    pickup_lockout_ticks: u32,
    state_timer_a: u32,
    state_timer_b: u32,
    already_hit: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SemanticFighter {
    id: u8,
    active: bool,
    position: [i32; 3],
    velocity: [i32; 3],
    grounded: bool,
    health: i32,
    stamina: i32,
    stocks: u8,
    action_id: u16,
    action_ticks: u32,
    #[serde(default, skip_serializing_if = "is_false")]
    held_item: bool,
    holding: Option<u8>,
    held_by: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    regrab_lockout_ticks: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_attacker: Option<u8>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TickObservation {
    hash: u64,
    semantics: SemanticCheckpoint,
    events: Vec<String>,
    canonical: CanonicalObservation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CanonicalObservation {
    hazard_clock_ticks: u32,
    hazard_cooldowns: [u32; crate::network_protocol::MAX_FIGHTERS],
    damage_by_fighter: [i32; crate::network_protocol::MAX_FIGHTERS],
    fighters: [CanonicalFighterObservation; crate::network_protocol::MAX_FIGHTERS],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CanonicalFighterObservation {
    dash_slide_ticks: u32,
    reaction_family: Option<u8>,
    last_attacker: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RunTrace {
    ticks: Vec<TickObservation>,
}

struct InitialRun {
    driver: crate::live_authority::LiveSimulationDriver,
    trace: RunTrace,
    restore_snapshot: CanonicalSnapshot,
}

struct CompiledFixture {
    config: HeadlessMatchConfig,
    inputs: Vec<CommittedTickInputs>,
}

fn fixture_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE_DIRECTORY)
}

fn fixture_paths() -> Vec<PathBuf> {
    let mut paths = fs::read_dir(fixture_directory())
        .expect("behavior fixture directory must exist")
        .map(|entry| {
            entry
                .expect("behavior fixture directory entry must be readable")
                .path()
        })
        .filter(|path| path.extension().is_some_and(|extension| extension == "ron"))
        .collect::<Vec<_>>();
    paths.sort();
    assert!(
        !paths.is_empty(),
        "at least one behavior fixture is required"
    );
    paths
}

fn load_fixture(path: &Path) -> BehaviorFixture {
    let source = fs::read_to_string(path).unwrap_or_else(|error| {
        panic!(
            "failed to read behavior fixture {}: {error}",
            path.display()
        )
    });
    let fixture: BehaviorFixture = ron::from_str(&source).unwrap_or_else(|error| {
        panic!(
            "failed to parse behavior fixture {}: {error}",
            path.display()
        )
    });
    validate_fixture(&fixture, path);
    fixture
}

fn validate_fixture(fixture: &BehaviorFixture, path: &Path) {
    assert_eq!(
        fixture.schema_version,
        FIXTURE_SCHEMA_VERSION,
        "{} uses an unsupported fixture schema",
        path.display()
    );
    assert_eq!(
        fixture.contract_version,
        CONTRACT_VERSION,
        "{} is not a simulation-v{CONTRACT_VERSION} fixture",
        path.display()
    );
    assert_ne!(fixture.id, 0, "{} reserves fixture ID zero", path.display());
    assert!(
        !fixture.name.is_empty(),
        "{} has an empty fixture name",
        path.display()
    );
    assert_eq!(
        fixture.setup.slots.len(),
        crate::network_protocol::MAX_FIGHTERS,
        "{} must declare all fighter slots",
        path.display()
    );
    assert!(
        fixture.duration_ticks > 0
            && fixture.restore_tick > 0
            && fixture.restore_tick < fixture.duration_ticks,
        "{} has an invalid duration/restore boundary",
        path.display()
    );
    assert!(
        fixture
            .checkpoint_ticks
            .windows(2)
            .all(|window| window[0] < window[1])
            && fixture
                .checkpoint_ticks
                .iter()
                .all(|tick| *tick > 0 && *tick <= fixture.duration_ticks),
        "{} checkpoint ticks must be sorted, unique, and in range",
        path.display()
    );
    for span in &fixture.raw_spans {
        validate_span(
            path,
            span.fighter,
            span.start_tick,
            span.end_tick,
            fixture.duration_ticks,
        );
        assert!(
            InputMask::CURRENT_BINDINGS.contains(InputMask::from_bits(span.held)),
            "{} raw span contains unsupported held bits",
            path.display()
        );
        assert_ne!(
            span.movement[0],
            i8::MIN,
            "{} uses the reserved movement axis",
            path.display()
        );
        assert_ne!(
            span.movement[1],
            i8::MIN,
            "{} uses the reserved movement axis",
            path.display()
        );
    }
    for edge in &fixture.raw_edges {
        validate_edge(path, edge.fighter, edge.tick, fixture.duration_ticks);
        let bits = InputMask::from_bits(edge.pressed | edge.released);
        assert!(
            InputMask::CURRENT_BINDINGS.contains(bits),
            "{} raw edge contains unsupported bits",
            path.display()
        );
    }
    for span in &fixture.action_spans {
        validate_span(
            path,
            span.fighter,
            span.start_tick,
            span.end_tick,
            fixture.duration_ticks,
        );
        InputButtons::new(span.held).unwrap_or_else(|error| {
            panic!("{} has invalid action held bits: {error}", path.display())
        });
    }
    for edge in &fixture.action_edges {
        validate_edge(path, edge.fighter, edge.tick, fixture.duration_ticks);
        InputButtons::new(edge.pressed | edge.released).unwrap_or_else(|error| {
            panic!("{} has invalid action edge bits: {error}", path.display())
        });
    }
    let mut initial_fighters = fixture
        .initial_state
        .fighters
        .iter()
        .map(|fighter| fighter.fighter)
        .collect::<Vec<_>>();
    initial_fighters.sort_unstable();
    assert!(
        initial_fighters.windows(2).all(|pair| pair[0] != pair[1]),
        "{} assigns one initial state per fighter",
        path.display()
    );
    for fighter in &fixture.initial_state.fighters {
        let index = usize::from(fighter.fighter);
        assert!(
            index < crate::network_protocol::MAX_FIGHTERS && fixture.setup.slots[index].occupied,
            "{} initial state targets an inactive/invalid fighter",
            path.display()
        );
        assert!(
            fighter.facing_q12.is_none_or(|facing| {
                facing
                    .into_iter()
                    .map(i64::from)
                    .map(|axis| axis * axis)
                    .sum::<i64>()
                    > 0
            }),
            "{} assigns a zero initial facing",
            path.display()
        );
    }
    let mut grab_roles = Vec::new();
    for grab in &fixture.initial_state.grabs {
        let holder = usize::from(grab.holder);
        let victim = usize::from(grab.victim);
        assert!(
            holder < crate::network_protocol::MAX_FIGHTERS
                && victim < crate::network_protocol::MAX_FIGHTERS
                && fixture.setup.slots[holder].occupied
                && fixture.setup.slots[victim].occupied
                && holder != victim,
            "{} initial grab targets an inactive, invalid, or identical pair",
            path.display()
        );
        grab_roles.extend([grab.holder, grab.victim]);
    }
    grab_roles.sort_unstable();
    assert!(
        grab_roles.windows(2).all(|pair| pair[0] != pair[1]),
        "{} assigns one fighter to multiple initial grab roles",
        path.display()
    );
    let mut initial_items = fixture
        .initial_state
        .items
        .iter()
        .map(|item| item.ordinal)
        .collect::<Vec<_>>();
    initial_items.sort_unstable();
    assert!(
        initial_items.windows(2).all(|pair| pair[0] != pair[1])
            && initial_items.iter().all(|ordinal| usize::from(*ordinal)
                < crate::arena_defs::arena_definitions()[fixture.setup.arena]
                    .item_anchors
                    .len()),
        "{} initial state targets a duplicate/invalid authored item",
        path.display()
    );
    let mut observed_items = fixture.observations.item_ordinals.clone();
    observed_items.sort_unstable();
    assert!(
        observed_items.windows(2).all(|pair| pair[0] != pair[1])
            && observed_items.iter().all(|ordinal| usize::from(*ordinal)
                < crate::arena_defs::arena_definitions()[fixture.setup.arena]
                    .item_anchors
                    .len()),
        "{} observes a duplicate/invalid authored item",
        path.display()
    );
}

fn validate_span(path: &Path, fighter: u8, start: u64, end: u64, duration: u64) {
    assert!(
        usize::from(fighter) < crate::network_protocol::MAX_FIGHTERS,
        "{} span has an invalid fighter",
        path.display()
    );
    assert!(
        start > 0 && start <= end && end <= duration,
        "{} span is outside its fixture duration",
        path.display()
    );
}

fn validate_edge(path: &Path, fighter: u8, tick: u64, duration: u64) {
    assert!(
        usize::from(fighter) < crate::network_protocol::MAX_FIGHTERS
            && tick > 0
            && tick <= duration,
        "{} edge is outside its fixture/fighter range",
        path.display()
    );
}

fn compile_fixture(fixture: &BehaviorFixture) -> CompiledFixture {
    let mut setup = LocalSetup::default();
    setup.arena_index = fixture.setup.arena;
    setup.rule_index = fixture.setup.rules;
    setup.replay_seed = fixture.setup.seed;

    let mut human_owners = [None; crate::network_protocol::MAX_FIGHTERS];
    for (index, fixture_slot) in fixture.setup.slots.iter().copied().enumerate() {
        let slot = &mut setup.slots[index];
        slot.participant = if fixture_slot.occupied {
            ParticipantKind::Human
        } else {
            ParticipantKind::Closed
        };
        slot.input = LocalInputAssignment::Unassigned;
        slot.character = *CHARACTER_KINDS
            .get(usize::from(fixture_slot.character))
            .expect("fixture character index was validated by production builder");
        slot.style = match fixture_slot.style {
            0 => FighterStyleKind::Anchor,
            1 => FighterStyleKind::Vector,
            2 => FighterStyleKind::Catalyst,
            value => panic!("fixture {} has invalid style {value}", fixture.name),
        };
        slot.equipment = match fixture_slot.equipment {
            0 => EquipmentKind::DashCoil,
            1 => EquipmentKind::AerialSpur,
            2 => EquipmentKind::CounterCell,
            3 => EquipmentKind::HeavySeal,
            value => panic!("fixture {} has invalid equipment {value}", fixture.name),
        };
        slot.team = match fixture_slot.team {
            0 => TeamId::Red,
            1 => TeamId::Blue,
            value => panic!("fixture {} has invalid team {value}", fixture.name),
        };
        if fixture_slot.occupied {
            human_owners[index] = Some(
                PeerId::new(10_000 + u64::from(fixture.id) * 10 + index as u64)
                    .expect("fixture peer ID is non-zero"),
            );
        }
    }

    let match_id = MatchId::new([fixture.id; 16]).expect("fixture ID is non-zero");
    let options = MatchBuildOptions {
        match_id,
        authority: AuthorityKind::Dedicated,
        trusted_results: true,
        human_owners,
        agreed_start_tick: SimTick(120),
        input_delay_ticks: DEFAULT_INPUT_DELAY_TICKS,
        rollback_limit_ticks: DEFAULT_ROLLBACK_LIMIT_TICKS,
        snapshot_history_ticks: DEFAULT_SNAPSHOT_HISTORY_TICKS,
    };
    // This production builder owns manifest hashing, compatibility, ownership,
    // loadout mapping, and snapshot-contract construction.
    let config = build_headless_match_config(&setup, options).unwrap_or_else(|error| {
        panic!(
            "fixture {} failed production config build: {error}",
            fixture.name
        )
    });

    let mut gestures: [SeatGestureTrackers; crate::network_protocol::MAX_FIGHTERS] =
        std::array::from_fn(|_| SeatGestureTrackers::default());
    let mut inputs = Vec::with_capacity(fixture.duration_ticks as usize);
    for raw_tick in 1..=fixture.duration_ticks {
        let tick = SimTick(raw_tick);
        let mut committed = CommittedTickInputs {
            tick,
            by_seat: [None; crate::network_protocol::MAX_SEATS],
        };
        for assignment in config.manifest.ownership.as_slice() {
            let fighter = assignment.fighter.index();
            let mut movement = [0_i8; 2];
            let mut held = 0_u16;
            for span in fixture
                .raw_spans
                .iter()
                .filter(|span| usize::from(span.fighter) == fighter)
                .filter(|span| span.start_tick <= raw_tick && raw_tick <= span.end_tick)
            {
                assert_eq!(
                    movement,
                    [0, 0],
                    "fixture {} overlaps raw movement spans for fighter {fighter} at tick {raw_tick}",
                    fixture.name
                );
                movement = span.movement;
                held |= span.held;
            }
            let mut pressed = 0_u16;
            let mut released = 0_u16;
            for edge in fixture
                .raw_edges
                .iter()
                .filter(|edge| usize::from(edge.fighter) == fighter && edge.tick == raw_tick)
            {
                pressed |= edge.pressed;
                released |= edge.released;
            }
            let local = TickInputFrame {
                tick: raw_tick,
                seat: LocalSeatId::new(fighter).expect("fixture fighter is a local seat"),
                sequence: InputSequence(raw_tick as u16),
                movement: QuantizedMovement::new(movement[0], movement[1]),
                held: InputMask::from_bits(held),
                pressed: InputMask::from_bits(pressed),
                released: InputMask::from_bits(released),
            };
            let mut frame = local_tick_to_network_input(local, &mut gestures[fighter]);

            let mut action_held = 0_u16;
            for span in fixture
                .action_spans
                .iter()
                .filter(|span| usize::from(span.fighter) == fighter)
                .filter(|span| span.start_tick <= raw_tick && raw_tick <= span.end_tick)
            {
                action_held |= span.held;
            }
            let mut action_pressed = 0_u16;
            let mut action_released = 0_u16;
            for edge in fixture
                .action_edges
                .iter()
                .filter(|edge| usize::from(edge.fighter) == fighter && edge.tick == raw_tick)
            {
                action_pressed |= edge.pressed;
                action_released |= edge.released;
            }
            frame.held_buttons =
                InputButtons::new(frame.held_buttons.bits() | action_held).unwrap();
            frame.pressed_buttons =
                InputButtons::new(frame.pressed_buttons.bits() | action_pressed).unwrap();
            frame.released_buttons =
                InputButtons::new(frame.released_buttons.bits() | action_released).unwrap();
            frame.sequence = NetworkInputSequence(raw_tick as u16);

            committed.by_seat[usize::from(assignment.seat.get())] = Some(AuthorityInputRecord {
                frame,
                fighter: assignment.fighter,
                origin: AuthorityInputOrigin::Peer(
                    human_owners[fighter].expect("occupied fixture fighter has an owner"),
                ),
                status: AuthorityInputStatus::Committed,
            });
        }
        inputs.push(committed);
    }

    CompiledFixture { config, inputs }
}

fn run_initial(fixture: &BehaviorFixture, compiled: &CompiledFixture) -> InitialRun {
    let driver = build_headless_simulation(compiled.config.clone()).unwrap_or_else(|error| {
        panic!(
            "fixture {} failed production bootstrap: {error}",
            fixture.name
        )
    });
    run_with_driver(fixture, compiled, driver, true)
}

fn run_with_driver(
    fixture: &BehaviorFixture,
    compiled: &CompiledFixture,
    mut driver: crate::live_authority::LiveSimulationDriver,
    capture_restore: bool,
) -> InitialRun {
    apply_initial_state(fixture, &mut driver);
    let mut ticks = Vec::with_capacity(compiled.inputs.len());
    let mut restore_snapshot = None;
    for inputs in &compiled.inputs {
        driver.step_committed(inputs).unwrap_or_else(|error| {
            panic!(
                "fixture {} failed at tick {}: {error}",
                fixture.name,
                inputs.tick.get()
            )
        });
        let snapshot = driver.capture_live_snapshot().unwrap();
        let events = driver
            .world()
            .resource::<SimEventJournal>()
            .iter_at(inputs.tick)
            .map(|event| format!("{event:?}"))
            .collect();
        ticks.push(observation(&snapshot, events, &fixture.observations));
        if capture_restore && inputs.tick.get() == fixture.restore_tick {
            restore_snapshot = Some(snapshot.clone());
        }
        if fixture.stop_on_result && snapshot.match_state.phase == MatchPhaseSnapshot::Result {
            break;
        }
    }
    InitialRun {
        driver,
        trace: RunTrace { ticks },
        restore_snapshot: restore_snapshot.expect("fixture must reach its restore boundary"),
    }
}

fn fixture_position(position_q12: [i32; 3]) -> Vec3 {
    Vec3::new(
        dequantize_f32(position_q12[0], DEFAULT_F32_QUANTIZATION),
        dequantize_f32(position_q12[1], DEFAULT_F32_QUANTIZATION),
        dequantize_f32(position_q12[2], DEFAULT_F32_QUANTIZATION),
    )
}

fn apply_initial_state(
    fixture: &BehaviorFixture,
    driver: &mut crate::live_authority::LiveSimulationDriver,
) {
    let world = driver.world_mut();
    let fighter_entities = {
        let mut query = world.query::<(Entity, &Fighter)>();
        query
            .iter(world)
            .map(|(entity, fighter)| (fighter.id, entity))
            .collect::<Vec<_>>()
    };
    for initial in &fixture.initial_state.fighters {
        let entity = fighter_entities
            .iter()
            .find_map(|(fighter, entity)| {
                (*fighter == usize::from(initial.fighter)).then_some(*entity)
            })
            .expect("validated initial fighter exists in production world");
        let position = fixture_position(initial.position_q12);
        if let Some(mut transform) = world.get_mut::<Transform>(entity) {
            transform.translation = position;
        }
        world
            .get_mut::<SimPosition>(entity)
            .expect("production fighter has a canonical position")
            .translation = position;
        if let Some(spawn_q12) = initial.spawn_q12 {
            world
                .get_mut::<Fighter>(entity)
                .expect("production fighter component remains present")
                .spawn = fixture_position(spawn_q12);
        }
        if let Some(facing_q12) = initial.facing_q12 {
            world
                .get_mut::<FighterMotor>(entity)
                .expect("production fighter motor remains present")
                .facing = fixture_position(facing_q12);
        }
    }
    for initial in &fixture.initial_state.grabs {
        let holder_id =
            FighterId::new(initial.holder).expect("validated initial grab holder exists");
        let victim_id =
            FighterId::new(initial.victim).expect("validated initial grab victim exists");
        let holder = fighter_entities
            .iter()
            .find_map(|(fighter, entity)| {
                (*fighter == usize::from(initial.holder)).then_some(*entity)
            })
            .expect("validated initial grab holder exists in production world");
        let victim = fighter_entities
            .iter()
            .find_map(|(fighter, entity)| {
                (*fighter == usize::from(initial.victim)).then_some(*entity)
            })
            .expect("validated initial grab victim exists in production world");

        *world
            .get_mut::<FighterActionState>(holder)
            .expect("production grab holder has an action state") = FighterActionState {
            action: FighterAction::GrabHold,
            elapsed: ElapsedTicks::from_ticks(initial.holder_elapsed_ticks),
            hitbox_spawned: true,
            ..Default::default()
        };
        *world
            .get_mut::<FighterGrabState>(holder)
            .expect("production grab holder has a relationship state") = FighterGrabState {
            holding: Some(victim_id),
            ..Default::default()
        };
        *world
            .get_mut::<FighterActionState>(victim)
            .expect("production grab victim has an action state") = FighterActionState {
            action: FighterAction::Grabbed,
            ..Default::default()
        };
        *world
            .get_mut::<FighterGrabState>(victim)
            .expect("production grab victim has a relationship state") = FighterGrabState {
            held_by: Some(holder_id),
            ..Default::default()
        };
    }

    let mut item_entities = {
        let mut query = world.query::<(Entity, &StableSimEntity, &ArenaItem)>();
        query
            .iter(world)
            .map(|(entity, stable, _)| (stable.id(), entity))
            .collect::<Vec<_>>()
    };
    item_entities.sort_unstable_by_key(|(stable, _)| *stable);
    for initial in &fixture.initial_state.items {
        let entity = item_entities[usize::from(initial.ordinal)].1;
        let position = fixture_position(initial.position_q12);
        world
            .get_mut::<ArenaItem>(entity)
            .expect("validated authored item remains present")
            .position = position;
        if let Some(mut transform) = world.get_mut::<Transform>(entity) {
            transform.translation = position;
        }
    }
}

fn run_clean(fixture: &BehaviorFixture, compiled: &CompiledFixture) -> RunTrace {
    run_initial(fixture, compiled).trace
}

fn run_perturbed(fixture: &BehaviorFixture, compiled: &CompiledFixture) -> RunTrace {
    let mut driver = build_headless_simulation(compiled.config.clone()).unwrap();
    driver.world_mut().spawn((
        Name::new(format!("{} presentation-only perturbation", fixture.name)),
        Transform::from_xyz(91.0, -37.0, 12.0),
    ));
    run_with_driver(fixture, compiled, driver, true).trace
}

fn replay_from_restore(
    fixture: &BehaviorFixture,
    compiled: &CompiledFixture,
    initial: &mut InitialRun,
) -> Vec<TickObservation> {
    initial
        .driver
        .restore_live_snapshot(&initial.restore_snapshot)
        .unwrap();
    assert_eq!(
        initial.driver.current_sim_tick(),
        SimTick(fixture.restore_tick)
    );
    let immediate_recapture = initial.driver.capture_live_snapshot().unwrap();
    assert_eq!(
        immediate_recapture.encode().unwrap(),
        initial.restore_snapshot.encode().unwrap(),
        "fixture {} did not restore its canonical snapshot byte-exactly",
        fixture.name
    );
    let final_tick = initial
        .trace
        .ticks
        .last()
        .expect("fixture produced at least one tick")
        .semantics
        .tick;
    let mut replayed = Vec::new();
    for inputs in compiled
        .inputs
        .iter()
        .filter(|inputs| inputs.tick.get() > fixture.restore_tick)
        .take_while(|inputs| inputs.tick.get() <= final_tick)
    {
        initial.driver.step_committed(inputs).unwrap();
        let snapshot = initial.driver.capture_live_snapshot().unwrap();
        let events = initial
            .driver
            .world()
            .resource::<SimEventJournal>()
            .iter_at(inputs.tick)
            .map(|event| format!("{event:?}"))
            .collect();
        replayed.push(observation(&snapshot, events, &fixture.observations));
    }
    replayed
}

fn observation(
    snapshot: &CanonicalSnapshot,
    events: Vec<String>,
    observations: &FixtureObservations,
) -> TickObservation {
    TickObservation {
        hash: snapshot.canonical_hash().unwrap(),
        semantics: semantic_checkpoint(snapshot, observations),
        events,
        canonical: CanonicalObservation {
            hazard_clock_ticks: snapshot.arena.hazard_clock_ticks,
            hazard_cooldowns: snapshot.arena.per_fighter_hazard_cooldowns,
            damage_by_fighter: snapshot.stats.damage_by_fighter,
            fighters: snapshot
                .fighters
                .map(|fighter| CanonicalFighterObservation {
                    dash_slide_ticks: fighter.rollback.motor.dash_slide_ticks,
                    reaction_family: fighter
                        .rollback
                        .action
                        .reaction_family
                        .present
                        .then_some(fighter.rollback.action.reaction_family.code),
                    last_attacker: fighter.relationships.last_attacker.map(FighterId::get),
                }),
        },
    }
}

fn payload_u32(payload: &[u8; crate::snapshot::DYNAMIC_PAYLOAD_BYTES], offset: usize) -> u32 {
    u32::from_le_bytes(
        payload[offset..offset + 4]
            .try_into()
            .expect("fixed item payload offset is bounded"),
    )
}

fn payload_i32(payload: &[u8; crate::snapshot::DYNAMIC_PAYLOAD_BYTES], offset: usize) -> i32 {
    i32::from_le_bytes(
        payload[offset..offset + 4]
            .try_into()
            .expect("fixed item payload offset is bounded"),
    )
}

fn payload_q12(payload: &[u8; crate::snapshot::DYNAMIC_PAYLOAD_BYTES], offset: usize) -> i32 {
    DEFAULT_F32_QUANTIZATION.quantize(f32::from_bits(payload_u32(payload, offset)))
}

fn semantic_item_kind(code: u16) -> &'static str {
    match code {
        0 => "crate",
        1 => "steamer",
        2 => "apple",
        3 => "white_wine",
        4 => "turkey",
        5 => "barrel",
        6 => "coffee",
        7 => "mushroom",
        _ => panic!("canonical item snapshot has an unknown definition ID {code}"),
    }
}

fn semantic_item_state(code: u8) -> &'static str {
    match code {
        0 => "loose",
        1 => "held",
        2 => "thrown",
        3 => "armed",
        4 => "spraying",
        5 => "rolling",
        6 => "respawning",
        _ => panic!("canonical item snapshot has an unknown state code {code}"),
    }
}

fn semantic_checkpoint(
    snapshot: &CanonicalSnapshot,
    observations: &FixtureObservations,
) -> SemanticCheckpoint {
    let fighters = snapshot
        .fighters
        .iter()
        .filter(|fighter| fighter.occupied)
        .map(|fighter| SemanticFighter {
            id: fighter.id.get(),
            active: fighter.active,
            position: [
                fighter.pose.position.x,
                fighter.pose.position.y,
                fighter.pose.position.z,
            ],
            velocity: [
                fighter.pose.velocity.x,
                fighter.pose.velocity.y,
                fighter.pose.velocity.z,
            ],
            grounded: fighter.pose.grounded,
            health: fighter.health,
            stamina: fighter.stamina,
            stocks: snapshot.match_state.stocks[fighter.id.index()],
            action_id: fighter.action.action_id,
            action_ticks: fighter.action.elapsed_ticks,
            held_item: fighter.relationships.held_item.is_some(),
            holding: fighter.relationships.holding.map(FighterId::get),
            held_by: fighter.relationships.held_by.map(FighterId::get),
            regrab_lockout_ticks: observations
                .extended_fighters
                .then_some(fighter.rollback.regrab_lockout_ticks),
            last_attacker: observations
                .extended_fighters
                .then(|| fighter.relationships.last_attacker.map(FighterId::get))
                .flatten(),
        })
        .collect();
    let items = snapshot
        .dynamic_objects
        .iter()
        .filter(|object| object.id.kind() == SimEntityKind::Item)
        .filter_map(|object| {
            let ordinal = u8::try_from(object.id.index())
                .expect("authored item pool ordinal fits the fixture schema");
            observations
                .item_ordinals
                .contains(&ordinal)
                .then(|| SemanticItem {
                    ordinal,
                    stable_index: object.id.index(),
                    generation: object.id.generation(),
                    kind: semantic_item_kind(object.definition_id).to_owned(),
                    state: semantic_item_state(object.payload[1]).to_owned(),
                    owner: object.owner.map(FighterId::get),
                    position: [
                        payload_q12(&object.payload, 4),
                        payload_q12(&object.payload, 8),
                        payload_q12(&object.payload, 12),
                    ],
                    velocity: [
                        payload_q12(&object.payload, 28),
                        payload_q12(&object.payload, 32),
                        payload_q12(&object.payload, 36),
                    ],
                    durability: payload_i32(&object.payload, 48),
                    max_durability: payload_i32(&object.payload, 52),
                    respawn_ticks: payload_u32(&object.payload, 56),
                    pickup_lockout_ticks: payload_u32(&object.payload, 60),
                    state_timer_a: payload_u32(&object.payload, 68),
                    state_timer_b: payload_u32(&object.payload, 72),
                    already_hit: object.fighter_hit_mask,
                })
        })
        .collect();
    SemanticCheckpoint {
        tick: snapshot.header.tick.get(),
        phase: match snapshot.match_state.phase {
            MatchPhaseSnapshot::Setup => "setup",
            MatchPhaseSnapshot::Countdown => "countdown",
            MatchPhaseSnapshot::Fight => "fight",
            MatchPhaseSnapshot::SuddenDeath => "sudden_death",
            MatchPhaseSnapshot::Result => "result",
            MatchPhaseSnapshot::TimeUp => "time_up",
            MatchPhaseSnapshot::Resetting => "resetting",
        }
        .to_owned(),
        hitstop_ticks: snapshot.match_state.hitstop_ticks,
        result: format!("{:?}", snapshot.match_state.result),
        dynamic_objects: snapshot.dynamic_objects.len(),
        throws: observations.match_stats.then_some(snapshot.stats.throws),
        item_hits: observations.match_stats.then_some(snapshot.stats.item_hits),
        items,
        fighters,
    }
}

fn expected_from_trace(fixture: &BehaviorFixture, trace: &RunTrace) -> FixtureExpected {
    let hashes = trace
        .ticks
        .iter()
        .map(|tick| GoldenHash {
            tick: tick.semantics.tick,
            hash: format!("{:016x}", tick.hash),
        })
        .collect();
    let checkpoints = trace
        .ticks
        .iter()
        .filter(|tick| fixture.checkpoint_ticks.contains(&tick.semantics.tick))
        .map(|tick| tick.semantics.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        checkpoints.len(),
        fixture.checkpoint_ticks.len(),
        "fixture {} did not reach every semantic checkpoint",
        fixture.name
    );
    let event_ticks = trace
        .ticks
        .iter()
        .filter(|tick| !tick.events.is_empty())
        .map(|tick| GoldenEventTick {
            tick: tick.semantics.tick,
            events: tick.events.clone(),
        })
        .collect();
    let final_semantics = &trace
        .ticks
        .last()
        .expect("fixture trace must contain a completed tick")
        .semantics;
    FixtureExpected {
        hashes,
        checkpoints,
        event_ticks,
        final_tick: final_semantics.tick,
        final_result: final_semantics.result.clone(),
    }
}

fn assert_execution_contract(
    fixture: &BehaviorFixture,
    compiled: &CompiledFixture,
) -> FixtureExpected {
    // Compile once, before any world exists. All four executions consume the
    // same immutable action frames; gesture state is never expected to restore.
    let mut initial = run_initial(fixture, compiled);
    let second = run_clean(fixture, compiled);
    assert_eq!(
        initial.trace, second,
        "fixture {} changed between clean runs in one process",
        fixture.name
    );

    let perturbed = run_perturbed(fixture, compiled);
    assert_eq!(
        initial.trace, perturbed,
        "fixture {} changed after an unrelated presentation-only entity",
        fixture.name
    );

    let expected_suffix = initial
        .trace
        .ticks
        .iter()
        .filter(|tick| tick.semantics.tick > fixture.restore_tick)
        .cloned()
        .collect::<Vec<_>>();
    let replayed = replay_from_restore(fixture, compiled, &mut initial);
    assert_eq!(
        replayed, expected_suffix,
        "fixture {} changed after same-world snapshot restore",
        fixture.name
    );

    assert_fixture_is_meaningful(fixture, compiled, &initial.trace);
    expected_from_trace(fixture, &initial.trace)
}

fn fighter_at(trace: &RunTrace, tick: usize, fighter: u8) -> &SemanticFighter {
    trace.ticks[tick]
        .semantics
        .fighters
        .iter()
        .find(|state| state.id == fighter)
        .expect("fixture semantic fighter is present")
}

fn observation_at(trace: &RunTrace, tick: u64) -> &TickObservation {
    trace
        .ticks
        .iter()
        .find(|observation| observation.semantics.tick == tick)
        .expect("fixture trace contains the requested tick")
}

fn item_at(trace: &RunTrace, tick: usize, ordinal: u8) -> &SemanticItem {
    trace.ticks[tick]
        .semantics
        .items
        .iter()
        .find(|item| item.ordinal == ordinal)
        .expect("fixture semantic item is present")
}

fn event_count(trace: &RunTrace, needle: &str) -> usize {
    trace
        .ticks
        .iter()
        .flat_map(|tick| &tick.events)
        .filter(|event| event.contains(needle))
        .count()
}

fn assert_fixture_is_meaningful(
    fixture: &BehaviorFixture,
    compiled: &CompiledFixture,
    trace: &RunTrace,
) {
    let final_index = trace.ticks.len() - 1;
    match fixture.name.as_str() {
        "BF001_move_ground_accel_stop" => {
            let first = fighter_at(trace, 0, 0);
            let final_state = fighter_at(trace, final_index, 0);
            assert_ne!(first.position, final_state.position);
            assert_eq!(final_state.velocity, [0, 0, 0]);
            assert!(final_state.grounded);
        }
        "BF002_move_air_control_land" | "BF003_jump_tap" => {
            assert!(
                trace.ticks.iter().any(|tick| !fighter_at(
                    trace,
                    tick.semantics.tick as usize - 1,
                    0
                )
                .grounded),
                "{} never left the ground",
                fixture.name
            );
            assert!(fighter_at(trace, final_index, 0).grounded);
        }
        "BF004_dash_gesture_and_motion" => {
            let dash_pulse_ticks = |seat: usize| {
                compiled
                    .inputs
                    .iter()
                    .filter_map(|inputs| {
                        let record = inputs.by_seat[seat].as_ref()?;
                        (record.frame.pressed_buttons.bits() & InputButtons::DASH != 0)
                            .then_some(inputs.tick.get())
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                dash_pulse_ticks(0),
                vec![18],
                "the inclusive 17-tick raw double-tap boundary must emit one dash"
            );
            assert_eq!(
                dash_pulse_ticks(1),
                vec![18],
                "the explicit action-level dash must match the raw gesture tick"
            );
            assert_eq!(
                compiled.inputs[0].by_seat[0]
                    .as_ref()
                    .expect("BF004 owns seat zero")
                    .frame
                    .pressed_buttons
                    .bits()
                    & InputButtons::DASH,
                0,
                "the first directional tap must only prime gesture history"
            );

            let trail_ticks = trace
                .ticks
                .iter()
                .filter_map(|tick| {
                    let count = tick
                        .events
                        .iter()
                        .filter(|event| event.contains("event: DashTrail"))
                        .count();
                    (count > 0).then_some((tick.semantics.tick, count))
                })
                .collect::<Vec<_>>();
            assert_eq!(
                trail_ticks,
                vec![(18, 2), (23, 2), (28, 2)],
                "dash trail cadence must derive from canonical action ticks"
            );

            for fighter in [0, 1] {
                let before = fighter_at(trace, 16, fighter);
                assert_eq!(before.action_id, 0);
                assert_eq!(before.velocity, [0, 0, 0]);

                for tick in 18..=29 {
                    assert_eq!(
                        fighter_at(trace, tick - 1, fighter).action_id,
                        3,
                        "fighter {fighter} left Dashing before held movement released at tick {tick}"
                    );
                }

                let released = fighter_at(trace, 29, fighter);
                assert_eq!(released.action_id, 0);
                assert_eq!(
                    observation_at(trace, 30).canonical.fighters[usize::from(fighter)]
                        .dash_slide_ticks,
                    10,
                    "the release tick must enter the authored inertial slide"
                );
                assert_eq!(
                    observation_at(trace, 34).canonical.fighters[usize::from(fighter)]
                        .dash_slide_ticks,
                    6,
                    "the restore point must sit inside the inertial slide"
                );
                assert_eq!(
                    observation_at(trace, 40).canonical.fighters[usize::from(fighter)]
                        .dash_slide_ticks,
                    0,
                    "the fixed-duration slide must expire before final recovery"
                );

                let final_state = fighter_at(trace, final_index, fighter);
                assert_eq!(final_state.action_id, 0);
                assert_eq!(final_state.velocity, [0, 0, 0]);
                assert!(final_state.grounded);
                assert!(
                    trace.ticks.iter().all(|tick| fighter_at(
                        trace,
                        tick.semantics.tick as usize - 1,
                        fighter
                    )
                    .stamina
                        == 204_800),
                    "the Preserve tape must retain the legacy zero-stamina-cost dash"
                );
            }

            let gesture_start = fighter_at(trace, 17, 0).velocity[0];
            let action_start = fighter_at(trace, 17, 1).velocity[0];
            assert!(gesture_start < 0 && action_start > 0);
            assert_eq!(gesture_start.abs(), action_start.abs());
            let gesture_release = fighter_at(trace, 29, 0).velocity[0].abs();
            let gesture_restore = fighter_at(trace, 33, 0).velocity[0].abs();
            let gesture_slide_end = fighter_at(trace, 39, 0).velocity[0].abs();
            assert!(
                gesture_start.abs() > gesture_release
                    && gesture_release > gesture_restore
                    && gesture_restore > gesture_slide_end
                    && gesture_slide_end > 0,
                "the dash impulse, release damping, slide decay, and later rest must be distinct"
            );
        }
        "BF005_light_combo" => {
            assert!(
                event_count(trace, "ActionStarted") >= 2,
                "light combo fixture did not start multiple authored actions"
            );
        }
        "BF006_heavy_charge_release" => {
            assert!(
                event_count(trace, "ActionStarted") >= 1,
                "heavy fixture did not start an authored action"
            );
            assert!(
                trace
                    .ticks
                    .iter()
                    .map(|tick| {
                        fighter_at(trace, tick.semantics.tick as usize - 1, 0).action_ticks
                    })
                    .max()
                    .unwrap()
                    >= 20,
                "heavy fixture did not sustain its authored charge/action timeline"
            );
        }
        "BF007_guard_hit" => {
            if event_count(trace, "Guarded") == 0 {
                let closest = trace
                    .ticks
                    .iter()
                    .map(|tick| {
                        let first = fighter_at(trace, tick.semantics.tick as usize - 1, 0);
                        let second = fighter_at(trace, tick.semantics.tick as usize - 1, 1);
                        let dx = i64::from(first.position[0] - second.position[0]);
                        let dz = i64::from(first.position[2] - second.position[2]);
                        (
                            dx * dx + dz * dz,
                            tick.semantics.tick,
                            first.position,
                            second.position,
                            first.action_id,
                            second.action_id,
                        )
                    })
                    .min()
                    .unwrap();
                let events = trace
                    .ticks
                    .iter()
                    .flat_map(|tick| &tick.events)
                    .filter(|event| {
                        event.contains("ActionStarted")
                            || event.contains("HitConfirmed")
                            || event.contains("Guarded")
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                panic!(
                    "guard fixture never produced a guarded contact; closest={closest:?}; \
                     combat_events={events:?}"
                );
            }
        }
        "BF008_aim_grab_short_tap" => {
            let grab_pulse_ticks = compiled
                .inputs
                .iter()
                .filter_map(|inputs| {
                    let record = inputs.by_seat[0].as_ref()?;
                    (record.frame.pressed_buttons.bits() & InputButtons::AIM_GRAB != 0)
                        .then_some(inputs.tick.get())
                })
                .collect::<Vec<_>>();
            assert_eq!(
                grab_pulse_ticks,
                vec![62],
                "short release must emit once; noise/long hold must not duplicate it"
            );
            assert!(
                trace.ticks.iter().any(|tick| {
                    fighter_at(trace, tick.semantics.tick as usize - 1, 0).holding == Some(1)
                        && fighter_at(trace, tick.semantics.tick as usize - 1, 1).held_by == Some(0)
                }),
                "aim/grab fixture never established the natural holder/victim relationship"
            );
        }
        "BF013_generic_special_variants" => {
            let tick_one = &compiled.inputs[0];
            let frames = [0, 1, 2, 3].map(|seat| {
                tick_one.by_seat[seat]
                    .as_ref()
                    .expect("generic-special fixture owns four seats")
                    .frame
            });
            assert_eq!(frames[0].pressed_buttons.bits(), InputButtons::SPECIAL);
            assert_eq!(frames[1].pressed_buttons.bits(), InputButtons::SPECIAL);
            assert_eq!(frames[1].held_buttons.bits(), InputButtons::GUARD);
            assert_eq!(
                frames[2].pressed_buttons.bits(),
                InputButtons::SPECIAL | InputButtons::AIM_GRAB
            );
            assert_eq!(
                frames[3].pressed_buttons.bits(),
                InputButtons::SPECIAL | InputButtons::HEAVY
            );
            assert!(
                event_count(trace, "AbilityLifecycle") >= 4,
                "generic-special fixture did not exercise all four spawns"
            );
        }
        "BF015_arena_hazard_contact" => {
            assert_eq!(
                fixture.restore_tick, 74,
                "BF015 must restore while hazard hitstop and cooldown are both active"
            );
            for checkpoint in [71, 72, 73, 74, 75, 76, 77] {
                assert!(
                    fixture.checkpoint_ticks.contains(&checkpoint),
                    "BF015 must checkpoint the inactive, impact, freeze, and resume boundaries"
                );
            }
            assert!(
                trace
                    .ticks
                    .iter()
                    .take(71)
                    .all(|tick| tick.events.is_empty()),
                "the phased vent must remain inactive through tick 71"
            );

            let before = observation_at(trace, 71);
            let impact = observation_at(trace, 72);
            assert_eq!(impact.events.len(), 1);
            assert_eq!(
                impact.events[0],
                "SimEvent { id: SimEventId { tick: SimTick(72), source: ArenaHazard { arena_index: 4, hazard_index: 1 }, ordinal: 0 }, kind: HitConfirmed { attacker: None, victim: FighterId(0), damage_q: 24576, reaction: LauncherDown } }",
                "the first active vent tick must emit one neutral typed hazard impact"
            );
            assert_eq!(
                event_count(trace, "kind: HitConfirmed"),
                1,
                "cooldown and displacement must suppress duplicate hazard damage"
            );

            let before_fighter = fighter_at(trace, 70, 0);
            assert_eq!(before_fighter.health, 409_600);
            assert_eq!(before_fighter.action_id, 0);
            assert_eq!(before_fighter.velocity, [0, 0, 0]);
            assert!(before_fighter.grounded);
            assert_eq!(before.canonical.hazard_clock_ticks, 71);
            assert_eq!(before.canonical.hazard_cooldowns, [0; 4]);

            let impacted = fighter_at(trace, 71, 0);
            assert_eq!(impacted.health, 385_024);
            assert_eq!(impacted.action_id, 28);
            assert_eq!(impacted.action_ticks, 0);
            assert_eq!(impacted.velocity, [0, 25_190, 0]);
            assert!(!impacted.grounded);
            assert_eq!(impact.semantics.hitstop_ticks, 5);

            for (tick, hitstop) in [(72, 5), (73, 4), (74, 3), (75, 2), (76, 1)] {
                let frozen = observation_at(trace, tick);
                assert_eq!(frozen.semantics.hitstop_ticks, hitstop);
                assert_eq!(frozen.canonical.hazard_clock_ticks, 72);
                assert_eq!(frozen.canonical.hazard_cooldowns[0], 63);
                assert_eq!(frozen.canonical.hazard_cooldowns[1..], [0; 3]);
                assert_eq!(frozen.canonical.damage_by_fighter, [0; 4]);
                assert_eq!(frozen.canonical.fighters[0].reaction_family, Some(4));
                assert_eq!(frozen.canonical.fighters[0].last_attacker, None);

                let fighter = fighter_at(trace, tick as usize - 1, 0);
                assert_eq!(
                    (
                        fighter.position,
                        fighter.velocity,
                        fighter.action_id,
                        fighter.action_ticks,
                    ),
                    (
                        impacted.position,
                        impacted.velocity,
                        impacted.action_id,
                        impacted.action_ticks,
                    ),
                    "hazard victim advanced while hitstop was active at tick {tick}"
                );
            }

            let resumed = observation_at(trace, 77);
            let resumed_fighter = fighter_at(trace, 76, 0);
            assert_eq!(resumed.semantics.hitstop_ticks, 0);
            assert_eq!(resumed.canonical.hazard_clock_ticks, 73);
            assert_eq!(resumed.canonical.hazard_cooldowns[0], 62);
            assert_eq!(resumed.canonical.damage_by_fighter, [0; 4]);
            assert_eq!(resumed_fighter.health, 385_024);
            assert_eq!(resumed_fighter.action_id, 28);
            assert_eq!(resumed_fighter.action_ticks, 1);
            assert_eq!(resumed_fighter.velocity, [0, 23_978, 0]);
            assert!(resumed_fighter.position[1] > impacted.position[1]);

            let final_observation = observation_at(trace, 90);
            let final_fighter = fighter_at(trace, 89, 0);
            assert_eq!(final_observation.canonical.hazard_clock_ticks, 86);
            assert_eq!(final_observation.canonical.hazard_cooldowns[0], 49);
            assert_eq!(final_observation.canonical.damage_by_fighter, [0; 4]);
            assert_eq!(
                final_observation.canonical.fighters[0].reaction_family,
                Some(4)
            );
            assert_eq!(final_observation.canonical.fighters[0].last_attacker, None);
            assert_eq!(final_fighter.health, 385_024);
            assert_eq!(final_fighter.action_id, 28);
            assert_eq!(final_fighter.velocity, [0, 8_222, 0]);
        }
        "BF021_last_stock_match_completion" => {
            assert_eq!(
                trace.ticks[final_index].semantics.phase, "result",
                "last-stock fixture did not finish through normal rules"
            );
            assert_ne!(
                trace.ticks[final_index].semantics.result, "Pending",
                "last-stock fixture reached Results without a canonical result"
            );
            assert!(
                event_count(trace, "StockLost") > 0,
                "last-stock fixture did not cross the stock-loss path"
            );
            assert!(
                event_count(trace, "MatchLifecycle { event: Results }") == 1,
                "last-stock fixture must publish exactly one Results transition"
            );
        }
        "BF023_hitstop_decrement_boundary" => {
            assert_eq!(
                fixture.restore_tick, 70,
                "BF023 must restore from inside active hitstop"
            );
            for checkpoint in 68..=72 {
                assert!(
                    fixture.checkpoint_ticks.contains(&checkpoint),
                    "BF023 must checkpoint every impact/freeze/resume boundary tick"
                );
            }

            let impact = observation_at(trace, 68);
            let frozen_one = observation_at(trace, 69);
            let frozen_two = observation_at(trace, 70);
            let frozen_three = observation_at(trace, 71);
            let resumed = observation_at(trace, 72);
            assert_eq!(
                [
                    impact.semantics.hitstop_ticks,
                    frozen_one.semantics.hitstop_ticks,
                    frozen_two.semantics.hitstop_ticks,
                    frozen_three.semantics.hitstop_ticks,
                    resumed.semantics.hitstop_ticks,
                ],
                [4, 3, 2, 1, 0],
                "BF023 must lock the trigger and exact post-decrement resume boundary"
            );

            let hit_events = impact
                .events
                .iter()
                .filter(|event| event.contains("HitConfirmed"))
                .map(String::as_str)
                .collect::<Vec<_>>();
            assert_eq!(
                hit_events,
                vec![
                    "SimEvent { id: SimEventId { tick: SimTick(68), source: Entity(SimEntityId { kind: Hitbox, index: 1, generation: 1 }), ordinal: 4 }, kind: HitConfirmed { attacker: Some(FighterId(1)), victim: FighterId(0), damage_q: 32768, reaction: ShortStandingStagger } }",
                    "SimEvent { id: SimEventId { tick: SimTick(68), source: Entity(SimEntityId { kind: Hitbox, index: 0, generation: 1 }), ordinal: 5 }, kind: HitConfirmed { attacker: Some(FighterId(0)), victim: FighterId(1), damage_q: 32768, reaction: ShortStandingStagger } }",
                ],
                "BF023 impact IDs and arbitration order are part of the rollback contract"
            );
            assert_eq!(
                event_count(trace, "HitConfirmed"),
                2,
                "frozen and resumed ticks must not duplicate the impact"
            );

            for fighter in [0, 1] {
                let at_impact = fighter_at(trace, 67, fighter);
                for tick in 68..=70 {
                    let frozen = fighter_at(trace, tick, fighter);
                    assert_eq!(
                        (
                            frozen.position,
                            frozen.velocity,
                            frozen.action_id,
                            frozen.action_ticks,
                        ),
                        (
                            at_impact.position,
                            at_impact.velocity,
                            at_impact.action_id,
                            at_impact.action_ticks,
                        ),
                        "fighter {fighter} advanced while BF023 hitstop was active at tick {}",
                        tick + 1
                    );
                }
                let after_resume = fighter_at(trace, 71, fighter);
                assert_eq!(after_resume.position, at_impact.position);
                assert_eq!(after_resume.action_id, at_impact.action_id);
                assert_eq!(
                    after_resume.action_ticks,
                    at_impact.action_ticks + 1,
                    "the post-decrement-zero tick must advance fighter action time once"
                );
            }
        }
        "BF024_contested_item_pickup" => {
            let pickup_input_tick = compiled
                .inputs
                .iter()
                .find(|inputs| {
                    (0..=1).all(|seat| {
                        inputs.by_seat[seat]
                            .as_ref()
                            .expect("contested-item fixture owns both seats")
                            .frame
                            .pressed_buttons
                            .bits()
                            & InputButtons::LIGHT
                            != 0
                    })
                })
                .expect("both contenders compile the same delayed pickup pulse")
                .tick;
            assert_eq!(
                event_count(trace, "event: PickedUp"),
                1,
                "exactly one fighter may win a contested item pickup"
            );
            let pickup_index = pickup_input_tick.get() as usize - 1;
            assert!(
                fighter_at(trace, pickup_index, 0).held_item
                    && !fighter_at(trace, pickup_index, 1).held_item,
                "canonical fighter order must award the contested item to fighter zero"
            );
        }
        "BF025_simultaneous_respawn_space_conflict" => {
            let respawn_ticks = trace
                .ticks
                .iter()
                .filter_map(|tick| {
                    let count = tick
                        .events
                        .iter()
                        .filter(|event| event.contains("FighterRespawned"))
                        .count();
                    (count > 0).then_some((tick.semantics.tick, count))
                })
                .collect::<Vec<_>>();
            assert_eq!(
                respawn_ticks.len(),
                1,
                "simultaneous life loss must produce one shared respawn tick"
            );
            assert_eq!(
                respawn_ticks[0].1, 2,
                "both non-eliminated fighters must respawn on that tick"
            );
            let respawn_index = respawn_ticks[0].0 as usize - 1;
            assert_eq!(
                fighter_at(trace, respawn_index, 0).position,
                fighter_at(trace, respawn_index, 1).position,
                "the fixture must exercise the exact shared-spawn conflict"
            );
            assert_ne!(
                fighter_at(trace, final_index, 0).position,
                fighter_at(trace, final_index, 1).position,
                "canonical body separation must resolve the shared spawn deterministically"
            );
        }
        "BF026_grab_escape_lockout_timeout" => {
            let grab_pulse_ticks = compiled
                .inputs
                .iter()
                .filter_map(|inputs| {
                    let record = inputs.by_seat[0].as_ref()?;
                    (record.frame.pressed_buttons.bits() & InputButtons::AIM_GRAB != 0)
                        .then_some(inputs.tick.get())
                })
                .collect::<Vec<_>>();
            assert_eq!(grab_pulse_ticks, vec![13, 55]);

            let before_escape = &trace.ticks[10];
            assert_eq!(fighter_at(trace, 10, 0).holding, Some(1));
            assert_eq!(fighter_at(trace, 10, 1).held_by, Some(0));
            assert_eq!(fighter_at(trace, 10, 0).action_ticks, 11);
            assert_eq!(before_escape.semantics.throws, Some(0));

            let escaped = &trace.ticks[11];
            assert_eq!(fighter_at(trace, 11, 0).holding, None);
            assert_eq!(fighter_at(trace, 11, 1).held_by, None);
            assert_eq!(fighter_at(trace, 11, 0).action_id, 0);
            assert_eq!(fighter_at(trace, 11, 1).action_id, 0);
            assert_eq!(fighter_at(trace, 11, 1).regrab_lockout_ticks, Some(26));
            assert_eq!(escaped.semantics.throws, Some(0));

            assert!(
                trace.ticks[23]
                    .events
                    .iter()
                    .any(|event| event.contains("kind: Hitbox")),
                "the in-lockout regrab probe must spawn its real grab hitbox"
            );
            assert_eq!(fighter_at(trace, 23, 0).holding, None);
            assert_eq!(fighter_at(trace, 23, 1).held_by, None);
            assert_eq!(fighter_at(trace, 23, 1).regrab_lockout_ticks, Some(14));
            assert_eq!(fighter_at(trace, 36, 1).regrab_lockout_ticks, Some(1));
            assert_eq!(fighter_at(trace, 37, 1).regrab_lockout_ticks, Some(0));

            assert_eq!(fighter_at(trace, 65, 0).holding, Some(1));
            assert_eq!(fighter_at(trace, 65, 1).held_by, Some(0));
            assert_eq!(fighter_at(trace, 65, 0).action_id, 17);
            assert_eq!(fighter_at(trace, 65, 1).action_id, 18);
            assert_eq!(fighter_at(trace, 65, 1).last_attacker, Some(0));
            assert_eq!(fighter_at(trace, 103, 0).action_ticks, 38);

            let timeout = &trace.ticks[104];
            assert_eq!(timeout.semantics.throws, Some(1));
            assert_eq!(fighter_at(trace, 104, 0).holding, None);
            assert_eq!(fighter_at(trace, 104, 1).held_by, None);
            assert_eq!(fighter_at(trace, 104, 0).action_id, 19);
            assert_eq!(fighter_at(trace, 104, 1).action_id, 29);
            assert_eq!(fighter_at(trace, 104, 1).health, 385_024);
            assert_eq!(fighter_at(trace, 104, 1).velocity, [36_870, 0, 0]);
            assert_eq!(fighter_at(trace, 104, 1).regrab_lockout_ticks, Some(51));
            assert_eq!(fighter_at(trace, 104, 1).last_attacker, Some(0));
            assert_eq!(event_count(trace, "kind: HitConfirmed"), 1);
            assert!(timeout.events[0].contains(
                "attacker: Some(FighterId(0)), victim: FighterId(1), damage_q: 24576, \
                 reaction: SlidingKnockdown"
            ));
        }
        "BF027_quick_directional_heavy_throw" => {
            let resolved = &trace.ticks[0];
            assert_eq!(resolved.semantics.throws, Some(2));
            assert_eq!(resolved.semantics.hitstop_ticks, 6);
            assert_eq!(resolved.events.len(), 2);
            assert!(resolved.events[0].contains("source: Fighter(FighterId(0)), ordinal: 0"));
            assert!(
                resolved.events[0]
                    .contains("victim: FighterId(1), damage_q: 12288, reaction: LightAirPop")
            );
            assert!(resolved.events[1].contains("source: Fighter(FighterId(2)), ordinal: 1"));
            assert!(
                resolved.events[1]
                    .contains("victim: FighterId(3), damage_q: 32768, reaction: SlidingKnockdown")
            );

            for holder in [0, 2] {
                assert_eq!(fighter_at(trace, 0, holder).action_id, 19);
                assert_eq!(fighter_at(trace, 0, holder).holding, None);
            }
            assert_eq!(fighter_at(trace, 0, 1).held_by, None);
            assert_eq!(fighter_at(trace, 0, 3).held_by, None);
            assert_eq!(fighter_at(trace, 0, 1).health, 397_312);
            assert_eq!(fighter_at(trace, 0, 1).velocity, [24_767, 10_617, 0]);
            assert_eq!(fighter_at(trace, 0, 1).action_id, 28);
            assert_eq!(fighter_at(trace, 0, 1).last_attacker, Some(0));
            assert_eq!(fighter_at(trace, 0, 3).health, 376_832);
            assert_eq!(fighter_at(trace, 0, 3).velocity, [0, 0, 43_893]);
            assert_eq!(fighter_at(trace, 0, 3).action_id, 29);
            assert_eq!(fighter_at(trace, 0, 3).last_attacker, Some(2));
            assert_eq!(fighter_at(trace, 0, 1).regrab_lockout_ticks, Some(51));
            assert_eq!(fighter_at(trace, 0, 3).regrab_lockout_ticks, Some(51));
            assert_eq!(fighter_at(trace, 7, 1).regrab_lockout_ticks, Some(49));
            assert_eq!(fighter_at(trace, 7, 3).regrab_lockout_ticks, Some(49));
        }
        "BF028_item_use_throw_impact_respawn" => {
            assert_eq!(event_count(trace, "event: PickedUp"), 2);
            assert_eq!(event_count(trace, "event: Used"), 1);
            assert_eq!(event_count(trace, "event: Thrown"), 1);
            assert_eq!(event_count(trace, "event: Broken"), 0);
            assert_eq!(event_count(trace, "kind: HitConfirmed"), 1);

            let picked_up = &trace.ticks[0];
            assert!(fighter_at(trace, 0, 0).held_item);
            assert!(fighter_at(trace, 0, 1).held_item);
            assert_eq!(item_at(trace, 0, 0).state, "held");
            assert_eq!(item_at(trace, 0, 0).owner, Some(0));
            assert_eq!(item_at(trace, 0, 2).state, "held");
            assert_eq!(item_at(trace, 0, 2).owner, Some(1));
            assert_eq!(picked_up.semantics.item_hits, Some(0));

            assert_eq!(fighter_at(trace, 14, 0).action_id, 0);
            assert_eq!(fighter_at(trace, 14, 1).action_id, 0);
            let resolved = &trace.ticks[15];
            assert_eq!(resolved.semantics.item_hits, Some(1));
            assert_eq!(resolved.semantics.hitstop_ticks, 6);
            assert_eq!(fighter_at(trace, 15, 0).action_id, 22);
            assert_eq!(fighter_at(trace, 15, 1).action_id, 23);
            assert_eq!(fighter_at(trace, 15, 2).health, 356_352);
            assert_eq!(fighter_at(trace, 15, 2).last_attacker, Some(1));

            let apple = item_at(trace, 15, 0);
            assert_eq!(apple.kind, "apple");
            assert_eq!(apple.state, "respawning");
            assert_eq!((apple.durability, apple.max_durability), (0, 1));
            assert_eq!(apple.respawn_ticks, 599);
            assert_eq!(apple.owner, None);

            let turkey = item_at(trace, 15, 2);
            assert_eq!(turkey.kind, "turkey");
            assert_eq!(turkey.state, "loose");
            assert_eq!((turkey.durability, turkey.max_durability), (2, 3));
            assert_eq!(turkey.owner, None);
            assert_eq!(turkey.already_hit, 0);

            assert_eq!(resolved.events.len(), 3);
            assert!(resolved.events[0].contains("event: Used"));
            assert!(resolved.events[1].contains("event: Thrown"));
            assert!(
                resolved.events[2].contains(
                    "attacker: Some(FighterId(1)), victim: FighterId(2), damage_q: 53248"
                )
            );

            assert_eq!(item_at(trace, 612, 0).respawn_ticks, 2);
            assert_eq!(item_at(trace, 613, 0).respawn_ticks, 1);
            let respawned = item_at(trace, 614, 0);
            assert_eq!(respawned.state, "loose");
            assert_eq!((respawned.durability, respawned.max_durability), (1, 1));
            assert_eq!(respawned.respawn_ticks, 0);
            assert_eq!(respawned.position, [-21_914, 3_809, 0]);
        }
        name => panic!("fixture {name} has no semantic coverage assertion"),
    }
}

fn load_fixture_named(name: &str) -> BehaviorFixture {
    fixture_paths()
        .into_iter()
        .map(|path| load_fixture(&path))
        .find(|fixture| fixture.name == name)
        .unwrap_or_else(|| panic!("missing behavior fixture {name}"))
}

fn vec3_bits(value: Vec3) -> [u32; 3] {
    [value.x.to_bits(), value.y.to_bits(), value.z.to_bits()]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HitboxPhaseProbe {
    id: SimEntityId,
    owner: FighterId,
    position: [u32; 3],
    lifetime_ticks: u32,
    elapsed_ticks: u32,
    already_hit: u8,
}

fn hitbox_phase_probes(world: &mut World) -> Vec<HitboxPhaseProbe> {
    let mut query = world.query::<(&StableSimEntity, &Hitbox, &SimPosition)>();
    let mut probes = query
        .iter(world)
        .map(|(stable, hitbox, position)| HitboxPhaseProbe {
            id: stable.id(),
            owner: hitbox.owner,
            position: vec3_bits(position.translation),
            lifetime_ticks: hitbox.lifetime.remaining(),
            elapsed_ticks: hitbox.elapsed.get(),
            already_hit: hitbox.already_hit.bits(),
        })
        .collect::<Vec<_>>();
    probes.sort_unstable_by_key(|probe| probe.id);
    probes
}

fn hitbox_phase_probe(world: &mut World, id: SimEntityId) -> HitboxPhaseProbe {
    hitbox_phase_probes(world)
        .into_iter()
        .find(|probe| probe.id == id)
        .expect("stable hitbox remains present")
}

fn assert_same_hitbox_phase(before: HitboxPhaseProbe, after: HitboxPhaseProbe) {
    assert_eq!(after.id, before.id);
    assert_eq!(after.owner, before.owner);
    assert_eq!(after.lifetime_ticks, before.lifetime_ticks);
    assert_eq!(after.elapsed_ticks, before.elapsed_ticks);
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct HitstopTickProbe {
    tick: SimTick,
    remaining_ticks: u32,
    hitboxes: Vec<HitboxPhaseProbe>,
    events: Vec<SimEvent>,
    hash: u64,
}

fn capture_hitstop_tick(driver: &mut LiveSimulationDriver) -> HitstopTickProbe {
    let tick = driver.current_sim_tick();
    let hitboxes = hitbox_phase_probes(driver.world_mut());
    let remaining_ticks = driver.world().resource::<Hitstop>().remaining_ticks;
    let events = driver
        .world()
        .resource::<SimEventJournal>()
        .iter_at(tick)
        .copied()
        .collect();
    let hash = driver.state_hash().expect("hitstop probe state hashes");
    HitstopTickProbe {
        tick,
        remaining_ticks,
        hitboxes,
        events,
        hash,
    }
}

#[test]
fn bf023_restores_inside_hitstop_and_resumes_hitboxes_exactly_once() {
    let fixture = load_fixture_named("BF023_hitstop_decrement_boundary");
    assert_eq!(fixture.restore_tick, 70);
    assert!((68..=72).all(|tick| fixture.checkpoint_ticks.contains(&tick)));
    let compiled = compile_fixture(&fixture);

    // This exercises the ordinary clean/perturbed/same-world restore contract
    // and the exact semantic assertions in the BF023 branch without consulting
    // or rewriting the checked-in golden.
    let _ = assert_execution_contract(&fixture, &compiled);

    let mut driver =
        build_headless_simulation(compiled.config.clone()).expect("BF023 production world builds");
    apply_initial_state(&fixture, &mut driver);
    let mut boundary = Vec::new();
    let mut restore_snapshot = None;
    for inputs in compiled.inputs.iter().take(72) {
        driver
            .step_committed(inputs)
            .expect("BF023 production schedule advances");
        if (68..=72).contains(&inputs.tick.get()) {
            boundary.push(capture_hitstop_tick(&mut driver));
        }
        if inputs.tick.get() == fixture.restore_tick {
            restore_snapshot = Some(
                driver
                    .capture_live_snapshot()
                    .expect("BF023 captures an active-hitstop restore point"),
            );
        }
    }

    assert_eq!(
        boundary
            .iter()
            .map(|tick| tick.remaining_ticks)
            .collect::<Vec<_>>(),
        vec![4, 3, 2, 1, 0]
    );
    assert_eq!(boundary[0].hitboxes.len(), 2);
    for frozen in &boundary[1..4] {
        assert_eq!(frozen.hitboxes.len(), boundary[0].hitboxes.len());
        for (impact, frozen) in boundary[0].hitboxes.iter().zip(&frozen.hitboxes) {
            assert_same_hitbox_phase(*impact, *frozen);
            assert_eq!(frozen.already_hit, impact.already_hit);
        }
    }
    assert_eq!(boundary[4].hitboxes.len(), boundary[0].hitboxes.len());
    for (frozen, resumed) in boundary[0].hitboxes.iter().zip(&boundary[4].hitboxes) {
        assert_eq!(resumed.id, frozen.id);
        assert_eq!(resumed.owner, frozen.owner);
        assert_eq!(resumed.already_hit, frozen.already_hit);
        assert_eq!(resumed.elapsed_ticks, frozen.elapsed_ticks + 1);
        assert_eq!(
            resumed.lifetime_ticks,
            frozen.lifetime_ticks.saturating_sub(1)
        );
    }

    let expected_suffix = boundary[3..].to_vec();
    driver
        .restore_live_snapshot(&restore_snapshot.expect("tick 70 snapshot was captured"))
        .expect("active-hitstop snapshot restores");
    let mut replayed_suffix = Vec::new();
    for inputs in compiled
        .inputs
        .iter()
        .filter(|inputs| inputs.tick.get() > fixture.restore_tick)
        .take(2)
    {
        driver
            .step_committed(inputs)
            .expect("restored BF023 suffix advances");
        replayed_suffix.push(capture_hitstop_tick(&mut driver));
    }
    assert_eq!(
        replayed_suffix, expected_suffix,
        "restore from remaining=2 must reproduce the frozen tick and resume tick byte-exactly"
    );
}

#[derive(Resource, Default)]
struct ActionHitstopTriggerProbe {
    count: u32,
    tick: Option<SimTick>,
}

fn trigger_hitstop_at_action_end(
    tick: Res<SimTick>,
    mut hitstop: ResMut<Hitstop>,
    mut probe: ResMut<ActionHitstopTriggerProbe>,
) {
    if *tick != SimTick(1) {
        return;
    }
    assert_eq!(
        hitstop.remaining_ticks, 0,
        "Match-phase hitstop processing must precede the Action trigger"
    );
    hitstop.remaining_ticks = 3;
    probe.count += 1;
    probe.tick = Some(*tick);
}

fn build_action_trigger_driver(config: &HeadlessMatchConfig) -> LiveSimulationDriver {
    config
        .validate()
        .expect("instrumented config passes production validation");
    let mut app = App::new();
    super::configure_canonical_fixed_schedule(&mut app);
    super::initialize_headless_resources(&mut app, config)
        .expect("instrumented production resources initialize");
    super::bootstrap_canonical_world(&mut app, config)
        .expect("instrumented production world bootstraps");
    app.init_resource::<ActionHitstopTriggerProbe>()
        .add_systems(
            FixedUpdate,
            trigger_hitstop_at_action_end
                .in_set(SimulationSet::Action)
                .after(crate::items::spawn_item_hitboxes),
        );
    LiveSimulationDriver::new(app, config.manifest.ownership)
        .expect("instrumented production driver builds")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ItemMotionProbe {
    id: SimEntityId,
    position: [u32; 3],
    velocity: [u32; 3],
    lifetime_ticks: u32,
    state_age_ticks: u32,
}

fn item_motion_probe(world: &mut World, id: SimEntityId) -> ItemMotionProbe {
    let mut query = world.query::<(&StableSimEntity, &ArenaItem)>();
    query
        .iter(world)
        .find_map(|(stable, item)| {
            if stable.id() != id {
                return None;
            }
            let ItemState::Rolling { lifetime } = item.state else {
                panic!("mid-step probe item must remain in rolling motion");
            };
            Some(ItemMotionProbe {
                id,
                position: vec3_bits(item.position),
                velocity: vec3_bits(item.velocity),
                lifetime_ticks: lifetime.remaining(),
                state_age_ticks: item.state_age.get(),
            })
        })
        .expect("mid-step probe item remains present")
}

fn fighter_position(world: &mut World, fighter_id: usize) -> Vec3 {
    let mut query = world.query::<(&Fighter, &SimPosition)>();
    query
        .iter(world)
        .find_map(|(fighter, position)| (fighter.id == fighter_id).then_some(position.translation))
        .expect("probe fighter remains present")
}

fn configure_motion_probes(
    driver: &mut LiveSimulationDriver,
) -> (Vec3, SimEntityId, ItemMotionProbe) {
    let world = driver.world_mut();
    let fighter_entity = {
        let mut query = world.query::<(Entity, &Fighter)>();
        query
            .iter(world)
            .find_map(|(entity, fighter)| (fighter.id == 0).then_some(entity))
            .expect("probe fighter zero exists")
    };
    world
        .get_mut::<FighterMotor>(fighter_entity)
        .expect("probe fighter has a motor")
        .velocity = Vec3::new(4.0, 0.0, 0.0);
    let fighter_before = world
        .get::<SimPosition>(fighter_entity)
        .expect("probe fighter has a position")
        .translation;

    let (item_id, item_entity) = {
        let mut query = world.query::<(Entity, &StableSimEntity, &ArenaItem)>();
        query
            .iter(world)
            .map(|(entity, stable, _)| (stable.id(), entity))
            .min_by_key(|(id, _)| *id)
            .expect("authored arena provides a probe item")
    };
    {
        let mut item = world
            .get_mut::<ArenaItem>(item_entity)
            .expect("probe item remains present");
        item.state = ItemState::Rolling {
            lifetime: TickTimer::from_ticks(30),
        };
        item.respawn_timer.clear();
        item.pickup_lockout.clear();
        item.position = Vec3::new(-4.0, 2.0, -4.0);
        item.velocity = Vec3::new(3.0, 0.0, 0.0);
        item.already_hit.clear();
        item.base_y = 2.0;
        item.state_age.reset();
    }
    let item_before = item_motion_probe(world, item_id);
    (fighter_before, item_id, item_before)
}

#[test]
fn action_phase_hitstop_trigger_freezes_movement_and_later_item_motion_same_step() {
    let fixture = load_fixture_named("BF001_move_ground_accel_stop");
    let compiled = compile_fixture(&fixture);
    let input = &compiled.inputs[0];

    let mut control = build_headless_simulation(compiled.config.clone())
        .expect("control production world builds");
    let (control_fighter_before, control_item_id, control_item_before) =
        configure_motion_probes(&mut control);
    control
        .step_committed(input)
        .expect("control production schedule advances");
    assert_ne!(
        fighter_position(control.world_mut(), 0),
        control_fighter_before,
        "control movement must prove the fighter setup is live"
    );
    assert_ne!(
        item_motion_probe(control.world_mut(), control_item_id),
        control_item_before,
        "control Items phase must prove the rolling item setup is live"
    );

    let mut triggered = build_action_trigger_driver(&compiled.config);
    let (fighter_before, item_id, item_before) = configure_motion_probes(&mut triggered);
    triggered
        .step_committed(input)
        .expect("instrumented production schedule advances");

    assert_eq!(triggered.current_sim_tick(), SimTick(1));
    assert_eq!(
        triggered.world().resource::<Hitstop>().remaining_ticks,
        3,
        "an Action trigger is visible to every later phase in the same step"
    );
    let trigger_probe = triggered.world().resource::<ActionHitstopTriggerProbe>();
    assert_eq!(
        (trigger_probe.count, trigger_probe.tick),
        (1, Some(SimTick(1)))
    );
    assert_eq!(
        fighter_position(triggered.world_mut(), 0),
        fighter_before,
        "Movement must observe hitstop triggered at the end of Action"
    );
    assert_eq!(
        item_motion_probe(triggered.world_mut(), item_id),
        item_before,
        "moving-item lifetime, age, velocity, and pose must freeze later in the same step"
    );
}

fn fighter_entity(world: &mut World, fighter_id: usize) -> Entity {
    let mut query = world.query::<(Entity, &Fighter)>();
    query
        .iter(world)
        .find_map(|(entity, fighter)| (fighter.id == fighter_id).then_some(entity))
        .expect("requested fighter remains present")
}

fn fighter_health(world: &mut World, fighter_id: usize) -> f32 {
    let entity = fighter_entity(world, fighter_id);
    world
        .get::<FighterStats>(entity)
        .expect("requested fighter has stats")
        .health
}

fn hit_events_for_victim(
    driver: &LiveSimulationDriver,
    tick: SimTick,
    victim: FighterId,
) -> Vec<SimEvent> {
    driver
        .world()
        .resource::<SimEventJournal>()
        .iter_at(tick)
        .filter(|event| {
            matches!(
                event.kind,
                SimEventKind::HitConfirmed {
                    victim: event_victim,
                    ..
                } if event_victim == victim
            )
        })
        .copied()
        .collect()
}

#[test]
fn pre_existing_hitbox_contacts_new_target_while_phase_remains_frozen() {
    let mut fixture = load_fixture_named("BF023_hitstop_decrement_boundary");
    fixture.setup.slots[2].occupied = true;
    let compiled = compile_fixture(&fixture);
    let mut driver = build_headless_simulation(compiled.config.clone())
        .expect("contact production world builds");
    apply_initial_state(&fixture, &mut driver);
    for inputs in compiled.inputs.iter().take(68) {
        driver
            .step_committed(inputs)
            .expect("natural BF023 impact setup advances");
    }

    let target = FighterId::new(2).expect("fixture target is in range");
    let prior_victim_position = {
        let entity = fighter_entity(driver.world_mut(), 1);
        driver
            .world()
            .get::<SimPosition>(entity)
            .expect("prior victim has a canonical position")
            .translation
    };
    {
        let prior_victim = fighter_entity(driver.world_mut(), 1);
        driver
            .world_mut()
            .get_mut::<SimPosition>(prior_victim)
            .expect("prior victim has a canonical position")
            .translation += Vec3::Z * 4.0;
        let new_target = fighter_entity(driver.world_mut(), target.index());
        driver
            .world_mut()
            .get_mut::<SimPosition>(new_target)
            .expect("new target has a canonical position")
            .translation = prior_victim_position;
        driver
            .world_mut()
            .get_mut::<FighterMotor>(new_target)
            .expect("new target has a motor")
            .velocity = Vec3::ZERO;
        let mut stats = driver
            .world_mut()
            .get_mut::<FighterStats>(new_target)
            .expect("new target has stats");
        stats.invulnerability.clear();
    }

    let primary_id = {
        let world = driver.world_mut();
        let mut query = world.query::<(&StableSimEntity, &mut Hitbox)>();
        let mut primary = None;
        for (stable, mut hitbox) in query.iter_mut(world) {
            if hitbox.owner == FighterId::ZERO {
                assert!(!hitbox.already_hit.contains(target));
                primary = Some(stable.id());
            } else {
                hitbox.already_hit.insert(target);
            }
        }
        primary.expect("natural BF023 owner-zero hitbox remains active")
    };
    driver.world_mut().resource_mut::<Hitstop>().remaining_ticks = 2;
    let before = hitbox_phase_probe(driver.world_mut(), primary_id);
    let health_before = fighter_health(driver.world_mut(), target.index());

    driver
        .step_committed(&compiled.inputs[68])
        .expect("active-hitstop contact tick advances");
    let after_contact = hitbox_phase_probe(driver.world_mut(), primary_id);
    assert_same_hitbox_phase(before, after_contact);
    assert_eq!(before.already_hit & (1 << target.get()), 0);
    assert!(after_contact.already_hit & (1 << target.get()) != 0);
    assert_eq!(
        driver.world().resource::<Hitstop>().remaining_ticks,
        4,
        "the accepted contact may extend hitstop by maximum, never by addition"
    );

    let contact_events = hit_events_for_victim(&driver, SimTick(69), target);
    assert_eq!(contact_events.len(), 1);
    assert_eq!(contact_events[0].id.tick, SimTick(69));
    assert_eq!(
        contact_events[0].id.source,
        SimEventSource::Entity(primary_id)
    );
    assert_eq!(contact_events[0].id.ordinal, 0);
    assert!(
        matches!(
            contact_events[0].kind,
            SimEventKind::HitConfirmed {
                attacker: Some(FighterId::ZERO),
                victim,
                damage_q: 24_576,
                ..
            } if victim == target
        ),
        "unexpected contact event: {:?}",
        contact_events[0]
    );
    let health_after = fighter_health(driver.world_mut(), target.index());
    assert_eq!(health_before - health_after, 6.0);

    driver
        .step_committed(&compiled.inputs[69])
        .expect("deduplication tick advances");
    let after_dedup = hitbox_phase_probe(driver.world_mut(), primary_id);
    assert_same_hitbox_phase(after_contact, after_dedup);
    assert_eq!(after_dedup.already_hit, after_contact.already_hit);
    assert_eq!(driver.world().resource::<Hitstop>().remaining_ticks, 3);
    assert!(
        hit_events_for_victim(&driver, SimTick(70), target).is_empty(),
        "the frozen existing hitbox must not hit the same target twice"
    );
    assert_eq!(
        fighter_health(driver.world_mut(), target.index()),
        health_after
    );
}

fn report_golden_change(path: &Path, previous: Option<&FixtureExpected>, next: &FixtureExpected) {
    match previous {
        None => eprintln!(
            "{}: created {} per-tick hashes, {} checkpoints, {} event ticks; final tick {}",
            path.display(),
            next.hashes.len(),
            next.checkpoints.len(),
            next.event_ticks.len(),
            next.final_tick
        ),
        Some(previous) if previous == next => {
            eprintln!("{}: unchanged", path.display());
        }
        Some(previous) => {
            let first_hash = previous
                .hashes
                .iter()
                .zip(&next.hashes)
                .find(|(before, after)| before != after)
                .map(|(before, after)| {
                    format!(
                        "tick {} hash {} -> {}",
                        before.tick, before.hash, after.hash
                    )
                })
                .unwrap_or_else(|| {
                    format!(
                        "hash length {} -> {}",
                        previous.hashes.len(),
                        next.hashes.len()
                    )
                });
            eprintln!(
                "{}: changed; {}; checkpoints {} -> {}; event ticks {} -> {}; final tick {} -> {}; result {} -> {}",
                path.display(),
                first_hash,
                previous.checkpoints.len(),
                next.checkpoints.len(),
                previous.event_ticks.len(),
                next.event_ticks.len(),
                previous.final_tick,
                next.final_tick,
                previous.final_result,
                next.final_result
            );
        }
    }
}

#[test]
fn checked_in_behavior_fixtures_match_production_simulation() {
    for path in fixture_paths() {
        let fixture = load_fixture(&path);
        let compiled = compile_fixture(&fixture);
        let observed = assert_execution_contract(&fixture, &compiled);
        let expected = fixture.expected.as_ref().unwrap_or_else(|| {
            panic!(
                "{} has no golden; run the explicit ignored updater",
                path.display()
            )
        });
        assert_eq!(
            &observed, expected,
            "behavior fixture {} differs from its checked-in golden",
            fixture.name
        );
    }
}

#[test]
fn checked_in_behavior_fixture_semantics_match_production_simulation() {
    for path in fixture_paths() {
        let fixture = load_fixture(&path);
        let Some(expected) = fixture.expected.as_ref() else {
            continue;
        };
        let compiled = compile_fixture(&fixture);
        let observed = assert_execution_contract(&fixture, &compiled);
        assert_eq!(
            observed.checkpoints, expected.checkpoints,
            "behavior fixture {} changed semantic checkpoints",
            fixture.name
        );
        assert_eq!(
            observed.event_ticks, expected.event_ticks,
            "behavior fixture {} changed semantic events",
            fixture.name
        );
        assert_eq!(
            (observed.final_tick, &observed.final_result),
            (expected.final_tick, &expected.final_result),
            "behavior fixture {} changed its final semantic outcome",
            fixture.name
        );
    }
}

#[test]
fn contested_behavior_fixtures_execute_semantic_contracts() {
    for path in fixture_paths() {
        let fixture = load_fixture(&path);
        if !matches!(
            fixture.name.as_str(),
            "BF024_contested_item_pickup" | "BF025_simultaneous_respawn_space_conflict"
        ) {
            continue;
        }
        let compiled = compile_fixture(&fixture);
        let _ = assert_execution_contract(&fixture, &compiled);
    }
}

fn mystery_crate_reward_from_production(seed: u64) -> u16 {
    let fixture = BehaviorFixture {
        schema_version: FIXTURE_SCHEMA_VERSION,
        contract_version: CONTRACT_VERSION,
        id: 29,
        name: "mystery_crate_seed_probe".to_owned(),
        classification: FixtureClassification::Preserve,
        setup: FixtureSetup {
            arena: 3,
            rules: 1,
            seed,
            slots: vec![
                FixtureSlot {
                    occupied: true,
                    character: 0,
                    style: 0,
                    equipment: 0,
                    team: 0,
                },
                FixtureSlot {
                    occupied: true,
                    character: 1,
                    style: 0,
                    equipment: 1,
                    team: 1,
                },
                FixtureSlot {
                    occupied: false,
                    character: 2,
                    style: 0,
                    equipment: 2,
                    team: 0,
                },
                FixtureSlot {
                    occupied: false,
                    character: 3,
                    style: 0,
                    equipment: 3,
                    team: 1,
                },
            ],
        },
        initial_state: FixtureInitialState::default(),
        observations: FixtureObservations::default(),
        duration_ticks: 1,
        restore_tick: 0,
        stop_on_result: false,
        checkpoint_ticks: vec![1],
        raw_spans: vec![],
        raw_edges: vec![],
        action_spans: vec![],
        action_edges: vec![],
        expected: None,
    };
    let compiled = compile_fixture(&fixture);
    let mut driver =
        build_headless_simulation(compiled.config).expect("crate probe production world builds");
    let world = driver.world_mut();
    let victim_position = {
        let mut fighters = world.query::<(&Fighter, &SimPosition)>();
        fighters
            .iter(world)
            .find_map(|(fighter, position)| (fighter.id == 1).then_some(position.translation))
            .expect("crate probe victim exists")
    };
    let (crate_entity, crate_id) = {
        let mut items = world.query::<(Entity, &StableSimEntity, &ArenaItem)>();
        items
            .iter(world)
            .find_map(|(entity, stable, item)| {
                (item.kind == ItemKind::Crate).then_some((entity, stable.id()))
            })
            .expect("Crank Yard contains its authored mystery crate")
    };
    let mut crate_item = world
        .get_mut::<ArenaItem>(crate_entity)
        .expect("authored mystery crate remains present");
    crate_item.position = victim_position + Vec3::Y * (crate::constants::FIGHTER_HEIGHT * 0.58);
    crate_item.launch_as_thrown(FighterId::ZERO, Vec3::ZERO);

    driver
        .step_committed(&compiled.inputs[0])
        .expect("crate contact advances through the production schedule");
    let snapshot = driver
        .capture_live_snapshot()
        .expect("crate reward is represented in the canonical snapshot");
    snapshot
        .dynamic_objects
        .iter()
        .find(|object| {
            object.id.kind() == SimEntityKind::Item && object.related_entity == Some(crate_id)
        })
        .expect("production crate contact spawns one related reward")
        .definition_id
}

#[test]
fn mystery_crate_reward_is_seeded_and_repeatable() {
    let first = mystery_crate_reward_from_production(1);
    let repeated = mystery_crate_reward_from_production(1);
    let different = mystery_crate_reward_from_production(2);

    assert_eq!(first, repeated);
    assert_eq!(first, 6, "seed one deterministically selects coffee");
    assert_eq!(different, 2, "seed two deterministically selects an apple");
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BotSeedCutoverTick {
    input: InputFrame,
    action: FighterAction,
    action_elapsed_ticks: u32,
}

fn bot_seed_cutover_trace(seed: u64, duration_ticks: u64) -> Vec<BotSeedCutoverTick> {
    let peer = PeerId::new(0xB07).expect("bot cutover peer ID is non-zero");
    let mut setup = LocalSetup::default();
    setup.replay_seed = seed;
    setup.slots[0].input = LocalInputAssignment::Unassigned;
    let options = MatchBuildOptions::single_peer(
        MatchId::new(*b"bot-rng-cutover!").expect("bot cutover match ID is non-zero"),
        AuthorityKind::Dedicated,
        true,
        peer,
        &setup,
        SimTick(120),
    );
    let config = build_headless_match_config(&setup, options).expect("bot cutover config is valid");
    let ownership = config.manifest.ownership;
    let mut driver =
        build_headless_simulation(config).expect("bot cutover production world builds");
    let mut trace = Vec::with_capacity(duration_ticks as usize);

    for raw_tick in 1..=duration_ticks {
        let tick = SimTick(raw_tick);
        let generated = AuthoritySimulation::generate_authority_bot_frames(&mut driver, tick)
            .expect("authority bot generation succeeds")
            .expect("the production headless world owns one authority bot");
        let mut committed = CommittedTickInputs {
            tick,
            by_seat: [None; crate::network_protocol::MAX_SEATS],
        };
        let mut selected_bot_frame = None;
        for assignment in ownership.as_slice() {
            let seat_index = usize::from(assignment.seat.get());
            let (frame, origin) = match assignment.owner {
                SeatOwner::Peer(owner) => (
                    InputFrame {
                        tick,
                        seat: assignment.seat,
                        sequence: NetworkInputSequence(raw_tick as u16),
                        ..InputFrame::default()
                    },
                    AuthorityInputOrigin::Peer(owner),
                ),
                SeatOwner::AuthorityBot => {
                    let frame = generated[seat_index]
                        .expect("authority generator emits the bot-owned seat");
                    selected_bot_frame = Some(frame);
                    (frame, AuthorityInputOrigin::AuthorityBot)
                }
            };
            committed.by_seat[seat_index] = Some(AuthorityInputRecord {
                frame,
                fighter: assignment.fighter,
                origin,
                status: AuthorityInputStatus::Committed,
            });
        }
        driver
            .step_committed(&committed)
            .expect("bot cutover production schedule advances");
        let bot_entity = fighter_entity(driver.world_mut(), 1);
        let action = driver
            .world()
            .get::<FighterActionState>(bot_entity)
            .expect("bot has canonical action state");
        trace.push(BotSeedCutoverTick {
            input: selected_bot_frame.expect("one authority bot frame was selected"),
            action: action.action,
            action_elapsed_ticks: action.elapsed.get(),
        });
    }
    trace
}

#[test]
fn authority_headless_bot_tape_is_seeded_and_repeatable() {
    const BASE_SEED: u64 = 0xAFC0_5EED_1234_5678;
    const CHANGED_SEED: u64 = 0xAFC0_5EED_1234_5679;
    const TICKS: u64 = 48;

    let first = bot_seed_cutover_trace(BASE_SEED, TICKS);
    let repeated = bot_seed_cutover_trace(BASE_SEED, TICKS);
    assert_eq!(first, repeated);

    let different = bot_seed_cutover_trace(CHANGED_SEED, TICKS);
    let first_input_difference = first
        .iter()
        .zip(&different)
        .position(|(left, right)| left.input != right.input)
        .map(|index| index + 1);
    let first_action_difference = first
        .iter()
        .zip(&different)
        .position(|(left, right)| {
            (left.action, left.action_elapsed_ticks) != (right.action, right.action_elapsed_ticks)
        })
        .map(|index| index + 1);

    assert_eq!(first_input_difference, Some(24));
    assert_eq!(first_action_difference, Some(44));
    assert_ne!(first, different);
}

#[test]
#[ignore = "requires AFC_UPDATE_BEHAVIOR_GOLDENS=1 and deliberately rewrites fixtures"]
fn update_behavior_fixture_goldens() {
    assert_eq!(
        std::env::var("AFC_UPDATE_BEHAVIOR_GOLDENS").as_deref(),
        Ok("1"),
        "refusing to write behavior goldens without AFC_UPDATE_BEHAVIOR_GOLDENS=1"
    );
    let selected = std::env::var("AFC_BEHAVIOR_FIXTURE").ok();
    let mut updated = 0_usize;
    for path in fixture_paths() {
        let mut fixture = load_fixture(&path);
        if selected
            .as_deref()
            .is_some_and(|selected| selected != fixture.name)
        {
            continue;
        }
        let compiled = compile_fixture(&fixture);
        let observed = assert_execution_contract(&fixture, &compiled);
        report_golden_change(&path, fixture.expected.as_ref(), &observed);
        fixture.expected = Some(observed);
        let encoded = ron::ser::to_string_pretty(&fixture, ron::ser::PrettyConfig::new())
            .expect("behavior fixture must serialize");
        fs::write(&path, format!("{encoded}\n"))
            .unwrap_or_else(|error| panic!("failed to write {}: {error}", path.display()));
        updated += 1;
    }
    assert!(
        updated > 0,
        "AFC_BEHAVIOR_FIXTURE did not match a checked-in fixture"
    );
}
