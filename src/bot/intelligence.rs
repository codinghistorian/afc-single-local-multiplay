use bevy::prelude::*;

use crate::arena::{SplitCausewayDoorState, arena_hazard_is_active_for_kind};
use crate::bot_profiles::{
    BOT_DECISION_HZ, BOT_OPPONENT_HISTORY_TICKS, BotProfile, BotProfileCatalog, BotProfileId,
};
use crate::components::{
    BotBehaviorMode, BotBrain, BotMovementPlan, Fighter, FighterAction, FighterActionState,
    FighterInput, FighterMotor, FighterSpecialState, FighterStats,
};
use crate::constants::{
    COMBO_QUEUE_END, COMBO_QUEUE_START, FIGHTER_COUNT, ITEM_PICKUP_RANGE, MAX_STAMINA,
};
use crate::equipment::FighterEquipment;
use crate::game_state::MatchState;
use crate::items::{ArenaItem, ItemKind, ItemState};
use crate::specials::{ActiveSpecial, SpecialKind};
use crate::styles::{FighterStyle, style_tuning};

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
        }
    }
}

impl BotOpponentMemory {
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
    style: &FighterStyle,
    equipment: &FighterEquipment,
    stats: &FighterStats,
    action: &FighterActionState,
    difficulty: BotDifficulty,
    held_kind: Option<ItemKind>,
    special_inputs_allowed: bool,
    snapshot: &BotSnapshotBuffer,
    profiles: &BotProfileCatalog,
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
        FighterAction::Hitstun
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
    let (perceived_action, opponent_aggression_rate, opponent_guard_rate) = {
        let memory = &slot.opponents[target_id];
        (
            memory.perceived_action.unwrap_or(FighterAction::Idle),
            memory.rate(0),
            memory.rate(1),
        )
    };
    let error = Vec2::new(
        signed_sample(seed, bot_id, slot.decision_tick, RandomStream::PerceptionX, target_id as u32),
        signed_sample(seed, bot_id, slot.decision_tick, RandomStream::PerceptionZ, target_id as u32),
    ) * profile.perception_error_m;
    let perceived_position = target.position + Vec3::new(error.x, 0.0, error.y);
    let distance = flat_distance(position, perceived_position);
    let direct = Vec2::new(
        perceived_position.x - position.x,
        perceived_position.z - position.z,
    )
    .normalize_or_zero();
    if bot_sample(seed, bot_id, slot.decision_tick, RandomStream::Strafe, 0) < 0.12 {
        slot.strafe_sign *= -1.0;
    }
    let strafe = Vec2::new(-direct.y, direct.x) * slot.strafe_sign;
    let personality = bot_personality(style.kind, equipment.kind);
    let range = bot_range_band(style_tuning(style.kind).bot_preferred_range, personality);
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
        best_item.map(|value| value.0).unwrap_or(perceived_position)
    } else {
        perceived_position
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
        distance,
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
    if bot_should_jump_for_elevation(
        position,
        movement,
        motor.grounded,
        crate::arena_defs::active_arena_definition(),
    ) {
        consider_action(&mut best_action, BotSemanticAction::Jump, 10.0);
    }
    if slot.decision_tick >= slot.dash_ready_tick
        && motor.grounded
        && movement.length_squared() > 0.1
        && (distance > range.max + 0.45 || selected_goal == BotGoal::Disengage)
    {
        consider_action(&mut best_action, BotSemanticAction::Dash, 2.0);
    }
    if let Some(kind) = held_kind {
        let wave = signed_sample(seed, bot_id, slot.decision_tick, RandomStream::Item, 0);
        if let Some(decision) = bot_held_item_decision(
            kind,
            stats.stamina,
            distance,
            if slot.decision_tick >= slot.attack_ready_tick { 0.0 } else { 1.0 },
            wave,
        ) {
            consider_action(
                &mut best_action,
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
        consider_action(&mut best_action, BotSemanticAction::Pickup, 3.2);
    }

    if !mistake && slot.decision_tick >= slot.attack_ready_tick {
        let reserve_ok = stamina_ratio >= profile.attack_stamina_reserve_ratio;
        if !motor.grounded && !motor.air_attack_used && distance < 1.9 {
            consider_action(&mut best_action, BotSemanticAction::Light, 4.0 + punish_need);
        }
        if motor.grounded && distance < 1.55 * style_modifier {
            consider_action(&mut best_action, BotSemanticAction::Light, 3.5 + punish_need * 2.0);
        }
        if reserve_ok && motor.grounded && distance < 1.95 {
            consider_action(&mut best_action, BotSemanticAction::Heavy, 2.7 + punish_need * 2.3);
        }
        if difficulty == BotDifficulty::Standard && motor.grounded && distance < 0.9 {
            consider_action(
                &mut best_action,
                BotSemanticAction::Grab,
                2.5 + opponent_guard_rate * 2.0,
            );
        }
        if special_inputs_allowed && held_kind.is_none() && special_state.cooldown <= 0.0 {
            if (2.4..6.0).contains(&distance) {
                consider_action(&mut best_action, BotSemanticAction::SpecialProjectile, 2.45);
            }
            if distance < 1.35 {
                consider_action(&mut best_action, BotSemanticAction::SpecialTrap, 2.4);
            }
            if (1.6..3.4).contains(&distance) {
                consider_action(&mut best_action, BotSemanticAction::SpecialHazard, 2.3);
            }
            if (1.4..3.8).contains(&distance) {
                consider_action(&mut best_action, BotSemanticAction::SpecialShockwave, 2.35);
            }
        }
    }

    if let Some((selected, base_score)) = best_action {
        let jitter = signed_sample(
            seed,
            bot_id,
            slot.decision_tick,
            RandomStream::Action,
            selected as u32,
        ) * profile.utility_jitter;
        if base_score * (1.0 + jitter) > 0.0 {
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

fn update_commitment(slot: &mut BotRuntimeSlot, state: &FighterActionState) {
    let Some(mut commitment) = slot.commitment else {
        return;
    };
    if action_matches(commitment.action, state.action) {
        commitment.accepted = true;
    }
    let completed = commitment.accepted
        && !action_matches(commitment.action, state.action)
        && matches!(state.action, FighterAction::Idle | FighterAction::Moving);
    if slot.decision_tick >= commitment.expires_tick
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
}
