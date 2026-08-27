//! Executable native/WASM determinism probe over the production simulation.
//!
//! The report deliberately uses a synthetic compatibility identity so build
//! profile and Cargo feature metadata cannot hide a simulation mismatch. CI
//! executes this exact function once as Linux native code and once from a
//! `wasm32-unknown-unknown` module under Node, then compares the JSON bytes.

use core::fmt;

use serde::Serialize;

use crate::authority::AuthoritySimulation;
use crate::authority_input::{
    AuthorityInputOrigin, AuthorityInputRecord, AuthorityInputStatus, CommittedTickInputs,
};
use crate::characters::CHARACTER_KINDS;
use crate::determinism::FighterId;
use crate::equipment::EquipmentKind;
use crate::game_state::{LocalSetup, MatchPhase, MatchState};
use crate::headless::{
    HeadlessBuildError, HeadlessMatchConfig, build_headless_simulation,
    snapshot_contract_for_manifest,
};
use crate::live_authority::LiveSimulationError;
use crate::match_config::CURRENT_SIMULATION_VERSION;
use crate::network_protocol::{
    AuthorityKind, BuildId, CompatibilityId, DefinitionId, FighterSlotConfig, GameplayContentHash,
    InputButtons, InputFrame, InputSequence, MAX_FIGHTERS, MAX_NORMAL_ROLLBACK_TICKS,
    MIN_SNAPSHOT_HISTORY_TICKS, ManifestHash, MatchId, MatchManifest, PeerId, ProtocolVersion,
    QuantizedAxis, ReplayFormatVersion, SIMULATION_HZ, SeatAssignment, SeatId, SeatOwner,
    SeatOwnership, SimulationVersion, TeamId as ProtocolTeamId,
};
use crate::snapshot::{MatchResultSnapshot, SnapshotError};
use crate::styles::FighterStyleKind;

pub const CROSS_TARGET_PROBE_SCHEMA: &str = "afc-linux-wasm-determinism-v1";
const PROBE_SEED: u64 = 0xAFC0_5EED_1234_5678;
const MAX_PROBE_TICKS: u64 = 2_400;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeterminismProbeCheckpoint {
    pub tick: u64,
    pub canonical_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeterminismProbeReport {
    pub schema: &'static str,
    pub simulation_version: u16,
    pub tick_rate_hz: u16,
    pub checkpoints: Vec<DeterminismProbeCheckpoint>,
    pub final_tick: u64,
    pub final_hash: String,
    pub winning_team: u8,
}

#[derive(Debug)]
pub enum DeterminismProbeError {
    Build(HeadlessBuildError),
    Simulation(LiveSimulationError),
    Snapshot(SnapshotError),
    Serialization(serde_json::Error),
    MissingResult,
    UnexpectedResult(MatchResultSnapshot),
}

impl fmt::Display for DeterminismProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for DeterminismProbeError {}

impl From<HeadlessBuildError> for DeterminismProbeError {
    fn from(error: HeadlessBuildError) -> Self {
        Self::Build(error)
    }
}

impl From<LiveSimulationError> for DeterminismProbeError {
    fn from(error: LiveSimulationError) -> Self {
        Self::Simulation(error)
    }
}

impl From<SnapshotError> for DeterminismProbeError {
    fn from(error: SnapshotError) -> Self {
        Self::Snapshot(error)
    }
}

impl From<serde_json::Error> for DeterminismProbeError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialization(error)
    }
}

pub fn run_cross_target_probe() -> Result<DeterminismProbeReport, DeterminismProbeError> {
    let config = stock_probe_config_for_arena(0);
    let mut driver = build_headless_simulation(config.clone())?;
    let mut checkpoints = Vec::with_capacity(8);

    for raw_tick in 1..=MAX_PROBE_TICKS {
        let tick = crate::determinism::SimTick(raw_tick);
        AuthoritySimulation::step(&mut driver, &outward_ringout_inputs(&config, tick))?;
        if raw_tick == 1 || raw_tick % 120 == 0 {
            let snapshot = driver.capture_live_snapshot()?;
            checkpoints.push(DeterminismProbeCheckpoint {
                tick: raw_tick,
                canonical_hash: format!("{:016x}", snapshot.canonical_hash()?),
            });
        }
        if driver.world().resource::<MatchState>().phase == MatchPhase::Results {
            break;
        }
    }

    let snapshot = driver.capture_live_snapshot()?;
    let MatchResultSnapshot::TeamWinner { team, decided_tick } = snapshot.match_state.result else {
        return if snapshot.match_state.result == MatchResultSnapshot::Pending {
            Err(DeterminismProbeError::MissingResult)
        } else {
            Err(DeterminismProbeError::UnexpectedResult(
                snapshot.match_state.result,
            ))
        };
    };
    if decided_tick != snapshot.header.tick {
        return Err(DeterminismProbeError::UnexpectedResult(
            snapshot.match_state.result,
        ));
    }

    Ok(DeterminismProbeReport {
        schema: CROSS_TARGET_PROBE_SCHEMA,
        simulation_version: CURRENT_SIMULATION_VERSION,
        tick_rate_hz: SIMULATION_HZ,
        checkpoints,
        final_tick: snapshot.header.tick.get(),
        final_hash: format!("{:016x}", snapshot.canonical_hash()?),
        winning_team: team,
    })
}

pub fn run_cross_target_probe_json() -> Result<String, DeterminismProbeError> {
    Ok(serde_json::to_string(&run_cross_target_probe()?)?)
}

pub(crate) fn stock_probe_config_for_arena(arena_index: usize) -> HeadlessMatchConfig {
    let peer = PeerId::new(77).expect("the probe peer ID is non-zero");
    let ownership = SeatOwnership::from_assignments(&[
        SeatAssignment {
            seat: SeatId::new(0).expect("probe seat zero is valid"),
            fighter: FighterId::new(0).expect("probe fighter zero is valid"),
            owner: SeatOwner::Peer(peer),
        },
        SeatAssignment {
            seat: SeatId::new(1).expect("probe seat one is valid"),
            fighter: FighterId::new(1).expect("probe fighter one is valid"),
            owner: SeatOwner::AuthorityBot,
        },
    ])
    .expect("the fixed probe ownership is valid");
    let mut slots = [FighterSlotConfig::default(); MAX_FIGHTERS];
    let setup = LocalSetup {
        arena_index,
        ..LocalSetup::default()
    };
    for (index, wire_slot) in slots.iter_mut().enumerate().take(2) {
        *wire_slot = FighterSlotConfig {
            occupied: true,
            fighter: FighterId::from_index(index).expect("probe fighter index is valid"),
            team: ProtocolTeamId::new(team_definition_id(setup.slots[index].team))
                .expect("probe team is valid"),
            character: DefinitionId::new(
                CHARACTER_KINDS
                    .iter()
                    .position(|kind| *kind == setup.slots[index].character)
                    .expect("probe character is cataloged") as u16,
            )
            .expect("probe character definition is valid"),
            style: DefinitionId::new(style_definition_id(setup.slots[index].style))
                .expect("probe style definition is valid"),
            equipment: DefinitionId::new(equipment_definition_id(setup.slots[index].equipment))
                .expect("probe equipment definition is valid"),
        };
    }
    let manifest = MatchManifest {
        compatibility: CompatibilityId {
            protocol: ProtocolVersion::new(1).expect("probe protocol is valid"),
            simulation: SimulationVersion::new(CURRENT_SIMULATION_VERSION)
                .expect("probe simulation version is valid"),
            replay: ReplayFormatVersion::new(1).expect("probe replay schema is valid"),
            build: BuildId::new([0xB1; 16]).expect("probe build ID is valid"),
            gameplay_content: GameplayContentHash::new([0xC7; 32])
                .expect("probe gameplay ID is valid"),
        },
        manifest_hash: ManifestHash(0xAFC0),
        match_id: MatchId::new(*b"headless-fixture").expect("probe match ID is valid"),
        authority: AuthorityKind::Dedicated,
        trusted_results: true,
        arena: DefinitionId::new(setup.arena_index as u16)
            .expect("probe arena definition is valid"),
        rules: DefinitionId::new(setup.rule_index as u16).expect("probe rules definition is valid"),
        slots,
        ownership,
        master_gameplay_seed: PROBE_SEED,
        rng_scheme_version: 1,
        tick_rate_hz: SIMULATION_HZ,
        input_delay_ticks: 2,
        rollback_limit_ticks: MAX_NORMAL_ROLLBACK_TICKS,
        snapshot_history_ticks: MIN_SNAPSHOT_HISTORY_TICKS,
        agreed_start_tick: crate::determinism::SimTick(120),
    };
    let mut local_setup = setup;
    local_setup.replay_seed = PROBE_SEED;
    HeadlessMatchConfig {
        snapshot_contract: snapshot_contract_for_manifest(&manifest),
        manifest,
        local_setup,
    }
}

fn outward_ringout_inputs(
    config: &HeadlessMatchConfig,
    tick: crate::determinism::SimTick,
) -> CommittedTickInputs {
    let mut committed = neutral_inputs(config, tick);
    let record = committed.by_seat[0]
        .as_mut()
        .expect("the probe owns the first active seat");
    record.frame.movement_y = QuantizedAxis::new(127).expect("full movement is valid");
    record.frame.held_buttons = InputButtons::default();
    committed
}

fn neutral_inputs(
    config: &HeadlessMatchConfig,
    tick: crate::determinism::SimTick,
) -> CommittedTickInputs {
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
            held_buttons: InputButtons::default(),
            pressed_buttons: InputButtons::default(),
            released_buttons: InputButtons::default(),
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

const fn team_definition_id(team: crate::game_state::TeamId) -> u8 {
    match team {
        crate::game_state::TeamId::Red => 0,
        crate::game_state::TeamId::Blue => 1,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_is_canonical_and_json_round_trips() {
        let report = run_cross_target_probe().unwrap();
        assert_eq!(report.schema, CROSS_TARGET_PROBE_SCHEMA);
        assert_eq!(report.simulation_version, CURRENT_SIMULATION_VERSION);
        assert_eq!(report.checkpoints.len(), 8);
        assert_eq!(report.final_tick, 934);
        assert_eq!(report.winning_team, 1);
        assert_eq!(
            serde_json::to_string(&report).unwrap(),
            run_cross_target_probe_json().unwrap()
        );
    }

    #[test]
    fn shared_probe_config_keeps_its_synthetic_identity() {
        let config = stock_probe_config_for_arena(0);
        assert_eq!(config.manifest.manifest_hash, ManifestHash(0xAFC0));
        assert_ne!(
            crate::match_config::canonical_manifest_hash(&config.manifest),
            config.manifest.manifest_hash
        );
        config.validate().unwrap();
    }
}
