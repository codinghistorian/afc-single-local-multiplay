use bevy::gltf::GltfAssetLabel;
use bevy::prelude::*;
use std::collections::HashMap;

use crate::arena::ground_height_at;
use crate::arena_defs::active_arena_definition;
use crate::bee_skills::BeeSkillTargetSnapshot;
use crate::characters::CharacterKind;
use crate::combat::{
    DamageDefenderProfile, HitEffects, ImpactSource, apply_impact, can_receive_impact,
    impact_profile_from_payload_with_feel,
};
use crate::components::{Fighter, FighterActionState, FighterMotor, FighterStats};
use crate::constants::{ARENA_TOP_Y, FIGHTER_HEIGHT, FIGHTER_RADIUS};
use crate::effects::{EffectAssets, FeedbackPackageId, spawn_feedback_package};
use crate::equipment::FighterEquipment;
use crate::feel::CombatFeelTuning;
use crate::game_state::{Hitstop, MatchState, MatchTelemetry};
use crate::styles::{FighterStyle, FighterStyleKind};
use crate::techniques::{AttackPayloadId, AttackShapeId, PenguinSkillId};

pub const PENGUIN_FISH_BONES_ASSET: &str = "food/kenney_food_kit/fish-bones.glb";
pub const PENGUIN_POPSICLE_ASSET: &str = "food/kenney_food_kit/popsicle.glb";
pub const PENGUIN_SNOW_PILE_ASSET: &str = "holiday/kenney_holiday_kit/snow-pile.glb";
pub const PENGUIN_SNOWFLAKE_ASSET: &str = "holiday/kenney_holiday_kit/snowflake-a.glb";
pub const PENGUIN_SNOWMAN_ASSET: &str = "holiday/kenney_holiday_kit/snowman.glb";
pub const PENGUIN_ULTIMATE_SNOW_FLAT_LARGE_ASSET: &str =
    "holiday/kenney_holiday_kit/snow-flat-large.glb";
pub const PENGUIN_ULTIMATE_SNOW_FLAT_ASSET: &str = "holiday/kenney_holiday_kit/snow-flat.glb";
pub const PENGUIN_ICE_TILE_ASSET: &str =
    "tower_defense/kenney_tower_defense_kit/snow-tile-straight.glb";
pub const PENGUIN_SNOW_BUMP_ASSET: &str =
    "tower_defense/kenney_tower_defense_kit/snow-tile-bump.glb";
pub const PENGUIN_SNOW_HILL_ASSET: &str =
    "tower_defense/kenney_tower_defense_kit/snow-tile-hill.glb";
pub const PENGUIN_SNOWFORT_ASSET: &str =
    "tower_defense/kenney_tower_defense_kit/snow-wood-structure.glb";
pub const PENGUIN_CANNON_ASSET: &str = "tower_defense/kenney_tower_defense_kit/weapon-cannon.glb";
pub const PENGUIN_BOULDER_ASSET: &str =
    "tower_defense/kenney_tower_defense_kit/weapon-ammo-boulder.glb";
pub const PENGUIN_SNOW_SLOPE_ASSET: &str =
    "platformer/kenney_platformer_kit/block-snow-large-slope.glb";
pub const PENGUIN_SNOW_STEEP_SLOPE_ASSET: &str =
    "platformer/kenney_platformer_kit/block-snow-large-slope-steep.glb";
pub const PENGUIN_SPRING_ASSET: &str = "platformer/kenney_platformer_kit/spring.glb";

const PENGUIN_SKILL_LOCK_RANGE: f32 = 7.5;
const PENGUIN_SKILL_LOCK_CONE_DOT: f32 = 0.70710677;
const PENGUIN_FISH_TORPEDO_SPEED: f32 = 8.8;
const PENGUIN_FISH_TORPEDO_TURN_RATE: f32 = 5.0;
const PENGUIN_FISH_TORPEDO_LIFETIME: f32 = 0.86;
const PENGUIN_FISH_TORPEDO_RADIUS: f32 = 0.38;
const PENGUIN_POPSICLE_SPEED: f32 = 6.9;
const PENGUIN_POPSICLE_LIFT: f32 = 2.35;
const PENGUIN_POPSICLE_GRAVITY: f32 = 8.8;
const PENGUIN_POPSICLE_LIFETIME: f32 = 1.12;
const PENGUIN_POPSICLE_RADIUS: f32 = 0.34;
const PENGUIN_SLED_WAKE_SPEED: f32 = 2.6;
const PENGUIN_SLED_WAKE_LIFETIME: f32 = 1.18;
const PENGUIN_SLED_WAKE_RADIUS: f32 = 0.92;
const PENGUIN_SLED_WAKE_TICK: f32 = 0.38;
const PENGUIN_SLED_WAKE_DAMPING: f32 = 0.58;
const PENGUIN_SNOWFLAKE_SPEED: f32 = 7.2;
const PENGUIN_SNOWFLAKE_LIFETIME: f32 = 1.08;
const PENGUIN_SNOWFLAKE_RADIUS: f32 = 0.32;
const PENGUIN_SNOWFLAKE_SHOT_FORWARD: f32 = 0.58;
const PENGUIN_SNOWFLAKE_SHOT_HEIGHT: f32 = 0.86;
const PENGUIN_SNOWMAN_DROP_FORWARD: f32 = 1.25;
const PENGUIN_SNOWMAN_DROP_HEIGHT: f32 = 2.15;
const PENGUIN_SNOWMAN_DROP_INITIAL_FALL_SPEED: f32 = 0.5;
const PENGUIN_SNOWMAN_DROP_GRAVITY: f32 = 15.0;
const PENGUIN_SNOWMAN_DROP_LIFETIME: f32 = 1.05;
const PENGUIN_SNOWMAN_DROP_SIZE_MULTIPLIER: f32 = 1.5;
const PENGUIN_SNOWMAN_DROP_RADIUS: f32 = 0.85 * PENGUIN_SNOWMAN_DROP_SIZE_MULTIPLIER;
const PENGUIN_SNOWMAN_DROP_LAND_CLEARANCE: f32 = 0.16;
const PENGUIN_SNOWMAN_DROP_SNOW_TILE_COUNT: usize = 2;
const PENGUIN_BOULDER_SPEED: f32 = 8.4;
const PENGUIN_BOULDER_LIFETIME: f32 = 1.08;
const PENGUIN_BOULDER_RADIUS: f32 = 0.46;
const PENGUIN_BODY_SLAM_LIFETIME: f32 = 0.34;
const PENGUIN_BODY_SLAM_RADIUS: f32 = 1.55;
pub const PENGUIN_ICE_TRAIL_LIFETIME: f32 = 15.0;
const PENGUIN_ICE_TRAIL_RADIUS: f32 = 1.05;
const PENGUIN_ICE_TRAIL_CAP_PER_OWNER: usize = 18;
const PENGUIN_ULTIMATE_ICE_FIELD_LIFETIME: f32 = 10.0;
const PENGUIN_ULTIMATE_ICE_FIELD_GRID_SIDE: i32 = 4;
const PENGUIN_ULTIMATE_ICE_FIELD_TILE_SPACING: f32 = 1.15;
const PENGUIN_ULTIMATE_ICE_FIELD_TILE_RADIUS: f32 = 0.78;
const PENGUIN_ULTIMATE_SNOW_FIELD_CLEARANCE: f32 = 0.024;
const PENGUIN_ULTIMATE_SNOW_FLAT_LARGE_SCALE: f32 = 0.88;
const PENGUIN_ULTIMATE_SNOW_FLAT_DETAIL_SCALE: f32 = 0.34;
const PENGUIN_SNOW_HILL_LIFETIME: f32 = 6.5;
const PENGUIN_SNOW_HILL_RADIUS: f32 = 1.08;
const PENGUIN_SNOW_HILL_LAUNCH: f32 = 4.2;
const PENGUIN_SNOW_HILL_PUSH: f32 = 3.4;
const PENGUIN_SNOW_HILL_RIDE_LIFT: f32 = 1.05;
const PENGUIN_SNOW_HILL_RIDE_PUSH: f32 = 7.2;
const PENGUIN_SNOW_HILL_RIDE_SLIDE: f32 = 0.42;
const PENGUIN_SNOW_HILL_RIDE_SPEED_LIMIT: f32 = 0.46;
const PENGUIN_SNOW_SLOPE_RIDE_LIFETIME: f32 = 2.4;
const PENGUIN_SNOW_SLOPE_RIDE_RADIUS: f32 = 1.08;
const PENGUIN_SNOW_SLOPE_RIDE_HALF_LENGTH: f32 = 0.72;
const PENGUIN_SNOW_SLOPE_RIDE_HALF_WIDTH: f32 = 0.72;
const PENGUIN_SNOW_SLOPE_RIDE_BASE_HEIGHT: f32 = 0.05;
const PENGUIN_SNOW_SLOPE_RIDE_HEIGHT: f32 = 0.52;
const PENGUIN_SNOW_SLOPE_RIDE_LIFT: f32 = 1.45;
const PENGUIN_SNOW_SLOPE_RIDE_PUSH: f32 = 4.8;
const PENGUIN_SNOW_SLOPE_RIDE_SLIDE: f32 = 0.62;
const PENGUIN_SNOW_SLOPE_RIDE_SPEED_LIMIT: f32 = 0.56;
const PENGUIN_SNOW_SLOPE_RIDE_EXIT_PROGRESS: f32 = 0.92;
const PENGUIN_SNOWFORT_LIFETIME: f32 = 1.55;
const PENGUIN_GLACIER_PARADE_LIFETIME: f32 = 15.0;
const PENGUIN_GLACIER_PARADE_TICK: f32 = 0.34;
const PENGUIN_SPRING_PAD_LIFETIME: f32 = 1.6;
const PENGUIN_SPRING_PAD_RADIUS: f32 = 0.64;
const PENGUIN_SPRING_PAD_LIFT: f32 = 5.6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PenguinSkillKind {
    FishTorpedo,
    PopsicleBounce,
    SledWake,
    SnowflakeShard,
    SnowBoulder,
    SnowmanDrop,
    BodySlamShockwave,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PenguinSurfaceKind {
    IceTrailSegment,
    UltimateIceTile,
    SnowHillRamp,
    SnowSlopeRide,
    SnowfortCannon,
    GlacierTrailPrinter,
    SpringPad,
}

#[derive(Component)]
pub struct ActivePenguinSurface {
    pub kind: PenguinSurfaceKind,
    pub owner: Entity,
    pub owner_id: usize,
    pub facing: Vec3,
    pub lifetime: f32,
    pub age: f32,
    pub radius: f32,
    pub next_tick: f32,
    pub already_touched: Vec<Entity>,
    pub size_scale: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PenguinIceModifier {
    pub ground_friction_scale: f32,
    pub stop_friction_scale: f32,
    pub turn_brake_scale: f32,
    pub input_scale: f32,
    pub dash_slide_timer: f32,
    pub hard_slide: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SnowHillRampTouch {
    forward_push: f32,
    lift: f32,
    slide_timer: f32,
    speed_limit_timer: f32,
    owner_ride: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SnowSlopeRideContact {
    progress: f32,
    target_y: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PenguinSnowflakeSwap {
    pub snowflake: Entity,
    pub penguin_destination: Vec3,
}

#[derive(Component)]
pub struct ActivePenguinSkill {
    pub kind: PenguinSkillKind,
    pub owner: Entity,
    pub owner_id: usize,
    pub owner_style: FighterStyleKind,
    pub payload_id: AttackPayloadId,
    pub shape_id: AttackShapeId,
    pub source: ImpactSource,
    pub facing: Vec3,
    pub velocity: Vec3,
    pub target: Option<Entity>,
    pub lifetime: f32,
    pub age: f32,
    pub radius: f32,
    pub guard_stamina_damage: f32,
    pub repeat_interval: Option<f32>,
    pub next_repeat: Option<f32>,
    pub already_hit: Vec<Entity>,
    pub size_scale: f32,
}

#[derive(Resource)]
pub struct PenguinSkillAssets {
    fish_bones_scene: Handle<Scene>,
    popsicle_scene: Handle<Scene>,
    snow_pile_scene: Handle<Scene>,
    snowflake_scene: Handle<Scene>,
    snowman_scene: Handle<Scene>,
    ultimate_snow_flat_large_scene: Handle<Scene>,
    ultimate_snow_flat_scene: Handle<Scene>,
    ice_tile_scene: Handle<Scene>,
    snow_bump_scene: Handle<Scene>,
    snow_hill_scene: Handle<Scene>,
    snowfort_scene: Handle<Scene>,
    cannon_scene: Handle<Scene>,
    boulder_scene: Handle<Scene>,
    snow_slope_scene: Handle<Scene>,
    snow_steep_slope_scene: Handle<Scene>,
    spring_scene: Handle<Scene>,
}

pub fn setup_penguin_skill_assets(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.insert_resource(PenguinSkillAssets {
        fish_bones_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_FISH_BONES_ASSET)),
        popsicle_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_POPSICLE_ASSET)),
        snow_pile_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_SNOW_PILE_ASSET)),
        snowflake_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_SNOWFLAKE_ASSET)),
        snowman_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_SNOWMAN_ASSET)),
        ultimate_snow_flat_large_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_ULTIMATE_SNOW_FLAT_LARGE_ASSET)),
        ultimate_snow_flat_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_ULTIMATE_SNOW_FLAT_ASSET)),
        ice_tile_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_ICE_TILE_ASSET)),
        snow_bump_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_SNOW_BUMP_ASSET)),
        snow_hill_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_SNOW_HILL_ASSET)),
        snowfort_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_SNOWFORT_ASSET)),
        cannon_scene: asset_server.load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_CANNON_ASSET)),
        boulder_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_BOULDER_ASSET)),
        snow_slope_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_SNOW_SLOPE_ASSET)),
        snow_steep_slope_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_SNOW_STEEP_SLOPE_ASSET)),
        spring_scene: asset_server.load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_SPRING_ASSET)),
    });
}

pub fn spawn_penguin_skill(
    commands: &mut Commands,
    assets: &PenguinSkillAssets,
    effect_assets: &EffectAssets,
    state: &MatchState,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    origin: Vec3,
    facing: Vec3,
    aim_held: bool,
    owner_size_scale: f32,
    skill: PenguinSkillId,
    targets: &[BeeSkillTargetSnapshot],
    active_skills: &[(PenguinSkillKind, usize, f32)],
) -> bool {
    if skill == PenguinSkillId::SnowflakeShot
        && penguin_snowflake_shot_is_active(owner_id, active_skills.iter().copied())
    {
        return false;
    }

    let facing = normalized_or_forward(facing);
    let size_scale = penguin_skill_size_scale(owner_size_scale);
    let target = penguin_skill_lock_target(owner_id, origin, facing, aim_held, state, targets);
    match skill {
        PenguinSkillId::FishTorpedo => {
            let spawn = grounded_position(origin + facing * 0.55, 0.26 * size_scale);
            let direction = target
                .and_then(|entity| target_position(entity, targets))
                .map(|position| flat_direction(spawn, position))
                .filter(|direction| direction.length_squared() > 0.01)
                .unwrap_or(facing);
            spawn_fish_torpedo(
                commands,
                assets,
                effect_assets,
                owner,
                owner_id,
                owner_style,
                spawn,
                direction,
                target,
                size_scale,
            );
        }
        PenguinSkillId::PopsicleBounce => {
            let spawn = origin + (Vec3::Y * 1.05 + facing * 0.52) * size_scale;
            let direction = target
                .and_then(|entity| target_position(entity, targets))
                .map(|position| flat_direction(spawn, position))
                .filter(|direction| direction.length_squared() > 0.01)
                .unwrap_or(facing);
            spawn_popsicle_bounce(
                commands,
                assets,
                effect_assets,
                owner,
                owner_id,
                owner_style,
                spawn,
                direction,
                size_scale,
            );
        }
        PenguinSkillId::SledWake => {
            let spawn = grounded_position(origin + facing * 0.75, 0.05);
            spawn_sled_wake(
                commands,
                assets,
                effect_assets,
                owner,
                owner_id,
                owner_style,
                spawn,
                facing,
                size_scale,
            );
        }
        PenguinSkillId::IceTrail => {
            spawn_ice_trail_line(
                commands,
                assets,
                effect_assets,
                owner,
                owner_id,
                origin,
                facing,
                size_scale,
                if aim_held { 5 } else { 3 },
            );
        }
        PenguinSkillId::UltimateIceField => {
            spawn_ultimate_ice_field(
                commands,
                assets,
                effect_assets,
                owner,
                owner_id,
                origin,
                facing,
                size_scale,
            );
        }
        PenguinSkillId::SnowmanDrop => {
            let ground = grounded_position(
                origin + facing * PENGUIN_SNOWMAN_DROP_FORWARD * size_scale,
                0.02,
            );
            let spawn = ground + Vec3::Y * PENGUIN_SNOWMAN_DROP_HEIGHT * size_scale;
            spawn_snowman_drop(
                commands,
                assets,
                effect_assets,
                owner,
                owner_id,
                owner_style,
                spawn,
                facing,
                size_scale,
            );
        }
        PenguinSkillId::SnowHillRamp => {
            let distance = if aim_held { 2.05 } else { 1.15 };
            let spawn = grounded_position(origin + facing * distance * size_scale, 0.02);
            spawn_snow_hill_ramp(
                commands,
                assets,
                effect_assets,
                owner,
                owner_id,
                spawn,
                facing,
                size_scale,
                aim_held,
            );
        }
        PenguinSkillId::SnowSlopeRide => {
            let spawn = grounded_position(origin + facing * 1.72 * size_scale, 0.02);
            spawn_snow_slope_ride(commands, assets, owner, owner_id, spawn, facing, size_scale);
        }
        PenguinSkillId::SnowfortCannon => {
            let spawn = grounded_position(origin + facing * 1.05 * size_scale, 0.02);
            spawn_snowfort_cannon(
                commands,
                assets,
                effect_assets,
                owner,
                owner_id,
                owner_style,
                spawn,
                facing,
                size_scale,
            );
        }
        PenguinSkillId::SpringPeck => {
            let spawn = grounded_position(origin + facing * 0.52 * size_scale, 0.03);
            spawn_spring_pad(
                commands,
                assets,
                effect_assets,
                owner,
                owner_id,
                spawn,
                facing,
                size_scale,
            );
            spawn_ice_trail_segment(
                commands,
                assets,
                owner,
                owner_id,
                spawn,
                facing,
                0.78 * size_scale,
                PENGUIN_ICE_TRAIL_LIFETIME * 0.35,
                size_scale,
            );
        }
        PenguinSkillId::BodySlam => {
            let distance = if aim_held { 1.35 } else { 0.72 };
            let spawn = grounded_position(origin + facing * distance * size_scale, 0.05);
            spawn_body_slam_shockwave(
                commands,
                assets,
                effect_assets,
                owner,
                owner_id,
                owner_style,
                spawn,
                facing,
                size_scale,
            );
        }
        PenguinSkillId::GlacierParade => {
            spawn_glacier_trail_printer(
                commands,
                effect_assets,
                owner,
                owner_id,
                origin,
                facing,
                size_scale,
            );
        }
        PenguinSkillId::SnowflakeShot => {
            let (spawn, direction) = snowflake_shot_spawn(origin, facing, size_scale);
            spawn_snowflake_shot(
                commands,
                assets,
                effect_assets,
                owner,
                owner_id,
                owner_style,
                spawn,
                direction,
                size_scale,
            );
        }
        PenguinSkillId::SnowflakeSwapShot => {
            return false;
        }
        PenguinSkillId::SnowflakeBurst => {
            let spawn = origin + Vec3::Y * 0.92 * size_scale;
            spawn_snowflake_burst(
                commands,
                assets,
                effect_assets,
                owner,
                owner_id,
                owner_style,
                spawn,
                facing,
                size_scale,
            );
        }
    }
    true
}

fn spawn_fish_torpedo(
    commands: &mut Commands,
    assets: &PenguinSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    direction: Vec3,
    target: Option<Entity>,
    size_scale: f32,
) {
    let facing = normalized_or_forward(direction);
    commands.spawn((
        SceneRoot(assets.fish_bones_scene.clone()),
        Transform::from_translation(position)
            .with_rotation(projectile_rotation(facing))
            .with_scale(penguin_skill_visual_scale(
                PenguinSkillKind::FishTorpedo,
                size_scale,
                0.0,
            )),
        active_penguin_skill(
            PenguinSkillKind::FishTorpedo,
            owner,
            owner_id,
            owner_style,
            facing,
            facing * PENGUIN_FISH_TORPEDO_SPEED,
            target,
            size_scale,
        ),
        Name::new("Penguin fish torpedo"),
    ));
    spawn_feedback_package(
        commands,
        effect_assets,
        position,
        facing,
        FeedbackPackageId::SpecialProjectileStartup,
    );
}

fn spawn_popsicle_bounce(
    commands: &mut Commands,
    assets: &PenguinSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    direction: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(direction);
    let velocity = facing * PENGUIN_POPSICLE_SPEED + Vec3::Y * PENGUIN_POPSICLE_LIFT;
    commands.spawn((
        SceneRoot(assets.popsicle_scene.clone()),
        Transform::from_translation(position)
            .with_rotation(projectile_rotation(facing))
            .with_scale(penguin_skill_visual_scale(
                PenguinSkillKind::PopsicleBounce,
                size_scale,
                0.0,
            )),
        active_penguin_skill(
            PenguinSkillKind::PopsicleBounce,
            owner,
            owner_id,
            owner_style,
            facing,
            velocity,
            None,
            size_scale,
        ),
        Name::new("Penguin popsicle bounce"),
    ));
    spawn_feedback_package(
        commands,
        effect_assets,
        position,
        facing,
        FeedbackPackageId::SpecialProjectileStartup,
    );
}

fn spawn_sled_wake(
    commands: &mut Commands,
    assets: &PenguinSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    commands.spawn((
        SceneRoot(assets.snow_pile_scene.clone()),
        Transform::from_translation(position)
            .with_rotation(projectile_rotation(facing))
            .with_scale(penguin_skill_visual_scale(
                PenguinSkillKind::SledWake,
                size_scale,
                0.0,
            )),
        active_penguin_skill(
            PenguinSkillKind::SledWake,
            owner,
            owner_id,
            owner_style,
            facing,
            facing * PENGUIN_SLED_WAKE_SPEED,
            None,
            size_scale,
        ),
        Name::new("Penguin sled wake"),
    ));
    spawn_feedback_package(
        commands,
        effect_assets,
        position,
        facing,
        FeedbackPackageId::SpecialHazardStartup,
    );
}

fn spawn_snowflake_shot(
    commands: &mut Commands,
    assets: &PenguinSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    direction: Vec3,
    size_scale: f32,
) {
    spawn_snowflake_projectile(
        commands,
        assets,
        effect_assets,
        owner,
        owner_id,
        owner_style,
        position,
        direction,
        size_scale,
        PenguinSkillKind::SnowflakeShard,
        "Penguin snowflake shot",
    );
}

fn spawn_snowflake_projectile(
    commands: &mut Commands,
    assets: &PenguinSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    direction: Vec3,
    size_scale: f32,
    kind: PenguinSkillKind,
    name: &'static str,
) {
    let direction = normalized_or_forward(direction);
    commands.spawn((
        SceneRoot(assets.snowflake_scene.clone()),
        Transform::from_translation(position)
            .with_rotation(projectile_rotation(direction))
            .with_scale(penguin_skill_visual_scale(kind, size_scale, 0.0)),
        active_penguin_skill(
            kind,
            owner,
            owner_id,
            owner_style,
            direction,
            direction * PENGUIN_SNOWFLAKE_SPEED,
            None,
            size_scale,
        ),
        Name::new(name),
    ));
    spawn_feedback_package(
        commands,
        effect_assets,
        position,
        direction,
        FeedbackPackageId::SpecialProjectileStartup,
    );
}

fn spawn_snowflake_burst(
    commands: &mut Commands,
    assets: &PenguinSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    for direction in snowflake_burst_directions(facing) {
        let spawn = position + direction * 0.24 * size_scale;
        commands.spawn((
            SceneRoot(assets.snowflake_scene.clone()),
            Transform::from_translation(spawn)
                .with_rotation(projectile_rotation(direction))
                .with_scale(penguin_skill_visual_scale(
                    PenguinSkillKind::SnowflakeShard,
                    size_scale,
                    0.0,
                )),
            active_penguin_skill(
                PenguinSkillKind::SnowflakeShard,
                owner,
                owner_id,
                owner_style,
                direction,
                direction * PENGUIN_SNOWFLAKE_SPEED,
                None,
                size_scale,
            ),
            Name::new("Penguin snowflake shard"),
        ));
    }
    spawn_feedback_package(
        commands,
        effect_assets,
        position,
        facing,
        FeedbackPackageId::SpecialProjectileStartup,
    );
}

fn spawn_snowman_drop(
    commands: &mut Commands,
    assets: &PenguinSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    commands.spawn((
        SceneRoot(assets.snowman_scene.clone()),
        Transform::from_translation(position)
            .with_rotation(projectile_rotation(facing))
            .with_scale(penguin_skill_visual_scale(
                PenguinSkillKind::SnowmanDrop,
                size_scale,
                0.0,
            )),
        active_penguin_skill(
            PenguinSkillKind::SnowmanDrop,
            owner,
            owner_id,
            owner_style,
            facing,
            Vec3::Y * -PENGUIN_SNOWMAN_DROP_INITIAL_FALL_SPEED,
            None,
            size_scale,
        ),
        Name::new("Penguin snowman drop"),
    ));
    spawn_feedback_package(
        commands,
        effect_assets,
        position,
        facing,
        FeedbackPackageId::SpecialProjectileStartup,
    );
}

fn spawn_ice_trail_line(
    commands: &mut Commands,
    assets: &PenguinSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    origin: Vec3,
    facing: Vec3,
    size_scale: f32,
    count: usize,
) {
    let facing = normalized_or_forward(facing);
    for index in 0..count {
        let offset = index as f32 * 0.72 * size_scale;
        let position = grounded_position(origin - facing * offset, 0.015);
        spawn_ice_trail_segment(
            commands,
            assets,
            owner,
            owner_id,
            position,
            facing,
            PENGUIN_ICE_TRAIL_RADIUS * size_scale,
            PENGUIN_ICE_TRAIL_LIFETIME,
            size_scale,
        );
    }
    spawn_feedback_package(
        commands,
        effect_assets,
        grounded_position(origin, 0.12),
        facing,
        FeedbackPackageId::SpecialHazardStartup,
    );
}

fn spawn_ice_trail_segment(
    commands: &mut Commands,
    assets: &PenguinSkillAssets,
    owner: Entity,
    owner_id: usize,
    position: Vec3,
    facing: Vec3,
    radius: f32,
    lifetime: f32,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    commands.spawn((
        SceneRoot(assets.ice_tile_scene.clone()),
        Transform::from_translation(grounded_position(position, 0.012))
            .with_rotation(projectile_rotation(facing))
            .with_scale(Vec3::new(0.95, 0.16, 1.2) * size_scale),
        active_penguin_surface(
            PenguinSurfaceKind::IceTrailSegment,
            owner,
            owner_id,
            facing,
            radius,
            lifetime,
            size_scale,
        ),
        Name::new("Penguin ice trail"),
    ));
}

fn spawn_ultimate_ice_field(
    commands: &mut Commands,
    assets: &PenguinSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    origin: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    let right = Vec3::new(facing.z, 0.0, -facing.x).normalize_or_zero();
    let spacing = PENGUIN_ULTIMATE_ICE_FIELD_TILE_SPACING * size_scale;
    for x in 0..PENGUIN_ULTIMATE_ICE_FIELD_GRID_SIDE {
        for z in 0..PENGUIN_ULTIMATE_ICE_FIELD_GRID_SIDE {
            let offset = right * ultimate_ice_field_grid_axis_offset(x) * spacing
                + facing * ultimate_ice_field_grid_axis_offset(z) * spacing;
            spawn_ultimate_ice_tile(
                commands,
                assets,
                owner,
                owner_id,
                grounded_position(origin + offset, 0.014),
                facing,
                size_scale,
            );
        }
    }
    spawn_feedback_package(
        commands,
        effect_assets,
        grounded_position(origin, 0.12),
        facing,
        FeedbackPackageId::SpecialHazardStartup,
    );
}

fn ultimate_ice_field_grid_axis_offset(index: i32) -> f32 {
    index as f32 - (PENGUIN_ULTIMATE_ICE_FIELD_GRID_SIDE as f32 - 1.0) * 0.5
}

fn spawn_ultimate_ice_tile(
    commands: &mut Commands,
    assets: &PenguinSkillAssets,
    owner: Entity,
    owner_id: usize,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    let tile = commands
        .spawn((
            SceneRoot(assets.ultimate_snow_flat_large_scene.clone()),
            Transform::from_translation(grounded_position(
                position,
                PENGUIN_ULTIMATE_SNOW_FIELD_CLEARANCE,
            ))
            .with_rotation(projectile_rotation(facing))
            .with_scale(ultimate_snow_field_visual_scale(
                size_scale,
                PENGUIN_ULTIMATE_ICE_FIELD_LIFETIME,
            )),
            active_penguin_surface(
                PenguinSurfaceKind::UltimateIceTile,
                owner,
                owner_id,
                facing,
                PENGUIN_ULTIMATE_ICE_FIELD_TILE_RADIUS * size_scale,
                PENGUIN_ULTIMATE_ICE_FIELD_LIFETIME,
                size_scale,
            ),
            Name::new("Penguin ultimate snow field"),
        ))
        .id();

    commands.entity(tile).with_children(|parent| {
        spawn_ultimate_snow_flat_detail(parent, assets, Vec3::new(-0.31, 0.012, 0.18), 0.68);
        spawn_ultimate_snow_flat_detail(parent, assets, Vec3::new(0.27, 0.014, -0.18), -0.54);
        spawn_ultimate_snow_flat_detail(parent, assets, Vec3::new(0.04, 0.016, 0.32), 1.36);
    });
}

fn spawn_ultimate_snow_flat_detail(
    parent: &mut ChildSpawnerCommands,
    assets: &PenguinSkillAssets,
    offset: Vec3,
    angle: f32,
) {
    parent.spawn((
        SceneRoot(assets.ultimate_snow_flat_scene.clone()),
        Transform::from_translation(offset)
            .with_rotation(Quat::from_rotation_y(angle))
            .with_scale(Vec3::splat(PENGUIN_ULTIMATE_SNOW_FLAT_DETAIL_SCALE)),
        Name::new("Penguin ultimate snow flat detail"),
    ));
}

fn spawn_snow_hill_ramp(
    commands: &mut Commands,
    assets: &PenguinSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
    steep: bool,
) {
    let facing = normalized_or_forward(facing);
    let scene = if steep {
        assets.snow_steep_slope_scene.clone()
    } else if size_scale > 1.0 {
        assets.snow_hill_scene.clone()
    } else {
        assets.snow_slope_scene.clone()
    };
    commands.spawn((
        SceneRoot(scene),
        Transform::from_translation(grounded_position(position, 0.02))
            .with_rotation(projectile_rotation(facing))
            .with_scale(Vec3::splat(0.72 * size_scale)),
        active_penguin_surface(
            PenguinSurfaceKind::SnowHillRamp,
            owner,
            owner_id,
            facing,
            PENGUIN_SNOW_HILL_RADIUS * size_scale,
            PENGUIN_SNOW_HILL_LIFETIME,
            size_scale,
        ),
        Name::new("Penguin snow hill ramp"),
    ));
    spawn_feedback_package(
        commands,
        effect_assets,
        position,
        facing,
        FeedbackPackageId::SpecialHazardStartup,
    );
}

fn spawn_snow_slope_ride(
    commands: &mut Commands,
    assets: &PenguinSkillAssets,
    owner: Entity,
    owner_id: usize,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    commands.spawn((
        SceneRoot(assets.snow_slope_scene.clone()),
        Transform::from_translation(grounded_position(position, 0.02))
            .with_rotation(snow_slope_ride_rotation(facing))
            .with_scale(Vec3::splat(0.72 * size_scale)),
        active_penguin_surface(
            PenguinSurfaceKind::SnowSlopeRide,
            owner,
            owner_id,
            facing,
            PENGUIN_SNOW_SLOPE_RIDE_RADIUS * size_scale,
            PENGUIN_SNOW_SLOPE_RIDE_LIFETIME,
            size_scale,
        ),
        Name::new("Penguin snow slope ride"),
    ));
}

fn snow_slope_ride_rotation(facing: Vec3) -> Quat {
    projectile_rotation(-normalized_or_forward(facing))
}

fn spawn_snowfort_cannon(
    commands: &mut Commands,
    assets: &PenguinSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    let side = Vec3::new(-facing.z, 0.0, facing.x).normalize_or_zero();
    commands.spawn((
        SceneRoot(assets.snowfort_scene.clone()),
        Transform::from_translation(grounded_position(position - facing * 0.22, 0.02))
            .with_rotation(projectile_rotation(facing))
            .with_scale(Vec3::splat(0.66 * size_scale)),
        active_penguin_surface(
            PenguinSurfaceKind::SnowfortCannon,
            owner,
            owner_id,
            facing,
            0.0,
            PENGUIN_SNOWFORT_LIFETIME,
            size_scale,
        ),
        Name::new("Penguin snowfort base"),
    ));
    commands.spawn((
        SceneRoot(assets.cannon_scene.clone()),
        Transform::from_translation(grounded_position(position + side * 0.08, 0.42 * size_scale))
            .with_rotation(projectile_rotation(facing))
            .with_scale(Vec3::splat(0.72 * size_scale)),
        active_penguin_surface(
            PenguinSurfaceKind::SnowfortCannon,
            owner,
            owner_id,
            facing,
            0.0,
            PENGUIN_SNOWFORT_LIFETIME,
            size_scale,
        ),
        Name::new("Penguin snowfort cannon"),
    ));
    spawn_snow_boulder(
        commands,
        assets,
        effect_assets,
        owner,
        owner_id,
        owner_style,
        position + facing * 0.48 + Vec3::Y * 0.52 * size_scale,
        facing,
        size_scale,
    );
}

fn spawn_snow_boulder(
    commands: &mut Commands,
    assets: &PenguinSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    commands.spawn((
        SceneRoot(assets.boulder_scene.clone()),
        Transform::from_translation(position)
            .with_rotation(projectile_rotation(facing))
            .with_scale(penguin_skill_visual_scale(
                PenguinSkillKind::SnowBoulder,
                size_scale,
                0.0,
            )),
        active_penguin_skill(
            PenguinSkillKind::SnowBoulder,
            owner,
            owner_id,
            owner_style,
            facing,
            facing * PENGUIN_BOULDER_SPEED,
            None,
            size_scale,
        ),
        Name::new("Penguin snow boulder"),
    ));
    spawn_feedback_package(
        commands,
        effect_assets,
        position,
        facing,
        FeedbackPackageId::SpecialProjectileStartup,
    );
}

fn spawn_spring_pad(
    commands: &mut Commands,
    assets: &PenguinSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    commands.spawn((
        SceneRoot(assets.spring_scene.clone()),
        Transform::from_translation(position)
            .with_rotation(projectile_rotation(facing))
            .with_scale(Vec3::splat(0.72 * size_scale)),
        active_penguin_surface(
            PenguinSurfaceKind::SpringPad,
            owner,
            owner_id,
            facing,
            PENGUIN_SPRING_PAD_RADIUS * size_scale,
            PENGUIN_SPRING_PAD_LIFETIME,
            size_scale,
        ),
        Name::new("Penguin spring peck pad"),
    ));
    spawn_feedback_package(
        commands,
        effect_assets,
        position,
        facing,
        FeedbackPackageId::SpecialHazardStartup,
    );
}

fn spawn_body_slam_shockwave(
    commands: &mut Commands,
    assets: &PenguinSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    commands.spawn((
        SceneRoot(assets.snow_bump_scene.clone()),
        Transform::from_translation(position)
            .with_rotation(projectile_rotation(facing))
            .with_scale(penguin_skill_visual_scale(
                PenguinSkillKind::BodySlamShockwave,
                size_scale,
                0.0,
            )),
        active_penguin_skill(
            PenguinSkillKind::BodySlamShockwave,
            owner,
            owner_id,
            owner_style,
            facing,
            Vec3::ZERO,
            None,
            size_scale,
        ),
        Name::new("Penguin body slam shockwave"),
    ));
    spawn_snow_hill_ramp(
        commands,
        assets,
        effect_assets,
        owner,
        owner_id,
        position + facing * 0.58 * size_scale,
        facing,
        size_scale,
        true,
    );
    spawn_ice_trail_line(
        commands,
        assets,
        effect_assets,
        owner,
        owner_id,
        position,
        facing,
        size_scale,
        4,
    );
}

fn spawn_glacier_trail_printer(
    commands: &mut Commands,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    origin: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    commands.spawn((
        Transform::from_translation(origin),
        active_penguin_surface(
            PenguinSurfaceKind::GlacierTrailPrinter,
            owner,
            owner_id,
            facing,
            0.0,
            PENGUIN_GLACIER_PARADE_LIFETIME,
            size_scale,
        ),
        Name::new("Penguin glacier parade trail printer"),
    ));
    spawn_feedback_package(
        commands,
        effect_assets,
        origin,
        facing,
        FeedbackPackageId::SpecialHazardStartup,
    );
}

fn active_penguin_skill(
    kind: PenguinSkillKind,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    facing: Vec3,
    velocity: Vec3,
    target: Option<Entity>,
    size_scale: f32,
) -> ActivePenguinSkill {
    let size_scale = penguin_skill_size_scale(size_scale);
    let (payload_id, shape_id, source, lifetime, radius, guard_stamina_damage, repeat_interval) =
        match kind {
            PenguinSkillKind::FishTorpedo => (
                AttackPayloadId::PenguinFishTorpedo,
                AttackShapeId::ProjectileBolt,
                ImpactSource::Projectile,
                PENGUIN_FISH_TORPEDO_LIFETIME,
                PENGUIN_FISH_TORPEDO_RADIUS,
                8.0,
                None,
            ),
            PenguinSkillKind::PopsicleBounce => (
                AttackPayloadId::PenguinPopsicleBounce,
                AttackShapeId::ProjectileBolt,
                ImpactSource::Projectile,
                PENGUIN_POPSICLE_LIFETIME,
                PENGUIN_POPSICLE_RADIUS,
                9.0,
                None,
            ),
            PenguinSkillKind::SledWake => (
                AttackPayloadId::PenguinSledWake,
                AttackShapeId::HazardField,
                ImpactSource::Hazard,
                PENGUIN_SLED_WAKE_LIFETIME,
                PENGUIN_SLED_WAKE_RADIUS,
                6.0,
                Some(PENGUIN_SLED_WAKE_TICK),
            ),
            PenguinSkillKind::SnowflakeShard => (
                AttackPayloadId::PenguinSnowflakeShard,
                AttackShapeId::ProjectileBolt,
                ImpactSource::Projectile,
                PENGUIN_SNOWFLAKE_LIFETIME,
                PENGUIN_SNOWFLAKE_RADIUS,
                10.0,
                None,
            ),
            PenguinSkillKind::SnowBoulder => (
                AttackPayloadId::PenguinSnowBoulder,
                AttackShapeId::ProjectileBolt,
                ImpactSource::Projectile,
                PENGUIN_BOULDER_LIFETIME,
                PENGUIN_BOULDER_RADIUS,
                11.0,
                None,
            ),
            PenguinSkillKind::SnowmanDrop => (
                AttackPayloadId::PenguinSnowmanDrop,
                AttackShapeId::ProjectileBolt,
                ImpactSource::Projectile,
                PENGUIN_SNOWMAN_DROP_LIFETIME,
                PENGUIN_SNOWMAN_DROP_RADIUS,
                7.0,
                None,
            ),
            PenguinSkillKind::BodySlamShockwave => (
                AttackPayloadId::PenguinBodySlamShockwave,
                AttackShapeId::ShockwaveRing,
                ImpactSource::Hazard,
                PENGUIN_BODY_SLAM_LIFETIME,
                PENGUIN_BODY_SLAM_RADIUS,
                14.0,
                None,
            ),
        };

    ActivePenguinSkill {
        kind,
        owner,
        owner_id,
        owner_style,
        payload_id,
        shape_id,
        source,
        facing: normalized_or_forward(facing),
        velocity,
        target,
        lifetime,
        age: 0.0,
        radius: radius * size_scale,
        guard_stamina_damage,
        repeat_interval,
        next_repeat: repeat_interval,
        already_hit: Vec::new(),
        size_scale,
    }
}

pub fn penguin_snowflake_shot_is_active(
    owner_id: usize,
    active_skills: impl IntoIterator<Item = (PenguinSkillKind, usize, f32)>,
) -> bool {
    active_skills
        .into_iter()
        .any(|(kind, skill_owner_id, lifetime)| {
            kind == PenguinSkillKind::SnowflakeShard && skill_owner_id == owner_id && lifetime > 0.0
        })
}

fn active_penguin_surface(
    kind: PenguinSurfaceKind,
    owner: Entity,
    owner_id: usize,
    facing: Vec3,
    radius: f32,
    lifetime: f32,
    size_scale: f32,
) -> ActivePenguinSurface {
    ActivePenguinSurface {
        kind,
        owner,
        owner_id,
        facing: normalized_or_forward(facing),
        lifetime,
        age: 0.0,
        radius,
        next_tick: 0.0,
        already_touched: Vec::new(),
        size_scale: penguin_skill_size_scale(size_scale),
    }
}

fn penguin_skill_size_scale(owner_size_scale: f32) -> f32 {
    owner_size_scale.max(0.1)
}

fn penguin_skill_visual_scale(kind: PenguinSkillKind, size_scale: f32, age: f32) -> Vec3 {
    let size_scale = penguin_skill_size_scale(size_scale);
    match kind {
        PenguinSkillKind::FishTorpedo => Vec3::splat(1.75 * size_scale),
        PenguinSkillKind::PopsicleBounce => Vec3::splat(1.55 * size_scale),
        PenguinSkillKind::SledWake => Vec3::splat(sled_wake_visual_pulse(age) * size_scale),
        PenguinSkillKind::SnowflakeShard => Vec3::splat(1.35 * size_scale),
        PenguinSkillKind::SnowBoulder => Vec3::splat(0.9 * size_scale),
        PenguinSkillKind::SnowmanDrop => {
            Vec3::splat(0.82 * PENGUIN_SNOWMAN_DROP_SIZE_MULTIPLIER * size_scale)
        }
        PenguinSkillKind::BodySlamShockwave => {
            Vec3::new(1.2, 0.28, 1.2) * (1.0 + age * 1.5) * size_scale
        }
    }
}

fn sled_wake_visual_pulse(age: f32) -> f32 {
    0.84 + (age * 9.0).sin().abs() * 0.12
}

pub fn update_penguin_skills(
    time: Res<Time>,
    mut commands: Commands,
    assets: Res<PenguinSkillAssets>,
    effect_assets: Res<EffectAssets>,
    state: Res<MatchState>,
    feel: Res<CombatFeelTuning>,
    mut hitstop: ResMut<Hitstop>,
    mut camera_effects: ResMut<HitEffects>,
    mut telemetry: ResMut<MatchTelemetry>,
    surfaces: Query<
        (&ActivePenguinSurface, &Transform),
        (Without<Fighter>, Without<ActivePenguinSkill>),
    >,
    mut skills: Query<
        (Entity, &mut ActivePenguinSkill, &mut Transform),
        (Without<Fighter>, Without<ActivePenguinSurface>),
    >,
    mut fighters: ParamSet<(
        Query<(&Fighter, &Transform), With<Fighter>>,
        Query<
            (
                Entity,
                &Fighter,
                &mut FighterStats,
                &mut FighterMotor,
                &mut FighterActionState,
                &FighterStyle,
                &FighterEquipment,
                &mut Transform,
            ),
            With<Fighter>,
        >,
    )>,
) {
    if hitstop.active() {
        return;
    }

    let dt = time.delta_secs();
    let snowfield_centers = active_snowfield_centers(&surfaces);
    for (skill_entity, mut skill, mut transform) in &mut skills {
        skill.age += dt;
        skill.lifetime -= dt;
        update_skill_repeat_window(&mut skill, &mut camera_effects);
        update_penguin_skill_motion(&mut skill, &mut transform, dt, &fighters.p0());

        let mut hit_this_frame = false;
        {
            let mut target_fighters = fighters.p1();
            for (
                target_entity,
                target,
                mut stats,
                mut motor,
                mut action,
                target_style,
                target_equipment,
                mut target_transform,
            ) in &mut target_fighters
            {
                if target_entity == skill.owner && skill.age < 0.16 {
                    continue;
                }
                if target_entity == skill.owner && skill.kind == PenguinSkillKind::SnowmanDrop {
                    continue;
                }
                if !state.combat_target_allowed_for_state(skill.owner_id, target.id) {
                    continue;
                }
                if skill.already_hit.contains(&target_entity)
                    || !can_receive_impact(&stats, &action)
                    || !penguin_skill_overlaps_target(
                        &skill,
                        transform.translation,
                        &target_transform,
                    )
                {
                    continue;
                }

                let profile = penguin_skill_impact_profile(&skill, &feel);
                apply_impact(
                    &mut commands,
                    &effect_assets,
                    &mut camera_effects,
                    &mut hitstop,
                    &state,
                    &mut stats,
                    &mut motor,
                    &mut action,
                    &target_transform,
                    None,
                    transform.translation,
                    profile,
                    DamageDefenderProfile::from_loadout(target_style, target_equipment),
                    &mut telemetry,
                );
                spawn_feedback_package(
                    &mut commands,
                    &effect_assets,
                    target_transform.translation + Vec3::Y * (FIGHTER_HEIGHT * 0.58),
                    skill.facing,
                    impact_package(skill.kind),
                );
                if skill.kind == PenguinSkillKind::SledWake && motor.grounded {
                    motor.velocity.x *= PENGUIN_SLED_WAKE_DAMPING;
                    motor.velocity.z *= PENGUIN_SLED_WAKE_DAMPING;
                    camera_effects.push_feedback_cue("impact_penguin_sled_wake", skill.source, 24);
                }
                if skill.kind == PenguinSkillKind::SnowBoulder {
                    spawn_ice_trail_segment(
                        &mut commands,
                        &assets,
                        skill.owner,
                        skill.owner_id,
                        grounded_position(target_transform.translation, 0.02),
                        skill.facing,
                        PENGUIN_ICE_TRAIL_RADIUS * 0.82 * skill.size_scale,
                        PENGUIN_ICE_TRAIL_LIFETIME * 0.5,
                        skill.size_scale,
                    );
                }
                if skill.kind == PenguinSkillKind::SnowmanDrop {
                    motor.velocity.x = 0.0;
                    motor.velocity.z = 0.0;
                    camera_effects.push_feedback_cue("impact_special_projectile", skill.source, 28);
                }
                if skill.kind == PenguinSkillKind::SnowflakeShard
                    && let Some(destination) = snowflake_magic_destination(
                        target_transform.translation,
                        snowfield_centers.iter().copied(),
                    )
                {
                    target_transform.translation = destination;
                    motor.velocity = Vec3::ZERO;
                    motor.grounded = true;
                    motor.landing_aftermath = None;
                    motor.knockdown_on_land = false;
                    motor.reaction_bounces = 0;
                    camera_effects.push_feedback_cue(
                        "impact_penguin_snowflake_warp",
                        skill.source,
                        32,
                    );
                    spawn_feedback_package(
                        &mut commands,
                        &effect_assets,
                        destination + Vec3::Y * (FIGHTER_HEIGHT * 0.58),
                        skill.facing,
                        FeedbackPackageId::SpecialHazardImpact,
                    );
                }
                skill.already_hit.push(target_entity);
                hit_this_frame = true;

                if !penguin_skill_persists_after_hit(skill.kind) {
                    skill.lifetime = 0.0;
                    break;
                }
            }
        }

        let popsicle_grounded = popsicle_touched_ground(&skill, transform.translation);
        let snowman_grounded = snowman_touched_ground(&skill, transform.translation);
        if snowman_grounded {
            let landing = grounded_position(transform.translation, 0.02);
            spawn_snowman_landing_snow(
                &mut commands,
                &assets,
                skill.owner,
                skill.owner_id,
                landing,
                skill.facing,
                skill.size_scale,
            );
            spawn_feedback_package(
                &mut commands,
                &effect_assets,
                landing,
                skill.facing,
                FeedbackPackageId::SpecialHazardImpact,
            );
            commands.entity(skill_entity).despawn();
            continue;
        }
        if skill.lifetime <= 0.0 || popsicle_grounded || should_despawn_skill(transform.translation)
        {
            if !hit_this_frame {
                spawn_feedback_package(
                    &mut commands,
                    &effect_assets,
                    transform.translation,
                    skill.facing,
                    despawn_package(skill.kind),
                );
            }
            commands.entity(skill_entity).despawn();
        }
    }
}

pub fn update_penguin_surfaces(
    time: Res<Time>,
    mut commands: Commands,
    assets: Res<PenguinSkillAssets>,
    effect_assets: Res<EffectAssets>,
    state: Res<MatchState>,
    hitstop: Res<Hitstop>,
    mut camera_effects: ResMut<HitEffects>,
    mut surfaces: Query<(Entity, &mut ActivePenguinSurface, &mut Transform), Without<Fighter>>,
    mut fighters: Query<
        (
            Entity,
            &Fighter,
            &mut FighterStats,
            &mut FighterMotor,
            &mut Transform,
        ),
        With<Fighter>,
    >,
) {
    if hitstop.active() {
        return;
    }

    let dt = time.delta_secs();
    let fighter_snapshots: Vec<_> = fighters
        .iter_mut()
        .map(|(entity, _, _, motor, transform)| {
            (
                entity,
                transform.translation,
                normalized_or_forward(motor.facing),
                planar_speed(motor.velocity),
            )
        })
        .collect();
    let mut ice_segments = Vec::new();
    for (surface_entity, mut surface, mut transform) in &mut surfaces {
        surface.age += dt;
        surface.lifetime -= dt;

        match surface.kind {
            PenguinSurfaceKind::IceTrailSegment => {
                ice_segments.push((surface_entity, surface.owner, surface.age));
                transform.scale = ice_trail_visual_scale(surface.size_scale, surface.lifetime);
            }
            PenguinSurfaceKind::UltimateIceTile => {
                transform.scale =
                    ultimate_snow_field_visual_scale(surface.size_scale, surface.lifetime);
            }
            PenguinSurfaceKind::SnowHillRamp => {
                update_ramp_hazard(
                    &mut commands,
                    &effect_assets,
                    &state,
                    &mut camera_effects,
                    &mut surface,
                    transform.translation,
                    &mut fighters,
                );
            }
            PenguinSurfaceKind::SnowSlopeRide => {
                update_snow_slope_ride(&mut surface, transform.translation, &mut fighters);
            }
            PenguinSurfaceKind::SpringPad => {
                update_spring_pad(
                    &mut commands,
                    &effect_assets,
                    &state,
                    &mut camera_effects,
                    &mut surface,
                    transform.translation,
                    &mut fighters,
                );
                let spring_pulse = 1.0 + (surface.age * 12.0).sin().abs() * 0.06;
                transform.scale = Vec3::splat(0.72 * spring_pulse * surface.size_scale);
            }
            PenguinSurfaceKind::SnowfortCannon => {
                transform.scale = Vec3::splat((1.0 - surface.age * 0.08).max(0.82));
            }
            PenguinSurfaceKind::GlacierTrailPrinter => {
                update_glacier_trail_printer(
                    &mut commands,
                    &assets,
                    &effect_assets,
                    &mut surface,
                    &mut transform,
                    &fighter_snapshots,
                );
            }
        }

        if surface.lifetime <= 0.0 || should_despawn_skill(transform.translation) {
            commands.entity(surface_entity).despawn();
        }
    }

    for entity in oldest_ice_segments_to_despawn(&ice_segments, PENGUIN_ICE_TRAIL_CAP_PER_OWNER) {
        commands.entity(entity).despawn();
    }
}

fn update_ramp_hazard(
    commands: &mut Commands,
    effect_assets: &EffectAssets,
    state: &MatchState,
    camera_effects: &mut HitEffects,
    surface: &mut ActivePenguinSurface,
    position: Vec3,
    fighters: &mut Query<
        (
            Entity,
            &Fighter,
            &mut FighterStats,
            &mut FighterMotor,
            &mut Transform,
        ),
        With<Fighter>,
    >,
) {
    for (fighter_entity, fighter, mut stats, mut motor, fighter_transform) in fighters.iter_mut() {
        if surface.already_touched.contains(&fighter_entity)
            || !surface_can_touch_fighter(surface, fighter_entity, fighter, state)
            || !surface_overlaps_fighter(surface, position, fighter_transform.translation)
        {
            continue;
        }
        let touch = snow_hill_ramp_touch(surface, fighter_entity);
        let push = if touch.owner_ride {
            normalized_or_forward(surface.facing)
        } else {
            ramp_push_direction(surface.facing, position, fighter_transform.translation)
        };
        motor.velocity.x += push.x * touch.forward_push;
        motor.velocity.z += push.z * touch.forward_push;
        motor.velocity.y = motor.velocity.y.max(touch.lift);
        motor.grounded = false;
        if touch.owner_ride {
            let planar_speed = Vec2::new(motor.velocity.x, motor.velocity.z).length();
            motor.dash_slide_timer = motor.dash_slide_timer.max(touch.slide_timer);
            motor.impact_speed_limit_timer =
                motor.impact_speed_limit_timer.max(touch.speed_limit_timer);
            motor.impact_speed_limit = motor.impact_speed_limit.max(planar_speed);
            motor.landing_stick_timer = 0.0;
        }
        stats.hud_flash = stats.hud_flash.max(0.12);
        surface.already_touched.push(fighter_entity);
        let cue = if touch.owner_ride {
            "impact_penguin_snow_hill_ski"
        } else {
            "impact_penguin_snow_hill_ramp"
        };
        camera_effects.push_feedback_cue(cue, ImpactSource::Hazard, 22);
        spawn_feedback_package(
            commands,
            effect_assets,
            fighter_transform.translation + Vec3::Y * 0.45,
            surface.facing,
            FeedbackPackageId::SpecialHazardImpact,
        );
    }
}

fn update_snow_slope_ride(
    surface: &mut ActivePenguinSurface,
    position: Vec3,
    fighters: &mut Query<
        (
            Entity,
            &Fighter,
            &mut FighterStats,
            &mut FighterMotor,
            &mut Transform,
        ),
        With<Fighter>,
    >,
) {
    for (fighter_entity, _, _, mut motor, mut fighter_transform) in fighters.iter_mut() {
        if fighter_entity != surface.owner || surface.already_touched.contains(&fighter_entity) {
            continue;
        }

        let Some(contact) = snow_slope_ride_contact(
            position,
            surface.facing,
            fighter_transform.translation,
            surface.size_scale,
        ) else {
            continue;
        };

        fighter_transform.translation.y = contact.target_y;
        motor.velocity.y = motor.velocity.y.max(0.0);
        motor.grounded = true;
        motor.dash_slide_timer = motor.dash_slide_timer.max(PENGUIN_SNOW_SLOPE_RIDE_SLIDE);

        if contact.progress >= PENGUIN_SNOW_SLOPE_RIDE_EXIT_PROGRESS {
            let push = normalized_or_forward(surface.facing);
            motor.velocity.x += push.x * PENGUIN_SNOW_SLOPE_RIDE_PUSH;
            motor.velocity.z += push.z * PENGUIN_SNOW_SLOPE_RIDE_PUSH;
            motor.velocity.y = motor.velocity.y.max(PENGUIN_SNOW_SLOPE_RIDE_LIFT);
            motor.grounded = false;
            let planar_speed = Vec2::new(motor.velocity.x, motor.velocity.z).length();
            motor.impact_speed_limit_timer = motor
                .impact_speed_limit_timer
                .max(PENGUIN_SNOW_SLOPE_RIDE_SPEED_LIMIT);
            motor.impact_speed_limit = motor.impact_speed_limit.max(planar_speed);
            motor.landing_stick_timer = 0.0;
            surface.already_touched.push(fighter_entity);
        }
    }
}

fn update_spring_pad(
    commands: &mut Commands,
    effect_assets: &EffectAssets,
    state: &MatchState,
    camera_effects: &mut HitEffects,
    surface: &mut ActivePenguinSurface,
    position: Vec3,
    fighters: &mut Query<
        (
            Entity,
            &Fighter,
            &mut FighterStats,
            &mut FighterMotor,
            &mut Transform,
        ),
        With<Fighter>,
    >,
) {
    for (fighter_entity, fighter, mut stats, mut motor, fighter_transform) in fighters.iter_mut() {
        if surface.already_touched.contains(&fighter_entity)
            || !surface_can_touch_fighter(surface, fighter_entity, fighter, state)
            || !surface_overlaps_fighter(surface, position, fighter_transform.translation)
        {
            continue;
        }
        motor.velocity.x += surface.facing.x * 1.25;
        motor.velocity.z += surface.facing.z * 1.25;
        motor.velocity.y = motor.velocity.y.max(PENGUIN_SPRING_PAD_LIFT);
        motor.grounded = false;
        stats.hud_flash = stats.hud_flash.max(0.1);
        surface.already_touched.push(fighter_entity);
        camera_effects.push_feedback_cue("impact_penguin_spring_peck", ImpactSource::Hazard, 20);
        spawn_feedback_package(
            commands,
            effect_assets,
            fighter_transform.translation + Vec3::Y * 0.45,
            surface.facing,
            FeedbackPackageId::SpecialHazardImpact,
        );
    }
}

fn update_glacier_trail_printer(
    commands: &mut Commands,
    assets: &PenguinSkillAssets,
    effect_assets: &EffectAssets,
    surface: &mut ActivePenguinSurface,
    transform: &mut Transform,
    fighters: &[(Entity, Vec3, Vec3, f32)],
) {
    let Some((_, owner_position, owner_facing, owner_speed)) = fighters
        .iter()
        .find(|(entity, _, _, _)| *entity == surface.owner)
        .copied()
    else {
        surface.lifetime = 0.0;
        return;
    };
    transform.translation = owner_position;
    surface.facing = owner_facing;
    if owner_speed <= 0.35 && surface.age > 0.2 {
        return;
    }
    while surface.age >= surface.next_tick {
        spawn_ice_trail_segment(
            commands,
            assets,
            surface.owner,
            surface.owner_id,
            grounded_position(owner_position, 0.015),
            surface.facing,
            PENGUIN_ICE_TRAIL_RADIUS * 1.08 * surface.size_scale,
            PENGUIN_ICE_TRAIL_LIFETIME,
            surface.size_scale,
        );
        spawn_feedback_package(
            commands,
            effect_assets,
            owner_position,
            surface.facing,
            FeedbackPackageId::SpecialHazardStartup,
        );
        surface.next_tick += PENGUIN_GLACIER_PARADE_TICK;
    }
}

fn active_snowfield_centers(
    surfaces: &Query<
        (&ActivePenguinSurface, &Transform),
        (Without<Fighter>, Without<ActivePenguinSkill>),
    >,
) -> Vec<Vec3> {
    surfaces
        .iter()
        .filter_map(|(surface, transform)| {
            active_snowfield_center(surface.kind, surface.lifetime, transform.translation)
        })
        .collect()
}

fn active_snowfield_center(
    kind: PenguinSurfaceKind,
    lifetime: f32,
    position: Vec3,
) -> Option<Vec3> {
    (lifetime > 0.0 && snowflake_magic_surface_kind(kind)).then_some(position)
}

fn snowflake_magic_surface_kind(kind: PenguinSurfaceKind) -> bool {
    matches!(
        kind,
        PenguinSurfaceKind::IceTrailSegment | PenguinSurfaceKind::UltimateIceTile
    )
}

fn snowflake_magic_destination(
    target_position: Vec3,
    snowfield_centers: impl IntoIterator<Item = Vec3>,
) -> Option<Vec3> {
    snowfield_centers
        .into_iter()
        .max_by(|a, b| {
            flat_distance(target_position, *a).total_cmp(&flat_distance(target_position, *b))
        })
        .map(|position| grounded_position(position, 0.0))
}

#[cfg(test)]
fn snowflake_magic_destination_from_surfaces(
    target_position: Vec3,
    surfaces: impl IntoIterator<Item = (PenguinSurfaceKind, f32, Vec3)>,
) -> Option<Vec3> {
    snowflake_magic_destination(
        target_position,
        surfaces
            .into_iter()
            .filter_map(|(kind, lifetime, position)| {
                active_snowfield_center(kind, lifetime, position)
            }),
    )
}

pub fn penguin_ice_modifier(
    position: Vec3,
    character_kind: CharacterKind,
    surfaces: &Query<(&ActivePenguinSurface, &Transform), Without<Fighter>>,
) -> Option<PenguinIceModifier> {
    let mut on_soft_ice = false;
    let mut on_hard_ice = false;
    for (surface, transform) in surfaces.iter() {
        if !surface_overlaps_position(surface, transform.translation, position, FIGHTER_RADIUS) {
            continue;
        }
        match surface.kind {
            PenguinSurfaceKind::IceTrailSegment => on_soft_ice = true,
            PenguinSurfaceKind::UltimateIceTile => on_hard_ice = true,
            _ => {}
        }
    }
    penguin_ice_modifier_from_overlaps(on_soft_ice, on_hard_ice, character_kind)
}

fn penguin_ice_modifier_from_overlaps(
    on_soft_ice: bool,
    on_hard_ice: bool,
    character_kind: CharacterKind,
) -> Option<PenguinIceModifier> {
    if !on_soft_ice && !on_hard_ice {
        return None;
    }
    let penguin = character_kind == CharacterKind::Penguin;
    if on_hard_ice {
        return Some(PenguinIceModifier {
            ground_friction_scale: 0.04,
            stop_friction_scale: 0.04,
            turn_brake_scale: 0.0,
            input_scale: 0.0,
            dash_slide_timer: 0.28,
            hard_slide: true,
        });
    }
    Some(PenguinIceModifier {
        ground_friction_scale: if penguin { 0.34 } else { 0.18 },
        stop_friction_scale: if penguin { 0.36 } else { 0.14 },
        turn_brake_scale: if penguin { 0.62 } else { 0.28 },
        input_scale: if penguin { 0.98 } else { 0.82 },
        dash_slide_timer: if penguin { 0.12 } else { 0.22 },
        hard_slide: false,
    })
}

fn penguin_skill_persists_after_hit(kind: PenguinSkillKind) -> bool {
    matches!(
        kind,
        PenguinSkillKind::SledWake
            | PenguinSkillKind::SnowmanDrop
            | PenguinSkillKind::BodySlamShockwave
    )
}

fn surface_can_touch_fighter(
    surface: &ActivePenguinSurface,
    fighter_entity: Entity,
    fighter: &Fighter,
    state: &MatchState,
) -> bool {
    fighter_entity == surface.owner
        || state.combat_target_allowed_for_state(surface.owner_id, fighter.id)
}

fn surface_overlaps_fighter(
    surface: &ActivePenguinSurface,
    surface_position: Vec3,
    fighter_position: Vec3,
) -> bool {
    surface_overlaps_position(surface, surface_position, fighter_position, FIGHTER_RADIUS)
}

fn surface_overlaps_position(
    surface: &ActivePenguinSurface,
    surface_position: Vec3,
    position: Vec3,
    radius: f32,
) -> bool {
    flat_distance(surface_position, position) <= surface.radius + radius
}

fn snow_slope_ride_contact(
    surface_position: Vec3,
    facing: Vec3,
    fighter_position: Vec3,
    size_scale: f32,
) -> Option<SnowSlopeRideContact> {
    let size_scale = penguin_skill_size_scale(size_scale);
    let forward = normalized_or_forward(facing);
    let right = Vec3::new(forward.z, 0.0, -forward.x).normalize_or_zero();
    let offset = fighter_position - surface_position;
    let along = offset.dot(forward);
    let side = offset.dot(right);
    let half_length = PENGUIN_SNOW_SLOPE_RIDE_HALF_LENGTH * size_scale;
    let half_width = PENGUIN_SNOW_SLOPE_RIDE_HALF_WIDTH * size_scale;
    if side.abs() > half_width + FIGHTER_RADIUS
        || along < -half_length - FIGHTER_RADIUS
        || along > half_length + FIGHTER_RADIUS
    {
        return None;
    }

    let progress = ((along + half_length) / (half_length * 2.0)).clamp(0.0, 1.0);
    let ground = ground_height_at(fighter_position.x, fighter_position.z).unwrap_or(ARENA_TOP_Y);
    Some(SnowSlopeRideContact {
        progress,
        target_y: ground
            + (PENGUIN_SNOW_SLOPE_RIDE_BASE_HEIGHT + PENGUIN_SNOW_SLOPE_RIDE_HEIGHT * progress)
                * size_scale,
    })
}

fn snow_hill_ramp_touch(
    surface: &ActivePenguinSurface,
    fighter_entity: Entity,
) -> SnowHillRampTouch {
    if fighter_entity == surface.owner {
        SnowHillRampTouch {
            forward_push: PENGUIN_SNOW_HILL_RIDE_PUSH,
            lift: PENGUIN_SNOW_HILL_RIDE_LIFT,
            slide_timer: PENGUIN_SNOW_HILL_RIDE_SLIDE,
            speed_limit_timer: PENGUIN_SNOW_HILL_RIDE_SPEED_LIMIT,
            owner_ride: true,
        }
    } else {
        SnowHillRampTouch {
            forward_push: PENGUIN_SNOW_HILL_PUSH,
            lift: PENGUIN_SNOW_HILL_LAUNCH,
            slide_timer: 0.0,
            speed_limit_timer: 0.0,
            owner_ride: false,
        }
    }
}

fn ramp_push_direction(facing: Vec3, ramp_position: Vec3, fighter_position: Vec3) -> Vec3 {
    let away = Vec3::new(
        fighter_position.x - ramp_position.x,
        0.0,
        fighter_position.z - ramp_position.z,
    )
    .normalize_or_zero();
    let facing = normalized_or_forward(facing);
    (facing * 0.76 + away * 0.24).normalize_or_zero()
}

fn ice_trail_visual_scale(size_scale: f32, lifetime: f32) -> Vec3 {
    let fade = (lifetime / 0.55).clamp(0.28, 1.0);
    Vec3::new(0.95 * fade, 0.16, 1.2 * fade) * size_scale
}

fn ultimate_snow_field_visual_scale(size_scale: f32, lifetime: f32) -> Vec3 {
    let fade = (lifetime / 0.85).clamp(0.22, 1.0);
    Vec3::new(
        PENGUIN_ULTIMATE_SNOW_FLAT_LARGE_SCALE * fade * size_scale,
        size_scale,
        PENGUIN_ULTIMATE_SNOW_FLAT_LARGE_SCALE * fade * size_scale,
    )
}

fn oldest_ice_segments_to_despawn(
    segments: &[(Entity, Entity, f32)],
    cap_per_owner: usize,
) -> Vec<Entity> {
    let mut by_owner: HashMap<Entity, Vec<(Entity, f32)>> = HashMap::new();
    for (entity, owner, age) in segments {
        by_owner.entry(*owner).or_default().push((*entity, *age));
    }

    let mut despawn = Vec::new();
    for owner_segments in by_owner.values_mut() {
        if owner_segments.len() <= cap_per_owner {
            continue;
        }
        owner_segments.sort_by(|(_, age_a), (_, age_b)| age_b.total_cmp(age_a));
        despawn.extend(
            owner_segments
                .iter()
                .take(owner_segments.len() - cap_per_owner)
                .map(|(entity, _)| *entity),
        );
    }
    despawn
}

fn planar_speed(velocity: Vec3) -> f32 {
    Vec2::new(velocity.x, velocity.z).length()
}

fn update_skill_repeat_window(skill: &mut ActivePenguinSkill, effects: &mut HitEffects) {
    let Some(interval) = skill.repeat_interval else {
        return;
    };
    let Some(mut next_repeat) = skill.next_repeat else {
        return;
    };
    while skill.age >= next_repeat {
        skill.already_hit.clear();
        effects.push_feedback_cue("pulse_penguin_sled_wake", skill.source, 24);
        next_repeat += interval;
    }
    skill.next_repeat = Some(next_repeat);
}

fn update_penguin_skill_motion(
    skill: &mut ActivePenguinSkill,
    transform: &mut Transform,
    dt: f32,
    targets: &Query<(&Fighter, &Transform), With<Fighter>>,
) {
    match skill.kind {
        PenguinSkillKind::FishTorpedo => {
            if let Some(target_entity) = skill.target
                && let Ok((_, target_transform)) = targets.get(target_entity)
            {
                steer_fish_torpedo_toward(
                    skill,
                    transform.translation,
                    target_transform.translation + Vec3::Y * 0.5,
                    dt,
                );
            }
            transform.translation += skill.velocity * dt;
            transform.translation =
                grounded_position(transform.translation, 0.26 * skill.size_scale);
            transform.rotation = projectile_rotation(skill.facing);
            transform.rotate_y(0.18);
        }
        PenguinSkillKind::PopsicleBounce => {
            skill.velocity.y -= PENGUIN_POPSICLE_GRAVITY * dt;
            transform.translation += skill.velocity * dt;
            transform.rotate_y(0.16);
            transform.rotate_x(0.12);
        }
        PenguinSkillKind::SledWake => {
            transform.translation += skill.velocity * dt;
            transform.translation = grounded_position(transform.translation, 0.05);
            transform.scale =
                penguin_skill_visual_scale(PenguinSkillKind::SledWake, skill.size_scale, skill.age);
        }
        PenguinSkillKind::SnowflakeShard => {
            transform.translation += skill.velocity * dt;
            transform.rotation = projectile_rotation(skill.facing);
            transform.rotate_z(skill.age * 12.0);
        }
        PenguinSkillKind::SnowBoulder => {
            transform.translation += skill.velocity * dt;
            transform.translation =
                grounded_position(transform.translation, 0.34 * skill.size_scale);
            transform.rotation = projectile_rotation(skill.facing);
            transform.rotate_x(skill.age * -12.0);
        }
        PenguinSkillKind::SnowmanDrop => {
            skill.velocity.y -= PENGUIN_SNOWMAN_DROP_GRAVITY * dt;
            transform.translation += skill.velocity * dt;
            transform.rotation = projectile_rotation(skill.facing);
            transform.rotate_x(skill.age * -3.4);
        }
        PenguinSkillKind::BodySlamShockwave => {
            transform.translation = grounded_position(transform.translation, 0.05);
            transform.scale = penguin_skill_visual_scale(
                PenguinSkillKind::BodySlamShockwave,
                skill.size_scale,
                skill.age,
            );
        }
    }
}

fn steer_fish_torpedo_toward(
    skill: &mut ActivePenguinSkill,
    current_position: Vec3,
    target_position: Vec3,
    dt: f32,
) {
    let desired = (target_position - current_position).normalize_or_zero();
    if desired.length_squared() <= 0.01 {
        return;
    }
    let speed = skill.velocity.length();
    skill.velocity = skill.velocity.lerp(
        desired * speed,
        (dt * PENGUIN_FISH_TORPEDO_TURN_RATE).clamp(0.0, 1.0),
    );
    skill.facing = normalized_or_forward(skill.velocity);
}

fn penguin_skill_impact_profile(
    skill: &ActivePenguinSkill,
    feel: &CombatFeelTuning,
) -> crate::combat::ImpactProfile {
    let mut profile = impact_profile_from_payload_with_feel(
        skill.owner_id,
        skill.source,
        skill.payload_id,
        1.0,
        1.0,
        1.0,
        skill.guard_stamina_damage,
        feel,
    );
    profile.shape_id = Some(skill.shape_id);
    profile.attacker_style = Some(skill.owner_style);
    profile
}

fn penguin_skill_overlaps_target(
    skill: &ActivePenguinSkill,
    origin: Vec3,
    target_transform: &Transform,
) -> bool {
    if skill.kind == PenguinSkillKind::SledWake {
        return flat_distance(origin, target_transform.translation)
            <= skill.radius + FIGHTER_RADIUS;
    }
    let target = target_transform.translation + Vec3::Y * (FIGHTER_HEIGHT * 0.58);
    target.distance(origin) <= skill.radius + FIGHTER_RADIUS
}

pub fn penguin_skill_lock_target(
    owner_id: usize,
    origin: Vec3,
    facing: Vec3,
    aim_held: bool,
    state: &MatchState,
    targets: &[BeeSkillTargetSnapshot],
) -> Option<Entity> {
    if !aim_held {
        return None;
    }
    let facing = normalized_or_forward(facing);

    targets
        .iter()
        .filter(|target| state.combat_target_allowed_for_state(owner_id, target.fighter_id))
        .filter_map(|target| {
            let offset = Vec3::new(
                target.position.x - origin.x,
                0.0,
                target.position.z - origin.z,
            );
            let distance = offset.length();
            if distance > PENGUIN_SKILL_LOCK_RANGE || distance <= 0.01 {
                return None;
            }
            let direction = offset / distance;
            (direction.dot(facing) >= PENGUIN_SKILL_LOCK_CONE_DOT)
                .then_some((target.entity, distance))
        })
        .min_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(entity, _)| entity)
}

fn target_position(entity: Entity, targets: &[BeeSkillTargetSnapshot]) -> Option<Vec3> {
    targets
        .iter()
        .find(|target| target.entity == entity)
        .map(|target| target.position)
}

fn snowflake_shot_spawn(origin: Vec3, facing: Vec3, size_scale: f32) -> (Vec3, Vec3) {
    let direction = normalized_or_forward(facing);
    let size_scale = penguin_skill_size_scale(size_scale);
    (
        origin
            + direction * PENGUIN_SNOWFLAKE_SHOT_FORWARD * size_scale
            + Vec3::Y * PENGUIN_SNOWFLAKE_SHOT_HEIGHT * size_scale,
        direction,
    )
}

pub fn penguin_snowflake_swap_target(
    owner_id: usize,
    active_skills: impl IntoIterator<Item = (Entity, PenguinSkillKind, usize, f32, Vec3)>,
) -> Option<PenguinSnowflakeSwap> {
    active_skills
        .into_iter()
        .find(|(_, kind, skill_owner_id, lifetime, _)| {
            *kind == PenguinSkillKind::SnowflakeShard
                && *skill_owner_id == owner_id
                && *lifetime > 0.0
        })
        .map(
            |(snowflake, _, _, _, penguin_destination)| PenguinSnowflakeSwap {
                snowflake,
                penguin_destination,
            },
        )
}

fn flat_direction(origin: Vec3, target: Vec3) -> Vec3 {
    Vec3::new(target.x - origin.x, 0.0, target.z - origin.z).normalize_or_zero()
}

fn flat_distance(a: Vec3, b: Vec3) -> f32 {
    Vec2::new(a.x - b.x, a.z - b.z).length()
}

fn grounded_position(position: Vec3, clearance: f32) -> Vec3 {
    let ground = ground_height_at(position.x, position.z).unwrap_or(ARENA_TOP_Y);
    Vec3::new(position.x, ground + clearance, position.z)
}

fn popsicle_touched_ground(skill: &ActivePenguinSkill, position: Vec3) -> bool {
    if skill.kind != PenguinSkillKind::PopsicleBounce {
        return false;
    }
    let ground = ground_height_at(position.x, position.z).unwrap_or(ARENA_TOP_Y);
    position.y <= ground + 0.08 && skill.age > 0.08
}

fn snowman_touched_ground(skill: &ActivePenguinSkill, position: Vec3) -> bool {
    if skill.kind != PenguinSkillKind::SnowmanDrop {
        return false;
    }
    let ground = ground_height_at(position.x, position.z).unwrap_or(ARENA_TOP_Y);
    position.y <= ground + PENGUIN_SNOWMAN_DROP_LAND_CLEARANCE * skill.size_scale
        && skill.age > 0.08
}

fn snowman_landing_snow_offsets(facing: Vec3, size_scale: f32) -> [Vec3; 2] {
    let facing = normalized_or_forward(facing);
    [
        Vec3::ZERO,
        facing * PENGUIN_ULTIMATE_ICE_FIELD_TILE_SPACING * penguin_skill_size_scale(size_scale),
    ]
}

fn spawn_snowman_landing_snow(
    commands: &mut Commands,
    assets: &PenguinSkillAssets,
    owner: Entity,
    owner_id: usize,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    for offset in snowman_landing_snow_offsets(facing, size_scale)
        .into_iter()
        .take(PENGUIN_SNOWMAN_DROP_SNOW_TILE_COUNT)
    {
        spawn_ultimate_ice_tile(
            commands,
            assets,
            owner,
            owner_id,
            grounded_position(position + offset, 0.014),
            facing,
            size_scale,
        );
    }
}

fn should_despawn_skill(position: Vec3) -> bool {
    let arena = active_arena_definition();
    position.y < arena.ringout_y
        || Vec2::new(position.x, position.z).length() > arena.ringout_radius
}

fn impact_package(kind: PenguinSkillKind) -> FeedbackPackageId {
    match kind {
        PenguinSkillKind::SledWake
        | PenguinSkillKind::SnowmanDrop
        | PenguinSkillKind::BodySlamShockwave => FeedbackPackageId::SpecialHazardImpact,
        _ => FeedbackPackageId::SpecialProjectileImpact,
    }
}

fn despawn_package(kind: PenguinSkillKind) -> FeedbackPackageId {
    match kind {
        PenguinSkillKind::SledWake
        | PenguinSkillKind::SnowmanDrop
        | PenguinSkillKind::BodySlamShockwave => FeedbackPackageId::SpecialHazardFade,
        _ => FeedbackPackageId::SpecialProjectileRecover,
    }
}

fn snowflake_burst_directions(facing: Vec3) -> [Vec3; 8] {
    let forward = normalized_or_forward(facing);
    let side = Vec3::new(-forward.z, 0.0, forward.x).normalize_or_zero();
    [
        forward,
        (forward + side).normalize_or_zero(),
        side,
        (-forward + side).normalize_or_zero(),
        -forward,
        (-forward - side).normalize_or_zero(),
        -side,
        (forward - side).normalize_or_zero(),
    ]
}

fn normalized_or_forward(value: Vec3) -> Vec3 {
    let normalized = value.normalize_or_zero();
    if normalized.length_squared() > 0.01 {
        normalized
    } else {
        Vec3::Z
    }
}

fn projectile_rotation(facing: Vec3) -> Quat {
    Quat::from_rotation_arc(Vec3::Z, normalized_or_forward(facing))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(index: u32) -> Entity {
        Entity::from_raw_u32(index).expect("test entity index should be valid")
    }

    fn assert_vec3_close(actual: Vec3, expected: Vec3) {
        assert!(
            (actual - expected).length() <= 0.0001,
            "expected {actual:?} to be close to {expected:?}"
        );
    }

    #[test]
    fn aim_held_lock_target_selects_valid_enemy_in_front() {
        let mut state = MatchState::default();
        state.active_slots = [true, true, true, false];
        state.active_fighter_count = 3;
        let targets = [
            BeeSkillTargetSnapshot {
                entity: entity(1),
                fighter_id: 1,
                position: Vec3::new(4.0, 0.0, 0.0),
            },
            BeeSkillTargetSnapshot {
                entity: entity(2),
                fighter_id: 2,
                position: Vec3::new(-1.0, 0.0, 0.0),
            },
        ];

        assert_eq!(
            penguin_skill_lock_target(0, Vec3::ZERO, Vec3::X, true, &state, &targets),
            Some(entity(1))
        );
        assert_eq!(
            penguin_skill_lock_target(0, Vec3::ZERO, Vec3::X, false, &state, &targets),
            None
        );
    }

    #[test]
    fn lock_target_ignores_inactive_and_friendly_slots() {
        let mut state = MatchState::default();
        state.active_slots = [true, true, true, false];
        state.active_fighter_count = 3;
        let targets = [
            BeeSkillTargetSnapshot {
                entity: entity(1),
                fighter_id: 2,
                position: Vec3::new(2.0, 0.0, 0.0),
            },
            BeeSkillTargetSnapshot {
                entity: entity(2),
                fighter_id: 1,
                position: Vec3::new(3.0, 0.0, 0.0),
            },
        ];

        assert_eq!(
            penguin_skill_lock_target(0, Vec3::ZERO, Vec3::X, true, &state, &targets),
            Some(entity(2))
        );
    }

    #[test]
    fn fish_torpedo_velocity_turns_toward_captured_target() {
        let mut skill = active_penguin_skill(
            PenguinSkillKind::FishTorpedo,
            entity(1),
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::X * PENGUIN_FISH_TORPEDO_SPEED,
            Some(entity(2)),
            1.0,
        );

        steer_fish_torpedo_toward(&mut skill, Vec3::ZERO, Vec3::new(0.0, 0.0, 3.0), 0.08);

        assert!(skill.velocity.z > 0.0);
        assert!(skill.velocity.x < PENGUIN_FISH_TORPEDO_SPEED);
    }

    #[test]
    fn sled_wake_repeat_window_clears_contact_memory() {
        let mut skill = active_penguin_skill(
            PenguinSkillKind::SledWake,
            entity(1),
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::X,
            None,
            1.0,
        );
        skill.already_hit.push(entity(2));
        skill.age = PENGUIN_SLED_WAKE_TICK;
        let mut effects = HitEffects::default();

        update_skill_repeat_window(&mut skill, &mut effects);

        assert!(skill.already_hit.is_empty());
        assert!(skill.next_repeat.unwrap() > PENGUIN_SLED_WAKE_TICK);
    }

    #[test]
    fn mushroom_size_scale_enlarges_penguin_skill_collision_radii() {
        let size_scale = crate::constants::ITEM_GIANT_SIZE_MULTIPLIER;
        let cases = [
            (PenguinSkillKind::FishTorpedo, PENGUIN_FISH_TORPEDO_RADIUS),
            (PenguinSkillKind::PopsicleBounce, PENGUIN_POPSICLE_RADIUS),
            (PenguinSkillKind::SledWake, PENGUIN_SLED_WAKE_RADIUS),
            (PenguinSkillKind::SnowflakeShard, PENGUIN_SNOWFLAKE_RADIUS),
            (PenguinSkillKind::SnowBoulder, PENGUIN_BOULDER_RADIUS),
            (PenguinSkillKind::SnowmanDrop, PENGUIN_SNOWMAN_DROP_RADIUS),
            (
                PenguinSkillKind::BodySlamShockwave,
                PENGUIN_BODY_SLAM_RADIUS,
            ),
        ];

        for (kind, base_radius) in cases {
            let skill = active_penguin_skill(
                kind,
                entity(1),
                0,
                FighterStyleKind::Anchor,
                Vec3::X,
                Vec3::X,
                None,
                size_scale,
            );

            assert_eq!(skill.size_scale, size_scale);
            assert_eq!(skill.radius, base_radius * size_scale);
        }
    }

    #[test]
    fn mushroom_size_scale_enlarges_penguin_skill_visuals() {
        let size_scale = crate::constants::ITEM_GIANT_SIZE_MULTIPLIER;

        assert_vec3_close(
            penguin_skill_visual_scale(PenguinSkillKind::FishTorpedo, size_scale, 0.0),
            Vec3::splat(1.75 * size_scale),
        );
        assert_vec3_close(
            penguin_skill_visual_scale(PenguinSkillKind::PopsicleBounce, size_scale, 0.0),
            Vec3::splat(1.55 * size_scale),
        );
        assert_vec3_close(
            penguin_skill_visual_scale(PenguinSkillKind::SnowflakeShard, size_scale, 0.0),
            Vec3::splat(1.35 * size_scale),
        );
        assert_vec3_close(
            penguin_skill_visual_scale(PenguinSkillKind::SledWake, size_scale, 0.25),
            Vec3::splat(sled_wake_visual_pulse(0.25) * size_scale),
        );
        assert_vec3_close(
            penguin_skill_visual_scale(PenguinSkillKind::SnowBoulder, size_scale, 0.0),
            Vec3::splat(0.9 * size_scale),
        );
        assert_vec3_close(
            penguin_skill_visual_scale(PenguinSkillKind::SnowmanDrop, size_scale, 0.0),
            Vec3::splat(0.82 * PENGUIN_SNOWMAN_DROP_SIZE_MULTIPLIER * size_scale),
        );
        assert!(
            penguin_skill_visual_scale(PenguinSkillKind::BodySlamShockwave, size_scale, 0.2).x
                > size_scale
        );
    }

    #[test]
    fn snowman_drop_payload_persists_and_freezes_until_landing() {
        let skill = active_penguin_skill(
            PenguinSkillKind::SnowmanDrop,
            entity(1),
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::Y * -PENGUIN_SNOWMAN_DROP_INITIAL_FALL_SPEED,
            None,
            1.0,
        );

        assert_eq!(skill.payload_id, AttackPayloadId::PenguinSnowmanDrop);
        assert_eq!(skill.shape_id, AttackShapeId::ProjectileBolt);
        assert_eq!(skill.radius, PENGUIN_SNOWMAN_DROP_RADIUS);
        assert_eq!(skill.lifetime, PENGUIN_SNOWMAN_DROP_LIFETIME);
        assert!(penguin_skill_persists_after_hit(skill.kind));
    }

    #[test]
    fn snowman_landing_snow_offsets_make_two_forward_tiles() {
        let offsets = snowman_landing_snow_offsets(Vec3::Z, 1.0);

        assert_eq!(offsets.len(), PENGUIN_SNOWMAN_DROP_SNOW_TILE_COUNT);
        assert_vec3_close(offsets[0], Vec3::ZERO);
        assert_vec3_close(
            offsets[1],
            Vec3::Z * PENGUIN_ULTIMATE_ICE_FIELD_TILE_SPACING,
        );
    }

    #[test]
    fn snowflake_burst_uses_radial_directions() {
        let directions = snowflake_burst_directions(Vec3::X);

        assert_eq!(directions.len(), 8);
        assert!(directions.iter().all(|direction| {
            (direction.length() - 1.0).abs() <= 0.0001 && direction.y.abs() <= 0.0001
        }));
        assert!(directions.contains(&Vec3::X));
        assert!(directions.contains(&Vec3::NEG_X));
    }

    #[test]
    fn snowflake_shot_spawn_uses_one_forward_projectile() {
        let (spawn, direction) = snowflake_shot_spawn(Vec3::new(1.0, 0.5, -2.0), Vec3::Z, 1.0);

        assert_vec3_close(direction, Vec3::Z);
        assert_vec3_close(
            spawn,
            Vec3::new(1.0, 0.5 + PENGUIN_SNOWFLAKE_SHOT_HEIGHT, -2.0)
                + Vec3::Z * PENGUIN_SNOWFLAKE_SHOT_FORWARD,
        );
    }

    #[test]
    fn snowflake_swap_uses_live_projectile_position_as_penguin_destination() {
        let snowflake = entity(3);
        let position = Vec3::new(1.0, 0.5, -2.0);
        let swap = penguin_snowflake_swap_target(
            0,
            [(
                snowflake,
                PenguinSkillKind::SnowflakeShard,
                0,
                0.4,
                position,
            )],
        )
        .unwrap();

        assert_eq!(swap.snowflake, snowflake);
        assert_vec3_close(swap.penguin_destination, position);
    }

    #[test]
    fn snowflake_swap_requires_owners_active_snowflake() {
        let position = Vec3::new(1.0, 0.5, -2.0);

        assert!(
            penguin_snowflake_swap_target(
                0,
                std::iter::empty::<(Entity, PenguinSkillKind, usize, f32, Vec3)>(),
            )
            .is_none()
        );
        assert!(
            penguin_snowflake_swap_target(
                0,
                [(entity(3), PenguinSkillKind::FishTorpedo, 0, 0.4, position)]
            )
            .is_none()
        );
        assert!(
            penguin_snowflake_swap_target(
                0,
                [(
                    entity(3),
                    PenguinSkillKind::SnowflakeShard,
                    1,
                    0.4,
                    position
                )]
            )
            .is_none()
        );
        assert!(
            penguin_snowflake_swap_target(
                0,
                [(
                    entity(3),
                    PenguinSkillKind::SnowflakeShard,
                    0,
                    0.0,
                    position
                )]
            )
            .is_none()
        );
    }

    #[test]
    fn snowflake_shot_lasts_longer_and_is_single_cast_per_owner() {
        let owner = entity(1);
        let skill = active_penguin_skill(
            PenguinSkillKind::SnowflakeShard,
            owner,
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::X,
            None,
            1.0,
        );

        assert_eq!(skill.lifetime, 1.08);
        assert!(penguin_snowflake_shot_is_active(
            0,
            [(PenguinSkillKind::SnowflakeShard, 0, skill.lifetime)]
        ));
        assert!(!penguin_snowflake_shot_is_active(
            0,
            [(PenguinSkillKind::SnowflakeShard, 0, 0.0)]
        ));
        assert!(!penguin_snowflake_shot_is_active(
            0,
            [(PenguinSkillKind::SnowflakeShard, 1, skill.lifetime)]
        ));
        assert!(!penguin_snowflake_shot_is_active(
            0,
            [(PenguinSkillKind::FishTorpedo, 0, skill.lifetime)]
        ));
    }

    #[test]
    fn snowflake_magic_destination_chooses_farthest_flat_snowfield() {
        let target = Vec3::new(0.0, ARENA_TOP_Y, 0.0);
        let destination = snowflake_magic_destination_from_surfaces(
            target,
            [
                (
                    PenguinSurfaceKind::IceTrailSegment,
                    PENGUIN_ICE_TRAIL_LIFETIME,
                    Vec3::new(1.0, ARENA_TOP_Y + 0.1, 0.0),
                ),
                (
                    PenguinSurfaceKind::UltimateIceTile,
                    PENGUIN_ULTIMATE_ICE_FIELD_LIFETIME,
                    Vec3::new(-5.0, ARENA_TOP_Y + 0.2, 0.0),
                ),
            ],
        )
        .expect("active snowfield should be selected");

        assert_vec3_close(destination, Vec3::new(-5.0, ARENA_TOP_Y, 0.0));
    }

    #[test]
    fn snowflake_magic_destination_ignores_non_flat_or_expired_snow() {
        let target = Vec3::new(0.0, ARENA_TOP_Y, 0.0);
        let destination = snowflake_magic_destination_from_surfaces(
            target,
            [
                (
                    PenguinSurfaceKind::SnowHillRamp,
                    PENGUIN_SNOW_HILL_LIFETIME,
                    Vec3::new(8.0, ARENA_TOP_Y, 0.0),
                ),
                (
                    PenguinSurfaceKind::SnowSlopeRide,
                    PENGUIN_SNOW_SLOPE_RIDE_LIFETIME,
                    Vec3::new(-8.0, ARENA_TOP_Y, 0.0),
                ),
                (
                    PenguinSurfaceKind::IceTrailSegment,
                    0.0,
                    Vec3::new(12.0, ARENA_TOP_Y, 0.0),
                ),
            ],
        );

        assert!(destination.is_none());
    }

    #[test]
    fn penguin_skill_assets_exist_for_runtime_loading() {
        for path in [
            "assets/food/kenney_food_kit/fish-bones.glb",
            "assets/food/kenney_food_kit/popsicle.glb",
            "assets/holiday/kenney_holiday_kit/snow-pile.glb",
            "assets/holiday/kenney_holiday_kit/snowflake-a.glb",
            "assets/holiday/kenney_holiday_kit/snowman.glb",
            "assets/holiday/kenney_holiday_kit/snow-flat-large.glb",
            "assets/holiday/kenney_holiday_kit/snow-flat.glb",
            "assets/holiday/kenney_holiday_kit/Textures/colormap.png",
            "assets/tower_defense/kenney_tower_defense_kit/snow-tile-straight.glb",
            "assets/tower_defense/kenney_tower_defense_kit/snow-tile-bump.glb",
            "assets/tower_defense/kenney_tower_defense_kit/snow-tile-hill.glb",
            "assets/tower_defense/kenney_tower_defense_kit/snow-wood-structure.glb",
            "assets/tower_defense/kenney_tower_defense_kit/weapon-cannon.glb",
            "assets/tower_defense/kenney_tower_defense_kit/weapon-ammo-boulder.glb",
            "assets/tower_defense/kenney_tower_defense_kit/Textures/colormap.png",
            "assets/platformer/kenney_platformer_kit/block-snow-large-slope.glb",
            "assets/platformer/kenney_platformer_kit/block-snow-large-slope-steep.glb",
            "assets/platformer/kenney_platformer_kit/spring.glb",
            "assets/platformer/kenney_platformer_kit/Textures/colormap.png",
        ] {
            assert!(std::path::Path::new(path).exists(), "{path} should exist");
        }
    }

    #[test]
    fn ice_trail_cap_despawns_oldest_segments_per_owner() {
        let owner = entity(1);
        let other = entity(2);
        let segments = [
            (entity(10), owner, 4.0),
            (entity(11), owner, 1.0),
            (entity(12), owner, 3.0),
            (entity(13), other, 2.0),
        ];

        let despawn = oldest_ice_segments_to_despawn(&segments, 2);

        assert_eq!(despawn, vec![entity(10)]);
    }

    #[test]
    fn ultimate_ice_modifier_hard_slide_overrides_soft_ice() {
        let soft = penguin_ice_modifier_from_overlaps(true, false, CharacterKind::Cat).unwrap();
        let hard = penguin_ice_modifier_from_overlaps(true, true, CharacterKind::Cat).unwrap();

        assert!(!soft.hard_slide);
        assert!(hard.hard_slide);
        assert_eq!(hard.input_scale, 0.0);
        assert!(hard.ground_friction_scale < soft.ground_friction_scale);
    }

    #[test]
    fn ultimate_ice_field_constants_make_four_by_four_ten_second_patch() {
        let tile_count = PENGUIN_ULTIMATE_ICE_FIELD_GRID_SIDE.pow(2) as usize;

        assert_eq!(tile_count, 16);
        assert_eq!(PENGUIN_ULTIMATE_ICE_FIELD_LIFETIME, 10.0);
        assert!(PENGUIN_ULTIMATE_ICE_FIELD_TILE_SPACING > PENGUIN_ULTIMATE_ICE_FIELD_TILE_RADIUS);
        assert!(PENGUIN_ULTIMATE_SNOW_FIELD_CLEARANCE < 0.03);
        assert!(PENGUIN_ULTIMATE_SNOW_FLAT_LARGE_SCALE > PENGUIN_ULTIMATE_SNOW_FLAT_DETAIL_SCALE);
    }

    #[test]
    fn ultimate_ice_field_even_grid_offsets_stay_centered() {
        let first = ultimate_ice_field_grid_axis_offset(0);
        let second = ultimate_ice_field_grid_axis_offset(1);
        let third = ultimate_ice_field_grid_axis_offset(2);
        let last = ultimate_ice_field_grid_axis_offset(3);

        assert!((first + 1.5).abs() < 0.001);
        assert!((last - 1.5).abs() < 0.001);
        assert!((second + third).abs() < 0.001);
    }

    #[test]
    fn ultimate_snow_field_scale_fades_only_across_the_floor() {
        let full = ultimate_snow_field_visual_scale(1.4, PENGUIN_ULTIMATE_ICE_FIELD_LIFETIME);
        let fading = ultimate_snow_field_visual_scale(1.4, 0.1);

        assert_eq!(full.y, 1.4);
        assert_eq!(fading.y, 1.4);
        assert!(full.x > fading.x);
        assert!(full.z > fading.z);
    }

    #[test]
    fn snow_hill_ramp_owner_touch_is_ski_ride() {
        let owner = entity(1);
        let surface = active_penguin_surface(
            PenguinSurfaceKind::SnowHillRamp,
            owner,
            0,
            Vec3::X,
            PENGUIN_SNOW_HILL_RADIUS,
            PENGUIN_SNOW_HILL_LIFETIME,
            1.0,
        );

        let owner_touch = snow_hill_ramp_touch(&surface, owner);
        let opponent_touch = snow_hill_ramp_touch(&surface, entity(2));

        assert!(owner_touch.owner_ride);
        assert!(owner_touch.forward_push > opponent_touch.forward_push);
        assert!(owner_touch.lift < opponent_touch.lift);
        assert!(owner_touch.slide_timer > 0.0);
        assert!(owner_touch.speed_limit_timer > 0.0);
        assert!(!opponent_touch.owner_ride);
        assert_eq!(opponent_touch.slide_timer, 0.0);
    }

    #[test]
    fn snow_slope_ride_low_side_faces_penguin() {
        let facing = Vec3::X;
        let rotation = snow_slope_ride_rotation(facing);

        assert_vec3_close(rotation * Vec3::Z, -facing);
    }

    #[test]
    fn snow_slope_ride_contact_climbs_from_low_to_high_end() {
        let center = Vec3::ZERO;
        let facing = Vec3::X;
        let low = center - facing * PENGUIN_SNOW_SLOPE_RIDE_HALF_LENGTH;
        let high = center + facing * PENGUIN_SNOW_SLOPE_RIDE_HALF_LENGTH;

        let low_contact = snow_slope_ride_contact(center, facing, low, 1.0).unwrap();
        let high_contact = snow_slope_ride_contact(center, facing, high, 1.0).unwrap();

        assert!(low_contact.progress <= 0.001);
        assert!(high_contact.progress >= 0.999);
        assert!(high_contact.target_y > low_contact.target_y);
    }

    #[test]
    fn snow_slope_ride_contact_rejects_side_entries() {
        let center = Vec3::ZERO;
        let facing = Vec3::X;
        let side_entry =
            center + Vec3::Z * (PENGUIN_SNOW_SLOPE_RIDE_HALF_WIDTH + FIGHTER_RADIUS + 0.05);

        assert!(snow_slope_ride_contact(center, facing, side_entry, 1.0).is_none());
    }

    #[test]
    fn snow_slope_ride_launch_keeps_short_arc_and_landing_slide() {
        assert!(PENGUIN_SNOW_SLOPE_RIDE_LIFT < 1.6);
        assert!(PENGUIN_SNOW_SLOPE_RIDE_PUSH < 5.0);
        assert!(PENGUIN_SNOW_SLOPE_RIDE_SLIDE > PENGUIN_SNOW_SLOPE_RIDE_SPEED_LIMIT);
        assert!(PENGUIN_SNOW_SLOPE_RIDE_EXIT_PROGRESS > 0.9);
    }

    #[test]
    fn active_surface_records_lifetime_radius_and_owner() {
        let surface = active_penguin_surface(
            PenguinSurfaceKind::IceTrailSegment,
            entity(1),
            0,
            Vec3::X,
            PENGUIN_ICE_TRAIL_RADIUS,
            PENGUIN_ICE_TRAIL_LIFETIME,
            1.25,
        );

        assert_eq!(surface.kind, PenguinSurfaceKind::IceTrailSegment);
        assert_eq!(surface.owner, entity(1));
        assert_eq!(surface.lifetime, PENGUIN_ICE_TRAIL_LIFETIME);
        assert_eq!(surface.radius, PENGUIN_ICE_TRAIL_RADIUS);
        assert_eq!(surface.size_scale, 1.25);
    }
}
