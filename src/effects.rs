use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use std::f32::consts::PI;

use crate::constants::{POP_BOMB_BLAST_MESH_RADIUS, POP_BOMB_BLAST_VISUAL_END_SCALE};
use crate::reactions::ReactionFamilyId;
use crate::techniques::{DamageElement, FeedbackPhase};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectKind {
    HitSpark,
    GuardFlash,
    DashTrail,
    DustPuff,
    TimelinePulse,
    AftermathPulse,
    FeedbackBurst,
    FirePunch,
    Burning,
    SpecialPulse,
    RingOutBurst,
    RespawnColumn,
    PopBombBlast,
}

#[derive(Component)]
pub struct VisualEffect {
    pub kind: EffectKind,
    pub lifetime: f32,
    pub age: f32,
    pub velocity: Vec3,
    pub spin: Vec3,
    pub start_scale: Vec3,
    pub end_scale: Vec3,
}

#[derive(Resource, Default)]
pub struct EffectAssets {
    spark_mesh: Handle<Mesh>,
    puff_mesh: Handle<Mesh>,
    column_mesh: Handle<Mesh>,
    ring_mesh: Handle<Mesh>,
    trail_mesh: Handle<Mesh>,
    fire_punch_mesh: Handle<Mesh>,
    burn_flame_mesh: Handle<Mesh>,
    crescent_mesh: Handle<Mesh>,
    heart_mesh: Handle<Mesh>,
    blast_mesh: Handle<Mesh>,
    yellow: Handle<StandardMaterial>,
    orange: Handle<StandardMaterial>,
    cyan: Handle<StandardMaterial>,
    blue: Handle<StandardMaterial>,
    smoke: Handle<StandardMaterial>,
    white: Handle<StandardMaterial>,
    red: Handle<StandardMaterial>,
    pink: Handle<StandardMaterial>,
    slash_red: Handle<StandardMaterial>,
    fire_punch: Handle<StandardMaterial>,
    fire_punch_blue: Handle<StandardMaterial>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirePunchPalette {
    Red,
    Blue,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ElementSparkProfile {
    pub count_scale: f32,
    pub velocity_scale: f32,
    pub heavy_bias: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackPackageId {
    GenericStartup,
    GenericPreHit,
    GenericImpact,
    GenericAftermath,
    LightStartup,
    FirePunchTrail,
    LightImpact,
    LightRecover,
    LauncherStartup,
    LauncherTrail,
    LauncherImpact,
    LauncherAftermath,
    FinisherStartup,
    FinisherTrail,
    PigHamSlamImpact,
    HeavyImpact,
    FloorPulse,
    UltimateStartup,
    UltimateScratch,
    UltimateScratchImpact,
    UltimateBomb,
    UltimateRecover,
    DashStartup,
    DashTrail,
    DashBrake,
    JumpStartup,
    JumpTrail,
    JumpAftermath,
    GuardCounterStartup,
    GuardClang,
    PerfectGuard,
    GuardBreak,
    QuickStand,
    RollTravel,
    LandingStick,
    SpecialCastStartup,
    SpecialCastRelease,
    SpecialCastRecover,
    SpecialProjectileStartup,
    SpecialProjectileRelease,
    SpecialProjectileImpact,
    SpecialProjectileRecover,
    SpecialTrapStartup,
    SpecialTrapArm,
    SpecialTrapImpact,
    SpecialTrapRecover,
    SpecialShockwaveStartup,
    SpecialShockwaveRelease,
    SpecialShockwaveImpact,
    SpecialShockwaveRecover,
    SpecialHazardStartup,
    SpecialHazardPulse,
    SpecialHazardImpact,
    SpecialHazardFade,
    ItemUtility,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
pub enum HitImpactEffectId {
    GenericLight,
    GenericHeavy,
    LauncherCyan,
    GroundBounceOrange,
    UltimateSlashRed,
    UltimateBombRed,
    LightBlue,
    PigHamSlamHeart,
    PigHamSwing,
    PigAirMeatSlam,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackVisualKind {
    Ring,
    Trail,
    FirePunch,
    CrescentSlash,
    Heart,
    Spark,
    Puff,
    Column,
    Burst,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackMaterialId {
    Yellow,
    Orange,
    Cyan,
    Blue,
    Smoke,
    White,
    Red,
    Pink,
    SlashRed,
    FirePunch,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FeedbackBurstDef {
    pub visual: FeedbackVisualKind,
    pub material: FeedbackMaterialId,
    pub count: u8,
    pub offset: Vec3,
    pub spread: f32,
    pub lifetime: f32,
    pub velocity: Vec3,
    pub spin: Vec3,
    pub start_scale: Vec3,
    pub end_scale: Vec3,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FeedbackPackageDef {
    pub id: FeedbackPackageId,
    pub reference_effect: Option<&'static str>,
    pub reference_sound: Option<&'static str>,
    pub shake_scale: f32,
    pub hud_flash_scale: f32,
    pub primary: FeedbackBurstDef,
    pub secondary: Option<FeedbackBurstDef>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HitImpactEffectDef {
    pub id: HitImpactEffectId,
    pub flash: FeedbackBurstDef,
    pub accent: Option<FeedbackBurstDef>,
    pub spark_scale: f32,
    pub force_heavy_spark: bool,
}

pub fn feedback_package_for_timeline_cue(
    phase: FeedbackPhase,
    cue: &'static str,
) -> FeedbackPackageDef {
    let id = match cue {
        "startup_A_s" | "startup_A_ss" => FeedbackPackageId::LightStartup,
        "trail_A_s" | "trail_A_ss" | "trail_pig_cat_light_1" | "trail_pig_cat_light_2" => {
            FeedbackPackageId::FirePunchTrail
        }
        "recover_anim_A_s" | "recover_anim_A_ss" => FeedbackPackageId::LightRecover,
        "startup_kiriage" => FeedbackPackageId::LauncherStartup,
        "trail_kiriage" => FeedbackPackageId::LauncherTrail,
        "post_hit_kiriage" => FeedbackPackageId::LauncherAftermath,
        "startup_combo_finisher" => FeedbackPackageId::FinisherStartup,
        "trail_combo_finisher" => FeedbackPackageId::FinisherTrail,
        "combo_finisher_floor_pulse" => FeedbackPackageId::FloorPulse,
        "startup_ultimate_beast"
        | "ultimate_lock_start"
        | "startup_dog_ultimate_beast"
        | "dog_ultimate_lock_start"
        | "startup_fox_ultimate_beast"
        | "fox_ultimate_lock_start"
        | "startup_panda_ultimate_beast"
        | "panda_ultimate_lock_start"
        | "startup_pig_unblockable_grab"
        | "pig_grab_lock_start"
        | "startup_bee_ultimate_swarm"
        | "bee_ultimate_lock_start" => FeedbackPackageId::UltimateStartup,
        "trail_ultimate_catch"
        | "trail_ultimate_scratch"
        | "trail_dog_ultimate_catch"
        | "trail_dog_ultimate_scratch"
        | "trail_fox_ultimate_catch"
        | "trail_fox_ultimate_scratch"
        | "trail_panda_ultimate_catch"
        | "trail_panda_ultimate_scratch"
        | "trail_bee_ultimate_catch"
        | "trail_bee_ultimate_scratch" => FeedbackPackageId::UltimateScratch,
        "trail_pig_unblockable_grab" => FeedbackPackageId::UltimateStartup,
        "pig_grab_crush" | "pig_grab_meat_slam" => FeedbackPackageId::HeavyImpact,
        "charge_ultimate_bomb"
        | "charge_dog_ultimate_bomb"
        | "charge_fox_ultimate_bomb"
        | "charge_panda_ultimate_bomb"
        | "charge_pig_ultimate_bomb"
        | "charge_bee_ultimate_bomb" => FeedbackPackageId::UltimateBomb,
        "recover_ultimate_whiff"
        | "recover_ultimate_bomb"
        | "recover_dog_ultimate_whiff"
        | "recover_dog_ultimate_bomb"
        | "recover_fox_ultimate_whiff"
        | "recover_fox_ultimate_bomb"
        | "recover_panda_ultimate_whiff"
        | "recover_panda_ultimate_bomb"
        | "recover_pig_ultimate_whiff"
        | "recover_pig_ultimate_bomb"
        | "recover_bee_ultimate_whiff"
        | "recover_bee_ultimate_bomb"
        | "recover_bee_ultimate_swarm" => FeedbackPackageId::UltimateRecover,
        "startup_dash_attack" => FeedbackPackageId::DashStartup,
        "trail_dash_attack" => FeedbackPackageId::DashTrail,
        "dash_attack_brake" => FeedbackPackageId::DashBrake,
        "startup_jump_attack"
        | "startup_dog_jump_pounce"
        | "startup_dog_jump_fish"
        | "startup_fox_jump_swipe"
        | "startup_fox_jump_fish"
        | "startup_panda_jump_drop"
        | "startup_panda_jump_fish" => FeedbackPackageId::JumpStartup,
        "trail_jump_attack"
        | "release_jump_x_fish"
        | "trail_dog_jump_pounce"
        | "release_dog_jump_fish"
        | "trail_fox_jump_swipe"
        | "release_fox_jump_fish"
        | "trail_panda_jump_drop"
        | "release_panda_jump_fish" => FeedbackPackageId::JumpTrail,
        "jump_attack_fall"
        | "dog_jump_pounce_fall"
        | "fox_jump_swipe_fall"
        | "panda_jump_drop_fall" => FeedbackPackageId::JumpAftermath,
        "startup_guard_counter" => FeedbackPackageId::GuardCounterStartup,
        "startup_special_cast" => FeedbackPackageId::SpecialCastStartup,
        "release_special_cast" => FeedbackPackageId::SpecialCastRelease,
        "recover_special_cast" => FeedbackPackageId::SpecialCastRecover,
        "startup_item_pickup"
        | "secure_item_pickup"
        | "startup_item_drop"
        | "release_item_drop"
        | "startup_guard_step"
        | "guard_step_recover" => FeedbackPackageId::ItemUtility,
        "startup_quick_stand" | "quick_stand_ready" => FeedbackPackageId::QuickStand,
        "startup_recovery_roll" | "travel_recovery_roll" => FeedbackPackageId::RollTravel,
        "landing_recovery_stick" | "landing_recovery_release" => FeedbackPackageId::LandingStick,
        _ => match phase {
            FeedbackPhase::Startup => FeedbackPackageId::GenericStartup,
            FeedbackPhase::PreHit => FeedbackPackageId::GenericPreHit,
            FeedbackPhase::Impact => FeedbackPackageId::GenericImpact,
            FeedbackPhase::Aftermath => FeedbackPackageId::GenericAftermath,
        },
    };
    feedback_package_definition(id)
}

pub fn feedback_package_for_named_cue(cue: &'static str) -> FeedbackPackageDef {
    let id = match cue {
        "impact_A_s_1" | "impact_A_s_2" | "impact_A_ss_1" | "impact_A_ss_2" | "strike_light" => {
            FeedbackPackageId::LightImpact
        }
        "impact_kiriage_lift" | "impact_kiriage_launch" | "hazard_launch" => {
            FeedbackPackageId::LauncherImpact
        }
        "impact_ultimate_scratch"
        | "impact_dog_ultimate_scratch"
        | "impact_fox_ultimate_scratch"
        | "impact_panda_ultimate_scratch"
        | "impact_bee_ultimate_scratch" => FeedbackPackageId::UltimateScratchImpact,
        "impact_pig_ultimate_scratch" | "impact_pig_ultimate_scratch_heavy" => {
            FeedbackPackageId::HeavyImpact
        }
        "impact_pig_ham_slam" => FeedbackPackageId::PigHamSlamImpact,
        "impact_combo_finisher_lift"
        | "impact_combo_finisher"
        | "impact_ultimate_catch"
        | "impact_dog_ultimate_catch"
        | "impact_fox_ultimate_catch"
        | "impact_panda_ultimate_catch"
        | "impact_pig_ultimate_catch"
        | "impact_bee_ultimate_catch"
        | "impact_dash_shoulder_1"
        | "impact_dash_shoulder_2"
        | "impact_jump_kick"
        | "impact_jump_spike"
        | "impact_dog_jump_pounce"
        | "impact_dog_jump_fish"
        | "impact_fox_jump_swipe"
        | "impact_fox_jump_fish"
        | "impact_panda_jump_drop"
        | "impact_panda_jump_fish"
        | "impact_guard_counter"
        | "strike_heavy"
        | "throw_heavy"
        | "item_melee_heavy"
        | "item_throw_heavy"
        | "item_blast"
        | "ringout_burst" => FeedbackPackageId::HeavyImpact,
        "impact_ultimate_bomb"
        | "impact_dog_ultimate_bomb"
        | "impact_fox_ultimate_bomb"
        | "impact_panda_ultimate_bomb"
        | "impact_pig_ultimate_bomb"
        | "impact_bee_ultimate_bomb" => FeedbackPackageId::UltimateBomb,
        "impact_special_projectile" | "projectile_ping" | "projectile_burst" => {
            FeedbackPackageId::SpecialProjectileImpact
        }
        "impact_special_trap" | "trap_snap" => FeedbackPackageId::SpecialTrapImpact,
        "impact_special_shockwave" | "shockwave_push" => FeedbackPackageId::SpecialShockwaveImpact,
        "impact_special_hazard" | "hazard_tick" => FeedbackPackageId::SpecialHazardImpact,
        "startup_special_projectile" => FeedbackPackageId::SpecialProjectileStartup,
        "release_special_projectile" => FeedbackPackageId::SpecialProjectileRelease,
        "recover_special_projectile" => FeedbackPackageId::SpecialProjectileRecover,
        "startup_special_trap" => FeedbackPackageId::SpecialTrapStartup,
        "arm_special_trap" => FeedbackPackageId::SpecialTrapArm,
        "recover_special_trap" => FeedbackPackageId::SpecialTrapRecover,
        "startup_special_shockwave" => FeedbackPackageId::SpecialShockwaveStartup,
        "release_special_shockwave" => FeedbackPackageId::SpecialShockwaveRelease,
        "recover_special_shockwave" => FeedbackPackageId::SpecialShockwaveRecover,
        "startup_special_hazard" => FeedbackPackageId::SpecialHazardStartup,
        "pulse_special_hazard" | "special_stamina_disrupt" => FeedbackPackageId::SpecialHazardPulse,
        "fade_special_hazard" => FeedbackPackageId::SpecialHazardFade,
        "guard_clang" => FeedbackPackageId::GuardClang,
        "perfect_guard" => FeedbackPackageId::PerfectGuard,
        "guard_break" => FeedbackPackageId::GuardBreak,
        "throw_quick" | "item_melee_light" | "item_throw_light" => FeedbackPackageId::GenericImpact,
        "item_utility" | "match_flow" => FeedbackPackageId::ItemUtility,
        _ => FeedbackPackageId::GenericImpact,
    };
    feedback_package_definition(id)
}

pub fn feedback_package_definition(id: FeedbackPackageId) -> FeedbackPackageDef {
    let mut package = match id {
        FeedbackPackageId::GenericStartup => package(
            id,
            0.9,
            0.9,
            ring(
                FeedbackMaterialId::Blue,
                Vec3::new(0.0, 0.1, 0.0),
                0.2,
                0.28,
                0.95,
            ),
            None,
        ),
        FeedbackPackageId::GenericPreHit => package(
            id,
            1.0,
            1.0,
            trail(
                FeedbackMaterialId::Yellow,
                Vec3::new(0.0, 0.7, 0.5),
                0.18,
                Vec3::new(0.46, 0.28, 1.0),
                Vec3::new(0.08, 0.05, 0.24),
            ),
            None,
        ),
        FeedbackPackageId::GenericImpact => package(
            id,
            1.0,
            1.0,
            spark(
                FeedbackMaterialId::Yellow,
                Vec3::new(0.0, 0.62, 0.18),
                5,
                0.12,
                0.18,
                0.9,
            ),
            None,
        ),
        FeedbackPackageId::GenericAftermath => package(
            id,
            1.0,
            1.0,
            ring(
                FeedbackMaterialId::White,
                Vec3::new(0.0, 0.08, 0.0),
                0.3,
                0.38,
                1.3,
            ),
            None,
        ),
        FeedbackPackageId::LightStartup => package(
            id,
            0.95,
            0.95,
            ring(
                FeedbackMaterialId::Blue,
                Vec3::new(0.0, 0.12, 0.08),
                0.18,
                0.22,
                0.82,
            ),
            Some(puff(
                FeedbackMaterialId::Smoke,
                Vec3::new(0.0, 0.06, -0.2),
                2,
                0.16,
                0.22,
            )),
        ),
        FeedbackPackageId::FirePunchTrail => package(
            id,
            1.08,
            1.08,
            fire_punch(
                FeedbackMaterialId::FirePunch,
                Vec3::new(0.0, 0.68, 0.58),
                0.16,
                Vec3::new(0.88, 0.82, 0.76),
                Vec3::new(0.12, 0.1, 0.18),
            ),
            None,
        ),
        FeedbackPackageId::LightImpact => package(
            id,
            1.05,
            1.05,
            spark(
                FeedbackMaterialId::Yellow,
                Vec3::new(0.0, 0.58, 0.12),
                6,
                0.13,
                0.18,
                0.95,
            ),
            Some(ring(
                FeedbackMaterialId::White,
                Vec3::new(0.0, 0.58, 0.08),
                0.16,
                0.18,
                0.78,
            )),
        ),
        FeedbackPackageId::LightRecover => package(
            id,
            0.95,
            1.0,
            ring(
                FeedbackMaterialId::White,
                Vec3::new(0.0, 0.08, -0.1),
                0.26,
                0.34,
                1.05,
            ),
            None,
        ),
        FeedbackPackageId::LauncherStartup => package(
            id,
            1.35,
            1.25,
            ring(
                FeedbackMaterialId::Cyan,
                Vec3::new(0.0, 0.12, 0.08),
                0.34,
                0.36,
                1.28,
            ),
            Some(column(
                FeedbackMaterialId::Blue,
                Vec3::new(0.0, 0.55, 0.2),
                0.3,
                Vec3::new(0.32, 0.2, 0.32),
                Vec3::new(0.68, 1.2, 0.68),
            )),
        ),
        FeedbackPackageId::LauncherTrail => package(
            id,
            1.45,
            1.3,
            trail(
                FeedbackMaterialId::Cyan,
                Vec3::new(0.0, 0.85, 0.62),
                0.24,
                Vec3::new(0.7, 0.42, 1.45),
                Vec3::new(0.12, 0.08, 0.34),
            ),
            Some(spark(
                FeedbackMaterialId::White,
                Vec3::new(0.0, 0.82, 0.75),
                5,
                0.16,
                0.2,
                0.9,
            )),
        ),
        FeedbackPackageId::LauncherImpact => package(
            id,
            1.35,
            1.25,
            burst(
                FeedbackMaterialId::White,
                Vec3::new(0.0, 0.72, 0.1),
                0.24,
                0.42,
                1.85,
            ),
            Some(spark(
                FeedbackMaterialId::Cyan,
                Vec3::new(0.0, 0.82, 0.08),
                9,
                0.22,
                0.22,
                1.15,
            )),
        ),
        FeedbackPackageId::LauncherAftermath => package(
            id,
            1.25,
            1.2,
            ring(
                FeedbackMaterialId::Cyan,
                Vec3::new(0.0, 0.06, 0.0),
                0.38,
                0.4,
                1.65,
            ),
            Some(puff(
                FeedbackMaterialId::Smoke,
                Vec3::new(0.0, 0.08, -0.18),
                4,
                0.22,
                0.34,
            )),
        ),
        FeedbackPackageId::FinisherStartup => package(
            id,
            1.12,
            1.1,
            ring(
                FeedbackMaterialId::Orange,
                Vec3::new(0.0, 0.12, 0.1),
                0.22,
                0.3,
                1.05,
            ),
            None,
        ),
        FeedbackPackageId::FinisherTrail => package(
            id,
            1.18,
            1.12,
            trail(
                FeedbackMaterialId::Orange,
                Vec3::new(0.0, 0.72, 0.58),
                0.2,
                Vec3::new(0.58, 0.34, 1.28),
                Vec3::new(0.1, 0.06, 0.3),
            ),
            None,
        ),
        FeedbackPackageId::PigHamSlamImpact => package(
            id,
            1.08,
            1.08,
            heart(
                FeedbackMaterialId::Pink,
                Vec3::new(0.0, 0.68, 0.02),
                3,
                0.13,
                0.72,
                0.24,
            ),
            Some(burst(
                FeedbackMaterialId::Orange,
                Vec3::new(0.0, 0.12, 0.08),
                0.22,
                0.28,
                0.78,
            )),
        ),
        FeedbackPackageId::HeavyImpact => package(
            id,
            1.22,
            1.15,
            burst(
                FeedbackMaterialId::Orange,
                Vec3::new(0.0, 0.62, 0.1),
                0.22,
                0.34,
                1.45,
            ),
            Some(spark(
                FeedbackMaterialId::Yellow,
                Vec3::new(0.0, 0.65, 0.12),
                8,
                0.2,
                0.22,
                1.08,
            )),
        ),
        FeedbackPackageId::FloorPulse => package(
            id,
            1.18,
            1.12,
            ring(
                FeedbackMaterialId::Orange,
                Vec3::new(0.0, 0.06, 0.1),
                0.38,
                0.42,
                1.95,
            ),
            Some(puff(
                FeedbackMaterialId::Smoke,
                Vec3::new(0.0, 0.08, 0.0),
                5,
                0.25,
                0.36,
            )),
        ),
        FeedbackPackageId::UltimateStartup => package(
            id,
            1.34,
            1.24,
            ring(
                FeedbackMaterialId::Red,
                Vec3::new(0.0, 0.18, 0.12),
                0.28,
                0.42,
                1.45,
            ),
            Some(puff(
                FeedbackMaterialId::Smoke,
                Vec3::new(0.0, 0.12, -0.16),
                5,
                0.22,
                0.38,
            )),
        ),
        FeedbackPackageId::UltimateScratch => package(
            id,
            1.28,
            1.2,
            crescent_slash(
                FeedbackMaterialId::SlashRed,
                Vec3::new(0.0, 0.68, 0.28),
                0.42,
                Vec3::new(0.96, 0.92, 1.0),
                Vec3::new(1.28, 1.12, 1.0),
                Vec3::new(0.08, 0.12, 0.5),
            ),
            Some(spark(
                FeedbackMaterialId::Orange,
                Vec3::new(0.0, 0.68, 0.24),
                7,
                0.22,
                0.18,
                0.9,
            )),
        ),
        FeedbackPackageId::UltimateScratchImpact => package(
            id,
            1.34,
            1.26,
            crescent_slash(
                FeedbackMaterialId::SlashRed,
                Vec3::new(0.0, -0.22, -0.1),
                0.46,
                Vec3::new(1.22, 1.12, 1.0),
                Vec3::new(1.52, 1.3, 1.0),
                Vec3::new(0.06, 0.08, 0.5),
            ),
            Some(spark(
                FeedbackMaterialId::Orange,
                Vec3::new(0.0, -0.08, -0.04),
                9,
                0.28,
                0.2,
                1.05,
            )),
        ),
        FeedbackPackageId::UltimateBomb => package(
            id,
            1.58,
            1.42,
            burst(
                FeedbackMaterialId::Red,
                Vec3::new(0.0, 0.46, 0.22),
                0.34,
                0.54,
                2.25,
            ),
            Some(spark(
                FeedbackMaterialId::Orange,
                Vec3::new(0.0, 0.62, 0.12),
                14,
                0.3,
                0.26,
                1.38,
            )),
        ),
        FeedbackPackageId::UltimateRecover => package(
            id,
            1.12,
            1.02,
            ring(
                FeedbackMaterialId::Smoke,
                Vec3::new(0.0, 0.06, -0.08),
                0.34,
                0.36,
                1.52,
            ),
            Some(puff(
                FeedbackMaterialId::Smoke,
                Vec3::new(0.0, 0.08, -0.12),
                4,
                0.22,
                0.34,
            )),
        ),
        FeedbackPackageId::DashStartup => package(
            id,
            0.95,
            0.95,
            trail(
                FeedbackMaterialId::Blue,
                Vec3::new(0.0, 0.38, -0.42),
                0.2,
                Vec3::new(0.48, 0.26, 1.25),
                Vec3::new(0.08, 0.04, 0.28),
            ),
            None,
        ),
        FeedbackPackageId::DashTrail => package(
            id,
            1.08,
            1.02,
            trail(
                FeedbackMaterialId::Yellow,
                Vec3::new(0.0, 0.62, 0.74),
                0.16,
                Vec3::new(0.55, 0.28, 1.35),
                Vec3::new(0.08, 0.05, 0.22),
            ),
            Some(spark(
                FeedbackMaterialId::Yellow,
                Vec3::new(0.0, 0.62, 0.95),
                3,
                0.1,
                0.13,
                0.6,
            )),
        ),
        FeedbackPackageId::DashBrake => package(
            id,
            0.98,
            1.0,
            puff(
                FeedbackMaterialId::Smoke,
                Vec3::new(0.0, 0.08, -0.42),
                5,
                0.25,
                0.32,
            ),
            None,
        ),
        FeedbackPackageId::JumpStartup => package(
            id,
            0.9,
            0.95,
            puff(
                FeedbackMaterialId::Smoke,
                Vec3::new(0.0, 0.08, -0.1),
                3,
                0.2,
                0.28,
            ),
            None,
        ),
        FeedbackPackageId::JumpTrail => package(
            id,
            1.08,
            1.05,
            trail(
                FeedbackMaterialId::Cyan,
                Vec3::new(0.0, 0.72, 0.5),
                0.17,
                Vec3::new(0.48, 0.28, 1.1),
                Vec3::new(0.08, 0.05, 0.24),
            ),
            None,
        ),
        FeedbackPackageId::JumpAftermath => package(
            id,
            0.95,
            1.0,
            trail(
                FeedbackMaterialId::Smoke,
                Vec3::new(0.0, 0.48, -0.18),
                0.28,
                Vec3::new(0.34, 0.2, 0.75),
                Vec3::new(0.06, 0.04, 0.18),
            ),
            None,
        ),
        FeedbackPackageId::GuardCounterStartup => package(
            id,
            1.12,
            1.1,
            ring(
                FeedbackMaterialId::Cyan,
                Vec3::new(0.0, 0.62, 0.24),
                0.2,
                0.24,
                1.0,
            ),
            Some(spark(
                FeedbackMaterialId::White,
                Vec3::new(0.0, 0.66, 0.36),
                4,
                0.08,
                0.14,
                0.65,
            )),
        ),
        FeedbackPackageId::GuardClang => package(
            id,
            1.0,
            1.05,
            ring(
                FeedbackMaterialId::Cyan,
                Vec3::new(0.0, 0.62, 0.05),
                0.18,
                0.24,
                0.96,
            ),
            Some(spark(
                FeedbackMaterialId::Cyan,
                Vec3::new(0.0, 0.66, 0.08),
                5,
                0.12,
                0.16,
                0.75,
            )),
        ),
        FeedbackPackageId::PerfectGuard => package(
            id,
            1.2,
            1.2,
            ring(
                FeedbackMaterialId::White,
                Vec3::new(0.0, 0.65, 0.04),
                0.22,
                0.24,
                1.28,
            ),
            Some(spark(
                FeedbackMaterialId::Cyan,
                Vec3::new(0.0, 0.7, 0.08),
                7,
                0.16,
                0.18,
                0.9,
            )),
        ),
        FeedbackPackageId::GuardBreak => package(
            id,
            1.4,
            1.25,
            burst(
                FeedbackMaterialId::Red,
                Vec3::new(0.0, 0.66, 0.08),
                0.28,
                0.34,
                1.55,
            ),
            Some(ring(
                FeedbackMaterialId::Orange,
                Vec3::new(0.0, 0.14, 0.0),
                0.32,
                0.4,
                1.8,
            )),
        ),
        FeedbackPackageId::QuickStand => package(
            id,
            0.9,
            1.0,
            ring(
                FeedbackMaterialId::White,
                Vec3::new(0.0, 0.08, -0.08),
                0.22,
                0.24,
                0.95,
            ),
            Some(puff(
                FeedbackMaterialId::Smoke,
                Vec3::new(0.0, 0.08, -0.18),
                3,
                0.16,
                0.26,
            )),
        ),
        FeedbackPackageId::RollTravel => package(
            id,
            0.95,
            1.0,
            trail(
                FeedbackMaterialId::Smoke,
                Vec3::new(0.0, 0.22, -0.32),
                0.28,
                Vec3::new(0.48, 0.22, 1.1),
                Vec3::new(0.08, 0.04, 0.26),
            ),
            None,
        ),
        FeedbackPackageId::LandingStick => package(
            id,
            1.0,
            1.05,
            ring(
                FeedbackMaterialId::Smoke,
                Vec3::new(0.0, 0.06, 0.0),
                0.3,
                0.34,
                1.28,
            ),
            Some(puff(
                FeedbackMaterialId::Smoke,
                Vec3::new(0.0, 0.08, 0.0),
                5,
                0.24,
                0.32,
            )),
        ),
        FeedbackPackageId::SpecialCastStartup => package(
            id,
            1.05,
            1.05,
            ring(
                FeedbackMaterialId::Blue,
                Vec3::new(0.0, 0.16, 0.0),
                0.22,
                0.32,
                1.1,
            ),
            Some(column(
                FeedbackMaterialId::Cyan,
                Vec3::new(0.0, 0.58, 0.1),
                0.22,
                Vec3::new(0.24, 0.22, 0.24),
                Vec3::new(0.45, 0.9, 0.45),
            )),
        ),
        FeedbackPackageId::SpecialCastRelease => package(
            id,
            1.1,
            1.08,
            trail(
                FeedbackMaterialId::Cyan,
                Vec3::new(0.0, 0.7, 0.55),
                0.18,
                Vec3::new(0.46, 0.3, 1.2),
                Vec3::new(0.08, 0.05, 0.25),
            ),
            None,
        ),
        FeedbackPackageId::SpecialCastRecover => package(
            id,
            0.95,
            1.0,
            ring(
                FeedbackMaterialId::White,
                Vec3::new(0.0, 0.08, -0.04),
                0.28,
                0.3,
                1.1,
            ),
            None,
        ),
        FeedbackPackageId::SpecialProjectileStartup => package(
            id,
            1.05,
            1.08,
            ring(
                FeedbackMaterialId::Cyan,
                Vec3::new(0.0, 0.78, 0.35),
                0.2,
                0.2,
                0.78,
            ),
            Some(spark(
                FeedbackMaterialId::Cyan,
                Vec3::new(0.0, 0.82, 0.42),
                4,
                0.1,
                0.16,
                0.7,
            )),
        ),
        FeedbackPackageId::SpecialProjectileRelease => package(
            id,
            1.08,
            1.08,
            trail(
                FeedbackMaterialId::Cyan,
                Vec3::new(0.0, 0.9, 0.72),
                0.2,
                Vec3::new(0.34, 0.22, 1.25),
                Vec3::new(0.06, 0.04, 0.24),
            ),
            None,
        ),
        FeedbackPackageId::SpecialProjectileImpact => package(
            id,
            1.1,
            1.08,
            burst(
                FeedbackMaterialId::Cyan,
                Vec3::new(0.0, 0.6, 0.02),
                0.2,
                0.26,
                1.08,
            ),
            Some(spark(
                FeedbackMaterialId::White,
                Vec3::new(0.0, 0.64, 0.02),
                6,
                0.16,
                0.18,
                0.85,
            )),
        ),
        FeedbackPackageId::SpecialProjectileRecover => package(
            id,
            0.92,
            1.0,
            ring(
                FeedbackMaterialId::Blue,
                Vec3::new(0.0, 0.08, -0.08),
                0.28,
                0.28,
                0.95,
            ),
            None,
        ),
        FeedbackPackageId::SpecialTrapStartup => package(
            id,
            1.0,
            1.05,
            ring(
                FeedbackMaterialId::Orange,
                Vec3::new(0.0, 0.08, 0.82),
                0.26,
                0.28,
                1.0,
            ),
            None,
        ),
        FeedbackPackageId::SpecialTrapArm => package(
            id,
            1.05,
            1.08,
            ring(
                FeedbackMaterialId::Yellow,
                Vec3::new(0.0, 0.06, 0.0),
                0.34,
                0.42,
                1.55,
            ),
            Some(spark(
                FeedbackMaterialId::Orange,
                Vec3::new(0.0, 0.18, 0.0),
                4,
                0.22,
                0.2,
                0.7,
            )),
        ),
        FeedbackPackageId::SpecialTrapImpact => package(
            id,
            1.2,
            1.12,
            burst(
                FeedbackMaterialId::Orange,
                Vec3::new(0.0, 0.45, 0.0),
                0.22,
                0.3,
                1.35,
            ),
            Some(ring(
                FeedbackMaterialId::Red,
                Vec3::new(0.0, 0.08, 0.0),
                0.28,
                0.42,
                1.72,
            )),
        ),
        FeedbackPackageId::SpecialTrapRecover => package(
            id,
            0.92,
            1.0,
            ring(
                FeedbackMaterialId::Smoke,
                Vec3::new(0.0, 0.06, 0.0),
                0.34,
                0.36,
                1.2,
            ),
            None,
        ),
        FeedbackPackageId::SpecialShockwaveStartup => package(
            id,
            1.1,
            1.08,
            ring(
                FeedbackMaterialId::White,
                Vec3::new(0.0, 0.08, 0.0),
                0.16,
                0.22,
                0.7,
            ),
            Some(ring(
                FeedbackMaterialId::Cyan,
                Vec3::new(0.0, 0.1, 0.0),
                0.18,
                0.38,
                0.52,
            )),
        ),
        FeedbackPackageId::SpecialShockwaveRelease => package(
            id,
            1.24,
            1.15,
            ring(
                FeedbackMaterialId::Cyan,
                Vec3::new(0.0, 0.08, 0.0),
                0.34,
                0.48,
                2.25,
            ),
            Some(puff(
                FeedbackMaterialId::Smoke,
                Vec3::new(0.0, 0.08, 0.0),
                6,
                0.32,
                0.36,
            )),
        ),
        FeedbackPackageId::SpecialShockwaveImpact => package(
            id,
            1.18,
            1.1,
            burst(
                FeedbackMaterialId::Cyan,
                Vec3::new(0.0, 0.48, 0.0),
                0.22,
                0.3,
                1.28,
            ),
            Some(ring(
                FeedbackMaterialId::White,
                Vec3::new(0.0, 0.1, 0.0),
                0.22,
                0.32,
                1.35,
            )),
        ),
        FeedbackPackageId::SpecialShockwaveRecover => package(
            id,
            0.92,
            1.0,
            ring(
                FeedbackMaterialId::Smoke,
                Vec3::new(0.0, 0.06, 0.0),
                0.32,
                0.4,
                1.3,
            ),
            None,
        ),
        FeedbackPackageId::SpecialHazardStartup => package(
            id,
            1.02,
            1.05,
            ring(
                FeedbackMaterialId::Red,
                Vec3::new(0.0, 0.08, 1.0),
                0.32,
                0.36,
                1.2,
            ),
            Some(column(
                FeedbackMaterialId::Orange,
                Vec3::new(0.0, 0.28, 1.0),
                0.28,
                Vec3::new(0.28, 0.08, 0.28),
                Vec3::new(0.72, 0.34, 0.72),
            )),
        ),
        FeedbackPackageId::SpecialHazardPulse => package(
            id,
            1.0,
            1.08,
            ring(
                FeedbackMaterialId::Red,
                Vec3::new(0.0, 0.08, 0.0),
                0.28,
                0.48,
                1.7,
            ),
            Some(spark(
                FeedbackMaterialId::Orange,
                Vec3::new(0.0, 0.24, 0.0),
                5,
                0.28,
                0.2,
                0.65,
            )),
        ),
        FeedbackPackageId::SpecialHazardImpact => package(
            id,
            1.08,
            1.08,
            spark(
                FeedbackMaterialId::Red,
                Vec3::new(0.0, 0.55, 0.0),
                7,
                0.16,
                0.2,
                0.82,
            ),
            Some(burst(
                FeedbackMaterialId::Orange,
                Vec3::new(0.0, 0.45, 0.0),
                0.18,
                0.22,
                1.02,
            )),
        ),
        FeedbackPackageId::SpecialHazardFade => package(
            id,
            0.88,
            0.95,
            ring(
                FeedbackMaterialId::Smoke,
                Vec3::new(0.0, 0.06, 0.0),
                0.42,
                0.52,
                1.85,
            ),
            None,
        ),
        FeedbackPackageId::ItemUtility => package(
            id,
            0.82,
            0.95,
            ring(
                FeedbackMaterialId::White,
                Vec3::new(0.0, 0.1, 0.0),
                0.18,
                0.22,
                0.72,
            ),
            None,
        ),
    };

    if matches!(
        id,
        FeedbackPackageId::LauncherStartup
            | FeedbackPackageId::LauncherTrail
            | FeedbackPackageId::SpecialCastStartup
            | FeedbackPackageId::SpecialCastRelease
            | FeedbackPackageId::SpecialProjectileStartup
            | FeedbackPackageId::SpecialProjectileRelease
            | FeedbackPackageId::SpecialShockwaveRelease
            | FeedbackPackageId::UltimateStartup
            | FeedbackPackageId::UltimateBomb
    ) {
        package.reference_effect = Some("/effect/special_parts.oa");
    }
    if matches!(
        id,
        FeedbackPackageId::LauncherStartup
            | FeedbackPackageId::SpecialCastStartup
            | FeedbackPackageId::SpecialShockwaveRelease
            | FeedbackPackageId::UltimateStartup
            | FeedbackPackageId::UltimateBomb
    ) {
        package.reference_sound = Some("/sound/25.sr");
    }
    package
}

pub fn hit_impact_effect_for_feedback_package(
    package_id: FeedbackPackageId,
    heavy: bool,
) -> HitImpactEffectId {
    match package_id {
        FeedbackPackageId::LightImpact | FeedbackPackageId::GenericImpact => {
            if heavy {
                HitImpactEffectId::GenericHeavy
            } else {
                HitImpactEffectId::GenericLight
            }
        }
        FeedbackPackageId::LauncherImpact => HitImpactEffectId::LauncherCyan,
        FeedbackPackageId::PigHamSlamImpact => HitImpactEffectId::PigHamSlamHeart,
        FeedbackPackageId::UltimateScratchImpact => HitImpactEffectId::UltimateSlashRed,
        FeedbackPackageId::UltimateBomb => HitImpactEffectId::UltimateBombRed,
        FeedbackPackageId::SpecialProjectileImpact => HitImpactEffectId::LauncherCyan,
        FeedbackPackageId::SpecialTrapImpact
        | FeedbackPackageId::SpecialShockwaveImpact
        | FeedbackPackageId::SpecialHazardImpact
        | FeedbackPackageId::GuardBreak
        | FeedbackPackageId::HeavyImpact => HitImpactEffectId::GenericHeavy,
        _ => {
            if heavy {
                HitImpactEffectId::GenericHeavy
            } else {
                HitImpactEffectId::GenericLight
            }
        }
    }
}

pub fn hit_impact_effect_definition(id: HitImpactEffectId) -> HitImpactEffectDef {
    match id {
        HitImpactEffectId::GenericLight => hit_effect(
            id,
            burst(FeedbackMaterialId::White, Vec3::ZERO, 0.12, 0.22, 1.05),
            Some(ring(
                FeedbackMaterialId::Yellow,
                Vec3::ZERO,
                0.15,
                0.16,
                0.82,
            )),
            1.08,
            false,
        ),
        HitImpactEffectId::GenericHeavy => hit_effect(
            id,
            burst(FeedbackMaterialId::Orange, Vec3::ZERO, 0.16, 0.3, 1.48),
            Some(ring(
                FeedbackMaterialId::White,
                Vec3::ZERO,
                0.18,
                0.22,
                1.16,
            )),
            1.22,
            true,
        ),
        HitImpactEffectId::LauncherCyan => hit_effect(
            id,
            burst(FeedbackMaterialId::White, Vec3::ZERO, 0.15, 0.28, 1.55),
            Some(column(
                FeedbackMaterialId::Cyan,
                Vec3::new(0.0, 0.02, 0.0),
                0.22,
                Vec3::new(0.28, 0.25, 0.28),
                Vec3::new(0.58, 1.25, 0.58),
            )),
            1.34,
            true,
        ),
        HitImpactEffectId::GroundBounceOrange => hit_effect(
            id,
            burst(FeedbackMaterialId::Orange, Vec3::ZERO, 0.18, 0.34, 1.72),
            Some(ring(
                FeedbackMaterialId::Orange,
                Vec3::new(0.0, -0.04, 0.0),
                0.2,
                0.28,
                1.42,
            )),
            1.3,
            true,
        ),
        HitImpactEffectId::UltimateSlashRed => hit_effect(
            id,
            burst(FeedbackMaterialId::Red, Vec3::ZERO, 0.16, 0.36, 1.85),
            Some(crescent_slash(
                FeedbackMaterialId::SlashRed,
                Vec3::ZERO,
                0.32,
                Vec3::new(1.06, 0.96, 1.0),
                Vec3::new(1.55, 1.32, 1.0),
                Vec3::new(0.06, 0.08, 0.55),
            )),
            1.42,
            true,
        ),
        HitImpactEffectId::UltimateBombRed => hit_effect(
            id,
            burst(FeedbackMaterialId::Red, Vec3::ZERO, 0.24, 0.48, 2.55),
            Some(ring(
                FeedbackMaterialId::Orange,
                Vec3::ZERO,
                0.28,
                0.42,
                2.3,
            )),
            1.65,
            true,
        ),
        HitImpactEffectId::LightBlue => hit_effect(
            id,
            burst(FeedbackMaterialId::Blue, Vec3::ZERO, 0.13, 0.24, 1.18),
            Some(ring(FeedbackMaterialId::Cyan, Vec3::ZERO, 0.15, 0.16, 0.9)),
            1.12,
            false,
        ),
        HitImpactEffectId::PigHamSlamHeart => hit_effect(
            id,
            burst(FeedbackMaterialId::Orange, Vec3::ZERO, 0.18, 0.34, 1.6),
            Some(heart(
                FeedbackMaterialId::Pink,
                Vec3::new(0.0, 0.04, 0.0),
                3,
                0.12,
                0.55,
                0.22,
            )),
            1.32,
            true,
        ),
        HitImpactEffectId::PigHamSwing => hit_effect(
            id,
            burst(FeedbackMaterialId::Orange, Vec3::ZERO, 0.17, 0.32, 1.75),
            Some(trail(
                FeedbackMaterialId::Orange,
                Vec3::new(0.0, 0.02, 0.08),
                0.2,
                Vec3::new(0.7, 0.32, 1.2),
                Vec3::new(0.08, 0.05, 0.24),
            )),
            1.38,
            true,
        ),
        HitImpactEffectId::PigAirMeatSlam => hit_effect(
            id,
            burst(FeedbackMaterialId::White, Vec3::ZERO, 0.18, 0.38, 1.95),
            Some(column(
                FeedbackMaterialId::Cyan,
                Vec3::new(0.0, -0.02, 0.0),
                0.24,
                Vec3::new(0.34, 0.28, 0.34),
                Vec3::new(0.72, 1.5, 0.72),
            )),
            1.48,
            true,
        ),
    }
}

fn hit_effect(
    id: HitImpactEffectId,
    flash: FeedbackBurstDef,
    accent: Option<FeedbackBurstDef>,
    spark_scale: f32,
    force_heavy_spark: bool,
) -> HitImpactEffectDef {
    HitImpactEffectDef {
        id,
        flash,
        accent,
        spark_scale,
        force_heavy_spark,
    }
}

fn package(
    id: FeedbackPackageId,
    shake_scale: f32,
    hud_flash_scale: f32,
    primary: FeedbackBurstDef,
    secondary: Option<FeedbackBurstDef>,
) -> FeedbackPackageDef {
    FeedbackPackageDef {
        id,
        reference_effect: None,
        reference_sound: None,
        shake_scale,
        hud_flash_scale,
        primary,
        secondary,
    }
}

fn ring(
    material: FeedbackMaterialId,
    offset: Vec3,
    lifetime: f32,
    start: f32,
    end: f32,
) -> FeedbackBurstDef {
    FeedbackBurstDef {
        visual: FeedbackVisualKind::Ring,
        material,
        count: 1,
        offset,
        spread: 0.0,
        lifetime,
        velocity: Vec3::Y * 0.12,
        spin: Vec3::new(0.0, 6.0, 0.0),
        start_scale: Vec3::splat(start),
        end_scale: Vec3::splat(end),
    }
}

fn trail(
    material: FeedbackMaterialId,
    offset: Vec3,
    lifetime: f32,
    start: Vec3,
    end: Vec3,
) -> FeedbackBurstDef {
    FeedbackBurstDef {
        visual: FeedbackVisualKind::Trail,
        material,
        count: 1,
        offset,
        spread: 0.0,
        lifetime,
        velocity: Vec3::new(0.0, 0.0, 0.7),
        spin: Vec3::ZERO,
        start_scale: start,
        end_scale: end,
    }
}

fn fire_punch(
    material: FeedbackMaterialId,
    offset: Vec3,
    lifetime: f32,
    start: Vec3,
    end: Vec3,
) -> FeedbackBurstDef {
    FeedbackBurstDef {
        visual: FeedbackVisualKind::FirePunch,
        material,
        count: 1,
        offset,
        spread: 0.0,
        lifetime,
        velocity: Vec3::new(0.0, 0.06, 0.28),
        spin: Vec3::ZERO,
        start_scale: start,
        end_scale: end,
    }
}

fn crescent_slash(
    material: FeedbackMaterialId,
    offset: Vec3,
    lifetime: f32,
    start: Vec3,
    end: Vec3,
    velocity: Vec3,
) -> FeedbackBurstDef {
    FeedbackBurstDef {
        visual: FeedbackVisualKind::CrescentSlash,
        material,
        count: 3,
        offset,
        spread: 0.14,
        lifetime,
        velocity,
        spin: Vec3::new(0.0, 0.0, -4.6),
        start_scale: start,
        end_scale: end,
    }
}

fn heart(
    material: FeedbackMaterialId,
    offset: Vec3,
    count: u8,
    spread: f32,
    lifetime: f32,
    scale: f32,
) -> FeedbackBurstDef {
    FeedbackBurstDef {
        visual: FeedbackVisualKind::Heart,
        material,
        count,
        offset,
        spread,
        lifetime,
        velocity: Vec3::new(0.0, 0.72, 0.18),
        spin: Vec3::new(0.0, 3.2, 1.2),
        start_scale: Vec3::splat(scale),
        end_scale: Vec3::splat(0.03),
    }
}

fn spark(
    material: FeedbackMaterialId,
    offset: Vec3,
    count: u8,
    spread: f32,
    lifetime: f32,
    scale: f32,
) -> FeedbackBurstDef {
    FeedbackBurstDef {
        visual: FeedbackVisualKind::Spark,
        material,
        count,
        offset,
        spread,
        lifetime,
        velocity: Vec3::new(0.0, 0.45, 2.4),
        spin: Vec3::new(4.0, 8.0, 2.0),
        start_scale: Vec3::splat(scale),
        end_scale: Vec3::splat(0.08),
    }
}

fn puff(
    material: FeedbackMaterialId,
    offset: Vec3,
    count: u8,
    spread: f32,
    lifetime: f32,
) -> FeedbackBurstDef {
    FeedbackBurstDef {
        visual: FeedbackVisualKind::Puff,
        material,
        count,
        offset,
        spread,
        lifetime,
        velocity: Vec3::new(0.0, 0.18, 0.9),
        spin: Vec3::new(2.0, 1.0, 0.5),
        start_scale: Vec3::splat(0.46),
        end_scale: Vec3::splat(0.08),
    }
}

fn column(
    material: FeedbackMaterialId,
    offset: Vec3,
    lifetime: f32,
    start: Vec3,
    end: Vec3,
) -> FeedbackBurstDef {
    FeedbackBurstDef {
        visual: FeedbackVisualKind::Column,
        material,
        count: 1,
        offset,
        spread: 0.0,
        lifetime,
        velocity: Vec3::Y * 0.18,
        spin: Vec3::new(0.0, 4.0, 0.0),
        start_scale: start,
        end_scale: end,
    }
}

fn burst(
    material: FeedbackMaterialId,
    offset: Vec3,
    lifetime: f32,
    start: f32,
    end: f32,
) -> FeedbackBurstDef {
    FeedbackBurstDef {
        visual: FeedbackVisualKind::Burst,
        material,
        count: 1,
        offset,
        spread: 0.0,
        lifetime,
        velocity: Vec3::Y * 0.2,
        spin: Vec3::new(3.0, 6.0, 4.0),
        start_scale: Vec3::splat(start),
        end_scale: Vec3::splat(end),
    }
}

pub fn spawn_feedback_package(
    commands: &mut Commands,
    assets: &EffectAssets,
    position: Vec3,
    facing: Vec3,
    package_id: FeedbackPackageId,
) {
    let package = feedback_package_definition(package_id);
    spawn_feedback_burst(commands, assets, position, facing, package.primary);
    if let Some(secondary) = package.secondary {
        spawn_feedback_burst(commands, assets, position, facing, secondary);
    }
}

pub fn spawn_hit_impact_effect(
    commands: &mut Commands,
    assets: &EffectAssets,
    position: Vec3,
    facing: Vec3,
    element: DamageElement,
    heavy: bool,
    spark_scale: f32,
    effect_id: HitImpactEffectId,
    include_accent: bool,
) {
    let effect = hit_impact_effect_definition(effect_id);
    spawn_feedback_burst(commands, assets, position, facing, effect.flash);
    spawn_element_hit_spark(
        commands,
        assets,
        position,
        element,
        heavy || effect.force_heavy_spark,
        spark_scale * effect.spark_scale,
    );
    if include_accent && let Some(accent) = effect.accent {
        spawn_feedback_burst(commands, assets, position, facing, accent);
    }
}

pub fn spawn_light_fire_punch(
    commands: &mut Commands,
    assets: &EffectAssets,
    position: Vec3,
    facing: Vec3,
    visual_side: f32,
    palette: FirePunchPalette,
) {
    let (translation, direction) = light_fire_punch_anchor(position, facing, visual_side);
    let start_scale = Vec3::new(1.22, 1.02, 0.96);
    let end_scale = Vec3::new(0.16, 0.12, 0.2);

    commands.spawn((
        Mesh3d(assets.fire_punch_mesh.clone()),
        MeshMaterial3d(fire_punch_material_for_palette(assets, palette)),
        Transform::from_translation(translation)
            .with_rotation(Quat::from_rotation_y(direction.x.atan2(direction.z)))
            .with_scale(start_scale),
        VisualEffect {
            kind: EffectKind::FirePunch,
            lifetime: 0.22,
            age: 0.0,
            velocity: direction * 0.42 + Vec3::Y * 0.06,
            spin: Vec3::ZERO,
            start_scale,
            end_scale,
        },
    ));
}

fn fire_punch_material_for_palette(
    assets: &EffectAssets,
    palette: FirePunchPalette,
) -> Handle<StandardMaterial> {
    match palette {
        FirePunchPalette::Red => assets.fire_punch.clone(),
        FirePunchPalette::Blue => assets.fire_punch_blue.clone(),
    }
}

fn light_fire_punch_anchor(position: Vec3, facing: Vec3, visual_side: f32) -> (Vec3, Vec3) {
    let facing = facing.normalize_or_zero();
    let forward = if facing.length_squared() > 0.0 {
        facing
    } else {
        Vec3::Z
    };
    let right = Vec3::new(forward.z, 0.0, -forward.x).normalize_or_zero();
    let forward_corner_side = if visual_side < 0.0 { 1.0 } else { -1.0 };
    let direction = (forward * 0.96 + right * forward_corner_side * 0.33).normalize_or_zero();
    let fist_corner =
        position + Vec3::Y * 0.74 + forward * 0.48 + right * forward_corner_side * 0.44;

    (fist_corner + direction * 0.24, direction)
}

fn spawn_feedback_burst(
    commands: &mut Commands,
    assets: &EffectAssets,
    position: Vec3,
    facing: Vec3,
    burst: FeedbackBurstDef,
) {
    let facing = facing.normalize_or_zero();
    let forward = if facing.length_squared() > 0.0 {
        facing
    } else {
        Vec3::Z
    };
    let right = Vec3::new(forward.z, 0.0, -forward.x).normalize_or_zero();
    let yaw = forward.x.atan2(forward.z);
    let count = burst.count.max(1);
    for i in 0..count {
        let angle = i as f32 / count as f32 * PI * 2.0;
        let claw_index = i as f32 - (count as f32 - 1.0) * 0.5;
        let scatter = if burst.visual == FeedbackVisualKind::CrescentSlash && count > 1 {
            right * claw_index * burst.spread
                + Vec3::Y * claw_index.abs() * 0.05
                + forward * claw_index * 0.04
        } else if burst.visual == FeedbackVisualKind::Heart && count > 1 {
            right * claw_index * burst.spread + Vec3::Y * claw_index.abs() * 0.08
                - forward * claw_index.abs() * 0.03
        } else if burst.visual == FeedbackVisualKind::Trail && count == 3 {
            right * claw_index * burst.spread + Vec3::Y * claw_index * 0.06
        } else if burst.visual == FeedbackVisualKind::FirePunch && count > 1 {
            right * claw_index * burst.spread + forward * claw_index.abs() * -0.03
        } else if count > 1 {
            Vec3::new(
                angle.cos() * burst.spread,
                if i % 2 == 0 { 0.0 } else { burst.spread * 0.25 },
                angle.sin() * burst.spread,
            )
        } else {
            Vec3::ZERO
        };
        let local = local_to_world(right, forward, burst.offset);
        let velocity =
            local_to_world(right, forward, burst.velocity) + scatter.normalize_or_zero() * 0.9;
        let camera_readability_offset = if burst.visual == FeedbackVisualKind::CrescentSlash {
            Vec3::Z * 0.38
        } else {
            Vec3::ZERO
        };
        let mut transform =
            Transform::from_translation(position + local + scatter + camera_readability_offset)
                .with_scale(burst.start_scale);
        transform.rotation = match burst.visual {
            FeedbackVisualKind::Ring => Quat::from_rotation_x(PI * 0.5),
            FeedbackVisualKind::Trail => {
                Quat::from_rotation_y(yaw) * Quat::from_rotation_z(claw_index * -0.18)
            }
            FeedbackVisualKind::FirePunch => Quat::from_rotation_y(yaw),
            FeedbackVisualKind::CrescentSlash => {
                Quat::from_rotation_y(yaw) * Quat::from_rotation_z(PI - 0.38 + claw_index * 0.22)
            }
            FeedbackVisualKind::Heart => {
                Quat::from_rotation_y(yaw) * Quat::from_rotation_z(claw_index * 0.18)
            }
            FeedbackVisualKind::Spark => Quat::from_rotation_y(-angle),
            FeedbackVisualKind::Puff | FeedbackVisualKind::Column | FeedbackVisualKind::Burst => {
                Quat::IDENTITY
            }
        };
        commands.spawn((
            Mesh3d(mesh_for_visual(assets, burst.visual)),
            MeshMaterial3d(material_for_id(assets, burst.material)),
            transform,
            VisualEffect {
                kind: effect_kind_for_visual(burst.visual),
                lifetime: burst.lifetime,
                age: 0.0,
                velocity,
                spin: burst.spin,
                start_scale: burst.start_scale,
                end_scale: burst.end_scale,
            },
        ));
    }
}

fn local_to_world(right: Vec3, forward: Vec3, local: Vec3) -> Vec3 {
    right * local.x + Vec3::Y * local.y + forward * local.z
}

fn mesh_for_visual(assets: &EffectAssets, visual: FeedbackVisualKind) -> Handle<Mesh> {
    match visual {
        FeedbackVisualKind::Ring => assets.ring_mesh.clone(),
        FeedbackVisualKind::Trail => assets.trail_mesh.clone(),
        FeedbackVisualKind::FirePunch => assets.fire_punch_mesh.clone(),
        FeedbackVisualKind::CrescentSlash => assets.crescent_mesh.clone(),
        FeedbackVisualKind::Heart => assets.heart_mesh.clone(),
        FeedbackVisualKind::Spark => assets.spark_mesh.clone(),
        FeedbackVisualKind::Puff => assets.puff_mesh.clone(),
        FeedbackVisualKind::Column => assets.column_mesh.clone(),
        FeedbackVisualKind::Burst => assets.blast_mesh.clone(),
    }
}

fn material_for_id(
    assets: &EffectAssets,
    material: FeedbackMaterialId,
) -> Handle<StandardMaterial> {
    match material {
        FeedbackMaterialId::Yellow => assets.yellow.clone(),
        FeedbackMaterialId::Orange => assets.orange.clone(),
        FeedbackMaterialId::Cyan => assets.cyan.clone(),
        FeedbackMaterialId::Blue => assets.blue.clone(),
        FeedbackMaterialId::Smoke => assets.smoke.clone(),
        FeedbackMaterialId::White => assets.white.clone(),
        FeedbackMaterialId::Red => assets.red.clone(),
        FeedbackMaterialId::Pink => assets.pink.clone(),
        FeedbackMaterialId::SlashRed => assets.slash_red.clone(),
        FeedbackMaterialId::FirePunch => assets.fire_punch.clone(),
    }
}

fn effect_kind_for_visual(visual: FeedbackVisualKind) -> EffectKind {
    match visual {
        FeedbackVisualKind::Ring => EffectKind::TimelinePulse,
        FeedbackVisualKind::Trail => EffectKind::DashTrail,
        FeedbackVisualKind::FirePunch => EffectKind::FirePunch,
        FeedbackVisualKind::CrescentSlash => EffectKind::FeedbackBurst,
        FeedbackVisualKind::Heart => EffectKind::FeedbackBurst,
        FeedbackVisualKind::Spark => EffectKind::HitSpark,
        FeedbackVisualKind::Puff => EffectKind::DustPuff,
        FeedbackVisualKind::Column => EffectKind::SpecialPulse,
        FeedbackVisualKind::Burst => EffectKind::FeedbackBurst,
    }
}

pub fn element_spark_profile(element: DamageElement) -> ElementSparkProfile {
    match element {
        DamageElement::Neutral => ElementSparkProfile {
            count_scale: 1.0,
            velocity_scale: 1.0,
            heavy_bias: false,
        },
        DamageElement::Strike => ElementSparkProfile {
            count_scale: 1.04,
            velocity_scale: 1.02,
            heavy_bias: false,
        },
        DamageElement::Launch => ElementSparkProfile {
            count_scale: 1.12,
            velocity_scale: 1.08,
            heavy_bias: true,
        },
        DamageElement::Shock => ElementSparkProfile {
            count_scale: 1.18,
            velocity_scale: 1.16,
            heavy_bias: false,
        },
        DamageElement::Wind => ElementSparkProfile {
            count_scale: 1.08,
            velocity_scale: 1.22,
            heavy_bias: false,
        },
        DamageElement::Earth => ElementSparkProfile {
            count_scale: 0.96,
            velocity_scale: 0.82,
            heavy_bias: true,
        },
        DamageElement::Hazard => ElementSparkProfile {
            count_scale: 1.1,
            velocity_scale: 0.94,
            heavy_bias: false,
        },
        DamageElement::Blast => ElementSparkProfile {
            count_scale: 1.24,
            velocity_scale: 1.18,
            heavy_bias: true,
        },
    }
}

pub fn setup_effect_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let assets = EffectAssets {
        spark_mesh: meshes.add(Cone::new(0.12, 0.42)),
        puff_mesh: meshes.add(Sphere::new(0.18).mesh().uv(12, 6)),
        column_mesh: meshes.add(Cylinder::new(0.36, 1.0)),
        ring_mesh: meshes.add(Torus::new(0.42, 0.035)),
        trail_mesh: meshes.add(Capsule3d::new(0.12, 0.5)),
        fire_punch_mesh: meshes.add(fire_punch_mesh()),
        burn_flame_mesh: meshes.add(Cone::new(0.3, 0.82)),
        crescent_mesh: meshes.add(crescent_slash_mesh()),
        heart_mesh: meshes.add(heart_mesh()),
        blast_mesh: meshes.add(Sphere::new(POP_BOMB_BLAST_MESH_RADIUS).mesh().uv(16, 8)),
        yellow: effect_material(&mut materials, Color::srgb(1.0, 0.88, 0.18), 0.45),
        orange: effect_material(&mut materials, Color::srgb(1.0, 0.32, 0.08), 0.35),
        cyan: effect_material(&mut materials, Color::srgb(0.2, 0.95, 1.0), 0.45),
        blue: effect_material(&mut materials, Color::srgb(0.12, 0.42, 1.0), 0.3),
        smoke: effect_material(&mut materials, Color::srgb(0.58, 0.54, 0.49), 0.0),
        white: effect_material(&mut materials, Color::srgb(1.0, 0.96, 0.82), 0.5),
        red: effect_material(&mut materials, Color::srgb(1.0, 0.06, 0.04), 0.4),
        pink: effect_material(&mut materials, Color::srgba(1.0, 0.18, 0.42, 0.86), 1.2),
        slash_red: slash_effect_material(&mut materials),
        fire_punch: fire_punch_effect_material(&mut materials, FirePunchPalette::Red),
        fire_punch_blue: fire_punch_effect_material(&mut materials, FirePunchPalette::Blue),
    };
    commands.insert_resource(assets);
}

fn fire_punch_mesh() -> Mesh {
    let mut positions = Vec::with_capacity(18);
    let mut normals = Vec::with_capacity(18);
    let mut uvs = Vec::with_capacity(18);
    let mut indices = Vec::with_capacity(48);

    append_fire_punch_card(
        &mut positions,
        &mut normals,
        &mut uvs,
        &mut indices,
        Quat::IDENTITY,
    );
    append_fire_punch_card(
        &mut positions,
        &mut normals,
        &mut uvs,
        &mut indices,
        Quat::from_rotation_z(PI * 0.5),
    );

    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_indices(Indices::U32(indices))
}

fn append_fire_punch_card(
    positions: &mut Vec<[f32; 3]>,
    normals: &mut Vec<[f32; 3]>,
    uvs: &mut Vec<[f32; 2]>,
    indices: &mut Vec<u32>,
    rotation: Quat,
) {
    let outline = [
        Vec3::new(0.0, 0.0, -0.5),
        Vec3::new(0.0, -0.05, -0.28),
        Vec3::new(0.0, -0.14, 0.02),
        Vec3::new(0.0, -0.1, 0.32),
        Vec3::new(0.0, 0.02, 0.48),
        Vec3::new(0.0, 0.16, 0.28),
        Vec3::new(0.0, 0.2, -0.02),
        Vec3::new(0.0, 0.07, -0.28),
    ];
    let center = Vec3::new(0.0, 0.02, -0.04);
    let normal = rotation * Vec3::X;
    let base = positions.len() as u32;

    let rotated_center = rotation * center;
    positions.push([rotated_center.x, rotated_center.y, rotated_center.z]);
    normals.push([normal.x, normal.y, normal.z]);
    uvs.push([0.5, 0.5]);

    for (index, point) in outline.iter().enumerate() {
        let rotated = rotation * *point;
        positions.push([rotated.x, rotated.y, rotated.z]);
        normals.push([normal.x, normal.y, normal.z]);
        uvs.push([
            index as f32 / outline.len() as f32,
            if point.y >= center.y { 1.0 } else { 0.0 },
        ]);
    }

    for index in 0..outline.len() {
        let current = base + 1 + index as u32;
        let next = base + 1 + ((index + 1) % outline.len()) as u32;
        indices.extend_from_slice(&[base, current, next]);
    }
}

fn crescent_slash_mesh() -> Mesh {
    let segments = 22;
    let start_angle = -PI * 0.38;
    let end_angle = PI * 0.38;
    let center_radius = 0.82;
    let base_half_width = 0.055;
    let peak_half_width = 0.19;
    let half_depth = 0.026;
    let y_bias = -0.22;

    let mut positions = Vec::with_capacity((segments + 1) * 4);
    let mut normals = Vec::with_capacity((segments + 1) * 4);
    let mut uvs = Vec::with_capacity((segments + 1) * 4);
    let mut front_indices = Vec::with_capacity(segments * 6);

    for i in 0..=segments {
        let t = i as f32 / segments as f32;
        let angle = start_angle + (end_angle - start_angle) * t;
        let taper = (t * PI).sin();
        let half_width = base_half_width + peak_half_width * taper;
        let inner_radius = center_radius - half_width;
        let outer_radius = center_radius + half_width;

        for (side, radius) in [inner_radius, outer_radius].into_iter().enumerate() {
            positions.push([
                angle.sin() * radius,
                angle.cos() * radius + y_bias,
                half_depth,
            ]);
            normals.push([0.0, 0.0, 1.0]);
            uvs.push([t, side as f32]);
        }
    }

    for i in 0..segments {
        let inner = (i * 2) as u32;
        let outer = inner + 1;
        let next_inner = inner + 2;
        let next_outer = outer + 2;
        front_indices.extend_from_slice(&[inner, outer, next_outer, next_outer, next_inner, inner]);
    }

    let front_vertex_count = positions.len() as u32;
    let mut back_positions = positions.clone();
    for position in &mut back_positions {
        position[2] = -half_depth;
    }
    let mut back_normals = vec![[0.0, 0.0, -1.0]; back_positions.len()];
    positions.append(&mut back_positions);
    normals.append(&mut back_normals);
    let back_uvs = uvs.clone();
    uvs.extend(back_uvs);

    let mut indices = front_indices.clone();
    for triangle in front_indices.chunks_exact(3) {
        indices.extend_from_slice(&[
            triangle[2] + front_vertex_count,
            triangle[1] + front_vertex_count,
            triangle[0] + front_vertex_count,
        ]);
    }
    for i in 0..segments {
        let inner = (i * 2) as u32;
        let outer = inner + 1;
        let next_inner = inner + 2;
        let next_outer = outer + 2;
        let back_inner = inner + front_vertex_count;
        let back_outer = outer + front_vertex_count;
        let next_back_inner = next_inner + front_vertex_count;
        let next_back_outer = next_outer + front_vertex_count;
        indices.extend_from_slice(&[
            inner,
            next_inner,
            next_back_inner,
            next_back_inner,
            back_inner,
            inner,
            outer,
            back_outer,
            next_back_outer,
            next_back_outer,
            next_outer,
            outer,
        ]);
    }
    let start_inner = 0;
    let start_outer = 1;
    let start_back_inner = front_vertex_count;
    let start_back_outer = front_vertex_count + 1;
    let end_inner = (segments * 2) as u32;
    let end_outer = end_inner + 1;
    let end_back_inner = end_inner + front_vertex_count;
    let end_back_outer = end_outer + front_vertex_count;
    indices.extend_from_slice(&[
        start_inner,
        start_back_inner,
        start_back_outer,
        start_back_outer,
        start_outer,
        start_inner,
        end_inner,
        end_outer,
        end_back_outer,
        end_back_outer,
        end_back_inner,
        end_inner,
    ]);

    let cross_base = positions.len() as u32;
    let cross_rotation = Quat::from_rotation_y(PI * 0.5);
    let cross_positions: Vec<[f32; 3]> = positions
        .iter()
        .map(|position| {
            let rotated = cross_rotation * Vec3::new(position[0], position[1], position[2]);
            [rotated.x, rotated.y, rotated.z]
        })
        .collect();
    let cross_normals: Vec<[f32; 3]> = normals
        .iter()
        .map(|normal| {
            let rotated = cross_rotation * Vec3::new(normal[0], normal[1], normal[2]);
            [rotated.x, rotated.y, rotated.z]
        })
        .collect();
    let cross_uvs = uvs.clone();
    let cross_indices: Vec<u32> = indices.iter().map(|index| index + cross_base).collect();
    positions.extend(cross_positions);
    normals.extend(cross_normals);
    uvs.extend(cross_uvs);
    indices.extend(cross_indices);

    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_indices(Indices::U32(indices))
}

fn heart_mesh() -> Mesh {
    let segments = 36;
    let mut positions = Vec::with_capacity(segments + 2);
    let mut normals = Vec::with_capacity(segments + 2);
    let mut uvs = Vec::with_capacity(segments + 2);
    let mut indices = Vec::with_capacity(segments * 3);

    positions.push([0.0, 0.0, 0.0]);
    normals.push([0.0, 0.0, 1.0]);
    uvs.push([0.5, 0.5]);

    for i in 0..=segments {
        let t = i as f32 / segments as f32 * PI * 2.0;
        let sin = t.sin();
        let x = 16.0 * sin * sin * sin / 18.0;
        let y = (13.0 * t.cos() - 5.0 * (2.0 * t).cos() - 2.0 * (3.0 * t).cos() - (4.0 * t).cos())
            / 18.0
            - 0.14;
        positions.push([x, y, 0.0]);
        normals.push([0.0, 0.0, 1.0]);
        uvs.push([(x + 1.0) * 0.5, (y + 1.0) * 0.5]);
    }

    for i in 1..=segments {
        indices.extend_from_slice(&[0, i as u32, i as u32 + 1]);
    }

    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_indices(Indices::U32(indices))
}

fn effect_material(
    materials: &mut Assets<StandardMaterial>,
    color: Color,
    emissive_strength: f32,
) -> Handle<StandardMaterial> {
    materials.add(StandardMaterial {
        base_color: color,
        emissive: LinearRgba::from(color.to_linear()) * emissive_strength,
        cull_mode: None,
        unlit: true,
        perceptual_roughness: 0.45,
        ..default()
    })
}

fn slash_effect_material(materials: &mut Assets<StandardMaterial>) -> Handle<StandardMaterial> {
    let color = Color::srgba(1.0, 0.04, 0.02, 0.82);
    materials.add(StandardMaterial {
        base_color: color,
        emissive: LinearRgba::from(color.to_linear()) * 4.0,
        alpha_mode: AlphaMode::Add,
        cull_mode: None,
        depth_bias: 128.0,
        unlit: true,
        perceptual_roughness: 0.2,
        ..default()
    })
}

fn fire_punch_effect_color(palette: FirePunchPalette) -> Color {
    match palette {
        FirePunchPalette::Red => Color::srgba(1.0, 0.24, 0.02, 0.68),
        FirePunchPalette::Blue => Color::srgba(0.1, 0.42, 1.0, 0.68),
    }
}

fn fire_punch_effect_material(
    materials: &mut Assets<StandardMaterial>,
    palette: FirePunchPalette,
) -> Handle<StandardMaterial> {
    let color = fire_punch_effect_color(palette);
    materials.add(StandardMaterial {
        base_color: color,
        emissive: LinearRgba::from(color.to_linear()) * 3.6,
        alpha_mode: AlphaMode::Add,
        cull_mode: None,
        depth_bias: 96.0,
        unlit: true,
        perceptual_roughness: 0.2,
        ..default()
    })
}

pub fn update_effects(
    time: Res<Time>,
    mut commands: Commands,
    mut effects: Query<(Entity, &mut VisualEffect, &mut Transform)>,
) {
    let dt = time.delta_secs();
    for (entity, mut effect, mut transform) in &mut effects {
        effect.age += dt;
        if effect.age >= effect.lifetime {
            commands.entity(entity).despawn();
            continue;
        }

        let t = (effect.age / effect.lifetime).clamp(0.0, 1.0);
        transform.translation += effect.velocity * dt;
        transform.rotate_x(effect.spin.x * dt);
        transform.rotate_y(effect.spin.y * dt);
        transform.rotate_z(effect.spin.z * dt);
        let kind_scale = match effect.kind {
            EffectKind::DashTrail => 0.92,
            EffectKind::FeedbackBurst => 1.0 + (t * PI).sin() * 0.05,
            EffectKind::FirePunch => 1.0 + (t * PI).sin() * 0.08,
            EffectKind::Burning => 1.0 + (effect.age * 18.0).sin() * 0.12,
            EffectKind::SpecialPulse => 1.0 + (t * PI).sin() * 0.1,
            EffectKind::RespawnColumn => 1.0 + (t * PI).sin() * 0.08,
            _ => 1.0,
        };
        transform.scale = effect.start_scale.lerp(effect.end_scale, t) * kind_scale;
    }
}

pub fn spawn_burning_fighter_effect(
    commands: &mut Commands,
    assets: &EffectAssets,
    fighter_entity: Entity,
    duration: f32,
) {
    let flame_offsets = [
        Vec3::new(-0.24, 0.18, 0.12),
        Vec3::new(0.22, 0.28, -0.1),
        Vec3::new(-0.12, 0.54, -0.18),
        Vec3::new(0.16, 0.7, 0.14),
        Vec3::new(0.0, 0.92, 0.0),
    ];

    commands.entity(fighter_entity).with_children(|parent| {
        for (index, offset) in flame_offsets.into_iter().enumerate() {
            let scale = 1.35 + index as f32 * 0.11;
            let material = match index % 2 {
                0 => assets.yellow.clone(),
                _ => assets.orange.clone(),
            };
            parent.spawn((
                Mesh3d(assets.burn_flame_mesh.clone()),
                MeshMaterial3d(material),
                Transform::from_translation(offset)
                    .with_rotation(Quat::from_rotation_y(index as f32 * 1.3))
                    .with_scale(Vec3::new(scale * 0.7, scale, scale * 0.7)),
                VisualEffect {
                    kind: EffectKind::Burning,
                    lifetime: duration * (0.86 + index as f32 * 0.035),
                    age: 0.0,
                    velocity: Vec3::new(
                        (index as f32 - 2.0) * 0.035,
                        0.46 + index as f32 * 0.04,
                        if index % 2 == 0 { 0.06 } else { -0.06 },
                    ),
                    spin: Vec3::new(0.0, 2.2 + index as f32 * 0.3, 0.0),
                    start_scale: Vec3::new(scale * 0.7, scale, scale * 0.7),
                    end_scale: Vec3::new(0.14, 0.3, 0.14),
                },
                Name::new("Burning fighter flame"),
            ));
        }

        for (index, offset) in [Vec3::new(-0.14, 0.88, 0.02), Vec3::new(0.16, 1.04, -0.04)]
            .into_iter()
            .enumerate()
        {
            parent.spawn((
                Mesh3d(assets.puff_mesh.clone()),
                MeshMaterial3d(assets.smoke.clone()),
                Transform::from_translation(offset).with_scale(Vec3::splat(0.7)),
                VisualEffect {
                    kind: EffectKind::Burning,
                    lifetime: duration * (0.72 + index as f32 * 0.1),
                    age: 0.0,
                    velocity: Vec3::new(if index == 0 { -0.08 } else { 0.08 }, 0.58, 0.0),
                    spin: Vec3::new(0.4, 0.8, 0.2),
                    start_scale: Vec3::splat(0.7),
                    end_scale: Vec3::splat(0.22),
                },
                Name::new("Burning fighter smoke"),
            ));
        }
    });
}

pub fn spawn_hit_spark(
    commands: &mut Commands,
    assets: &EffectAssets,
    position: Vec3,
    heavy: bool,
    scale: f32,
) {
    spawn_element_hit_spark(
        commands,
        assets,
        position,
        DamageElement::Neutral,
        heavy,
        scale,
    );
}

pub fn spawn_element_hit_spark(
    commands: &mut Commands,
    assets: &EffectAssets,
    position: Vec3,
    element: DamageElement,
    heavy: bool,
    scale: f32,
) {
    let profile = element_spark_profile(element);
    let heavy = heavy || profile.heavy_bias;
    let scale = scale.clamp(0.65, 1.65);
    let count = ((if heavy { 8.0 } else { 5.0 }) * scale * profile.count_scale)
        .round()
        .max(3.0) as usize;
    let material = match element {
        DamageElement::Neutral | DamageElement::Strike => {
            if heavy {
                assets.orange.clone()
            } else {
                assets.yellow.clone()
            }
        }
        DamageElement::Launch => assets.white.clone(),
        DamageElement::Shock | DamageElement::Wind => assets.cyan.clone(),
        DamageElement::Earth => assets.smoke.clone(),
        DamageElement::Hazard => assets.red.clone(),
        DamageElement::Blast => assets.orange.clone(),
    };
    for i in 0..count {
        let angle = i as f32 / count as f32 * PI * 2.0;
        let dir = Vec3::new(angle.cos(), 0.45, angle.sin()).normalize();
        commands.spawn((
            Mesh3d(assets.spark_mesh.clone()),
            MeshMaterial3d(material.clone()),
            Transform::from_translation(position + dir * 0.08)
                .with_rotation(Quat::from_rotation_y(-angle))
                .with_scale(Vec3::splat(if heavy { 1.35 } else { 1.0 }) * scale),
            VisualEffect {
                kind: EffectKind::HitSpark,
                lifetime: if heavy { 0.24 } else { 0.18 } * (0.92 + scale * 0.08),
                age: 0.0,
                velocity: dir * if heavy { 4.4 } else { 3.2 } * scale * profile.velocity_scale,
                spin: Vec3::new(4.0, 8.0, 2.0),
                start_scale: Vec3::splat(if heavy { 1.1 } else { 0.8 }) * scale,
                end_scale: Vec3::splat(0.08),
            },
        ));
    }
}

pub fn spawn_guard_flash(commands: &mut Commands, assets: &EffectAssets, position: Vec3) {
    commands.spawn((
        Mesh3d(assets.ring_mesh.clone()),
        MeshMaterial3d(assets.cyan.clone()),
        Transform::from_translation(position).with_rotation(Quat::from_rotation_x(PI * 0.5)),
        VisualEffect {
            kind: EffectKind::GuardFlash,
            lifetime: 0.28,
            age: 0.0,
            velocity: Vec3::Y * 0.35,
            spin: Vec3::new(0.0, 10.0, 0.0),
            start_scale: Vec3::splat(0.65),
            end_scale: Vec3::splat(1.75),
        },
    ));
}

pub fn spawn_dash_trail(
    commands: &mut Commands,
    assets: &EffectAssets,
    position: Vec3,
    facing: Vec3,
) {
    let yaw = facing.x.atan2(facing.z);
    commands.spawn((
        Mesh3d(assets.trail_mesh.clone()),
        MeshMaterial3d(assets.blue.clone()),
        Transform::from_translation(position + Vec3::Y * 0.55 - facing.normalize_or_zero() * 0.55)
            .with_rotation(Quat::from_rotation_y(yaw))
            .with_scale(Vec3::new(1.0, 0.7, 1.8)),
        VisualEffect {
            kind: EffectKind::DashTrail,
            lifetime: 0.24,
            age: 0.0,
            velocity: -facing.normalize_or_zero() * 1.6,
            spin: Vec3::ZERO,
            start_scale: Vec3::new(1.0, 0.7, 1.8),
            end_scale: Vec3::new(0.2, 0.12, 0.35),
        },
    ));
}

pub fn spawn_dust_puff(commands: &mut Commands, assets: &EffectAssets, position: Vec3) {
    for i in 0..5 {
        let angle = i as f32 / 5.0 * PI * 2.0;
        let dir = Vec3::new(angle.cos(), 0.18, angle.sin()).normalize();
        commands.spawn((
            Mesh3d(assets.puff_mesh.clone()),
            MeshMaterial3d(assets.smoke.clone()),
            Transform::from_translation(position + Vec3::Y * 0.08),
            VisualEffect {
                kind: EffectKind::DustPuff,
                lifetime: 0.42,
                age: 0.0,
                velocity: dir * 1.15,
                spin: Vec3::new(2.0, 1.0, 0.5),
                start_scale: Vec3::splat(0.55),
                end_scale: Vec3::splat(0.08),
            },
        ));
    }
}

pub fn spawn_aftermath_pulse(
    commands: &mut Commands,
    assets: &EffectAssets,
    position: Vec3,
    family: ReactionFamilyId,
) {
    let (material, lifetime, end_scale) = match family {
        ReactionFamilyId::GroundedDownGetup => (assets.orange.clone(), 0.42, 2.05),
        ReactionFamilyId::GroundBounceDown => (assets.red.clone(), 0.48, 2.45),
        ReactionFamilyId::AerialSpikeDown => (assets.cyan.clone(), 0.36, 1.85),
        ReactionFamilyId::AirFishKnockdown => (assets.cyan.clone(), 0.5, 2.55),
        ReactionFamilyId::UltimateBombDown => (assets.red.clone(), 0.56, 2.85),
        _ => (assets.smoke.clone(), 0.34, 1.65),
    };
    commands.spawn((
        Mesh3d(assets.ring_mesh.clone()),
        MeshMaterial3d(material),
        Transform::from_translation(position + Vec3::Y * 0.06)
            .with_rotation(Quat::from_rotation_x(PI * 0.5)),
        VisualEffect {
            kind: EffectKind::AftermathPulse,
            lifetime,
            age: 0.0,
            velocity: Vec3::Y * 0.1,
            spin: Vec3::new(0.0, 6.0, 0.0),
            start_scale: Vec3::splat(0.42),
            end_scale: Vec3::splat(end_scale),
        },
    ));
    spawn_dust_puff(commands, assets, position);
}

pub fn spawn_ringout_burst(commands: &mut Commands, assets: &EffectAssets, position: Vec3) {
    commands.spawn((
        Mesh3d(assets.ring_mesh.clone()),
        MeshMaterial3d(assets.red.clone()),
        Transform::from_translation(position + Vec3::Y * 0.2),
        VisualEffect {
            kind: EffectKind::RingOutBurst,
            lifetime: 0.62,
            age: 0.0,
            velocity: Vec3::Y * 0.5,
            spin: Vec3::new(0.0, 12.0, 0.0),
            start_scale: Vec3::splat(0.5),
            end_scale: Vec3::splat(3.1),
        },
    ));
    spawn_hit_spark(commands, assets, position + Vec3::Y * 0.55, true, 1.35);
}

pub fn spawn_respawn_column(commands: &mut Commands, assets: &EffectAssets, position: Vec3) {
    commands.spawn((
        Mesh3d(assets.column_mesh.clone()),
        MeshMaterial3d(assets.white.clone()),
        Transform::from_translation(position + Vec3::Y * 0.55).with_scale(Vec3::new(0.8, 1.0, 0.8)),
        VisualEffect {
            kind: EffectKind::RespawnColumn,
            lifetime: 0.8,
            age: 0.0,
            velocity: Vec3::Y * 0.25,
            spin: Vec3::new(0.0, 5.0, 0.0),
            start_scale: Vec3::new(0.65, 0.2, 0.65),
            end_scale: Vec3::new(1.45, 2.2, 1.45),
        },
    ));
}

pub fn spawn_pop_bomb_blast(commands: &mut Commands, assets: &EffectAssets, position: Vec3) {
    commands.spawn((
        Mesh3d(assets.blast_mesh.clone()),
        MeshMaterial3d(assets.orange.clone()),
        Transform::from_translation(position + Vec3::Y * 0.45).with_scale(Vec3::splat(0.25)),
        VisualEffect {
            kind: EffectKind::PopBombBlast,
            lifetime: 0.5,
            age: 0.0,
            velocity: Vec3::Y * 0.2,
            spin: Vec3::new(3.0, 6.0, 4.0),
            start_scale: Vec3::splat(0.35),
            end_scale: Vec3::splat(POP_BOMB_BLAST_VISUAL_END_SCALE),
        },
    ));
    spawn_hit_spark(commands, assets, position + Vec3::Y * 0.7, true, 1.25);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn element_spark_profiles_make_damage_metadata_read_visually() {
        let neutral = element_spark_profile(DamageElement::Neutral);
        let shock = element_spark_profile(DamageElement::Shock);
        let earth = element_spark_profile(DamageElement::Earth);
        let blast = element_spark_profile(DamageElement::Blast);

        assert!(shock.count_scale > neutral.count_scale);
        assert!(shock.velocity_scale > earth.velocity_scale);
        assert!(earth.heavy_bias);
        assert!(blast.count_scale > shock.count_scale);
    }

    #[test]
    fn timeline_cues_resolve_to_authored_feedback_packages() {
        let light = feedback_package_for_timeline_cue(FeedbackPhase::PreHit, "trail_A_s");
        let launcher = feedback_package_for_timeline_cue(FeedbackPhase::PreHit, "trail_kiriage");
        let fallback = feedback_package_for_timeline_cue(FeedbackPhase::Aftermath, "unknown");

        assert_eq!(light.id, FeedbackPackageId::FirePunchTrail);
        assert_eq!(launcher.id, FeedbackPackageId::LauncherTrail);
        assert_eq!(launcher.reference_effect, Some("/effect/special_parts.oa"));
        assert_eq!(fallback.id, FeedbackPackageId::GenericAftermath);
    }

    #[test]
    fn cat_and_pig_light_trails_spawn_fire_punches() {
        for cue in [
            "trail_A_s",
            "trail_A_ss",
            "trail_pig_cat_light_1",
            "trail_pig_cat_light_2",
        ] {
            let package = feedback_package_for_timeline_cue(FeedbackPhase::PreHit, cue);
            let punch = package.primary;

            assert_eq!(package.id, FeedbackPackageId::FirePunchTrail);
            assert_eq!(punch.visual, FeedbackVisualKind::FirePunch);
            assert_eq!(punch.material, FeedbackMaterialId::FirePunch);
            assert!(punch.offset.y > 0.55);
            assert!(punch.offset.z > 0.5);
            assert!(punch.velocity.z > 0.0);
            assert!(punch.start_scale.z < 0.9);
            assert!(punch.end_scale.z < punch.start_scale.z);
            assert!(package.secondary.is_none());
        }

        let mesh = fire_punch_mesh();
        assert!(mesh.count_vertices() >= 16);
        assert!(mesh.indices().is_some());
    }

    #[test]
    fn light_fire_punch_anchor_tracks_rotated_cube_front_corner() {
        let (first_translation, first_direction) =
            light_fire_punch_anchor(Vec3::ZERO, Vec3::Z, -1.0);
        let (second_translation, second_direction) =
            light_fire_punch_anchor(Vec3::ZERO, Vec3::Z, 1.0);

        assert!(first_translation.x > 0.0);
        assert!(second_translation.x < 0.0);
        assert!(first_translation.z > 0.5);
        assert!(second_translation.z > 0.5);
        assert!(first_direction.x > 0.0);
        assert!(second_direction.x < 0.0);
        assert!(first_direction.z > 0.8);
        assert!(second_direction.z > 0.8);
    }

    #[test]
    fn fire_punch_palette_can_shift_pig_trial_to_blue() {
        let red = fire_punch_effect_color(FirePunchPalette::Red).to_srgba();
        let blue = fire_punch_effect_color(FirePunchPalette::Blue).to_srgba();

        assert!(red.red > red.blue);
        assert!(blue.blue > blue.red);
        assert_eq!(red.alpha, blue.alpha);
    }

    #[test]
    fn hit_impact_effect_presets_make_contact_hits_readable() {
        let light = hit_impact_effect_definition(HitImpactEffectId::GenericLight);
        let pig_light = hit_impact_effect_definition(HitImpactEffectId::LightBlue);
        let launcher = hit_impact_effect_definition(HitImpactEffectId::LauncherCyan);
        let ham_slam = hit_impact_effect_definition(HitImpactEffectId::PigHamSlamHeart);
        let bomb = hit_impact_effect_definition(HitImpactEffectId::UltimateBombRed);

        assert_eq!(light.flash.visual, FeedbackVisualKind::Burst);
        assert_eq!(light.accent.unwrap().visual, FeedbackVisualKind::Ring);
        assert_eq!(pig_light.flash.material, FeedbackMaterialId::Blue);
        assert_eq!(pig_light.accent.unwrap().material, FeedbackMaterialId::Cyan);
        assert!(launcher.force_heavy_spark);
        assert_eq!(launcher.accent.unwrap().visual, FeedbackVisualKind::Column);
        assert_eq!(ham_slam.accent.unwrap().visual, FeedbackVisualKind::Heart);
        assert!(bomb.flash.end_scale.x > launcher.flash.end_scale.x);
        assert!(bomb.spark_scale > light.spark_scale);
    }

    #[test]
    fn feedback_packages_fall_back_to_hit_impact_effects() {
        assert_eq!(
            hit_impact_effect_for_feedback_package(FeedbackPackageId::LightImpact, false),
            HitImpactEffectId::GenericLight
        );
        assert_eq!(
            hit_impact_effect_for_feedback_package(FeedbackPackageId::LightImpact, true),
            HitImpactEffectId::GenericHeavy
        );
        assert_eq!(
            hit_impact_effect_for_feedback_package(FeedbackPackageId::LauncherImpact, true),
            HitImpactEffectId::LauncherCyan
        );
        assert_eq!(
            hit_impact_effect_for_feedback_package(FeedbackPackageId::UltimateBomb, true),
            HitImpactEffectId::UltimateBombRed
        );
    }

    #[test]
    fn named_cues_resolve_special_and_guard_packages() {
        assert_eq!(
            feedback_package_for_named_cue("impact_special_shockwave").id,
            FeedbackPackageId::SpecialShockwaveImpact
        );
        assert_eq!(
            feedback_package_for_named_cue("perfect_guard").id,
            FeedbackPackageId::PerfectGuard
        );
        assert_eq!(
            feedback_package_for_named_cue("pulse_special_hazard").id,
            FeedbackPackageId::SpecialHazardPulse
        );
    }

    #[test]
    fn ultimate_scratch_cues_spawn_crescent_slash() {
        let timeline =
            feedback_package_for_timeline_cue(FeedbackPhase::PreHit, "trail_ultimate_scratch");
        let impact = feedback_package_for_named_cue("impact_ultimate_scratch");
        let mesh = crescent_slash_mesh();

        assert_eq!(timeline.id, FeedbackPackageId::UltimateScratch);
        assert_eq!(impact.id, FeedbackPackageId::UltimateScratchImpact);
        assert_eq!(timeline.primary.visual, FeedbackVisualKind::CrescentSlash);
        assert_eq!(impact.primary.visual, FeedbackVisualKind::CrescentSlash);
        assert_eq!(impact.primary.count, 3);
        assert_eq!(impact.primary.material, FeedbackMaterialId::SlashRed);
        assert!(timeline.primary.offset.y > 0.5);
        assert!(impact.primary.offset.y < 0.0);
        assert!(impact.primary.velocity.z > 0.4);
        assert!(impact.primary.end_scale.x > impact.primary.start_scale.x);
        assert_eq!(impact.secondary.unwrap().visual, FeedbackVisualKind::Spark);
        assert!(mesh.count_vertices() >= 180);
        assert!(mesh.indices().is_some());
    }

    #[test]
    fn bee_ultimate_cues_resolve_to_ultimate_packages() {
        assert_eq!(
            feedback_package_for_timeline_cue(FeedbackPhase::Startup, "startup_bee_ultimate_swarm")
                .id,
            FeedbackPackageId::UltimateStartup
        );
        assert_eq!(
            feedback_package_for_timeline_cue(FeedbackPhase::PreHit, "trail_bee_ultimate_scratch")
                .id,
            FeedbackPackageId::UltimateScratch
        );
        assert_eq!(
            feedback_package_for_timeline_cue(FeedbackPhase::PreHit, "charge_bee_ultimate_bomb").id,
            FeedbackPackageId::UltimateBomb
        );
        assert_eq!(
            feedback_package_for_timeline_cue(
                FeedbackPhase::Aftermath,
                "recover_bee_ultimate_bomb"
            )
            .id,
            FeedbackPackageId::UltimateRecover
        );
        assert_eq!(
            feedback_package_for_named_cue("impact_bee_ultimate_scratch").id,
            FeedbackPackageId::UltimateScratchImpact
        );
        assert_eq!(
            feedback_package_for_named_cue("impact_bee_ultimate_bomb").id,
            FeedbackPackageId::UltimateBomb
        );
    }

    #[test]
    fn pig_ultimate_cues_avoid_cat_scratch_slashes() {
        let trail =
            feedback_package_for_timeline_cue(FeedbackPhase::PreHit, "trail_pig_unblockable_grab");
        let crush = feedback_package_for_timeline_cue(FeedbackPhase::PreHit, "pig_grab_crush");
        let meat_slam =
            feedback_package_for_timeline_cue(FeedbackPhase::PreHit, "pig_grab_meat_slam");
        let light_impact = feedback_package_for_named_cue("impact_pig_ultimate_scratch");
        let heavy_impact = feedback_package_for_named_cue("impact_pig_ultimate_scratch_heavy");

        assert_eq!(trail.id, FeedbackPackageId::UltimateStartup);
        assert_eq!(crush.id, FeedbackPackageId::HeavyImpact);
        assert_eq!(meat_slam.id, FeedbackPackageId::HeavyImpact);
        assert_eq!(light_impact.id, FeedbackPackageId::HeavyImpact);
        assert_eq!(heavy_impact.id, FeedbackPackageId::HeavyImpact);
        assert_ne!(crush.primary.visual, FeedbackVisualKind::CrescentSlash);
        assert_ne!(
            light_impact.primary.visual,
            FeedbackVisualKind::CrescentSlash
        );
    }

    #[test]
    fn pig_ham_slam_impact_spawns_head_height_hearts() {
        let impact = feedback_package_for_named_cue("impact_pig_ham_slam");
        let mesh = heart_mesh();

        assert_eq!(impact.id, FeedbackPackageId::PigHamSlamImpact);
        assert_eq!(impact.primary.visual, FeedbackVisualKind::Heart);
        assert_eq!(impact.primary.material, FeedbackMaterialId::Pink);
        assert_eq!(impact.primary.count, 3);
        assert!(impact.primary.offset.y > 0.6);
        assert!(impact.primary.velocity.y > 0.6);
        assert!(impact.primary.end_scale.x < impact.primary.start_scale.x);
        assert_eq!(impact.secondary.unwrap().visual, FeedbackVisualKind::Burst);
        assert!(mesh.count_vertices() >= 36);
        assert!(mesh.indices().is_some());
    }
}
