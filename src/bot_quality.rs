//! Opt-in deterministic smoke scenarios for developer-facing bot quality checks.
//!
//! This deliberately remains separate from the performance harness. It advances
//! live gameplay with a fixed delta and evaluates semantic action transitions and
//! authoritative damage telemetry rather than render timing or entity churn.

use std::time::Duration;

use bevy::prelude::*;
use bevy::time::TimeUpdateStrategy;

use crate::GameSet;
use crate::arena_defs::{TRAINING_GROUND_ARENA_INDEX, arena_definitions, set_active_arena_index};
use crate::bot_profiles::BotProfileCatalog;
use crate::characters::CharacterKind;
use crate::characters::CharacterMoveCatalog;
use crate::components::{
    BotBehaviorMode, BotBrain, Controller, Fighter, FighterAction, FighterActionState,
    FighterInput, FighterStats, LocalInputAssignment, ParticipantKind,
};
use crate::constants::FIGHTER_COUNT;
use crate::fighter::is_ringout_position;
use crate::game_state::{DEFAULT_REPLAY_SEED, LocalSetup, MatchPhase, MatchState, MatchTelemetry};
use crate::styles::FighterStyleKind;

const SCENARIO_ENV: &str = "AFC_BOT_QUALITY_SCENARIO";
const START_TIMEOUT_UPDATES: u32 = 3_600;
const FIXED_FRAME: Duration = Duration::from_nanos(16_666_667);
const PROGRESS_DISTANCE: f32 = 0.5;
const MAX_FRAME_TRAVEL: f32 = 2.0;
const MOVEMENT_THRESHOLD_SQUARED: f32 = 0.25;
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Adds no systems unless `AFC_BOT_QUALITY_SCENARIO` names a supported fixture.
pub struct BotQualityPlugin;

impl Plugin for BotQualityPlugin {
    fn build(&self, app: &mut App) {
        let Some(scenario) = BotQualityScenario::from_environment() else {
            return;
        };
        if std::env::var_os("AFC_PERF_SCENARIO").is_some() {
            panic!("{SCENARIO_ENV} cannot be combined with AFC_PERF_SCENARIO");
        }

        if scenario == BotQualityScenario::Tactics {
            app.add_systems(Update, run_tactics_quality.in_set(GameSet::Global));
            return;
        }

        app.insert_resource(TimeUpdateStrategy::ManualDuration(FIXED_FRAME))
            .insert_resource(BotQualityRun::new(scenario))
            .add_systems(
                Startup,
                configure_bot_quality
                    .before(crate::arena::setup_arena)
                    .before(crate::items::setup_items)
                    .before(crate::fighter::spawn_fighters),
            )
            .add_systems(Update, keep_bot_quality_running.in_set(GameSet::Global))
            .add_systems(Update, collect_bot_quality.in_set(GameSet::Presentation));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BotQualityScenario {
    Duel,
    FourBot,
    Tactics,
}

impl BotQualityScenario {
    fn from_environment() -> Option<Self> {
        let value = std::env::var_os(SCENARIO_ENV)?;
        let value = value
            .to_str()
            .unwrap_or_else(|| panic!("{SCENARIO_ENV} must be valid UTF-8"));
        Some(match value {
            "Duel" => Self::Duel,
            "FourBot" => Self::FourBot,
            "Tactics" => Self::Tactics,
            _ => panic!("unsupported {SCENARIO_ENV}={value:?}; expected Duel, FourBot, or Tactics"),
        })
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Duel => "Duel",
            Self::FourBot => "FourBot",
            Self::Tactics => "Tactics",
        }
    }

    const fn fighter_count(self) -> usize {
        match self {
            Self::Duel => 2,
            Self::FourBot => FIGHTER_COUNT,
            Self::Tactics => 0,
        }
    }

    const fn tick_budget(self) -> u32 {
        match self {
            Self::Duel => 1_800,
            Self::FourBot => 3_600,
            Self::Tactics => 0,
        }
    }

    const fn thresholds(self) -> BotQualityThresholds {
        match self {
            Self::Duel => BotQualityThresholds {
                movement_ticks: 120,
                accepted_attacks: 4,
                damaging_hits: 1,
                max_idle_ticks: 180,
                max_stuck_ticks: 240,
                max_no_hit_ticks: 900,
                action_families: 2,
            },
            Self::FourBot => BotQualityThresholds {
                movement_ticks: 240,
                accepted_attacks: 4,
                damaging_hits: 1,
                max_idle_ticks: 240,
                max_stuck_ticks: 300,
                max_no_hit_ticks: 1_200,
                action_families: 3,
            },
            Self::Tactics => BotQualityThresholds {
                movement_ticks: 0,
                accepted_attacks: 0,
                damaging_hits: 0,
                max_idle_ticks: 0,
                max_stuck_ticks: 0,
                max_no_hit_ticks: 0,
                action_families: 0,
            },
        }
    }

    const fn arena_reason(self) -> &'static str {
        match self {
            Self::Duel => "controlled continuous practice floor",
            Self::FourBot => {
                "controlled continuous floor isolates multi-character combat conversion"
            }
            Self::Tactics => "allocation-free authored forecast fixtures",
        }
    }
}

fn run_tactics_quality(
    move_catalog: Res<CharacterMoveCatalog>,
    profiles: Res<BotProfileCatalog>,
    mut finished: Local<bool>,
    mut app_exit: MessageWriter<AppExit>,
) {
    if *finished {
        return;
    }
    *finished = true;
    let report = crate::bot::run_tactics_quality_fixture(&move_catalog, &profiles);
    let percent = |successes: u32, trials: u32| {
        if trials == 0 {
            0.0
        } else {
            successes as f32 / trials as f32 * 100.0
        }
    };
    let failed_drop = if report.failed_tactic_first_half == 0 {
        0.0
    } else {
        (1.0 - report.failed_tactic_second_half as f32 / report.failed_tactic_first_half as f32)
            * 100.0
    };
    println!(
        concat!(
            "BOT_TACTICS_RESULT pass={} characters={} seeds={} ",
            "whiff_punish={}/{}({:.1}%) anti_air={}/{}({:.1}%) ",
            "safe_escape={}/{}({:.1}%) hit_confirm={}/{}({:.1}%) ",
            "guard_counter_before={} guard_counter_after={} guard_trials={} ",
            "failed_first_half={} failed_second_half={} failed_drop_percent={:.1} ",
            "half_trials={}"
        ),
        report.passed(),
        report.characters,
        report.seeds,
        report.whiff_punishes,
        report.whiff_trials,
        percent(report.whiff_punishes, report.whiff_trials),
        report.anti_airs,
        report.jump_trials,
        percent(report.anti_airs, report.jump_trials),
        report.safe_escapes,
        report.pressure_trials,
        percent(report.safe_escapes, report.pressure_trials),
        report.hit_confirm_follow_ups,
        report.hit_confirm_trials,
        percent(report.hit_confirm_follow_ups, report.hit_confirm_trials),
        report.guard_counter_before,
        report.guard_counter_after,
        report.guard_trials,
        report.failed_tactic_first_half,
        report.failed_tactic_second_half,
        failed_drop,
        report.failed_tactic_half_trials,
    );
    println!(
        "BOT_TACTICS_BEHAVIORS guard={} jump={} retreat={} whiff={} pressure={} passive={}",
        report.behavior_trials[3],
        report.behavior_trials[1],
        report.behavior_trials[2],
        report.behavior_trials[0],
        report.behavior_trials[4],
        report.behavior_trials[5],
    );
    app_exit.write(if report.passed() {
        AppExit::Success
    } else {
        AppExit::error()
    });
}

#[derive(Clone, Copy)]
struct BotQualityThresholds {
    movement_ticks: u32,
    accepted_attacks: u32,
    damaging_hits: u32,
    max_idle_ticks: u32,
    max_stuck_ticks: u32,
    max_no_hit_ticks: u32,
    action_families: u32,
}

#[derive(Clone, Copy, Default)]
struct FighterQualityMetrics {
    movement_ticks: u32,
    button_ticks: u32,
    accepted_attacks: u32,
    attempted_family_mask: u16,
    landed_family_mask: u16,
    damaging_hits: u32,
    damage_milli: u64,
    credited_ringouts: u32,
    unique_victim_mask: u8,
    first_attack_tick: Option<u32>,
    first_hit_tick: Option<u32>,
    idle_run: u32,
    max_idle_run: u32,
    no_progress_run: u32,
    max_stuck_run: u32,
    no_hit_run: u32,
    max_no_hit_run: u32,
    distance_milli: u64,
    previous_action: Option<FighterAction>,
    previous_health: Option<f32>,
    previous_position: Option<Vec3>,
    progress_anchor: Option<Vec3>,
    last_offensive_family: Option<u8>,
}

#[derive(Resource)]
struct BotQualityRun {
    scenario: BotQualityScenario,
    arena_index: usize,
    arena_name: &'static str,
    updates_seen: u32,
    tick: u32,
    started: bool,
    finished: bool,
    tape_hash: u64,
    last_damage: [f32; FIGHTER_COUNT],
    last_ring_outs: u32,
    last_falls: u32,
    credited_ringouts: u32,
    falls: u32,
    non_finite_observations: u32,
    fighters: [FighterQualityMetrics; FIGHTER_COUNT],
}

impl BotQualityRun {
    fn new(scenario: BotQualityScenario) -> Self {
        Self {
            scenario,
            arena_index: 0,
            arena_name: "unconfigured",
            updates_seen: 0,
            tick: 0,
            started: false,
            finished: false,
            tape_hash: FNV_OFFSET,
            last_damage: [0.0; FIGHTER_COUNT],
            last_ring_outs: 0,
            last_falls: 0,
            credited_ringouts: 0,
            falls: 0,
            non_finite_observations: 0,
            fighters: [FighterQualityMetrics::default(); FIGHTER_COUNT],
        }
    }
}

fn configure_bot_quality(
    mut run: ResMut<BotQualityRun>,
    mut setup: ResMut<LocalSetup>,
    mut state: ResMut<MatchState>,
    mut telemetry: ResMut<MatchTelemetry>,
) {
    let arena_index = TRAINING_GROUND_ARENA_INDEX;
    let characters = [
        CharacterKind::Cat,
        CharacterKind::Pig,
        CharacterKind::Bee,
        CharacterKind::Penguin,
    ];
    let fighter_count = run.scenario.fighter_count();

    setup.set_rule(1);
    setup.arena_index = arena_index;
    setup.selected_character_fighter = 0;
    setup.replay_seed = DEFAULT_REPLAY_SEED;
    for (fighter_id, slot) in setup.slots.iter_mut().enumerate() {
        slot.participant = if fighter_id < fighter_count {
            ParticipantKind::Bot
        } else {
            ParticipantKind::Closed
        };
        slot.input = LocalInputAssignment::Unassigned;
        slot.character = characters[fighter_id];
        slot.style = FighterStyleKind::Catalyst;
    }

    state.rule_index = setup.rule_index;
    state.rules = setup.active_rule();
    state.arena_index = arena_index;
    state.replay_seed = setup.replay_seed;
    state.apply_local_setup(&setup);
    telemetry.reset_for_seed(setup.replay_seed);
    set_active_arena_index(arena_index);
    state.request_rematch();

    run.arena_index = arena_index;
    run.arena_name = arena_definitions()[arena_index].name;
    println!(
        "BOT_QUALITY_CONFIG scenario={} fighters={} arena_index={} arena={:?} arena_reason={:?} seed={:#x} ticks={} fixed_hz=60",
        run.scenario.label(),
        fighter_count,
        arena_index,
        run.arena_name,
        run.scenario.arena_reason(),
        setup.replay_seed,
        run.scenario.tick_budget(),
    );
}

fn keep_bot_quality_running(
    mut state: ResMut<MatchState>,
    mut bots: Query<(&Controller, &mut BotBrain)>,
) {
    for (controller, mut brain) in &mut bots {
        if controller.is_bot() && brain.behavior != BotBehaviorMode::Combatant {
            crate::bot::start_bot_combat_ai(&mut brain);
        }
    }
    if state.phase == MatchPhase::Results {
        state.request_rematch();
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_bot_quality(
    state: Res<MatchState>,
    telemetry: Res<MatchTelemetry>,
    fighters: Query<(
        &Fighter,
        &Controller,
        &BotBrain,
        &FighterInput,
        &FighterActionState,
        &FighterStats,
        &Transform,
    )>,
    mut run: ResMut<BotQualityRun>,
    mut app_exit: MessageWriter<AppExit>,
) {
    if run.finished {
        return;
    }
    run.updates_seen = run.updates_seen.saturating_add(1);

    let expected = run.scenario.fighter_count();
    let active_combatants = fighters
        .iter()
        .filter(|(fighter, controller, brain, ..)| {
            fighter.id < expected
                && controller.is_bot()
                && brain.behavior == BotBehaviorMode::Combatant
        })
        .count();
    if state.phase != MatchPhase::Fighting || active_combatants != expected {
        if run.started {
            reset_transient_tracking(&mut run, expected);
        }
        if !run.started && run.updates_seen >= START_TIMEOUT_UPDATES {
            println!(
                "BOT_QUALITY_RESULT scenario={} pass=false ticks=0 failures=1",
                run.scenario.label()
            );
            println!(
                "BOT_QUALITY_FAILURE code=SCENARIO_DID_NOT_START phase={:?} active_combatants={} expected={}",
                state.phase, active_combatants, expected
            );
            run.finished = true;
            app_exit.write(AppExit::error());
        }
        return;
    }

    if !run.started {
        run.started = true;
        run.last_damage = telemetry.damage_by_fighter;
        run.last_ring_outs = telemetry.ring_outs;
        run.last_falls = telemetry.falls;
        println!(
            "BOT_QUALITY_SAMPLE_BEGIN scenario={} ticks={}",
            run.scenario.label(),
            run.scenario.tick_budget()
        );
    }

    run.tick = run.tick.saturating_add(1);
    let tick = run.tick;
    let mut frame_hash = run.tape_hash;
    mix_hash(&mut frame_hash, tick as u64);
    let mut victim_attackers = [None; FIGHTER_COUNT];
    let mut ringout_attackers = [None; FIGHTER_COUNT];
    let mut targetable = [false; FIGHTER_COUNT];
    let arena = &arena_definitions()[run.arena_index];

    for (fighter, controller, _, _, action, stats, _) in &fighters {
        if fighter.id < expected && controller.is_bot() {
            targetable[fighter.id] = state.fighter_can_participate(fighter.id)
                && stats.respawn_timer <= 0.0
                && !matches!(
                    action.action,
                    FighterAction::RingOut | FighterAction::Respawning
                );
        }
    }

    for (fighter, controller, _, input, action, stats, transform) in &fighters {
        let fighter_id = fighter.id;
        if fighter_id >= expected || !controller.is_bot() {
            continue;
        }

        let position = transform.translation;
        if !position.is_finite() || !stats.health.is_finite() || !stats.stamina.is_finite() {
            run.non_finite_observations = run.non_finite_observations.saturating_add(1);
        }
        mix_hash(&mut frame_hash, fighter_id as u64);
        mix_hash(&mut frame_hash, quantize(position.x, 1_000.0));
        mix_hash(&mut frame_hash, quantize(position.y, 1_000.0));
        mix_hash(&mut frame_hash, quantize(position.z, 1_000.0));
        mix_hash(&mut frame_hash, quantize(stats.health, 1_000.0));
        mix_hash(&mut frame_hash, action.action as u64);
        mix_hash(&mut frame_hash, quantize(input.movement.x, 4_096.0));
        mix_hash(&mut frame_hash, quantize(input.movement.y, 4_096.0));

        let metrics = &mut run.fighters[fighter_id];
        let action_changed = metrics.previous_action != Some(action.action);
        if action_changed {
            if let Some(family) = offensive_family(action.action) {
                metrics.accepted_attacks = metrics.accepted_attacks.saturating_add(1);
                metrics.attempted_family_mask |= 1_u16 << family;
                metrics.last_offensive_family = Some(family);
                metrics.first_attack_tick.get_or_insert(tick);
            }
            if action.action == FighterAction::RingOut && is_ringout_position(position, arena) {
                ringout_attackers[fighter_id] = stats
                    .last_attacker
                    .filter(|attacker| *attacker < expected && *attacker != fighter_id);
            }
        }

        if let Some(previous_health) = metrics.previous_health
            && previous_health - stats.health > 0.001
        {
            victim_attackers[fighter_id] = stats
                .last_attacker
                .filter(|attacker| *attacker < expected && *attacker != fighter_id);
        }

        let movement_requested = input.movement.length_squared() >= MOVEMENT_THRESHOLD_SQUARED;
        let button_active = input.jump
            || input.dash
            || input.light
            || input.heavy
            || input.grab
            || input.guard
            || input.ultimate
            || input.special;
        let has_valid_target = (0..expected).any(|target_id| {
            target_id != fighter_id
                && targetable[target_id]
                && state.combat_target_allowed_for_state(fighter_id, target_id)
        });
        let controllable = state.fighter_can_participate(fighter_id)
            && action_is_controllable(action.action)
            && stats.respawn_timer <= 0.0;
        let meaningful_action =
            !matches!(action.action, FighterAction::Idle | FighterAction::Moving);

        if movement_requested {
            metrics.movement_ticks = metrics.movement_ticks.saturating_add(1);
        }
        if button_active {
            metrics.button_ticks = metrics.button_ticks.saturating_add(1);
        }

        if controllable
            && has_valid_target
            && !movement_requested
            && !button_active
            && !meaningful_action
        {
            metrics.idle_run = metrics.idle_run.saturating_add(1);
            metrics.max_idle_run = metrics.max_idle_run.max(metrics.idle_run);
        } else {
            metrics.idle_run = 0;
        }

        if controllable && has_valid_target && movement_requested {
            let anchor = metrics.progress_anchor.get_or_insert(position);
            if planar_distance(*anchor, position) >= PROGRESS_DISTANCE {
                metrics.progress_anchor = Some(position);
                metrics.no_progress_run = 0;
            } else {
                metrics.no_progress_run = metrics.no_progress_run.saturating_add(1);
                metrics.max_stuck_run = metrics.max_stuck_run.max(metrics.no_progress_run);
            }
        } else {
            metrics.progress_anchor = None;
            metrics.no_progress_run = 0;
        }

        if controllable && has_valid_target && metrics.first_hit_tick.is_some() {
            metrics.no_hit_run = metrics.no_hit_run.saturating_add(1);
            metrics.max_no_hit_run = metrics.max_no_hit_run.max(metrics.no_hit_run);
        } else if !has_valid_target {
            metrics.no_hit_run = 0;
        }

        if let Some(previous_position) = metrics.previous_position {
            let travelled = planar_distance(previous_position, position);
            if travelled.is_finite() && travelled <= MAX_FRAME_TRAVEL {
                metrics.distance_milli = metrics
                    .distance_milli
                    .saturating_add((travelled * 1_000.0).round() as u64);
            }
        }
        metrics.previous_action = Some(action.action);
        metrics.previous_health = Some(stats.health);
        metrics.previous_position = Some(position);
    }

    run.tape_hash = frame_hash;

    for victim_id in 0..expected {
        if let Some(attacker_id) = victim_attackers[victim_id] {
            run.fighters[attacker_id].unique_victim_mask |= 1_u8 << victim_id;
        }
    }

    for fighter_id in 0..expected {
        let current = telemetry.damage_by_fighter[fighter_id];
        let previous = run.last_damage[fighter_id];
        let delta = if current + 0.001 >= previous {
            (current - previous).max(0.0)
        } else {
            current.max(0.0)
        };
        run.last_damage[fighter_id] = current;
        if delta > 0.001 {
            let metrics = &mut run.fighters[fighter_id];
            metrics.damaging_hits = metrics.damaging_hits.saturating_add(1);
            metrics.damage_milli = metrics
                .damage_milli
                .saturating_add((delta * 1_000.0).round() as u64);
            metrics.first_hit_tick.get_or_insert(tick);
            metrics.no_hit_run = 0;
            if let Some(family) = metrics.last_offensive_family {
                metrics.landed_family_mask |= 1_u16 << family;
            }
        }
    }

    let ringout_delta = monotonic_counter_delta(telemetry.ring_outs, run.last_ring_outs);
    let fall_delta = monotonic_counter_delta(telemetry.falls, run.last_falls);
    run.credited_ringouts = run.credited_ringouts.saturating_add(ringout_delta);
    run.falls = run.falls.saturating_add(fall_delta);
    run.last_ring_outs = telemetry.ring_outs;
    run.last_falls = telemetry.falls;
    let mut unattributed_ringouts = ringout_delta;
    for attacker_id in ringout_attackers.into_iter().flatten() {
        if unattributed_ringouts == 0 {
            break;
        }
        run.fighters[attacker_id].credited_ringouts = run.fighters[attacker_id]
            .credited_ringouts
            .saturating_add(1);
        unattributed_ringouts -= 1;
    }

    if run.tick >= run.scenario.tick_budget() {
        finish_bot_quality(&mut run, &mut app_exit);
    }
}

fn reset_transient_tracking(run: &mut BotQualityRun, fighter_count: usize) {
    for metrics in &mut run.fighters[..fighter_count] {
        metrics.idle_run = 0;
        metrics.no_progress_run = 0;
        metrics.no_hit_run = 0;
        metrics.previous_action = None;
        metrics.previous_health = None;
        metrics.previous_position = None;
        metrics.progress_anchor = None;
        metrics.last_offensive_family = None;
    }
}

fn finish_bot_quality(run: &mut BotQualityRun, app_exit: &mut MessageWriter<AppExit>) {
    let thresholds = run.scenario.thresholds();
    let expected = run.scenario.fighter_count();
    let mut failures = Vec::new();
    let mut combined_families = 0_u16;

    if run.non_finite_observations > 0 {
        failures.push(format!(
            "NON_FINITE_STATE:count={}",
            run.non_finite_observations
        ));
    }

    for fighter_id in 0..expected {
        let metrics = run.fighters[fighter_id];
        combined_families |= metrics.attempted_family_mask;
        if metrics.movement_ticks < thresholds.movement_ticks {
            failures.push(format!(
                "INSUFFICIENT_MOVEMENT:fighter={fighter_id}:actual={}:required={}",
                metrics.movement_ticks, thresholds.movement_ticks
            ));
        }
        if metrics.accepted_attacks < thresholds.accepted_attacks {
            failures.push(format!(
                "NO_ACCEPTED_ATTACKS:fighter={fighter_id}:actual={}:required={}",
                metrics.accepted_attacks, thresholds.accepted_attacks
            ));
        }
        if metrics.damaging_hits < thresholds.damaging_hits {
            failures.push(format!(
                "NO_DAMAGING_HITS:fighter={fighter_id}:actual={}:required={}",
                metrics.damaging_hits, thresholds.damaging_hits
            ));
        }
        if metrics.damage_milli == 0 {
            failures.push(format!("NO_DAMAGE:fighter={fighter_id}"));
        }
        if metrics.max_idle_run > thresholds.max_idle_ticks {
            failures.push(format!(
                "IDLE:fighter={fighter_id}:max={}:limit={}",
                metrics.max_idle_run, thresholds.max_idle_ticks
            ));
        }
        if metrics.max_stuck_run > thresholds.max_stuck_ticks {
            failures.push(format!(
                "STUCK:fighter={fighter_id}:max={}:limit={}",
                metrics.max_stuck_run, thresholds.max_stuck_ticks
            ));
        }
        if metrics.max_no_hit_run > thresholds.max_no_hit_ticks {
            failures.push(format!(
                "NO_HIT_GAP:fighter={fighter_id}:max={}:limit={}",
                metrics.max_no_hit_run, thresholds.max_no_hit_ticks
            ));
        }
    }

    let family_count = combined_families.count_ones();
    if family_count < thresholds.action_families {
        failures.push(format!(
            "LOW_ACTION_DIVERSITY:actual={family_count}:required={}",
            thresholds.action_families
        ));
    }
    let passed = failures.is_empty();
    println!(
        "BOT_QUALITY_RESULT version=1 scenario={} seed={:#x} arena_index={} ticks={} pass={} tape_hash={:#018x} credited_ringouts={} falls={} action_families={} failures={}",
        run.scenario.label(),
        DEFAULT_REPLAY_SEED,
        run.arena_index,
        run.tick,
        passed,
        run.tape_hash,
        run.credited_ringouts,
        run.falls,
        family_count,
        failures.len(),
    );
    for fighter_id in 0..expected {
        let metrics = run.fighters[fighter_id];
        println!(
            "BOT_QUALITY_FIGHTER id={} movement_ticks={} button_ticks={} accepted_attacks={} damaging_hits={} damage_milli={} ringouts={} attempted_mask={:#x} landed_mask={:#x} unique_victims={:#x} first_attack_tick={} first_hit_tick={} max_idle_ticks={} max_stuck_ticks={} max_no_hit_ticks={} distance_milli={}",
            fighter_id,
            metrics.movement_ticks,
            metrics.button_ticks,
            metrics.accepted_attacks,
            metrics.damaging_hits,
            metrics.damage_milli,
            metrics.credited_ringouts,
            metrics.attempted_family_mask,
            metrics.landed_family_mask,
            metrics.unique_victim_mask,
            optional_tick(metrics.first_attack_tick),
            optional_tick(metrics.first_hit_tick),
            metrics.max_idle_run,
            metrics.max_stuck_run,
            metrics.max_no_hit_run,
            metrics.distance_milli,
        );
    }
    for failure in &failures {
        println!("BOT_QUALITY_FAILURE code={failure}");
    }

    run.finished = true;
    app_exit.write(if passed {
        AppExit::Success
    } else {
        AppExit::error()
    });
}

fn offensive_family(action: FighterAction) -> Option<u8> {
    match action {
        FighterAction::LightAttack1
        | FighterAction::LightAttack2
        | FighterAction::ComboFinisher => Some(0),
        FighterAction::HeavyAttack | FighterAction::HeavyAttack2 => Some(1),
        FighterAction::DashAttack | FighterAction::JumpAttack | FighterAction::JumpHeavyAttack => {
            Some(2)
        }
        FighterAction::GrabStartup | FighterAction::Throwing => Some(3),
        FighterAction::UltimateStartup | FighterAction::UltimateRush => Some(4),
        FighterAction::SpecialCast => Some(5),
        FighterAction::ItemSwing | FighterAction::ItemThrow => Some(6),
        FighterAction::GuardCounter => Some(7),
        _ => None,
    }
}

fn action_is_controllable(action: FighterAction) -> bool {
    !matches!(
        action,
        FighterAction::LandingRecovery
            | FighterAction::UltimateVictim
            | FighterAction::Grabbed
            | FighterAction::Hitstun
            | FighterAction::Knockdown
            | FighterAction::GetUp
            | FighterAction::GuardBroken
            | FighterAction::RingOut
            | FighterAction::Respawning
    )
}

fn planar_distance(first: Vec3, second: Vec3) -> f32 {
    Vec2::new(first.x - second.x, first.z - second.z).length()
}

fn quantize(value: f32, scale: f32) -> u64 {
    if value.is_finite() {
        (value * scale).round() as i64 as u64
    } else {
        u64::MAX
    }
}

fn mix_hash(hash: &mut u64, value: u64) {
    *hash ^= value;
    *hash = hash.wrapping_mul(FNV_PRIME);
}

fn monotonic_counter_delta(current: u32, previous: u32) -> u32 {
    if current >= previous {
        current - previous
    } else {
        current
    }
}

fn optional_tick(value: Option<u32>) -> i64 {
    value.map(i64::from).unwrap_or(-1)
}
