use bevy::prelude::*;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use bevy::window::PrimaryWindow;

use crate::arena::{
    ArenaHazardState, arena_hazard_is_active_for_kind, ground_support_for_arena_with_radius,
};
use crate::arena_defs::{
    ArenaDefinition, ArenaHazardDefinition, ArenaHazardKind, active_arena_definition,
};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::camera::ArenaCamera;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::characters::{CharacterKind, FighterCharacter, character_label, next_character_kind};
use crate::components::{
    BotBehaviorMode, BotBrain, BotMovementPlan, Controller, Fighter, FighterAction,
    FighterActionState, FighterInput, FighterInventory, FighterMotor, FighterSpecialState,
    FighterStats,
};
#[cfg(test)]
use crate::constants::ARENA_RADIUS;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::constants::{ARENA_TOP_Y, FIGHTER_RADIUS};
use crate::constants::{
    COMBO_QUEUE_END, COMBO_QUEUE_START, ITEM_BREEZE_BUOY_STAMINA, ITEM_PICKUP_RANGE,
    ITEM_THROW_RADIUS, MAX_STAMINA, POP_BOMB_RADIUS, QUICK_STAND_AFTER, SPECIAL_HAZARD_RADIUS,
    SPECIAL_PROJECTILE_RADIUS, SPECIAL_SHOCKWAVE_RADIUS, SPECIAL_TRAP_RADIUS,
};
use crate::equipment::{EquipmentKind, FighterEquipment};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::game_state::MatchAnnouncements;
use crate::game_state::{Hitstop, MatchState};
use crate::items::{ArenaItem, ItemKind, ItemRole, ItemState};
use crate::specials::{ActiveSpecial, SpecialKind};
use crate::styles::{FighterStyle, style_tuning};
use crate::user_mode::UserModeState;

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const BOT_ACTION_SELECT_RADIUS: f32 = FIGHTER_RADIUS * 2.65;
const BOT_EDGE_WARNING_DISTANCE: f32 = 1.35;

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct BotActionControl {
    selected_bot_id: Option<usize>,
    jump_bot_id: Option<usize>,
    guard_bot_id: Option<usize>,
    refill_bot_id: Option<usize>,
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn setup_bot_action_control(mut commands: Commands) {
    commands.init_resource::<BotActionControl>();
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
impl BotActionControl {
    fn select(&mut self, bot_id: usize) {
        self.selected_bot_id = Some(bot_id);
        if self.jump_bot_id.is_some_and(|id| id != bot_id) {
            self.jump_bot_id = None;
        }
        if self.guard_bot_id.is_some_and(|id| id != bot_id) {
            self.guard_bot_id = None;
        }
    }

    pub fn refill_bot_id(&self) -> Option<usize> {
        self.refill_bot_id
    }

    fn select_and_toggle_refill(&mut self, bot_id: usize) -> bool {
        self.select(bot_id);
        if self.refill_bot_id == Some(bot_id) {
            self.refill_bot_id = None;
            false
        } else {
            self.refill_bot_id = Some(bot_id);
            true
        }
    }

    fn clear(&mut self) -> bool {
        let had_control = self.selected_bot_id.is_some()
            || self.jump_bot_id.is_some()
            || self.guard_bot_id.is_some()
            || self.refill_bot_id.is_some();
        self.selected_bot_id = None;
        self.jump_bot_id = None;
        self.guard_bot_id = None;
        self.refill_bot_id = None;
        had_control
    }

    fn toggle_jump(&mut self) -> Option<bool> {
        let bot_id = self.selected_bot_id?;
        self.guard_bot_id = None;
        if self.jump_bot_id == Some(bot_id) {
            self.jump_bot_id = None;
            Some(false)
        } else {
            self.jump_bot_id = Some(bot_id);
            Some(true)
        }
    }

    fn toggle_guard(&mut self) -> Option<bool> {
        let bot_id = self.selected_bot_id?;
        self.jump_bot_id = None;
        if self.guard_bot_id == Some(bot_id) {
            self.guard_bot_id = None;
            Some(false)
        } else {
            self.guard_bot_id = Some(bot_id);
            Some(true)
        }
    }

    fn activate_movement(&mut self) -> Option<usize> {
        if self.jump_bot_id.is_none() && self.guard_bot_id.is_none() {
            return self.selected_bot_id;
        }
        self.jump_bot_id = None;
        self.guard_bot_id = None;
        self.selected_bot_id
    }
}

#[derive(Clone, Copy, Debug)]
struct BotPersonality {
    aggression: f32,
    item_greed: f32,
    hazard_fear: f32,
    special_bias: f32,
    mistake_rate: f32,
    panic_health: f32,
}

#[derive(Clone, Copy)]
struct BotTargetSnapshot {
    position: Vec3,
    distance: f32,
    facing: Vec3,
    action: FighterAction,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct BotRangeBand {
    min: f32,
    ideal: f32,
    max: f32,
}

#[derive(Debug, PartialEq)]
enum BotRecoveryDecision {
    Wait,
    QuickStand,
    Roll(Vec2),
}

#[derive(Debug, PartialEq, Eq)]
enum BotHeldItemDecision {
    Light,
    Heavy,
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn bot_action_control_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    cameras: Query<(&Camera, &GlobalTransform), With<ArenaCamera>>,
    state: Res<MatchState>,
    user_mode: Res<UserModeState>,
    fighters: Query<(&Fighter, &Controller, &Transform)>,
    mut bot_characters: Query<(&Fighter, &mut FighterCharacter)>,
    mut bot_brains: Query<(&Fighter, &mut BotBrain)>,
    mut control: ResMut<BotActionControl>,
    mut announcements: ResMut<MatchAnnouncements>,
) {
    if user_mode.blocks_dev_input() {
        control.clear();
        return;
    }

    if mouse.just_pressed(MouseButton::Left) {
        let selected = cursor_floor_position(&windows, &cameras)
            .and_then(|cursor| selectable_bot_at_cursor(cursor, &state, &fighters));
        if let Some(bot_id) = selected {
            let refill_enabled = control.select_and_toggle_refill(bot_id);
            announcements.show(
                format!(
                    "Bot target: P{} | HP refill {} | 0 Jump  1 Guard  2 Animal  3 Move",
                    bot_id + 1,
                    if refill_enabled { "on" } else { "off" }
                ),
                1.0,
            );
        } else if control.clear() {
            announcements.show("Bot control cleared", 0.75);
        }
    }

    if keys.just_pressed(KeyCode::Digit0) {
        match control.toggle_jump() {
            Some(true) => {
                if let Some(bot_id) = control.selected_bot_id {
                    announcements.show(format!("Bot P{} repeat jump", bot_id + 1), 0.75);
                }
            }
            Some(false) => {
                if let Some(bot_id) = control.selected_bot_id {
                    announcements.show(format!("Bot P{} jump off", bot_id + 1), 0.75);
                }
            }
            None => announcements.show("Click a bot first", 0.65),
        }
    }

    if keys.just_pressed(KeyCode::Digit1) {
        match control.toggle_guard() {
            Some(true) => {
                if let Some(bot_id) = control.selected_bot_id {
                    announcements.show(format!("Bot P{} guard", bot_id + 1), 0.75);
                }
            }
            Some(false) => {
                if let Some(bot_id) = control.selected_bot_id {
                    announcements.show(format!("Bot P{} guard off", bot_id + 1), 0.75);
                }
            }
            None => announcements.show("Click a bot first", 0.65),
        }
    }

    if keys.just_pressed(KeyCode::Digit2) {
        match control.selected_bot_id {
            Some(bot_id) => match cycle_bot_character(bot_id, &mut bot_characters) {
                Some(next_character) => {
                    announcements.show(
                        format!(
                            "Bot P{} animal -> {}",
                            bot_id + 1,
                            character_label(next_character)
                        ),
                        0.75,
                    );
                }
                None => announcements.show("Bot not found", 0.65),
            },
            None => announcements.show("Click a bot first", 0.65),
        }
    }

    if keys.just_pressed(KeyCode::Digit3) {
        match control.activate_movement() {
            Some(bot_id) => {
                if enable_bot_ai_for_selected(bot_id, &mut bot_brains) {
                    announcements.show(format!("Bot P{} combat AI enabled", bot_id + 1), 0.75);
                } else {
                    announcements.show("Bot not found", 0.65);
                }
            }
            None => announcements.show("Click a bot first", 0.65),
        }
    }
}

pub fn bot_input(
    time: Res<Time>,
    hitstop: Res<Hitstop>,
    state: Res<MatchState>,
    user_mode: Res<UserModeState>,
    hazard_state: Res<ArenaHazardState>,
    #[cfg(all(feature = "native", not(target_arch = "wasm32")))] mut action_control: ResMut<
        BotActionControl,
    >,
    mut bots: Query<(
        &Fighter,
        &Controller,
        &mut FighterInput,
        &mut BotBrain,
        &FighterMotor,
        &FighterInventory,
        &Transform,
        &FighterSpecialState,
        &FighterStyle,
        &FighterEquipment,
        &FighterStats,
        &FighterActionState,
    )>,
    all_fighters: Query<(&Fighter, &Transform, &FighterActionState, &FighterMotor)>,
    items: Query<(&ArenaItem, &Transform)>,
    specials: Query<(&ActiveSpecial, &Transform)>,
) {
    if hitstop.active() {
        return;
    }

    let dt = time.delta_secs();

    for (
        bot,
        controller,
        mut input,
        mut brain,
        motor,
        inventory,
        transform,
        special_state,
        style,
        equipment,
        stats,
        action,
    ) in &mut bots
    {
        if !controller.is_bot() {
            continue;
        }
        if !state.fighter_can_participate(bot.id) {
            *input = FighterInput::default();
            continue;
        }
        let tuning = style_tuning(style.kind);
        let personality = bot_personality(style.kind, equipment.kind);
        *input = FighterInput::default();

        #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
        {
            if let Some(forced_input) =
                controlled_bot_input(&mut action_control, bot.id, motor, action)
            {
                *input = forced_input;
                continue;
            }
        }

        if !bot_should_drive_autonomous_inputs(brain.behavior) {
            brain.decision_timer = 0.0;
            brain.movement_plan_timer = 0.0;
            brain.dash_timer = 0.0;
            brain.attack_timer = 0.0;
            continue;
        }

        brain.decision_timer -= dt;
        brain.movement_plan_timer -= dt;
        brain.dash_timer -= dt;
        brain.attack_timer -= dt;

        let mut nearest: Option<BotTargetSnapshot> = None;
        for (other, other_transform, other_action, other_motor) in &all_fighters {
            if other.id == bot.id
                || !state.fighter_can_participate(other.id)
                || !state.combat_target_allowed_for_state(bot.id, other.id)
                || matches!(
                    other_action.action,
                    FighterAction::RingOut | FighterAction::Respawning
                )
            {
                continue;
            }

            let delta = other_transform.translation - transform.translation;
            let dist = Vec2::new(delta.x, delta.z).length();
            if nearest.map_or(true, |target| dist < target.distance) {
                nearest = Some(BotTargetSnapshot {
                    position: other_transform.translation,
                    distance: dist,
                    facing: other_motor.facing,
                    action: other_action.action,
                });
            }
        }

        if action.action == FighterAction::Knockdown {
            match bot_recovery_decision(action.elapsed, transform.translation, nearest) {
                BotRecoveryDecision::Wait => {}
                BotRecoveryDecision::QuickStand => input.jump = true,
                BotRecoveryDecision::Roll(direction) => {
                    input.movement = direction;
                    input.dash = true;
                }
            }
            continue;
        }

        if matches!(
            action.action,
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
        ) {
            continue;
        }

        if matches!(
            action.action,
            FighterAction::LightAttack1 | FighterAction::LightAttack2
        ) {
            if action.elapsed >= COMBO_QUEUE_START && action.elapsed <= COMBO_QUEUE_END {
                input.light = true;
            }
            continue;
        }

        if brain.decision_timer <= 0.0 {
            brain.decision_timer = (0.65 + bot.id as f32 * 0.07) / personality.aggression;
            brain.strafe_sign *= -1.0;
        }

        let avoid_arena_hazard = arena_hazard_avoidance(
            transform.translation,
            hazard_state.elapsed(),
            active_arena_definition().hazards,
            personality.hazard_fear,
        );
        if avoid_arena_hazard.length_squared() > 0.01 {
            input.movement = apply_edge_steering(
                transform.translation,
                avoid_arena_hazard.normalize_or_zero(),
            );
            if motor.grounded && brain.dash_timer <= 0.0 {
                input.dash = true;
                brain.dash_timer = 1.55;
            }
            continue;
        }

        let mut avoid_special = Vec2::ZERO;
        for (special, special_transform) in &specials {
            if special.owner_id == bot.id
                || !state.combat_target_allowed_for_state(special.owner_id, bot.id)
            {
                continue;
            }
            let Some(avoid_radius) = special_avoid_radius(special.kind) else {
                continue;
            };
            let delta = transform.translation - special_transform.translation;
            let flat = Vec2::new(delta.x, delta.z);
            if flat.length() < avoid_radius {
                avoid_special += away_from_flat(flat) * (avoid_radius - flat.length());
            }
        }
        if avoid_special.length_squared() > 0.01 {
            input.movement =
                apply_edge_steering(transform.translation, avoid_special.normalize_or_zero());
            if motor.grounded && brain.dash_timer <= 0.0 {
                input.dash = true;
                brain.dash_timer = 1.55;
            }
            continue;
        }

        let mut avoid_item = Vec2::ZERO;
        for (item, item_transform) in &items {
            let Some((owner_id, avoid_radius)) = item_avoidance_radius(item) else {
                continue;
            };
            if !state.combat_target_allowed_for_state(owner_id, bot.id) {
                continue;
            }
            let delta = transform.translation - item_transform.translation;
            let flat = Vec2::new(delta.x, delta.z);
            if flat.length() < avoid_radius {
                avoid_item += away_from_flat(flat) * (avoid_radius - flat.length());
            }
        }
        if avoid_item.length_squared() > 0.01 {
            input.movement =
                apply_edge_steering(transform.translation, avoid_item.normalize_or_zero());
            if motor.grounded && brain.dash_timer <= 0.0 {
                input.dash = true;
                brain.dash_timer = 1.45;
            }
            continue;
        }

        let Some(target_snapshot) = nearest else {
            continue;
        };
        let target = target_snapshot.position;
        let distance = target_snapshot.distance;
        let to_target = target - transform.translation;
        let toward = Vec2::new(to_target.x, to_target.z).normalize_or_zero();
        let strafe = Vec2::new(-toward.y, toward.x) * brain.strafe_sign;
        let range = bot_range_band(tuning.bot_preferred_range, personality);

        if bot_should_panic(stats.health, personality) && distance < 2.8 && motor.grounded {
            input.guard = true;
            brain.movement_plan = BotMovementPlan::Retreat;
            brain.movement_plan_timer = brain.movement_plan_timer.max(0.35);
            input.movement = apply_edge_steering(
                transform.translation,
                defensive_away_from(transform.translation, target),
            );
            if brain.dash_timer <= 0.0 {
                input.dash = true;
                brain.dash_timer = 1.35;
            }
            brain.attack_timer = brain.attack_timer.max(0.35);
            continue;
        }

        if brain.attack_timer <= 0.0
            && bot_should_make_mistake(time.elapsed_secs(), bot.id, personality)
        {
            input.movement = strafe * 0.35;
            brain.attack_timer = 0.34;
            continue;
        }

        if action.action == FighterAction::Dashing && distance < 2.15 {
            input.light = true;
            continue;
        }

        if !motor.grounded {
            if !motor.air_attack_used && distance < 1.9 {
                input.light = true;
            }
            continue;
        }

        if bot_should_guard_threat(transform.translation, target_snapshot) && motor.grounded {
            input.guard = true;
            input.movement = apply_edge_steering(transform.translation, toward);
            brain.attack_timer = brain.attack_timer.max(0.28);
            if distance < 1.45 && brain.dash_timer <= 0.0 {
                input.movement = apply_edge_steering(
                    transform.translation,
                    defensive_away_from(transform.translation, target),
                );
                input.dash = true;
                brain.dash_timer = 1.65;
            }
            continue;
        }

        if let Some(held_entity) = inventory.held {
            let held_kind = items.get(held_entity).ok().map(|(item, _)| item.kind);
            let item_wave = (time.elapsed_secs() * (bot.id as f32 + 1.4)).sin();
            if let Some(decision) = held_kind.and_then(|kind| {
                bot_held_item_decision(kind, stats.stamina, distance, brain.attack_timer, item_wave)
            }) {
                match decision {
                    BotHeldItemDecision::Light => {
                        input.light = true;
                    }
                    BotHeldItemDecision::Heavy => {
                        input.heavy = true;
                    }
                }
                brain.attack_timer = bot_held_item_recovery(decision);
                continue;
            }
        } else if distance > 1.0 && brain.attack_timer <= 0.25 {
            let mut best_item_score = 0.0;
            for (item, item_transform) in &items {
                if !matches!(item.state, ItemState::Loose) || item.pickup_lockout > 0.0 {
                    continue;
                }
                let delta = item_transform.translation - transform.translation;
                let item_distance = Vec2::new(delta.x, delta.z).length();
                if item_distance <= ITEM_PICKUP_RANGE + 0.25 {
                    let score = bot_pickup_score(item.kind, stats.stamina, distance, item_distance)
                        * personality.item_greed;
                    if score > best_item_score {
                        best_item_score = score;
                    }
                }
            }
            if best_item_score > 0.25 {
                input.grab = true;
                brain.attack_timer = 0.7;
                continue;
            }
        }
        if bot_ai_special_inputs_allowed(&user_mode)
            && inventory.held.is_none()
            && special_state.cooldown <= 0.0
            && brain.attack_timer <= 0.0
        {
            let wave = time.elapsed_secs()
                * (bot.id as f32 + 1.9)
                * tuning.bot_special_bias
                * personality.special_bias;
            if distance > 2.4 && distance < 6.0 && wave.sin() > 0.68 {
                input.special = true;
                brain.attack_timer = 1.2;
                continue;
            }
            if distance < 1.35 && wave.cos() > 0.74 {
                input.special = true;
                input.grab = true;
                brain.attack_timer = 1.35;
                continue;
            }
            if distance > 1.6 && distance < 3.4 && wave.sin() < -0.76 {
                input.special = true;
                input.guard = true;
                brain.attack_timer = 1.4;
                continue;
            }
            if distance > 1.4 && distance < 3.8 && wave.cos() < -0.82 {
                input.special = true;
                input.heavy = true;
                brain.attack_timer = 1.55;
                continue;
            }
        }

        if brain.movement_plan_timer <= 0.0 {
            brain.movement_plan = choose_bot_movement_plan(
                transform.translation,
                target,
                distance,
                range,
                personality,
                stats.health,
                time.elapsed_secs(),
                bot.id,
            );
            brain.movement_plan_timer =
                bot_movement_plan_duration(brain.movement_plan, bot.id, personality);
        }
        input.movement = bot_tactical_movement(
            brain.movement_plan,
            transform.translation,
            target,
            toward,
            strafe,
            distance,
            range,
        );

        if bot_should_dash_for_movement(
            brain.movement_plan,
            transform.translation,
            input.movement,
            distance,
            range,
            motor.grounded,
            brain.dash_timer,
        ) {
            input.dash = true;
            brain.dash_timer = bot_movement_dash_cooldown(brain.movement_plan, bot.id);
        }

        if distance < 0.9
            && brain.attack_timer <= 0.0
            && (time.elapsed_secs() * (bot.id as f32 + 3.3)).sin() > 0.55
        {
            input.grab = true;
            brain.attack_timer = 1.15;
        }

        if distance < 1.55 * personality.aggression && brain.attack_timer <= 0.0 {
            input.light = true;
            brain.attack_timer = 0.72 / personality.aggression;
        }

        if distance < 1.75
            && brain.attack_timer <= 0.0
            && (time.elapsed_secs() * (bot.id as f32 + 2.6)).cos() > 0.58
        {
            input.jump = true;
            brain.attack_timer = 0.9;
        }

        if distance < 1.95
            && brain.attack_timer <= 0.18
            && (time.elapsed_secs() * (bot.id as f32 + 1.7)).sin() > 0.72
        {
            input.heavy = true;
            brain.attack_timer = 0.95;
        }

        if distance > 3.2 && brain.dash_timer <= 0.0 && edge_danger(transform.translation) <= 0.0 {
            input.dash = true;
            brain.dash_timer = 2.4 + bot.id as f32 * 0.35;
        }

        let facing_target = motor
            .facing
            .normalize_or_zero()
            .dot(Vec3::new(toward.x, 0.0, toward.y))
            > 0.35;
        if distance < 1.35
            && facing_target
            && (time.elapsed_secs() * (bot.id as f32 + 2.1)).cos() > 0.68
        {
            input.guard = true;
        }
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn controlled_bot_input(
    control: &mut BotActionControl,
    bot_id: usize,
    motor: &FighterMotor,
    action: &FighterActionState,
) -> Option<FighterInput> {
    if control.jump_bot_id == Some(bot_id) {
        return Some(FighterInput {
            jump: true,
            ..default()
        });
    }

    if control.guard_bot_id == Some(bot_id) {
        if !bot_guard_should_press(motor, action) {
            return Some(FighterInput::default());
        }
        return Some(FighterInput {
            guard: true,
            ..default()
        });
    }

    None
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn bot_guard_should_press(motor: &FighterMotor, action: &FighterActionState) -> bool {
    if action.action == FighterAction::Guarding {
        return true;
    }
    motor.grounded
        && motor.guard_cooldown_timer <= 0.0
        && !motor.guard_was_requested
        && matches!(action.action, FighterAction::Idle | FighterAction::Moving)
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn selectable_bot_at_cursor(
    cursor: Vec3,
    state: &MatchState,
    fighters: &Query<(&Fighter, &Controller, &Transform)>,
) -> Option<usize> {
    let mut selected_bot_id = None;
    let mut best_distance = BOT_ACTION_SELECT_RADIUS;
    for (fighter, controller, transform) in fighters {
        if !controller.is_bot() || !state.fighter_can_participate(fighter.id) {
            continue;
        }
        let distance = bot_selection_distance(cursor, transform.translation);
        if distance <= best_distance {
            best_distance = distance;
            selected_bot_id = Some(fighter.id);
        }
    }
    selected_bot_id
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn cycle_bot_character(
    bot_id: usize,
    bot_characters: &mut Query<(&Fighter, &mut FighterCharacter)>,
) -> Option<CharacterKind> {
    for (fighter, mut character) in bot_characters.iter_mut() {
        if fighter.id != bot_id {
            continue;
        }

        let next = next_character_kind(character.kind);
        character.kind = next;
        return Some(next);
    }
    None
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn enable_bot_ai_for_selected(
    bot_id: usize,
    bot_brains: &mut Query<(&Fighter, &mut BotBrain)>,
) -> bool {
    for (fighter, mut brain) in bot_brains.iter_mut() {
        if fighter.id != bot_id {
            continue;
        }
        start_bot_combat_ai(&mut brain);
        return true;
    }
    false
}

pub(crate) fn start_bot_combat_ai(brain: &mut BotBrain) {
    brain.behavior = BotBehaviorMode::Combatant;
    brain.decision_timer = 0.05;
    brain.movement_plan_timer = 0.05;
    brain.dash_timer = 0.05;
    brain.attack_timer = 0.05;
}

pub fn default_bot_brain_for_fighter(fighter_id: usize) -> BotBrain {
    BotBrain {
        behavior: if std::env::var_os("FFC_BOT_COMBATANT").is_some() {
            BotBehaviorMode::Combatant
        } else {
            BotBehaviorMode::TrainingDummy
        },
        decision_timer: 0.0,
        movement_plan_timer: 0.0,
        dash_timer: 0.7 + fighter_id as f32 * 0.45,
        attack_timer: 0.25,
        strafe_sign: if fighter_id == 2 { 1.0 } else { -1.0 },
        movement_plan: BotMovementPlan::Circle,
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn bot_selection_distance(cursor: Vec3, fighter_position: Vec3) -> f32 {
    Vec2::new(cursor.x - fighter_position.x, cursor.z - fighter_position.z).length()
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn cursor_floor_position(
    windows: &Query<&Window, With<PrimaryWindow>>,
    cameras: &Query<(&Camera, &GlobalTransform), With<ArenaCamera>>,
) -> Option<Vec3> {
    let window = windows.iter().next()?;
    let cursor_position = window.cursor_position()?;
    let (camera, camera_transform) = cameras.iter().next()?;
    let ray = camera
        .viewport_to_world(camera_transform, cursor_position)
        .ok()?;
    ray.plane_intersection_point(
        Vec3::new(0.0, ARENA_TOP_Y + 0.04, 0.0),
        InfinitePlane3d::new(Vec3::Y),
    )
}

fn bot_should_drive_autonomous_inputs(behavior: BotBehaviorMode) -> bool {
    matches!(behavior, BotBehaviorMode::Combatant)
}

fn bot_ai_special_inputs_allowed(user_mode: &UserModeState) -> bool {
    !user_mode.restricts_bot_special_inputs()
}

fn bot_recovery_decision(
    elapsed: f32,
    bot_position: Vec3,
    target: Option<BotTargetSnapshot>,
) -> BotRecoveryDecision {
    if elapsed < QUICK_STAND_AFTER {
        return BotRecoveryDecision::Wait;
    }

    if let Some(target) = target {
        if target.distance < 3.2 {
            return BotRecoveryDecision::Roll(defensive_away_from(bot_position, target.position));
        }
    }

    BotRecoveryDecision::QuickStand
}

fn bot_should_guard_threat(bot_position: Vec3, target: BotTargetSnapshot) -> bool {
    let Some(range) = guard_threat_range(target.action) else {
        return false;
    };
    if target.distance > range {
        return false;
    }

    let target_to_bot = Vec3::new(
        bot_position.x - target.position.x,
        0.0,
        bot_position.z - target.position.z,
    )
    .normalize_or_zero();
    target.facing.normalize_or_zero().dot(target_to_bot) > 0.2
}

fn bot_personality(
    style: crate::styles::FighterStyleKind,
    equipment: EquipmentKind,
) -> BotPersonality {
    let mut personality = match style {
        crate::styles::FighterStyleKind::Anchor => BotPersonality {
            aggression: 0.92,
            item_greed: 0.9,
            hazard_fear: 1.1,
            special_bias: 0.85,
            mistake_rate: 0.08,
            panic_health: 34.0,
        },
        crate::styles::FighterStyleKind::Vector => BotPersonality {
            aggression: 1.16,
            item_greed: 0.96,
            hazard_fear: 0.92,
            special_bias: 1.0,
            mistake_rate: 0.12,
            panic_health: 24.0,
        },
        crate::styles::FighterStyleKind::Catalyst => BotPersonality {
            aggression: 0.98,
            item_greed: 0.86,
            hazard_fear: 1.0,
            special_bias: 1.22,
            mistake_rate: 0.07,
            panic_health: 28.0,
        },
    };

    match equipment {
        EquipmentKind::DashCoil => personality.aggression *= 1.06,
        EquipmentKind::AerialSpur => personality.mistake_rate *= 1.03,
        EquipmentKind::CounterCell => personality.hazard_fear *= 1.08,
        EquipmentKind::HeavySeal => personality.item_greed *= 1.08,
    }

    personality
}

fn bot_should_panic(health: f32, personality: BotPersonality) -> bool {
    health <= personality.panic_health
}

fn bot_should_make_mistake(elapsed: f32, bot_id: usize, personality: BotPersonality) -> bool {
    let wave = (elapsed * (bot_id as f32 + 2.9)).sin();
    wave > 1.0 - personality.mistake_rate
}

fn guard_threat_range(action: FighterAction) -> Option<f32> {
    match action {
        FighterAction::LightAttack1 | FighterAction::LightAttack2 => Some(1.85),
        FighterAction::ComboFinisher | FighterAction::HeavyAttack | FighterAction::HeavyAttack2 => {
            Some(2.25)
        }
        FighterAction::DashAttack | FighterAction::JumpAttack | FighterAction::JumpHeavyAttack => {
            Some(2.45)
        }
        FighterAction::ItemSwing | FighterAction::ItemThrow => Some(2.6),
        FighterAction::SpecialCast => Some(3.1),
        FighterAction::GuardCounter => Some(1.8),
        _ => None,
    }
}

fn special_avoid_radius(kind: SpecialKind) -> Option<f32> {
    match kind {
        SpecialKind::Projectile => Some(SPECIAL_PROJECTILE_RADIUS + 1.05),
        SpecialKind::Trap => Some(SPECIAL_TRAP_RADIUS + 0.85),
        SpecialKind::Shockwave => Some(SPECIAL_SHOCKWAVE_RADIUS + 0.8),
        SpecialKind::Hazard => Some(SPECIAL_HAZARD_RADIUS + 1.15),
    }
}

fn item_avoidance_radius(item: &ArenaItem) -> Option<(usize, f32)> {
    match item.state {
        ItemState::Thrown { owner_id, .. } => Some((owner_id, ITEM_THROW_RADIUS + 0.9)),
        ItemState::Armed { owner_id, .. } => Some((owner_id, POP_BOMB_RADIUS + 0.65)),
        _ => None,
    }
}

fn bot_held_item_decision(
    kind: ItemKind,
    stamina: f32,
    distance: f32,
    attack_timer: f32,
    wave: f32,
) -> Option<BotHeldItemDecision> {
    if attack_timer > 0.0 {
        return None;
    }

    match kind {
        ItemKind::Apple | ItemKind::Turkey => return Some(BotHeldItemDecision::Light),
        ItemKind::WineWhite | ItemKind::Barrel => {
            if stamina <= MAX_STAMINA - 10.0 {
                return Some(BotHeldItemDecision::Light);
            }
            return None;
        }
        ItemKind::CupCoffee | ItemKind::Mushroom => {
            if wave > -0.2 {
                return Some(BotHeldItemDecision::Light);
            }
        }
        ItemKind::Steamer => {}
    }

    match kind.role() {
        ItemRole::Explosive => {
            if distance < 1.15 {
                Some(BotHeldItemDecision::Light)
            } else if (1.6..5.0).contains(&distance) {
                Some(BotHeldItemDecision::Heavy)
            } else {
                None
            }
        }
        ItemRole::Utility => {
            if stamina <= MAX_STAMINA - ITEM_BREEZE_BUOY_STAMINA * 0.45 {
                Some(BotHeldItemDecision::Light)
            } else {
                None
            }
        }
        ItemRole::Recovery => Some(BotHeldItemDecision::Light),
    }
}

fn bot_held_item_recovery(decision: BotHeldItemDecision) -> f32 {
    match decision {
        BotHeldItemDecision::Light => 0.72,
        BotHeldItemDecision::Heavy => 1.12,
    }
}

fn bot_pickup_score(kind: ItemKind, stamina: f32, target_distance: f32, item_distance: f32) -> f32 {
    let role_bonus = match kind.role() {
        ItemRole::Recovery => 0.2,
        ItemRole::Utility if stamina <= MAX_STAMINA - 10.0 => 0.28,
        ItemRole::Utility if stamina > MAX_STAMINA - 7.0 => -0.12,
        ItemRole::Explosive if (1.6..5.2).contains(&target_distance) => 0.18,
        _ => 0.0,
    };

    kind.bot_pickup_priority() + role_bonus - item_distance * 0.2
}

fn bot_range_band(preferred_range: f32, personality: BotPersonality) -> BotRangeBand {
    let ideal = (preferred_range / personality.aggression.sqrt()).clamp(1.05, 2.65);
    BotRangeBand {
        min: (ideal * 0.68).clamp(0.85, 1.55),
        ideal,
        max: ideal * 1.45 + 0.35,
    }
}

#[allow(clippy::too_many_arguments)]
fn choose_bot_movement_plan(
    bot_position: Vec3,
    target_position: Vec3,
    distance: f32,
    range: BotRangeBand,
    personality: BotPersonality,
    health: f32,
    elapsed: f32,
    bot_id: usize,
) -> BotMovementPlan {
    choose_bot_movement_plan_for_arena(
        bot_position,
        target_position,
        distance,
        range,
        personality,
        health,
        elapsed,
        bot_id,
        active_arena_definition(),
    )
}

#[allow(clippy::too_many_arguments)]
fn choose_bot_movement_plan_for_arena(
    bot_position: Vec3,
    target_position: Vec3,
    distance: f32,
    range: BotRangeBand,
    personality: BotPersonality,
    health: f32,
    elapsed: f32,
    bot_id: usize,
    arena: &ArenaDefinition,
) -> BotMovementPlan {
    if bot_should_panic(health, personality) && distance < range.max + 0.9 {
        return BotMovementPlan::Retreat;
    }
    if distance > range.max {
        return BotMovementPlan::Approach;
    }
    if distance < range.min {
        return BotMovementPlan::Backstep;
    }

    let bot_edge = edge_danger_for_arena(bot_position, arena);
    let target_edge = edge_danger_for_arena(target_position, arena);
    if target_edge > 0.3 && bot_edge < target_edge {
        return BotMovementPlan::Pressure;
    }
    if bot_edge > 0.55 {
        return BotMovementPlan::Circle;
    }

    let wave = (elapsed * (bot_id as f32 + 1.75)).sin();
    if distance < range.ideal * 1.15 && wave > 0.68 {
        BotMovementPlan::Pressure
    } else {
        BotMovementPlan::Circle
    }
}

fn bot_movement_plan_duration(
    plan: BotMovementPlan,
    bot_id: usize,
    personality: BotPersonality,
) -> f32 {
    let base = match plan {
        BotMovementPlan::Approach => 0.38,
        BotMovementPlan::Circle => 0.55,
        BotMovementPlan::Backstep => 0.28,
        BotMovementPlan::Pressure => 0.34,
        BotMovementPlan::Retreat => 0.42,
    };
    base / personality.aggression.clamp(0.75, 1.35) + bot_id as f32 * 0.015
}

fn bot_tactical_movement(
    plan: BotMovementPlan,
    bot_position: Vec3,
    target_position: Vec3,
    toward: Vec2,
    strafe: Vec2,
    distance: f32,
    range: BotRangeBand,
) -> Vec2 {
    let movement = match plan {
        BotMovementPlan::Approach => {
            let strafe_weight = if distance > range.max + 0.9 {
                0.12
            } else {
                0.28
            };
            toward + strafe * strafe_weight
        }
        BotMovementPlan::Circle => {
            let radial = if distance < range.min {
                -toward * 0.45
            } else if distance > range.ideal {
                toward * 0.35
            } else {
                Vec2::ZERO
            };
            radial + strafe * 0.95
        }
        BotMovementPlan::Backstep => -toward * 0.72 + strafe * 0.45,
        BotMovementPlan::Pressure => {
            let target_edge = edge_danger(target_position);
            let strafe_weight = if target_edge > 0.0 { 0.16 } else { 0.34 };
            toward * 0.78 + strafe * strafe_weight
        }
        BotMovementPlan::Retreat => -toward * 0.82 + strafe * 0.28,
    };
    apply_edge_steering(bot_position, movement.normalize_or_zero())
}

fn bot_should_dash_for_movement(
    plan: BotMovementPlan,
    bot_position: Vec3,
    movement: Vec2,
    distance: f32,
    range: BotRangeBand,
    grounded: bool,
    dash_timer: f32,
) -> bool {
    bot_should_dash_for_movement_for_arena(
        plan,
        bot_position,
        movement,
        distance,
        range,
        grounded,
        dash_timer,
        active_arena_definition(),
    )
}

#[allow(clippy::too_many_arguments)]
fn bot_should_dash_for_movement_for_arena(
    plan: BotMovementPlan,
    bot_position: Vec3,
    movement: Vec2,
    distance: f32,
    range: BotRangeBand,
    grounded: bool,
    dash_timer: f32,
    arena: &ArenaDefinition,
) -> bool {
    if !grounded
        || dash_timer > 0.0
        || movement.length_squared() <= 0.01
        || movement_points_toward_edge_for_arena(bot_position, movement, arena)
    {
        return false;
    }

    match plan {
        BotMovementPlan::Approach => distance > range.max + 0.45,
        BotMovementPlan::Pressure => distance > range.ideal + 0.35 && distance < range.max + 1.4,
        BotMovementPlan::Backstep | BotMovementPlan::Retreat => distance < range.min + 0.25,
        BotMovementPlan::Circle => false,
    }
}

fn bot_movement_dash_cooldown(plan: BotMovementPlan, bot_id: usize) -> f32 {
    let base = match plan {
        BotMovementPlan::Approach => 1.45,
        BotMovementPlan::Pressure => 1.65,
        BotMovementPlan::Backstep | BotMovementPlan::Retreat => 1.25,
        BotMovementPlan::Circle => 2.2,
    };
    base + bot_id as f32 * 0.12
}

fn edge_danger(position: Vec3) -> f32 {
    edge_danger_for_arena(position, active_arena_definition())
}

fn edge_danger_for_arena(position: Vec3, arena: &ArenaDefinition) -> f32 {
    if !arena_point_supported(arena, position.x, position.z) {
        return 1.0;
    }

    const PROBE_STEPS: usize = 6;
    const PROBE_DIRECTIONS: usize = 16;
    for step in 1..=PROBE_STEPS {
        let distance = BOT_EDGE_WARNING_DISTANCE * step as f32 / PROBE_STEPS as f32;
        for index in 0..PROBE_DIRECTIONS {
            let angle = index as f32 * std::f32::consts::TAU / PROBE_DIRECTIONS as f32;
            let probe =
                Vec2::new(position.x, position.z) + Vec2::new(angle.cos(), angle.sin()) * distance;
            if !arena_point_supported(arena, probe.x, probe.y) {
                return 1.0 - (step.saturating_sub(1) as f32 / PROBE_STEPS as f32);
            }
        }
    }

    0.0
}

fn edge_inward_direction_for_arena(position: Vec3, arena: &ArenaDefinition) -> Vec2 {
    const PROBE_DIRECTIONS: usize = 16;
    let origin = Vec2::new(position.x, position.z);
    let mut inward = Vec2::ZERO;
    for index in 0..PROBE_DIRECTIONS {
        let angle = index as f32 * std::f32::consts::TAU / PROBE_DIRECTIONS as f32;
        let direction = Vec2::new(angle.cos(), angle.sin());
        for (distance, weight) in [(0.65, 0.7), (1.25, 1.0), (1.9, 1.35)] {
            let probe = origin + direction * distance;
            if arena_point_supported(arena, probe.x, probe.y) {
                inward += direction * weight;
            }
        }
    }

    if inward.length_squared() > 0.001 {
        inward.normalize_or_zero()
    } else {
        let arena_center = arena
            .spawn_points
            .iter()
            .map(|point| Vec2::new(point.x, point.z))
            .sum::<Vec2>()
            / arena.spawn_points.len() as f32;
        (arena_center - origin).normalize_or_zero()
    }
}

fn movement_points_toward_edge_for_arena(
    position: Vec3,
    movement: Vec2,
    arena: &ArenaDefinition,
) -> bool {
    let movement = movement.normalize_or_zero();
    if movement.length_squared() <= 0.001 {
        return false;
    }
    let probe = Vec2::new(position.x, position.z) + movement * BOT_EDGE_WARNING_DISTANCE;
    !arena_point_supported(arena, probe.x, probe.y)
        || (edge_danger_for_arena(position, arena) > 0.0
            && movement.dot(edge_inward_direction_for_arena(position, arena)) < -0.25)
}

fn apply_edge_steering(position: Vec3, movement: Vec2) -> Vec2 {
    apply_edge_steering_for_arena(position, movement, active_arena_definition())
}

fn apply_edge_steering_for_arena(position: Vec3, movement: Vec2, arena: &ArenaDefinition) -> Vec2 {
    let movement = movement.normalize_or_zero();
    let movement_probe = Vec2::new(position.x, position.z) + movement * BOT_EDGE_WARNING_DISTANCE;
    let points_off_stage = movement.length_squared() > 0.001
        && !arena_point_supported(arena, movement_probe.x, movement_probe.y);
    let danger =
        edge_danger_for_arena(position, arena).max(if points_off_stage { 0.85 } else { 0.0 });
    if danger <= 0.0 {
        return movement;
    }

    let inward = edge_inward_direction_for_arena(position, arena);
    if movement.length_squared() <= 0.01 {
        return inward;
    }
    (movement * (1.0 - danger * 0.85) + inward * (0.75 + danger * 0.9)).normalize_or_zero()
}

fn arena_point_supported(arena: &ArenaDefinition, x: f32, z: f32) -> bool {
    ground_support_for_arena_with_radius(arena, x, z, 0.0)
        .height()
        .is_some()
}

fn arena_hazard_avoidance(
    position: Vec3,
    hazard_elapsed: f32,
    hazards: &[ArenaHazardDefinition],
    hazard_fear: f32,
) -> Vec2 {
    let mut avoidance = Vec2::ZERO;
    for hazard in hazards {
        if !arena_hazard_is_active_for_kind(hazard_elapsed, hazard) {
            continue;
        }
        let flat = Vec2::new(position.x - hazard.center.x, position.z - hazard.center.z);
        let avoid_radius = arena_hazard_avoid_radius(hazard) * hazard_fear;
        if flat.length() < avoid_radius {
            avoidance += away_from_flat(flat) * (avoid_radius - flat.length());
        }
    }
    avoidance
}

fn arena_hazard_avoid_radius(hazard: &ArenaHazardDefinition) -> f32 {
    hazard.radius
        + match hazard.kind {
            ArenaHazardKind::PulseVent => 1.15,
            ArenaHazardKind::SnareField => 0.8,
            ArenaHazardKind::BumperNode => 1.35,
        }
}

fn defensive_away_from(bot_position: Vec3, target_position: Vec3) -> Vec2 {
    away_from_flat(Vec2::new(
        bot_position.x - target_position.x,
        bot_position.z - target_position.z,
    ))
}

fn away_from_flat(flat: Vec2) -> Vec2 {
    if flat.length_squared() > 0.001 {
        flat.normalize_or_zero()
    } else {
        Vec2::X
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(position: Vec3, facing: Vec3, action: FighterAction) -> BotTargetSnapshot {
        BotTargetSnapshot {
            position,
            distance: Vec2::new(position.x, position.z).length(),
            facing,
            action,
        }
    }

    fn default_motor_and_action() -> (FighterMotor, FighterActionState) {
        (FighterMotor::default(), FighterActionState::default())
    }

    #[test]
    fn bot_action_control_jump_and_guard_repeat_until_toggled() {
        let mut control = BotActionControl::default();
        control.select(1);
        let (motor, action) = default_motor_and_action();

        assert_eq!(control.toggle_jump(), Some(true));
        let jump = controlled_bot_input(&mut control, 1, &motor, &action).unwrap();
        assert!(jump.jump);
        assert!(!jump.guard);
        assert!(
            controlled_bot_input(&mut control, 1, &motor, &action)
                .unwrap()
                .jump
        );

        assert_eq!(control.toggle_jump(), Some(false));
        assert!(controlled_bot_input(&mut control, 1, &motor, &action).is_none());

        assert_eq!(control.toggle_guard(), Some(true));
        let guard = controlled_bot_input(&mut control, 1, &motor, &action).unwrap();
        assert!(guard.guard);
        assert!(!guard.jump);
        assert!(
            controlled_bot_input(&mut control, 1, &motor, &action)
                .unwrap()
                .guard
        );

        assert_eq!(control.toggle_guard(), Some(false));
        assert!(controlled_bot_input(&mut control, 1, &motor, &action).is_none());
    }

    #[test]
    fn bot_action_control_selecting_new_bot_clears_old_actions() {
        let mut control = BotActionControl::default();
        let (motor, action) = default_motor_and_action();
        control.select(1);
        assert_eq!(control.toggle_guard(), Some(true));
        control.select(2);

        assert!(controlled_bot_input(&mut control, 1, &motor, &action).is_none());
        assert!(controlled_bot_input(&mut control, 2, &motor, &action).is_none());
        assert_eq!(control.toggle_jump(), Some(true));
        assert!(
            controlled_bot_input(&mut control, 2, &motor, &action)
                .unwrap()
                .jump
        );
    }

    #[test]
    fn bot_click_refill_toggle_moves_between_bots() {
        let mut control = BotActionControl::default();

        assert!(control.select_and_toggle_refill(1));
        assert_eq!(control.refill_bot_id(), Some(1));

        assert!(!control.select_and_toggle_refill(1));
        assert_eq!(control.refill_bot_id(), None);

        assert!(control.select_and_toggle_refill(2));
        assert_eq!(control.refill_bot_id(), Some(2));

        assert!(control.clear());
        assert_eq!(control.refill_bot_id(), None);
    }

    #[test]
    fn bot_action_control_clear_removes_forced_debug_inputs() {
        let mut control = BotActionControl::default();
        let (motor, action) = default_motor_and_action();
        control.select(1);
        assert_eq!(control.toggle_jump(), Some(true));
        assert!(controlled_bot_input(&mut control, 1, &motor, &action).is_some());

        assert!(control.clear());

        assert_eq!(control.selected_bot_id, None);
        assert_eq!(control.refill_bot_id(), None);
        assert!(controlled_bot_input(&mut control, 1, &motor, &action).is_none());
    }

    #[test]
    fn bot_digit3_enables_combat_ai() {
        let mut brain = BotBrain {
            behavior: BotBehaviorMode::TrainingDummy,
            decision_timer: 2.8,
            movement_plan_timer: 1.6,
            dash_timer: 1.9,
            attack_timer: 3.4,
            strafe_sign: -1.0,
            movement_plan: BotMovementPlan::Circle,
        };
        start_bot_combat_ai(&mut brain);

        assert_eq!(brain.behavior, BotBehaviorMode::Combatant);
        assert!(brain.decision_timer <= 0.05);
        assert!(brain.movement_plan_timer <= 0.05);
        assert!(brain.dash_timer <= 0.05);
        assert!(brain.attack_timer <= 0.05);
        assert_eq!(brain.strafe_sign, -1.0);
    }

    #[test]
    fn bot_guard_control_releases_until_guard_can_restart() {
        let mut control = BotActionControl::default();
        control.select(1);
        assert_eq!(control.toggle_guard(), Some(true));

        let mut motor = FighterMotor::default();
        let mut action = FighterActionState::default();
        assert!(
            controlled_bot_input(&mut control, 1, &motor, &action)
                .unwrap()
                .guard
        );

        motor.guard_cooldown_timer = 0.05;
        motor.guard_was_requested = true;
        assert!(
            !controlled_bot_input(&mut control, 1, &motor, &action)
                .unwrap()
                .guard
        );

        motor.guard_cooldown_timer = 0.0;
        motor.guard_was_requested = false;
        assert!(
            controlled_bot_input(&mut control, 1, &motor, &action)
                .unwrap()
                .guard
        );

        action.action = FighterAction::Hitstun;
        assert!(
            !controlled_bot_input(&mut control, 1, &motor, &action)
                .unwrap()
                .guard
        );

        action.action = FighterAction::Guarding;
        motor.grounded = false;
        assert!(
            controlled_bot_input(&mut control, 1, &motor, &action)
                .unwrap()
                .guard
        );
    }

    #[test]
    fn bot_selection_distance_ignores_height() {
        let cursor = Vec3::new(1.0, ARENA_TOP_Y + 5.0, -2.0);
        let fighter = Vec3::new(1.0, ARENA_TOP_Y, -2.0);

        assert_eq!(bot_selection_distance(cursor, fighter), 0.0);
        assert!(bot_selection_distance(cursor, fighter + Vec3::X) > FIGHTER_RADIUS);
    }

    #[test]
    fn bot_recovery_rolls_away_from_close_threat() {
        let decision = bot_recovery_decision(
            QUICK_STAND_AFTER,
            Vec3::ZERO,
            Some(target(Vec3::X * 2.0, -Vec3::X, FighterAction::HeavyAttack)),
        );

        assert_eq!(decision, BotRecoveryDecision::Roll(-Vec2::X));
        assert_eq!(
            bot_recovery_decision(QUICK_STAND_AFTER, Vec3::ZERO, None),
            BotRecoveryDecision::QuickStand
        );
    }

    #[test]
    fn training_dummy_bots_do_not_drive_autonomous_inputs() {
        assert!(!bot_should_drive_autonomous_inputs(
            BotBehaviorMode::TrainingDummy
        ));
        assert!(bot_should_drive_autonomous_inputs(
            BotBehaviorMode::Combatant
        ));
    }

    #[test]
    fn bot_guard_threat_requires_enemy_facing_bot() {
        let threatening = target(Vec3::X * 1.4, -Vec3::X, FighterAction::HeavyAttack);
        let turned_away = target(Vec3::X * 1.4, Vec3::X, FighterAction::HeavyAttack);
        let grab = target(Vec3::X, -Vec3::X, FighterAction::GrabStartup);

        assert!(bot_should_guard_threat(Vec3::ZERO, threatening));
        assert!(!bot_should_guard_threat(Vec3::ZERO, turned_away));
        assert!(!bot_should_guard_threat(Vec3::ZERO, grab));
    }

    #[test]
    fn bot_avoids_active_arena_hazard_only_when_pulsing() {
        let hazards = [ArenaHazardDefinition {
            kind: ArenaHazardKind::PulseVent,
            center: Vec3::new(0.0, ARENA_TOP_Y + 0.04, 0.0),
            radius: 1.0,
            pulse_seconds: 2.0,
            phase: 0.0,
        }];
        let position = Vec3::new(0.5, ARENA_TOP_Y, 0.0);

        assert!(arena_hazard_avoidance(position, 0.2, &hazards, 1.0).length_squared() > 0.01);
        assert_eq!(
            arena_hazard_avoidance(position, 1.2, &hazards, 1.0),
            Vec2::ZERO
        );
    }

    #[test]
    fn bot_avoidance_accounts_for_hazard_kind() {
        let pulse = ArenaHazardDefinition {
            kind: ArenaHazardKind::PulseVent,
            center: Vec3::ZERO,
            radius: 1.0,
            pulse_seconds: 2.0,
            phase: 0.0,
        };
        let bumper = ArenaHazardDefinition {
            kind: ArenaHazardKind::BumperNode,
            center: Vec3::ZERO,
            radius: 1.0,
            pulse_seconds: 2.0,
            phase: 0.0,
        };

        assert!(arena_hazard_avoid_radius(&bumper) > arena_hazard_avoid_radius(&pulse));
    }

    #[test]
    fn bot_range_bands_track_style_spacing() {
        let anchor = bot_range_band(
            style_tuning(crate::styles::FighterStyleKind::Anchor).bot_preferred_range,
            bot_personality(
                crate::styles::FighterStyleKind::Anchor,
                EquipmentKind::CounterCell,
            ),
        );
        let catalyst = bot_range_band(
            style_tuning(crate::styles::FighterStyleKind::Catalyst).bot_preferred_range,
            bot_personality(
                crate::styles::FighterStyleKind::Catalyst,
                EquipmentKind::CounterCell,
            ),
        );

        assert!(anchor.min < anchor.ideal && anchor.ideal < anchor.max);
        assert!(catalyst.ideal > anchor.ideal);
        assert!(catalyst.max > anchor.max);
    }

    #[test]
    fn bot_movement_plan_uses_range_and_edge_pressure() {
        let arena = crate::arena_defs::arena_definition(0);
        let personality = bot_personality(
            crate::styles::FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let range = bot_range_band(1.25, personality);

        assert_eq!(
            choose_bot_movement_plan_for_arena(
                Vec3::ZERO,
                Vec3::X * 4.0,
                range.max + 0.2,
                range,
                personality,
                100.0,
                0.0,
                1,
                arena,
            ),
            BotMovementPlan::Approach
        );
        assert_eq!(
            choose_bot_movement_plan_for_arena(
                Vec3::ZERO,
                Vec3::X,
                range.min - 0.1,
                range,
                personality,
                100.0,
                0.0,
                1,
                arena,
            ),
            BotMovementPlan::Backstep
        );
        assert_eq!(
            choose_bot_movement_plan_for_arena(
                Vec3::new(4.2, ARENA_TOP_Y, 4.2),
                Vec3::new(5.55, ARENA_TOP_Y, 5.55),
                range.ideal,
                range,
                personality,
                100.0,
                0.0,
                1,
                arena,
            ),
            BotMovementPlan::Pressure
        );
    }

    #[test]
    fn edge_steering_pulls_outward_motion_back_inward() {
        let arena = crate::arena_defs::arena_definition(0);
        let outward = Vec2::splat(std::f32::consts::FRAC_1_SQRT_2);
        let edge = outward * (ARENA_RADIUS - 0.1);
        let position = Vec3::new(edge.x, ARENA_TOP_Y, edge.y);
        let steered = apply_edge_steering_for_arena(position, outward, arena);

        assert!(edge_danger_for_arena(position, arena) > 0.8);
        assert!(steered.dot(outward) < 0.0);
    }

    #[test]
    fn dash_planning_rejects_edgeward_dashes() {
        let arena = crate::arena_defs::arena_definition(0);
        let personality = bot_personality(
            crate::styles::FighterStyleKind::Vector,
            EquipmentKind::DashCoil,
        );
        let range = bot_range_band(1.55, personality);

        assert!(bot_should_dash_for_movement_for_arena(
            BotMovementPlan::Approach,
            Vec3::ZERO,
            Vec2::X,
            range.max + 1.0,
            range,
            true,
            0.0,
            arena,
        ));
        let outward = Vec2::splat(std::f32::consts::FRAC_1_SQRT_2);
        let edge = outward * (ARENA_RADIUS - 0.2);
        assert!(!bot_should_dash_for_movement_for_arena(
            BotMovementPlan::Approach,
            Vec3::new(edge.x, ARENA_TOP_Y, edge.y),
            outward,
            range.max + 1.0,
            range,
            true,
            0.0,
            arena,
        ));
    }

    #[test]
    fn bot_personality_varies_by_style_and_equipment() {
        let anchor_counter = bot_personality(
            crate::styles::FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let vector_dash = bot_personality(
            crate::styles::FighterStyleKind::Vector,
            EquipmentKind::DashCoil,
        );
        let catalyst_heavy = bot_personality(
            crate::styles::FighterStyleKind::Catalyst,
            EquipmentKind::HeavySeal,
        );

        assert!(vector_dash.aggression > anchor_counter.aggression);
        assert!(anchor_counter.hazard_fear > vector_dash.hazard_fear);
        assert!(catalyst_heavy.special_bias > anchor_counter.special_bias);
        assert!(
            catalyst_heavy.item_greed
                > bot_personality(
                    crate::styles::FighterStyleKind::Catalyst,
                    EquipmentKind::CounterCell
                )
                .item_greed
        );
    }

    #[test]
    fn bot_mistake_and_panic_helpers_are_bounded() {
        let personality = bot_personality(
            crate::styles::FighterStyleKind::Vector,
            EquipmentKind::DashCoil,
        );
        assert!(bot_should_panic(personality.panic_health, personality));
        assert!(!bot_should_panic(
            personality.panic_health + 1.0,
            personality
        ));
        assert!(!bot_should_make_mistake(0.0, 2, personality));
        assert!(personality.mistake_rate > 0.0 && personality.mistake_rate < 0.25);
    }

    #[test]
    fn bot_uses_mp_food_only_when_stamina_is_missing() {
        assert_eq!(
            bot_held_item_decision(
                ItemKind::WineWhite,
                MAX_STAMINA - ITEM_BREEZE_BUOY_STAMINA * 0.5,
                2.0,
                0.0,
                0.0
            ),
            Some(BotHeldItemDecision::Light)
        );
        assert_eq!(
            bot_held_item_decision(ItemKind::WineWhite, MAX_STAMINA, 2.0, 0.0, 0.0),
            None
        );
    }

    #[test]
    fn bot_mushroom_uses_buff_directly() {
        assert_eq!(
            bot_held_item_decision(ItemKind::Mushroom, 100.0, 1.8, 0.0, 1.0),
            Some(BotHeldItemDecision::Light)
        );
    }

    #[test]
    fn bot_pickup_score_uses_item_role_context() {
        assert!(
            bot_pickup_score(ItemKind::Steamer, MAX_STAMINA, 4.0, 0.4)
                > bot_pickup_score(ItemKind::Apple, MAX_STAMINA, 4.0, 0.4)
        );
        assert!(
            bot_pickup_score(
                ItemKind::CupCoffee,
                MAX_STAMINA - ITEM_BREEZE_BUOY_STAMINA * 0.6,
                2.0,
                0.4
            ) > bot_pickup_score(
                ItemKind::Apple,
                MAX_STAMINA - ITEM_BREEZE_BUOY_STAMINA * 0.6,
                4.0,
                0.4
            )
        );
    }
}
