//! Lossless bridge between live fighter ECS components and canonical snapshots.
//!
//! Fighter entities are first indexed into the fixed [`FighterId`] domain. No
//! capture or restore decision depends on Bevy entity allocation/query order.
//! Restore is split into a read-only, fallible preflight and an infallible commit
//! so malformed state cannot partially mutate the world.

use bevy::prelude::*;

use crate::characters::{CharacterKind, FighterCharacter};
use crate::components::{
    DrunkStatus, Fighter, FighterAction, FighterActionState, FighterGrabState, FighterInput,
    FighterInventory, FighterMotor, FighterSpecialState, FighterStats, FighterUltimateState,
    SimPosition,
};
use crate::determinism::{
    DEFAULT_F32_QUANTIZATION, FIGHTER_CAPACITY, FighterId, SimEntityKind, canonicalize_f32,
};
use crate::equipment::{EquipmentKind, FighterEquipment};
use crate::game_state::MatchState;
use crate::reactions::{QueuedAftermath, ReactionFamilyId, queued_aftermath_presentation_cue};
use crate::simulation::{ElapsedTicks, TickTimer};
use crate::snapshot::{
    COOLDOWN_SLOTS, DAMAGE_ELEMENT_CODE_COUNT, EQUIPMENT_SLOTS, F32Vec2BitsSnapshot,
    F32Vec3BitsSnapshot, FIGHTER_ACTION_BRANCH_WINDOW_OPEN, FIGHTER_ACTION_CANCEL_WINDOW_OPEN,
    FIGHTER_ACTION_CHARGE_RELEASE_REQUESTED, FIGHTER_ACTION_CODE_COUNT,
    FIGHTER_ACTION_CONFIRMED_HIT, FIGHTER_ACTION_HITBOX_SPAWNED, FIGHTER_ACTION_QUEUED_COMBO,
    FIGHTER_MOTOR_AIR_ATTACK_USED, FIGHTER_MOTOR_BEE_AIR_DASH_MOTION_ACTIVE,
    FIGHTER_MOTOR_BEE_AIR_DASH_SHOT_AVAILABLE, FIGHTER_MOTOR_GUARD_COUNTER_BUFFERED,
    FIGHTER_MOTOR_GUARD_WAS_REQUESTED, FIGHTER_MOTOR_JUMP_ATTACK_LANDING_RECOVERY,
    FIGHTER_MOTOR_KNOCKDOWN_ON_LAND, FighterActionRollbackSnapshot, FighterActionSnapshot,
    FighterCooldownSnapshot, FighterInputSnapshot, FighterLoadoutSnapshot,
    FighterMotorRollbackSnapshot, FighterPoseSnapshot, FighterRelationshipsSnapshot,
    FighterRollbackExtensionSnapshot, FighterSnapshot, FighterStatsRollbackSnapshot,
    FighterStatusSnapshot, OptionalF32Vec3BitsSnapshot, OptionalU8CodeSnapshot,
    OptionalU16CodeSnapshot, OptionalU32Snapshot, QuantizedVec2, QuantizedVec3,
    QueuedAftermathSnapshot, REACTION_FAMILY_CODE_COUNT, STATUS_TIMER_SLOTS,
    TECHNIQUE_BUTTON_CODE_COUNT, TECHNIQUE_CODE_COUNT,
};
use crate::snapshot_ecs::{FighterSnapshotCodec, SnapshotCodecError};
use crate::styles::{FighterStyle, FighterStyleKind};
use crate::techniques::{DamageElement, TechniqueButton, TechniqueId};

const ERR_INVALID_LIVE_ID: u16 = 1;
const ERR_DUPLICATE_LIVE_ID: u16 = 2;
const ERR_MISSING_LIVE_SLOT: u16 = 3;
const ERR_MISSING_COMPONENT: u16 = 4;
const ERR_INVALID_LIVE_VALUE: u16 = 5;
const ERR_INVALID_SNAPSHOT_SLOT: u16 = 6;
const ERR_INVALID_ENUM: u16 = 7;
const ERR_INVALID_RELATIONSHIP: u16 = 8;
const ERR_NON_CANONICAL_SUMMARY: u16 = 9;
const ERR_MISSING_MATCH_STATE: u16 = 10;

const SPECIAL_COOLDOWN_SLOT: usize = 0;
const EQUIPMENT_COOLDOWN_SLOT: usize = 1;
const DRUNK_STATUS_TIMER_SLOT: usize = 6;

const INPUT_AIM: u32 = 1 << 0;
const INPUT_JUMP: u32 = 1 << 1;
const INPUT_DASH: u32 = 1 << 2;
const INPUT_LIGHT: u32 = 1 << 3;
const INPUT_LIGHT_HELD: u32 = 1 << 4;
const INPUT_RAW_LIGHT_PRESSED: u32 = 1 << 5;
const INPUT_HEAVY: u32 = 1 << 6;
const INPUT_HEAVY_HELD: u32 = 1 << 7;
const INPUT_RAW_HEAVY_PRESSED: u32 = 1 << 8;
const INPUT_HEAVY_RELEASED: u32 = 1 << 9;
const INPUT_GRAB: u32 = 1 << 10;
const INPUT_GUARD: u32 = 1 << 11;
const INPUT_ULTIMATE: u32 = 1 << 12;
const INPUT_SPECIAL: u32 = 1 << 13;
const INPUT_FLAG_MASK: u32 = (1 << 14) - 1;

const FIGHTER_ACTIONS: [FighterAction; FIGHTER_ACTION_CODE_COUNT as usize] = [
    FighterAction::Idle,
    FighterAction::Moving,
    FighterAction::Jumping,
    FighterAction::Dashing,
    FighterAction::DashAttack,
    FighterAction::JumpAttack,
    FighterAction::JumpHeavyAttack,
    FighterAction::LandingRecovery,
    FighterAction::LightAttack1,
    FighterAction::LightAttack2,
    FighterAction::ComboFinisher,
    FighterAction::HeavyAttack,
    FighterAction::HeavyAttack2,
    FighterAction::UltimateStartup,
    FighterAction::UltimateRush,
    FighterAction::UltimateVictim,
    FighterAction::GrabStartup,
    FighterAction::GrabHold,
    FighterAction::Grabbed,
    FighterAction::Throwing,
    FighterAction::SpecialCast,
    FighterAction::ItemPickup,
    FighterAction::ItemSwing,
    FighterAction::ItemThrow,
    FighterAction::ItemDrop,
    FighterAction::Guarding,
    FighterAction::GuardCounter,
    FighterAction::GuardStep,
    FighterAction::Hitstun,
    FighterAction::Knockdown,
    FighterAction::QuickStand,
    FighterAction::RecoveryRoll,
    FighterAction::GetUp,
    FighterAction::GuardBroken,
    FighterAction::RingOut,
    FighterAction::Respawning,
];

const TECHNIQUES: [TechniqueId; TECHNIQUE_CODE_COUNT as usize] = [
    TechniqueId::CatLight1,
    TechniqueId::CatLight2,
    TechniqueId::CatComboFinisher,
    TechniqueId::CatDashComboFinisher,
    TechniqueId::CatHeavy,
    TechniqueId::CatHeavy2,
    TechniqueId::CatDashAttack,
    TechniqueId::CatJumpAttack,
    TechniqueId::CatJumpHeavy,
    TechniqueId::CatUltimateStartup,
    TechniqueId::CatUltimateRush,
    TechniqueId::PigLight1,
    TechniqueId::PigLight2,
    TechniqueId::PigComboFinisher,
    TechniqueId::PigHeavy,
    TechniqueId::PigHeavy2,
    TechniqueId::PigDashAttack,
    TechniqueId::PigJumpAttack,
    TechniqueId::PigJumpHeavy,
    TechniqueId::PigUltimateStartup,
    TechniqueId::PigUltimateRush,
    TechniqueId::DogLight1,
    TechniqueId::DogLight2,
    TechniqueId::DogComboFinisher,
    TechniqueId::DogHeavy,
    TechniqueId::DogHeavy2,
    TechniqueId::DogDashAttack,
    TechniqueId::DogJumpAttack,
    TechniqueId::DogJumpHeavy,
    TechniqueId::DogUltimateStartup,
    TechniqueId::DogUltimateRush,
    TechniqueId::FoxLight1,
    TechniqueId::FoxLight2,
    TechniqueId::FoxComboFinisher,
    TechniqueId::FoxHeavy,
    TechniqueId::FoxHeavy2,
    TechniqueId::FoxDashAttack,
    TechniqueId::FoxJumpAttack,
    TechniqueId::FoxJumpHeavy,
    TechniqueId::FoxUltimateStartup,
    TechniqueId::FoxUltimateRush,
    TechniqueId::PandaLight1,
    TechniqueId::PandaLight2,
    TechniqueId::PandaComboFinisher,
    TechniqueId::PandaHeavy,
    TechniqueId::PandaHeavy2,
    TechniqueId::PandaDashAttack,
    TechniqueId::PandaJumpAttack,
    TechniqueId::PandaJumpHeavy,
    TechniqueId::PandaUltimateStartup,
    TechniqueId::PandaUltimateRush,
    TechniqueId::BeeLight1,
    TechniqueId::BeeLight2,
    TechniqueId::BeeComboFinisher,
    TechniqueId::BeeHeavy,
    TechniqueId::BeeHeavy2,
    TechniqueId::BeeDashAttack,
    TechniqueId::BeeJumpAttack,
    TechniqueId::BeeJumpHeavy,
    TechniqueId::BeeUltimateStartup,
    TechniqueId::BeeLegacyUltimateStartup,
    TechniqueId::BeeLegacyUltimateRush,
    TechniqueId::PenguinLight1,
    TechniqueId::PenguinLight2,
    TechniqueId::PenguinComboFinisher,
    TechniqueId::PenguinHeavy,
    TechniqueId::PenguinHeavy2,
    TechniqueId::PenguinDashAttack,
    TechniqueId::PenguinDashHeavy,
    TechniqueId::PenguinJumpAttack,
    TechniqueId::PenguinJumpHeavy,
    TechniqueId::PenguinUltimateStartup,
    TechniqueId::PenguinUltimateRush,
    TechniqueId::ChickLight1,
    TechniqueId::ChickLight2,
    TechniqueId::ChickComboFinisher,
    TechniqueId::ChickHeavy,
    TechniqueId::ChickHeavy2,
    TechniqueId::ChickDashAttack,
    TechniqueId::ChickDashHeavy,
    TechniqueId::ChickJumpAttack,
    TechniqueId::ChickJumpHeavy,
    TechniqueId::ChickUltimateStartup,
    TechniqueId::Grab,
    TechniqueId::Throw,
    TechniqueId::GuardCounter,
    TechniqueId::SpecialCast,
    TechniqueId::ItemPickup,
    TechniqueId::ItemSwing,
    TechniqueId::ItemThrow,
    TechniqueId::ItemDrop,
    TechniqueId::GuardStep,
    TechniqueId::QuickStand,
    TechniqueId::RecoveryRoll,
    TechniqueId::LandingRecovery,
];

const TECHNIQUE_BUTTONS: [TechniqueButton; TECHNIQUE_BUTTON_CODE_COUNT as usize] = [
    TechniqueButton::A,
    TechniqueButton::B,
    TechniqueButton::AB,
    TechniqueButton::Grab,
    TechniqueButton::Dash,
    TechniqueButton::Jump,
    TechniqueButton::Item,
    TechniqueButton::Special,
    TechniqueButton::Ultimate,
];

const REACTION_FAMILIES: [ReactionFamilyId; REACTION_FAMILY_CODE_COUNT as usize] = [
    ReactionFamilyId::ShortStandingStagger,
    ReactionFamilyId::MediumStandingStagger,
    ReactionFamilyId::HeavyStandingStagger,
    ReactionFamilyId::FrozenStun,
    ReactionFamilyId::LauncherDown,
    ReactionFamilyId::GroundedDownGetup,
    ReactionFamilyId::SlidingKnockdown,
    ReactionFamilyId::LightAirPop,
    ReactionFamilyId::CounterPop,
    ReactionFamilyId::GroundBounceDown,
    ReactionFamilyId::AerialSpikeDown,
    ReactionFamilyId::AirFishKnockdown,
    ReactionFamilyId::UltimateLockedStagger,
    ReactionFamilyId::UltimateBombDown,
];

const DAMAGE_ELEMENTS: [DamageElement; DAMAGE_ELEMENT_CODE_COUNT as usize] = [
    DamageElement::Neutral,
    DamageElement::Strike,
    DamageElement::Launch,
    DamageElement::Shock,
    DamageElement::Wind,
    DamageElement::Earth,
    DamageElement::Hazard,
    DamageElement::Blast,
];

const CHARACTER_KINDS: [CharacterKind; 8] = [
    CharacterKind::Cat,
    CharacterKind::Pig,
    CharacterKind::Dog,
    CharacterKind::Fox,
    CharacterKind::Panda,
    CharacterKind::Bee,
    CharacterKind::Penguin,
    CharacterKind::Chick,
];

const STYLE_KINDS: [FighterStyleKind; 3] = [
    FighterStyleKind::Anchor,
    FighterStyleKind::Vector,
    FighterStyleKind::Catalyst,
];

const EQUIPMENT_KINDS: [EquipmentKind; 4] = [
    EquipmentKind::DashCoil,
    EquipmentKind::AerialSpur,
    EquipmentKind::CounterCell,
    EquipmentKind::HeavySeal,
];

fn code_in<T: Copy + PartialEq>(value: T, table: &[T]) -> u16 {
    table
        .iter()
        .position(|candidate| *candidate == value)
        .expect("the explicit snapshot enum table contains every variant") as u16
}

pub fn fighter_action_code(value: FighterAction) -> u16 {
    code_in(value, &FIGHTER_ACTIONS)
}

pub fn fighter_action_from_code(code: u16) -> Option<FighterAction> {
    FIGHTER_ACTIONS.get(code as usize).copied()
}

pub fn technique_code(value: TechniqueId) -> u16 {
    code_in(value, &TECHNIQUES)
}

pub fn technique_from_code(code: u16) -> Option<TechniqueId> {
    TECHNIQUES.get(code as usize).copied()
}

pub fn technique_button_code(value: TechniqueButton) -> u8 {
    code_in(value, &TECHNIQUE_BUTTONS) as u8
}

pub fn technique_button_from_code(code: u8) -> Option<TechniqueButton> {
    TECHNIQUE_BUTTONS.get(code as usize).copied()
}

pub fn reaction_family_code(value: ReactionFamilyId) -> u8 {
    code_in(value, &REACTION_FAMILIES) as u8
}

pub fn reaction_family_from_code(code: u8) -> Option<ReactionFamilyId> {
    REACTION_FAMILIES.get(code as usize).copied()
}

pub fn damage_element_code(value: DamageElement) -> u8 {
    code_in(value, &DAMAGE_ELEMENTS) as u8
}

pub fn damage_element_from_code(code: u8) -> Option<DamageElement> {
    DAMAGE_ELEMENTS.get(code as usize).copied()
}

pub fn character_code(value: CharacterKind) -> u16 {
    code_in(value, &CHARACTER_KINDS)
}

pub fn character_from_code(code: u16) -> Option<CharacterKind> {
    CHARACTER_KINDS.get(code as usize).copied()
}

pub fn style_code(value: FighterStyleKind) -> u16 {
    code_in(value, &STYLE_KINDS)
}

pub fn style_from_code(code: u16) -> Option<FighterStyleKind> {
    STYLE_KINDS.get(code as usize).copied()
}

pub fn equipment_code(value: EquipmentKind) -> u16 {
    code_in(value, &EQUIPMENT_KINDS)
}

pub fn equipment_from_code(code: u16) -> Option<EquipmentKind> {
    EQUIPMENT_KINDS.get(code as usize).copied()
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LiveFighterSnapshotCodec;

pub struct LiveFighterRestorePlan {
    fighters: [PreparedFighter; FIGHTER_CAPACITY as usize],
}

struct PreparedFighter {
    entity: Entity,
    transform_translation: Vec3,
    spawn: Vec3,
    input: FighterInput,
    stats: FighterStats,
    motor: FighterMotor,
    action: FighterActionState,
    drunk: DrunkStatus,
    inventory: FighterInventory,
    grab: FighterGrabState,
    ultimate: FighterUltimateState,
    special: FighterSpecialState,
    character: FighterCharacter,
    style: FighterStyle,
    equipment: FighterEquipment,
}

impl FighterSnapshotCodec for LiveFighterSnapshotCodec {
    type RestorePlan = LiveFighterRestorePlan;

    fn capture_fighters(
        &self,
        world: &World,
    ) -> Result<[FighterSnapshot; FIGHTER_CAPACITY as usize], SnapshotCodecError> {
        let entities = fighter_entities_by_id(world)?;
        let active_slots = world
            .get_resource::<MatchState>()
            .ok_or(SnapshotCodecError::new(
                ERR_MISSING_MATCH_STATE,
                "live fighter snapshot requires MatchState",
            ))?
            .active_slots;
        let mut snapshots = FighterId::ALL.map(FighterSnapshot::empty);
        for id in FighterId::ALL {
            let entity = entities[id.index()];
            ensure_required_components(world, entity)?;
            if active_slots[id.index()] {
                snapshots[id.index()] = capture_fighter(world, entity, id)?;
            }
        }
        validate_relationships(&snapshots, active_slots)?;
        Ok(snapshots)
    }

    fn prepare_restore(
        &self,
        world: &World,
        fighters: &[FighterSnapshot; FIGHTER_CAPACITY as usize],
    ) -> Result<Self::RestorePlan, SnapshotCodecError> {
        let entities = fighter_entities_by_id(world)?;
        // Restore preparation runs before the incoming non-fighter plan commits.
        // The destination world's MatchState can therefore describe a different
        // roster (for example across reconnect or match reset). Derive the roster
        // from the already validated incoming fighter section instead of stale
        // destination state.
        let active_slots = fighters.map(|fighter| fighter.active);
        validate_relationships(fighters, active_slots)?;
        let mut prepared: [Option<PreparedFighter>; FIGHTER_CAPACITY as usize] =
            std::array::from_fn(|_| None);
        for id in FighterId::ALL {
            let entity = entities[id.index()];
            prepared[id.index()] = Some(if active_slots[id.index()] {
                prepare_fighter(world, entity, id, &fighters[id.index()])?
            } else {
                prepare_inactive_fighter(world, entity)?
            });
        }
        Ok(LiveFighterRestorePlan {
            fighters: prepared.map(|fighter| {
                fighter.expect("every fixed fighter slot was prepared before plan construction")
            }),
        })
    }

    fn commit_restore(&self, world: &mut World, plan: Self::RestorePlan) {
        for prepared in plan.fighters {
            commit_fighter(world, prepared);
        }
    }
}

fn fighter_entities_by_id(
    world: &World,
) -> Result<[Entity; FIGHTER_CAPACITY as usize], SnapshotCodecError> {
    let mut slots: [Option<Entity>; FIGHTER_CAPACITY as usize] = [None; FIGHTER_CAPACITY as usize];
    for archetype in world.archetypes().iter() {
        for entry in archetype.entities() {
            let entity = entry.id();
            let Some(fighter) = world.get::<Fighter>(entity) else {
                continue;
            };
            let Some(id) = FighterId::from_index(fighter.id) else {
                return Err(SnapshotCodecError::new(
                    ERR_INVALID_LIVE_ID,
                    "live Fighter.id is outside the fixed fighter domain",
                ));
            };
            if slots[id.index()].replace(entity).is_some() {
                return Err(SnapshotCodecError::new(
                    ERR_DUPLICATE_LIVE_ID,
                    "multiple live fighter entities use the same FighterId",
                ));
            }
        }
    }
    if slots.iter().any(Option::is_none) {
        return Err(SnapshotCodecError::new(
            ERR_MISSING_LIVE_SLOT,
            "the live world is missing a fixed fighter slot",
        ));
    }
    Ok(slots.map(|slot| slot.expect("all fighter slots were checked above")))
}

fn required<'w, T: Component>(
    world: &'w World,
    entity: Entity,
) -> Result<&'w T, SnapshotCodecError> {
    world.get::<T>(entity).ok_or(SnapshotCodecError::new(
        ERR_MISSING_COMPONENT,
        "live fighter is missing a required authoritative component",
    ))
}

fn ensure_required_components(world: &World, entity: Entity) -> Result<(), SnapshotCodecError> {
    required::<Fighter>(world, entity)?;
    required::<SimPosition>(world, entity)?;
    required::<FighterInput>(world, entity)?;
    required::<FighterStats>(world, entity)?;
    required::<FighterMotor>(world, entity)?;
    required::<FighterActionState>(world, entity)?;
    required::<DrunkStatus>(world, entity)?;
    required::<FighterInventory>(world, entity)?;
    required::<FighterGrabState>(world, entity)?;
    required::<FighterUltimateState>(world, entity)?;
    required::<FighterSpecialState>(world, entity)?;
    required::<FighterCharacter>(world, entity)?;
    required::<FighterStyle>(world, entity)?;
    required::<FighterEquipment>(world, entity)?;
    Ok(())
}

fn capture_fighter(
    world: &World,
    entity: Entity,
    id: FighterId,
) -> Result<FighterSnapshot, SnapshotCodecError> {
    let fighter = required::<Fighter>(world, entity)?;
    let position = required::<SimPosition>(world, entity)?;
    let input = required::<FighterInput>(world, entity)?;
    let stats = required::<FighterStats>(world, entity)?;
    let motor = required::<FighterMotor>(world, entity)?;
    let action = required::<FighterActionState>(world, entity)?;
    let drunk = required::<DrunkStatus>(world, entity)?;
    let inventory = required::<FighterInventory>(world, entity)?;
    let grab = required::<FighterGrabState>(world, entity)?;
    let ultimate = required::<FighterUltimateState>(world, entity)?;
    let special = required::<FighterSpecialState>(world, entity)?;
    let character = required::<FighterCharacter>(world, entity)?;
    let style = required::<FighterStyle>(world, entity)?;
    let equipment = required::<FighterEquipment>(world, entity)?;

    if fighter.id != id.index() {
        return Err(SnapshotCodecError::new(
            ERR_INVALID_LIVE_ID,
            "indexed fighter identity changed during capture",
        ));
    }
    if inventory
        .held
        .is_some_and(|held| held.kind() != SimEntityKind::Item)
    {
        return Err(SnapshotCodecError::new(
            ERR_INVALID_RELATIONSHIP,
            "live held-item relationship does not reference the item pool",
        ));
    }
    let last_attacker = stats.last_attacker;

    let input_flags = encode_input_flags(input);
    let action_flags = encode_action_flags(action);
    let motor_flags = encode_motor_flags(motor);
    let element_carry = optional_u8(stats.element_carry.map(damage_element_code));
    let character_id = character_code(character.kind);
    let style_id = style_code(style.kind);
    let equipment_id = equipment_code(equipment.kind);

    let stats_rollback = FighterStatsRollbackSnapshot {
        health_bits: stats.health.to_bits(),
        stamina_bits: stats.stamina.to_bits(),
        invulnerability_ticks: stats.invulnerability.remaining(),
        health_refill_ticks: stats.health_refill_timer.remaining(),
        respawn_ticks: stats.respawn_timer.remaining(),
        element_carry,
        element_carry_strength_bits: stats.element_carry_strength.to_bits(),
        element_carry_ticks: stats.element_carry_timer.remaining(),
        item_speed_ticks: stats.item_speed_timer.remaining(),
        item_giant_ticks: stats.item_giant_timer.remaining(),
    };
    let motor_rollback = FighterMotorRollbackSnapshot {
        velocity: vec3_bits(motor.velocity),
        facing: vec3_bits(motor.facing),
        flags: motor_flags,
        landing_aftermath: encode_landing_aftermath(motor.landing_aftermath),
        queued_air_attack: optional_u8(motor.queued_air_attack.map(technique_button_code)),
        queued_air_attack_ticks: motor.queued_air_attack_timer.remaining(),
        ledge_grace_ticks: motor.ledge_grace_timer.remaining(),
        landing_stick_ticks: motor.landing_stick_timer.remaining(),
        jump_takeoff_ticks: motor.jump_takeoff_timer.remaining(),
        reaction_bounces: motor.reaction_bounces,
        pig_air_meat_slam_air_hits: motor.pig_air_meat_slam_air_hits,
        dash_slide_ticks: motor.dash_slide_timer.remaining(),
        dash_jump_carry_ticks: motor.dash_jump_carry_timer.remaining(),
        dash_jump_carry_speed_limit_bits: motor.dash_jump_carry_speed_limit.to_bits(),
        impact_speed_limit_ticks: motor.impact_speed_limit_timer.remaining(),
        impact_speed_limit_bits: motor.impact_speed_limit.to_bits(),
        penguin_ice_slide_direction: optional_vec3_bits(motor.penguin_ice_slide_direction),
        penguin_ice_slide_speed_bits: motor.penguin_ice_slide_speed.to_bits(),
        guard_active_elapsed_ticks: motor.guard_active_timer.get(),
        guard_cooldown_ticks: motor.guard_cooldown_timer.remaining(),
        guard_start_buffer_ticks: motor.guard_start_buffer_timer.remaining(),
        guard_counter_window_ticks: motor.guard_counter_window_timer.remaining(),
        guard_counter_source: optional_vec3_bits(motor.guard_counter_source),
    };
    let action_rollback = FighterActionRollbackSnapshot {
        flags: action_flags,
        queued_technique: optional_u16(action.queued_technique.map(technique_code)),
        queued_button: optional_u8(action.queued_button.map(technique_button_code)),
        buffered_button: optional_u8(action.buffered_button.map(technique_button_code)),
        buffered_button_elapsed_ticks: action.buffered_button_elapsed.get(),
        technique_id: optional_u16(action.technique_id.map(technique_code)),
        timeline_events_fired: action.timeline_events_fired,
        reaction_getup_ms: optional_u32(action.reaction_getup_ms),
        reaction_recover_ms: optional_u32(action.reaction_recover_ms),
        reaction_family: optional_u8(action.reaction_family.map(reaction_family_code)),
        charge_elapsed_ticks: action.charge_elapsed.get(),
    };
    let rollback = FighterRollbackExtensionSnapshot {
        position: vec3_bits(position.translation),
        input_movement: F32Vec2BitsSnapshot {
            x: input.movement.x.to_bits(),
            y: input.movement.y.to_bits(),
        },
        spawn: vec3_bits(fighter.spawn),
        stats: stats_rollback,
        motor: motor_rollback,
        action: action_rollback,
        regrab_lockout_ticks: grab.regrab_lockout.remaining(),
    };
    rollback.validate().map_err(|_| {
        SnapshotCodecError::new(
            ERR_INVALID_LIVE_VALUE,
            "live fighter contains non-finite or otherwise non-canonical state",
        )
    })?;
    validate_canonical_rollback(rollback)?;

    let mut cooldown_ticks = [0; COOLDOWN_SLOTS];
    cooldown_ticks[SPECIAL_COOLDOWN_SLOT] = special.cooldown.remaining();
    cooldown_ticks[EQUIPMENT_COOLDOWN_SLOT] = equipment.cooldown.remaining();
    let mut status_timers = [0; STATUS_TIMER_SLOTS];
    status_timers[0] = stats.invulnerability.remaining();
    status_timers[1] = stats.health_refill_timer.remaining();
    status_timers[2] = stats.respawn_timer.remaining();
    status_timers[3] = stats.element_carry_timer.remaining();
    status_timers[4] = stats.item_speed_timer.remaining();
    status_timers[5] = stats.item_giant_timer.remaining();
    status_timers[DRUNK_STATUS_TIMER_SLOT] = drunk.remaining.remaining();
    let mut equipment_ids = [0; EQUIPMENT_SLOTS];
    equipment_ids[0] = equipment_id;

    Ok(FighterSnapshot {
        occupied: true,
        active: true,
        id,
        input: FighterInputSnapshot {
            move_x: input_axis_summary(input.movement.x),
            move_y: input_axis_summary(input.movement.y),
            held_buttons: input_flags,
            pressed_latches: 0,
            released_latches: 0,
        },
        pose: FighterPoseSnapshot {
            position: quantized_vec3(position.translation),
            velocity: quantized_vec3(motor.velocity),
            facing: QuantizedVec2 {
                x: quantize(motor.facing.x),
                z: quantize(motor.facing.z),
            },
            grounded: motor.grounded,
            collision_flags: 0,
        },
        health: quantize(stats.health),
        stamina: quantize(stats.stamina),
        score: stats.score,
        action: FighterActionSnapshot {
            action_id: fighter_action_code(action.action),
            elapsed_ticks: action.elapsed.get(),
            flags: u32::from(action_flags),
            buffered_action_id: optional_u8_summary(
                action.buffered_button.map(technique_button_code),
            ),
            reaction_id: optional_u8_summary(action.reaction_family.map(reaction_family_code)),
            reaction_ticks: 0,
        },
        cooldowns: FighterCooldownSnapshot {
            ticks: cooldown_ticks,
        },
        status: FighterStatusSnapshot {
            flags: 0,
            timers: status_timers,
            elemental_carry: stats
                .element_carry
                .map_or(-1, |element| i32::from(damage_element_code(element))),
            size_scale: quantize(stats.item_size_multiplier()),
            speed_scale: quantize(stats.item_speed_multiplier()),
        },
        loadout: FighterLoadoutSnapshot {
            character_id,
            style_id,
            move_set_id: character_id,
            equipment_ids,
        },
        relationships: FighterRelationshipsSnapshot {
            held_item: inventory.held,
            linked_entity: None,
            holding: grab.holding,
            held_by: grab.held_by,
            ultimate_owner: ultimate.owner,
            ultimate_target: ultimate.target,
            last_attacker,
        },
        rollback,
    })
}

fn quantize(value: f32) -> i32 {
    DEFAULT_F32_QUANTIZATION.quantize(value)
}

fn quantized_vec3(value: Vec3) -> QuantizedVec3 {
    QuantizedVec3 {
        x: quantize(value.x),
        y: quantize(value.y),
        z: quantize(value.z),
    }
}

fn input_axis_summary(value: f32) -> i16 {
    quantize(value).clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

fn vec3_bits(value: Vec3) -> F32Vec3BitsSnapshot {
    F32Vec3BitsSnapshot {
        x: value.x.to_bits(),
        y: value.y.to_bits(),
        z: value.z.to_bits(),
    }
}

fn optional_u8(value: Option<u8>) -> OptionalU8CodeSnapshot {
    OptionalU8CodeSnapshot {
        present: value.is_some(),
        code: value.unwrap_or(0),
    }
}

fn optional_u16(value: Option<u16>) -> OptionalU16CodeSnapshot {
    OptionalU16CodeSnapshot {
        present: value.is_some(),
        code: value.unwrap_or(0),
    }
}

fn optional_u32(value: Option<u32>) -> OptionalU32Snapshot {
    OptionalU32Snapshot {
        present: value.is_some(),
        value: value.unwrap_or(0),
    }
}

fn optional_vec3_bits(value: Option<Vec3>) -> OptionalF32Vec3BitsSnapshot {
    OptionalF32Vec3BitsSnapshot {
        present: value.is_some(),
        value: value.map_or_else(F32Vec3BitsSnapshot::default, vec3_bits),
    }
}

fn optional_u8_summary(value: Option<u8>) -> u16 {
    value.map_or(0, |code| u16::from(code) + 1)
}

fn encode_input_flags(input: &FighterInput) -> u32 {
    u32::from(input.aim) * INPUT_AIM
        | u32::from(input.jump) * INPUT_JUMP
        | u32::from(input.dash) * INPUT_DASH
        | u32::from(input.light) * INPUT_LIGHT
        | u32::from(input.light_held) * INPUT_LIGHT_HELD
        | u32::from(input.raw_light_pressed) * INPUT_RAW_LIGHT_PRESSED
        | u32::from(input.heavy) * INPUT_HEAVY
        | u32::from(input.heavy_held) * INPUT_HEAVY_HELD
        | u32::from(input.raw_heavy_pressed) * INPUT_RAW_HEAVY_PRESSED
        | u32::from(input.heavy_released) * INPUT_HEAVY_RELEASED
        | u32::from(input.grab) * INPUT_GRAB
        | u32::from(input.guard) * INPUT_GUARD
        | u32::from(input.ultimate) * INPUT_ULTIMATE
        | u32::from(input.special) * INPUT_SPECIAL
}

fn encode_motor_flags(motor: &FighterMotor) -> u16 {
    u16::from(motor.knockdown_on_land) * FIGHTER_MOTOR_KNOCKDOWN_ON_LAND
        | u16::from(motor.air_attack_used) * FIGHTER_MOTOR_AIR_ATTACK_USED
        | u16::from(motor.jump_attack_landing_recovery) * FIGHTER_MOTOR_JUMP_ATTACK_LANDING_RECOVERY
        | u16::from(motor.bee_air_dash_motion_active) * FIGHTER_MOTOR_BEE_AIR_DASH_MOTION_ACTIVE
        | u16::from(motor.bee_air_dash_shot_available) * FIGHTER_MOTOR_BEE_AIR_DASH_SHOT_AVAILABLE
        | u16::from(motor.guard_was_requested) * FIGHTER_MOTOR_GUARD_WAS_REQUESTED
        | u16::from(motor.guard_counter_buffered) * FIGHTER_MOTOR_GUARD_COUNTER_BUFFERED
}

fn encode_action_flags(action: &FighterActionState) -> u8 {
    u8::from(action.hitbox_spawned) * FIGHTER_ACTION_HITBOX_SPAWNED
        | u8::from(action.queued_combo) * FIGHTER_ACTION_QUEUED_COMBO
        | u8::from(action.confirmed_hit) * FIGHTER_ACTION_CONFIRMED_HIT
        | u8::from(action.cancel_window_open) * FIGHTER_ACTION_CANCEL_WINDOW_OPEN
        | u8::from(action.branch_window_open) * FIGHTER_ACTION_BRANCH_WINDOW_OPEN
        | u8::from(action.charge_release_requested) * FIGHTER_ACTION_CHARGE_RELEASE_REQUESTED
}

fn encode_landing_aftermath(value: Option<QueuedAftermath>) -> QueuedAftermathSnapshot {
    value.map_or_else(QueuedAftermathSnapshot::default, |value| {
        QueuedAftermathSnapshot {
            present: true,
            family_code: reaction_family_code(value.family),
            getup_transition_ms: value.getup_transition_ms,
            recover_ms: value.recover_ms,
            landing_stick_ms: value.landing_stick_ms,
            horizontal_damping_bits: value.horizontal_damping.to_bits(),
        }
    })
}

fn validate_relationships(
    fighters: &[FighterSnapshot; FIGHTER_CAPACITY as usize],
    active_slots: [bool; FIGHTER_CAPACITY as usize],
) -> Result<(), SnapshotCodecError> {
    for id in FighterId::ALL {
        let fighter = &fighters[id.index()];
        if !active_slots[id.index()] {
            if *fighter != FighterSnapshot::empty(id) {
                return Err(SnapshotCodecError::new(
                    ERR_INVALID_SNAPSHOT_SLOT,
                    "inactive live slot must use the canonical empty fighter snapshot",
                ));
            }
            continue;
        }
        if fighter.id != id || !fighter.occupied || !fighter.active {
            return Err(SnapshotCodecError::new(
                ERR_INVALID_SNAPSHOT_SLOT,
                "live fighter restore requires one occupied active snapshot per FighterId",
            ));
        }
        if fighter.relationships.linked_entity.is_some() {
            return Err(SnapshotCodecError::new(
                ERR_INVALID_RELATIONSHIP,
                "live fighter has no component mapping for linked_entity",
            ));
        }
        if fighter
            .relationships
            .held_item
            .is_some_and(|held| held.kind() != SimEntityKind::Item)
        {
            return Err(SnapshotCodecError::new(
                ERR_INVALID_RELATIONSHIP,
                "snapshot held-item relationship does not reference the item pool",
            ));
        }
        for target in [
            fighter.relationships.holding,
            fighter.relationships.held_by,
            fighter.relationships.ultimate_owner,
            fighter.relationships.ultimate_target,
            fighter.relationships.last_attacker,
        ] {
            if target.is_some_and(|target| !active_slots[target.index()]) {
                return Err(SnapshotCodecError::new(
                    ERR_INVALID_RELATIONSHIP,
                    "snapshot fighter relationship points at an inactive slot",
                ));
            }
        }
        if let Some(target) = fighter.relationships.holding
            && fighters[target.index()].relationships.held_by != Some(id)
        {
            return Err(SnapshotCodecError::new(
                ERR_INVALID_RELATIONSHIP,
                "snapshot grab holder/victim relationships are not reciprocal",
            ));
        }
        if let Some(holder) = fighter.relationships.held_by
            && fighters[holder.index()].relationships.holding != Some(id)
        {
            return Err(SnapshotCodecError::new(
                ERR_INVALID_RELATIONSHIP,
                "snapshot grab victim/holder relationships are not reciprocal",
            ));
        }
        if let Some(target) = fighter.relationships.ultimate_target
            && fighters[target.index()].relationships.ultimate_owner != Some(id)
        {
            return Err(SnapshotCodecError::new(
                ERR_INVALID_RELATIONSHIP,
                "snapshot ultimate attacker/victim relationships are not reciprocal",
            ));
        }
        if let Some(owner) = fighter.relationships.ultimate_owner
            && fighters[owner.index()].relationships.ultimate_target != Some(id)
        {
            return Err(SnapshotCodecError::new(
                ERR_INVALID_RELATIONSHIP,
                "snapshot ultimate victim/attacker relationships are not reciprocal",
            ));
        }
    }
    Ok(())
}

fn prepare_inactive_fighter(
    world: &World,
    entity: Entity,
) -> Result<PreparedFighter, SnapshotCodecError> {
    ensure_required_components(world, entity)?;
    let fighter = required::<Fighter>(world, entity)?;
    let character = required::<FighterCharacter>(world, entity)?.kind;
    let style = required::<FighterStyle>(world, entity)?.kind;
    let equipment = required::<FighterEquipment>(world, entity)?.kind;
    let mut stats = FighterStats::default();
    stats.respawn_timer = TickTimer::INDEFINITE;
    let mut action = FighterActionState::default();
    action.action = FighterAction::RingOut;
    Ok(PreparedFighter {
        entity,
        // Empty wire slots intentionally omit rollback state. Reconstruct the
        // same inert placeholder invariant used by canonical bootstrap so a
        // restored closed seat can never become a collidable idle fighter.
        transform_translation: fighter.spawn,
        spawn: fighter.spawn,
        input: FighterInput::default(),
        stats,
        motor: FighterMotor {
            facing: if fighter.id % 2 == 0 {
                Vec3::X
            } else {
                Vec3::NEG_X
            },
            ..default()
        },
        action,
        drunk: DrunkStatus::default(),
        inventory: FighterInventory::default(),
        grab: FighterGrabState::default(),
        ultimate: FighterUltimateState::default(),
        special: FighterSpecialState::default(),
        character: FighterCharacter::new(character),
        style: FighterStyle { kind: style },
        equipment: FighterEquipment::new(equipment),
    })
}

fn prepare_fighter(
    world: &World,
    entity: Entity,
    id: FighterId,
    snapshot: &FighterSnapshot,
) -> Result<PreparedFighter, SnapshotCodecError> {
    ensure_required_components(world, entity)?;

    if snapshot.id != id || !snapshot.occupied || !snapshot.active {
        return Err(SnapshotCodecError::new(
            ERR_INVALID_SNAPSHOT_SLOT,
            "fighter snapshot does not match its fixed live slot",
        ));
    }
    snapshot.rollback.validate().map_err(|_| {
        SnapshotCodecError::new(
            ERR_INVALID_LIVE_VALUE,
            "fighter rollback extension failed canonical validation",
        )
    })?;
    validate_canonical_rollback(snapshot.rollback)?;

    let position = decode_vec3(snapshot.rollback.position);
    let spawn = decode_vec3(snapshot.rollback.spawn);
    let stats_snapshot = snapshot.rollback.stats;
    let element_carry = decode_optional_damage_element(stats_snapshot.element_carry)?;
    let mut stats = FighterStats {
        health: f32::from_bits(stats_snapshot.health_bits),
        stamina: f32::from_bits(stats_snapshot.stamina_bits),
        score: snapshot.score,
        last_attacker: snapshot.relationships.last_attacker,
        invulnerability: TickTimer::from_ticks(stats_snapshot.invulnerability_ticks),
        health_refill_timer: TickTimer::from_ticks(stats_snapshot.health_refill_ticks),
        respawn_timer: TickTimer::from_ticks(stats_snapshot.respawn_ticks),
        hud_flash: 0.0,
        element_carry,
        element_carry_strength: f32::from_bits(stats_snapshot.element_carry_strength_bits),
        element_carry_timer: TickTimer::from_ticks(stats_snapshot.element_carry_ticks),
        item_speed_timer: TickTimer::from_ticks(stats_snapshot.item_speed_ticks),
        item_giant_timer: TickTimer::from_ticks(stats_snapshot.item_giant_ticks),
    };
    // Filled only so the staged component itself is valid; commit preserves the
    // live presentation value instead of restoring this placeholder.
    stats.hud_flash = 0.0;

    let motor_snapshot = snapshot.rollback.motor;
    let motor = FighterMotor {
        velocity: decode_vec3(motor_snapshot.velocity),
        facing: decode_vec3(motor_snapshot.facing),
        grounded: snapshot.pose.grounded,
        knockdown_on_land: motor_snapshot.flags & FIGHTER_MOTOR_KNOCKDOWN_ON_LAND != 0,
        landing_aftermath: decode_landing_aftermath(motor_snapshot.landing_aftermath)?,
        air_attack_used: motor_snapshot.flags & FIGHTER_MOTOR_AIR_ATTACK_USED != 0,
        queued_air_attack: decode_optional_technique_button(motor_snapshot.queued_air_attack)?,
        queued_air_attack_timer: TickTimer::from_ticks(motor_snapshot.queued_air_attack_ticks),
        jump_attack_landing_recovery: motor_snapshot.flags
            & FIGHTER_MOTOR_JUMP_ATTACK_LANDING_RECOVERY
            != 0,
        bee_air_dash_motion_active: motor_snapshot.flags & FIGHTER_MOTOR_BEE_AIR_DASH_MOTION_ACTIVE
            != 0,
        bee_air_dash_shot_available: motor_snapshot.flags
            & FIGHTER_MOTOR_BEE_AIR_DASH_SHOT_AVAILABLE
            != 0,
        ledge_grace_timer: TickTimer::from_ticks(motor_snapshot.ledge_grace_ticks),
        landing_stick_timer: TickTimer::from_ticks(motor_snapshot.landing_stick_ticks),
        jump_takeoff_timer: TickTimer::from_ticks(motor_snapshot.jump_takeoff_ticks),
        reaction_bounces: motor_snapshot.reaction_bounces,
        pig_air_meat_slam_air_hits: motor_snapshot.pig_air_meat_slam_air_hits,
        dash_slide_timer: TickTimer::from_ticks(motor_snapshot.dash_slide_ticks),
        dash_jump_carry_timer: TickTimer::from_ticks(motor_snapshot.dash_jump_carry_ticks),
        dash_jump_carry_speed_limit: f32::from_bits(
            motor_snapshot.dash_jump_carry_speed_limit_bits,
        ),
        impact_speed_limit_timer: TickTimer::from_ticks(motor_snapshot.impact_speed_limit_ticks),
        impact_speed_limit: f32::from_bits(motor_snapshot.impact_speed_limit_bits),
        penguin_ice_slide_direction: decode_optional_vec3(
            motor_snapshot.penguin_ice_slide_direction,
        ),
        penguin_ice_slide_speed: f32::from_bits(motor_snapshot.penguin_ice_slide_speed_bits),
        guard_active_timer: ElapsedTicks::from_ticks(motor_snapshot.guard_active_elapsed_ticks),
        guard_cooldown_timer: TickTimer::from_ticks(motor_snapshot.guard_cooldown_ticks),
        guard_start_buffer_timer: TickTimer::from_ticks(motor_snapshot.guard_start_buffer_ticks),
        guard_was_requested: motor_snapshot.flags & FIGHTER_MOTOR_GUARD_WAS_REQUESTED != 0,
        guard_counter_window_timer: TickTimer::from_ticks(
            motor_snapshot.guard_counter_window_ticks,
        ),
        guard_counter_source: decode_optional_vec3(motor_snapshot.guard_counter_source),
        guard_counter_buffered: motor_snapshot.flags & FIGHTER_MOTOR_GUARD_COUNTER_BUFFERED != 0,
    };

    let action_snapshot = snapshot.rollback.action;
    let action = FighterActionState {
        action: fighter_action_from_code(snapshot.action.action_id).ok_or(
            SnapshotCodecError::new(ERR_INVALID_ENUM, "invalid FighterAction snapshot code"),
        )?,
        elapsed: ElapsedTicks::from_ticks(snapshot.action.elapsed_ticks),
        hitbox_spawned: action_snapshot.flags & FIGHTER_ACTION_HITBOX_SPAWNED != 0,
        queued_combo: action_snapshot.flags & FIGHTER_ACTION_QUEUED_COMBO != 0,
        queued_technique: decode_optional_technique(action_snapshot.queued_technique)?,
        queued_button: decode_optional_technique_button(action_snapshot.queued_button)?,
        buffered_button: decode_optional_technique_button(action_snapshot.buffered_button)?,
        buffered_button_elapsed: ElapsedTicks::from_ticks(
            action_snapshot.buffered_button_elapsed_ticks,
        ),
        confirmed_hit: action_snapshot.flags & FIGHTER_ACTION_CONFIRMED_HIT != 0,
        technique_id: decode_optional_technique(action_snapshot.technique_id)?,
        cancel_window_open: action_snapshot.flags & FIGHTER_ACTION_CANCEL_WINDOW_OPEN != 0,
        branch_window_open: action_snapshot.flags & FIGHTER_ACTION_BRANCH_WINDOW_OPEN != 0,
        timeline_events_fired: action_snapshot.timeline_events_fired,
        reaction_getup_ms: decode_optional_u32(action_snapshot.reaction_getup_ms),
        reaction_recover_ms: decode_optional_u32(action_snapshot.reaction_recover_ms),
        reaction_family: decode_optional_reaction_family(action_snapshot.reaction_family)?,
        reaction_visual_side: 1.0,
        charge_elapsed: ElapsedTicks::from_ticks(action_snapshot.charge_elapsed_ticks),
        charge_release_requested: action_snapshot.flags & FIGHTER_ACTION_CHARGE_RELEASE_REQUESTED
            != 0,
    };
    let drunk = DrunkStatus {
        remaining: TickTimer::from_ticks(snapshot.status.timers[DRUNK_STATUS_TIMER_SLOT]),
    };
    let character =
        FighterCharacter::new(character_from_code(snapshot.loadout.character_id).ok_or(
            SnapshotCodecError::new(ERR_INVALID_ENUM, "invalid CharacterKind snapshot code"),
        )?);
    let style = FighterStyle {
        kind: style_from_code(snapshot.loadout.style_id).ok_or(SnapshotCodecError::new(
            ERR_INVALID_ENUM,
            "invalid FighterStyleKind snapshot code",
        ))?,
    };
    let equipment = FighterEquipment {
        kind: equipment_from_code(snapshot.loadout.equipment_ids[0]).ok_or(
            SnapshotCodecError::new(ERR_INVALID_ENUM, "invalid EquipmentKind snapshot code"),
        )?,
        cooldown: TickTimer::from_ticks(snapshot.cooldowns.ticks[EQUIPMENT_COOLDOWN_SLOT]),
    };
    let input = decode_input(snapshot)?;

    let prepared = PreparedFighter {
        entity,
        transform_translation: position,
        spawn,
        input,
        stats,
        motor,
        action,
        drunk,
        inventory: FighterInventory {
            held: snapshot.relationships.held_item,
        },
        grab: FighterGrabState {
            holding: snapshot.relationships.holding,
            held_by: snapshot.relationships.held_by,
            regrab_lockout: TickTimer::from_ticks(snapshot.rollback.regrab_lockout_ticks),
        },
        ultimate: FighterUltimateState {
            target: snapshot.relationships.ultimate_target,
            owner: snapshot.relationships.ultimate_owner,
        },
        special: FighterSpecialState {
            cooldown: TickTimer::from_ticks(snapshot.cooldowns.ticks[SPECIAL_COOLDOWN_SLOT]),
        },
        character,
        style,
        equipment,
    };
    validate_summaries(snapshot, &prepared)?;
    Ok(prepared)
}

fn decode_vec3(value: F32Vec3BitsSnapshot) -> Vec3 {
    Vec3::new(
        f32::from_bits(value.x),
        f32::from_bits(value.y),
        f32::from_bits(value.z),
    )
}

fn validate_canonical_rollback(
    rollback: FighterRollbackExtensionSnapshot,
) -> Result<(), SnapshotCodecError> {
    require_canonical_vec3(rollback.position)?;
    require_canonical_bits(rollback.input_movement.x)?;
    require_canonical_bits(rollback.input_movement.y)?;
    require_canonical_vec3(rollback.spawn)?;

    require_canonical_bits(rollback.stats.health_bits)?;
    require_canonical_bits(rollback.stats.stamina_bits)?;
    require_canonical_bits(rollback.stats.element_carry_strength_bits)?;

    let motor = rollback.motor;
    require_canonical_vec3(motor.velocity)?;
    require_canonical_vec3(motor.facing)?;
    if motor.landing_aftermath.present {
        require_canonical_bits(motor.landing_aftermath.horizontal_damping_bits)?;
    }
    require_canonical_bits(motor.dash_jump_carry_speed_limit_bits)?;
    require_canonical_bits(motor.impact_speed_limit_bits)?;
    if motor.penguin_ice_slide_direction.present {
        require_canonical_vec3(motor.penguin_ice_slide_direction.value)?;
    }
    require_canonical_bits(motor.penguin_ice_slide_speed_bits)?;
    if motor.guard_counter_source.present {
        require_canonical_vec3(motor.guard_counter_source.value)?;
    }
    Ok(())
}

fn require_canonical_vec3(value: F32Vec3BitsSnapshot) -> Result<(), SnapshotCodecError> {
    require_canonical_bits(value.x)?;
    require_canonical_bits(value.y)?;
    require_canonical_bits(value.z)
}

fn require_canonical_bits(bits: u32) -> Result<(), SnapshotCodecError> {
    let value = f32::from_bits(bits);
    if value.is_finite()
        && canonicalize_f32(value, DEFAULT_F32_QUANTIZATION).to_bits() == value.to_bits()
    {
        Ok(())
    } else {
        Err(SnapshotCodecError::new(
            ERR_INVALID_LIVE_VALUE,
            "fighter rollback float is non-finite or off the canonical grid",
        ))
    }
}

fn decode_optional_vec3(value: OptionalF32Vec3BitsSnapshot) -> Option<Vec3> {
    value.present.then(|| decode_vec3(value.value))
}

fn decode_optional_u32(value: OptionalU32Snapshot) -> Option<u32> {
    value.present.then_some(value.value)
}

fn decode_optional_damage_element(
    value: OptionalU8CodeSnapshot,
) -> Result<Option<DamageElement>, SnapshotCodecError> {
    if value.present {
        damage_element_from_code(value.code)
            .map(Some)
            .ok_or(SnapshotCodecError::new(
                ERR_INVALID_ENUM,
                "invalid DamageElement snapshot code",
            ))
    } else {
        Ok(None)
    }
}

fn decode_optional_technique(
    value: OptionalU16CodeSnapshot,
) -> Result<Option<TechniqueId>, SnapshotCodecError> {
    if value.present {
        technique_from_code(value.code)
            .map(Some)
            .ok_or(SnapshotCodecError::new(
                ERR_INVALID_ENUM,
                "invalid TechniqueId snapshot code",
            ))
    } else {
        Ok(None)
    }
}

fn decode_optional_technique_button(
    value: OptionalU8CodeSnapshot,
) -> Result<Option<TechniqueButton>, SnapshotCodecError> {
    if value.present {
        technique_button_from_code(value.code)
            .map(Some)
            .ok_or(SnapshotCodecError::new(
                ERR_INVALID_ENUM,
                "invalid TechniqueButton snapshot code",
            ))
    } else {
        Ok(None)
    }
}

fn decode_optional_reaction_family(
    value: OptionalU8CodeSnapshot,
) -> Result<Option<ReactionFamilyId>, SnapshotCodecError> {
    if value.present {
        reaction_family_from_code(value.code)
            .map(Some)
            .ok_or(SnapshotCodecError::new(
                ERR_INVALID_ENUM,
                "invalid ReactionFamilyId snapshot code",
            ))
    } else {
        Ok(None)
    }
}

fn decode_landing_aftermath(
    value: QueuedAftermathSnapshot,
) -> Result<Option<QueuedAftermath>, SnapshotCodecError> {
    if !value.present {
        return Ok(None);
    }
    let family = reaction_family_from_code(value.family_code).ok_or(SnapshotCodecError::new(
        ERR_INVALID_ENUM,
        "invalid landing-aftermath ReactionFamilyId snapshot code",
    ))?;
    let mut aftermath = QueuedAftermath {
        family,
        getup_transition_ms: value.getup_transition_ms,
        recover_ms: value.recover_ms,
        landing_stick_ms: value.landing_stick_ms,
        horizontal_damping: f32::from_bits(value.horizontal_damping_bits),
        cue: "",
    };
    aftermath.cue =
        queued_aftermath_presentation_cue(&aftermath).ok_or(SnapshotCodecError::new(
            ERR_INVALID_LIVE_VALUE,
            "landing aftermath tuple has no unambiguous authored presentation cue",
        ))?;
    Ok(Some(aftermath))
}

fn decode_input(snapshot: &FighterSnapshot) -> Result<FighterInput, SnapshotCodecError> {
    let flags = snapshot.input.held_buttons;
    if flags & !INPUT_FLAG_MASK != 0
        || snapshot.input.pressed_latches != 0
        || snapshot.input.released_latches != 0
    {
        return Err(SnapshotCodecError::new(
            ERR_INVALID_LIVE_VALUE,
            "fighter input snapshot contains unknown flags or latch payload",
        ));
    }
    Ok(FighterInput {
        movement: Vec2::new(
            f32::from_bits(snapshot.rollback.input_movement.x),
            f32::from_bits(snapshot.rollback.input_movement.y),
        ),
        aim: flags & INPUT_AIM != 0,
        jump: flags & INPUT_JUMP != 0,
        dash: flags & INPUT_DASH != 0,
        light: flags & INPUT_LIGHT != 0,
        light_held: flags & INPUT_LIGHT_HELD != 0,
        raw_light_pressed: flags & INPUT_RAW_LIGHT_PRESSED != 0,
        heavy: flags & INPUT_HEAVY != 0,
        heavy_held: flags & INPUT_HEAVY_HELD != 0,
        raw_heavy_pressed: flags & INPUT_RAW_HEAVY_PRESSED != 0,
        heavy_released: flags & INPUT_HEAVY_RELEASED != 0,
        grab: flags & INPUT_GRAB != 0,
        guard: flags & INPUT_GUARD != 0,
        ultimate: flags & INPUT_ULTIMATE != 0,
        special: flags & INPUT_SPECIAL != 0,
    })
}

fn validate_summaries(
    snapshot: &FighterSnapshot,
    prepared: &PreparedFighter,
) -> Result<(), SnapshotCodecError> {
    let expected_input = FighterInputSnapshot {
        move_x: input_axis_summary(prepared.input.movement.x),
        move_y: input_axis_summary(prepared.input.movement.y),
        held_buttons: encode_input_flags(&prepared.input),
        pressed_latches: 0,
        released_latches: 0,
    };
    let expected_pose = FighterPoseSnapshot {
        position: quantized_vec3(prepared.transform_translation),
        velocity: quantized_vec3(prepared.motor.velocity),
        facing: QuantizedVec2 {
            x: quantize(prepared.motor.facing.x),
            z: quantize(prepared.motor.facing.z),
        },
        grounded: prepared.motor.grounded,
        collision_flags: 0,
    };
    let expected_action = FighterActionSnapshot {
        action_id: fighter_action_code(prepared.action.action),
        elapsed_ticks: prepared.action.elapsed.get(),
        flags: u32::from(encode_action_flags(&prepared.action)),
        buffered_action_id: optional_u8_summary(
            prepared.action.buffered_button.map(technique_button_code),
        ),
        reaction_id: optional_u8_summary(prepared.action.reaction_family.map(reaction_family_code)),
        reaction_ticks: 0,
    };
    let mut expected_cooldowns = [0; COOLDOWN_SLOTS];
    expected_cooldowns[SPECIAL_COOLDOWN_SLOT] = prepared.special.cooldown.remaining();
    expected_cooldowns[EQUIPMENT_COOLDOWN_SLOT] = prepared.equipment.cooldown.remaining();
    let mut expected_status_timers = [0; STATUS_TIMER_SLOTS];
    expected_status_timers[0] = prepared.stats.invulnerability.remaining();
    expected_status_timers[1] = prepared.stats.health_refill_timer.remaining();
    expected_status_timers[2] = prepared.stats.respawn_timer.remaining();
    expected_status_timers[3] = prepared.stats.element_carry_timer.remaining();
    expected_status_timers[4] = prepared.stats.item_speed_timer.remaining();
    expected_status_timers[5] = prepared.stats.item_giant_timer.remaining();
    expected_status_timers[DRUNK_STATUS_TIMER_SLOT] = prepared.drunk.remaining.remaining();
    let expected_status = FighterStatusSnapshot {
        flags: 0,
        timers: expected_status_timers,
        elemental_carry: prepared
            .stats
            .element_carry
            .map_or(-1, |element| i32::from(damage_element_code(element))),
        size_scale: quantize(prepared.stats.item_size_multiplier()),
        speed_scale: quantize(prepared.stats.item_speed_multiplier()),
    };
    let character_id = character_code(prepared.character.kind);
    let mut expected_equipment = [0; EQUIPMENT_SLOTS];
    expected_equipment[0] = equipment_code(prepared.equipment.kind);
    let expected_loadout = FighterLoadoutSnapshot {
        character_id,
        style_id: style_code(prepared.style.kind),
        move_set_id: character_id,
        equipment_ids: expected_equipment,
    };

    if snapshot.input != expected_input
        || snapshot.pose != expected_pose
        || snapshot.health != quantize(prepared.stats.health)
        || snapshot.stamina != quantize(prepared.stats.stamina)
        || snapshot.action != expected_action
        || snapshot.cooldowns.ticks != expected_cooldowns
        || snapshot.status != expected_status
        || snapshot.loadout != expected_loadout
    {
        return Err(SnapshotCodecError::new(
            ERR_NON_CANONICAL_SUMMARY,
            "fighter legacy summary fields disagree with exact rollback state",
        ));
    }
    Ok(())
}

fn commit_fighter(world: &mut World, prepared: PreparedFighter) {
    let PreparedFighter {
        entity,
        transform_translation,
        spawn,
        input,
        stats,
        motor,
        action,
        drunk,
        inventory,
        grab,
        ultimate,
        special,
        character,
        style,
        equipment,
    } = prepared;

    world
        .get_mut::<Fighter>(entity)
        .expect("restore preflight checked Fighter")
        .spawn = spawn;
    world
        .get_mut::<SimPosition>(entity)
        .expect("restore preflight checked SimPosition")
        .translation = transform_translation;
    if let Some(mut transform) = world.get_mut::<Transform>(entity) {
        // One-way presentation projection. Rotation and scale deliberately
        // survive rollback and can never feed snapshot state back into play.
        transform.translation = transform_translation;
    }
    *world
        .get_mut::<FighterInput>(entity)
        .expect("restore preflight checked FighterInput") = input;

    {
        let mut live = world
            .get_mut::<FighterStats>(entity)
            .expect("restore preflight checked FighterStats");
        let hud_flash = live.hud_flash;
        *live = stats;
        live.hud_flash = hud_flash;
    }
    *world
        .get_mut::<FighterMotor>(entity)
        .expect("restore preflight checked FighterMotor") = motor;
    {
        let mut live = world
            .get_mut::<FighterActionState>(entity)
            .expect("restore preflight checked FighterActionState");
        let reaction_visual_side = live.reaction_visual_side;
        *live = action;
        live.reaction_visual_side = reaction_visual_side;
    }
    *world
        .get_mut::<DrunkStatus>(entity)
        .expect("restore preflight checked DrunkStatus") = drunk;

    *world
        .get_mut::<FighterInventory>(entity)
        .expect("restore preflight checked FighterInventory") = inventory;
    *world
        .get_mut::<FighterGrabState>(entity)
        .expect("restore preflight checked FighterGrabState") = grab;
    *world
        .get_mut::<FighterUltimateState>(entity)
        .expect("restore preflight checked FighterUltimateState") = ultimate;
    *world
        .get_mut::<FighterSpecialState>(entity)
        .expect("restore preflight checked FighterSpecialState") = special;
    *world
        .get_mut::<FighterCharacter>(entity)
        .expect("restore preflight checked FighterCharacter") = character;
    *world
        .get_mut::<FighterStyle>(entity)
        .expect("restore preflight checked FighterStyle") = style;
    *world
        .get_mut::<FighterEquipment>(entity)
        .expect("restore preflight checked FighterEquipment") = equipment;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::determinism::SimEntityId;

    const NAMES: [&str; FIGHTER_CAPACITY as usize] = ["cat", "pig", "dog", "fox"];

    fn fixture_world(order: [usize; FIGHTER_CAPACITY as usize]) -> (World, [Entity; 4]) {
        let mut world = World::new();
        let mut match_state = MatchState::default();
        match_state.set_active_slots([true; FIGHTER_CAPACITY as usize]);
        world.insert_resource(match_state);
        let mut entities = [None; FIGHTER_CAPACITY as usize];
        for index in order {
            let mut stats = FighterStats {
                health: 95.25 - index as f32 * 3.125,
                stamina: 44.5 + index as f32 * 1.75,
                score: index as i32 - 2,
                last_attacker: Some(FighterId::from_index(index ^ 1).unwrap()),
                invulnerability: TickTimer::from_ticks(10 + index as u32),
                health_refill_timer: TickTimer::from_ticks(20 + index as u32),
                respawn_timer: TickTimer::from_ticks(30 + index as u32),
                hud_flash: 0.1 + index as f32 * 0.01,
                element_carry: Some(DAMAGE_ELEMENTS[index]),
                element_carry_strength: 0.25 + index as f32 * 0.125,
                element_carry_timer: TickTimer::from_ticks(40 + index as u32),
                item_speed_timer: TickTimer::from_ticks(50 + index as u32),
                item_giant_timer: TickTimer::from_ticks(60 + index as u32),
            };
            if index == 3 {
                stats.element_carry = None;
            }
            let landing_aftermath = (index == 0).then(|| {
                let mut aftermath = crate::reactions::reaction_family_definition(
                    ReactionFamilyId::GroundBounceDown,
                )
                .landing_aftermath
                .unwrap();
                aftermath.horizontal_damping =
                    canonicalize_f32(aftermath.horizontal_damping, DEFAULT_F32_QUANTIZATION);
                aftermath.cue = "presentation-definition-cue-is-excluded";
                aftermath
            });
            let motor = FighterMotor {
                velocity: Vec3::new(1.25 + index as f32, -2.5, 3.75),
                facing: Vec3::new(-0.5, 0.0, 0.875),
                grounded: index % 2 == 0,
                knockdown_on_land: index == 0,
                landing_aftermath,
                air_attack_used: index == 1,
                queued_air_attack: Some(TECHNIQUE_BUTTONS[index]),
                queued_air_attack_timer: TickTimer::from_ticks(70 + index as u32),
                jump_attack_landing_recovery: index == 2,
                bee_air_dash_motion_active: index == 3,
                bee_air_dash_shot_available: index == 0,
                ledge_grace_timer: TickTimer::from_ticks(80 + index as u32),
                landing_stick_timer: TickTimer::from_ticks(90 + index as u32),
                jump_takeoff_timer: TickTimer::from_ticks(100 + index as u32),
                reaction_bounces: index as u8,
                pig_air_meat_slam_air_hits: index as u8 + 1,
                dash_slide_timer: TickTimer::from_ticks(110 + index as u32),
                dash_jump_carry_timer: TickTimer::from_ticks(120 + index as u32),
                dash_jump_carry_speed_limit: 12.5 + index as f32,
                impact_speed_limit_timer: TickTimer::from_ticks(130 + index as u32),
                impact_speed_limit: 9.75 + index as f32,
                penguin_ice_slide_direction: Some(Vec3::new(0.5, 0.0, -0.75)),
                penguin_ice_slide_speed: 7.25 + index as f32,
                guard_active_timer: ElapsedTicks::from_ticks(140 + index as u32),
                guard_cooldown_timer: TickTimer::from_ticks(150 + index as u32),
                guard_start_buffer_timer: TickTimer::from_ticks(160 + index as u32),
                guard_was_requested: index % 2 == 0,
                guard_counter_window_timer: TickTimer::from_ticks(170 + index as u32),
                guard_counter_source: Some(Vec3::new(-1.25, 2.5, index as f32)),
                guard_counter_buffered: index % 2 == 1,
            };
            let action = FighterActionState {
                action: FIGHTER_ACTIONS[8 + index],
                elapsed: ElapsedTicks::from_ticks(180 + index as u32),
                hitbox_spawned: index == 0,
                queued_combo: index == 1,
                queued_technique: Some(TECHNIQUES[10 + index]),
                queued_button: Some(TECHNIQUE_BUTTONS[(index + 1) % TECHNIQUE_BUTTONS.len()]),
                buffered_button: Some(TECHNIQUE_BUTTONS[(index + 2) % TECHNIQUE_BUTTONS.len()]),
                buffered_button_elapsed: ElapsedTicks::from_ticks(190 + index as u32),
                confirmed_hit: index == 2,
                technique_id: Some(TECHNIQUES[20 + index]),
                cancel_window_open: index == 3,
                branch_window_open: index == 0,
                timeline_events_fired: 0x1000_0000_0000_0000 + index as u64,
                reaction_getup_ms: Some(400 + index as u32),
                reaction_recover_ms: (index != 3).then_some(600 + index as u32),
                reaction_family: Some(REACTION_FAMILIES[index]),
                reaction_visual_side: if index % 2 == 0 { 1.0 } else { -1.0 },
                charge_elapsed: ElapsedTicks::from_ticks(200 + index as u32),
                charge_release_requested: index == 1,
            };
            let grab = FighterGrabState {
                holding: (index == 0).then_some(FighterId::new(1).unwrap()),
                held_by: (index == 1).then_some(FighterId::new(0).unwrap()),
                regrab_lockout: TickTimer::from_ticks(210 + index as u32),
            };
            let ultimate = FighterUltimateState {
                target: (index == 2).then_some(FighterId::new(3).unwrap()),
                owner: (index == 3).then_some(FighterId::new(2).unwrap()),
            };
            let inventory = FighterInventory {
                held: (index == 0).then_some(SimEntityId::new(SimEntityKind::Item, 2, 7)),
            };
            let transform = Transform {
                translation: Vec3::new(index as f32 + 0.125, 2.25, -3.5),
                rotation: Quat::from_rotation_y(0.2 + index as f32 * 0.1),
                scale: Vec3::splat(1.1 + index as f32 * 0.05),
            };
            let entity = world
                .spawn((
                    Fighter {
                        id: index,
                        name: NAMES[index],
                        color: Color::srgb(0.1 + index as f32 * 0.1, 0.2, 0.3),
                        spawn: Vec3::new(-10.0 + index as f32, 0.5, 8.0),
                    },
                    SimPosition::new(transform.translation),
                    transform,
                    FighterInput {
                        movement: Vec2::new(
                            canonicalize_f32(-0.75 + index as f32 * 0.2, DEFAULT_F32_QUANTIZATION),
                            canonicalize_f32(0.4, DEFAULT_F32_QUANTIZATION),
                        ),
                        aim: index == 0,
                        jump: index == 1,
                        dash: index == 2,
                        light: index == 3,
                        light_held: true,
                        raw_light_pressed: index == 0,
                        heavy: index == 1,
                        heavy_held: true,
                        raw_heavy_pressed: index == 2,
                        heavy_released: index == 3,
                        grab: index == 0,
                        guard: index == 1,
                        ultimate: index == 2,
                        special: index == 3,
                    },
                    stats,
                    motor,
                    action,
                    DrunkStatus {
                        remaining: TickTimer::from_ticks(240 + index as u32),
                    },
                    inventory,
                    grab,
                    ultimate,
                    FighterSpecialState {
                        cooldown: TickTimer::from_ticks(220 + index as u32),
                    },
                    FighterCharacter::new(CHARACTER_KINDS[index]),
                    FighterStyle {
                        kind: STYLE_KINDS[index % STYLE_KINDS.len()],
                    },
                    FighterEquipment {
                        kind: EQUIPMENT_KINDS[index],
                        cooldown: TickTimer::from_ticks(230 + index as u32),
                    },
                ))
                .id();
            entities[index] = Some(entity);
        }
        (
            world,
            entities.map(|entity| entity.expect("fixture created every fighter")),
        )
    }

    #[test]
    fn snapshot_decode_rejects_unknown_landing_aftermath_cue_tuple() {
        let authored =
            crate::reactions::reaction_family_definition(ReactionFamilyId::GroundBounceDown)
                .landing_aftermath
                .unwrap();
        let mut snapshot = encode_landing_aftermath(Some(authored));
        snapshot.recover_ms = snapshot.recover_ms.saturating_add(1);

        let error = decode_landing_aftermath(snapshot).unwrap_err();
        assert_eq!(error.code, ERR_INVALID_LIVE_VALUE);
    }

    #[test]
    fn full_live_round_trip_restores_authoritative_state_and_preserves_presentation() {
        let codec = LiveFighterSnapshotCodec;
        let (mut world, entities) = fixture_world([0, 1, 2, 3]);
        let baseline = codec.capture_fighters(&world).unwrap();

        for entity in entities {
            {
                let mut fighter = world.get_mut::<Fighter>(entity).unwrap();
                fighter.spawn = Vec3::splat(999.0);
                fighter.name = "preserved-name";
                fighter.color = Color::srgb(0.9, 0.8, 0.7);
            }
            {
                world.get_mut::<SimPosition>(entity).unwrap().translation = Vec3::splat(888.0);
                let mut transform = world.get_mut::<Transform>(entity).unwrap();
                transform.translation = Vec3::splat(888.0);
                transform.rotation = Quat::from_rotation_x(0.75);
                transform.scale = Vec3::splat(2.25);
            }
            *world.get_mut::<FighterInput>(entity).unwrap() = FighterInput::default();
            world.get_mut::<FighterStats>(entity).unwrap().health = -100.0;
            world.get_mut::<FighterStats>(entity).unwrap().hud_flash = 0.777;
            world.get_mut::<FighterMotor>(entity).unwrap().velocity = Vec3::splat(-50.0);
            if let Some(aftermath) = world
                .get_mut::<FighterMotor>(entity)
                .unwrap()
                .landing_aftermath
                .as_mut()
            {
                aftermath.cue = "future-side-cue-must-not-survive-restore";
            }
            world.get_mut::<FighterActionState>(entity).unwrap().action = FighterAction::Idle;
            world
                .get_mut::<FighterActionState>(entity)
                .unwrap()
                .reaction_visual_side = -0.625;
            {
                let mut drunk = world.get_mut::<DrunkStatus>(entity).unwrap();
                drunk.remaining = TickTimer::ZERO;
            }
            world.get_mut::<FighterInventory>(entity).unwrap().held = None;
            *world.get_mut::<FighterGrabState>(entity).unwrap() = FighterGrabState::default();
            *world.get_mut::<FighterUltimateState>(entity).unwrap() =
                FighterUltimateState::default();
            world
                .get_mut::<FighterSpecialState>(entity)
                .unwrap()
                .cooldown = TickTimer::ZERO;
            world.get_mut::<FighterCharacter>(entity).unwrap().kind = CharacterKind::Chick;
            world.get_mut::<FighterStyle>(entity).unwrap().kind = FighterStyleKind::Anchor;
            world.get_mut::<FighterEquipment>(entity).unwrap().kind = EquipmentKind::HeavySeal;
        }

        let plan = codec.prepare_restore(&world, &baseline).unwrap();
        codec.commit_restore(&mut world, plan);
        assert_eq!(codec.capture_fighters(&world).unwrap(), baseline);

        for entity in entities {
            let fighter = world.get::<Fighter>(entity).unwrap();
            assert_eq!(fighter.name, "preserved-name");
            assert_eq!(fighter.color, Color::srgb(0.9, 0.8, 0.7));
            let transform = world.get::<Transform>(entity).unwrap();
            assert_eq!(
                world.get::<SimPosition>(entity).unwrap().translation,
                transform.translation
            );
            assert_eq!(transform.rotation, Quat::from_rotation_x(0.75));
            assert_eq!(transform.scale, Vec3::splat(2.25));
            assert_eq!(world.get::<FighterStats>(entity).unwrap().hud_flash, 0.777);
            assert_eq!(
                world
                    .get::<FighterActionState>(entity)
                    .unwrap()
                    .reaction_visual_side,
                -0.625
            );
            let drunk = world.get::<DrunkStatus>(entity).unwrap();
            assert!(drunk.remaining.active());
        }
        let restored_aftermath = world
            .get::<FighterMotor>(entities[0])
            .unwrap()
            .landing_aftermath
            .unwrap();
        assert_eq!(
            restored_aftermath.cue,
            queued_aftermath_presentation_cue(&restored_aftermath)
                .expect("fixture aftermath tuple is authored")
        );
    }

    #[test]
    fn capture_and_restore_are_independent_of_entity_creation_order() {
        let codec = LiveFighterSnapshotCodec;
        let (ordered, _) = fixture_world([0, 1, 2, 3]);
        let (mut reversed, _) = fixture_world([3, 2, 1, 0]);
        let expected = codec.capture_fighters(&ordered).unwrap();
        assert_eq!(codec.capture_fighters(&reversed).unwrap(), expected);

        let plan = codec.prepare_restore(&reversed, &expected).unwrap();
        codec.commit_restore(&mut reversed, plan);
        assert_eq!(codec.capture_fighters(&reversed).unwrap(), expected);
    }

    #[test]
    fn mixed_active_roster_uses_empty_snapshots_and_restores_inert_placeholders() {
        let codec = LiveFighterSnapshotCodec;
        let (mut world, entities) = fixture_world([3, 1, 0, 2]);
        let four_active = codec.capture_fighters(&world).unwrap();
        assert!(four_active.iter().all(|fighter| fighter.active));
        world
            .resource_mut::<MatchState>()
            .set_active_slots([true, true, false, false]);

        let snapshots = codec.capture_fighters(&world).unwrap();
        assert!(snapshots[0].occupied && snapshots[1].occupied);
        assert_eq!(
            snapshots[2],
            FighterSnapshot::empty(FighterId::new(2).unwrap())
        );
        assert_eq!(
            snapshots[3],
            FighterSnapshot::empty(FighterId::new(3).unwrap())
        );

        for entity in [entities[2], entities[3]] {
            world.get_mut::<Fighter>(entity).unwrap().name = "closed-seat";
            world.get_mut::<FighterStats>(entity).unwrap().hud_flash = 0.456;
            world
                .get_mut::<FighterActionState>(entity)
                .unwrap()
                .reaction_visual_side = -0.875;
            let mut transform = world.get_mut::<Transform>(entity).unwrap();
            transform.rotation = Quat::from_rotation_z(0.33);
            transform.scale = Vec3::splat(1.75);
        }

        let plan = codec.prepare_restore(&world, &snapshots).unwrap();
        codec.commit_restore(&mut world, plan);
        assert_eq!(codec.capture_fighters(&world).unwrap(), snapshots);

        for entity in [entities[2], entities[3]] {
            let fighter = world.get::<Fighter>(entity).unwrap();
            assert_eq!(fighter.name, "closed-seat");
            let expected_spawn = Vec3::new(-10.0 + fighter.id as f32, 0.5, 8.0);
            assert_eq!(fighter.spawn, expected_spawn);
            assert_eq!(
                world.get::<SimPosition>(entity).unwrap().translation,
                expected_spawn
            );
            let transform = world.get::<Transform>(entity).unwrap();
            assert_eq!(transform.translation, expected_spawn);
            assert_eq!(transform.rotation, Quat::from_rotation_z(0.33));
            assert_eq!(transform.scale, Vec3::splat(1.75));
            let stats = world.get::<FighterStats>(entity).unwrap();
            assert_eq!(stats.health, FighterStats::default().health);
            assert_eq!(stats.respawn_timer, TickTimer::INDEFINITE);
            assert_eq!(stats.hud_flash, 0.456);
            let motor = world.get::<FighterMotor>(entity).unwrap();
            assert_eq!(motor.velocity, Vec3::ZERO);
            assert_eq!(
                motor.facing,
                if fighter.id % 2 == 0 {
                    Vec3::X
                } else {
                    Vec3::NEG_X
                }
            );
            let action = world.get::<FighterActionState>(entity).unwrap();
            assert_eq!(action.action, FighterAction::RingOut);
            assert_eq!(action.reaction_visual_side, -0.875);
            let drunk = world.get::<DrunkStatus>(entity).unwrap();
            assert!(!drunk.remaining.active());
            assert_eq!(
                world.get::<FighterCharacter>(entity).unwrap().kind,
                CHARACTER_KINDS[fighter.id]
            );
            assert_eq!(
                world.get::<FighterStyle>(entity).unwrap().kind,
                STYLE_KINDS[fighter.id % STYLE_KINDS.len()]
            );
            assert_eq!(
                world.get::<FighterEquipment>(entity).unwrap().kind,
                EQUIPMENT_KINDS[fighter.id]
            );
        }
    }

    #[test]
    fn restore_uses_incoming_roster_and_relationship_to_closed_slot_fails_preflight() {
        let codec = LiveFighterSnapshotCodec;
        let (mut world, _) = fixture_world([0, 1, 2, 3]);
        world
            .resource_mut::<MatchState>()
            .set_active_slots([true, true, false, false]);
        let snapshots = codec.capture_fighters(&world).unwrap();

        let mut invalid_relationship = snapshots;
        invalid_relationship[0].relationships.last_attacker = Some(FighterId::new(2).unwrap());
        let before_rejected_restore = codec.capture_fighters(&world).unwrap();
        assert_eq!(
            codec
                .prepare_restore(&world, &invalid_relationship)
                .err()
                .unwrap()
                .code,
            ERR_INVALID_RELATIONSHIP
        );
        assert_eq!(
            codec.capture_fighters(&world).unwrap(),
            before_rejected_restore,
            "relationship validation must finish before any live component mutation"
        );

        world
            .resource_mut::<MatchState>()
            .set_active_slots([true, true, true, false]);
        let plan = codec
            .prepare_restore(&world, &snapshots)
            .expect("incoming roster, not stale destination MatchState, drives preflight");
        world
            .resource_mut::<MatchState>()
            .set_active_slots([true, true, false, false]);
        codec.commit_restore(&mut world, plan);
        assert_eq!(codec.capture_fighters(&world).unwrap(), snapshots);
    }

    #[test]
    fn missing_duplicate_and_out_of_range_live_slots_fail_closed() {
        let codec = LiveFighterSnapshotCodec;

        let (mut missing_component, entities) = fixture_world([0, 1, 2, 3]);
        missing_component
            .entity_mut(entities[2])
            .remove::<FighterMotor>();
        assert_eq!(
            codec.capture_fighters(&missing_component).unwrap_err().code,
            ERR_MISSING_COMPONENT
        );

        let (mut duplicate, _) = fixture_world([0, 1, 2, 3]);
        duplicate.spawn(Fighter {
            id: 0,
            name: "duplicate",
            color: Color::WHITE,
            spawn: Vec3::ZERO,
        });
        assert_eq!(
            codec.capture_fighters(&duplicate).unwrap_err().code,
            ERR_DUPLICATE_LIVE_ID
        );

        let (mut out_of_range, entities) = fixture_world([0, 1, 2, 3]);
        out_of_range.get_mut::<Fighter>(entities[3]).unwrap().id = 99;
        assert_eq!(
            codec.capture_fighters(&out_of_range).unwrap_err().code,
            ERR_INVALID_LIVE_ID
        );

        let (mut missing_slot, entities) = fixture_world([0, 1, 2, 3]);
        missing_slot.entity_mut(entities[1]).remove::<Fighter>();
        assert_eq!(
            codec.capture_fighters(&missing_slot).unwrap_err().code,
            ERR_MISSING_LIVE_SLOT
        );
    }

    #[test]
    fn invalid_enum_and_relationship_snapshots_fail_preflight() {
        let codec = LiveFighterSnapshotCodec;
        let (world, _) = fixture_world([0, 1, 2, 3]);
        let snapshots = codec.capture_fighters(&world).unwrap();

        let mut invalid_action = snapshots;
        invalid_action[0].action.action_id = FIGHTER_ACTION_CODE_COUNT;
        assert_eq!(
            codec
                .prepare_restore(&world, &invalid_action)
                .err()
                .unwrap()
                .code,
            ERR_INVALID_ENUM
        );

        let mut invalid_character = snapshots;
        invalid_character[0].loadout.character_id = u16::MAX;
        assert_eq!(
            codec
                .prepare_restore(&world, &invalid_character)
                .err()
                .unwrap()
                .code,
            ERR_INVALID_ENUM
        );

        let mut invalid_item = snapshots;
        invalid_item[0].relationships.held_item =
            Some(SimEntityId::new(SimEntityKind::Hitbox, 0, 1));
        assert_eq!(
            codec
                .prepare_restore(&world, &invalid_item)
                .err()
                .unwrap()
                .code,
            ERR_INVALID_RELATIONSHIP
        );

        let mut nonreciprocal = snapshots;
        nonreciprocal[1].relationships.held_by = None;
        assert_eq!(
            codec
                .prepare_restore(&world, &nonreciprocal)
                .err()
                .unwrap()
                .code,
            ERR_INVALID_RELATIONSHIP
        );
    }

    #[test]
    fn failed_preflight_is_atomic_and_leaves_every_live_fighter_unchanged() {
        let codec = LiveFighterSnapshotCodec;
        let (world, _) = fixture_world([0, 1, 2, 3]);
        let before = codec.capture_fighters(&world).unwrap();
        let mut hostile = before;
        hostile[3].rollback.stats.health_bits = f32::NAN.to_bits();

        assert_eq!(
            codec.prepare_restore(&world, &hostile).err().unwrap().code,
            ERR_INVALID_LIVE_VALUE
        );
        assert_eq!(codec.capture_fighters(&world).unwrap(), before);

        let mut off_grid = before;
        off_grid[3].rollback.stats.health_bits = 0.1_f32.to_bits();
        off_grid[3].health = quantize(0.1);
        assert_eq!(
            codec.prepare_restore(&world, &off_grid).err().unwrap().code,
            ERR_INVALID_LIVE_VALUE
        );
        assert_eq!(codec.capture_fighters(&world).unwrap(), before);

        let (mut off_grid_live, entities) = fixture_world([0, 1, 2, 3]);
        off_grid_live
            .get_mut::<SimPosition>(entities[0])
            .unwrap()
            .translation
            .x = 0.1;
        assert_eq!(
            codec.capture_fighters(&off_grid_live).unwrap_err().code,
            ERR_INVALID_LIVE_VALUE
        );
    }
}
