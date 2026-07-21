use bevy::gltf::GltfAssetLabel;
use bevy::prelude::*;
use std::f32::consts::TAU;

use crate::arena::ground_height_at;
use crate::arena_defs::active_arena_definition;
use crate::bee_skills::BeeSkillTargetSnapshot;
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
use crate::techniques::{AttackPayloadId, AttackShapeId, ChickSkillId, TechniqueId};

pub const CHICK_EGG_ASSET: &str = "food/kenney_food_kit/egg.glb";
pub const CHICK_EGG_HALF_ASSET: &str = "food/kenney_food_kit/egg-half.glb";
pub const CHICK_EGG_COOKED_ASSET: &str = "food/kenney_food_kit/egg-cooked.glb";
pub const CHICK_EGG_CUP_ASSET: &str = "food/kenney_food_kit/egg-cup.glb";
pub const CHICK_EGGPLANT_ASSET: &str = "food/kenney_food_kit/eggplant.glb";

const CHICK_SKILL_LOCK_RANGE: f32 = 7.2;
const CHICK_SKILL_LOCK_CONE_DOT: f32 = 0.70710677;
const CHICK_SHELL_CHIP_SPEED: f32 = 9.6;
pub const CHICK_SHELL_CHIP_LIFETIME: f32 = 0.62;
pub const CHICK_SHELL_CHIP_RADIUS: f32 = 0.25;
const CHICK_FRIED_DISC_SPEED: f32 = 7.4;
pub const CHICK_FRIED_DISC_LIFETIME: f32 = 0.74;
pub const CHICK_FRIED_DISC_RADIUS: f32 = 0.34;
const CHICK_EGG_CUP_SPEED: f32 = 5.8;
const CHICK_EGG_CUP_LIFT: f32 = 3.95;
const CHICK_EGG_CUP_GRAVITY: f32 = 9.6;
pub const CHICK_EGG_CUP_LIFETIME: f32 = 1.12;
pub const CHICK_EGG_CUP_RADIUS: f32 = 0.42;
pub const CHICK_ORBIT_EGG_LIFETIME: f32 = 8.0;
pub const CHICK_ORBIT_EGG_RADIUS: f32 = 0.36;
const CHICK_ORBIT_EGG_LAUNCH_SPEED: f32 = 18.0;
pub const CHICK_ORBIT_EGG_LAUNCH_LIFETIME: f32 = 1.0;
pub const CHICK_ORBIT_EGG_LAUNCH_RADIUS: f32 = 0.78;
const CHICK_ORBIT_EGG_RETURN_SPEED: f32 = 18.0;
pub const CHICK_ORBIT_EGG_RETURN_LIFETIME: f32 = 1.2;
pub const CHICK_ORBIT_EGG_RETURN_RADIUS: f32 = CHICK_ORBIT_EGG_LAUNCH_RADIUS;
const CHICK_ORBIT_EGG_RETURN_ARRIVAL_DISTANCE: f32 = 0.14;
const CHICK_ORBIT_EGG_ORBIT_RADIUS: f32 = 0.95;
const CHICK_ORBIT_EGG_HEIGHT: f32 = 1.0;
const CHICK_ORBIT_EGG_ANGULAR_SPEED: f32 = TAU * 0.85;
const CHICK_ORBIT_EGG_VISUAL_SCALE: f32 = 5.0;
pub const CHICK_ULTIMATE_EGG_COUNT: usize = 16;
pub const CHICK_ULTIMATE_EGG_LIFETIME: f32 = 4.0;
const CHICK_ULTIMATE_EGG_SPAWN_RADIUS: f32 = 0.72;
const CHICK_FRESH_EGG_FORWARD_SPEED: f32 = 1.7;
const CHICK_FRESH_EGG_INITIAL_FALL_SPEED: f32 = 0.4;
const CHICK_FRESH_EGG_GRAVITY: f32 = 13.0;
const CHICK_FRESH_EGG_BASE_VISUAL_SCALE: f32 = 0.46;
const CHICK_FRESH_EGG_BASE_RADIUS: f32 = 0.38;
const CHICK_FRESH_EGG_SIZE_MULTIPLIER: f32 = 3.0;
const CHICK_FRESH_EGG_VISUAL_SCALE: f32 =
    CHICK_FRESH_EGG_BASE_VISUAL_SCALE * CHICK_FRESH_EGG_SIZE_MULTIPLIER;
pub const CHICK_FRESH_EGG_LIFETIME: f32 = 1.0;
pub const CHICK_FRESH_EGG_RADIUS: f32 =
    CHICK_FRESH_EGG_BASE_RADIUS * CHICK_FRESH_EGG_SIZE_MULTIPLIER;
pub const CHICK_FRESH_EGG_RIDE_LIFETIME: f32 = 0.56;
const CHICK_FRESH_EGG_RIDE_FORWARD_OFFSET: f32 = 0.22;
const CHICK_FRESH_EGG_RIDE_VERTICAL_OFFSET: f32 = 0.18;
const CHICK_FRESH_EGG_RIDE_BOB_HEIGHT: f32 = 0.04;
const CHICK_EGGPLANT_SPEED: f32 = 4.6;
pub const CHICK_EGGPLANT_LIFETIME: f32 = 1.22;
pub const CHICK_EGGPLANT_RADIUS: f32 = 0.46;
pub const CHICK_SUNNY_SPLASH_LIFETIME: f32 = 1.15;
pub const CHICK_SUNNY_SPLASH_RADIUS: f32 = 0.84;
pub const CHICK_SUNNY_SPLASH_TICK: f32 = 0.36;
const CHICK_SUNNY_SPLASH_VERTICAL_REACH: f32 = 0.44;
pub const CHICK_OMELET_FIELD_LIFETIME: f32 = 2.05;
pub const CHICK_OMELET_FIELD_RADIUS: f32 = 1.55;
pub const CHICK_OMELET_FIELD_TICK: f32 = 0.42;
const CHICK_OMELET_FIELD_VERTICAL_REACH: f32 = 1.1;
const CHICK_OMELET_FIELD_CENTER_OFFSET: f32 = 1.75;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChickSkillKind {
    ShellChip,
    FriedEggDisc,
    EggCupMortar,
    OrbitEgg,
    OrbitEggLaunch,
    OrbitEggReturn,
    FreshEggDrop,
    FreshEggRide,
    EggplantRoll,
    SunnySplash,
    OmeletField,
}

#[derive(Component)]
pub struct ActiveChickSkill {
    pub kind: ChickSkillKind,
    pub owner: Entity,
    pub owner_id: usize,
    pub owner_style: FighterStyleKind,
    pub payload_id: Option<AttackPayloadId>,
    pub shape_id: AttackShapeId,
    pub source: ImpactSource,
    pub facing: Vec3,
    pub velocity: Vec3,
    pub lifetime: f32,
    pub age: f32,
    pub radius: f32,
    pub guard_stamina_damage: f32,
    pub repeat_interval: Option<f32>,
    pub next_repeat: Option<f32>,
    pub already_hit: Vec<Entity>,
    pub size_scale: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ActiveChickSkillSnapshot {
    pub entity: Entity,
    pub owner: Entity,
    pub kind: ChickSkillKind,
    pub position: Vec3,
}

#[derive(Resource)]
pub struct ChickSkillAssets {
    egg_scene: Handle<Scene>,
    egg_half_scene: Handle<Scene>,
    egg_cooked_scene: Handle<Scene>,
    egg_cup_scene: Handle<Scene>,
    eggplant_scene: Handle<Scene>,
}

pub fn setup_chick_skill_assets(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.insert_resource(ChickSkillAssets {
        egg_scene: asset_server.load(GltfAssetLabel::Scene(0).from_asset(CHICK_EGG_ASSET)),
        egg_half_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(CHICK_EGG_HALF_ASSET)),
        egg_cooked_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(CHICK_EGG_COOKED_ASSET)),
        egg_cup_scene: asset_server.load(GltfAssetLabel::Scene(0).from_asset(CHICK_EGG_CUP_ASSET)),
        eggplant_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(CHICK_EGGPLANT_ASSET)),
    });
}

#[allow(clippy::too_many_arguments)]
pub fn spawn_chick_skill(
    commands: &mut Commands,
    assets: &ChickSkillAssets,
    effect_assets: &EffectAssets,
    state: &MatchState,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    origin: Vec3,
    facing: Vec3,
    aim_held: bool,
    owner_size_scale: f32,
    skill: ChickSkillId,
    targets: &[BeeSkillTargetSnapshot],
    active_skills: &[ActiveChickSkillSnapshot],
) {
    let facing = normalized_or_forward(facing);
    let size_scale = chick_skill_size_scale(owner_size_scale);
    let target = chick_skill_lock_target(owner_id, origin, facing, aim_held, state, targets);
    match skill {
        ChickSkillId::ShellPeck => {
            spawn_shell_chip_pair(
                commands,
                assets,
                effect_assets,
                owner,
                owner_id,
                owner_style,
                origin + Vec3::Y * 0.92 * size_scale + facing * 0.48 * size_scale,
                facing,
                target,
                targets,
                size_scale,
            );
        }
        ChickSkillId::SunnyFlip => {
            let spawn = origin + (Vec3::Y * 0.92 + facing * 0.54) * size_scale;
            let direction = target
                .and_then(|entity| target_position(entity, targets))
                .map(|position| flat_direction(spawn, position))
                .filter(|direction| direction.length_squared() > 0.01)
                .unwrap_or(facing);
            spawn_fried_egg_disc(
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
        ChickSkillId::ShellScramble => {
            spawn_shell_chip_fan(
                commands,
                assets,
                effect_assets,
                owner,
                owner_id,
                owner_style,
                origin + (Vec3::Y * 0.55 + facing * 0.72) * size_scale,
                facing,
                size_scale,
            );
        }
        ChickSkillId::EggCupMortar => {
            let spawn = origin + (Vec3::Y * 1.05 + facing * 0.45) * size_scale;
            let direction = target
                .and_then(|entity| target_position(entity, targets))
                .map(|position| flat_direction(spawn, position))
                .filter(|direction| direction.length_squared() > 0.01)
                .unwrap_or(facing);
            spawn_egg_cup_mortar(
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
        ChickSkillId::OrbitEgg => {
            replace_owner_orbit_eggs(commands, owner, active_skills);
            spawn_orbit_egg(
                commands,
                assets,
                effect_assets,
                owner,
                owner_id,
                owner_style,
                origin,
                facing,
                size_scale,
            );
        }
        ChickSkillId::OrbitEggLaunch => {
            if let Some(orbit_egg) = owner_orbit_egg_for_launch(owner, active_skills) {
                replace_owner_orbit_eggs(commands, owner, active_skills);
                spawn_orbit_egg_launch(
                    commands,
                    assets,
                    effect_assets,
                    owner,
                    owner_id,
                    owner_style,
                    orbit_egg.position,
                    facing,
                    size_scale,
                );
            } else {
                let launched_eggs = owner_launched_orbit_eggs_for_recall(owner, active_skills);
                for launched_egg in launched_eggs {
                    commands.entity(launched_egg.entity).despawn();
                    spawn_orbit_egg_return(
                        commands,
                        assets,
                        effect_assets,
                        owner,
                        owner_id,
                        owner_style,
                        launched_egg.position,
                        facing,
                        size_scale,
                    );
                }
            }
        }
        ChickSkillId::UltimateEggBurst => {
            replace_owner_orbit_eggs(commands, owner, active_skills);
            spawn_ultimate_egg_burst(
                commands,
                assets,
                effect_assets,
                owner,
                owner_id,
                owner_style,
                origin,
                facing,
                size_scale,
            );
        }
        ChickSkillId::EggplantRoll => {
            let spawn = grounded_position(origin + facing * 0.58 * size_scale, 0.19 * size_scale);
            spawn_eggplant_roll(
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
        ChickSkillId::FreshEggDrop => {
            let spawn = origin + (Vec3::Y * 0.28 + facing * 0.24) * size_scale;
            spawn_fresh_egg_drop(
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
        ChickSkillId::FreshEggRide => {
            spawn_fresh_egg_ride(
                commands,
                assets,
                effect_assets,
                owner,
                owner_id,
                owner_style,
                origin,
                facing,
                size_scale,
            );
        }
        ChickSkillId::SunnySideSplash => {
            let spawn = grounded_position(origin + facing * 0.58 * size_scale, 0.04);
            spawn_sunny_splash(
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
        ChickSkillId::OmeletField => {
            let spawn = chick_omelet_field_center(origin, facing, size_scale);
            spawn_omelet_field(
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
}

#[allow(clippy::too_many_arguments)]
fn spawn_shell_chip_pair(
    commands: &mut Commands,
    assets: &ChickSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    facing: Vec3,
    target: Option<Entity>,
    targets: &[BeeSkillTargetSnapshot],
    size_scale: f32,
) {
    let side_vec = chick_skill_side_vec(facing);
    for spread in [-0.42, 0.42] {
        let spawn = position + side_vec * spread * 0.22 * size_scale;
        let direction = target
            .and_then(|entity| target_position(entity, targets))
            .map(|target| flat_direction(spawn, target))
            .filter(|direction| direction.length_squared() > 0.01)
            .unwrap_or_else(|| (facing + side_vec * spread).normalize_or_zero());
        spawn_shell_chip(
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
}

#[allow(clippy::too_many_arguments)]
fn spawn_shell_chip_fan(
    commands: &mut Commands,
    assets: &ChickSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let side_vec = chick_skill_side_vec(facing);
    for spread in [-0.55, 0.0, 0.55] {
        let spawn = position + side_vec * spread * 0.24 * size_scale;
        let direction = (facing + side_vec * spread * 0.72).normalize_or_zero();
        spawn_shell_chip(
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
}

#[allow(clippy::too_many_arguments)]
fn spawn_shell_chip(
    commands: &mut Commands,
    assets: &ChickSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    direction: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(direction);
    commands.spawn((
        SceneRoot(assets.egg_half_scene.clone()),
        Transform::from_translation(position)
            .with_rotation(projectile_rotation(facing))
            .with_scale(chick_skill_visual_scale(
                ChickSkillKind::ShellChip,
                size_scale,
                0.0,
            )),
        active_chick_skill(
            ChickSkillKind::ShellChip,
            owner,
            owner_id,
            owner_style,
            facing,
            facing * CHICK_SHELL_CHIP_SPEED,
            size_scale,
        ),
        Name::new("Chick shell chip"),
    ));
    spawn_feedback_package(
        commands,
        effect_assets,
        position,
        facing,
        FeedbackPackageId::SpecialProjectileStartup,
    );
}

#[allow(clippy::too_many_arguments)]
fn spawn_fried_egg_disc(
    commands: &mut Commands,
    assets: &ChickSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    direction: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(direction);
    commands.spawn((
        SceneRoot(assets.egg_cooked_scene.clone()),
        Transform::from_translation(position)
            .with_rotation(projectile_rotation(facing))
            .with_scale(chick_skill_visual_scale(
                ChickSkillKind::FriedEggDisc,
                size_scale,
                0.0,
            )),
        active_chick_skill(
            ChickSkillKind::FriedEggDisc,
            owner,
            owner_id,
            owner_style,
            facing,
            facing * CHICK_FRIED_DISC_SPEED,
            size_scale,
        ),
        Name::new("Chick fried egg disc"),
    ));
    spawn_feedback_package(
        commands,
        effect_assets,
        position,
        facing,
        FeedbackPackageId::SpecialProjectileStartup,
    );
}

#[allow(clippy::too_many_arguments)]
fn spawn_egg_cup_mortar(
    commands: &mut Commands,
    assets: &ChickSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    direction: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(direction);
    let velocity = facing * CHICK_EGG_CUP_SPEED + Vec3::Y * CHICK_EGG_CUP_LIFT;
    commands.spawn((
        SceneRoot(assets.egg_cup_scene.clone()),
        Transform::from_translation(position)
            .with_rotation(projectile_rotation(facing))
            .with_scale(chick_skill_visual_scale(
                ChickSkillKind::EggCupMortar,
                size_scale,
                0.0,
            )),
        active_chick_skill(
            ChickSkillKind::EggCupMortar,
            owner,
            owner_id,
            owner_style,
            facing,
            velocity,
            size_scale,
        ),
        Name::new("Chick egg-cup mortar"),
    ));
    spawn_feedback_package(
        commands,
        effect_assets,
        position,
        facing,
        FeedbackPackageId::SpecialProjectileStartup,
    );
}

fn replace_owner_orbit_eggs(
    commands: &mut Commands,
    owner: Entity,
    active_skills: &[ActiveChickSkillSnapshot],
) {
    for entity in owner_orbit_egg_replacements(owner, active_skills) {
        commands.entity(entity).despawn();
    }
}

fn owner_orbit_egg_replacements(
    owner: Entity,
    active_skills: &[ActiveChickSkillSnapshot],
) -> Vec<Entity> {
    active_skills
        .iter()
        .filter(|skill| {
            skill.owner == owner
                && matches!(
                    skill.kind,
                    ChickSkillKind::OrbitEgg
                        | ChickSkillKind::OrbitEggLaunch
                        | ChickSkillKind::OrbitEggReturn
                )
        })
        .map(|skill| skill.entity)
        .collect()
}

fn owner_orbit_egg_for_launch(
    owner: Entity,
    active_skills: &[ActiveChickSkillSnapshot],
) -> Option<ActiveChickSkillSnapshot> {
    active_skills
        .iter()
        .find(|skill| skill.owner == owner && skill.kind == ChickSkillKind::OrbitEgg)
        .copied()
}

fn owner_launched_orbit_eggs_for_recall(
    owner: Entity,
    active_skills: &[ActiveChickSkillSnapshot],
) -> Vec<ActiveChickSkillSnapshot> {
    active_skills
        .iter()
        .filter(|skill| skill.owner == owner && skill.kind == ChickSkillKind::OrbitEggLaunch)
        .copied()
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn spawn_orbit_egg(
    commands: &mut Commands,
    assets: &ChickSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    owner_position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    let position = orbit_egg_position(owner_position, facing, size_scale, 0.0);
    commands.spawn((
        SceneRoot(assets.egg_scene.clone()),
        Transform::from_translation(position)
            .with_rotation(orbit_egg_rotation(facing, 0.0))
            .with_scale(chick_skill_visual_scale(
                ChickSkillKind::OrbitEgg,
                size_scale,
                0.0,
            )),
        active_chick_skill(
            ChickSkillKind::OrbitEgg,
            owner,
            owner_id,
            owner_style,
            facing,
            Vec3::ZERO,
            size_scale,
        ),
        Name::new("Chick orbit egg"),
    ));
    spawn_feedback_package(
        commands,
        effect_assets,
        position,
        facing,
        FeedbackPackageId::SpecialProjectileStartup,
    );
}

#[allow(clippy::too_many_arguments)]
fn spawn_orbit_egg_launch(
    commands: &mut Commands,
    assets: &ChickSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    direction: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(direction);
    commands.spawn((
        SceneRoot(assets.egg_scene.clone()),
        Transform::from_translation(position)
            .with_rotation(projectile_rotation(facing))
            .with_scale(chick_skill_visual_scale(
                ChickSkillKind::OrbitEggLaunch,
                size_scale,
                0.0,
            )),
        active_chick_skill(
            ChickSkillKind::OrbitEggLaunch,
            owner,
            owner_id,
            owner_style,
            facing,
            facing * CHICK_ORBIT_EGG_LAUNCH_SPEED,
            size_scale,
        ),
        Name::new("Chick launched orbit egg"),
    ));
    spawn_feedback_package(
        commands,
        effect_assets,
        position,
        facing,
        FeedbackPackageId::SpecialProjectileStartup,
    );
}

#[allow(clippy::too_many_arguments)]
fn spawn_ultimate_egg_burst(
    commands: &mut Commands,
    assets: &ChickSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    origin: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    let origin = origin + Vec3::Y * CHICK_ORBIT_EGG_HEIGHT * size_scale;
    for direction in ultimate_egg_burst_directions(facing) {
        let spawn = origin + direction * CHICK_ULTIMATE_EGG_SPAWN_RADIUS * size_scale;
        spawn_ultimate_orbit_egg_launch(
            commands,
            assets,
            owner,
            owner_id,
            owner_style,
            spawn,
            direction,
            size_scale,
        );
    }
    spawn_feedback_package(
        commands,
        effect_assets,
        origin,
        facing,
        FeedbackPackageId::SpecialProjectileStartup,
    );
}

#[allow(clippy::too_many_arguments)]
fn spawn_ultimate_orbit_egg_launch(
    commands: &mut Commands,
    assets: &ChickSkillAssets,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    direction: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(direction);
    commands.spawn((
        SceneRoot(assets.egg_scene.clone()),
        Transform::from_translation(position)
            .with_rotation(projectile_rotation(facing))
            .with_scale(chick_skill_visual_scale(
                ChickSkillKind::OrbitEggLaunch,
                size_scale,
                0.0,
            )),
        ultimate_orbit_egg_skill(owner, owner_id, owner_style, facing, size_scale),
        Name::new("Chick ultimate orbit egg"),
    ));
}

fn ultimate_orbit_egg_skill(
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    facing: Vec3,
    size_scale: f32,
) -> ActiveChickSkill {
    let facing = normalized_or_forward(facing);
    let mut skill = active_chick_skill(
        ChickSkillKind::OrbitEggLaunch,
        owner,
        owner_id,
        owner_style,
        facing,
        facing * CHICK_ORBIT_EGG_LAUNCH_SPEED,
        size_scale,
    );
    skill.lifetime = CHICK_ULTIMATE_EGG_LIFETIME;
    skill
}

fn ultimate_egg_burst_directions(facing: Vec3) -> [Vec3; CHICK_ULTIMATE_EGG_COUNT] {
    let facing = normalized_or_forward(facing);
    let base = facing.z.atan2(facing.x);
    std::array::from_fn(|index| {
        let angle = base + TAU * index as f32 / CHICK_ULTIMATE_EGG_COUNT as f32;
        Vec3::new(angle.cos(), 0.0, angle.sin()).normalize_or_zero()
    })
}

#[allow(clippy::too_many_arguments)]
fn spawn_orbit_egg_return(
    commands: &mut Commands,
    assets: &ChickSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    owner_facing: Vec3,
    size_scale: f32,
) {
    let owner_facing = normalized_or_forward(owner_facing);
    commands.spawn((
        SceneRoot(assets.egg_scene.clone()),
        Transform::from_translation(position)
            .with_rotation(projectile_rotation(owner_facing))
            .with_scale(chick_skill_visual_scale(
                ChickSkillKind::OrbitEggReturn,
                size_scale,
                0.0,
            )),
        active_chick_skill(
            ChickSkillKind::OrbitEggReturn,
            owner,
            owner_id,
            owner_style,
            owner_facing,
            Vec3::ZERO,
            size_scale,
        ),
        Name::new("Chick returning orbit egg"),
    ));
    spawn_feedback_package(
        commands,
        effect_assets,
        position,
        owner_facing,
        FeedbackPackageId::SpecialProjectileStartup,
    );
}

#[allow(clippy::too_many_arguments)]
fn spawn_fresh_egg_drop(
    commands: &mut Commands,
    assets: &ChickSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    let velocity =
        facing * CHICK_FRESH_EGG_FORWARD_SPEED + Vec3::Y * -CHICK_FRESH_EGG_INITIAL_FALL_SPEED;
    commands.spawn((
        SceneRoot(assets.egg_scene.clone()),
        Transform::from_translation(position)
            .with_rotation(projectile_rotation(facing))
            .with_scale(chick_skill_visual_scale(
                ChickSkillKind::FreshEggDrop,
                size_scale,
                0.0,
            )),
        active_chick_skill(
            ChickSkillKind::FreshEggDrop,
            owner,
            owner_id,
            owner_style,
            facing,
            velocity,
            size_scale,
        ),
        Name::new("Chick fresh egg drop"),
    ));
    spawn_feedback_package(
        commands,
        effect_assets,
        position,
        facing,
        FeedbackPackageId::SpecialProjectileStartup,
    );
}

#[allow(clippy::too_many_arguments)]
fn spawn_fresh_egg_ride(
    commands: &mut Commands,
    assets: &ChickSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    owner_position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    let position = fresh_egg_ride_position(owner_position, facing, size_scale, 0.0);
    commands.spawn((
        SceneRoot(assets.egg_scene.clone()),
        Transform::from_translation(position)
            .with_rotation(projectile_rotation(facing))
            .with_scale(chick_skill_visual_scale(
                ChickSkillKind::FreshEggRide,
                size_scale,
                0.0,
            )),
        active_chick_skill(
            ChickSkillKind::FreshEggRide,
            owner,
            owner_id,
            owner_style,
            facing,
            Vec3::ZERO,
            size_scale,
        ),
        Name::new("Chick fresh egg ride"),
    ));
    spawn_feedback_package(
        commands,
        effect_assets,
        position,
        facing,
        FeedbackPackageId::SpecialProjectileStartup,
    );
}

#[allow(clippy::too_many_arguments)]
fn spawn_eggplant_roll(
    commands: &mut Commands,
    assets: &ChickSkillAssets,
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
        SceneRoot(assets.eggplant_scene.clone()),
        Transform::from_translation(position)
            .with_rotation(projectile_rotation(facing))
            .with_scale(chick_skill_visual_scale(
                ChickSkillKind::EggplantRoll,
                size_scale,
                0.0,
            )),
        active_chick_skill(
            ChickSkillKind::EggplantRoll,
            owner,
            owner_id,
            owner_style,
            facing,
            facing * CHICK_EGGPLANT_SPEED,
            size_scale,
        ),
        Name::new("Chick eggplant roll"),
    ));
    spawn_feedback_package(
        commands,
        effect_assets,
        position,
        facing,
        FeedbackPackageId::SpecialProjectileStartup,
    );
}

#[allow(clippy::too_many_arguments)]
fn spawn_sunny_splash(
    commands: &mut Commands,
    assets: &ChickSkillAssets,
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
        SceneRoot(assets.egg_cooked_scene.clone()),
        Transform::from_translation(grounded_position(position, 0.035))
            .with_rotation(projectile_rotation(facing))
            .with_scale(chick_skill_visual_scale(
                ChickSkillKind::SunnySplash,
                size_scale,
                0.0,
            )),
        active_chick_skill(
            ChickSkillKind::SunnySplash,
            owner,
            owner_id,
            owner_style,
            facing,
            Vec3::ZERO,
            size_scale,
        ),
        Name::new("Chick sunny-side splash"),
    ));
    spawn_feedback_package(
        commands,
        effect_assets,
        position,
        facing,
        FeedbackPackageId::SpecialHazardStartup,
    );
}

#[allow(clippy::too_many_arguments)]
fn spawn_omelet_field(
    commands: &mut Commands,
    assets: &ChickSkillAssets,
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
        SceneRoot(assets.egg_cooked_scene.clone()),
        Transform::from_translation(grounded_position(position, 0.04))
            .with_rotation(projectile_rotation(facing))
            .with_scale(chick_skill_visual_scale(
                ChickSkillKind::OmeletField,
                size_scale,
                0.0,
            )),
        active_chick_skill(
            ChickSkillKind::OmeletField,
            owner,
            owner_id,
            owner_style,
            facing,
            Vec3::ZERO,
            size_scale,
        ),
        Name::new("Chick omelet field"),
    ));
    spawn_feedback_package(
        commands,
        effect_assets,
        position,
        facing,
        FeedbackPackageId::SpecialHazardStartup,
    );
}

fn active_chick_skill(
    kind: ChickSkillKind,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    facing: Vec3,
    velocity: Vec3,
    size_scale: f32,
) -> ActiveChickSkill {
    let size_scale = chick_skill_size_scale(size_scale);
    let (payload_id, shape_id, source, lifetime, radius, guard_stamina_damage, repeat_interval) =
        match kind {
            ChickSkillKind::ShellChip => (
                Some(AttackPayloadId::ChickShellChip),
                AttackShapeId::ProjectileBolt,
                ImpactSource::Projectile,
                CHICK_SHELL_CHIP_LIFETIME,
                CHICK_SHELL_CHIP_RADIUS,
                5.0,
                None,
            ),
            ChickSkillKind::FriedEggDisc => (
                Some(AttackPayloadId::ChickFriedEggDisc),
                AttackShapeId::ProjectileBolt,
                ImpactSource::Projectile,
                CHICK_FRIED_DISC_LIFETIME,
                CHICK_FRIED_DISC_RADIUS,
                8.0,
                None,
            ),
            ChickSkillKind::EggCupMortar => (
                Some(AttackPayloadId::ChickEggCupMortar),
                AttackShapeId::ProjectileBolt,
                ImpactSource::Projectile,
                CHICK_EGG_CUP_LIFETIME,
                CHICK_EGG_CUP_RADIUS,
                11.0,
                None,
            ),
            ChickSkillKind::OrbitEgg => (
                Some(AttackPayloadId::ChickOrbitEgg),
                AttackShapeId::ProjectileBolt,
                ImpactSource::Projectile,
                CHICK_ORBIT_EGG_LIFETIME,
                CHICK_ORBIT_EGG_RADIUS,
                2.0,
                None,
            ),
            ChickSkillKind::OrbitEggLaunch => (
                Some(AttackPayloadId::ChickOrbitEgg),
                AttackShapeId::ProjectileBolt,
                ImpactSource::Projectile,
                CHICK_ORBIT_EGG_LAUNCH_LIFETIME,
                CHICK_ORBIT_EGG_LAUNCH_RADIUS,
                2.0,
                None,
            ),
            ChickSkillKind::OrbitEggReturn => (
                Some(AttackPayloadId::ChickOrbitEggLaunch),
                AttackShapeId::ProjectileBolt,
                ImpactSource::Projectile,
                CHICK_ORBIT_EGG_RETURN_LIFETIME,
                CHICK_ORBIT_EGG_RETURN_RADIUS,
                10.0,
                None,
            ),
            ChickSkillKind::FreshEggDrop => (
                Some(AttackPayloadId::ChickFreshEggDrop),
                AttackShapeId::ProjectileBolt,
                ImpactSource::Projectile,
                CHICK_FRESH_EGG_LIFETIME,
                CHICK_FRESH_EGG_RADIUS,
                7.0,
                None,
            ),
            ChickSkillKind::FreshEggRide => (
                None,
                AttackShapeId::ProjectileBolt,
                ImpactSource::Projectile,
                CHICK_FRESH_EGG_RIDE_LIFETIME,
                0.0,
                0.0,
                None,
            ),
            ChickSkillKind::EggplantRoll => (
                Some(AttackPayloadId::ChickEggplantRoll),
                AttackShapeId::ProjectileBolt,
                ImpactSource::Projectile,
                CHICK_EGGPLANT_LIFETIME,
                CHICK_EGGPLANT_RADIUS,
                12.0,
                None,
            ),
            ChickSkillKind::SunnySplash => (
                Some(AttackPayloadId::ChickSunnySplash),
                AttackShapeId::HazardField,
                ImpactSource::Hazard,
                CHICK_SUNNY_SPLASH_LIFETIME,
                CHICK_SUNNY_SPLASH_RADIUS,
                5.0,
                Some(CHICK_SUNNY_SPLASH_TICK),
            ),
            ChickSkillKind::OmeletField => (
                Some(AttackPayloadId::ChickOmeletField),
                AttackShapeId::HazardField,
                ImpactSource::Hazard,
                CHICK_OMELET_FIELD_LIFETIME,
                CHICK_OMELET_FIELD_RADIUS,
                6.0,
                Some(CHICK_OMELET_FIELD_TICK),
            ),
        };

    ActiveChickSkill {
        kind,
        owner,
        owner_id,
        owner_style,
        payload_id,
        shape_id,
        source,
        facing: normalized_or_forward(facing),
        velocity,
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

fn chick_skill_size_scale(owner_size_scale: f32) -> f32 {
    owner_size_scale.max(0.1)
}

fn chick_skill_visual_scale(kind: ChickSkillKind, size_scale: f32, age: f32) -> Vec3 {
    let size_scale = chick_skill_size_scale(size_scale);
    match kind {
        ChickSkillKind::ShellChip => Vec3::splat(0.38 * size_scale),
        ChickSkillKind::FriedEggDisc => Vec3::splat(0.48 * size_scale),
        ChickSkillKind::EggCupMortar => Vec3::splat(0.5 * size_scale),
        ChickSkillKind::OrbitEgg
        | ChickSkillKind::OrbitEggLaunch
        | ChickSkillKind::OrbitEggReturn => {
            Vec3::splat(0.44 * CHICK_ORBIT_EGG_VISUAL_SCALE * size_scale)
        }
        ChickSkillKind::FreshEggDrop | ChickSkillKind::FreshEggRide => {
            Vec3::splat(CHICK_FRESH_EGG_VISUAL_SCALE * size_scale)
        }
        ChickSkillKind::EggplantRoll => Vec3::splat(0.58 * size_scale),
        ChickSkillKind::SunnySplash => Vec3::splat(sunny_splash_visual_pulse(age) * size_scale),
        ChickSkillKind::OmeletField => Vec3::splat(omelet_field_visual_pulse(age) * size_scale),
    }
}

fn sunny_splash_visual_pulse(age: f32) -> f32 {
    0.78 + (age * 9.0).sin().abs() * 0.08
}

fn omelet_field_visual_pulse(age: f32) -> f32 {
    1.28 + (age * 12.0).sin().abs() * 0.12
}

pub fn update_chick_skills(
    time: Res<Time>,
    mut commands: Commands,
    assets: Res<ChickSkillAssets>,
    effect_assets: Res<EffectAssets>,
    state: Res<MatchState>,
    feel: Res<CombatFeelTuning>,
    mut hitstop: ResMut<Hitstop>,
    mut camera_effects: ResMut<HitEffects>,
    mut telemetry: ResMut<MatchTelemetry>,
    mut skills: Query<(Entity, &mut ActiveChickSkill, &mut Transform), Without<Fighter>>,
    mut fighters: ParamSet<(
        Query<(&FighterActionState, &Transform), With<Fighter>>,
        Query<
            (
                Entity,
                &Fighter,
                &mut FighterStats,
                &mut FighterMotor,
                &mut FighterActionState,
                &FighterStyle,
                &FighterEquipment,
                &Transform,
            ),
            With<Fighter>,
        >,
    )>,
) {
    if hitstop.active() {
        return;
    }

    let dt = time.delta_secs();
    for (skill_entity, mut skill, mut transform) in &mut skills {
        skill.age += dt;
        skill.lifetime -= dt;
        update_skill_repeat_window(&mut skill, &mut camera_effects);
        update_chick_skill_motion(&mut skill, &mut transform, dt, &fighters.p0());

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
                target_transform,
            ) in &mut target_fighters
            {
                if !chick_skill_can_hit_target(&skill, target_entity, target.id, &state) {
                    continue;
                }
                if (chick_skill_uses_hit_memory(skill.kind)
                    && skill.already_hit.contains(&target_entity))
                    || !can_receive_impact(&stats, &action)
                    || !chick_skill_overlaps_target(&skill, transform.translation, target_transform)
                {
                    continue;
                }

                let profile = chick_skill_impact_profile(&skill, &feel);
                apply_impact(
                    &mut commands,
                    &effect_assets,
                    &mut camera_effects,
                    &mut hitstop,
                    &state,
                    &mut stats,
                    &mut motor,
                    &mut action,
                    target_transform,
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
                if chick_skill_uses_hit_memory(skill.kind) {
                    skill.already_hit.push(target_entity);
                }
                hit_this_frame = true;

                if chick_skill_consumed_on_hit(skill.kind) {
                    skill.lifetime = 0.0;
                    break;
                }
            }
        }

        let cracked = fresh_egg_drop_touched_ground(&skill, transform.translation);
        let projectile_grounded = chick_projectile_touched_ground(&skill, transform.translation);
        if cracked {
            spawn_shell_chip_fan(
                &mut commands,
                &assets,
                &effect_assets,
                skill.owner,
                skill.owner_id,
                skill.owner_style,
                transform.translation + Vec3::Y * 0.16,
                skill.facing,
                skill.size_scale,
            );
            spawn_feedback_package(
                &mut commands,
                &effect_assets,
                transform.translation,
                skill.facing,
                FeedbackPackageId::SpecialProjectileImpact,
            );
        }

        if skill.lifetime <= 0.0
            || cracked
            || projectile_grounded
            || should_despawn_skill(transform.translation)
        {
            if !hit_this_frame && !cracked {
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

fn update_skill_repeat_window(skill: &mut ActiveChickSkill, effects: &mut HitEffects) {
    let Some(interval) = skill.repeat_interval else {
        return;
    };
    let Some(mut next_repeat) = skill.next_repeat else {
        return;
    };
    while skill.age >= next_repeat {
        skill.already_hit.clear();
        effects.push_feedback_cue("pulse_chick_breakfast_hazard", skill.source, 24);
        next_repeat += interval;
    }
    skill.next_repeat = Some(next_repeat);
}

fn update_chick_skill_motion(
    skill: &mut ActiveChickSkill,
    transform: &mut Transform,
    dt: f32,
    owners: &Query<(&FighterActionState, &Transform), With<Fighter>>,
) {
    match skill.kind {
        ChickSkillKind::ShellChip
        | ChickSkillKind::FriedEggDisc
        | ChickSkillKind::OrbitEggLaunch
        | ChickSkillKind::OrbitEggReturn
        | ChickSkillKind::EggplantRoll => {
            if skill.kind == ChickSkillKind::OrbitEggReturn {
                let owner_position = owners
                    .get(skill.owner)
                    .ok()
                    .map(|(_, owner_transform)| owner_transform.translation);
                update_orbit_egg_return_motion(skill, transform, owner_position, dt);
            } else {
                transform.translation += skill.velocity * dt;
                transform.rotate_y(0.16);
                transform.rotation = projectile_rotation(skill.facing) * transform.rotation;
            }
        }
        ChickSkillKind::EggCupMortar => {
            skill.velocity.y -= CHICK_EGG_CUP_GRAVITY * dt;
            transform.translation += skill.velocity * dt;
            transform.rotate_x(0.12);
            transform.rotate_y(0.08);
        }
        ChickSkillKind::OrbitEgg => {
            let owner_position = owners
                .get(skill.owner)
                .ok()
                .map(|(_, owner_transform)| owner_transform.translation);
            update_orbit_egg_motion(skill, transform, owner_position);
        }
        ChickSkillKind::FreshEggDrop => {
            skill.velocity.y -= CHICK_FRESH_EGG_GRAVITY * dt;
            transform.translation += skill.velocity * dt;
            transform.rotate_x(0.1);
        }
        ChickSkillKind::FreshEggRide => {
            let owner_state = owners.get(skill.owner).ok();
            update_fresh_egg_ride_motion(skill, transform, owner_state);
        }
        ChickSkillKind::SunnySplash => {
            transform.scale =
                chick_skill_visual_scale(ChickSkillKind::SunnySplash, skill.size_scale, skill.age);
        }
        ChickSkillKind::OmeletField => {
            transform.scale =
                chick_skill_visual_scale(ChickSkillKind::OmeletField, skill.size_scale, skill.age);
        }
    }
}

fn update_orbit_egg_motion(
    skill: &mut ActiveChickSkill,
    transform: &mut Transform,
    owner_position: Option<Vec3>,
) {
    let Some(owner_position) = owner_position else {
        skill.lifetime = 0.0;
        return;
    };
    transform.translation =
        orbit_egg_position(owner_position, skill.facing, skill.size_scale, skill.age);
    transform.rotation = orbit_egg_rotation(skill.facing, skill.age);
    transform.scale =
        chick_skill_visual_scale(ChickSkillKind::OrbitEgg, skill.size_scale, skill.age);
}

fn update_orbit_egg_return_motion(
    skill: &mut ActiveChickSkill,
    transform: &mut Transform,
    owner_position: Option<Vec3>,
    dt: f32,
) {
    let Some(owner_position) = owner_position else {
        skill.lifetime = 0.0;
        return;
    };
    let target = orbit_egg_position(owner_position, skill.facing, skill.size_scale, 0.0);
    let to_target = target - transform.translation;
    let distance = to_target.length();
    if distance <= CHICK_ORBIT_EGG_RETURN_ARRIVAL_DISTANCE {
        restore_returned_orbit_egg(skill, transform, target);
        return;
    }

    let travel = CHICK_ORBIT_EGG_RETURN_SPEED * dt;
    if travel >= distance {
        transform.translation = target;
        restore_returned_orbit_egg(skill, transform, target);
        return;
    }

    let direction = to_target / distance;
    skill.velocity = direction * CHICK_ORBIT_EGG_RETURN_SPEED;
    transform.translation += skill.velocity * dt;
    transform.rotation = projectile_rotation(direction)
        * Quat::from_rotation_y(skill.age * CHICK_ORBIT_EGG_ANGULAR_SPEED);
    transform.scale =
        chick_skill_visual_scale(ChickSkillKind::OrbitEggReturn, skill.size_scale, skill.age);
}

fn restore_returned_orbit_egg(
    skill: &mut ActiveChickSkill,
    transform: &mut Transform,
    target: Vec3,
) {
    let owner = skill.owner;
    let owner_id = skill.owner_id;
    let owner_style = skill.owner_style;
    let facing = skill.facing;
    let size_scale = skill.size_scale;
    *skill = active_chick_skill(
        ChickSkillKind::OrbitEgg,
        owner,
        owner_id,
        owner_style,
        facing,
        Vec3::ZERO,
        size_scale,
    );
    transform.translation = target;
    transform.rotation = orbit_egg_rotation(facing, 0.0);
    transform.scale = chick_skill_visual_scale(ChickSkillKind::OrbitEgg, size_scale, 0.0);
}

fn update_fresh_egg_ride_motion(
    skill: &mut ActiveChickSkill,
    transform: &mut Transform,
    owner_state: Option<(&FighterActionState, &Transform)>,
) {
    let Some((owner_action, owner_transform)) = owner_state else {
        skill.lifetime = 0.0;
        return;
    };
    if owner_action.technique_id != Some(TechniqueId::ChickJumpHeavy) {
        skill.lifetime = 0.0;
        return;
    }

    transform.translation = fresh_egg_ride_position(
        owner_transform.translation,
        skill.facing,
        skill.size_scale,
        skill.age,
    );
    transform.rotation = projectile_rotation(skill.facing);
    transform.scale =
        chick_skill_visual_scale(ChickSkillKind::FreshEggRide, skill.size_scale, skill.age);
}

fn fresh_egg_ride_position(owner_position: Vec3, facing: Vec3, size_scale: f32, age: f32) -> Vec3 {
    let size_scale = chick_skill_size_scale(size_scale);
    let bob = (age / CHICK_FRESH_EGG_RIDE_LIFETIME * TAU).sin()
        * CHICK_FRESH_EGG_RIDE_BOB_HEIGHT
        * size_scale;
    owner_position
        + normalized_or_forward(facing) * CHICK_FRESH_EGG_RIDE_FORWARD_OFFSET * size_scale
        + Vec3::Y * (CHICK_FRESH_EGG_RIDE_VERTICAL_OFFSET * size_scale + bob)
}

fn orbit_egg_position(owner_position: Vec3, facing: Vec3, size_scale: f32, age: f32) -> Vec3 {
    let facing = normalized_or_forward(facing);
    let side = chick_skill_side_vec(facing);
    let angle = age * CHICK_ORBIT_EGG_ANGULAR_SPEED;
    let orbit = (facing * angle.cos() + side * angle.sin())
        * CHICK_ORBIT_EGG_ORBIT_RADIUS
        * chick_skill_size_scale(size_scale);
    owner_position + orbit + Vec3::Y * CHICK_ORBIT_EGG_HEIGHT * chick_skill_size_scale(size_scale)
}

fn orbit_egg_rotation(facing: Vec3, age: f32) -> Quat {
    let angle = age * CHICK_ORBIT_EGG_ANGULAR_SPEED;
    projectile_rotation(facing) * Quat::from_rotation_y(angle) * Quat::from_rotation_x(angle * 0.35)
}

fn chick_skill_impact_profile(
    skill: &ActiveChickSkill,
    feel: &CombatFeelTuning,
) -> crate::combat::ImpactProfile {
    let payload_id = skill
        .payload_id
        .expect("visual-only Chick skills should not build impact profiles");
    let mut profile = impact_profile_from_payload_with_feel(
        skill.owner_id,
        skill.source,
        payload_id,
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

fn chick_skill_overlaps_target(
    skill: &ActiveChickSkill,
    origin: Vec3,
    target_transform: &Transform,
) -> bool {
    match skill.kind {
        ChickSkillKind::FreshEggRide => false,
        ChickSkillKind::SunnySplash => {
            let target_position = target_transform.translation;
            flat_distance(origin, target_position)
                <= skill.radius * sunny_splash_visual_pulse(skill.age) + FIGHTER_RADIUS
                && (target_position.y - origin.y).abs()
                    <= CHICK_SUNNY_SPLASH_VERTICAL_REACH * skill.size_scale
        }
        ChickSkillKind::OmeletField => {
            let target_position = target_transform.translation;
            flat_distance(origin, target_position)
                <= skill.radius * omelet_field_visual_pulse(skill.age) + FIGHTER_RADIUS
                && (target_position.y - origin.y).abs()
                    <= CHICK_OMELET_FIELD_VERTICAL_REACH * skill.size_scale
        }
        _ => {
            let target = target_transform.translation + Vec3::Y * (FIGHTER_HEIGHT * 0.58);
            target.distance(origin) <= skill.radius + FIGHTER_RADIUS
        }
    }
}

pub fn chick_skill_lock_target(
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
        .filter(|target| target.fighter_id != owner_id)
        .filter(|target| state.combat_target_allowed_for_state(owner_id, target.fighter_id))
        .filter_map(|target| {
            let offset = Vec3::new(
                target.position.x - origin.x,
                0.0,
                target.position.z - origin.z,
            );
            let distance = offset.length();
            if distance > CHICK_SKILL_LOCK_RANGE || distance <= 0.01 {
                return None;
            }
            let direction = offset / distance;
            (direction.dot(facing) >= CHICK_SKILL_LOCK_CONE_DOT)
                .then_some((target.entity, distance))
        })
        .min_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(entity, _)| entity)
}

fn chick_skill_can_hit_target(
    skill: &ActiveChickSkill,
    target_entity: Entity,
    target_id: usize,
    state: &MatchState,
) -> bool {
    if skill.kind == ChickSkillKind::FreshEggRide || skill.payload_id.is_none() {
        return false;
    }
    target_entity != skill.owner
        && target_id != skill.owner_id
        && state.combat_target_allowed_for_state(skill.owner_id, target_id)
}

fn chick_skill_consumed_on_hit(kind: ChickSkillKind) -> bool {
    matches!(
        kind,
        ChickSkillKind::ShellChip
            | ChickSkillKind::FriedEggDisc
            | ChickSkillKind::EggCupMortar
            | ChickSkillKind::FreshEggDrop
            | ChickSkillKind::EggplantRoll
    )
}

fn chick_skill_uses_hit_memory(kind: ChickSkillKind) -> bool {
    !matches!(
        kind,
        ChickSkillKind::OrbitEgg | ChickSkillKind::FreshEggRide
    )
}

fn fresh_egg_drop_touched_ground(skill: &ActiveChickSkill, position: Vec3) -> bool {
    if skill.kind != ChickSkillKind::FreshEggDrop || skill.age <= 0.08 {
        return false;
    }
    let ground = ground_height_at(position.x, position.z).unwrap_or(ARENA_TOP_Y);
    position.y <= ground + 0.08
}

fn chick_projectile_touched_ground(skill: &ActiveChickSkill, position: Vec3) -> bool {
    if skill.kind != ChickSkillKind::EggCupMortar || skill.age <= 0.18 {
        return false;
    }
    let ground = ground_height_at(position.x, position.z).unwrap_or(ARENA_TOP_Y);
    position.y <= ground + 0.08
}

fn target_position(entity: Entity, targets: &[BeeSkillTargetSnapshot]) -> Option<Vec3> {
    targets
        .iter()
        .find(|target| target.entity == entity)
        .map(|target| target.position)
}

fn chick_omelet_field_center(origin: Vec3, facing: Vec3, size_scale: f32) -> Vec3 {
    grounded_position(
        origin + normalized_or_forward(facing) * CHICK_OMELET_FIELD_CENTER_OFFSET * size_scale,
        0.04,
    )
}

fn grounded_position(position: Vec3, clearance: f32) -> Vec3 {
    let ground = ground_height_at(position.x, position.z).unwrap_or(ARENA_TOP_Y);
    Vec3::new(position.x, ground + clearance, position.z)
}

fn should_despawn_skill(position: Vec3) -> bool {
    let arena = active_arena_definition();
    position.y < arena.ringout_y
        || Vec2::new(position.x, position.z).length() > arena.ringout_radius
}

fn impact_package(kind: ChickSkillKind) -> FeedbackPackageId {
    match kind {
        ChickSkillKind::SunnySplash | ChickSkillKind::OmeletField => {
            FeedbackPackageId::SpecialHazardImpact
        }
        _ => FeedbackPackageId::SpecialProjectileImpact,
    }
}

fn despawn_package(kind: ChickSkillKind) -> FeedbackPackageId {
    match kind {
        ChickSkillKind::SunnySplash | ChickSkillKind::OmeletField => {
            FeedbackPackageId::SpecialHazardFade
        }
        _ => FeedbackPackageId::SpecialProjectileRecover,
    }
}

fn chick_skill_side_vec(facing: Vec3) -> Vec3 {
    Vec3::new(-facing.z, 0.0, facing.x).normalize_or_zero()
}

fn flat_direction(origin: Vec3, target: Vec3) -> Vec3 {
    Vec3::new(target.x - origin.x, 0.0, target.z - origin.z).normalize_or_zero()
}

fn flat_distance(a: Vec3, b: Vec3) -> f32 {
    Vec2::new(a.x - b.x, a.z - b.z).length()
}

fn normalized_or_forward(direction: Vec3) -> Vec3 {
    if direction.length_squared() > 0.01 {
        direction.normalize()
    } else {
        Vec3::Z
    }
}

fn projectile_rotation(facing: Vec3) -> Quat {
    let facing = normalized_or_forward(facing);
    Quat::from_rotation_arc(Vec3::Z, facing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reactions::ReactionFamilyId;
    use crate::techniques::attack_payload_definition;

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
    fn chick_asset_paths_exist_for_runtime_loading() {
        assert!(std::path::Path::new("assets/food/kenney_food_kit/egg.glb").exists());
        assert!(std::path::Path::new("assets/food/kenney_food_kit/egg-half.glb").exists());
        assert!(std::path::Path::new("assets/food/kenney_food_kit/egg-cooked.glb").exists());
        assert!(std::path::Path::new("assets/food/kenney_food_kit/egg-cup.glb").exists());
        assert!(std::path::Path::new("assets/food/kenney_food_kit/eggplant.glb").exists());
    }

    #[test]
    fn active_skill_constants_map_to_chick_payloads() {
        let cases = [
            (
                ChickSkillKind::ShellChip,
                Some(AttackPayloadId::ChickShellChip),
                CHICK_SHELL_CHIP_RADIUS,
                CHICK_SHELL_CHIP_LIFETIME,
                None,
                5.0,
            ),
            (
                ChickSkillKind::FriedEggDisc,
                Some(AttackPayloadId::ChickFriedEggDisc),
                CHICK_FRIED_DISC_RADIUS,
                CHICK_FRIED_DISC_LIFETIME,
                None,
                8.0,
            ),
            (
                ChickSkillKind::EggCupMortar,
                Some(AttackPayloadId::ChickEggCupMortar),
                CHICK_EGG_CUP_RADIUS,
                CHICK_EGG_CUP_LIFETIME,
                None,
                11.0,
            ),
            (
                ChickSkillKind::OrbitEgg,
                Some(AttackPayloadId::ChickOrbitEgg),
                CHICK_ORBIT_EGG_RADIUS,
                CHICK_ORBIT_EGG_LIFETIME,
                None,
                2.0,
            ),
            (
                ChickSkillKind::OrbitEggLaunch,
                Some(AttackPayloadId::ChickOrbitEgg),
                CHICK_ORBIT_EGG_LAUNCH_RADIUS,
                CHICK_ORBIT_EGG_LAUNCH_LIFETIME,
                None,
                2.0,
            ),
            (
                ChickSkillKind::OrbitEggReturn,
                Some(AttackPayloadId::ChickOrbitEggLaunch),
                CHICK_ORBIT_EGG_RETURN_RADIUS,
                CHICK_ORBIT_EGG_RETURN_LIFETIME,
                None,
                10.0,
            ),
            (
                ChickSkillKind::FreshEggDrop,
                Some(AttackPayloadId::ChickFreshEggDrop),
                CHICK_FRESH_EGG_RADIUS,
                CHICK_FRESH_EGG_LIFETIME,
                None,
                7.0,
            ),
            (
                ChickSkillKind::FreshEggRide,
                None,
                0.0,
                CHICK_FRESH_EGG_RIDE_LIFETIME,
                None,
                0.0,
            ),
            (
                ChickSkillKind::EggplantRoll,
                Some(AttackPayloadId::ChickEggplantRoll),
                CHICK_EGGPLANT_RADIUS,
                CHICK_EGGPLANT_LIFETIME,
                None,
                12.0,
            ),
            (
                ChickSkillKind::SunnySplash,
                Some(AttackPayloadId::ChickSunnySplash),
                CHICK_SUNNY_SPLASH_RADIUS,
                CHICK_SUNNY_SPLASH_LIFETIME,
                Some(CHICK_SUNNY_SPLASH_TICK),
                5.0,
            ),
            (
                ChickSkillKind::OmeletField,
                Some(AttackPayloadId::ChickOmeletField),
                CHICK_OMELET_FIELD_RADIUS,
                CHICK_OMELET_FIELD_LIFETIME,
                Some(CHICK_OMELET_FIELD_TICK),
                6.0,
            ),
        ];

        for (kind, payload, radius, lifetime, repeat, guard_stamina_damage) in cases {
            let skill = active_chick_skill(
                kind,
                entity(1),
                0,
                FighterStyleKind::Anchor,
                Vec3::X,
                Vec3::ZERO,
                1.0,
            );

            assert_eq!(skill.payload_id, payload);
            assert_eq!(skill.radius, radius);
            assert_eq!(skill.lifetime, lifetime);
            assert_eq!(skill.repeat_interval, repeat);
            assert_eq!(skill.guard_stamina_damage, guard_stamina_damage);
        }
    }

    #[test]
    fn orbit_egg_position_tracks_owner_and_rotates() {
        let mut skill = active_chick_skill(
            ChickSkillKind::OrbitEgg,
            entity(1),
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::ZERO,
            1.0,
        );
        let mut transform = Transform::default();

        update_orbit_egg_motion(&mut skill, &mut transform, Some(Vec3::new(1.0, 0.0, 2.0)));
        assert_vec3_close(
            transform.translation,
            Vec3::new(1.0, 0.0, 2.0)
                + Vec3::X * CHICK_ORBIT_EGG_ORBIT_RADIUS
                + Vec3::Y * CHICK_ORBIT_EGG_HEIGHT,
        );

        skill.age = 0.25;
        update_orbit_egg_motion(&mut skill, &mut transform, Some(Vec3::new(3.0, 0.0, -1.0)));
        let expected_offset = orbit_egg_position(Vec3::ZERO, Vec3::X, 1.0, 0.25);
        assert_vec3_close(
            transform.translation,
            Vec3::new(3.0, 0.0, -1.0) + expected_offset,
        );
    }

    #[test]
    fn orbit_egg_visual_is_five_times_base_egg_size() {
        assert_vec3_close(
            chick_skill_visual_scale(ChickSkillKind::OrbitEgg, 1.0, 0.0),
            Vec3::splat(0.44 * CHICK_ORBIT_EGG_VISUAL_SCALE),
        );
        assert_vec3_close(
            chick_skill_visual_scale(ChickSkillKind::OrbitEggLaunch, 1.0, 0.0),
            Vec3::splat(0.44 * CHICK_ORBIT_EGG_VISUAL_SCALE),
        );
        assert_vec3_close(
            chick_skill_visual_scale(ChickSkillKind::OrbitEggReturn, 1.0, 0.0),
            Vec3::splat(0.44 * CHICK_ORBIT_EGG_VISUAL_SCALE),
        );
        assert_eq!(CHICK_ORBIT_EGG_VISUAL_SCALE, 5.0);
    }

    #[test]
    fn fresh_egg_drop_and_ride_use_triple_jump_x_egg_size() {
        assert_vec3_close(
            chick_skill_visual_scale(ChickSkillKind::FreshEggDrop, 1.0, 0.0),
            Vec3::splat(CHICK_FRESH_EGG_VISUAL_SCALE),
        );
        assert_vec3_close(
            chick_skill_visual_scale(ChickSkillKind::FreshEggRide, 1.0, 0.0),
            Vec3::splat(CHICK_FRESH_EGG_VISUAL_SCALE),
        );
        assert_eq!(CHICK_FRESH_EGG_SIZE_MULTIPLIER, 3.0);
        assert_eq!(
            CHICK_FRESH_EGG_VISUAL_SCALE,
            CHICK_FRESH_EGG_BASE_VISUAL_SCALE * 3.0
        );
        assert_eq!(CHICK_FRESH_EGG_RADIUS, CHICK_FRESH_EGG_BASE_RADIUS * 3.0);
    }

    #[test]
    fn fresh_egg_ride_follows_owner_with_mount_offset() {
        let mut skill = active_chick_skill(
            ChickSkillKind::FreshEggRide,
            entity(1),
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::ZERO,
            2.0,
        );
        let owner_action = FighterActionState {
            technique_id: Some(TechniqueId::ChickJumpHeavy),
            ..default()
        };
        let owner_transform = Transform::from_translation(Vec3::new(3.0, 1.0, -2.0));
        let mut transform = Transform::default();

        update_fresh_egg_ride_motion(
            &mut skill,
            &mut transform,
            Some((&owner_action, &owner_transform)),
        );

        assert_vec3_close(
            transform.translation,
            Vec3::new(3.0, 1.0, -2.0)
                + Vec3::X * CHICK_FRESH_EGG_RIDE_FORWARD_OFFSET * 2.0
                + Vec3::Y * CHICK_FRESH_EGG_RIDE_VERTICAL_OFFSET * 2.0,
        );

        skill.age = CHICK_FRESH_EGG_RIDE_LIFETIME * 0.25;
        update_fresh_egg_ride_motion(
            &mut skill,
            &mut transform,
            Some((&owner_action, &owner_transform)),
        );

        assert_vec3_close(
            transform.translation,
            fresh_egg_ride_position(owner_transform.translation, Vec3::X, 2.0, skill.age),
        );
    }

    #[test]
    fn fresh_egg_ride_expires_when_owner_missing_or_exits_jump_x() {
        let owner = entity(1);
        let owner_transform = Transform::from_translation(Vec3::ZERO);
        let owner_idle = FighterActionState::default();
        let mut transform = Transform::default();
        let mut missing_owner_skill = active_chick_skill(
            ChickSkillKind::FreshEggRide,
            owner,
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::ZERO,
            1.0,
        );
        let mut exited_action_skill = active_chick_skill(
            ChickSkillKind::FreshEggRide,
            owner,
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::ZERO,
            1.0,
        );

        update_fresh_egg_ride_motion(&mut missing_owner_skill, &mut transform, None);
        update_fresh_egg_ride_motion(
            &mut exited_action_skill,
            &mut transform,
            Some((&owner_idle, &owner_transform)),
        );

        assert_eq!(missing_owner_skill.lifetime, 0.0);
        assert_eq!(exited_action_skill.lifetime, 0.0);
    }

    #[test]
    fn orbit_egg_expires_when_owner_is_missing() {
        let mut skill = active_chick_skill(
            ChickSkillKind::OrbitEgg,
            entity(1),
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::ZERO,
            1.0,
        );
        let mut transform = Transform::default();

        update_orbit_egg_motion(&mut skill, &mut transform, None);

        assert_eq!(skill.lifetime, 0.0);
    }

    #[test]
    fn orbit_egg_recast_replaces_only_same_owner_orbit_egg() {
        let owner = entity(10);
        let other_owner = entity(11);
        let same_owner_orbit = entity(20);
        let same_owner_launch = entity(21);
        let same_owner_return = entity(22);
        let same_owner_shell = entity(23);
        let other_owner_orbit = entity(24);
        let other_owner_launch = entity(25);
        let other_owner_return = entity(26);
        let snapshots = [
            ActiveChickSkillSnapshot {
                entity: same_owner_orbit,
                owner,
                kind: ChickSkillKind::OrbitEgg,
                position: Vec3::new(1.0, 1.0, 0.0),
            },
            ActiveChickSkillSnapshot {
                entity: same_owner_launch,
                owner,
                kind: ChickSkillKind::OrbitEggLaunch,
                position: Vec3::new(1.5, 1.0, 0.0),
            },
            ActiveChickSkillSnapshot {
                entity: same_owner_return,
                owner,
                kind: ChickSkillKind::OrbitEggReturn,
                position: Vec3::new(1.75, 1.0, 0.0),
            },
            ActiveChickSkillSnapshot {
                entity: same_owner_shell,
                owner,
                kind: ChickSkillKind::ShellChip,
                position: Vec3::new(2.0, 1.0, 0.0),
            },
            ActiveChickSkillSnapshot {
                entity: other_owner_orbit,
                owner: other_owner,
                kind: ChickSkillKind::OrbitEgg,
                position: Vec3::new(3.0, 1.0, 0.0),
            },
            ActiveChickSkillSnapshot {
                entity: other_owner_launch,
                owner: other_owner,
                kind: ChickSkillKind::OrbitEggLaunch,
                position: Vec3::new(4.0, 1.0, 0.0),
            },
            ActiveChickSkillSnapshot {
                entity: other_owner_return,
                owner: other_owner,
                kind: ChickSkillKind::OrbitEggReturn,
                position: Vec3::new(5.0, 1.0, 0.0),
            },
        ];

        assert_eq!(
            owner_orbit_egg_replacements(owner, &snapshots),
            vec![same_owner_orbit, same_owner_launch, same_owner_return]
        );
    }

    #[test]
    fn orbit_egg_launch_uses_same_owner_orbit_position() {
        let owner = entity(10);
        let other_owner = entity(11);
        let owner_orbit_position = Vec3::new(2.5, 1.2, -0.75);
        let snapshots = [
            ActiveChickSkillSnapshot {
                entity: entity(20),
                owner: other_owner,
                kind: ChickSkillKind::OrbitEgg,
                position: Vec3::new(-4.0, 1.0, 0.0),
            },
            ActiveChickSkillSnapshot {
                entity: entity(21),
                owner,
                kind: ChickSkillKind::ShellChip,
                position: Vec3::new(0.0, 1.0, 0.0),
            },
            ActiveChickSkillSnapshot {
                entity: entity(22),
                owner,
                kind: ChickSkillKind::OrbitEgg,
                position: owner_orbit_position,
            },
        ];

        let launch = owner_orbit_egg_for_launch(owner, &snapshots).unwrap();

        assert_eq!(launch.entity, entity(22));
        assert_eq!(launch.position, owner_orbit_position);
        assert!(owner_orbit_egg_for_launch(entity(99), &snapshots).is_none());
    }

    #[test]
    fn orbit_egg_recall_uses_all_same_owner_launch_positions() {
        let owner = entity(10);
        let other_owner = entity(11);
        let first_owner_launch_position = Vec3::new(4.25, 1.1, -1.5);
        let second_owner_launch_position = Vec3::new(-1.25, 1.1, 2.0);
        let snapshots = [
            ActiveChickSkillSnapshot {
                entity: entity(20),
                owner: other_owner,
                kind: ChickSkillKind::OrbitEggLaunch,
                position: Vec3::new(-4.0, 1.0, 0.0),
            },
            ActiveChickSkillSnapshot {
                entity: entity(21),
                owner,
                kind: ChickSkillKind::OrbitEggReturn,
                position: Vec3::new(1.0, 1.0, 0.0),
            },
            ActiveChickSkillSnapshot {
                entity: entity(22),
                owner,
                kind: ChickSkillKind::OrbitEggLaunch,
                position: first_owner_launch_position,
            },
            ActiveChickSkillSnapshot {
                entity: entity(23),
                owner,
                kind: ChickSkillKind::OrbitEggLaunch,
                position: second_owner_launch_position,
            },
        ];

        let recall = owner_launched_orbit_eggs_for_recall(owner, &snapshots);

        assert_eq!(recall.len(), 2);
        assert_eq!(recall[0].entity, entity(22));
        assert_eq!(recall[0].position, first_owner_launch_position);
        assert_eq!(recall[1].entity, entity(23));
        assert_eq!(recall[1].position, second_owner_launch_position);
        assert!(owner_launched_orbit_eggs_for_recall(entity(99), &snapshots).is_empty());
    }

    #[test]
    fn ultimate_egg_burst_uses_sixteen_even_radial_directions() {
        let directions = ultimate_egg_burst_directions(Vec3::Z);

        assert_eq!(directions.len(), CHICK_ULTIMATE_EGG_COUNT);
        assert_eq!(CHICK_ULTIMATE_EGG_COUNT, 16);
        assert_vec3_close(directions[0], Vec3::Z);
        assert_vec3_close(directions[8], -Vec3::Z);
        assert_vec3_close(directions[4], -Vec3::X);
        let adjacent_dot = directions[0].dot(directions[1]);
        assert!((adjacent_dot - (TAU / CHICK_ULTIMATE_EGG_COUNT as f32).cos()).abs() < 0.001);
    }

    #[test]
    fn ultimate_orbit_eggs_use_four_second_launched_egg_control_profile() {
        let skill = ultimate_orbit_egg_skill(entity(1), 0, FighterStyleKind::Anchor, Vec3::X, 1.0);

        assert_eq!(skill.kind, ChickSkillKind::OrbitEggLaunch);
        assert_eq!(skill.payload_id, Some(AttackPayloadId::ChickOrbitEgg));
        assert_eq!(skill.lifetime, CHICK_ULTIMATE_EGG_LIFETIME);
        assert_eq!(CHICK_ULTIMATE_EGG_LIFETIME, 4.0);
        assert_eq!(skill.velocity, Vec3::X * CHICK_ORBIT_EGG_LAUNCH_SPEED);
    }

    #[test]
    fn orbit_egg_is_not_consumed_and_does_not_use_hit_memory() {
        let mut skill = active_chick_skill(
            ChickSkillKind::OrbitEgg,
            entity(1),
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::ZERO,
            1.0,
        );
        skill.already_hit.push(entity(2));

        assert!(!chick_skill_consumed_on_hit(skill.kind));
        assert!(!chick_skill_uses_hit_memory(skill.kind));
        assert!(chick_skill_uses_hit_memory(ChickSkillKind::ShellChip));
    }

    #[test]
    fn launched_orbit_egg_uses_soft_payload_and_hit_memory() {
        let skill = active_chick_skill(
            ChickSkillKind::OrbitEggLaunch,
            entity(1),
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::ZERO,
            1.0,
        );
        let payload = attack_payload_definition(skill.payload_id.unwrap());

        assert_eq!(skill.payload_id, Some(AttackPayloadId::ChickOrbitEgg));
        assert_eq!(
            payload.reaction_family,
            ReactionFamilyId::ShortStandingStagger
        );
        assert_eq!(skill.guard_stamina_damage, 2.0);
        assert!(!chick_skill_consumed_on_hit(skill.kind));
        assert!(chick_skill_uses_hit_memory(skill.kind));
    }

    #[test]
    fn returning_orbit_egg_uses_hard_payload_and_hit_memory() {
        let skill = active_chick_skill(
            ChickSkillKind::OrbitEggReturn,
            entity(1),
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::ZERO,
            1.0,
        );
        let payload = attack_payload_definition(skill.payload_id.unwrap());

        assert_eq!(skill.payload_id, Some(AttackPayloadId::ChickOrbitEggLaunch));
        assert_eq!(payload.reaction_family, ReactionFamilyId::SlidingKnockdown);
        assert_eq!(skill.guard_stamina_damage, 10.0);
        assert_eq!(
            chick_skill_visual_scale(ChickSkillKind::OrbitEggReturn, 1.0, skill.age),
            Vec3::splat(0.44 * CHICK_ORBIT_EGG_VISUAL_SCALE)
        );
        assert!(!chick_skill_consumed_on_hit(skill.kind));
        assert!(chick_skill_uses_hit_memory(skill.kind));
    }

    #[test]
    fn returning_orbit_egg_homes_to_owner_and_resumes_orbit() {
        let owner = entity(1);
        let owner_position = Vec3::new(2.0, 0.0, -1.0);
        let expected_anchor = orbit_egg_position(owner_position, Vec3::X, 1.0, 0.0);
        let mut skill = active_chick_skill(
            ChickSkillKind::OrbitEggReturn,
            owner,
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::ZERO,
            1.0,
        );
        skill.already_hit.push(entity(2));
        let mut transform = Transform::from_translation(expected_anchor - Vec3::new(2.0, 0.0, 0.0));

        update_orbit_egg_return_motion(&mut skill, &mut transform, Some(owner_position), 0.05);

        assert_eq!(skill.kind, ChickSkillKind::OrbitEggReturn);
        assert!(transform.translation.x > expected_anchor.x - 2.0);

        update_orbit_egg_return_motion(&mut skill, &mut transform, Some(owner_position), 0.2);

        assert_eq!(skill.kind, ChickSkillKind::OrbitEgg);
        assert_eq!(skill.lifetime, CHICK_ORBIT_EGG_LIFETIME);
        assert_eq!(skill.payload_id, Some(AttackPayloadId::ChickOrbitEgg));
        assert!(skill.already_hit.is_empty());
        assert_vec3_close(transform.translation, expected_anchor);
    }

    #[test]
    fn fresh_egg_drop_cracks_only_after_reaching_ground() {
        let mut skill = active_chick_skill(
            ChickSkillKind::FreshEggDrop,
            entity(1),
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::ZERO,
            1.0,
        );
        skill.age = 0.2;

        assert!(!fresh_egg_drop_touched_ground(
            &skill,
            Vec3::new(2.0, ARENA_TOP_Y + 0.5, 0.0)
        ));
        assert!(fresh_egg_drop_touched_ground(
            &skill,
            Vec3::new(2.0, ARENA_TOP_Y + 0.04, 0.0)
        ));
    }

    #[test]
    fn fresh_egg_ride_is_visual_only_and_fresh_egg_drop_still_attacks() {
        let state = MatchState::default();
        let owner = entity(1);
        let target = entity(2);
        let mut drop = active_chick_skill(
            ChickSkillKind::FreshEggDrop,
            owner,
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::ZERO,
            1.0,
        );
        let mut ride = active_chick_skill(
            ChickSkillKind::FreshEggRide,
            owner,
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::ZERO,
            1.0,
        );
        drop.age = 0.2;
        ride.age = 0.2;

        assert_eq!(drop.payload_id, Some(AttackPayloadId::ChickFreshEggDrop));
        assert_eq!(ride.payload_id, None);
        assert!(chick_skill_can_hit_target(&drop, target, 1, &state));
        assert!(!chick_skill_can_hit_target(&ride, target, 1, &state));
        assert!(chick_skill_consumed_on_hit(drop.kind));
        assert!(!chick_skill_consumed_on_hit(ride.kind));
        assert!(chick_skill_uses_hit_memory(drop.kind));
        assert!(!chick_skill_uses_hit_memory(ride.kind));
        assert!(fresh_egg_drop_touched_ground(
            &drop,
            Vec3::new(0.0, ARENA_TOP_Y + 0.04, 0.0)
        ));
        assert!(!fresh_egg_drop_touched_ground(
            &ride,
            Vec3::new(0.0, ARENA_TOP_Y + 0.04, 0.0)
        ));
        assert!(!chick_projectile_touched_ground(
            &ride,
            Vec3::new(0.0, ARENA_TOP_Y + 0.04, 0.0)
        ));
        assert!(!chick_skill_overlaps_target(
            &ride,
            Vec3::ZERO,
            &Transform::from_translation(Vec3::ZERO)
        ));
    }

    #[test]
    fn hazard_repeat_window_clears_contact_memory() {
        let mut skill = active_chick_skill(
            ChickSkillKind::OmeletField,
            entity(1),
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::ZERO,
            1.0,
        );
        skill.already_hit.push(entity(2));
        skill.age = CHICK_OMELET_FIELD_TICK;
        let mut effects = HitEffects::default();

        update_skill_repeat_window(&mut skill, &mut effects);

        assert!(skill.already_hit.is_empty());
        assert!(skill.next_repeat.unwrap() > CHICK_OMELET_FIELD_TICK);
    }

    #[test]
    fn chick_skill_lock_target_uses_aimed_front_enemy() {
        let mut state = MatchState::default();
        state.active_slots = [true, true, true, false];
        state.active_fighter_count = 3;
        let targets = [
            BeeSkillTargetSnapshot {
                entity: entity(1),
                fighter_id: 1,
                position: Vec3::new(3.0, 0.0, 0.0),
            },
            BeeSkillTargetSnapshot {
                entity: entity(2),
                fighter_id: 2,
                position: Vec3::new(-2.0, 0.0, 0.0),
            },
        ];

        assert_eq!(
            chick_skill_lock_target(0, Vec3::ZERO, Vec3::X, true, &state, &targets),
            Some(entity(1))
        );
        assert_eq!(
            chick_skill_lock_target(0, Vec3::ZERO, Vec3::X, false, &state, &targets),
            None
        );
    }

    #[test]
    fn mushroom_size_scale_enlarges_chick_collision_and_visuals() {
        let size_scale = crate::constants::ITEM_GIANT_SIZE_MULTIPLIER;
        let skill = active_chick_skill(
            ChickSkillKind::ShellChip,
            entity(1),
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::ZERO,
            size_scale,
        );

        assert_eq!(skill.size_scale, size_scale);
        assert_eq!(skill.radius, CHICK_SHELL_CHIP_RADIUS * size_scale);
        assert_vec3_close(
            chick_skill_visual_scale(ChickSkillKind::ShellChip, size_scale, 0.0),
            Vec3::splat(0.38 * size_scale),
        );
        assert_vec3_close(
            chick_skill_visual_scale(ChickSkillKind::OmeletField, size_scale, 0.25),
            Vec3::splat(omelet_field_visual_pulse(0.25) * size_scale),
        );
    }
}
