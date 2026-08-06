use bevy::prelude::*;

use crate::arena::{SplitCausewayDoorState, arena_hazard_is_active_for_kind};
use crate::bot_profiles::{
    BOT_DECISION_HZ, BOT_OPPONENT_HISTORY_TICKS, BotProfile, BotProfileCatalog, BotProfileId,
};
use crate::characters::{CharacterMoveCatalog, FighterCharacter};
use crate::components::{
    BotBehaviorMode, BotBrain, BotMovementPlan, Fighter, FighterAction, FighterActionState,
    FighterInput, FighterMotor, FighterSpecialState, FighterStats,
};
use crate::constants::{
    COMBO_QUEUE_END, COMBO_QUEUE_START, FIGHTER_COUNT, ITEM_PICKUP_RANGE, MAX_STAMINA,
};
use crate::equipment::{FighterEquipment, LoadoutContext};
use crate::game_state::MatchState;
use crate::items::{ArenaItem, ItemKind, ItemState};
use crate::specials::{ActiveSpecial, SpecialKind};
use crate::styles::{FighterStyle, style_tuning};
use crate::techniques::{
    TechniqueButton, TechniqueMatchContext, TechniquePrediction,
    technique_prediction_for_context_in_catalog,
};

use super::{
    BotDifficulty, BotHeldItemDecision, BotNavigationCache, BotRecoveryDecision,
    BotTargetSnapshot, apply_edge_steering, arena_hazard_avoidance, bot_held_item_decision,
    arena_hazard_avoid_radius, bot_personality, bot_pickup_score, bot_range_band,
    bot_recovery_decision,
    bot_should_guard_threat, bot_should_jump_for_elevation, defensive_away_from,
    edge_danger, item_avoidance_radius, special_avoid_radius,
};
use super::navigation::NavigationBlockers;

const DECISION_STEP: f32 = 1.0 / BOT_DECISION_HZ as f32;
const MEMORY_TICKS: usize = BOT_OPPONENT_HISTORY_TICKS as usize;

const OBS_ATTACK: u8 = 1 << 0;
const OBS_GUARD: u8 = 1 << 1;
const OBS_GRAB: u8 = 1 << 2;
const OBS_AIR: u8 = 1 << 3;

#[derive(Clone, Copy, Debug)]
struct FighterSnapshot {
    id: usize,
    position: Vec3,
    facing: Vec3,
    action: FighterAction,
    action_elapsed: f32,
    grounded: bool,
    health: f32,
    stamina: f32,
    targetable_by: [bool; FIGHTER_COUNT],
}

#[derive(Clone, Copy, Debug)]
struct ItemSnapshot {
    kind: ItemKind,
    position: Vec3,
    loose: bool,
    threat_owner: Option<usize>,
    threat_radius: f32,
}

#[derive(Clone, Copy, Debug)]
struct SpecialSnapshot {
    owner_id: usize,
    kind: SpecialKind,
    position: Vec3,
}

#[derive(Default)]
pub(crate) struct BotSnapshotBuffer {
    replay_seed: u64,
    hazard_elapsed: f32,
    fighters: [Option<FighterSnapshot>; FIGHTER_COUNT],
    items: Vec<ItemSnapshot>,
    specials: Vec<SpecialSnapshot>,
    navigation_blockers: NavigationBlockers,
}

pub(super) fn build_world_snapshot(
    snapshot: &mut BotSnapshotBuffer,
    state: &MatchState,
    hazard_elapsed: f32,
    fighters: &Query<(
        &Fighter,
        &Transform,
        &FighterActionState,
        &FighterMotor,
        &FighterStats,
    )>,
    items: &Query<(&ArenaItem, &Transform)>,
    specials: &Query<(&ActiveSpecial, &Transform)>,
) {
    snapshot.replay_seed = state.replay_seed;
    snapshot.hazard_elapsed = hazard_elapsed;
    snapshot.fighters.fill(None);
    snapshot.items.clear();
    snapshot.specials.clear();
    snapshot.navigation_blockers.clear();

    for (fighter, transform, action, motor, stats) in fighters {
        if fighter.id >= FIGHTER_COUNT {
            continue;
        }
        snapshot.fighters[fighter.id] = Some(FighterSnapshot {
            id: fighter.id,
            position: transform.translation,
            facing: motor.facing,
            action: action.action,
            action_elapsed: action.elapsed,
            grounded: motor.grounded,
            health: stats.health,
            stamina: stats.stamina,
            targetable_by: std::array::from_fn(|attacker_id| {
                state.fighter_can_participate(fighter.id)
                    && state.combat_target_allowed_for_state(attacker_id, fighter.id)
            }),
        });
    }

    for (item, transform) in items {
        let threat = item_avoidance_radius(item);
        snapshot.items.push(ItemSnapshot {
            kind: item.kind,
            position: transform.translation,
            loose: matches!(item.state, ItemState::Loose) && item.pickup_lockout <= 0.0,
            threat_owner: threat.map(|value| value.0),
            threat_radius: threat.map_or(0.0, |value| value.1),
        });
    }
    snapshot.items.sort_by(|left, right| {
        item_rank(left.kind)
            .cmp(&item_rank(right.kind))
            .then_with(|| left.position.x.total_cmp(&right.position.x))
            .then_with(|| left.position.y.total_cmp(&right.position.y))
            .then_with(|| left.position.z.total_cmp(&right.position.z))
    });

    for (special, transform) in specials {
        snapshot.specials.push(SpecialSnapshot {
            owner_id: special.owner_id,
            kind: special.kind,
            position: transform.translation,
        });
    }
    snapshot.specials.sort_by(|left, right| {
        left.owner_id
            .cmp(&right.owner_id)
            .then_with(|| special_rank(left.kind).cmp(&special_rank(right.kind)))
            .then_with(|| left.position.x.total_cmp(&right.position.x))
            .then_with(|| left.position.y.total_cmp(&right.position.y))
            .then_with(|| left.position.z.total_cmp(&right.position.z))
    });

    for hazard in crate::arena_defs::active_arena_definition().hazards {
        if arena_hazard_is_active_for_kind(hazard_elapsed, hazard) {
            let _ = snapshot.navigation_blockers.push(
                Vec2::new(hazard.center.x, hazard.center.z),
                arena_hazard_avoid_radius(hazard),
            );
        }
    }
    for special in &snapshot.specials {
        if let Some(radius) = special_avoid_radius(special.kind) {
            let _ = snapshot.navigation_blockers.push(
                Vec2::new(special.position.x, special.position.z),
                radius,
            );
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

fn item_rank(kind: ItemKind) -> u8 {
    match kind {
        ItemKind::Crate => 0,
        ItemKind::Apple => 1,
        ItemKind::Turkey => 2,
        ItemKind::WineWhite => 3,
        ItemKind::Barrel => 4,
        ItemKind::CupCoffee => 5,
        ItemKind::Mushroom => 6,
        ItemKind::Steamer => 7,
    }
}

fn special_rank(kind: SpecialKind) -> u8 {
    match kind {
        SpecialKind::Projectile => 0,
        SpecialKind::Trap => 1,
        SpecialKind::Hazard => 2,
        SpecialKind::Shockwave => 3,
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
    pub(crate) goal: BotGoal,
    pub(crate) action: Option<BotSemanticAction>,
    pub(crate) reaction_gated: bool,
    pub(crate) reaction_delay_ticks: u8,
    pub(crate) commitment: BotCommitmentTrace,
    pub(crate) reason: BotDecisionReason,
    pub(crate) utility_score: f32,
}

impl BotDecisionTrace {
    pub(crate) fn signature(self) -> u64 {
        let target = self.target_id.map_or(0xff, |id| id.min(0xfe)) as u64;
        let action = self.action.map_or(0x1f, |action| action as u8) as u64;
        let score = (self.utility_score * 100.0)
            .round()
            .clamp(i16::MIN as f32, i16::MAX as f32) as i16 as u16 as u64;
        target
            | (self.goal as u64) << 8
            | action << 13
            | u64::from(self.reaction_gated) << 18
            | (self.reaction_delay_ticks as u64) << 19
            | (self.commitment as u64) << 27
            | (self.reason as u64) << 29
            | score << 37
    }
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
    pending_action: Option<FighterAction>,
    pending_since_tick: u64,
    pending_delay: u64,
    perceived_action: Option<FighterAction>,
    last_opener: Option<FighterAction>,
    repeated_openers: u8,
    last_position: Option<Vec3>,
    last_position_tick: u64,
    velocity: Vec2,
}

impl Default for BotOpponentMemory {
    fn default() -> Self {
        Self {
            samples: [0; MEMORY_TICKS],
            cursor: 0,
            len: 0,
            totals: [0; 4],
            raw_action: None,
            pending_action: None,
            pending_since_tick: 0,
            pending_delay: 0,
            perceived_action: None,
            last_opener: None,
            repeated_openers: 0,
            last_position: None,
            last_position_tick: 0,
            velocity: Vec2::ZERO,
        }
    }
}

impl BotOpponentMemory {
    fn observe_position(&mut self, position: Vec3, tick: u64) {
        if let Some(previous) = self.last_position {
            let tick_delta = tick.saturating_sub(self.last_position_tick);
            let delta = Vec2::new(position.x - previous.x, position.z - previous.z);
            if tick_delta == 0 || delta.length() > 6.0 {
                self.velocity = Vec2::ZERO;
            } else {
                let seconds = tick_delta as f32 * DECISION_STEP;
                let mut sample = delta / seconds.max(DECISION_STEP);
                if sample.length() > 12.0 {
                    sample = sample.normalize_or_zero() * 12.0;
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
    accumulator: f32,
    decision_tick: u64,
    target_id: Option<usize>,
    intent: BotIntent,
    trace: BotDecisionTrace,
    commitment: Option<BotCommitment>,
    opponents: [BotOpponentMemory; FIGHTER_COUNT],
    attack_ready_tick: u64,
    dash_ready_tick: u64,
    strafe_sign: f32,
}

impl Default for BotRuntimeSlot {
    fn default() -> Self {
        Self {
            initialized: false,
            last_seen_frame: 0,
            last_behavior: None,
            profile: None,
            accumulator: DECISION_STEP,
            decision_tick: 0,
            target_id: None,
            intent: BotIntent::default(),
            trace: BotDecisionTrace::default(),
            commitment: None,
            opponents: std::array::from_fn(|_| BotOpponentMemory::default()),
            attack_ready_tick: 0,
            dash_ready_tick: 0,
            strafe_sign: -1.0,
        }
    }
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

    pub(crate) fn trace(&self, bot_id: usize) -> Option<BotDecisionTrace> {
        self.slots
            .get(bot_id)
            .filter(|slot| slot.initialized)
            .map(|slot| slot.trace)
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
        technique_prediction_for_context_in_catalog(
            TechniqueMatchContext {
                previous: action.technique_id,
                button,
                elapsed: action.elapsed,
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

fn technique_execution_confidence(
    prediction: TechniquePrediction,
    contact_seconds: f32,
) -> f32 {
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
    Some(
        technique_execution_confidence(prediction, window.startup_seconds) + neutral_tiebreak,
    )
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
    let distance = flat.length();
    if distance > window.envelope || (predicted_position.y - bot_position.y).abs() > window.vertical_tolerance {
        return None;
    }
    let facing = Vec2::new(bot_facing.x, bot_facing.z)
        .normalize_or_zero()
        .dot(flat.normalize_or_zero());
    if facing < window.facing_dot {
        return None;
    }
    let ideal = window.envelope * 0.7;
    let range_quality = (1.0 - (distance - ideal).abs() / window.envelope.max(0.01))
        .clamp(0.0, 1.0);
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
        || stats.stamina - prediction.stamina_cost
            < MAX_STAMINA * stamina_reserve_ratio
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

fn signed_sample(
    seed: u64,
    fighter_id: usize,
    tick: u64,
    stream: RandomStream,
    index: u32,
) -> f32 {
    bot_sample(seed, fighter_id, tick, stream, index) * 2.0 - 1.0
}

#[allow(clippy::too_many_arguments)]
pub(super) fn drive_bot(
    dt: f32,
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

    slot.accumulator += dt.max(0.0);
    let elapsed_ticks = (slot.accumulator / DECISION_STEP).floor() as u64;
    if elapsed_ticks > 0 {
        slot.accumulator -= elapsed_ticks as f32 * DECISION_STEP;
        slot.decision_tick = slot.decision_tick.wrapping_add(elapsed_ticks);
        update_opponent_memory(slot, snapshot, bot_id, elapsed_ticks, replay_seed, profile);
        slot.target_id = choose_target(snapshot, bot_id, position, slot, profile, replay_seed);
    }

    update_commitment(slot, action);
    refresh_trace_state(slot);
    let nearest = nearest_legal_target(snapshot, bot_id, position);
    if action.action == FighterAction::Knockdown {
        slot.commitment = None;
        match bot_recovery_decision(
            action.elapsed,
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

    if action_locked(action.action) {
        sync_legacy_brain(brain, slot);
        return;
    }
    if matches!(action.action, FighterAction::LightAttack1 | FighterAction::LightAttack2) {
        if action.elapsed >= COMBO_QUEUE_START && action.elapsed <= COMBO_QUEUE_END {
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
        crate::arena_defs::active_arena_definition().hazards,
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
        let flat = Vec2::new(position.x - special.position.x, position.z - special.position.z);
        if flat.length() < radius + profile.hazard_safety_margin_m {
            emergency += flat.normalize_or_zero()
                * (radius + profile.hazard_safety_margin_m - flat.length());
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
        if flat.length() < radius {
            emergency += flat.normalize_or_zero() * (radius - flat.length());
        }
    }
    if emergency.length_squared() > 0.01 {
        slot.commitment = None;
        slot.intent = BotIntent {
            goal: BotGoal::Survive,
            movement: apply_edge_steering(position, emergency.normalize_or_zero()),
            ..default()
        };
        slot.trace.goal = BotGoal::Survive;
        slot.trace.reason = BotDecisionReason::Safety;
        slot.trace.utility_score = emergency.length();
        refresh_trace_state(slot);
        input.movement = slot.intent.movement;
        if motor.grounded && slot.decision_tick >= slot.dash_ready_tick {
            input.dash = true;
            slot.dash_ready_tick = slot.decision_tick + 30;
        }
        sync_legacy_brain(brain, slot);
        return;
    }

    if elapsed_ticks > 0 {
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
            style,
            equipment,
            stats,
            action,
            difficulty,
            held_kind,
            special_inputs_allowed,
            &techniques,
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
        position: target.position,
        distance: flat_distance(position, target.position),
        facing: target.facing,
        action: target.action,
    }
}

fn flat_distance(left: Vec3, right: Vec3) -> f32 {
    Vec2::new(left.x - right.x, left.z - right.z).length()
}

fn observation_flags(fighter: FighterSnapshot) -> u8 {
    let mut flags = 0;
    if is_attack_action(fighter.action) {
        flags |= OBS_ATTACK;
    }
    if fighter.action == FighterAction::Guarding {
        flags |= OBS_GUARD;
    }
    if matches!(fighter.action, FighterAction::GrabStartup | FighterAction::GrabHold) {
        flags |= OBS_GRAB;
    }
    if !fighter.grounded {
        flags |= OBS_AIR;
    }
    flags
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

fn update_opponent_memory(
    slot: &mut BotRuntimeSlot,
    snapshot: &BotSnapshotBuffer,
    bot_id: usize,
    elapsed_ticks: u64,
    seed: u64,
    profile: BotProfile,
) {
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
        memory.push(observation_flags(*fighter));

        if memory.raw_action != Some(fighter.action) {
            if is_attack_action(fighter.action) {
                if memory.last_opener == Some(fighter.action) {
                    memory.repeated_openers = memory.repeated_openers.saturating_add(1);
                } else {
                    memory.repeated_openers = 0;
                    memory.last_opener = Some(fighter.action);
                }
            }
            memory.raw_action = Some(fighter.action);
            memory.pending_action = Some(fighter.action);
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
            && slot.decision_tick.saturating_sub(memory.pending_since_tick)
                >= memory.pending_delay
        {
            memory.perceived_action = memory.pending_action.take();
        }
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
            * (1.0 + target.action_elapsed.clamp(0.0, 1.0) * 0.25);
        let threat = threat(perceived);
        let edge = edge_danger(target.position);
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
    let current_vulnerability = vulnerability(
        memory.perceived_action.unwrap_or(FighterAction::Idle),
    ) * (1.0 + current_fighter.action_elapsed.clamp(0.0, 1.0) * 0.25);
    let current_health_weakness =
        1.0 - (current_fighter.health / 100.0).clamp(0.0, 1.0);
    let current_stamina_weakness =
        1.0 - (current_fighter.stamina / MAX_STAMINA).clamp(0.0, 1.0);
    let current_score = (1.0 / (0.5 + flat_distance(position, current_fighter.position))
        + current_vulnerability * 0.35
        + memory.rate(0) * 0.2
        + edge_danger(current_fighter.position) * 0.25
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

#[allow(clippy::too_many_arguments)]
fn plan_bot(
    arena_index: usize,
    bot_id: usize,
    position: Vec3,
    motor: &FighterMotor,
    special_state: &FighterSpecialState,
    style: &FighterStyle,
    equipment: &FighterEquipment,
    stats: &FighterStats,
    action: &FighterActionState,
    difficulty: BotDifficulty,
    held_kind: Option<ItemKind>,
    special_inputs_allowed: bool,
    techniques: &BotTechniqueOptions,
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
    let (perceived_action, opponent_aggression_rate, opponent_guard_rate, target_velocity) = {
        let memory = &slot.opponents[target_id];
        (
            memory.perceived_action.unwrap_or(FighterAction::Idle),
            memory.rate(0),
            memory.rate(1),
            memory.velocity,
        )
    };
    let error = Vec2::new(
        signed_sample(seed, bot_id, slot.decision_tick, RandomStream::PerceptionX, target_id as u32),
        signed_sample(seed, bot_id, slot.decision_tick, RandomStream::PerceptionZ, target_id as u32),
    ) * profile.perception_error_m;
    let perceived_position = target.position + Vec3::new(error.x, 0.0, error.y);
    let lead_scale = if difficulty == BotDifficulty::Tutorial {
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
    let distance = flat_distance(position, intercept_position);
    let direct = Vec2::new(
        intercept_position.x - position.x,
        intercept_position.z - position.z,
    )
    .normalize_or_zero();
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
    let target_edge = edge_danger(target.position);
    let self_edge = edge_danger(position);
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
    let punish_need = vulnerability(perceived_action)
        * (1.0 + target.action_elapsed.clamp(0.0, 1.0) * 0.35);
    let collect_need = best_item.map_or(0.0, |(_, score, _)| score.max(0.0));
    let goals = [
        (BotGoal::Survive, profile.weights.survive * (danger_need + self_edge)),
        (BotGoal::RegainStamina, profile.weights.regain_stamina * stamina_need),
        (BotGoal::Reposition, profile.weights.reposition * 0.28),
        (BotGoal::Approach, profile.weights.approach * approach_need * style_modifier),
        (
            BotGoal::Pressure,
            profile.weights.pressure
                * (pressure_need + target_weakness * 0.25)
                * style_modifier
                + profile.weights.objective * target_edge,
        ),
        (BotGoal::Punish, profile.weights.punish * punish_need * style_modifier),
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
    let movement = match selected_goal {
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
    };
    let movement = apply_edge_steering(position, movement.normalize_or_zero());
    let delayed_target = BotTargetSnapshot {
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
        crate::arena_defs::active_arena_definition(),
    ) {
        consider(BotSemanticAction::Jump, 10.0);
    }
    if neutral_action_available(action.action)
        && slot.decision_tick >= slot.dash_ready_tick
        && motor.grounded
        && movement.length_squared() > 0.1
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
            if slot.decision_tick >= slot.attack_ready_tick { 0.0 } else { 1.0 },
            wave,
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
            && special_state.cooldown <= 0.0
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
            slot.commitment = Some(BotCommitment {
                action: selected,
                expected_action: match selected {
                    BotSemanticAction::Light => techniques.light.map(|prediction| prediction.action),
                    BotSemanticAction::Heavy => techniques.heavy.map(|prediction| prediction.action),
                    _ => None,
                },
                expires_tick: slot.decision_tick + duration.max(1),
                accepted: false,
                last_press_tick: None,
            });
        }
    }
    refresh_trace_state(slot);
    let _ = action;
}

fn reason_for_goal(goal: BotGoal, target_edge: f32, punish_need: f32) -> BotDecisionReason {
    match goal {
        BotGoal::Survive => BotDecisionReason::LowHealth,
        BotGoal::RegainStamina => BotDecisionReason::LowStamina,
        BotGoal::Approach | BotGoal::Reposition | BotGoal::Disengage => {
            BotDecisionReason::Spacing
        }
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
    slot.trace.commitment = slot.commitment.map_or(
        BotCommitmentTrace::None,
        |commitment| {
            if commitment.accepted {
                BotCommitmentTrace::Accepted
            } else {
                BotCommitmentTrace::Waiting
            }
        },
    );
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

fn update_commitment(slot: &mut BotRuntimeSlot, state: &FighterActionState) {
    let Some(mut commitment) = slot.commitment else {
        return;
    };
    let matches_expected = commitment.expected_action.map_or_else(
        || action_matches(commitment.action, state.action),
        |expected| state.action == expected,
    );
    if matches_expected {
        if matches!(commitment.action, BotSemanticAction::Dash | BotSemanticAction::Jump) {
            slot.commitment = None;
            return;
        }
        commitment.accepted = true;
    }
    let completed = commitment.accepted
        && !matches_expected
        && matches!(state.action, FighterAction::Idle | FighterAction::Moving);
    if (!commitment.accepted && slot.decision_tick >= commitment.expires_tick)
        || completed
        || (commitment.accepted && (state.cancel_window_open || state.branch_window_open))
    {
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
            FighterAction::HeavyAttack | FighterAction::HeavyAttack2 | FighterAction::JumpHeavyAttack
        ),
        BotSemanticAction::Grab => matches!(state, FighterAction::GrabStartup | FighterAction::GrabHold),
        BotSemanticAction::Jump => state == FighterAction::Jumping,
        BotSemanticAction::Dash => matches!(state, FighterAction::Dashing | FighterAction::DashAttack),
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
        | BotSemanticAction::SpecialShockwave => {
            slot.attack_ready_tick = slot.decision_tick + 28
        }
    }
}

fn sync_legacy_brain(brain: &mut BotBrain, slot: &BotRuntimeSlot) {
    brain.decision_timer = (DECISION_STEP - slot.accumulator).max(0.0);
    brain.attack_timer = slot
        .attack_ready_tick
        .saturating_sub(slot.decision_tick) as f32
        * DECISION_STEP;
    brain.dash_timer = slot.dash_ready_tick.saturating_sub(slot.decision_tick) as f32
        * DECISION_STEP;
    brain.movement_plan_timer = slot
        .commitment
        .map_or(0.0, |commitment| commitment.expires_tick.saturating_sub(slot.decision_tick) as f32 * DECISION_STEP);
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

        let mut targetable_by = [false; FIGHTER_COUNT];
        targetable_by[0] = true;
        let snapshot = BotSnapshotBuffer {
            replay_seed: seed,
            hazard_elapsed: 0.0,
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
                }),
                None,
                None,
            ],
            items: Vec::new(),
            specials: Vec::new(),
            navigation_blockers: NavigationBlockers::default(),
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
            cooldown: 0.0,
        };
        let action_state = FighterActionState::default();
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
                &style,
                &equipment,
                &stats,
                &action_state,
                BotDifficulty::Standard,
                None,
                false,
                &BotTechniqueOptions::default(),
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
                (Some(1), BotGoal::Approach, Some(BotSemanticAction::Dash), true, 3, BotCommitmentTrace::Waiting, 1),
                (Some(1), BotGoal::Approach, Some(BotSemanticAction::Dash), true, 2, BotCommitmentTrace::Waiting, 1),
                (Some(1), BotGoal::Approach, None, true, 1, BotCommitmentTrace::None, 0),
                (Some(1), BotGoal::Approach, None, false, 0, BotCommitmentTrace::None, 0),
                (Some(1), BotGoal::Approach, None, false, 0, BotCommitmentTrace::None, 0),
                (Some(1), BotGoal::Approach, None, false, 0, BotCommitmentTrace::None, 0),
            ]
        );
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
            let prediction =
                crate::techniques::technique_prediction_for_loadout_id_in_catalog(
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
        assert!(evaluate_contact_window(
            window,
            Vec3::ZERO,
            -Vec3::X,
            Vec3::new(1.4, 0.0, 0.0),
            Vec2::ZERO,
            1.0,
        )
        .is_none());
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
        assert!(slot.commitment.is_some_and(|commitment| commitment.accepted));
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
        assert!(slot.commitment.is_some_and(|commitment| !commitment.accepted));
        let mut first = FighterInput::default();
        actuate(&mut slot, &mut first);
        assert!(first.light);

        slot.decision_tick += 1;
        update_commitment(&mut slot, &moving);
        assert!(slot.commitment.is_some_and(|commitment| !commitment.accepted));
        let mut retry = FighterInput::default();
        actuate(&mut slot, &mut retry);
        assert!(retry.light);
    }
}
