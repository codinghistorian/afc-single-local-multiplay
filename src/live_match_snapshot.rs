//! Canonical snapshot mapping for the live match and arena resources.
//!
//! This codec reads the existing authoritative resources directly. It does not
//! mirror them into a second mutable schema-shaped resource between ticks.

use bevy::prelude::*;

use crate::arena::{
    ArenaRuntimeRestorePlan, capture_arena_runtime_snapshot, commit_arena_runtime_restore,
    prepare_arena_runtime_restore,
};
use crate::arena_defs::ActiveArena;
use crate::components::{Fighter, FighterStats};
use crate::constants::FIGHTER_COUNT;
use crate::determinism::{DEFAULT_F32_QUANTIZATION, FighterId, dequantize_f32, quantize_f32};
use crate::game_state::{
    Hitstop, MatchPhase, MatchRules, MatchState, MatchTelemetry, RULE_PRESETS, RulePreset, TeamId,
};
use crate::simulation::{SimTick, seconds_to_ticks_ceil};
use crate::snapshot::{
    CanonicalSnapshot, FighterMatchStatsSnapshot, MatchPhaseSnapshot, MatchResultSnapshot,
    MatchRulesSnapshot, MatchStateSnapshot, MatchStatsSnapshot,
};
use crate::snapshot_ecs::{
    CanonicalNonFighterState, NonFighterSnapshotCodec, SnapshotCodecError, SnapshotContract,
};

const RULE_TIMED_TEAM_SCORE: u32 = 0;
const RULE_FREE_FOR_ALL: u32 = 1;
const RULE_STOCK_RING_OUT: u32 = 2;

fn codec_error(code: u16, message: &'static str) -> SnapshotCodecError {
    SnapshotCodecError::new(code, message)
}

fn phase_to_snapshot(phase: MatchPhase) -> MatchPhaseSnapshot {
    match phase {
        MatchPhase::Setup => MatchPhaseSnapshot::Setup,
        MatchPhase::Fighting => MatchPhaseSnapshot::Fight,
        MatchPhase::TimeUp => MatchPhaseSnapshot::TimeUp,
        MatchPhase::Results => MatchPhaseSnapshot::Result,
        MatchPhase::Resetting => MatchPhaseSnapshot::Resetting,
    }
}

fn phase_from_snapshot(phase: MatchPhaseSnapshot) -> Result<MatchPhase, SnapshotCodecError> {
    match phase {
        MatchPhaseSnapshot::Setup => Ok(MatchPhase::Setup),
        MatchPhaseSnapshot::Fight => Ok(MatchPhase::Fighting),
        MatchPhaseSnapshot::TimeUp => Ok(MatchPhase::TimeUp),
        MatchPhaseSnapshot::Result => Ok(MatchPhase::Results),
        MatchPhaseSnapshot::Resetting => Ok(MatchPhase::Resetting),
        MatchPhaseSnapshot::Countdown | MatchPhaseSnapshot::SuddenDeath => Err(codec_error(
            10,
            "live game does not implement countdown or sudden-death phases",
        )),
    }
}

fn ruleset_code(preset: RulePreset) -> u32 {
    match preset {
        RulePreset::TimedTeamScore => RULE_TIMED_TEAM_SCORE,
        RulePreset::FreeForAll => RULE_FREE_FOR_ALL,
        RulePreset::StockRingOut => RULE_STOCK_RING_OUT,
    }
}

fn ruleset_index(code: u32) -> Option<usize> {
    match code {
        RULE_TIMED_TEAM_SCORE => Some(0),
        RULE_FREE_FOR_ALL => Some(1),
        RULE_STOCK_RING_OUT => Some(2),
        _ => None,
    }
}

fn rules_snapshot(state: &MatchState, arena: ActiveArena) -> MatchRulesSnapshot {
    MatchRulesSnapshot {
        ruleset_id: ruleset_code(state.rules.preset),
        arena_id: arena.index() as u32,
        duration_ticks: state.rules.time_limit.map_or(0, seconds_to_ticks_ceil),
        starting_stocks: state
            .rules
            .starting_stocks
            .and_then(|stocks| u8::try_from(stocks).ok())
            .unwrap_or(0),
        score_limit: 0,
        team_mode: state.rules.team_scoring,
        friendly_fire: state.rules.friendly_fire,
    }
}

fn validate_rules_snapshot(
    snapshot: MatchRulesSnapshot,
    active_arena: ActiveArena,
) -> Result<(usize, MatchRules), SnapshotCodecError> {
    let rule_index =
        ruleset_index(snapshot.ruleset_id).ok_or(codec_error(11, "unknown live ruleset ID"))?;
    let rules = RULE_PRESETS[rule_index];
    if snapshot.arena_id != active_arena.index() as u32 {
        return Err(codec_error(
            12,
            "snapshot arena differs from match contract",
        ));
    }
    let expected = MatchRulesSnapshot {
        ruleset_id: snapshot.ruleset_id,
        arena_id: snapshot.arena_id,
        duration_ticks: rules.time_limit.map_or(0, seconds_to_ticks_ceil),
        starting_stocks: rules
            .starting_stocks
            .and_then(|stocks| u8::try_from(stocks).ok())
            .unwrap_or(0),
        score_limit: 0,
        team_mode: rules.team_scoring,
        friendly_fire: rules.friendly_fire,
    };
    if snapshot != expected {
        return Err(codec_error(13, "rules fields do not match the ruleset ID"));
    }
    Ok((rule_index, rules))
}

fn team_code(team: TeamId) -> u8 {
    match team {
        TeamId::Red => 0,
        TeamId::Blue => 1,
    }
}

fn team_from_code(code: u8) -> Result<TeamId, SnapshotCodecError> {
    match code {
        0 => Ok(TeamId::Red),
        1 => Ok(TeamId::Blue),
        _ => Err(codec_error(14, "invalid live team code")),
    }
}

fn active_mask(active: [bool; FIGHTER_COUNT]) -> u8 {
    active.into_iter().enumerate().fold(
        0,
        |mask, (index, active)| {
            if active { mask | (1 << index) } else { mask }
        },
    )
}

fn collect_scores(world: &World) -> Result<[i32; FIGHTER_COUNT], SnapshotCodecError> {
    let mut scores = [0; FIGHTER_COUNT];
    let mut seen = 0_u8;
    for entity in world
        .archetypes()
        .iter()
        .flat_map(|archetype| archetype.entities())
        .map(|entity| entity.id())
    {
        let (Some(fighter), Some(stats)) = (
            world.get::<Fighter>(entity),
            world.get::<FighterStats>(entity),
        ) else {
            continue;
        };
        let Some(id) = FighterId::new(fighter.id as u8) else {
            return Err(codec_error(15, "fighter ID is outside the fixed roster"));
        };
        let bit = 1 << id.get();
        if seen & bit != 0 {
            return Err(codec_error(
                16,
                "duplicate fighter ID while deriving result",
            ));
        }
        seen |= bit;
        scores[id.index()] = stats.score;
    }
    Ok(scores)
}

fn derive_result(
    phase: MatchPhaseSnapshot,
    active_slots_mask: u8,
    teams: [u8; FIGHTER_COUNT],
    stocks: [u8; FIGHTER_COUNT],
    stock_mode: bool,
    team_mode: bool,
    scores: [i32; FIGHTER_COUNT],
    tick: SimTick,
) -> MatchResultSnapshot {
    if !matches!(
        phase,
        MatchPhaseSnapshot::Result | MatchPhaseSnapshot::Resetting
    ) {
        return MatchResultSnapshot::Pending;
    }

    if stock_mode {
        let mut survivor_count = 0_usize;
        let mut first_survivor = None;
        let mut surviving_team = None;
        let mut multiple_teams = false;
        for fighter in 0..FIGHTER_COUNT {
            if active_slots_mask & (1 << fighter) == 0 || stocks[fighter] == 0 {
                continue;
            }
            survivor_count += 1;
            first_survivor.get_or_insert(fighter);
            match surviving_team {
                None => surviving_team = Some(teams[fighter]),
                Some(team) if team == teams[fighter] => {}
                Some(_) => multiple_teams = true,
            }
        }
        if survivor_count == 0 {
            return MatchResultSnapshot::Draw { decided_tick: tick };
        }
        if team_mode
            && !multiple_teams
            && let Some(team) = surviving_team
        {
            return MatchResultSnapshot::TeamWinner {
                team,
                decided_tick: tick,
            };
        }
        if !team_mode
            && survivor_count == 1
            && let Some(fighter) = first_survivor
        {
            return MatchResultSnapshot::FighterWinner {
                fighter: FighterId::ALL[fighter],
                decided_tick: tick,
            };
        }
    }

    let mut best: Option<(usize, i32)> = None;
    let mut tied = false;
    for index in 0..FIGHTER_COUNT {
        if active_slots_mask & (1 << index) == 0 {
            continue;
        }
        match best {
            None => best = Some((index, scores[index])),
            Some((_, score)) if scores[index] > score => {
                best = Some((index, scores[index]));
                tied = false;
            }
            Some((_, score)) if scores[index] == score => tied = true,
            _ => {}
        }
    }
    if tied {
        MatchResultSnapshot::Draw { decided_tick: tick }
    } else if let Some((fighter, _)) = best {
        MatchResultSnapshot::FighterWinner {
            fighter: FighterId::ALL[fighter],
            decided_tick: tick,
        }
    } else {
        MatchResultSnapshot::Draw { decided_tick: tick }
    }
}

fn capture_match_state(
    world: &World,
    tick: SimTick,
) -> Result<MatchStateSnapshot, SnapshotCodecError> {
    let state = world
        .get_resource::<MatchState>()
        .ok_or(codec_error(1, "missing MatchState"))?;
    let hitstop = world
        .get_resource::<Hitstop>()
        .ok_or(codec_error(2, "missing Hitstop"))?;
    let arena = *world
        .get_resource::<ActiveArena>()
        .ok_or(codec_error(3, "missing ActiveArena"))?;
    let mut stocks = [0; FIGHTER_COUNT];
    for (destination, source) in stocks.iter_mut().zip(state.stocks) {
        *destination = u8::try_from(source)
            .map_err(|_| codec_error(17, "fighter stock is outside u8 range"))?;
    }
    let phase = phase_to_snapshot(state.phase);
    let teams = state.teams.map(team_code);
    let mask = active_mask(state.active_slots);
    let rules = rules_snapshot(state, arena);
    let scores = collect_scores(world)?;
    Ok(MatchStateSnapshot {
        phase,
        phase_ticks: state.phase_timer_ticks,
        match_ticks_remaining: state.timer_ticks,
        hitstop_ticks: hitstop.remaining_ticks,
        // TickEventBuffer resets at the beginning of every subsequent step.
        next_event_ordinal: 0,
        active_slots_mask: mask,
        teams,
        stocks,
        rules,
        result: derive_result(
            phase,
            mask,
            teams,
            stocks,
            rules.starting_stocks > 0,
            rules.team_mode,
            scores,
            tick,
        ),
    })
}

fn capture_match_stats(telemetry: &MatchTelemetry, tick: SimTick) -> MatchStatsSnapshot {
    let damage_by_fighter = telemetry
        .damage_by_fighter
        .map(|damage| quantize_f32(damage, DEFAULT_F32_QUANTIZATION));
    MatchStatsSnapshot {
        gameplay_ticks: tick.get(),
        resolved_contacts: 0,
        emitted_events: 0,
        ring_outs: telemetry.ring_outs,
        falls: telemetry.falls,
        item_hits: telemetry.item_hits,
        throws: telemetry.throws,
        guard_breaks: telemetry.guard_breaks,
        damage_by_fighter,
        fighter: [FighterMatchStatsSnapshot::default(); FIGHTER_COUNT],
        rejected_dynamic_spawns: Default::default(),
    }
}

fn validate_derived_result(snapshot: &CanonicalSnapshot) -> Result<(), SnapshotCodecError> {
    let scores = snapshot.fighters.map(|fighter| fighter.score);
    let expected = derive_result(
        snapshot.match_state.phase,
        snapshot.match_state.active_slots_mask,
        snapshot.match_state.teams,
        snapshot.match_state.stocks,
        snapshot.match_state.rules.starting_stocks > 0,
        snapshot.match_state.rules.team_mode,
        scores,
        snapshot.header.tick,
    );
    if snapshot.match_state.result != expected {
        return Err(codec_error(
            18,
            "match result is not canonical for live state",
        ));
    }
    Ok(())
}

fn validate_match_stats(
    stats: &MatchStatsSnapshot,
    tick: SimTick,
) -> Result<(), SnapshotCodecError> {
    if stats.gameplay_ticks != tick.get()
        || stats.resolved_contacts != 0
        || stats.emitted_events != 0
        || stats.fighter != [FighterMatchStatsSnapshot::default(); FIGHTER_COUNT]
    {
        return Err(codec_error(
            19,
            "unsupported live match-stat extension is nonzero",
        ));
    }
    Ok(())
}

pub struct LiveMatchRestorePlan {
    state: MatchState,
    telemetry: MatchTelemetry,
    hitstop: Hitstop,
    arena: ArenaRuntimeRestorePlan,
}

/// Live resource codec used by local, listen, dedicated, and predicted worlds.
#[derive(Clone, Copy, Debug, Default)]
pub struct LiveMatchSnapshotCodec;

impl NonFighterSnapshotCodec for LiveMatchSnapshotCodec {
    type RestorePlan = LiveMatchRestorePlan;

    fn snapshot_contract(&self, world: &World) -> Result<SnapshotContract, SnapshotCodecError> {
        world
            .get_resource::<SnapshotContract>()
            .copied()
            .ok_or(codec_error(4, "missing SnapshotContract"))
    }

    fn capture_non_fighter(
        &self,
        world: &World,
        tick: SimTick,
    ) -> Result<CanonicalNonFighterState, SnapshotCodecError> {
        let contract = *world
            .get_resource::<SnapshotContract>()
            .ok_or(codec_error(4, "missing SnapshotContract"))?;
        let state = world
            .get_resource::<MatchState>()
            .ok_or(codec_error(1, "missing MatchState"))?;
        if state.replay_seed != contract.master_seed {
            return Err(codec_error(20, "match seed differs from snapshot contract"));
        }
        let telemetry = world
            .get_resource::<MatchTelemetry>()
            .ok_or(codec_error(5, "missing MatchTelemetry"))?;
        if telemetry.replay_seed != state.replay_seed {
            return Err(codec_error(21, "telemetry seed differs from match seed"));
        }
        Ok(CanonicalNonFighterState {
            contract,
            match_state: capture_match_state(world, tick)?,
            arena: capture_arena_runtime_snapshot(world)
                .map_err(|_| codec_error(6, "arena runtime capture failed"))?,
            // Current gameplay randomness is event-keyed from master seed, tick,
            // stable entity/fighter ID, and purpose; it has no mutable stream.
            rng_streams: Vec::new(),
            stats: capture_match_stats(telemetry, tick),
        })
    }

    fn prepare_restore(
        &self,
        world: &World,
        snapshot: &CanonicalSnapshot,
    ) -> Result<Self::RestorePlan, SnapshotCodecError> {
        let contract = *world
            .get_resource::<SnapshotContract>()
            .ok_or(codec_error(4, "missing SnapshotContract"))?;
        contract
            .validate_header(&snapshot.header)
            .map_err(|_| codec_error(7, "snapshot contract mismatch"))?;
        if !snapshot.rng_streams.is_empty() {
            return Err(codec_error(
                22,
                "live keyed-RNG snapshot contains mutable streams",
            ));
        }
        if snapshot.match_state.next_event_ordinal != 0 {
            return Err(codec_error(
                23,
                "completed live tick has a nonzero next event ordinal",
            ));
        }
        validate_match_stats(&snapshot.stats, snapshot.header.tick)?;
        validate_derived_result(snapshot)?;

        let active_arena = *world
            .get_resource::<ActiveArena>()
            .ok_or(codec_error(3, "missing ActiveArena"))?;
        let (rule_index, rules) =
            validate_rules_snapshot(snapshot.match_state.rules, active_arena)?;
        let phase = phase_from_snapshot(snapshot.match_state.phase)?;
        let mut active_slots = [false; FIGHTER_COUNT];
        for (index, active) in active_slots.iter_mut().enumerate() {
            *active = snapshot.match_state.active_slots_mask & (1 << index) != 0;
        }
        let mut teams = [TeamId::Red; FIGHTER_COUNT];
        for (destination, source) in teams.iter_mut().zip(snapshot.match_state.teams) {
            *destination = team_from_code(source)?;
        }
        let stocks = snapshot.match_state.stocks.map(i32::from);
        let active_fighter_count = active_slots.iter().filter(|active| **active).count();
        let debug_hitboxes = world
            .get_resource::<MatchState>()
            .map(|state| state.debug_hitboxes)
            .unwrap_or(false);
        let state = MatchState {
            timer_ticks: snapshot.match_state.match_ticks_remaining,
            phase,
            phase_timer_ticks: snapshot.match_state.phase_ticks,
            rules,
            rule_index,
            arena_index: active_arena.index(),
            active_fighter_count,
            active_slots,
            teams,
            stocks,
            replay_seed: snapshot.header.master_seed,
            // Debug visibility is presentation-local and does not enter hashes.
            debug_hitboxes,
            reset_requested: phase == MatchPhase::Resetting,
        };
        let telemetry = MatchTelemetry {
            replay_seed: snapshot.header.master_seed,
            ring_outs: snapshot.stats.ring_outs,
            falls: snapshot.stats.falls,
            item_hits: snapshot.stats.item_hits,
            throws: snapshot.stats.throws,
            guard_breaks: snapshot.stats.guard_breaks,
            damage_by_fighter: snapshot
                .stats
                .damage_by_fighter
                .map(|damage| dequantize_f32(damage, DEFAULT_F32_QUANTIZATION)),
        };
        let arena = prepare_arena_runtime_restore(world, &snapshot.arena)
            .map_err(|_| codec_error(8, "arena runtime restore validation failed"))?;
        Ok(LiveMatchRestorePlan {
            state,
            telemetry,
            hitstop: Hitstop {
                remaining_ticks: snapshot.match_state.hitstop_ticks,
            },
            arena,
        })
    }

    fn commit_restore(&self, world: &mut World, plan: Self::RestorePlan) {
        world.insert_resource(plan.state);
        world.insert_resource(plan.telemetry);
        world.insert_resource(plan.hitstop);
        commit_arena_runtime_restore(world, plan.arena);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena::{ArenaHazardState, ArenaPipeState, PowderKegCannonState};
    use crate::components::FighterStats;
    use crate::ecs_identity::SIM_ENTITY_POOL_CAPACITIES;
    use crate::snapshot::{ArenaRuntimeSnapshot, SnapshotHeader};

    fn contract(seed: u64) -> SnapshotContract {
        SnapshotContract {
            simulation_version: 2,
            protocol_version: 1,
            gameplay_content_hash: 77,
            match_id: *b"live-match-test!",
            master_seed: seed,
            pool_capacities: SIM_ENTITY_POOL_CAPACITIES,
        }
    }

    fn world_with_fighter_order(order: [usize; 2]) -> World {
        let mut world = World::new();
        let seed = 0xAFC0_1234;
        let active_arena = ActiveArena::new(3);
        let mut state = MatchState::default();
        state.phase = MatchPhase::Fighting;
        state.active_slots = [true, true, false, false];
        state.active_fighter_count = 2;
        state.arena_index = active_arena.index();
        state.replay_seed = seed;
        world.insert_resource(contract(seed));
        world.insert_resource(active_arena);
        world.insert_resource(state);
        world.insert_resource(MatchTelemetry {
            replay_seed: seed,
            ring_outs: 2,
            falls: 1,
            item_hits: 3,
            throws: 4,
            guard_breaks: 5,
            damage_by_fighter: [1.25, 2.5, 0.0, 0.0],
        });
        world.insert_resource(Hitstop { remaining_ticks: 6 });
        world.insert_resource(ArenaHazardState::new(
            active_arena.index(),
            active_arena.definition().hazards.len(),
        ));
        world.insert_resource(ArenaPipeState::new(active_arena.index()));
        world.insert_resource(PowderKegCannonState::new(active_arena.index()));
        for id in order {
            world.spawn((
                Fighter {
                    id,
                    name: "fixture",
                    color: Color::WHITE,
                    spawn: Vec3::ZERO,
                },
                FighterStats {
                    score: id as i32,
                    ..Default::default()
                },
            ));
        }
        world
    }

    fn world() -> World {
        world_with_fighter_order([1, 0])
    }

    #[test]
    fn live_match_capture_and_restore_round_trip_is_entity_order_independent() {
        let codec = LiveMatchSnapshotCodec;
        let mut world = world();
        let tick = SimTick(44);
        let captured = codec.capture_non_fighter(&world, tick).unwrap();
        assert_eq!(captured.match_state.active_slots_mask, 0b0011);
        assert_eq!(captured.stats.ring_outs, 2);
        assert_eq!(captured.stats.damage_by_fighter[1], 10_240);

        let snapshot = CanonicalSnapshot {
            header: captured.contract.header(tick),
            match_state: captured.match_state,
            fighters: FighterId::ALL.map(|id| {
                let mut fighter = crate::snapshot::FighterSnapshot::empty(id);
                if id.index() < 2 {
                    fighter.occupied = true;
                    fighter.active = true;
                    fighter.score = id.index() as i32;
                }
                fighter
            }),
            arena: captured.arena,
            allocators: crate::determinism::SimEntityKind::ALL
                .map(|kind| crate::snapshot::PoolAllocatorSnapshot::empty(kind, 0).unwrap())
                .into_iter()
                .collect(),
            dynamic_objects: Vec::new(),
            rng_streams: captured.rng_streams,
            stats: captured.stats,
        };
        snapshot.validate().unwrap();

        world.resource_mut::<MatchState>().timer_ticks = 999;
        world.resource_mut::<MatchTelemetry>().ring_outs = 99;
        let plan = codec.prepare_restore(&world, &snapshot).unwrap();
        codec.commit_restore(&mut world, plan);
        let recaptured = codec.capture_non_fighter(&world, tick).unwrap();
        assert_eq!(recaptured.match_state, snapshot.match_state);
        assert_eq!(recaptured.stats, snapshot.stats);
        assert_eq!(recaptured.arena, snapshot.arena);
    }

    #[test]
    fn simultaneous_final_stock_loss_is_a_draw_in_every_fighter_entity_order() {
        let codec = LiveMatchSnapshotCodec;
        let capture = |order| {
            let mut world = world_with_fighter_order(order);
            let mut state = world.resource_mut::<MatchState>();
            state.phase = MatchPhase::Results;
            state.stocks = [0, 0, 0, 0];
            drop(state);
            codec
                .capture_non_fighter(&world, SimTick(77))
                .unwrap()
                .match_state
                .result
        };

        let forward = capture([0, 1]);
        let reversed = capture([1, 0]);
        assert_eq!(reversed, forward);
        assert_eq!(
            forward,
            MatchResultSnapshot::Draw {
                decided_tick: SimTick(77),
            }
        );
    }

    #[test]
    fn stock_team_result_names_the_only_surviving_team() {
        let codec = LiveMatchSnapshotCodec;
        let mut world = world();
        {
            let mut state = world.resource_mut::<MatchState>();
            state.phase = MatchPhase::Results;
            state.active_slots = [true, true, true, false];
            state.active_fighter_count = 3;
            state.stocks = [1, 0, 1, 0];
        }

        let result = codec
            .capture_non_fighter(&world, SimTick(78))
            .unwrap()
            .match_state
            .result;
        assert_eq!(
            result,
            MatchResultSnapshot::TeamWinner {
                team: team_code(TeamId::Red),
                decided_tick: SimTick(78),
            }
        );
    }

    #[test]
    fn live_match_restore_rejects_unsupported_or_inconsistent_sections_atomically() {
        let codec = LiveMatchSnapshotCodec;
        let world = world();
        let tick = SimTick(9);
        let captured = codec.capture_non_fighter(&world, tick).unwrap();
        let base = CanonicalSnapshot {
            header: captured.contract.header(tick),
            match_state: captured.match_state,
            fighters: FighterId::ALL.map(|id| {
                let mut fighter = crate::snapshot::FighterSnapshot::empty(id);
                if id.index() < 2 {
                    fighter.occupied = true;
                    fighter.active = true;
                    fighter.score = id.index() as i32;
                }
                fighter
            }),
            arena: captured.arena,
            allocators: crate::determinism::SimEntityKind::ALL
                .map(|kind| crate::snapshot::PoolAllocatorSnapshot::empty(kind, 0).unwrap())
                .into_iter()
                .collect(),
            dynamic_objects: Vec::new(),
            rng_streams: Vec::new(),
            stats: captured.stats,
        };

        let mut bad_rng = base.clone();
        bad_rng
            .rng_streams
            .push(crate::snapshot::NamedRngSnapshot::new(
                crate::determinism::RngStreamName::from_code(1),
                2,
                3,
            ));
        assert!(codec.prepare_restore(&world, &bad_rng).is_err());

        let mut bad_phase = base.clone();
        bad_phase.match_state.phase = MatchPhaseSnapshot::Countdown;
        assert!(codec.prepare_restore(&world, &bad_phase).is_err());

        let mut bad_arena = base;
        bad_arena.arena = ArenaRuntimeSnapshot::default();
        assert!(codec.prepare_restore(&world, &bad_arena).is_err());
        assert_eq!(world.resource::<MatchState>().timer_ticks, 0);
        assert_eq!(world.resource::<MatchTelemetry>().ring_outs, 2);
    }

    #[test]
    fn contract_header_validation_rejects_another_match_before_commit() {
        let codec = LiveMatchSnapshotCodec;
        let world = world();
        let captured = codec.capture_non_fighter(&world, SimTick(1)).unwrap();
        let mut snapshot = CanonicalSnapshot {
            header: SnapshotHeader::new(2, 1, 77, *b"wrong-match-id!!", SimTick(1), 0xAFC0_1234),
            match_state: captured.match_state,
            fighters: FighterId::ALL.map(crate::snapshot::FighterSnapshot::empty),
            arena: captured.arena,
            allocators: Vec::new(),
            dynamic_objects: Vec::new(),
            rng_streams: Vec::new(),
            stats: captured.stats,
        };
        snapshot.header.quantization_units_per_unit = crate::snapshot::SNAPSHOT_QUANTIZATION_UNITS;
        assert!(codec.prepare_restore(&world, &snapshot).is_err());
    }
}
