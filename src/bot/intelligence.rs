use bevy::prelude::*;

use crate::arena::{SplitCausewayDoorState, arena_hazard_is_active_for_kind_ticks};
use crate::arena_defs::{ArenaDefinition, arena_definition};
use crate::bot_profiles::{
    BOT_DECISION_HZ, BOT_OPPONENT_HISTORY_TICKS, BotProfile, BotProfileCatalog, BotProfileId,
};
use crate::canonical_math::{vec2_dot, vec2_length, vec2_length_squared, vec2_normalize_or_zero};
#[cfg(any(test, feature = "bot-quality"))]
use crate::characters::CHARACTER_KINDS;
use crate::characters::{CharacterKind, CharacterMoveCatalog, CharacterMoveSlot, FighterCharacter};
use crate::components::{
    BotBehaviorMode, BotBrain, BotMovementPlan, Fighter, FighterAction, FighterActionState,
    FighterContactState, FighterInput, FighterMotor, FighterSpecialState, FighterStats,
    SimPosition,
};
use crate::constants::{
    COMBO_QUEUE_END, COMBO_QUEUE_START, FIGHTER_COUNT, ITEM_PICKUP_RANGE, MAX_STAMINA,
};
use crate::determinism::SimEntityId;
use crate::ecs_identity::StableSimEntity;
use crate::equipment::{FighterEquipment, LoadoutContext};
use crate::game_state::MatchState;
use crate::items::{ArenaItem, ItemKind, ItemState};
use crate::simulation::{ElapsedTicks, SIM_HZ_U32, TickTimer};
use crate::specials::{ActiveSpecial, SpecialKind};
use crate::styles::{FighterStyle, style_tuning};
use crate::techniques::{
    TechniqueButton, TechniqueId, TechniqueMatchContext, TechniquePrediction, TechniqueStatus,
    active_technique_definition_in_catalog, technique_prediction_for_context_in_catalog,
    technique_slot_for_loadout,
};

use super::tactics::{
    ActiveTacticPlan, CombatForecastState, DASH_FORECAST_TICKS, ForecastAction,
    ForecastActionPhase, ForecastFollowUp, ForecastMoveFacts, MAX_FORECAST_FOLLOW_UPS,
    MAX_PLAN_STEPS, MAX_RESPONSE_BRANCHES, MAX_TACTICAL_ACTIONS, OpponentResponse,
    OpponentResponseModel, PhaseInputs, PlanBranch, PlanInvalidationInputs, PlanMovement, PlanStep,
    PreviousOutcome, ResponsePrediction, TacticId, TacticOutcome, TacticalPhase, bounded_velocity,
    classify_phase, plan_is_invalid, response_context, response_family, score_forecast_plan,
    tactic_for_action, update_tactic_bias,
};

use super::navigation::NavigationBlockers;
use super::{
    BotDifficulty, BotHeldItemDecision, BotNavigationCache, BotRecoveryDecision, BotTargetSnapshot,
    arena_hazard_avoid_radius, arena_hazard_avoidance, bot_held_item_decision, bot_personality,
    bot_pickup_score, bot_range_band, bot_recovery_decision, bot_should_guard_threat,
    bot_should_jump_for_elevation, defensive_away_from, item_avoidance_radius,
    special_avoid_radius,
};

const DECISION_STEP: f32 = 1.0 / BOT_DECISION_HZ as f32;
const SIM_TICKS_PER_DECISION: u8 = (SIM_HZ_U32 / BOT_DECISION_HZ) as u8;
const ACTIONABLE_RESPONSE_PROBABILITY: f32 = 0.20;
const MEMORY_TICKS: usize = BOT_OPPONENT_HISTORY_TICKS as usize;
#[cfg(feature = "perf")]
const PLANNER_TIMING_CAPACITY: usize = 4_096;

const OBS_ATTACK: u8 = 1 << 0;
const OBS_GUARD: u8 = 1 << 1;
const OBS_GRAB: u8 = 1 << 2;
const OBS_AIR: u8 = 1 << 3;

#[derive(Clone, Copy, Debug)]
struct FighterSnapshot {
    id: usize,
    position: Vec3,
    facing: Vec3,
    velocity: Vec3,
    action: FighterAction,
    action_elapsed: f32,
    technique_id: Option<TechniqueId>,
    confirmed_hit: bool,
    confirmed_guard: bool,
    cancel_window_open: bool,
    branch_window_open: bool,
    recovery_remaining: f32,
    active_prediction: Option<TechniquePrediction>,
    character: CharacterKind,
    grounded: bool,
    health: f32,
    health_delta: f32,
    stamina: f32,
    stamina_delta: f32,
    targetable_by: [bool; FIGHTER_COUNT],
}

impl Default for FighterSnapshot {
    fn default() -> Self {
        Self {
            id: 0,
            position: Vec3::ZERO,
            facing: Vec3::X,
            velocity: Vec3::ZERO,
            action: FighterAction::Idle,
            action_elapsed: 0.0,
            technique_id: None,
            confirmed_hit: false,
            confirmed_guard: false,
            cancel_window_open: false,
            branch_window_open: false,
            recovery_remaining: 0.0,
            active_prediction: None,
            character: CharacterKind::Cat,
            grounded: true,
            health: 100.0,
            health_delta: 0.0,
            stamina: MAX_STAMINA,
            stamina_delta: 0.0,
            targetable_by: [false; FIGHTER_COUNT],
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ItemSnapshot {
    stable_id: SimEntityId,
    kind: ItemKind,
    position: Vec3,
    loose: bool,
    threat_owner: Option<usize>,
    threat_radius: f32,
}

#[derive(Clone, Copy, Debug)]
struct SpecialSnapshot {
    stable_id: SimEntityId,
    owner_id: usize,
    kind: SpecialKind,
    position: Vec3,
}

#[derive(Default)]
pub(crate) struct BotSnapshotBuffer {
    replay_seed: u64,
    arena_index: usize,
    hazard_elapsed: ElapsedTicks,
    fighters: [Option<FighterSnapshot>; FIGHTER_COUNT],
    items: Vec<ItemSnapshot>,
    specials: Vec<SpecialSnapshot>,
    navigation_blockers: NavigationBlockers,
    previous_replay_seed: Option<u64>,
    previous_health: [Option<f32>; FIGHTER_COUNT],
    previous_stamina: [Option<f32>; FIGHTER_COUNT],
}

fn snapshot_arena(snapshot: &BotSnapshotBuffer) -> &'static ArenaDefinition {
    arena_definition(snapshot.arena_index)
}

fn snapshot_edge_danger(snapshot: &BotSnapshotBuffer, position: Vec3) -> f32 {
    super::edge_danger_for_arena(position, snapshot_arena(snapshot))
}

fn snapshot_edge_steering(snapshot: &BotSnapshotBuffer, position: Vec3, movement: Vec2) -> Vec2 {
    super::apply_edge_steering_for_arena(position, movement, snapshot_arena(snapshot))
}

pub(super) fn build_world_snapshot(
    snapshot: &mut BotSnapshotBuffer,
    state: &MatchState,
    arena_index: usize,
    hazard_elapsed: ElapsedTicks,
    fighters: &Query<(
        &Fighter,
        &SimPosition,
        &FighterActionState,
        &FighterMotor,
        &FighterStats,
        &FighterCharacter,
        &FighterStyle,
        &FighterEquipment,
        Option<&FighterContactState>,
    )>,
    move_catalog: &CharacterMoveCatalog,
    items: &[(&StableSimEntity, &ArenaItem)],
    specials: &[(&StableSimEntity, (&ActiveSpecial, &SimPosition))],
) {
    if snapshot.previous_replay_seed != Some(state.replay_seed) {
        snapshot.previous_replay_seed = Some(state.replay_seed);
        snapshot.previous_health.fill(None);
        snapshot.previous_stamina.fill(None);
    }
    snapshot.replay_seed = state.replay_seed;
    snapshot.arena_index = arena_index;
    snapshot.hazard_elapsed = hazard_elapsed;
    snapshot.fighters.fill(None);
    snapshot.items.clear();
    snapshot.specials.clear();
    snapshot.navigation_blockers.clear();

    for (fighter, position, action, motor, stats, character, style, equipment, contact) in fighters
    {
        if fighter.id >= FIGHTER_COUNT {
            continue;
        }
        let loadout = LoadoutContext::for_character(character.kind, style.kind, equipment.kind);
        let active_prediction = active_technique_definition_in_catalog(
            action.action,
            action.technique_id,
            loadout,
            move_catalog,
        )
        .map(|definition| definition.prediction());
        let health_delta =
            snapshot.previous_health[fighter.id].map_or(0.0, |previous| stats.health - previous);
        let stamina_delta =
            snapshot.previous_stamina[fighter.id].map_or(0.0, |previous| stats.stamina - previous);
        snapshot.previous_health[fighter.id] = Some(stats.health);
        snapshot.previous_stamina[fighter.id] = Some(stats.stamina);
        snapshot.fighters[fighter.id] = Some(FighterSnapshot {
            id: fighter.id,
            position: position.translation,
            facing: motor.facing,
            velocity: motor.velocity,
            action: action.action,
            action_elapsed: action.elapsed.as_seconds(),
            technique_id: action.technique_id,
            confirmed_hit: action.confirmed_hit,
            confirmed_guard: contact.is_some_and(|contact| contact.guarded_for(action)),
            cancel_window_open: action.cancel_window_open,
            branch_window_open: action.branch_window_open,
            recovery_remaining: active_prediction.map_or(0.0, |prediction| {
                prediction
                    .recover_at_ms
                    .saturating_sub(action.elapsed.as_millis_floor()) as f32
                    / 1_000.0
            }),
            active_prediction,
            character: character.kind,
            grounded: motor.grounded,
            health: stats.health,
            health_delta,
            stamina: stats.stamina,
            stamina_delta,
            targetable_by: std::array::from_fn(|attacker_id| {
                state.fighter_can_participate(fighter.id)
                    && state.combat_target_allowed_for_state(attacker_id, fighter.id)
            }),
        });
    }

    for (stable, item) in items {
        let threat = item_avoidance_radius(item);
        snapshot.items.push(ItemSnapshot {
            stable_id: stable.id(),
            kind: item.kind,
            position: item.position,
            loose: matches!(item.state, ItemState::Loose) && !item.pickup_lockout.active(),
            threat_owner: threat.map(|value| value.0),
            threat_radius: threat.map_or(0.0, |value| value.1),
        });
    }
    snapshot
        .items
        .sort_by_key(|item| (item.stable_id.index(), item.stable_id.generation()));

    for (stable, (special, position)) in specials {
        snapshot.specials.push(SpecialSnapshot {
            stable_id: stable.id(),
            owner_id: special.owner.index(),
            kind: special.kind,
            position: position.translation,
        });
    }
    snapshot
        .specials
        .sort_by_key(|special| (special.stable_id.index(), special.stable_id.generation()));

    for hazard in arena_definition(arena_index).hazards {
        if arena_hazard_is_active_for_kind_ticks(hazard_elapsed, hazard) {
            let _ = snapshot.navigation_blockers.push(
                Vec2::new(hazard.center.x, hazard.center.z),
                arena_hazard_avoid_radius(hazard),
            );
        }
    }
    for special in &snapshot.specials {
        if let Some(radius) = special_avoid_radius(special.kind) {
            let _ = snapshot
                .navigation_blockers
                .push(Vec2::new(special.position.x, special.position.z), radius);
        }
    }
    for item in &snapshot.items {
        if item.threat_owner.is_some() {
            let _ = snapshot.navigation_blockers.push(
                Vec2::new(item.position.x, item.position.z),
                item.threat_radius,
            );
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum BotGoal {
    #[default]
    Idle,
    Survive,
    RegainStamina,
    Reposition,
    Approach,
    Pressure,
    Punish,
    Disengage,
    CollectItem,
    UseItem,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum BotSemanticAction {
    Light,
    Heavy,
    Grab,
    Jump,
    Dash,
    Pickup,
    ItemLight,
    ItemHeavy,
    SpecialProjectile,
    SpecialTrap,
    SpecialHazard,
    SpecialShockwave,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum BotCommitmentTrace {
    #[default]
    None,
    Waiting,
    Accepted,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum BotDecisionReason {
    #[default]
    None,
    Safety,
    LowHealth,
    LowStamina,
    Spacing,
    Vulnerability,
    EdgePressure,
    ItemOpportunity,
    HeldItem,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct BotDecisionTrace {
    pub(crate) target_id: Option<usize>,
    pub(crate) target_distance: f32,
    pub(crate) goal: BotGoal,
    pub(crate) action: Option<BotSemanticAction>,
    pub(crate) reaction_gated: bool,
    pub(crate) reaction_delay_ticks: u8,
    pub(crate) commitment: BotCommitmentTrace,
    pub(crate) reason: BotDecisionReason,
    pub(crate) utility_score: f32,
    pub(crate) phase: TacticalPhase,
    pub(crate) tactic: Option<TacticId>,
    pub(crate) predicted_responses: [ResponsePrediction; MAX_RESPONSE_BRANCHES],
    pub(crate) forecast_score: f32,
    pub(crate) plan_step: u8,
    pub(crate) branch: PlanBranch,
    pub(crate) learned_bias: f32,
}

#[derive(Clone, Copy, Debug, Default)]
struct BotIntent {
    goal: BotGoal,
    movement: Vec2,
    guard: bool,
}

#[derive(Clone, Copy, Debug)]
struct BotCommitment {
    action: BotSemanticAction,
    expected_action: Option<FighterAction>,
    expires_tick: u64,
    accepted: bool,
    last_press_tick: Option<u64>,
}

struct BotOpponentMemory {
    samples: [u8; MEMORY_TICKS],
    cursor: usize,
    len: usize,
    totals: [u16; 4],
    raw_action: Option<FighterAction>,
    raw_technique: Option<TechniqueId>,
    pending_action: Option<FighterAction>,
    pending_technique: Option<TechniqueId>,
    pending_prediction: Option<TechniquePrediction>,
    pending_action_elapsed: f32,
    pending_since_tick: u64,
    pending_delay: u64,
    perceived_action: Option<FighterAction>,
    perceived_technique: Option<TechniqueId>,
    perceived_prediction: Option<TechniquePrediction>,
    perceived_action_elapsed: f32,
    last_opener: Option<FighterAction>,
    repeated_openers: u8,
    last_position: Option<Vec3>,
    last_position_tick: u64,
    velocity: Vec2,
    responses: OpponentResponseModel,
}

impl Default for BotOpponentMemory {
    fn default() -> Self {
        Self {
            samples: [0; MEMORY_TICKS],
            cursor: 0,
            len: 0,
            totals: [0; 4],
            raw_action: None,
            raw_technique: None,
            pending_action: None,
            pending_technique: None,
            pending_prediction: None,
            pending_action_elapsed: 0.0,
            pending_since_tick: 0,
            pending_delay: 0,
            perceived_action: None,
            perceived_technique: None,
            perceived_prediction: None,
            perceived_action_elapsed: 0.0,
            last_opener: None,
            repeated_openers: 0,
            last_position: None,
            last_position_tick: 0,
            velocity: Vec2::ZERO,
            responses: OpponentResponseModel::default(),
        }
    }
}

impl BotOpponentMemory {
    fn observe_position(&mut self, position: Vec3, tick: u64) {
        if let Some(previous) = self.last_position {
            let tick_delta = tick.saturating_sub(self.last_position_tick);
            let delta = Vec2::new(position.x - previous.x, position.z - previous.z);
            if tick_delta == 0 || vec2_length(delta) > 6.0 {
                self.velocity = Vec2::ZERO;
            } else {
                let seconds = tick_delta as f32 * DECISION_STEP;
                let mut sample = delta / seconds.max(DECISION_STEP);
                if vec2_length(sample) > 12.0 {
                    sample = vec2_normalize_or_zero(sample) * 12.0;
                }
                self.velocity = self.velocity * 0.55 + sample * 0.45;
            }
        }
        self.last_position = Some(position);
        self.last_position_tick = tick;
    }

    fn push(&mut self, flags: u8) {
        if self.len == MEMORY_TICKS {
            let old = self.samples[self.cursor];
            for (index, bit) in [OBS_ATTACK, OBS_GUARD, OBS_GRAB, OBS_AIR]
                .into_iter()
                .enumerate()
            {
                if old & bit != 0 {
                    self.totals[index] -= 1;
                }
            }
        } else {
            self.len += 1;
        }
        self.samples[self.cursor] = flags;
        self.cursor = (self.cursor + 1) % MEMORY_TICKS;
        for (index, bit) in [OBS_ATTACK, OBS_GUARD, OBS_GRAB, OBS_AIR]
            .into_iter()
            .enumerate()
        {
            if flags & bit != 0 {
                self.totals[index] += 1;
            }
        }
    }

    fn rate(&self, index: usize) -> f32 {
        if self.len == 0 {
            0.0
        } else {
            self.totals[index] as f32 / self.len as f32
        }
    }
}

struct BotRuntimeSlot {
    initialized: bool,
    last_seen_frame: u64,
    last_behavior: Option<BotBehaviorMode>,
    profile: Option<BotProfile>,
    decision_subticks: u8,
    decision_tick: u64,
    target_id: Option<usize>,
    intent: BotIntent,
    trace: BotDecisionTrace,
    commitment: Option<BotCommitment>,
    opponents: [BotOpponentMemory; FIGHTER_COUNT],
    attack_ready_tick: u64,
    dash_ready_tick: u64,
    strafe_sign: f32,
    tactical_phase: TacticalPhase,
    active_plan: Option<ActiveTacticPlan>,
    tactic_biases: [f32; TacticId::COUNT],
    last_outcome: PreviousOutcome,
    last_outcome_tick: u64,
    observed_action: Option<FighterAction>,
    observed_technique: Option<TechniqueId>,
    observed_action_outcome: PreviousOutcome,
    observed_outcome_published: bool,
    last_move_envelope: f32,
    commitment_rejected: bool,
    #[cfg(feature = "perf")]
    planner_timing_ns: [u32; PLANNER_TIMING_CAPACITY],
    #[cfg(feature = "perf")]
    planner_timing_len: usize,
    #[cfg(feature = "perf")]
    planner_timing_cursor: usize,
}

impl Default for BotRuntimeSlot {
    fn default() -> Self {
        Self {
            initialized: false,
            last_seen_frame: 0,
            last_behavior: None,
            profile: None,
            decision_subticks: 0,
            decision_tick: 0,
            target_id: None,
            intent: BotIntent::default(),
            trace: BotDecisionTrace::default(),
            commitment: None,
            opponents: std::array::from_fn(|_| BotOpponentMemory::default()),
            attack_ready_tick: 0,
            dash_ready_tick: 0,
            strafe_sign: -1.0,
            tactical_phase: TacticalPhase::Neutral,
            active_plan: None,
            tactic_biases: [0.0; TacticId::COUNT],
            last_outcome: PreviousOutcome::None,
            last_outcome_tick: 0,
            observed_action: None,
            observed_technique: None,
            observed_action_outcome: PreviousOutcome::None,
            observed_outcome_published: false,
            last_move_envelope: 1.5,
            commitment_rejected: false,
            #[cfg(feature = "perf")]
            planner_timing_ns: [0; PLANNER_TIMING_CAPACITY],
            #[cfg(feature = "perf")]
            planner_timing_len: 0,
            #[cfg(feature = "perf")]
            planner_timing_cursor: 0,
        }
    }
}

impl BotRuntimeSlot {
    #[cfg(feature = "perf")]
    fn record_planner_timing(&mut self, elapsed: std::time::Duration) {
        self.planner_timing_ns[self.planner_timing_cursor] =
            elapsed.as_nanos().min(u128::from(u32::MAX)) as u32;
        self.planner_timing_cursor = (self.planner_timing_cursor + 1) % PLANNER_TIMING_CAPACITY;
        self.planner_timing_len = (self.planner_timing_len + 1).min(PLANNER_TIMING_CAPACITY);
    }
}

#[inline]
fn advance_decision_clock(slot: &mut BotRuntimeSlot, appeared: bool) -> bool {
    let advances = if appeared {
        slot.decision_subticks = 0;
        true
    } else {
        slot.decision_subticks = slot.decision_subticks.saturating_add(1);
        if slot.decision_subticks >= SIM_TICKS_PER_DECISION {
            slot.decision_subticks = 0;
            true
        } else {
            false
        }
    };
    if advances {
        slot.decision_tick = slot.decision_tick.wrapping_add(1);
    }
    advances
}

#[derive(Resource)]
pub(crate) struct BotRuntimeStore {
    frame: u64,
    seed_initialized: bool,
    replay_seed: u64,
    slots: [BotRuntimeSlot; FIGHTER_COUNT],
}

impl Default for BotRuntimeStore {
    fn default() -> Self {
        Self {
            frame: 0,
            seed_initialized: false,
            replay_seed: 0,
            slots: std::array::from_fn(|_| BotRuntimeSlot::default()),
        }
    }
}

impl BotRuntimeStore {
    pub(super) fn begin_frame(&mut self, replay_seed: u64) {
        self.frame = self.frame.wrapping_add(1);
        if !self.seed_initialized || self.replay_seed != replay_seed {
            self.seed_initialized = true;
            self.replay_seed = replay_seed;
            self.slots = std::array::from_fn(|_| BotRuntimeSlot::default());
        }
    }

    #[cfg(feature = "perf")]
    pub(crate) fn clear_planner_timings(&mut self) {
        for slot in &mut self.slots {
            slot.planner_timing_len = 0;
            slot.planner_timing_cursor = 0;
        }
    }

    #[cfg(feature = "perf")]
    pub(crate) fn planner_timing_ns(&self) -> Vec<u64> {
        let sample_count = self.slots.iter().map(|slot| slot.planner_timing_len).sum();
        let mut samples = Vec::with_capacity(sample_count);
        for slot in &self.slots {
            samples.extend(
                slot.planner_timing_ns[..slot.planner_timing_len]
                    .iter()
                    .map(|nanoseconds| u64::from(*nanoseconds)),
            );
        }
        samples
    }
}

#[derive(Clone, Copy)]
#[repr(u64)]
enum RandomStream {
    Reaction = 1,
    PerceptionX = 2,
    PerceptionZ = 3,
    Target = 4,
    Goal = 5,
    Action = 6,
    Mistake = 7,
    Commitment = 8,
    Strafe = 9,
    Item = 10,
    Tactic = 11,
}

#[derive(Default)]
struct BotTechniqueOptions {
    light: Option<TechniquePrediction>,
    heavy: Option<TechniquePrediction>,
}

fn technique_options(
    character: &FighterCharacter,
    style: &FighterStyle,
    equipment: &FighterEquipment,
    motor: &FighterMotor,
    stats: &FighterStats,
    action: &FighterActionState,
    catalog: &CharacterMoveCatalog,
) -> BotTechniqueOptions {
    let loadout = LoadoutContext::for_character(character.kind, style.kind, equipment.kind);
    let prediction = |button| {
        if action.action == FighterAction::Dashing {
            let Some(slot) = (match button {
                TechniqueButton::A => Some(CharacterMoveSlot::DashLight),
                TechniqueButton::B => Some(CharacterMoveSlot::DashHeavy),
                _ => None,
            }) else {
                return None;
            };
            let definition = technique_slot_for_loadout(slot, loadout, catalog)?;
            return definition
                .prediction_requirements_allow(motor.grounded, stats.stamina)
                .then(|| definition.prediction());
        }
        technique_prediction_for_context_in_catalog(
            TechniqueMatchContext {
                previous: action.technique_id,
                button,
                elapsed: action.elapsed.as_seconds(),
                style: style.kind,
                loadout,
                grounded: motor.grounded,
                confirmed_hit: action.confirmed_hit,
                cancel_window_open: action.cancel_window_open,
                branch_window_open: action.branch_window_open,
                current_action: action.action,
            },
            stats.stamina,
            catalog,
        )
    };
    BotTechniqueOptions {
        light: prediction(TechniqueButton::A),
        heavy: prediction(TechniqueButton::B),
    }
}

#[derive(Clone, Copy, Debug)]
struct MoveContactWindow {
    startup_seconds: f32,
    envelope: f32,
    vertical_tolerance: f32,
    facing_dot: f32,
}

#[derive(Clone, Copy, Debug)]
struct MoveEvaluation {
    predicted_position: Vec3,
    score: f32,
    contact_seconds: f32,
}

const SPAWNED_SKILL_RELIABLE_TRAVEL_SECONDS: f32 = 0.25;
const SPAWNED_SKILL_FALLBACK_FACING_DOT: f32 = 0.7;

fn spawned_skill_reliable_range(prediction: TechniquePrediction) -> f32 {
    if !prediction.has_spawned_skill || prediction.spawned_skill_range <= 0.0 {
        return 0.0;
    }
    if prediction.spawned_skill_speed <= 0.0 {
        return prediction.spawned_skill_range;
    }

    (prediction.spawned_skill_lead_distance_offset
        + prediction.spawned_skill_speed * SPAWNED_SKILL_RELIABLE_TRAVEL_SECONDS)
        .min(prediction.spawned_skill_range)
}

fn technique_engagement_envelope(prediction: TechniquePrediction) -> f32 {
    let direct = prediction
        .has_direct_attack
        .then(|| prediction.planar_contact_envelope())
        .unwrap_or(0.0);
    let spawned = spawned_skill_reliable_range(prediction);
    direct.max(spawned)
}

fn technique_contact_window(
    prediction: TechniquePrediction,
    action: BotSemanticAction,
    distance: f32,
) -> Option<MoveContactWindow> {
    let direct = prediction.has_direct_attack.then(|| MoveContactWindow {
        startup_seconds: prediction.startup_ms.unwrap_or(0) as f32 / 1_000.0,
        envelope: prediction.planar_contact_envelope().max(0.1),
        vertical_tolerance: prediction.max_radius.max(0.65),
        facing_dot: match action {
            BotSemanticAction::Heavy => 0.3,
            _ => 0.15,
        },
    });
    let reliable_spawned_range = spawned_skill_reliable_range(prediction);
    let spawned = (distance <= reliable_spawned_range)
        .then(|| prediction.spawned_skill_contact_ms(distance))
        .flatten()
        .map(|contact_ms| MoveContactWindow {
            startup_seconds: contact_ms as f32 / 1_000.0,
            envelope: reliable_spawned_range,
            vertical_tolerance: prediction.spawned_skill_vertical_tolerance.max(0.1),
            facing_dot: prediction
                .spawned_skill_facing_cone_dot
                .unwrap_or(SPAWNED_SKILL_FALLBACK_FACING_DOT),
        });

    match (direct, spawned) {
        (Some(direct), Some(spawned)) => {
            if distance <= direct.envelope && direct.startup_seconds <= spawned.startup_seconds {
                Some(direct)
            } else {
                Some(spawned)
            }
        }
        (Some(direct), None) => Some(direct),
        (None, Some(spawned)) => Some(spawned),
        (None, None) => None,
    }
}

fn technique_execution_confidence(prediction: TechniquePrediction, contact_seconds: f32) -> f32 {
    let contact = (1.0 - contact_seconds / 0.75).clamp(0.0, 1.0);
    let recovery = (1.0 - prediction.recover_at_ms as f32 / 1_000.0).clamp(0.0, 1.0);
    contact * 0.65 + recovery * 0.35
}

fn technique_planning_confidence(
    prediction: TechniquePrediction,
    action: BotSemanticAction,
) -> Option<f32> {
    let envelope = technique_engagement_envelope(prediction);
    let window = technique_contact_window(prediction, action, envelope * 0.65)?;
    let neutral_tiebreak = if action == BotSemanticAction::Light {
        0.01
    } else {
        0.0
    };
    Some(technique_execution_confidence(prediction, window.startup_seconds) + neutral_tiebreak)
}

fn offensive_candidate_score(
    prediction: TechniquePrediction,
    action: BotSemanticAction,
    evaluation: MoveEvaluation,
    punish_need: f32,
    spacing_bonus: f32,
) -> f32 {
    let (base, punish_weight) = match action {
        BotSemanticAction::Heavy => (2.0, 2.4),
        _ => (2.2, 0.8),
    };
    base + evaluation.score
        + technique_execution_confidence(prediction, evaluation.contact_seconds) * 1.6
        + spacing_bonus
        + punish_need * punish_weight
}

fn grab_candidate_score(opponent_guard_rate: f32, punish_need: f32) -> f32 {
    1.8 + opponent_guard_rate.clamp(0.0, 1.0) * 4.5 + punish_need * 0.35
}

fn evaluate_contact_window(
    window: MoveContactWindow,
    bot_position: Vec3,
    bot_facing: Vec3,
    target_position: Vec3,
    target_velocity: Vec2,
    lead_scale: f32,
) -> Option<MoveEvaluation> {
    let lead_seconds = window.startup_seconds.clamp(0.0, 0.35) * lead_scale.clamp(0.0, 1.0);
    let predicted_position = target_position
        + Vec3::new(
            target_velocity.x * lead_seconds,
            0.0,
            target_velocity.y * lead_seconds,
        );
    let flat = Vec2::new(
        predicted_position.x - bot_position.x,
        predicted_position.z - bot_position.z,
    );
    let distance = vec2_length(flat);
    if distance > window.envelope
        || (predicted_position.y - bot_position.y).abs() > window.vertical_tolerance
    {
        return None;
    }
    let facing = vec2_dot(
        vec2_normalize_or_zero(Vec2::new(bot_facing.x, bot_facing.z)),
        vec2_normalize_or_zero(flat),
    );
    if facing < window.facing_dot {
        return None;
    }
    let ideal = window.envelope * 0.7;
    let range_quality =
        (1.0 - (distance - ideal).abs() / window.envelope.max(0.01)).clamp(0.0, 1.0);
    Some(MoveEvaluation {
        predicted_position,
        score: range_quality * 1.6 + facing.clamp(0.0, 1.0) * 0.8,
        contact_seconds: window.startup_seconds,
    })
}

fn evaluate_technique(
    prediction: TechniquePrediction,
    action: BotSemanticAction,
    bot_position: Vec3,
    motor: &FighterMotor,
    stats: &FighterStats,
    target_position: Vec3,
    target_velocity: Vec2,
    lead_scale: f32,
    stamina_reserve_ratio: f32,
) -> Option<MoveEvaluation> {
    if !prediction.requirements_allow(motor.grounded, stats.stamina)
        || !stamina_reserve_allows(
            stats.stamina,
            prediction.stamina_cost,
            stamina_reserve_ratio,
        )
    {
        return None;
    }
    let distance = flat_distance(bot_position, target_position);
    let window = technique_contact_window(prediction, action, distance)?;
    evaluate_contact_window(
        window,
        bot_position,
        motor.facing,
        target_position,
        target_velocity,
        lead_scale,
    )
}

fn stamina_reserve_allows(stamina: f32, cost: f32, reserve_ratio: f32) -> bool {
    cost <= f32::EPSILON || stamina - cost >= MAX_STAMINA * reserve_ratio
}

fn response_is_actionable(prediction: ResponsePrediction, expected: OpponentResponse) -> bool {
    prediction.response == expected && prediction.probability >= ACTIONABLE_RESPONSE_PROBABILITY
}

fn bot_sample(seed: u64, fighter_id: usize, tick: u64, stream: RandomStream, index: u32) -> f32 {
    let mut value = seed
        ^ (fighter_id as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ tick.rotate_left(23)
        ^ (stream as u64).wrapping_mul(0xbf58_476d_1ce4_e5b9)
        ^ (index as u64).wrapping_mul(0x94d0_49bb_1331_11eb);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^= value >> 31;
    ((value >> 40) as u32) as f32 / (1_u32 << 24) as f32
}

fn signed_sample(seed: u64, fighter_id: usize, tick: u64, stream: RandomStream, index: u32) -> f32 {
    bot_sample(seed, fighter_id, tick, stream, index) * 2.0 - 1.0
}

#[allow(clippy::too_many_arguments)]
pub(super) fn drive_bot(
    arena_index: usize,
    bot_id: usize,
    brain: &mut BotBrain,
    motor: &FighterMotor,
    position: Vec3,
    special_state: &FighterSpecialState,
    character: &FighterCharacter,
    style: &FighterStyle,
    equipment: &FighterEquipment,
    stats: &FighterStats,
    action: &FighterActionState,
    difficulty: BotDifficulty,
    held_kind: Option<ItemKind>,
    special_inputs_allowed: bool,
    snapshot: &BotSnapshotBuffer,
    profiles: &BotProfileCatalog,
    move_catalog: &CharacterMoveCatalog,
    runtime: &mut BotRuntimeStore,
    navigation: &mut BotNavigationCache,
    split_causeway_doors: &SplitCausewayDoorState,
    input: &mut FighterInput,
) {
    if bot_id >= FIGHTER_COUNT {
        return;
    }

    let profile_id = match difficulty {
        BotDifficulty::Standard => BotProfileId::Standard,
        BotDifficulty::Tutorial => BotProfileId::Tutorial,
    };
    let profile = profiles.profile(profile_id);
    let frame = runtime.frame;
    let replay_seed = snapshot.replay_seed;
    let slot = &mut runtime.slots[bot_id];
    let appeared = !slot.initialized || slot.last_seen_frame.wrapping_add(1) != frame;
    let behavior_changed = slot.last_behavior != Some(brain.behavior);
    if appeared || behavior_changed {
        *slot = BotRuntimeSlot::default();
        slot.initialized = true;
        slot.profile = Some(profile);
        slot.last_behavior = Some(brain.behavior);
        slot.strafe_sign = if bot_id == 2 { 1.0 } else { -1.0 };
    }
    slot.last_seen_frame = frame;
    let profile = slot.profile.unwrap_or(profile);

    debug_assert_eq!(SIM_HZ_U32 % BOT_DECISION_HZ, 0);
    let decision_advanced = advance_decision_clock(slot, appeared);
    if decision_advanced {
        update_opponent_memory(slot, snapshot, bot_id, 1, replay_seed, profile);
        slot.target_id = choose_target(snapshot, bot_id, position, slot, profile, replay_seed);
    }

    if profile.tactical_planning_enabled
        && slot.active_plan.is_some_and(|plan| {
            slot.target_id != Some(plan.target_id)
                || !snapshot
                    .fighters
                    .get(plan.target_id)
                    .and_then(|fighter| *fighter)
                    .is_some_and(|fighter| fighter.targetable_by[bot_id])
        })
    {
        abort_tactic_plan(slot, snapshot, bot_id, profile, PlanBranch::Unsafe);
    }

    update_commitment(slot, action);
    refresh_trace_state(slot);
    if profile.tactical_planning_enabled
        && tactical_dash_hold_expired(
            action.action,
            action.elapsed.as_seconds(),
            slot.commitment.is_some(),
        )
    {
        abort_tactic_plan(slot, snapshot, bot_id, profile, PlanBranch::Timeout);
        slot.intent.movement = Vec2::ZERO;
        input.movement = Vec2::ZERO;
        sync_legacy_brain(brain, slot);
        return;
    }
    let nearest = nearest_legal_target(snapshot, bot_id, position);
    if action.action == FighterAction::Knockdown {
        if profile.tactical_planning_enabled {
            slot.tactical_phase = TacticalPhase::WakeUp;
            abort_tactic_plan(slot, snapshot, bot_id, profile, PlanBranch::Unsafe);
            refresh_trace_state(slot);
        }
        slot.commitment = None;
        match bot_recovery_decision(
            action.elapsed.as_seconds(),
            position,
            nearest.map(|target| target_snapshot(position, target)),
        ) {
            BotRecoveryDecision::Wait => {}
            BotRecoveryDecision::QuickStand => input.jump = true,
            BotRecoveryDecision::Roll(direction) => {
                input.movement = direction;
                input.dash = true;
            }
        }
        sync_legacy_brain(brain, slot);
        return;
    }

    if profile.tactical_planning_enabled
        && matches!(
            action.action,
            FighterAction::Hitstun
                | FighterAction::GuardBroken
                | FighterAction::Grabbed
                | FighterAction::LandingRecovery
                | FighterAction::RingOut
                | FighterAction::Respawning
        )
    {
        slot.tactical_phase = TacticalPhase::Disadvantage;
        abort_tactic_plan(slot, snapshot, bot_id, profile, PlanBranch::Unsafe);
        refresh_trace_state(slot);
    }

    let tactical_branch_window = profile.tactical_planning_enabled
        && decision_advanced
        && (action.cancel_window_open || action.branch_window_open);
    if matches!(
        action.action,
        FighterAction::LightAttack1 | FighterAction::LightAttack2
    ) {
        let elapsed = action.elapsed.as_seconds();
        if elapsed >= COMBO_QUEUE_START && elapsed <= COMBO_QUEUE_END {
            // Preserve the authored fixed-tick combo buffer independently of
            // the higher-level tactical branch selection. The planner may add
            // a branch action, but it must never make a legal light-chain less
            // reliable than the canonical legacy controller.
            input.light = true;
        }
    }
    if action_locked(action.action) && !tactical_branch_window {
        sync_legacy_brain(brain, slot);
        return;
    }
    if !profile.tactical_planning_enabled
        && matches!(
            action.action,
            FighterAction::LightAttack1 | FighterAction::LightAttack2
        )
    {
        let elapsed = action.elapsed.as_seconds();
        if elapsed >= COMBO_QUEUE_START && elapsed <= COMBO_QUEUE_END {
            input.light = true;
        }
        sync_legacy_brain(brain, slot);
        return;
    }

    let personality = bot_personality(style.kind, equipment.kind);
    let hazard_fear = personality.hazard_fear + profile.hazard_safety_margin_m * 0.2;
    let avoid_hazard = arena_hazard_avoidance(
        position,
        snapshot.hazard_elapsed,
        arena_definition(snapshot.arena_index).hazards,
        hazard_fear,
    );
    let mut emergency = avoid_hazard;
    for special in &snapshot.specials {
        if special.owner_id == bot_id || !combat_allowed(snapshot, special.owner_id, bot_id) {
            continue;
        }
        let Some(radius) = special_avoid_radius(special.kind) else {
            continue;
        };
        let flat = Vec2::new(
            position.x - special.position.x,
            position.z - special.position.z,
        );
        let distance = vec2_length(flat);
        if distance < radius + profile.hazard_safety_margin_m {
            emergency +=
                vec2_normalize_or_zero(flat) * (radius + profile.hazard_safety_margin_m - distance);
        }
    }
    for item in &snapshot.items {
        let Some(owner_id) = item.threat_owner else {
            continue;
        };
        if !combat_allowed(snapshot, owner_id, bot_id) {
            continue;
        }
        let flat = Vec2::new(position.x - item.position.x, position.z - item.position.z);
        let radius = item.threat_radius + profile.hazard_safety_margin_m;
        let distance = vec2_length(flat);
        if distance < radius {
            emergency += vec2_normalize_or_zero(flat) * (radius - distance);
        }
    }
    if vec2_length_squared(emergency) > 0.01 {
        if profile.tactical_planning_enabled {
            abort_tactic_plan(slot, snapshot, bot_id, profile, PlanBranch::Unsafe);
        }
        slot.commitment = None;
        slot.intent = BotIntent {
            goal: BotGoal::Survive,
            movement: snapshot_edge_steering(snapshot, position, vec2_normalize_or_zero(emergency)),
            ..default()
        };
        slot.trace.goal = BotGoal::Survive;
        slot.trace.reason = BotDecisionReason::Safety;
        slot.trace.utility_score = vec2_length(emergency);
        refresh_trace_state(slot);
        input.movement = slot.intent.movement;
        if motor.grounded && slot.decision_tick >= slot.dash_ready_tick {
            input.dash = true;
            slot.dash_ready_tick = slot.decision_tick + 30;
        }
        sync_legacy_brain(brain, slot);
        return;
    }

    if decision_advanced {
        let techniques = technique_options(
            character,
            style,
            equipment,
            motor,
            stats,
            action,
            move_catalog,
        );
        plan_bot(
            arena_index,
            bot_id,
            position,
            motor,
            special_state,
            character,
            style,
            equipment,
            stats,
            action,
            difficulty,
            held_kind,
            special_inputs_allowed,
            &techniques,
            move_catalog,
            snapshot,
            profile,
            replay_seed,
            slot,
            navigation,
            split_causeway_doors,
        );
    }
    actuate(slot, input);
    sync_legacy_brain(brain, slot);
}

fn action_locked(action: FighterAction) -> bool {
    matches!(
        action,
        FighterAction::DashAttack
            | FighterAction::JumpAttack
            | FighterAction::JumpHeavyAttack
            | FighterAction::ComboFinisher
            | FighterAction::HeavyAttack
            | FighterAction::HeavyAttack2
            | FighterAction::UltimateStartup
            | FighterAction::UltimateRush
            | FighterAction::UltimateVictim
            | FighterAction::GrabStartup
            | FighterAction::Throwing
            | FighterAction::SpecialCast
            | FighterAction::ItemPickup
            | FighterAction::ItemSwing
            | FighterAction::ItemThrow
            | FighterAction::ItemDrop
            | FighterAction::Guarding
            | FighterAction::Hitstun
            | FighterAction::GetUp
            | FighterAction::GuardBroken
            | FighterAction::GrabHold
            | FighterAction::Grabbed
            | FighterAction::LandingRecovery
            | FighterAction::GuardCounter
            | FighterAction::GuardStep
            | FighterAction::QuickStand
            | FighterAction::RecoveryRoll
            | FighterAction::RingOut
            | FighterAction::Respawning
    )
}

fn combat_allowed(snapshot: &BotSnapshotBuffer, attacker: usize, victim: usize) -> bool {
    attacker < FIGHTER_COUNT
        && victim < FIGHTER_COUNT
        && snapshot.fighters[victim].is_some_and(|fighter| fighter.targetable_by[attacker])
}

fn nearest_legal_target(
    snapshot: &BotSnapshotBuffer,
    bot_id: usize,
    position: Vec3,
) -> Option<FighterSnapshot> {
    snapshot
        .fighters
        .iter()
        .flatten()
        .filter(|fighter| fighter.id != bot_id && fighter.targetable_by[bot_id])
        .min_by(|left, right| {
            flat_distance(position, left.position)
                .total_cmp(&flat_distance(position, right.position))
                .then_with(|| left.id.cmp(&right.id))
        })
        .copied()
}

fn target_snapshot(position: Vec3, target: FighterSnapshot) -> BotTargetSnapshot {
    BotTargetSnapshot {
        fighter_id: crate::determinism::FighterId::from_index(target.id)
            .expect("bot snapshot fighter IDs are canonical"),
        position: target.position,
        distance: flat_distance(position, target.position),
        facing: target.facing,
        action: target.action,
    }
}

fn flat_distance(left: Vec3, right: Vec3) -> f32 {
    vec2_length(Vec2::new(left.x - right.x, left.z - right.z))
}

fn observation_flags(action: FighterAction) -> u8 {
    let mut flags = 0;
    if is_attack_action(action) {
        flags |= OBS_ATTACK;
    }
    if action == FighterAction::Guarding {
        flags |= OBS_GUARD;
    }
    if matches!(action, FighterAction::GrabStartup | FighterAction::GrabHold) {
        flags |= OBS_GRAB;
    }
    if matches!(
        action,
        FighterAction::Jumping | FighterAction::JumpAttack | FighterAction::JumpHeavyAttack
    ) {
        flags |= OBS_AIR;
    }
    flags
}

fn observed_response_transition(
    previous: FighterAction,
    perceived: FighterAction,
    opponent_position: Vec3,
    bot_position: Vec3,
    opponent_velocity: Vec2,
) -> Option<OpponentResponse> {
    if matches!(
        perceived,
        FighterAction::Hitstun
            | FighterAction::Knockdown
            | FighterAction::GuardBroken
            | FighterAction::Grabbed
            | FighterAction::GrabHold
            | FighterAction::UltimateVictim
            | FighterAction::LandingRecovery
            | FighterAction::RingOut
            | FighterAction::Respawning
    ) {
        return None;
    }
    if perceived == FighterAction::Idle
        && !matches!(previous, FighterAction::Idle | FighterAction::Moving)
    {
        return None;
    }
    Some(response_family(
        perceived,
        opponent_position,
        bot_position,
        opponent_velocity,
    ))
}

fn is_attack_action(action: FighterAction) -> bool {
    matches!(
        action,
        FighterAction::LightAttack1
            | FighterAction::LightAttack2
            | FighterAction::ComboFinisher
            | FighterAction::HeavyAttack
            | FighterAction::HeavyAttack2
            | FighterAction::DashAttack
            | FighterAction::JumpAttack
            | FighterAction::JumpHeavyAttack
            | FighterAction::GrabStartup
            | FighterAction::ItemSwing
            | FighterAction::ItemThrow
            | FighterAction::SpecialCast
            | FighterAction::GuardCounter
    )
}

fn publish_bot_outcome(slot: &mut BotRuntimeSlot, outcome: PreviousOutcome) {
    if outcome == PreviousOutcome::None {
        return;
    }
    slot.last_outcome = outcome;
    slot.last_outcome_tick = slot.decision_tick;
    slot.observed_outcome_published = true;
}

fn update_bot_action_outcome(slot: &mut BotRuntimeSlot, fighter: FighterSnapshot) {
    let transitioned = slot.observed_action != Some(fighter.action)
        || slot.observed_technique != fighter.technique_id;
    if transitioned {
        if let Some(previous_action) = slot.observed_action
            && is_attack_action(previous_action)
            && !slot.observed_outcome_published
        {
            let outcome = if previous_action == FighterAction::GrabStartup
                && fighter.action == FighterAction::GrabHold
            {
                PreviousOutcome::Hit
            } else if slot.observed_action_outcome == PreviousOutcome::None {
                PreviousOutcome::Whiff
            } else {
                slot.observed_action_outcome
            };
            publish_bot_outcome(slot, outcome);
        }
        slot.observed_action = Some(fighter.action);
        slot.observed_technique = fighter.technique_id;
        slot.observed_action_outcome = PreviousOutcome::None;
        slot.observed_outcome_published = false;
    }

    let contact = if fighter.confirmed_guard {
        PreviousOutcome::Guarded
    } else if fighter.confirmed_hit || fighter.action == FighterAction::GrabHold {
        PreviousOutcome::Hit
    } else {
        PreviousOutcome::None
    };
    if contact != PreviousOutcome::None && contact != slot.observed_action_outcome {
        slot.observed_action_outcome = contact;
        publish_bot_outcome(slot, contact);
    }
}

fn update_opponent_memory(
    slot: &mut BotRuntimeSlot,
    snapshot: &BotSnapshotBuffer,
    bot_id: usize,
    elapsed_ticks: u64,
    seed: u64,
    profile: BotProfile,
) {
    let bot_snapshot = snapshot.fighters.get(bot_id).and_then(|fighter| *fighter);
    if let Some(bot) = bot_snapshot {
        update_bot_action_outcome(slot, bot);
    }
    let bot_position = bot_snapshot.map_or(Vec3::ZERO, |bot| bot.position);
    let last_move_envelope = slot.last_move_envelope;
    let last_outcome = slot.last_outcome;
    for opponent in &mut slot.opponents {
        opponent.responses.advance_ticks(elapsed_ticks);
        if opponent.perceived_action.is_some() {
            opponent.perceived_action_elapsed = (opponent.perceived_action_elapsed
                + elapsed_ticks as f32 * DECISION_STEP)
                .min(60.0);
        }
    }
    let skipped = elapsed_ticks.saturating_sub(1).min(MEMORY_TICKS as u64);
    for fighter in snapshot.fighters.iter().flatten() {
        if fighter.id == bot_id {
            continue;
        }
        let memory = &mut slot.opponents[fighter.id];
        memory.observe_position(fighter.position, slot.decision_tick);
        for _ in 0..skipped {
            memory.push(0);
        }

        if memory.raw_action != Some(fighter.action) || memory.raw_technique != fighter.technique_id
        {
            memory.raw_action = Some(fighter.action);
            memory.raw_technique = fighter.technique_id;
            memory.pending_action = Some(fighter.action);
            memory.pending_technique = fighter.technique_id;
            memory.pending_prediction = fighter.active_prediction;
            memory.pending_action_elapsed = fighter.action_elapsed;
            memory.pending_since_tick = slot.decision_tick;
            let min = profile.reaction_ticks_min as u64;
            let max = (profile.reaction_ticks_max as u64).max(min);
            let span = max - min + 1;
            memory.pending_delay = min
                + (bot_sample(
                    seed,
                    bot_id,
                    slot.decision_tick,
                    RandomStream::Reaction,
                    fighter.id as u32,
                ) * span as f32)
                    .floor()
                    .min((span - 1) as f32) as u64;
        }
        if memory.pending_action.is_some()
            && slot.decision_tick.saturating_sub(memory.pending_since_tick) >= memory.pending_delay
        {
            let perceived = memory
                .pending_action
                .take()
                .expect("pending action checked above");
            let previous_perceived = memory.perceived_action.unwrap_or(FighterAction::Idle);
            let context = response_context(
                flat_distance(bot_position, fighter.position),
                last_move_envelope,
                previous_perceived,
                snapshot_edge_danger(snapshot, fighter.position),
                last_outcome,
            );
            if let Some(response) = observed_response_transition(
                previous_perceived,
                perceived,
                fighter.position,
                bot_position,
                memory.velocity,
            ) {
                memory.responses.observe(context, response);
            }
            if is_attack_action(perceived) {
                if memory.last_opener == Some(perceived) {
                    memory.repeated_openers = memory.repeated_openers.saturating_add(1);
                } else {
                    memory.repeated_openers = 0;
                    memory.last_opener = Some(perceived);
                }
            }
            memory.perceived_action = Some(perceived);
            memory.perceived_technique = memory.pending_technique.take();
            memory.perceived_prediction = memory.pending_prediction.take();
            memory.perceived_action_elapsed = memory.pending_action_elapsed
                + slot.decision_tick.saturating_sub(memory.pending_since_tick) as f32
                    * DECISION_STEP;
        }
        memory.push(observation_flags(
            memory.perceived_action.unwrap_or(FighterAction::Idle),
        ));
    }
}

fn choose_target(
    snapshot: &BotSnapshotBuffer,
    bot_id: usize,
    position: Vec3,
    slot: &BotRuntimeSlot,
    profile: BotProfile,
    seed: u64,
) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for target in snapshot.fighters.iter().flatten() {
        if target.id == bot_id || !target.targetable_by[bot_id] {
            continue;
        }
        let memory = &slot.opponents[target.id];
        let perceived = memory.perceived_action.unwrap_or(FighterAction::Idle);
        let distance = flat_distance(position, target.position);
        let vulnerability = vulnerability(perceived)
            * (1.0 + memory.perceived_action_elapsed.clamp(0.0, 1.0) * 0.25);
        let threat = threat(perceived);
        let edge = snapshot_edge_danger(snapshot, target.position);
        let health_weakness = 1.0 - (target.health / 100.0).clamp(0.0, 1.0);
        let stamina_weakness = 1.0 - (target.stamina / MAX_STAMINA).clamp(0.0, 1.0);
        let jitter = signed_sample(
            seed,
            bot_id,
            slot.decision_tick,
            RandomStream::Target,
            target.id as u32,
        ) * profile.utility_jitter;
        let score = (1.0 / (0.5 + distance)
            + vulnerability * 0.35
            + memory.rate(0) * 0.2
            + edge * 0.25
            + health_weakness * 0.12
            + stamina_weakness * 0.08
            - threat * 0.18)
            .max(0.01)
            * (1.0 + jitter);
        if best.is_none_or(|(_, best_score)| score > best_score) {
            best = Some((target.id, score));
        }
    }

    let Some((challenger, challenger_score)) = best else {
        return None;
    };
    let Some(current) = slot.target_id else {
        return Some(challenger);
    };
    if current == challenger {
        return Some(current);
    }
    let Some(current_fighter) = snapshot.fighters.get(current).and_then(|value| *value) else {
        return Some(challenger);
    };
    if !current_fighter.targetable_by[bot_id] {
        return Some(challenger);
    }
    let memory = &slot.opponents[current];
    let current_vulnerability =
        vulnerability(memory.perceived_action.unwrap_or(FighterAction::Idle))
            * (1.0 + memory.perceived_action_elapsed.clamp(0.0, 1.0) * 0.25);
    let current_health_weakness = 1.0 - (current_fighter.health / 100.0).clamp(0.0, 1.0);
    let current_stamina_weakness = 1.0 - (current_fighter.stamina / MAX_STAMINA).clamp(0.0, 1.0);
    let current_score = (1.0 / (0.5 + flat_distance(position, current_fighter.position))
        + current_vulnerability * 0.35
        + memory.rate(0) * 0.2
        + snapshot_edge_danger(snapshot, current_fighter.position) * 0.25
        + current_health_weakness * 0.12
        + current_stamina_weakness * 0.08)
        .max(0.01);
    if challenger_score > current_score * (1.0 + profile.target_switch_margin) {
        Some(challenger)
    } else {
        Some(current)
    }
}

fn vulnerability(action: FighterAction) -> f32 {
    if matches!(
        action,
        FighterAction::LandingRecovery
            | FighterAction::GuardBroken
            | FighterAction::GetUp
            | FighterAction::HeavyAttack
            | FighterAction::HeavyAttack2
            | FighterAction::SpecialCast
    ) {
        1.0
    } else {
        0.0
    }
}

fn threat(action: FighterAction) -> f32 {
    if is_attack_action(action) { 1.0 } else { 0.0 }
}

#[derive(Clone, Copy, Debug, Default)]
struct TacticalFollowUp {
    branch: PlanBranch,
    action: Option<BotSemanticAction>,
    expected_action: Option<FighterAction>,
    facts: Option<ForecastMoveFacts>,
    requires_grounded: bool,
}

fn decision_ticks(seconds: f32) -> u8 {
    (seconds.max(0.0) * BOT_DECISION_HZ as f32)
        .ceil()
        .clamp(0.0, u8::MAX as f32) as u8
}

fn semantic_for_button(button: TechniqueButton) -> Option<BotSemanticAction> {
    match button {
        TechniqueButton::A => Some(BotSemanticAction::Light),
        TechniqueButton::B => Some(BotSemanticAction::Heavy),
        TechniqueButton::Grab => Some(BotSemanticAction::Grab),
        TechniqueButton::Dash => Some(BotSemanticAction::Dash),
        TechniqueButton::Jump => Some(BotSemanticAction::Jump),
        TechniqueButton::Special => Some(BotSemanticAction::SpecialProjectile),
        TechniqueButton::Item => Some(BotSemanticAction::ItemLight),
        TechniqueButton::AB | TechniqueButton::Ultimate => None,
    }
}

fn semantic_for_fighter_action(action: FighterAction) -> Option<BotSemanticAction> {
    match action {
        FighterAction::LightAttack1
        | FighterAction::LightAttack2
        | FighterAction::ComboFinisher
        | FighterAction::DashAttack
        | FighterAction::JumpAttack => Some(BotSemanticAction::Light),
        FighterAction::HeavyAttack
        | FighterAction::HeavyAttack2
        | FighterAction::JumpHeavyAttack => Some(BotSemanticAction::Heavy),
        FighterAction::GrabStartup | FighterAction::GrabHold | FighterAction::Throwing => {
            Some(BotSemanticAction::Grab)
        }
        FighterAction::Jumping => Some(BotSemanticAction::Jump),
        FighterAction::Dashing => Some(BotSemanticAction::Dash),
        FighterAction::ItemPickup => Some(BotSemanticAction::Pickup),
        FighterAction::ItemSwing => Some(BotSemanticAction::ItemLight),
        FighterAction::ItemThrow => Some(BotSemanticAction::ItemHeavy),
        FighterAction::SpecialCast => Some(BotSemanticAction::SpecialProjectile),
        _ => None,
    }
}

fn forecast_action_phase(
    action: FighterAction,
    elapsed: f32,
    prediction: Option<TechniquePrediction>,
) -> (ForecastActionPhase, u8) {
    if matches!(
        action,
        FighterAction::Hitstun
            | FighterAction::Knockdown
            | FighterAction::QuickStand
            | FighterAction::RecoveryRoll
            | FighterAction::GetUp
            | FighterAction::GuardBroken
            | FighterAction::Grabbed
            | FighterAction::RingOut
            | FighterAction::Respawning
    ) {
        return (ForecastActionPhase::Disabled, 0);
    }
    if matches!(
        action,
        FighterAction::Idle | FighterAction::Moving | FighterAction::Guarding
    ) {
        return (ForecastActionPhase::Neutral, 0);
    }
    let Some(prediction) = prediction else {
        return (ForecastActionPhase::Recovery, 1);
    };
    let elapsed_ms = (elapsed.max(0.0) * 1_000.0).round() as u32;
    let startup_ms = prediction
        .startup_ms
        .or(prediction.spawned_skill_startup_ms)
        .unwrap_or(0);
    let active_end_ms = prediction
        .direct_active_end_ms
        .unwrap_or(startup_ms.saturating_add(50));
    let phase = if elapsed_ms < startup_ms {
        ForecastActionPhase::Startup
    } else if elapsed_ms <= active_end_ms {
        ForecastActionPhase::Active
    } else {
        ForecastActionPhase::Recovery
    };
    let remaining_ms = prediction.recover_at_ms.saturating_sub(elapsed_ms);
    (
        phase,
        ((remaining_ms.saturating_add(49) / 50).min(u8::MAX as u32)) as u8,
    )
}

fn authored_follow_up(
    first: TechniquePrediction,
    branch: PlanBranch,
    button: TechniqueButton,
    grounded: bool,
    stamina: f32,
    character: &FighterCharacter,
    style: &FighterStyle,
    equipment: &FighterEquipment,
    catalog: &CharacterMoveCatalog,
) -> Option<TacticalFollowUp> {
    let window = match branch {
        PlanBranch::Hit | PlanBranch::Guarded => first.cancel_window.or(first.branch_window),
        PlanBranch::Whiff => first.branch_window.or(first.cancel_window),
        _ => None,
    };
    let elapsed_ms = window
        .map(|value| value.start_ms)
        .or(first.next_tech_ms)
        .unwrap_or(first.recover_at_ms.saturating_sub(first.input_buffer_ms));
    let loadout = LoadoutContext::for_character(character.kind, style.kind, equipment.kind);
    let prediction = technique_prediction_for_context_in_catalog(
        TechniqueMatchContext {
            previous: Some(first.id),
            button,
            elapsed: elapsed_ms as f32 / 1_000.0,
            style: style.kind,
            loadout,
            grounded,
            confirmed_hit: matches!(branch, PlanBranch::Hit | PlanBranch::Guarded),
            cancel_window_open: first
                .cancel_window
                .is_some_and(|value| value.contains_ms(elapsed_ms)),
            branch_window_open: first
                .branch_window
                .is_some_and(|value| value.contains_ms(elapsed_ms)),
            current_action: first.action,
        },
        stamina,
        catalog,
    )?;
    if !prediction.is_chain {
        return None;
    }
    let action = semantic_for_button(button)?;
    Some(TacticalFollowUp {
        branch,
        action: Some(action),
        expected_action: Some(prediction.action),
        facts: Some(ForecastMoveFacts::from_prediction(action, prediction)),
        requires_grounded: prediction.status == TechniqueStatus::Grounded,
    })
}

#[allow(clippy::too_many_arguments)]
fn authored_follow_ups(
    first: Option<TechniquePrediction>,
    grounded: bool,
    stamina: f32,
    character: &FighterCharacter,
    style: &FighterStyle,
    equipment: &FighterEquipment,
    catalog: &CharacterMoveCatalog,
) -> [TacticalFollowUp; MAX_FORECAST_FOLLOW_UPS] {
    let Some(first) = first else {
        return [TacticalFollowUp::default(); MAX_FORECAST_FOLLOW_UPS];
    };
    [
        authored_follow_up(
            first,
            PlanBranch::Hit,
            TechniqueButton::A,
            grounded,
            stamina,
            character,
            style,
            equipment,
            catalog,
        )
        .unwrap_or_default(),
        authored_follow_up(
            first,
            PlanBranch::Hit,
            TechniqueButton::B,
            grounded,
            stamina,
            character,
            style,
            equipment,
            catalog,
        )
        .unwrap_or_default(),
        authored_follow_up(
            first,
            PlanBranch::Guarded,
            TechniqueButton::A,
            grounded,
            stamina,
            character,
            style,
            equipment,
            catalog,
        )
        .unwrap_or_default(),
        authored_follow_up(
            first,
            PlanBranch::Guarded,
            TechniqueButton::B,
            grounded,
            stamina,
            character,
            style,
            equipment,
            catalog,
        )
        .unwrap_or_default(),
    ]
}

fn candidate_prediction(
    action: BotSemanticAction,
    techniques: &BotTechniqueOptions,
) -> Option<TechniquePrediction> {
    match action {
        BotSemanticAction::Light => techniques.light,
        BotSemanticAction::Heavy => techniques.heavy,
        _ => None,
    }
}

fn add_tactical_candidate(
    candidates: &mut [Option<ForecastAction>; MAX_TACTICAL_ACTIONS],
    len: &mut usize,
    action: BotSemanticAction,
    facts: ForecastMoveFacts,
    phase: TacticalPhase,
    predicted_response: OpponentResponse,
    target_airborne: bool,
    force_tactic: Option<TacticId>,
) {
    if *len >= MAX_TACTICAL_ACTIONS {
        return;
    }
    let tactic = force_tactic
        .unwrap_or_else(|| tactic_for_action(action, phase, predicted_response, target_airborne));
    candidates[*len] = Some(ForecastAction {
        action,
        tactic,
        facts,
    });
    *len += 1;
}

fn external_threat_cost(
    snapshot: &BotSnapshotBuffer,
    slot: &BotRuntimeSlot,
    bot_id: usize,
    target_id: usize,
    position: Vec3,
) -> f32 {
    let mut cost = snapshot_edge_danger(snapshot, position) * 0.35;
    for fighter in snapshot.fighters.iter().flatten() {
        if fighter.id == bot_id
            || fighter.id == target_id
            || !combat_allowed(snapshot, fighter.id, bot_id)
        {
            continue;
        }
        let distance = flat_distance(position, fighter.position);
        if distance >= 5.0 {
            continue;
        }
        let perceived = slot.opponents[fighter.id]
            .perceived_action
            .unwrap_or(FighterAction::Idle);
        cost += (1.0 - distance / 5.0) * (0.25 + threat(perceived) * 0.55);
    }
    for special in &snapshot.specials {
        if special.owner_id == bot_id || special.owner_id == target_id {
            continue;
        }
        let distance = flat_distance(position, special.position);
        cost += (1.0 - distance / 6.0).clamp(0.0, 1.0) * 0.35;
    }
    for item in &snapshot.items {
        if item
            .threat_owner
            .is_some_and(|owner| owner != bot_id && owner != target_id)
        {
            let distance = flat_distance(position, item.position);
            cost += (1.0 - distance / item.threat_radius.max(0.5)).clamp(0.0, 1.0) * 0.3;
        }
    }
    cost.clamp(0.0, 2.0)
}

fn expected_action_for_semantic(
    action: BotSemanticAction,
    techniques: &BotTechniqueOptions,
) -> Option<FighterAction> {
    match action {
        BotSemanticAction::Light => techniques.light.map(|prediction| prediction.action),
        BotSemanticAction::Heavy => techniques.heavy.map(|prediction| prediction.action),
        BotSemanticAction::Grab => Some(FighterAction::GrabStartup),
        BotSemanticAction::Jump => Some(FighterAction::Jumping),
        BotSemanticAction::Dash => Some(FighterAction::Dashing),
        BotSemanticAction::Pickup => Some(FighterAction::ItemPickup),
        BotSemanticAction::ItemLight => Some(FighterAction::ItemSwing),
        BotSemanticAction::ItemHeavy => Some(FighterAction::ItemThrow),
        BotSemanticAction::SpecialProjectile
        | BotSemanticAction::SpecialTrap
        | BotSemanticAction::SpecialHazard
        | BotSemanticAction::SpecialShockwave => Some(FighterAction::SpecialCast),
    }
}

fn begin_commitment(
    slot: &mut BotRuntimeSlot,
    action: BotSemanticAction,
    expected_action: Option<FighterAction>,
    duration_ticks: u64,
) {
    slot.commitment_rejected = false;
    slot.commitment = Some(BotCommitment {
        action,
        expected_action,
        expires_tick: slot.decision_tick + duration_ticks.max(1),
        accepted: false,
        last_press_tick: None,
    });
}

fn branch_for_outcome(outcome: PreviousOutcome) -> PlanBranch {
    match outcome {
        PreviousOutcome::Hit => PlanBranch::Hit,
        PreviousOutcome::Guarded => PlanBranch::Guarded,
        PreviousOutcome::Whiff => PlanBranch::Whiff,
        PreviousOutcome::None => PlanBranch::Pending,
    }
}

fn record_plan_branch(plan: &mut ActiveTacticPlan, branch: PlanBranch) {
    plan.branch = branch;
    match branch {
        PlanBranch::Hit => plan.accumulated.hits = plan.accumulated.hits.saturating_add(1),
        PlanBranch::Guarded => plan.accumulated.blocks = plan.accumulated.blocks.saturating_add(1),
        PlanBranch::Whiff | PlanBranch::Timeout | PlanBranch::Unsafe => {
            plan.accumulated.whiffs = plan.accumulated.whiffs.saturating_add(1)
        }
        _ => {}
    }
}

fn complete_tactic_plan(
    slot: &mut BotRuntimeSlot,
    snapshot: &BotSnapshotBuffer,
    bot_id: usize,
    branch: PlanBranch,
    profile: BotProfile,
) {
    let Some(mut plan) = slot.active_plan.take() else {
        return;
    };
    if plan.branch != branch {
        record_plan_branch(&mut plan, branch);
    }
    if let Some(bot) = snapshot.fighters.get(bot_id).and_then(|value| *value)
        && let Some(target) = snapshot
            .fighters
            .get(plan.target_id)
            .and_then(|value| *value)
    {
        let dealt = (plan.start_target_health - target.health).max(0.0);
        let received = (plan.start_bot_health - bot.health).max(0.0);
        let target_spent = (plan.start_target_stamina - target.stamina).max(0.0);
        let bot_spent = (plan.start_bot_stamina - bot.stamina).max(0.0);
        plan.accumulated.damage_swing = dealt - received;
        plan.accumulated.stamina_swing = target_spent - bot_spent;
        plan.accumulated.position_change = (snapshot_edge_danger(snapshot, target.position)
            - snapshot_edge_danger(snapshot, plan.target_origin))
            + (snapshot_edge_danger(snapshot, plan.origin)
                - snapshot_edge_danger(snapshot, bot.position));
        plan.accumulated.initiative_result =
            (vulnerability(target.action) - vulnerability(bot.action)).clamp(-1.0, 1.0);
    }
    let learned = update_tactic_bias(
        &mut slot.tactic_biases,
        plan.tactic,
        plan.accumulated,
        profile.tactic_learning_rate,
        profile.tactic_bias_cap,
    );
    slot.trace.tactic = Some(plan.tactic);
    slot.trace.forecast_score = plan.forecast_score;
    slot.trace.plan_step = plan.current_step;
    slot.trace.branch = branch;
    slot.trace.learned_bias = learned;
}

fn abort_tactic_plan(
    slot: &mut BotRuntimeSlot,
    snapshot: &BotSnapshotBuffer,
    bot_id: usize,
    profile: BotProfile,
    branch: PlanBranch,
) {
    complete_tactic_plan(slot, snapshot, bot_id, branch, profile);
    slot.commitment = None;
    slot.commitment_rejected = false;
}

fn advance_plan_branch(plan: &mut ActiveTacticPlan, branch: PlanBranch, tick: u64) -> bool {
    record_plan_branch(plan, branch);
    let Some(next) = plan.next_matching_step(branch) else {
        return false;
    };
    plan.current_step = next;
    plan.step_started_tick = tick;
    plan.step_committed = false;
    plan.rejection_count = 0;
    let duration = plan.current().map_or(1, |step| step.deadline_ticks.max(1));
    plan.deadline_tick = tick + u64::from(duration);
    plan.branch = branch;
    true
}

fn apply_active_plan_step(
    slot: &mut BotRuntimeSlot,
    snapshot: &BotSnapshotBuffer,
    position: Vec3,
    target_position: Vec3,
) -> bool {
    let Some(mut plan) = slot.active_plan else {
        return false;
    };
    let Some(step) = plan.current() else {
        return false;
    };
    let toward = vec2_normalize_or_zero(Vec2::new(
        target_position.x - position.x,
        target_position.z - position.z,
    ));
    let movement = match step.movement {
        PlanMovement::Hold => {
            if matches!(
                plan.tactic,
                TacticId::BaitAndPunish | TacticId::EscapePressure
            ) && step.action == Some(BotSemanticAction::Dash)
            {
                -toward
            } else {
                slot.intent.movement
            }
        }
        PlanMovement::Approach => toward,
        PlanMovement::Retreat => -toward,
        PlanMovement::Strafe => Vec2::new(-toward.y, toward.x) * slot.strafe_sign,
    };
    slot.intent.movement =
        snapshot_edge_steering(snapshot, position, vec2_normalize_or_zero(movement));
    if step.action.is_none() {
        plan.step_committed = true;
        slot.active_plan = Some(plan);
        return true;
    }
    if !plan.step_committed && slot.commitment.is_none() {
        let semantic = step.action.expect("active action step checked above");
        begin_commitment(
            slot,
            semantic,
            step.expected_action,
            u64::from(step.deadline_ticks.max(1)),
        );
        plan.step_committed = true;
        plan.branch = PlanBranch::Pending;
        slot.active_plan = Some(plan);
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn advance_active_tactic(
    slot: &mut BotRuntimeSlot,
    snapshot: &BotSnapshotBuffer,
    bot_id: usize,
    position: Vec3,
    motor: &FighterMotor,
    stats: &FighterStats,
    action: &FighterActionState,
    target: FighterSnapshot,
    perceived_action: FighterAction,
    profile: BotProfile,
) -> bool {
    let Some(mut plan) = slot.active_plan else {
        return false;
    };
    if let Some(bot) = snapshot.fighters.get(bot_id).and_then(|value| *value) {
        plan.accumulated.damage_swing += -target.health_delta + bot.health_delta;
        plan.accumulated.stamina_swing += -target.stamina_delta + bot.stamina_delta;
    }
    let Some(step) = plan.current() else {
        complete_tactic_plan(slot, snapshot, bot_id, PlanBranch::Timeout, profile);
        return false;
    };
    let displacement_limit = if matches!(
        plan.tactic,
        TacticId::EscapePressure | TacticId::BaitAndPunish
    ) {
        f32::INFINITY
    } else {
        6.0
    };
    if plan_is_invalid(PlanInvalidationInputs {
        target_valid: slot.target_id == Some(plan.target_id)
            && plan.target_id == target.id
            && target.targetable_by[bot_id],
        incapacitated: matches!(
            action.action,
            FighterAction::Hitstun
                | FighterAction::Knockdown
                | FighterAction::GuardBroken
                | FighterAction::Grabbed
                | FighterAction::RingOut
                | FighterAction::Respawning
        ),
        unsafe_ground: snapshot_edge_danger(snapshot, position) > 0.9
            || (step.requires_grounded && !motor.grounded),
        stamina: stats.stamina,
        required_stamina: step.stamina_cost,
        displacement: flat_distance(position, plan.origin),
        displacement_limit,
        rejection_count: plan.rejection_count,
    }) {
        slot.active_plan = Some(plan);
        abort_tactic_plan(slot, snapshot, bot_id, profile, PlanBranch::Unsafe);
        return false;
    }

    if slot.commitment_rejected {
        slot.commitment_rejected = false;
        plan.rejection_count = plan.rejection_count.saturating_add(1);
        if plan.rejection_count >= 2 {
            slot.active_plan = Some(plan);
            abort_tactic_plan(slot, snapshot, bot_id, profile, PlanBranch::Unsafe);
            return false;
        }
        plan.step_committed = false;
        plan.deadline_tick = slot.decision_tick + u64::from(step.deadline_ticks.max(1));
        slot.active_plan = Some(plan);
        return apply_active_plan_step(slot, snapshot, position, target.position);
    }

    if slot.last_outcome_tick > plan.last_outcome_tick {
        plan.last_outcome_tick = slot.last_outcome_tick;
        let branch = branch_for_outcome(slot.last_outcome);
        if advance_plan_branch(&mut plan, branch, slot.decision_tick) {
            slot.active_plan = Some(plan);
            return apply_active_plan_step(slot, snapshot, position, target.position);
        }
        slot.active_plan = Some(plan);
        complete_tactic_plan(slot, snapshot, bot_id, branch, profile);
        return false;
    }

    let target_airborne = matches!(
        perceived_action,
        FighterAction::Jumping | FighterAction::JumpAttack | FighterAction::JumpHeavyAttack
    );
    if target_airborne
        && plan
            .next_matching_step(PlanBranch::TargetAirborne)
            .is_some()
        && advance_plan_branch(&mut plan, PlanBranch::TargetAirborne, slot.decision_tick)
    {
        slot.active_plan = Some(plan);
        return apply_active_plan_step(slot, snapshot, position, target.position);
    }
    if is_attack_action(perceived_action)
        && plan.next_matching_step(PlanBranch::Threatened).is_some()
        && advance_plan_branch(&mut plan, PlanBranch::Threatened, slot.decision_tick)
    {
        slot.active_plan = Some(plan);
        return apply_active_plan_step(slot, snapshot, position, target.position);
    }

    if plan.step_committed
        && step.action.is_some_and(|semantic| {
            matches!(semantic, BotSemanticAction::Dash | BotSemanticAction::Jump)
        })
        && step
            .action
            .is_some_and(|semantic| action_matches(semantic, action.action))
        && plan.next_matching_step(PlanBranch::Threatened).is_none()
    {
        plan.accumulated.initiative_result += 0.25;
        slot.active_plan = Some(plan);
        complete_tactic_plan(slot, snapshot, bot_id, PlanBranch::Pending, profile);
        return false;
    }

    if slot.decision_tick >= plan.deadline_tick {
        let branch = if step.action.is_some_and(|semantic| {
            matches!(
                semantic,
                BotSemanticAction::Light
                    | BotSemanticAction::Heavy
                    | BotSemanticAction::Grab
                    | BotSemanticAction::ItemLight
                    | BotSemanticAction::ItemHeavy
                    | BotSemanticAction::SpecialProjectile
                    | BotSemanticAction::SpecialTrap
                    | BotSemanticAction::SpecialHazard
                    | BotSemanticAction::SpecialShockwave
            )
        }) {
            PlanBranch::Whiff
        } else {
            PlanBranch::Timeout
        };
        if advance_plan_branch(&mut plan, branch, slot.decision_tick) {
            slot.active_plan = Some(plan);
            return apply_active_plan_step(slot, snapshot, position, target.position);
        }
        slot.active_plan = Some(plan);
        complete_tactic_plan(slot, snapshot, bot_id, branch, profile);
        return false;
    }

    slot.active_plan = Some(plan);
    apply_active_plan_step(slot, snapshot, position, target.position)
}

fn select_near_optimal_index(
    scores: &[Option<f32>; MAX_TACTICAL_ACTIONS],
    len: usize,
    margin: f32,
    jitter_scale: f32,
    seed: u64,
    bot_id: usize,
    tick: u64,
) -> Option<(usize, f32)> {
    let best_strategic = scores
        .iter()
        .take(len.min(MAX_TACTICAL_ACTIONS))
        .flatten()
        .copied()
        .max_by(f32::total_cmp)?;
    let mut selected = None;
    for (index, strategic) in scores
        .iter()
        .take(len.min(MAX_TACTICAL_ACTIONS))
        .enumerate()
        .filter_map(|(index, score)| score.map(|score| (index, score)))
    {
        if best_strategic - strategic > margin {
            continue;
        }
        let jitter =
            signed_sample(seed, bot_id, tick, RandomStream::Tactic, index as u32) * jitter_scale;
        let final_score = strategic + jitter;
        if selected.is_none_or(|(_, selected_score)| final_score > selected_score) {
            selected = Some((index, final_score));
        }
    }
    selected
}

#[allow(clippy::too_many_arguments)]
fn start_tactical_plan(
    bot_id: usize,
    position: Vec3,
    motor: &FighterMotor,
    stats: &FighterStats,
    action: &FighterActionState,
    character: &FighterCharacter,
    style: &FighterStyle,
    equipment: &FighterEquipment,
    special_state: &FighterSpecialState,
    special_inputs_allowed: bool,
    techniques: &BotTechniqueOptions,
    move_catalog: &CharacterMoveCatalog,
    snapshot: &BotSnapshotBuffer,
    target: FighterSnapshot,
    perceived_position: Vec3,
    perceived_action: FighterAction,
    target_velocity: Vec2,
    profile: BotProfile,
    seed: u64,
    held_kind: Option<ItemKind>,
    slot: &mut BotRuntimeSlot,
) -> bool {
    let memory = &slot.opponents[target.id];
    let target_prediction = memory.perceived_prediction;
    let bot_prediction = snapshot
        .fighters
        .get(bot_id)
        .and_then(|value| *value)
        .and_then(|fighter| fighter.active_prediction);
    let (bot_action_phase, mut bot_remaining) =
        forecast_action_phase(action.action, action.elapsed.as_seconds(), bot_prediction);
    let (target_action_phase, target_remaining) = forecast_action_phase(
        perceived_action,
        memory.perceived_action_elapsed,
        target_prediction,
    );
    let bot_snapshot = snapshot.fighters.get(bot_id).and_then(|value| *value);
    let Some(bot_snapshot) = bot_snapshot.filter(|fighter| fighter.character == character.kind)
    else {
        return false;
    };
    bot_remaining = bot_remaining.max(decision_ticks(bot_snapshot.recovery_remaining));
    if bot_snapshot.cancel_window_open || bot_snapshot.branch_window_open {
        bot_remaining = bot_remaining.min(1);
    }
    let phase = classify_phase(PhaseInputs {
        bot_action: action.action,
        target_action: perceived_action,
        bot_confirmed_hit: action.confirmed_hit,
        bot_confirmed_guard: bot_snapshot.confirmed_guard,
        cancel_window_open: action.cancel_window_open || action.branch_window_open,
        bot_recovery_ticks: bot_remaining,
        target_recovery_ticks: target_remaining,
        stamina_ratio: (stats.stamina / MAX_STAMINA).clamp(0.0, 1.0),
        distance: flat_distance(position, perceived_position),
        bot_edge_risk: snapshot_edge_danger(snapshot, position),
        target_edge_risk: snapshot_edge_danger(snapshot, target.position),
    });
    slot.tactical_phase = phase;

    if advance_active_tactic(
        slot,
        snapshot,
        bot_id,
        position,
        motor,
        stats,
        action,
        target,
        perceived_action,
        profile,
    ) {
        return true;
    }
    if slot.commitment.is_some() {
        return true;
    }
    if held_kind.is_some()
        || !matches!(
            slot.intent.goal,
            BotGoal::Survive
                | BotGoal::RegainStamina
                | BotGoal::Reposition
                | BotGoal::Approach
                | BotGoal::Pressure
                | BotGoal::Punish
                | BotGoal::Disengage
        )
    {
        return false;
    }

    slot.trace.tactic = None;
    slot.trace.forecast_score = 0.0;
    slot.trace.plan_step = 0;
    slot.trace.branch = PlanBranch::Start;
    slot.trace.learned_bias = 0.0;

    let response_context = response_context(
        flat_distance(position, perceived_position),
        slot.last_move_envelope,
        perceived_action,
        snapshot_edge_danger(snapshot, target.position),
        slot.last_outcome,
    );
    let responses = slot.opponents[target.id]
        .responses
        .top_three(response_context);
    slot.trace.predicted_responses = responses;
    let predicted_response = responses[0].response;
    let predicted_attack = response_is_actionable(responses[0], OpponentResponse::Attack);
    let target_airborne = matches!(
        perceived_action,
        FighterAction::Jumping | FighterAction::JumpAttack | FighterAction::JumpHeavyAttack
    );
    let mut candidates = [None; MAX_TACTICAL_ACTIONS];
    let mut candidate_len = 0;
    let distance = flat_distance(position, perceived_position);
    let lead_scale = if action.action == FighterAction::Dashing {
        0.0
    } else {
        (1.0 - profile.perception_error_m * 0.15).clamp(0.75, 1.0)
    };
    let can_chain = action.cancel_window_open || action.branch_window_open;
    let can_request_neutral = neutral_action_available(action.action);
    let can_request_mobility = can_request_neutral || action.action == FighterAction::Dashing;
    let vertical_reposition = vertically_stacked(position.y, perceived_position.y);
    let offense_ready = slot.decision_tick >= slot.attack_ready_tick || can_chain;
    let survival_phase = matches!(phase, TacticalPhase::Disadvantage | TacticalPhase::WakeUp);
    let resource_recovery = phase == TacticalPhase::ResourceRecovery;
    let intentional_mistake =
        bot_sample(seed, bot_id, slot.decision_tick, RandomStream::Mistake, 1)
            < profile.intentional_mistake_rate;

    if offense_ready && !survival_phase && !intentional_mistake {
        if let Some(prediction) = techniques.light
            && evaluate_technique(
                prediction,
                BotSemanticAction::Light,
                position,
                motor,
                stats,
                perceived_position,
                target_velocity,
                lead_scale,
                profile.attack_stamina_reserve_ratio,
            )
            .is_some()
        {
            add_tactical_candidate(
                &mut candidates,
                &mut candidate_len,
                BotSemanticAction::Light,
                ForecastMoveFacts::from_prediction(BotSemanticAction::Light, prediction),
                phase,
                predicted_response,
                target_airborne,
                (target_remaining
                    > ForecastMoveFacts::from_prediction(BotSemanticAction::Light, prediction)
                        .startup_ticks
                        .saturating_add(1))
                .then_some(TacticId::WhiffPunish),
            );
        }
        if let Some(prediction) = techniques.heavy {
            let facts = ForecastMoveFacts::from_prediction(BotSemanticAction::Heavy, prediction);
            let punish_window = matches!(
                phase,
                TacticalPhase::Advantage
                    | TacticalPhase::HitConfirm
                    | TacticalPhase::GuardPressure
                    | TacticalPhase::EdgePressure
            ) || target_remaining > facts.startup_ticks.saturating_add(1);
            if punish_window
                && evaluate_technique(
                    prediction,
                    BotSemanticAction::Heavy,
                    position,
                    motor,
                    stats,
                    perceived_position,
                    target_velocity,
                    lead_scale,
                    profile.attack_stamina_reserve_ratio,
                )
                .is_some()
            {
                add_tactical_candidate(
                    &mut candidates,
                    &mut candidate_len,
                    BotSemanticAction::Heavy,
                    facts,
                    phase,
                    predicted_response,
                    target_airborne,
                    (target_remaining > facts.startup_ticks.saturating_add(1))
                        .then_some(TacticId::WhiffPunish),
                );
            }
        }
        if can_request_neutral && motor.grounded && distance < 0.95 {
            add_tactical_candidate(
                &mut candidates,
                &mut candidate_len,
                BotSemanticAction::Grab,
                ForecastMoveFacts::fallback(BotSemanticAction::Grab, distance),
                phase,
                predicted_response,
                target_airborne,
                None,
            );
        }
    }

    if can_request_neutral
        && motor.grounded
        && slot.decision_tick >= slot.dash_ready_tick
        && (survival_phase
            || vertical_reposition
            || distance > 2.25
            || (resource_recovery && distance > 1.15)
            || predicted_attack
            || (matches!(
                predicted_response,
                OpponentResponse::Dodge | OpponentResponse::Retreat
            ) && distance > 1.15))
    {
        add_tactical_candidate(
            &mut candidates,
            &mut candidate_len,
            BotSemanticAction::Dash,
            ForecastMoveFacts::fallback(BotSemanticAction::Dash, distance),
            phase,
            predicted_response,
            target_airborne,
            Some(if survival_phase || vertical_reposition {
                TacticId::EscapePressure
            } else if predicted_attack {
                TacticId::BaitAndPunish
            } else {
                TacticId::NeutralPoke
            }),
        );
    }
    if can_request_mobility
        && motor.grounded
        && (phase == TacticalPhase::Disadvantage || target_airborne)
    {
        add_tactical_candidate(
            &mut candidates,
            &mut candidate_len,
            BotSemanticAction::Jump,
            ForecastMoveFacts::fallback(BotSemanticAction::Jump, distance),
            phase,
            predicted_response,
            target_airborne,
            None,
        );
    }
    if can_request_neutral
        && !survival_phase
        && !intentional_mistake
        && special_inputs_allowed
        && !special_state.cooldown.active()
    {
        for (semantic, legal) in [
            (
                BotSemanticAction::SpecialProjectile,
                (2.4..6.0).contains(&distance),
            ),
            (BotSemanticAction::SpecialTrap, distance < 1.35),
            (
                BotSemanticAction::SpecialHazard,
                (1.6..3.4).contains(&distance),
            ),
            (
                BotSemanticAction::SpecialShockwave,
                (1.4..3.8).contains(&distance),
            ),
        ] {
            if legal {
                add_tactical_candidate(
                    &mut candidates,
                    &mut candidate_len,
                    semantic,
                    ForecastMoveFacts::fallback(semantic, distance),
                    phase,
                    predicted_response,
                    target_airborne,
                    None,
                );
            }
        }
    }
    if candidate_len == 0 {
        return survival_phase;
    }

    let state = CombatForecastState {
        bot_position: Vec2::new(position.x, position.z),
        target_position: Vec2::new(perceived_position.x, perceived_position.z),
        bot_velocity: bounded_velocity(bot_snapshot.velocity),
        target_velocity,
        bot_facing: vec2_normalize_or_zero(Vec2::new(motor.facing.x, motor.facing.z)),
        target_facing: vec2_normalize_or_zero(Vec2::new(target.facing.x, target.facing.z)),
        bot_action: action.action,
        target_action: perceived_action,
        bot_action_phase,
        target_action_phase,
        bot_action_ticks_remaining: bot_remaining,
        target_action_ticks_remaining: target_remaining,
        bot_health: stats.health,
        target_health: target.health,
        bot_stamina: stats.stamina,
        target_stamina: target.stamina,
        bot_grounded: bot_snapshot.grounded,
        target_grounded: !target_airborne,
        bot_edge_risk: snapshot_edge_danger(snapshot, position),
        target_edge_risk: snapshot_edge_danger(snapshot, target.position),
        bot_move: bot_prediction.and_then(|prediction| {
            semantic_for_fighter_action(prediction.action)
                .map(|semantic| ForecastMoveFacts::from_prediction(semantic, prediction))
        }),
        target_move: target_prediction.and_then(|prediction| {
            semantic_for_fighter_action(prediction.action)
                .map(|semantic| ForecastMoveFacts::from_prediction(semantic, prediction))
        }),
        external_threat_cost: external_threat_cost(snapshot, slot, bot_id, target.id, position),
    };

    let mut scored: [Option<(
        ForecastAction,
        f32,
        [TacticalFollowUp; MAX_FORECAST_FOLLOW_UPS],
    )>; MAX_TACTICAL_ACTIONS] = [None; MAX_TACTICAL_ACTIONS];
    let mut strategic_scores = [None; MAX_TACTICAL_ACTIONS];
    let mut total_outcomes = 0_u16;
    for index in 0..candidate_len {
        let candidate = candidates[index].expect("candidate prefix is dense");
        let follow_ups = authored_follow_ups(
            candidate_prediction(candidate.action, techniques),
            motor.grounded,
            stats.stamina,
            character,
            style,
            equipment,
            move_catalog,
        );
        let forecast_follow_ups = follow_ups.map(|follow| ForecastFollowUp {
            facts: follow.facts,
        });
        let score = score_forecast_plan(
            state,
            candidate,
            responses,
            forecast_follow_ups,
            profile.forecast_horizon_ticks,
            profile.forecast_risk_weight,
            slot.tactic_biases[candidate.tactic.index()],
            0.0,
        );
        total_outcomes = total_outcomes.saturating_add(score.outcomes_evaluated);
        strategic_scores[index] = Some(score.strategic_score);
        scored[index] = Some((candidate, score.strategic_score, follow_ups));
    }
    debug_assert!(
        usize::from(total_outcomes)
            <= MAX_TACTICAL_ACTIONS * MAX_RESPONSE_BRANCHES * MAX_FORECAST_FOLLOW_UPS
    );

    let Some((selected_index, forecast_score)) = select_near_optimal_index(
        &strategic_scores,
        candidate_len,
        profile.near_optimal_margin,
        profile.utility_jitter,
        seed,
        bot_id,
        slot.decision_tick,
    ) else {
        return false;
    };
    let Some((selected, _, follow_ups)) = scored[selected_index] else {
        return false;
    };

    let expected = expected_action_for_semantic(selected.action, techniques);
    let first_deadline = selected
        .facts
        .recovery_ticks
        .saturating_add(4)
        .clamp(4, profile.forecast_horizon_ticks.min(u8::MAX as u32) as u8);
    let first_requires_grounded = candidate_prediction(selected.action, techniques)
        .is_some_and(|prediction| prediction.status == TechniqueStatus::Grounded)
        || matches!(
            selected.action,
            BotSemanticAction::Dash | BotSemanticAction::Grab
        );
    let mut first = PlanStep::action(selected.action, expected, PlanBranch::Start, first_deadline)
        .with_requirements(selected.facts.stamina_cost, first_requires_grounded);
    if matches!(
        selected.tactic,
        TacticId::BaitAndPunish | TacticId::EscapePressure
    ) && selected.action == BotSemanticAction::Dash
    {
        first.movement = PlanMovement::Retreat;
    } else if selected.action == BotSemanticAction::Dash {
        first.movement = PlanMovement::Approach;
    } else if matches!(
        selected.tactic,
        TacticId::EdgeControl | TacticId::PressureString
    ) {
        first.movement = PlanMovement::Approach;
    } else if selected.tactic == TacticId::NeutralPoke {
        first.movement = PlanMovement::Strafe;
    }
    let mut steps = [PlanStep::default(); MAX_PLAN_STEPS];
    steps[0] = first;
    let mut next_step = 1;

    if selected.tactic == TacticId::StrikeThrow
        && selected.action == BotSemanticAction::Light
        && next_step < MAX_PLAN_STEPS
    {
        steps[next_step] = PlanStep::action(
            BotSemanticAction::Grab,
            Some(FighterAction::GrabStartup),
            PlanBranch::Guarded,
            18,
        )
        .with_requirements(0.0, true);
        next_step += 1;
    } else if selected.tactic == TacticId::BaitAndPunish
        && selected.action == BotSemanticAction::Dash
        && let Some(prediction) = techniques.light
        && next_step < MAX_PLAN_STEPS
    {
        steps[next_step] = PlanStep::action(
            BotSemanticAction::Light,
            Some(prediction.action),
            PlanBranch::Threatened,
            ForecastMoveFacts::from_prediction(BotSemanticAction::Light, prediction)
                .recovery_ticks
                .saturating_add(4)
                .max(4),
        )
        .with_requirements(
            prediction.stamina_cost,
            prediction.status == TechniqueStatus::Grounded,
        );
        next_step += 1;
    }
    for follow_up in follow_ups {
        if next_step >= MAX_PLAN_STEPS {
            break;
        }
        let (Some(semantic), Some(facts)) = (follow_up.action, follow_up.facts) else {
            continue;
        };
        if steps[..next_step]
            .iter()
            .any(|step| step.enabled && step.condition == follow_up.branch)
        {
            continue;
        }
        steps[next_step] = PlanStep::action(
            semantic,
            follow_up.expected_action,
            follow_up.branch,
            facts.recovery_ticks.saturating_add(4).max(4),
        )
        .with_requirements(facts.stamina_cost, follow_up.requires_grounded);
        next_step += 1;
    }

    let learned_bias = slot.tactic_biases[selected.tactic.index()];
    slot.last_move_envelope = selected.facts.range.max(0.75);
    slot.active_plan = Some(ActiveTacticPlan {
        target_id: target.id,
        tactic: selected.tactic,
        expected_response: predicted_response,
        steps,
        current_step: 0,
        branch: PlanBranch::Start,
        deadline_tick: slot.decision_tick + u64::from(first_deadline),
        step_started_tick: slot.decision_tick,
        step_committed: false,
        last_outcome_tick: slot.last_outcome_tick,
        rejection_count: 0,
        origin: position,
        target_origin: target.position,
        start_bot_health: stats.health,
        start_target_health: target.health,
        start_bot_stamina: stats.stamina,
        start_target_stamina: target.stamina,
        accumulated: TacticOutcome::default(),
        forecast_score,
        learned_bias,
    });
    slot.trace.phase = phase;
    slot.trace.tactic = Some(selected.tactic);
    slot.trace.forecast_score = forecast_score;
    slot.trace.plan_step = 0;
    slot.trace.branch = PlanBranch::Start;
    slot.trace.learned_bias = learned_bias;
    apply_active_plan_step(slot, snapshot, position, target.position)
}

#[allow(clippy::too_many_arguments)]
fn plan_bot(
    arena_index: usize,
    bot_id: usize,
    position: Vec3,
    motor: &FighterMotor,
    special_state: &FighterSpecialState,
    character: &FighterCharacter,
    style: &FighterStyle,
    equipment: &FighterEquipment,
    stats: &FighterStats,
    action: &FighterActionState,
    difficulty: BotDifficulty,
    held_kind: Option<ItemKind>,
    special_inputs_allowed: bool,
    techniques: &BotTechniqueOptions,
    move_catalog: &CharacterMoveCatalog,
    snapshot: &BotSnapshotBuffer,
    profile: BotProfile,
    seed: u64,
    slot: &mut BotRuntimeSlot,
    navigation: &mut BotNavigationCache,
    split_causeway_doors: &SplitCausewayDoorState,
) {
    let Some(target_id) = slot.target_id else {
        slot.intent = BotIntent::default();
        slot.commitment = None;
        slot.trace = BotDecisionTrace::default();
        return;
    };
    let Some(target) = snapshot.fighters[target_id] else {
        slot.intent = BotIntent::default();
        slot.commitment = None;
        slot.trace = BotDecisionTrace::default();
        return;
    };
    let (
        perceived_action,
        perceived_action_elapsed,
        opponent_aggression_rate,
        opponent_guard_rate,
        target_velocity,
    ) = {
        let memory = &slot.opponents[target_id];
        (
            memory.perceived_action.unwrap_or(FighterAction::Idle),
            memory.perceived_action_elapsed,
            memory.rate(0),
            memory.rate(1),
            memory.velocity,
        )
    };
    let error = Vec2::new(
        signed_sample(
            seed,
            bot_id,
            slot.decision_tick,
            RandomStream::PerceptionX,
            target_id as u32,
        ),
        signed_sample(
            seed,
            bot_id,
            slot.decision_tick,
            RandomStream::PerceptionZ,
            target_id as u32,
        ),
    ) * profile.perception_error_m;
    let perceived_position = target.position + Vec3::new(error.x, 0.0, error.y);
    let lead_scale = if action.action == FighterAction::Dashing {
        0.0
    } else if difficulty == BotDifficulty::Tutorial {
        0.6
    } else {
        (1.0 - profile.perception_error_m * 0.15).clamp(0.75, 1.0)
    };
    let intercept_position = perceived_position
        + Vec3::new(
            target_velocity.x * 0.15 * lead_scale,
            0.0,
            target_velocity.y * 0.15 * lead_scale,
        );
    let perceived_distance = flat_distance(position, perceived_position);
    slot.trace.target_distance = perceived_distance;
    let distance = flat_distance(position, intercept_position);
    let direct = vec2_normalize_or_zero(Vec2::new(
        intercept_position.x - position.x,
        intercept_position.z - position.z,
    ));
    if bot_sample(seed, bot_id, slot.decision_tick, RandomStream::Strafe, 0) < 0.12 {
        slot.strafe_sign *= -1.0;
    }
    let strafe = Vec2::new(-direct.y, direct.x) * slot.strafe_sign;
    let personality = bot_personality(style.kind, equipment.kind);
    let mut range = bot_range_band(style_tuning(style.kind).bot_preferred_range, personality);
    let authored_contact_envelope = [
        (BotSemanticAction::Light, techniques.light),
        (BotSemanticAction::Heavy, techniques.heavy),
    ]
    .into_iter()
    .filter_map(|(semantic, prediction)| {
        let prediction = prediction?;
        Some((
            technique_planning_confidence(prediction, semantic)?,
            technique_engagement_envelope(prediction),
        ))
    })
    .max_by(|left, right| left.0.total_cmp(&right.0))
    .map_or(0.0, |(_, envelope)| envelope);
    if authored_contact_envelope > 0.0 {
        let authored_ideal = (authored_contact_envelope * 0.72).clamp(0.6, 2.25);
        range.ideal = range.ideal.min(authored_ideal);
        range.min = (range.ideal * 0.68).clamp(0.45, range.ideal - 0.1);
        range.max = (authored_contact_envelope * 0.92).max(range.ideal + 0.15);
    }
    let health_ratio = (stats.health / 100.0).clamp(0.0, 1.0);
    let stamina_ratio = (stats.stamina / MAX_STAMINA).clamp(0.0, 1.0);
    let danger_need = ((profile.danger_health_ratio - health_ratio)
        / profile.danger_health_ratio.max(0.01))
    .clamp(0.0, 1.0);
    let stamina_need = ((profile.low_stamina_ratio - stamina_ratio)
        / profile.low_stamina_ratio.max(0.01))
    .clamp(0.0, 1.0);
    let target_edge = snapshot_edge_danger(snapshot, target.position);
    let self_edge = snapshot_edge_danger(snapshot, position);
    let aggressive_adaptation =
        (opponent_aggression_rate * profile.adaptation_cap.min(0.2)).clamp(0.0, 0.2);
    let style_modifier = personality.aggression.clamp(
        1.0 - profile.style_modifier_cap,
        1.0 + profile.style_modifier_cap,
    );
    let item_modifier = personality.item_greed.clamp(
        1.0 - profile.equipment_modifier_cap,
        1.0 + profile.equipment_modifier_cap,
    );

    let mut best_item: Option<(Vec3, f32, f32)> = None;
    for item in &snapshot.items {
        if !item.loose {
            continue;
        }
        let item_distance = flat_distance(position, item.position);
        let score = bot_pickup_score(item.kind, stats.stamina, distance, item_distance);
        if best_item.is_none_or(|(_, best_score, _)| score > best_score) {
            best_item = Some((item.position, score, item_distance));
        }
    }

    let approach_need = ((distance - range.ideal) / range.max.max(0.01)).clamp(0.0, 1.0);
    let pressure_need = (1.0 - (distance - range.ideal).abs() / range.max).clamp(0.0, 1.0);
    let target_weakness = ((1.0 - (target.health / 100.0).clamp(0.0, 1.0))
        + (1.0 - (target.stamina / MAX_STAMINA).clamp(0.0, 1.0)))
        * 0.5;
    let punish_need =
        vulnerability(perceived_action) * (1.0 + perceived_action_elapsed.clamp(0.0, 1.0) * 0.35);
    let collect_need = best_item.map_or(0.0, |(_, score, _)| score.max(0.0));
    let goals = [
        (
            BotGoal::Survive,
            profile.weights.survive * (danger_need + self_edge),
        ),
        (
            BotGoal::RegainStamina,
            profile.weights.regain_stamina * stamina_need,
        ),
        (BotGoal::Reposition, profile.weights.reposition * 0.28),
        (
            BotGoal::Approach,
            profile.weights.approach * approach_need * style_modifier,
        ),
        (
            BotGoal::Pressure,
            profile.weights.pressure * (pressure_need + target_weakness * 0.25) * style_modifier
                + profile.weights.objective * target_edge,
        ),
        (
            BotGoal::Punish,
            profile.weights.punish * punish_need * style_modifier,
        ),
        (
            BotGoal::Disengage,
            profile.weights.disengage * (danger_need + aggressive_adaptation),
        ),
        (
            BotGoal::CollectItem,
            profile.weights.collect_item * collect_need * item_modifier,
        ),
        (
            BotGoal::UseItem,
            profile.weights.use_item * if held_kind.is_some() { 1.0 } else { 0.0 },
        ),
    ];
    let mut selected_goal = BotGoal::Reposition;
    let mut selected_score = f32::NEG_INFINITY;
    for (index, (goal, base_score)) in goals.into_iter().enumerate() {
        let jitter = signed_sample(
            seed,
            bot_id,
            slot.decision_tick,
            RandomStream::Goal,
            index as u32,
        ) * profile.utility_jitter;
        let score = base_score * (1.0 + jitter);
        if score > selected_score {
            selected_goal = goal;
            selected_score = score;
        }
    }

    let destination = if selected_goal == BotGoal::CollectItem {
        best_item.map(|value| value.0).unwrap_or(intercept_position)
    } else {
        intercept_position
    };
    let routed = navigation
        .next_direction(
            bot_id,
            slot.decision_tick,
            arena_index,
            position,
            destination,
            &snapshot.navigation_blockers,
            split_causeway_doors,
        )
        .unwrap_or(direct);
    let movement = if vertically_stacked(position.y, perceived_position.y) {
        -direct + strafe * 0.35
    } else {
        match selected_goal {
            BotGoal::Survive | BotGoal::Disengage | BotGoal::RegainStamina => {
                defensive_away_from(position, perceived_position) + strafe * 0.22
            }
            BotGoal::Approach | BotGoal::CollectItem | BotGoal::Punish => routed + strafe * 0.16,
            BotGoal::Pressure => routed * 0.82 + strafe * 0.25,
            BotGoal::Reposition => {
                if distance < range.min {
                    -direct * 0.5 + strafe
                } else if distance > range.max {
                    direct * 0.4 + strafe
                } else {
                    strafe
                }
            }
            BotGoal::UseItem | BotGoal::Idle => Vec2::ZERO,
        }
    };
    let movement = snapshot_edge_steering(snapshot, position, vec2_normalize_or_zero(movement));
    let delayed_target = BotTargetSnapshot {
        fighter_id: crate::determinism::FighterId::from_index(target.id)
            .expect("bot snapshot fighter IDs are canonical"),
        position: perceived_position,
        distance: perceived_distance,
        facing: target.facing,
        action: perceived_action,
    };
    let guard = bot_should_guard_threat(position, delayed_target) && motor.grounded;
    slot.intent = BotIntent {
        goal: selected_goal,
        movement,
        guard,
    };
    slot.trace.target_id = Some(target_id);
    slot.trace.goal = selected_goal;
    slot.trace.reason = reason_for_goal(selected_goal, target_edge, punish_need);
    slot.trace.utility_score = selected_score;
    refresh_trace_state(slot);

    if profile.tactical_planning_enabled && difficulty == BotDifficulty::Standard {
        #[cfg(feature = "perf")]
        let planner_started = std::time::Instant::now();
        let handled = start_tactical_plan(
            bot_id,
            position,
            motor,
            stats,
            action,
            character,
            style,
            equipment,
            special_state,
            special_inputs_allowed,
            techniques,
            move_catalog,
            snapshot,
            target,
            perceived_position,
            perceived_action,
            target_velocity,
            profile,
            seed,
            held_kind,
            slot,
        );
        #[cfg(feature = "perf")]
        slot.record_planner_timing(planner_started.elapsed());
        if handled {
            refresh_trace_state(slot);
            return;
        }
    }

    if slot.commitment.is_some() {
        return;
    }
    let mistake = bot_sample(seed, bot_id, slot.decision_tick, RandomStream::Mistake, 0)
        < profile.intentional_mistake_rate;
    let mut best_action: Option<(BotSemanticAction, f32)> = None;
    let mut consider = |candidate: BotSemanticAction, base_score: f32| {
        let jitter = signed_sample(
            seed,
            bot_id,
            slot.decision_tick,
            RandomStream::Action,
            candidate as u32,
        ) * profile.utility_jitter;
        consider_action(&mut best_action, candidate, base_score * (1.0 + jitter));
    };
    if bot_should_jump_for_elevation(
        position,
        movement,
        motor.grounded,
        arena_definition(arena_index),
    ) {
        consider(BotSemanticAction::Jump, 10.0);
    }
    if neutral_action_available(action.action)
        && slot.decision_tick >= slot.dash_ready_tick
        && motor.grounded
        && vec2_length_squared(movement) > 0.1
        && (distance > range.max + 0.45 || selected_goal == BotGoal::Disengage)
    {
        consider(BotSemanticAction::Dash, 2.0);
    }
    if neutral_action_available(action.action) {
        if let Some(kind) = held_kind {
            let wave = signed_sample(seed, bot_id, slot.decision_tick, RandomStream::Item, 0);
            if let Some(decision) = bot_held_item_decision(
                kind,
                stats.stamina,
                distance,
                if slot.decision_tick >= slot.attack_ready_tick {
                    TickTimer::ZERO
                } else {
                    TickTimer::from_ticks(1)
                },
                wave >= 0.0,
            ) {
                consider(
                    match decision {
                        BotHeldItemDecision::Light => BotSemanticAction::ItemLight,
                        BotHeldItemDecision::Heavy => BotSemanticAction::ItemHeavy,
                    },
                    4.0,
                );
            }
        } else if best_item.is_some_and(|(_, score, item_distance)| {
            score > 0.25 && item_distance <= ITEM_PICKUP_RANGE + 0.25
        }) {
            consider(BotSemanticAction::Pickup, 3.2);
        }
    }

    if !mistake && slot.decision_tick >= slot.attack_ready_tick {
        if let Some(prediction) = techniques.light {
            if let Some(evaluation) = evaluate_technique(
                prediction,
                BotSemanticAction::Light,
                position,
                motor,
                stats,
                perceived_position,
                target_velocity,
                lead_scale,
                profile.attack_stamina_reserve_ratio,
            ) {
                let closing_bonus = 1.0
                    - flat_distance(position, evaluation.predicted_position)
                        / technique_engagement_envelope(prediction).max(0.1);
                consider(
                    BotSemanticAction::Light,
                    offensive_candidate_score(
                        prediction,
                        BotSemanticAction::Light,
                        evaluation,
                        punish_need,
                        closing_bonus.max(0.0),
                    ),
                );
            }
        }
        if let Some(prediction) = techniques.heavy {
            if let Some(evaluation) = evaluate_technique(
                prediction,
                BotSemanticAction::Heavy,
                position,
                motor,
                stats,
                perceived_position,
                target_velocity,
                lead_scale,
                profile.attack_stamina_reserve_ratio,
            ) {
                consider(
                    BotSemanticAction::Heavy,
                    offensive_candidate_score(
                        prediction,
                        BotSemanticAction::Heavy,
                        evaluation,
                        punish_need,
                        0.0,
                    ),
                );
            }
        }
        if difficulty == BotDifficulty::Standard
            && neutral_action_available(action.action)
            && motor.grounded
            && perceived_distance < 0.9
        {
            consider(
                BotSemanticAction::Grab,
                grab_candidate_score(opponent_guard_rate, punish_need),
            );
        }
        if neutral_action_available(action.action)
            && special_inputs_allowed
            && held_kind.is_none()
            && !special_state.cooldown.active()
        {
            if (2.4..6.0).contains(&distance) {
                consider(BotSemanticAction::SpecialProjectile, 2.45);
            }
            if distance < 1.35 {
                consider(BotSemanticAction::SpecialTrap, 2.4);
            }
            if (1.6..3.4).contains(&distance) {
                consider(BotSemanticAction::SpecialHazard, 2.3);
            }
            if (1.4..3.8).contains(&distance) {
                consider(BotSemanticAction::SpecialShockwave, 2.35);
            }
        }
    }
    drop(consider);

    if let Some((selected, selected_score)) = best_action {
        if selected_score > 0.0 {
            let min = profile.commitment_ticks_min as u64;
            let max = (profile.commitment_ticks_max as u64).max(min);
            let span = max - min + 1;
            let duration = min
                + (bot_sample(
                    seed,
                    bot_id,
                    slot.decision_tick,
                    RandomStream::Commitment,
                    selected as u32,
                ) * span as f32)
                    .floor()
                    .min((span - 1) as f32) as u64;
            begin_commitment(
                slot,
                selected,
                match selected {
                    BotSemanticAction::Light => {
                        techniques.light.map(|prediction| prediction.action)
                    }
                    BotSemanticAction::Heavy => {
                        techniques.heavy.map(|prediction| prediction.action)
                    }
                    _ => None,
                },
                duration,
            );
        }
    }
    refresh_trace_state(slot);
    let _ = action;
}

#[cfg(any(test, feature = "bot-quality"))]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TacticsQualityReport {
    pub(crate) characters: u32,
    pub(crate) seeds: u32,
    pub(crate) whiff_punishes: u32,
    pub(crate) whiff_trials: u32,
    pub(crate) anti_airs: u32,
    pub(crate) jump_trials: u32,
    pub(crate) safe_escapes: u32,
    pub(crate) pressure_trials: u32,
    pub(crate) hit_confirm_follow_ups: u32,
    pub(crate) hit_confirm_trials: u32,
    pub(crate) guard_counter_before: u32,
    pub(crate) guard_counter_after: u32,
    pub(crate) guard_trials: u32,
    pub(crate) failed_tactic_first_half: u32,
    pub(crate) failed_tactic_second_half: u32,
    pub(crate) failed_tactic_half_trials: u32,
    pub(crate) behavior_trials: [u32; 6],
}

#[cfg(any(test, feature = "bot-quality"))]
impl TacticsQualityReport {
    pub(crate) fn passed(self) -> bool {
        self.characters == CHARACTER_KINDS.len() as u32
            && self.seeds >= 3
            && percentage_at_least(self.whiff_punishes, self.whiff_trials, 70)
            && percentage_at_least(self.anti_airs, self.jump_trials, 60)
            && percentage_at_least(self.safe_escapes, self.pressure_trials, 80)
            && percentage_at_least(self.hit_confirm_follow_ups, self.hit_confirm_trials, 70)
            && self.guard_counter_after > self.guard_counter_before
            && self.failed_tactic_first_half > 0
            && self.failed_tactic_second_half * 100
                <= self.failed_tactic_first_half.saturating_mul(70)
    }
}

#[cfg(any(test, feature = "bot-quality"))]
fn percentage_at_least(successes: u32, trials: u32, required: u32) -> bool {
    trials > 0 && successes.saturating_mul(100) >= trials.saturating_mul(required)
}

#[cfg(any(test, feature = "bot-quality"))]
fn quality_responses(primary: OpponentResponse) -> [ResponsePrediction; MAX_RESPONSE_BRANCHES] {
    let secondary = match primary {
        OpponentResponse::Attack => OpponentResponse::Guard,
        OpponentResponse::Guard => OpponentResponse::Wait,
        OpponentResponse::Grab => OpponentResponse::Attack,
        OpponentResponse::Jump => OpponentResponse::Wait,
        OpponentResponse::Dodge => OpponentResponse::Retreat,
        OpponentResponse::Retreat => OpponentResponse::Wait,
        OpponentResponse::Special => OpponentResponse::Guard,
        OpponentResponse::Wait => OpponentResponse::Guard,
    };
    [
        ResponsePrediction {
            response: primary,
            probability: 0.70,
        },
        ResponsePrediction {
            response: secondary,
            probability: 0.20,
        },
        ResponsePrediction {
            response: OpponentResponse::Wait,
            probability: 0.10,
        },
    ]
}

#[cfg(any(test, feature = "bot-quality"))]
fn quality_select_action(
    state: CombatForecastState,
    candidates: &[Option<ForecastAction>; MAX_TACTICAL_ACTIONS],
    len: usize,
    responses: [ResponsePrediction; MAX_RESPONSE_BRANCHES],
    biases: &[f32; TacticId::COUNT],
    profile: BotProfile,
    seed: u64,
    tick: u64,
) -> Option<ForecastAction> {
    let mut scores = [None; MAX_TACTICAL_ACTIONS];
    for index in 0..len.min(MAX_TACTICAL_ACTIONS) {
        let candidate = candidates[index]?;
        scores[index] = Some(
            score_forecast_plan(
                state,
                candidate,
                responses,
                [ForecastFollowUp::default(); MAX_FORECAST_FOLLOW_UPS],
                profile.forecast_horizon_ticks,
                profile.forecast_risk_weight,
                biases[candidate.tactic.index()],
                0.0,
            )
            .strategic_score,
        );
    }
    let (selected, _) = select_near_optimal_index(
        &scores,
        len,
        profile.near_optimal_margin,
        profile.utility_jitter,
        seed,
        0,
        tick,
    )?;
    candidates[selected]
}

#[cfg(any(test, feature = "bot-quality"))]
fn neutral_quality_prediction(
    character: CharacterKind,
    button: TechniqueButton,
    style: &FighterStyle,
    equipment: &FighterEquipment,
    catalog: &CharacterMoveCatalog,
) -> Option<TechniquePrediction> {
    technique_prediction_for_context_in_catalog(
        TechniqueMatchContext {
            previous: None,
            button,
            elapsed: 0.0,
            style: style.kind,
            loadout: LoadoutContext::for_character(character, style.kind, equipment.kind),
            grounded: true,
            confirmed_hit: false,
            cancel_window_open: false,
            branch_window_open: false,
            current_action: FighterAction::Idle,
        },
        MAX_STAMINA,
        catalog,
    )
}

#[cfg(any(test, feature = "bot-quality"))]
pub(crate) fn run_tactics_quality_fixture(
    catalog: &CharacterMoveCatalog,
    profiles: &BotProfileCatalog,
) -> TacticsQualityReport {
    const SEEDS: [u64; 4] = [0xffc0_0001, 0x00a1_1ce5, 0x00c0_ffee, 0xdead_beef];
    let profile = profiles.profile(BotProfileId::Standard);
    let style = FighterStyle {
        kind: crate::styles::FighterStyleKind::Catalyst,
    };
    let equipment = FighterEquipment {
        kind: crate::equipment::EquipmentKind::CounterCell,
        cooldown: TickTimer::ZERO,
    };
    let mut report = TacticsQualityReport {
        characters: CHARACTER_KINDS.len() as u32,
        seeds: SEEDS.len() as u32,
        ..Default::default()
    };

    for (character_index, character) in CHARACTER_KINDS.into_iter().enumerate() {
        let Some(light_prediction) =
            neutral_quality_prediction(character, TechniqueButton::A, &style, &equipment, catalog)
        else {
            continue;
        };
        let Some(heavy_prediction) =
            neutral_quality_prediction(character, TechniqueButton::B, &style, &equipment, catalog)
        else {
            continue;
        };
        let light_facts =
            ForecastMoveFacts::from_prediction(BotSemanticAction::Light, light_prediction);
        let heavy_facts =
            ForecastMoveFacts::from_prediction(BotSemanticAction::Heavy, heavy_prediction);
        let distance = light_facts
            .range
            .max(0.8)
            .min(heavy_facts.range.max(0.8))
            .mul_add(0.65, 0.0)
            .clamp(0.5, 1.25);
        let base_state = CombatForecastState {
            bot_position: Vec2::ZERO,
            target_position: Vec2::new(distance, 0.0),
            bot_facing: Vec2::X,
            target_facing: -Vec2::X,
            bot_health: 100.0,
            target_health: 100.0,
            bot_stamina: MAX_STAMINA,
            target_stamina: MAX_STAMINA,
            bot_grounded: true,
            target_grounded: true,
            target_action_phase: ForecastActionPhase::Recovery,
            target_action_ticks_remaining: 14,
            target_move: Some(heavy_facts),
            ..Default::default()
        };
        let no_bias = [0.0; TacticId::COUNT];

        for (seed_index, seed) in SEEDS.into_iter().enumerate() {
            let tick = (character_index * SEEDS.len() + seed_index + 1) as u64;

            report.behavior_trials[0] += 1;
            report.whiff_trials += 1;
            let whiff_candidates = [
                Some(ForecastAction {
                    action: BotSemanticAction::Heavy,
                    tactic: TacticId::WhiffPunish,
                    facts: heavy_facts,
                }),
                Some(ForecastAction {
                    action: BotSemanticAction::Light,
                    tactic: TacticId::NeutralPoke,
                    facts: light_facts,
                }),
                Some(ForecastAction {
                    action: BotSemanticAction::Dash,
                    tactic: TacticId::BaitAndPunish,
                    facts: ForecastMoveFacts::fallback(BotSemanticAction::Dash, distance),
                }),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ];
            if quality_select_action(
                base_state,
                &whiff_candidates,
                3,
                quality_responses(OpponentResponse::Attack),
                &no_bias,
                profile,
                seed,
                tick,
            )
            .is_some_and(|choice| choice.tactic == TacticId::WhiffPunish)
            {
                report.whiff_punishes += 1;
            }

            report.behavior_trials[1] += 1;
            report.jump_trials += 1;
            let jump_candidates = [
                Some(ForecastAction {
                    action: BotSemanticAction::Light,
                    tactic: TacticId::AntiAir,
                    facts: light_facts,
                }),
                Some(ForecastAction {
                    action: BotSemanticAction::Heavy,
                    tactic: TacticId::NeutralPoke,
                    facts: heavy_facts,
                }),
                Some(ForecastAction {
                    action: BotSemanticAction::Dash,
                    tactic: TacticId::BaitAndPunish,
                    facts: ForecastMoveFacts::fallback(BotSemanticAction::Dash, distance),
                }),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ];
            let jump_state = CombatForecastState {
                target_grounded: false,
                target_position: Vec2::new(distance, 0.0),
                ..base_state
            };
            if quality_select_action(
                jump_state,
                &jump_candidates,
                3,
                quality_responses(OpponentResponse::Jump),
                &no_bias,
                profile,
                seed,
                tick,
            )
            .is_some_and(|choice| choice.tactic == TacticId::AntiAir)
            {
                report.anti_airs += 1;
            }

            report.behavior_trials[2] += 1;
            let retreat_candidates = [
                Some(ForecastAction {
                    action: BotSemanticAction::SpecialProjectile,
                    tactic: TacticId::ProjectilePressure,
                    facts: ForecastMoveFacts::fallback(BotSemanticAction::SpecialProjectile, 4.0),
                }),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ];
            let _ = quality_select_action(
                CombatForecastState {
                    target_position: Vec2::new(4.0, 0.0),
                    ..base_state
                },
                &retreat_candidates,
                2,
                quality_responses(OpponentResponse::Retreat),
                &no_bias,
                profile,
                seed,
                tick,
            );

            report.behavior_trials[3] += 1;
            report.guard_trials += 1;
            let context = response_context(
                distance,
                light_facts.range,
                FighterAction::Idle,
                0.0,
                PreviousOutcome::None,
            );
            let mut response_model = OpponentResponseModel::default();
            let before = response_model.top_three(context);
            let before_candidates = [
                Some(ForecastAction {
                    action: BotSemanticAction::Light,
                    tactic: tactic_for_action(
                        BotSemanticAction::Light,
                        TacticalPhase::Neutral,
                        before[0].response,
                        false,
                    ),
                    facts: light_facts,
                }),
                Some(ForecastAction {
                    action: BotSemanticAction::Grab,
                    tactic: tactic_for_action(
                        BotSemanticAction::Grab,
                        TacticalPhase::Neutral,
                        before[0].response,
                        false,
                    ),
                    facts: ForecastMoveFacts::fallback(BotSemanticAction::Grab, distance),
                }),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ];
            if quality_select_action(
                base_state,
                &before_candidates,
                2,
                before,
                &no_bias,
                profile,
                seed,
                tick,
            )
            .is_some_and(|choice| choice.tactic == TacticId::StrikeThrow)
            {
                report.guard_counter_before += 1;
            }
            for _ in 0..3 {
                response_model.observe(context, OpponentResponse::Guard);
            }
            let after = response_model.top_three(context);
            let after_candidates = before_candidates.map(|candidate| {
                candidate.map(|mut candidate| {
                    candidate.tactic = tactic_for_action(
                        candidate.action,
                        TacticalPhase::Neutral,
                        after[0].response,
                        false,
                    );
                    candidate
                })
            });
            if quality_select_action(
                base_state,
                &after_candidates,
                2,
                after,
                &no_bias,
                profile,
                seed,
                tick,
            )
            .is_some_and(|choice| choice.tactic == TacticId::StrikeThrow)
            {
                report.guard_counter_after += 1;
            }

            report.behavior_trials[4] += 1;
            report.pressure_trials += 1;
            let pressure_candidates = [
                Some(ForecastAction {
                    action: BotSemanticAction::Dash,
                    tactic: TacticId::EscapePressure,
                    facts: ForecastMoveFacts::fallback(BotSemanticAction::Dash, distance),
                }),
                Some(ForecastAction {
                    action: BotSemanticAction::Jump,
                    tactic: TacticId::EscapePressure,
                    facts: ForecastMoveFacts::fallback(BotSemanticAction::Jump, distance),
                }),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ];
            let pressure_state = CombatForecastState {
                target_action_phase: ForecastActionPhase::Active,
                target_action_ticks_remaining: 1,
                ..base_state
            };
            if quality_select_action(
                pressure_state,
                &pressure_candidates,
                2,
                quality_responses(OpponentResponse::Attack),
                &no_bias,
                profile,
                seed,
                tick,
            )
            .is_some_and(|choice| choice.tactic == TacticId::EscapePressure)
            {
                report.safe_escapes += 1;
            }

            report.behavior_trials[5] += 1;
            let passive_candidates = [
                Some(ForecastAction {
                    action: BotSemanticAction::Light,
                    tactic: TacticId::NeutralPoke,
                    facts: light_facts,
                }),
                Some(ForecastAction {
                    action: BotSemanticAction::Dash,
                    tactic: TacticId::BaitAndPunish,
                    facts: ForecastMoveFacts::fallback(BotSemanticAction::Dash, distance),
                }),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ];
            let _ = quality_select_action(
                base_state,
                &passive_candidates,
                2,
                quality_responses(OpponentResponse::Wait),
                &no_bias,
                profile,
                seed,
                tick,
            );

            report.hit_confirm_trials += 1;
            if authored_follow_ups(
                Some(light_prediction),
                true,
                MAX_STAMINA,
                &FighterCharacter::new(character),
                &style,
                &equipment,
                catalog,
            )
            .iter()
            .any(|follow| follow.action.is_some() && follow.branch == PlanBranch::Hit)
            {
                report.hit_confirm_follow_ups += 1;
            }
        }

        let adaptive_candidates = [
            Some(ForecastAction {
                action: BotSemanticAction::Light,
                tactic: TacticId::BaitAndPunish,
                facts: light_facts,
            }),
            Some(ForecastAction {
                action: BotSemanticAction::Light,
                tactic: TacticId::NeutralPoke,
                facts: light_facts,
            }),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        ];
        let mut biases = [0.0; TacticId::COUNT];
        for trial in 0..40_u64 {
            let selected = quality_select_action(
                base_state,
                &adaptive_candidates,
                2,
                quality_responses(OpponentResponse::Attack),
                &biases,
                profile,
                0xfaded_u64 + character_index as u64,
                trial,
            );
            let bait_selected =
                selected.is_some_and(|choice| choice.tactic == TacticId::BaitAndPunish);
            if trial < 20 {
                report.failed_tactic_first_half += u32::from(bait_selected);
            } else {
                report.failed_tactic_second_half += u32::from(bait_selected);
            }
            if bait_selected {
                update_tactic_bias(
                    &mut biases,
                    TacticId::BaitAndPunish,
                    TacticOutcome {
                        whiffs: 1,
                        damage_swing: -8.0,
                        initiative_result: -1.0,
                        ..Default::default()
                    },
                    profile.tactic_learning_rate,
                    profile.tactic_bias_cap,
                );
            }
        }
        report.failed_tactic_half_trials += 20;
    }
    report
}

fn reason_for_goal(goal: BotGoal, target_edge: f32, punish_need: f32) -> BotDecisionReason {
    match goal {
        BotGoal::Survive => BotDecisionReason::LowHealth,
        BotGoal::RegainStamina => BotDecisionReason::LowStamina,
        BotGoal::Approach | BotGoal::Reposition | BotGoal::Disengage => BotDecisionReason::Spacing,
        BotGoal::Pressure if target_edge > 0.0 => BotDecisionReason::EdgePressure,
        BotGoal::Pressure => BotDecisionReason::Spacing,
        BotGoal::Punish if punish_need > 0.0 => BotDecisionReason::Vulnerability,
        BotGoal::Punish => BotDecisionReason::Spacing,
        BotGoal::CollectItem => BotDecisionReason::ItemOpportunity,
        BotGoal::UseItem => BotDecisionReason::HeldItem,
        BotGoal::Idle => BotDecisionReason::None,
    }
}

fn refresh_trace_state(slot: &mut BotRuntimeSlot) {
    slot.trace.target_id = slot.target_id;
    slot.trace.phase = slot.tactical_phase;
    if let Some(plan) = slot.active_plan {
        slot.trace.tactic = Some(plan.tactic);
        slot.trace.forecast_score = plan.forecast_score;
        slot.trace.plan_step = plan.current_step;
        slot.trace.branch = plan.branch;
        slot.trace.learned_bias = plan.learned_bias;
    }
    if let Some(target_id) = slot.target_id {
        let memory = &slot.opponents[target_id];
        slot.trace.reaction_gated = memory.pending_action.is_some();
        slot.trace.reaction_delay_ticks = if slot.trace.reaction_gated {
            memory
                .pending_delay
                .saturating_sub(slot.decision_tick.saturating_sub(memory.pending_since_tick))
                .min(u8::MAX as u64) as u8
        } else {
            0
        };
    } else {
        slot.trace.reaction_gated = false;
        slot.trace.reaction_delay_ticks = 0;
    }
    slot.trace.action = slot.commitment.map(|commitment| commitment.action);
    slot.trace.commitment = slot
        .commitment
        .map_or(BotCommitmentTrace::None, |commitment| {
            if commitment.accepted {
                BotCommitmentTrace::Accepted
            } else {
                BotCommitmentTrace::Waiting
            }
        });
}

fn consider_action(
    best: &mut Option<(BotSemanticAction, f32)>,
    action: BotSemanticAction,
    score: f32,
) {
    if best.is_none_or(|(_, best_score)| score > best_score) {
        *best = Some((action, score));
    }
}

fn neutral_action_available(action: FighterAction) -> bool {
    matches!(action, FighterAction::Idle | FighterAction::Moving)
}

fn vertically_stacked(bot_y: f32, target_y: f32) -> bool {
    (target_y - bot_y).abs() > 0.75
}

fn tactical_dash_hold_expired(action: FighterAction, elapsed: f32, has_commitment: bool) -> bool {
    action == FighterAction::Dashing
        && !has_commitment
        && elapsed >= f32::from(DASH_FORECAST_TICKS) * DECISION_STEP
}

fn update_commitment(slot: &mut BotRuntimeSlot, state: &FighterActionState) {
    let Some(mut commitment) = slot.commitment else {
        return;
    };
    let matches_expected = commitment.expected_action.map_or_else(
        || action_matches(commitment.action, state.action),
        |expected| state.action == expected,
    );
    if matches_expected {
        if matches!(
            commitment.action,
            BotSemanticAction::Dash | BotSemanticAction::Jump
        ) {
            slot.commitment = None;
            return;
        }
        commitment.accepted = true;
    }
    let completed = commitment.accepted
        && !matches_expected
        && matches!(state.action, FighterAction::Idle | FighterAction::Moving);
    let rejected = !commitment.accepted && slot.decision_tick >= commitment.expires_tick;
    if rejected
        || completed
        || (commitment.accepted && (state.cancel_window_open || state.branch_window_open))
    {
        if rejected {
            slot.commitment_rejected = true;
        }
        slot.commitment = None;
    } else {
        slot.commitment = Some(commitment);
    }
}

fn action_matches(action: BotSemanticAction, state: FighterAction) -> bool {
    match action {
        BotSemanticAction::Light => matches!(
            state,
            FighterAction::LightAttack1
                | FighterAction::LightAttack2
                | FighterAction::JumpAttack
                | FighterAction::DashAttack
        ),
        BotSemanticAction::Heavy => matches!(
            state,
            FighterAction::HeavyAttack
                | FighterAction::HeavyAttack2
                | FighterAction::JumpHeavyAttack
        ),
        BotSemanticAction::Grab => {
            matches!(state, FighterAction::GrabStartup | FighterAction::GrabHold)
        }
        BotSemanticAction::Jump => state == FighterAction::Jumping,
        BotSemanticAction::Dash => {
            matches!(state, FighterAction::Dashing | FighterAction::DashAttack)
        }
        BotSemanticAction::Pickup => state == FighterAction::ItemPickup,
        BotSemanticAction::ItemLight => state == FighterAction::ItemSwing,
        BotSemanticAction::ItemHeavy => state == FighterAction::ItemThrow,
        BotSemanticAction::SpecialProjectile
        | BotSemanticAction::SpecialTrap
        | BotSemanticAction::SpecialHazard
        | BotSemanticAction::SpecialShockwave => state == FighterAction::SpecialCast,
    }
}

fn actuate(slot: &mut BotRuntimeSlot, input: &mut FighterInput) {
    input.movement = slot.intent.movement;
    input.guard = slot.intent.guard;
    let Some(mut commitment) = slot.commitment else {
        return;
    };
    if commitment.accepted || commitment.last_press_tick == Some(slot.decision_tick) {
        return;
    }
    match commitment.action {
        BotSemanticAction::Light | BotSemanticAction::ItemLight => input.light = true,
        BotSemanticAction::Heavy | BotSemanticAction::ItemHeavy => input.heavy = true,
        BotSemanticAction::Grab | BotSemanticAction::Pickup => input.grab = true,
        BotSemanticAction::Jump => input.jump = true,
        BotSemanticAction::Dash => input.dash = true,
        BotSemanticAction::SpecialProjectile => input.special = true,
        BotSemanticAction::SpecialTrap => {
            input.special = true;
            input.grab = true;
        }
        BotSemanticAction::SpecialHazard => {
            input.special = true;
            input.guard = true;
        }
        BotSemanticAction::SpecialShockwave => {
            input.special = true;
            input.heavy = true;
        }
    }
    commitment.last_press_tick = Some(slot.decision_tick);
    slot.commitment = Some(commitment);
    match commitment.action {
        BotSemanticAction::Dash => slot.dash_ready_tick = slot.decision_tick + 30,
        BotSemanticAction::Light | BotSemanticAction::ItemLight => {
            slot.attack_ready_tick = slot.decision_tick + 14
        }
        BotSemanticAction::Heavy | BotSemanticAction::ItemHeavy => {
            slot.attack_ready_tick = slot.decision_tick + 20
        }
        BotSemanticAction::Grab | BotSemanticAction::Pickup => {
            slot.attack_ready_tick = slot.decision_tick + 22
        }
        BotSemanticAction::Jump => slot.attack_ready_tick = slot.decision_tick + 10,
        BotSemanticAction::SpecialProjectile
        | BotSemanticAction::SpecialTrap
        | BotSemanticAction::SpecialHazard
        | BotSemanticAction::SpecialShockwave => slot.attack_ready_tick = slot.decision_tick + 28,
    }
}

fn sync_legacy_brain(brain: &mut BotBrain, slot: &BotRuntimeSlot) {
    brain.decision_timer = TickTimer::from_ticks(u32::from(
        SIM_TICKS_PER_DECISION.saturating_sub(slot.decision_subticks),
    ));
    brain.attack_timer = TickTimer::from_ticks(
        slot.attack_ready_tick
            .saturating_sub(slot.decision_tick)
            .saturating_mul(u64::from(SIM_TICKS_PER_DECISION))
            .min(u64::from(u32::MAX)) as u32,
    );
    brain.dash_timer = TickTimer::from_ticks(
        slot.dash_ready_tick
            .saturating_sub(slot.decision_tick)
            .saturating_mul(u64::from(SIM_TICKS_PER_DECISION))
            .min(u64::from(u32::MAX)) as u32,
    );
    brain.movement_plan_timer = TickTimer::from_ticks(
        slot.commitment
            .map_or(0, |commitment| {
                commitment.expires_tick.saturating_sub(slot.decision_tick)
            })
            .saturating_mul(u64::from(SIM_TICKS_PER_DECISION))
            .min(u64::from(u32::MAX)) as u32,
    );
    brain.strafe_sign = slot.strafe_sign;
    brain.movement_plan = match slot.intent.goal {
        BotGoal::Approach | BotGoal::CollectItem | BotGoal::Punish => BotMovementPlan::Approach,
        BotGoal::Pressure | BotGoal::UseItem => BotMovementPlan::Pressure,
        BotGoal::Survive | BotGoal::RegainStamina | BotGoal::Disengage => BotMovementPlan::Retreat,
        BotGoal::Idle | BotGoal::Reposition => BotMovementPlan::Circle,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct TapeFrame {
        target: Option<usize>,
        goal: BotGoal,
        action: Option<BotSemanticAction>,
        reaction_gated: bool,
        reaction_delay: u8,
        commitment: BotCommitmentTrace,
        buttons: u8,
        movement_x: i16,
        movement_z: i16,
        utility: i16,
    }

    #[test]
    fn decision_clock_is_immediate_then_exactly_twenty_hertz() {
        assert_eq!(SIM_HZ_U32, 60);
        assert_eq!(BOT_DECISION_HZ, 20);
        let mut slot = BotRuntimeSlot::default();
        let mut advancing_sim_ticks = Vec::new();
        for sim_tick in 1..=10 {
            if advance_decision_clock(&mut slot, sim_tick == 1) {
                advancing_sim_ticks.push(sim_tick);
            }
        }
        assert_eq!(advancing_sim_ticks, [1, 4, 7, 10]);
        assert_eq!(slot.decision_tick, 4);
        assert_eq!(slot.decision_subticks, 0);
    }

    #[test]
    fn zero_cost_attacks_ignore_the_stamina_reserve_and_uniform_priors_do_not_trigger_bait() {
        assert!(stamina_reserve_allows(0.0, 0.0, 0.15));
        assert!(stamina_reserve_allows(30.0, 10.0, 0.15));
        assert!(!stamina_reserve_allows(15.0, 10.0, 0.15));

        assert!(!response_is_actionable(
            ResponsePrediction {
                response: OpponentResponse::Attack,
                probability: 0.125,
            },
            OpponentResponse::Attack,
        ));
        assert!(response_is_actionable(
            ResponsePrediction {
                response: OpponentResponse::Attack,
                probability: ACTIONABLE_RESPONSE_PROBABILITY,
            },
            OpponentResponse::Attack,
        ));
    }

    #[test]
    fn tactical_dash_releases_at_its_forecast_deadline() {
        let deadline = f32::from(DASH_FORECAST_TICKS) * DECISION_STEP;
        assert!(!tactical_dash_hold_expired(
            FighterAction::Dashing,
            deadline - 0.001,
            false,
        ));
        assert!(!tactical_dash_hold_expired(
            FighterAction::Dashing,
            deadline,
            true,
        ));
        assert!(!tactical_dash_hold_expired(
            FighterAction::Moving,
            deadline,
            false,
        ));
        assert!(tactical_dash_hold_expired(
            FighterAction::Dashing,
            deadline,
            false,
        ));
    }

    #[test]
    fn fighter_height_mismatch_uses_lateral_reposition_threshold() {
        assert!(!vertically_stacked(1.0, 1.75));
        assert!(vertically_stacked(1.0, 1.751));
        assert!(vertically_stacked(2.0, 1.0));
    }

    #[test]
    fn dashing_technique_options_use_each_characters_authored_dash_slots() {
        let catalog = CharacterMoveCatalog::default();
        let motor = FighterMotor::default();
        let stats = FighterStats::default();
        let action = FighterActionState {
            action: FighterAction::Dashing,
            ..Default::default()
        };
        let style = FighterStyle {
            kind: crate::styles::FighterStyleKind::Catalyst,
        };
        let equipment = FighterEquipment {
            kind: crate::equipment::EquipmentKind::CounterCell,
            cooldown: TickTimer::ZERO,
        };

        for character_kind in CHARACTER_KINDS {
            let character = FighterCharacter::new(character_kind);
            let loadout = LoadoutContext::for_character(character_kind, style.kind, equipment.kind);
            let options = technique_options(
                &character, &style, &equipment, &motor, &stats, &action, &catalog,
            );
            assert_eq!(
                options.light.map(|prediction| prediction.id),
                technique_slot_for_loadout(CharacterMoveSlot::DashLight, loadout, &catalog)
                    .map(|definition| definition.id),
                "{character_kind:?} light dash slot",
            );
            assert_eq!(
                options.heavy.map(|prediction| prediction.id),
                technique_slot_for_loadout(CharacterMoveSlot::DashHeavy, loadout, &catalog)
                    .map(|definition| definition.id),
                "{character_kind:?} heavy dash slot",
            );
        }
    }

    fn intent_tape(seed: u64, randomized: bool) -> Vec<TapeFrame> {
        let catalog = BotProfileCatalog::default();
        let mut profile = catalog.profile(BotProfileId::Standard);
        profile.weights.survive = 0.0;
        profile.weights.regain_stamina = 0.0;
        profile.weights.reposition = 0.0;
        profile.weights.approach = 10.0;
        profile.weights.pressure = 0.0;
        profile.weights.punish = 0.0;
        profile.weights.disengage = 0.0;
        profile.weights.collect_item = 0.0;
        profile.weights.use_item = 0.0;
        profile.weights.objective = 0.0;
        profile.perception_error_m = if randomized { 0.3 } else { 0.0 };
        profile.utility_jitter = if randomized { 0.15 } else { 0.0 };
        profile.intentional_mistake_rate = 0.0;
        profile.reaction_ticks_min = 3;
        profile.reaction_ticks_max = if randomized { 5 } else { 3 };
        profile.commitment_ticks_min = 2;
        profile.commitment_ticks_max = if randomized { 4 } else { 2 };
        profile.tactical_planning_enabled = false;

        let mut targetable_by = [false; FIGHTER_COUNT];
        targetable_by[0] = true;
        let snapshot = BotSnapshotBuffer {
            replay_seed: seed,
            hazard_elapsed: ElapsedTicks::ZERO,
            fighters: [
                Some(FighterSnapshot {
                    id: 0,
                    position: Vec3::ZERO,
                    facing: Vec3::X,
                    action: FighterAction::Idle,
                    action_elapsed: 0.0,
                    grounded: true,
                    health: 100.0,
                    stamina: MAX_STAMINA,
                    targetable_by: [false; FIGHTER_COUNT],
                    ..Default::default()
                }),
                Some(FighterSnapshot {
                    id: 1,
                    position: Vec3::new(4.0, 0.0, 0.0),
                    facing: -Vec3::X,
                    action: FighterAction::HeavyAttack,
                    action_elapsed: 0.25,
                    grounded: true,
                    health: 65.0,
                    stamina: MAX_STAMINA * 0.5,
                    targetable_by,
                    ..Default::default()
                }),
                None,
                None,
            ],
            items: Vec::new(),
            specials: Vec::new(),
            navigation_blockers: NavigationBlockers::default(),
            ..Default::default()
        };
        let mut slot = BotRuntimeSlot::default();
        slot.initialized = true;
        slot.profile = Some(profile);
        let mut motor = FighterMotor::default();
        motor.grounded = true;
        motor.facing = Vec3::X;
        let stats = FighterStats {
            health: 100.0,
            stamina: MAX_STAMINA,
            ..Default::default()
        };
        let special_state = FighterSpecialState::default();
        let style = FighterStyle {
            kind: crate::styles::FighterStyleKind::Vector,
        };
        let equipment = FighterEquipment {
            kind: crate::equipment::EquipmentKind::DashCoil,
            cooldown: TickTimer::ZERO,
        };
        let action_state = FighterActionState::default();
        let character = FighterCharacter::new(CharacterKind::Cat);
        let move_catalog = CharacterMoveCatalog::default();
        let mut navigation = BotNavigationCache::default();
        let doors = SplitCausewayDoorState::default();
        let mut tape = Vec::new();

        for tick in 1..=6 {
            slot.decision_tick = tick;
            update_commitment(&mut slot, &action_state);
            update_opponent_memory(&mut slot, &snapshot, 0, 1, seed, profile);
            slot.target_id = choose_target(&snapshot, 0, Vec3::ZERO, &slot, profile, seed);
            plan_bot(
                crate::arena_defs::TRAINING_GROUND_ARENA_INDEX,
                0,
                Vec3::ZERO,
                &motor,
                &special_state,
                &character,
                &style,
                &equipment,
                &stats,
                &action_state,
                BotDifficulty::Standard,
                None,
                false,
                &BotTechniqueOptions::default(),
                &move_catalog,
                &snapshot,
                profile,
                seed,
                &mut slot,
                &mut navigation,
                &doors,
            );
            let mut input = FighterInput::default();
            actuate(&mut slot, &mut input);
            let buttons = u8::from(input.dash)
                | (u8::from(input.jump) << 1)
                | (u8::from(input.light) << 2)
                | (u8::from(input.heavy) << 3)
                | (u8::from(input.grab) << 4)
                | (u8::from(input.special) << 5);
            tape.push(TapeFrame {
                target: slot.trace.target_id,
                goal: slot.trace.goal,
                action: slot.trace.action,
                reaction_gated: slot.trace.reaction_gated,
                reaction_delay: slot.trace.reaction_delay_ticks,
                commitment: slot.trace.commitment,
                buttons,
                movement_x: (input.movement.x * 1_000.0).round() as i16,
                movement_z: (input.movement.y * 1_000.0).round() as i16,
                utility: (slot.trace.utility_score * 100.0).round() as i16,
            });
        }
        tape
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct TacticalTapeFrame {
        target: Option<usize>,
        phase: TacticalPhase,
        tactic: Option<TacticId>,
        action: Option<BotSemanticAction>,
        branch: PlanBranch,
        buttons: u8,
        movement_x: i16,
        movement_z: i16,
    }

    fn tactical_intent_tape(seed: u64) -> Vec<TacticalTapeFrame> {
        let profile_catalog = BotProfileCatalog::default();
        let mut profile = profile_catalog.profile(BotProfileId::Standard);
        profile.weights.survive = 0.0;
        profile.weights.regain_stamina = 0.0;
        profile.weights.reposition = 0.0;
        profile.weights.approach = 0.0;
        profile.weights.pressure = 3.5;
        profile.weights.punish = 0.0;
        profile.weights.disengage = 0.0;
        profile.weights.collect_item = 0.0;
        profile.weights.use_item = 0.0;
        profile.weights.objective = 0.0;
        profile.perception_error_m = 0.0;
        profile.intentional_mistake_rate = 0.0;
        profile.reaction_ticks_min = 3;
        profile.reaction_ticks_max = 3;

        let mut targetable_by = [false; FIGHTER_COUNT];
        targetable_by[0] = true;
        let snapshot = BotSnapshotBuffer {
            replay_seed: seed,
            fighters: [
                Some(FighterSnapshot {
                    id: 0,
                    position: Vec3::ZERO,
                    facing: Vec3::X,
                    character: CharacterKind::Cat,
                    targetable_by: [false; FIGHTER_COUNT],
                    ..Default::default()
                }),
                Some(FighterSnapshot {
                    id: 1,
                    position: Vec3::new(0.8, 0.0, 0.0),
                    facing: -Vec3::X,
                    character: CharacterKind::Pig,
                    targetable_by,
                    ..Default::default()
                }),
                None,
                None,
            ],
            ..Default::default()
        };
        let mut slot = BotRuntimeSlot::default();
        slot.initialized = true;
        slot.profile = Some(profile);
        let mut motor = FighterMotor::default();
        motor.grounded = true;
        motor.facing = Vec3::X;
        let stats = FighterStats::default();
        let action = FighterActionState::default();
        let character = FighterCharacter::new(CharacterKind::Cat);
        let style = FighterStyle {
            kind: crate::styles::FighterStyleKind::Catalyst,
        };
        let equipment = FighterEquipment {
            kind: crate::equipment::EquipmentKind::CounterCell,
            cooldown: TickTimer::ZERO,
        };
        let special = FighterSpecialState::default();
        let catalog = CharacterMoveCatalog::default();
        let techniques = technique_options(
            &character, &style, &equipment, &motor, &stats, &action, &catalog,
        );
        let mut navigation = BotNavigationCache::default();
        let doors = SplitCausewayDoorState::default();
        let mut tape = Vec::new();

        for tick in 1..=10 {
            slot.decision_tick = tick;
            update_commitment(&mut slot, &action);
            update_opponent_memory(&mut slot, &snapshot, 0, 1, seed, profile);
            slot.target_id = choose_target(&snapshot, 0, Vec3::ZERO, &slot, profile, seed);
            plan_bot(
                crate::arena_defs::TRAINING_GROUND_ARENA_INDEX,
                0,
                Vec3::ZERO,
                &motor,
                &special,
                &character,
                &style,
                &equipment,
                &stats,
                &action,
                BotDifficulty::Standard,
                None,
                false,
                &techniques,
                &catalog,
                &snapshot,
                profile,
                seed,
                &mut slot,
                &mut navigation,
                &doors,
            );
            let mut input = FighterInput::default();
            actuate(&mut slot, &mut input);
            tape.push(TacticalTapeFrame {
                target: slot.trace.target_id,
                phase: slot.trace.phase,
                tactic: slot.trace.tactic,
                action: slot.trace.action,
                branch: slot.trace.branch,
                buttons: u8::from(input.dash)
                    | (u8::from(input.jump) << 1)
                    | (u8::from(input.light) << 2)
                    | (u8::from(input.heavy) << 3)
                    | (u8::from(input.grab) << 4)
                    | (u8::from(input.special) << 5),
                movement_x: (input.movement.x * 1_000.0).round() as i16,
                movement_z: (input.movement.y * 1_000.0).round() as i16,
            });
        }
        tape
    }

    #[test]
    fn deterministic_intent_tape_repeats_and_other_seeds_stay_legal() {
        let first = intent_tape(0x1234_5678, true);
        assert_eq!(first, intent_tape(0x1234_5678, true));
        let alternate = intent_tape(0x8765_4321, true);
        assert_ne!(first, alternate);
        assert!(alternate.iter().all(|frame| {
            frame.target == Some(1)
                && frame.goal == BotGoal::Approach
                && matches!(frame.action, None | Some(BotSemanticAction::Dash))
                && frame.buttons & !1 == 0
                && i32::from(frame.movement_x).pow(2) + i32::from(frame.movement_z).pow(2)
                    <= 1_001_i32.pow(2)
        }));

        let golden = intent_tape(0x1234_5678, false)
            .into_iter()
            .map(|frame| {
                (
                    frame.target,
                    frame.goal,
                    frame.action,
                    frame.reaction_gated,
                    frame.reaction_delay,
                    frame.commitment,
                    frame.buttons,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            golden,
            vec![
                (
                    Some(1),
                    BotGoal::Approach,
                    Some(BotSemanticAction::Dash),
                    true,
                    3,
                    BotCommitmentTrace::Waiting,
                    1
                ),
                (
                    Some(1),
                    BotGoal::Approach,
                    Some(BotSemanticAction::Dash),
                    true,
                    2,
                    BotCommitmentTrace::Waiting,
                    1
                ),
                (
                    Some(1),
                    BotGoal::Approach,
                    None,
                    true,
                    1,
                    BotCommitmentTrace::None,
                    0
                ),
                (
                    Some(1),
                    BotGoal::Approach,
                    None,
                    false,
                    0,
                    BotCommitmentTrace::None,
                    0
                ),
                (
                    Some(1),
                    BotGoal::Approach,
                    None,
                    false,
                    0,
                    BotCommitmentTrace::None,
                    0
                ),
                (
                    Some(1),
                    BotGoal::Approach,
                    None,
                    false,
                    0,
                    BotCommitmentTrace::None,
                    0
                ),
            ]
        );
    }

    #[test]
    fn tactical_seed_repeats_the_same_semantic_input_trace() {
        let first = tactical_intent_tape(0x1234_5678);
        assert_eq!(first, tactical_intent_tape(0x1234_5678));
        assert!(first.iter().all(|frame| frame.target == Some(1)));
        assert!(first.iter().any(|frame| frame.tactic.is_some()));
        assert!(first.iter().any(|frame| frame.buttons != 0));
    }

    #[test]
    fn near_optimal_ties_are_stable_and_seeded_selection_repeats() {
        let mut scores = [None; MAX_TACTICAL_ACTIONS];
        scores[0] = Some(1.0);
        scores[1] = Some(1.0);
        assert_eq!(
            select_near_optimal_index(&scores, 2, 0.25, 0.0, 7, 0, 10),
            Some((0, 1.0))
        );
        let first = select_near_optimal_index(&scores, 2, 0.25, 0.1, 91, 2, 33);
        assert_eq!(
            first,
            select_near_optimal_index(&scores, 2, 0.25, 0.1, 91, 2, 33)
        );
    }

    #[test]
    fn perceived_responses_update_only_after_the_reaction_delay() {
        let mut targetable_by = [false; FIGHTER_COUNT];
        targetable_by[0] = true;
        let mut snapshot = BotSnapshotBuffer {
            replay_seed: 41,
            fighters: [
                Some(FighterSnapshot {
                    id: 0,
                    position: Vec3::ZERO,
                    targetable_by: [false; FIGHTER_COUNT],
                    ..Default::default()
                }),
                Some(FighterSnapshot {
                    id: 1,
                    position: Vec3::X,
                    action: FighterAction::Guarding,
                    action_elapsed: 0.4,
                    targetable_by,
                    ..Default::default()
                }),
                None,
                None,
            ],
            ..Default::default()
        };
        let catalog = BotProfileCatalog::default();
        let mut profile = catalog.profile(BotProfileId::Standard);
        profile.reaction_ticks_min = 3;
        profile.reaction_ticks_max = 3;
        let mut slot = BotRuntimeSlot::default();
        slot.last_move_envelope = 1.5;
        let context = response_context(
            1.0,
            slot.last_move_envelope,
            FighterAction::Idle,
            snapshot_edge_danger(&snapshot, Vec3::X),
            PreviousOutcome::None,
        );

        for tick in 1..=3 {
            slot.decision_tick = tick;
            update_opponent_memory(&mut slot, &snapshot, 0, 1, 41, profile);
            assert_eq!(slot.opponents[1].perceived_action, None);
            assert_eq!(
                slot.opponents[1]
                    .responses
                    .count(context, OpponentResponse::Guard),
                2.0
            );
        }
        slot.decision_tick = 4;
        update_opponent_memory(&mut slot, &snapshot, 0, 1, 41, profile);
        assert_eq!(
            slot.opponents[1].perceived_action,
            Some(FighterAction::Guarding)
        );
        assert_eq!(
            slot.opponents[1]
                .responses
                .count(context, OpponentResponse::Guard),
            3.0
        );
        assert!((slot.opponents[1].perceived_action_elapsed - 0.55).abs() < 0.001);

        let target = snapshot.fighters[1].as_mut().expect("target fixture");
        target.action = FighterAction::Idle;
        target.action_elapsed = 0.0;
        slot.decision_tick = 5;
        update_opponent_memory(&mut slot, &snapshot, 0, 1, 41, profile);
        assert_eq!(
            slot.opponents[1].perceived_action,
            Some(FighterAction::Guarding)
        );
        assert!((slot.opponents[1].perceived_action_elapsed - 0.60).abs() < 0.001);
    }

    #[test]
    fn response_learning_excludes_forced_states_and_automatic_idle_returns() {
        let position = Vec3::ZERO;
        assert_eq!(
            observed_response_transition(
                FighterAction::Idle,
                FighterAction::Guarding,
                position,
                position,
                Vec2::ZERO,
            ),
            Some(OpponentResponse::Guard)
        );
        assert_eq!(
            observed_response_transition(
                FighterAction::Guarding,
                FighterAction::Hitstun,
                position,
                position,
                Vec2::ZERO,
            ),
            None
        );
        assert_eq!(
            observed_response_transition(
                FighterAction::Guarding,
                FighterAction::Idle,
                position,
                position,
                Vec2::ZERO,
            ),
            None
        );
        assert_eq!(
            observed_response_transition(
                FighterAction::Moving,
                FighterAction::Idle,
                position,
                position,
                Vec2::ZERO,
            ),
            Some(OpponentResponse::Wait)
        );
    }

    #[test]
    fn branch_transition_selects_only_the_matching_fixed_follow_up() {
        let mut plan = ActiveTacticPlan {
            target_id: 1,
            tactic: TacticId::StrikeThrow,
            expected_response: OpponentResponse::Guard,
            steps: [
                PlanStep::action(BotSemanticAction::Light, None, PlanBranch::Start, 8),
                PlanStep::action(BotSemanticAction::Grab, None, PlanBranch::Guarded, 7),
                PlanStep::action(BotSemanticAction::Heavy, None, PlanBranch::Hit, 9),
            ],
            current_step: 0,
            branch: PlanBranch::Pending,
            deadline_tick: 8,
            step_started_tick: 0,
            step_committed: true,
            last_outcome_tick: 0,
            rejection_count: 0,
            origin: Vec3::ZERO,
            target_origin: Vec3::X,
            start_bot_health: 100.0,
            start_target_health: 100.0,
            start_bot_stamina: MAX_STAMINA,
            start_target_stamina: MAX_STAMINA,
            accumulated: TacticOutcome::default(),
            forecast_score: 1.0,
            learned_bias: 0.0,
        };
        assert!(advance_plan_branch(&mut plan, PlanBranch::Guarded, 20));
        assert_eq!(plan.current_step, 1);
        assert_eq!(plan.deadline_tick, 27);
        assert_eq!(plan.accumulated.blocks, 1);
        assert!(!plan.step_committed);
    }

    #[test]
    fn tactics_quality_fixture_covers_every_character_and_required_rates() {
        let report = run_tactics_quality_fixture(
            &CharacterMoveCatalog::default(),
            &BotProfileCatalog::default(),
        );
        assert!(report.passed(), "{report:?}");
        assert!(report.behavior_trials.into_iter().all(|trials| trials > 0));
    }

    #[test]
    fn named_counter_samples_are_repeatable_and_independent() {
        let first: Vec<_> = (0..16)
            .map(|tick| bot_sample(91, 2, tick, RandomStream::Action, 3))
            .collect();
        let repeated: Vec<_> = (0..16)
            .map(|tick| bot_sample(91, 2, tick, RandomStream::Action, 3))
            .collect();
        let other: Vec<_> = (0..16)
            .map(|tick| bot_sample(91, 2, tick, RandomStream::Goal, 3))
            .collect();
        assert_eq!(first, repeated);
        assert_ne!(first, other);
        assert!(first.iter().all(|sample| (0.0..1.0).contains(sample)));
    }

    #[test]
    fn opponent_history_is_bounded_to_eight_seconds() {
        let mut memory = BotOpponentMemory::default();
        for _ in 0..MEMORY_TICKS * 3 {
            memory.push(OBS_ATTACK | OBS_GUARD);
        }
        assert_eq!(memory.len, MEMORY_TICKS);
        assert_eq!(memory.totals[0], MEMORY_TICKS as u16);
        assert_eq!(memory.totals[1], MEMORY_TICKS as u16);
    }

    #[test]
    fn commitment_emits_at_most_once_per_decision_tick() {
        let mut slot = BotRuntimeSlot::default();
        slot.decision_tick = 7;
        slot.commitment = Some(BotCommitment {
            action: BotSemanticAction::Light,
            expected_action: Some(FighterAction::LightAttack1),
            expires_tick: 10,
            accepted: false,
            last_press_tick: None,
        });
        let mut first = FighterInput::default();
        actuate(&mut slot, &mut first);
        assert!(first.light);
        let mut repeated = FighterInput::default();
        actuate(&mut slot, &mut repeated);
        assert!(!repeated.light);
        slot.decision_tick += 1;
        let mut retry = FighterInput::default();
        actuate(&mut slot, &mut retry);
        assert!(retry.light);
    }

    #[test]
    fn spawned_skill_only_neutral_techniques_receive_combat_windows() {
        let catalog = CharacterMoveCatalog::default();
        let cases = [
            (
                crate::characters::CharacterKind::Bee,
                crate::techniques::TechniqueId::BeeLight1,
                BotSemanticAction::Light,
            ),
            (
                crate::characters::CharacterKind::Bee,
                crate::techniques::TechniqueId::BeeHeavy2,
                BotSemanticAction::Heavy,
            ),
            (
                crate::characters::CharacterKind::Penguin,
                crate::techniques::TechniqueId::PenguinLight1,
                BotSemanticAction::Light,
            ),
            (
                crate::characters::CharacterKind::Penguin,
                crate::techniques::TechniqueId::PenguinHeavy,
                BotSemanticAction::Heavy,
            ),
        ];

        for (character, technique, action) in cases {
            let loadout = LoadoutContext::for_character(
                character,
                crate::styles::FighterStyleKind::Catalyst,
                crate::equipment::EquipmentKind::CounterCell,
            );
            let prediction = crate::techniques::technique_prediction_for_loadout_id_in_catalog(
                technique, loadout, &catalog,
            )
            .expect("fixture technique should resolve through its character catalog");
            assert!(!prediction.has_direct_attack);
            assert!(prediction.has_spawned_skill);

            let reliable_range = spawned_skill_reliable_range(prediction);
            let sample_distance = reliable_range * 0.9;
            let expected_contact_ms = prediction
                .spawned_skill_contact_ms(sample_distance)
                .expect("a distance inside the reliable window must be authored contact");
            let window = technique_contact_window(prediction, action, sample_distance)
                .expect("a spawned skill is an authored offensive contact path");
            assert_eq!(window.envelope, reliable_range);
            assert_eq!(window.startup_seconds, expected_contact_ms as f32 / 1_000.0);
            assert_eq!(
                window.vertical_tolerance,
                prediction.spawned_skill_vertical_tolerance
            );
            assert_eq!(
                window.facing_dot,
                prediction
                    .spawned_skill_facing_cone_dot
                    .unwrap_or(SPAWNED_SKILL_FALLBACK_FACING_DOT)
            );
            assert!(reliable_range <= prediction.spawned_skill_range);
            assert!(
                technique_contact_window(prediction, action, reliable_range + 0.01).is_none(),
                "skill-only offense should not commit beyond its reliable travel window"
            );
        }
    }

    #[test]
    fn fast_move_wins_neutral_while_heavy_and_grab_require_context() {
        let catalog = CharacterMoveCatalog::default();
        let loadout = LoadoutContext::for_character(
            crate::characters::CharacterKind::Bee,
            crate::styles::FighterStyleKind::Catalyst,
            crate::equipment::EquipmentKind::CounterCell,
        );
        let prediction = |technique| {
            crate::techniques::technique_prediction_for_loadout_id_in_catalog(
                technique, loadout, &catalog,
            )
            .expect("fixture technique should resolve")
        };
        let light = prediction(crate::techniques::TechniqueId::BeeLight1);
        let heavy = prediction(crate::techniques::TechniqueId::BeeHeavy2);
        let distance = 2.0;
        let light_contact = technique_contact_window(light, BotSemanticAction::Light, distance)
            .expect("light should reach neutral test spacing")
            .startup_seconds;
        let heavy_contact = technique_contact_window(heavy, BotSemanticAction::Heavy, distance)
            .expect("heavy should reach neutral test spacing")
            .startup_seconds;
        let evaluation = |contact_seconds| MoveEvaluation {
            predicted_position: Vec3::X * distance,
            score: 1.8,
            contact_seconds,
        };

        let neutral_light = offensive_candidate_score(
            light,
            BotSemanticAction::Light,
            evaluation(light_contact),
            0.0,
            0.25,
        );
        let neutral_heavy = offensive_candidate_score(
            heavy,
            BotSemanticAction::Heavy,
            evaluation(heavy_contact),
            0.0,
            0.0,
        );
        assert!(neutral_light > neutral_heavy);
        assert!(neutral_light > grab_candidate_score(0.0, 0.0));

        let punish_light = offensive_candidate_score(
            light,
            BotSemanticAction::Light,
            evaluation(light_contact),
            1.0,
            0.25,
        );
        let punish_heavy = offensive_candidate_score(
            heavy,
            BotSemanticAction::Heavy,
            evaluation(heavy_contact),
            1.0,
            0.0,
        );
        assert!(punish_heavy > punish_light);
        assert!(grab_candidate_score(1.0, 0.0) > neutral_light);
    }

    #[test]
    fn opponent_velocity_estimate_is_deterministic_and_resets_after_teleport() {
        let mut first = BotOpponentMemory::default();
        let mut repeated = BotOpponentMemory::default();
        for (tick, position) in [
            (1, Vec3::ZERO),
            (2, Vec3::new(0.5, 0.0, 0.0)),
            (3, Vec3::new(1.0, 0.0, 0.2)),
        ] {
            first.observe_position(position, tick);
            repeated.observe_position(position, tick);
        }
        assert_eq!(first.velocity, repeated.velocity);
        assert!(first.velocity.x > 0.0);
        first.observe_position(Vec3::new(20.0, 0.0, 0.0), 4);
        assert_eq!(first.velocity, Vec2::ZERO);
    }

    #[test]
    fn contact_evaluation_leads_motion_and_requires_facing() {
        let window = MoveContactWindow {
            startup_seconds: 0.2,
            envelope: 2.0,
            vertical_tolerance: 0.8,
            facing_dot: 0.25,
        };
        let evaluation = evaluate_contact_window(
            window,
            Vec3::ZERO,
            Vec3::X,
            Vec3::new(1.4, 0.0, 0.0),
            Vec2::new(0.0, 2.0),
            1.0,
        )
        .expect("crossing target remains in the authored contact envelope");
        assert!(evaluation.predicted_position.z > 0.0);
        assert!(
            evaluate_contact_window(
                window,
                Vec3::ZERO,
                -Vec3::X,
                Vec3::new(1.4, 0.0, 0.0),
                Vec2::ZERO,
                1.0,
            )
            .is_none()
        );
    }

    #[test]
    fn accepted_commitment_survives_acceptance_deadline_until_action_completes() {
        let mut slot = BotRuntimeSlot::default();
        slot.decision_tick = 5;
        slot.commitment = Some(BotCommitment {
            action: BotSemanticAction::Light,
            expected_action: Some(FighterAction::LightAttack1),
            expires_tick: 5,
            accepted: false,
            last_press_tick: Some(4),
        });
        let mut active = FighterActionState::default();
        active.action = FighterAction::LightAttack1;
        update_commitment(&mut slot, &active);
        assert!(
            slot.commitment
                .is_some_and(|commitment| commitment.accepted)
        );
        active.action = FighterAction::Idle;
        slot.decision_tick += 1;
        update_commitment(&mut slot, &active);
        assert!(slot.commitment.is_none());
    }

    #[test]
    fn unaccepted_offensive_commitment_retries_while_movement_state_persists() {
        let mut slot = BotRuntimeSlot::default();
        slot.decision_tick = 7;
        slot.commitment = Some(BotCommitment {
            action: BotSemanticAction::Light,
            expected_action: Some(FighterAction::LightAttack1),
            expires_tick: 10,
            accepted: false,
            last_press_tick: None,
        });
        let mut moving = FighterActionState::default();
        moving.action = FighterAction::Moving;

        update_commitment(&mut slot, &moving);
        assert!(
            slot.commitment
                .is_some_and(|commitment| !commitment.accepted)
        );
        let mut first = FighterInput::default();
        actuate(&mut slot, &mut first);
        assert!(first.light);

        slot.decision_tick += 1;
        update_commitment(&mut slot, &moving);
        assert!(
            slot.commitment
                .is_some_and(|commitment| !commitment.accepted)
        );
        let mut retry = FighterInput::default();
        actuate(&mut slot, &mut retry);
        assert!(retry.light);
    }
}
