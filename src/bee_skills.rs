use bevy::prelude::*;

use crate::arena::ground_height_at;
use crate::arena_defs::active_arena_definition;
use crate::combat::{
    DamageDefenderProfile, HitEffects, ImpactSource, apply_impact, can_receive_impact,
    impact_profile_from_payload_with_feel,
};
use crate::components::{Fighter, FighterActionState, FighterMotor, FighterStats};
use crate::constants::{ARENA_TOP_Y, FIGHTER_HEIGHT, FIGHTER_RADIUS, KENNEY_CUBE_PET_SCALE};
use crate::controller_haptics::CombatHapticQueue;
use crate::effects::{EffectAssets, FeedbackPackageId, spawn_feedback_package};
use crate::equipment::FighterEquipment;
use crate::feel::CombatFeelTuning;
use crate::game_state::{Hitstop, MatchState, MatchTelemetry};
use crate::styles::{FighterStyle, FighterStyleKind};
use crate::techniques::{AttackPayloadId, AttackShapeId, BeeSkillId};

const BEE_SKILL_LOCK_RANGE: f32 = 8.0;
const BEE_SKILL_LOCK_CONE_DOT: f32 = 0.70710677;
const BEE_WORKER_SPEED: f32 = 8.4;
const BEE_WORKER_LIFETIME: f32 = 0.78;
const BEE_WORKER_RADIUS: f32 = 0.3;
const BEE_HONEY_GLOB_SPEED: f32 = 7.8;
const BEE_HONEY_GLOB_LIFT: f32 = 1.35;
const BEE_HONEY_GLOB_GRAVITY: f32 = 9.5;
const BEE_HONEY_GLOB_LIFETIME: f32 = 1.15;
const BEE_HONEY_GLOB_RADIUS: f32 = 0.42;
const BEE_HONEY_PUDDLE_LIFETIME: f32 = 2.4;
const BEE_HONEY_PUDDLE_RADIUS: f32 = 0.68;
const BEE_HONEY_PUDDLE_TICK: f32 = 0.45;
const BEE_HONEY_PUDDLE_DAMPING: f32 = 0.45;
const BEE_HONEY_PUDDLE_VERTICAL_REACH: f32 = 0.38;
const BEE_HOMING_SPEED: f32 = 11.2;
const BEE_HOMING_TURN_RATE: f32 = 12.0;
const BEE_HOMING_LIFETIME: f32 = 1.05;
const BEE_HOMING_RADIUS: f32 = 0.34;
const BEE_ULTIMATE_SWARM_LIFETIME: f32 = 2.4;
const BEE_ULTIMATE_SWARM_RADIUS: f32 = 2.0;
const BEE_ULTIMATE_SWARM_TICK: f32 = 0.3;
const BEE_ULTIMATE_SWARM_VERTICAL_REACH: f32 = 1.7;
const BEE_ULTIMATE_SWARM_GUARD_DAMAGE: f32 = 5.0;
const BEE_ULTIMATE_SWARM_CENTER_OFFSET: f32 = 2.4;
const BEE_ULTIMATE_SWARM_BEE_COUNT: usize = 5;
const BEE_ULTIMATE_SWARM_ORBIT_RADIUS: f32 = 1.35;
const BEE_ULTIMATE_SWARM_ORBIT_SPEED: f32 = 14.0;
const BEE_ULTIMATE_SWARM_BOB_HEIGHT: f32 = 0.28;
const BEE_ULTIMATE_SWARM_BOB_SPEED: f32 = 22.0;
const BEE_ULTIMATE_SWARM_BEE_HEIGHT: f32 = 1.04;
const BEE_ULTIMATE_SWARM_BEE_SCALE: f32 = 0.22;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BeeSkillKind {
    WorkerBee,
    HoneyGlob,
    HoneyPuddle,
    HomingSting,
    UltimateSwarm,
}

#[derive(Clone, Copy, Debug)]
pub struct BeeSkillTargetSnapshot {
    pub entity: Entity,
    pub fighter_id: usize,
    pub position: Vec3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BeeSkillSpawnMode {
    Standard,
    AreaSwarm,
}

#[derive(Component)]
pub struct ActiveBeeSkill {
    pub kind: BeeSkillKind,
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
pub struct BeeSkillAssets {
    worker_mesh: Handle<Mesh>,
    worker_material: Handle<StandardMaterial>,
    homing_mesh: Handle<Mesh>,
    homing_material: Handle<StandardMaterial>,
    honey_scene: Handle<Scene>,
    honey_puddle_mesh: Handle<Mesh>,
    honey_puddle_material: Handle<StandardMaterial>,
    ultimate_swarm_mesh: Handle<Mesh>,
    ultimate_swarm_material: Handle<StandardMaterial>,
    ultimate_swarm_bee_scene: Handle<Scene>,
}

#[derive(Component)]
pub(crate) struct BeeSwarmOrbiter {
    index: usize,
    age: f32,
}

pub fn setup_bee_skill_assets(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(BeeSkillAssets {
        worker_mesh: meshes.add(Sphere::new(0.16).mesh().uv(12, 6)),
        worker_material: materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.78, 0.1),
            emissive: LinearRgba::rgb(0.25, 0.12, 0.0),
            ..default()
        }),
        homing_mesh: meshes.add(Capsule3d::new(0.12, 0.42)),
        homing_material: materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.9, 0.22),
            emissive: LinearRgba::rgb(0.32, 0.2, 0.02),
            ..default()
        }),
        honey_scene: asset_server.load(GltfAssetLabel::Scene(0).from_asset(BEE_HONEY_ASSET)),
        honey_puddle_mesh: meshes.add(Cylinder::new(BEE_HONEY_PUDDLE_RADIUS, 0.045)),
        honey_puddle_material: materials.add(StandardMaterial {
            base_color: Color::srgba(1.0, 0.62, 0.05, 0.78),
            emissive: LinearRgba::rgb(0.22, 0.08, 0.0),
            alpha_mode: AlphaMode::Blend,
            ..default()
        }),
        ultimate_swarm_mesh: meshes.add(Cylinder::new(BEE_ULTIMATE_SWARM_RADIUS, 0.05)),
        ultimate_swarm_material: materials.add(StandardMaterial {
            base_color: Color::srgba(1.0, 0.84, 0.08, 0.28),
            emissive: LinearRgba::rgb(0.26, 0.18, 0.02),
            alpha_mode: AlphaMode::Blend,
            ..default()
        }),
        ultimate_swarm_bee_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(BEE_SWARM_BEE_ASSET)),
    });
}

pub const BEE_HONEY_ASSET: &str = "food/kenney_food_kit/honey.glb";
pub const BEE_SWARM_BEE_ASSET: &str = "characters/kenney_cube_pets/animal-bee.glb";

pub fn spawn_bee_skill(
    commands: &mut Commands,
    assets: &BeeSkillAssets,
    effect_assets: &EffectAssets,
    state: &MatchState,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    origin: Vec3,
    facing: Vec3,
    aim_held: bool,
    owner_size_scale: f32,
    spawn_mode: BeeSkillSpawnMode,
    skill: BeeSkillId,
    targets: &[BeeSkillTargetSnapshot],
) {
    let facing = facing.normalize_or_zero();
    let size_scale = bee_skill_size_scale(owner_size_scale);
    let target = bee_skill_target_for_mode(
        spawn_mode, owner_id, origin, facing, aim_held, state, targets,
    );
    match skill {
        BeeSkillId::WorkerSwarm => {
            if spawn_mode == BeeSkillSpawnMode::AreaSwarm {
                let side_vec = bee_skill_side_vec(facing);
                for spread in [-0.9, -0.35, 0.35, 0.9] {
                    let spawn = origin
                        + (Vec3::Y * 1.05 + facing * 0.36 + side_vec * spread * 0.34) * size_scale;
                    let direction = (facing + side_vec * spread * 0.55).normalize_or_zero();
                    spawn_worker_bee(
                        commands,
                        assets,
                        effect_assets,
                        owner,
                        owner_id,
                        owner_style,
                        spawn,
                        direction,
                        None,
                        size_scale,
                    );
                }
            } else {
                for side in [-1.0, 1.0] {
                    let side_vec = bee_skill_side_vec(facing) * side;
                    let spawn =
                        origin + (Vec3::Y * 1.05 + facing * 0.45 + side_vec * 0.28) * size_scale;
                    let direction = target
                        .and_then(|entity| target_position(entity, targets))
                        .map(|position| flat_direction(spawn, position))
                        .filter(|direction| direction.length_squared() > 0.01)
                        .unwrap_or_else(|| (facing + side_vec * 0.18).normalize_or_zero());
                    spawn_worker_bee(
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
            }
        }
        BeeSkillId::HoneyGlob => {
            if spawn_mode == BeeSkillSpawnMode::AreaSwarm {
                let side_vec = bee_skill_side_vec(facing);
                for spread in [-0.55, 0.55] {
                    let spawn = origin
                        + (Vec3::Y * 1.05 + facing * 0.45 + side_vec * spread * 0.34) * size_scale;
                    let direction = (facing + side_vec * spread * 0.35).normalize_or_zero();
                    spawn_honey_glob(
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
            } else {
                let spawn = origin + (Vec3::Y * 1.05 + facing * 0.5) * size_scale;
                spawn_honey_glob(
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
        BeeSkillId::HomingSting => {
            if spawn_mode == BeeSkillSpawnMode::AreaSwarm {
                let side_vec = bee_skill_side_vec(facing);
                for spread in [-0.6, 0.0, 0.6] {
                    let spawn = origin
                        + (Vec3::Y * 0.98 + facing * 0.58 + side_vec * spread * 0.26) * size_scale;
                    let direction = (facing + side_vec * spread * 0.62).normalize_or_zero();
                    spawn_homing_sting(
                        commands,
                        assets,
                        effect_assets,
                        owner,
                        owner_id,
                        owner_style,
                        spawn,
                        direction,
                        None,
                        size_scale,
                    );
                }
            } else {
                let spawn = origin + (Vec3::Y * 0.98 + facing * 0.6) * size_scale;
                let direction = target
                    .and_then(|entity| target_position(entity, targets))
                    .map(|position| flat_direction(spawn, position))
                    .filter(|direction| direction.length_squared() > 0.01)
                    .unwrap_or(facing);
                spawn_homing_sting(
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
        }
        BeeSkillId::UltimateSwarm => {
            spawn_ultimate_swarm(
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
    }
}

fn spawn_worker_bee(
    commands: &mut Commands,
    assets: &BeeSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    direction: Vec3,
    target: Option<Entity>,
    size_scale: f32,
) {
    let facing = direction.normalize_or_zero();
    commands.spawn((
        Mesh3d(assets.worker_mesh.clone()),
        MeshMaterial3d(assets.worker_material.clone()),
        Transform::from_translation(position).with_scale(bee_skill_visual_scale(
            BeeSkillKind::WorkerBee,
            size_scale,
            0.0,
        )),
        active_bee_skill(
            BeeSkillKind::WorkerBee,
            owner,
            owner_id,
            owner_style,
            AttackPayloadId::BeeWorkerSting,
            facing,
            facing * BEE_WORKER_SPEED,
            target,
            size_scale,
        ),
        Name::new("Bee worker assist"),
    ));
    spawn_feedback_package(
        commands,
        effect_assets,
        position,
        facing,
        FeedbackPackageId::SpecialProjectileStartup,
    );
}

fn spawn_honey_glob(
    commands: &mut Commands,
    assets: &BeeSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let velocity =
        facing.normalize_or_zero() * BEE_HONEY_GLOB_SPEED + Vec3::Y * BEE_HONEY_GLOB_LIFT;
    commands.spawn((
        SceneRoot(assets.honey_scene.clone()),
        Transform::from_translation(position).with_scale(bee_skill_visual_scale(
            BeeSkillKind::HoneyGlob,
            size_scale,
            0.0,
        )),
        active_bee_skill(
            BeeSkillKind::HoneyGlob,
            owner,
            owner_id,
            owner_style,
            AttackPayloadId::BeeHoneyGlob,
            facing,
            velocity,
            None,
            size_scale,
        ),
        Name::new("Bee honey glob"),
    ));
    spawn_feedback_package(
        commands,
        effect_assets,
        position,
        facing,
        FeedbackPackageId::SpecialProjectileStartup,
    );
}

fn spawn_homing_sting(
    commands: &mut Commands,
    assets: &BeeSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    direction: Vec3,
    target: Option<Entity>,
    size_scale: f32,
) {
    let facing = direction.normalize_or_zero();
    commands.spawn((
        Mesh3d(assets.homing_mesh.clone()),
        MeshMaterial3d(assets.homing_material.clone()),
        Transform::from_translation(position)
            .with_rotation(projectile_rotation(facing))
            .with_scale(bee_skill_visual_scale(
                BeeSkillKind::HomingSting,
                size_scale,
                0.0,
            )),
        active_bee_skill(
            BeeSkillKind::HomingSting,
            owner,
            owner_id,
            owner_style,
            AttackPayloadId::BeeHomingSting,
            facing,
            facing * BEE_HOMING_SPEED,
            target,
            size_scale,
        ),
        Name::new("Bee homing sting"),
    ));
    spawn_feedback_package(
        commands,
        effect_assets,
        position,
        facing,
        FeedbackPackageId::SpecialProjectileStartup,
    );
}

fn spawn_honey_puddle(
    commands: &mut Commands,
    assets: &BeeSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let ground = ground_height_at(position.x, position.z).unwrap_or(ARENA_TOP_Y);
    let position = Vec3::new(position.x, ground + 0.035, position.z);
    commands.spawn((
        Mesh3d(assets.honey_puddle_mesh.clone()),
        MeshMaterial3d(assets.honey_puddle_material.clone()),
        Transform::from_translation(position).with_scale(bee_skill_visual_scale(
            BeeSkillKind::HoneyPuddle,
            size_scale,
            0.0,
        )),
        active_bee_skill(
            BeeSkillKind::HoneyPuddle,
            owner,
            owner_id,
            owner_style,
            AttackPayloadId::BeeHoneyPuddle,
            facing,
            Vec3::ZERO,
            None,
            size_scale,
        ),
        Name::new("Bee honey puddle"),
    ));
    spawn_feedback_package(
        commands,
        effect_assets,
        position,
        facing,
        FeedbackPackageId::SpecialHazardStartup,
    );
}

fn spawn_ultimate_swarm(
    commands: &mut Commands,
    assets: &BeeSkillAssets,
    effect_assets: &EffectAssets,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    origin: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let center = bee_ultimate_swarm_center(origin, facing, size_scale);
    let facing = facing.normalize_or_zero();
    commands
        .spawn((
            Mesh3d(assets.ultimate_swarm_mesh.clone()),
            MeshMaterial3d(assets.ultimate_swarm_material.clone()),
            Transform::from_translation(center).with_scale(bee_skill_visual_scale(
                BeeSkillKind::UltimateSwarm,
                size_scale,
                0.0,
            )),
            active_bee_skill(
                BeeSkillKind::UltimateSwarm,
                owner,
                owner_id,
                owner_style,
                AttackPayloadId::BeeUltimateSwarmTick,
                facing,
                Vec3::ZERO,
                None,
                size_scale,
            ),
            Name::new("Bee ultimate swarm field"),
        ))
        .with_children(|parent| {
            for index in 0..BEE_ULTIMATE_SWARM_BEE_COUNT {
                parent.spawn((
                    SceneRoot(assets.ultimate_swarm_bee_scene.clone()),
                    bee_swarm_orbiter_transform(index, 0.0),
                    BeeSwarmOrbiter { index, age: 0.0 },
                    Name::new("Bee ultimate mini bee"),
                ));
            }
        });
    spawn_feedback_package(
        commands,
        effect_assets,
        center,
        facing,
        FeedbackPackageId::SpecialHazardStartup,
    );
}

fn active_bee_skill(
    kind: BeeSkillKind,
    owner: Entity,
    owner_id: usize,
    owner_style: FighterStyleKind,
    payload_id: AttackPayloadId,
    facing: Vec3,
    velocity: Vec3,
    target: Option<Entity>,
    size_scale: f32,
) -> ActiveBeeSkill {
    let size_scale = bee_skill_size_scale(size_scale);
    let (shape_id, source, lifetime, radius, guard_stamina_damage, repeat_interval) = match kind {
        BeeSkillKind::WorkerBee => (
            AttackShapeId::ProjectileBolt,
            ImpactSource::Projectile,
            BEE_WORKER_LIFETIME,
            BEE_WORKER_RADIUS,
            8.0,
            None,
        ),
        BeeSkillKind::HoneyGlob => (
            AttackShapeId::ProjectileBolt,
            ImpactSource::Projectile,
            BEE_HONEY_GLOB_LIFETIME,
            BEE_HONEY_GLOB_RADIUS,
            12.0,
            None,
        ),
        BeeSkillKind::HoneyPuddle => (
            AttackShapeId::HazardField,
            ImpactSource::Hazard,
            BEE_HONEY_PUDDLE_LIFETIME,
            BEE_HONEY_PUDDLE_RADIUS,
            6.0,
            Some(BEE_HONEY_PUDDLE_TICK),
        ),
        BeeSkillKind::HomingSting => (
            AttackShapeId::ProjectileBolt,
            ImpactSource::Projectile,
            BEE_HOMING_LIFETIME,
            BEE_HOMING_RADIUS,
            14.0,
            None,
        ),
        BeeSkillKind::UltimateSwarm => (
            AttackShapeId::HazardField,
            ImpactSource::Hazard,
            BEE_ULTIMATE_SWARM_LIFETIME,
            BEE_ULTIMATE_SWARM_RADIUS,
            BEE_ULTIMATE_SWARM_GUARD_DAMAGE,
            Some(BEE_ULTIMATE_SWARM_TICK),
        ),
    };

    ActiveBeeSkill {
        kind,
        owner,
        owner_id,
        owner_style,
        payload_id,
        shape_id,
        source,
        facing: facing.normalize_or_zero(),
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

fn bee_skill_size_scale(owner_size_scale: f32) -> f32 {
    owner_size_scale.max(0.1)
}

fn bee_skill_visual_scale(kind: BeeSkillKind, size_scale: f32, age: f32) -> Vec3 {
    let size_scale = bee_skill_size_scale(size_scale);
    match kind {
        BeeSkillKind::WorkerBee => Vec3::new(1.25, 0.75, 0.75) * size_scale,
        BeeSkillKind::HoneyGlob => Vec3::splat(0.38 * size_scale),
        BeeSkillKind::HoneyPuddle => Vec3::splat(honey_puddle_visual_pulse(age) * size_scale),
        BeeSkillKind::HomingSting => Vec3::splat(size_scale),
        BeeSkillKind::UltimateSwarm => Vec3::splat(ultimate_swarm_visual_pulse(age) * size_scale),
    }
}

fn honey_puddle_visual_pulse(age: f32) -> f32 {
    0.95 + (age * 8.0).sin().abs() * 0.12
}

fn ultimate_swarm_visual_pulse(age: f32) -> f32 {
    0.96 + (age * 12.0).sin().abs() * 0.08
}

pub fn update_bee_skills(
    time: Res<Time>,
    mut commands: Commands,
    assets: Res<BeeSkillAssets>,
    effect_assets: Res<EffectAssets>,
    state: Res<MatchState>,
    feel: Res<CombatFeelTuning>,
    mut hitstop: ResMut<Hitstop>,
    mut camera_effects: ResMut<HitEffects>,
    mut haptics: ResMut<CombatHapticQueue>,
    mut telemetry: ResMut<MatchTelemetry>,
    mut skills: Query<
        (Entity, &mut ActiveBeeSkill, &mut Transform),
        (Without<Fighter>, Without<BeeSwarmOrbiter>),
    >,
    mut orbiters: Query<
        (&mut BeeSwarmOrbiter, &mut Transform),
        (Without<ActiveBeeSkill>, Without<Fighter>),
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
        update_skill_motion(&mut skill, &mut transform, dt, &fighters.p0());

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
                if !bee_skill_can_hit_target(&skill, target_entity, target.id, &state) {
                    continue;
                }
                if skill.already_hit.contains(&target_entity)
                    || !can_receive_impact(&stats, &action)
                    || !bee_skill_overlaps_target(&skill, transform.translation, target_transform)
                {
                    continue;
                }

                let profile = bee_skill_impact_profile(&skill, &feel);
                apply_impact(
                    &mut commands,
                    &effect_assets,
                    &mut camera_effects,
                    &mut haptics,
                    &mut hitstop,
                    &state,
                    target.id,
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
                if skill.kind == BeeSkillKind::HoneyPuddle && motor.grounded {
                    motor.velocity.x *= BEE_HONEY_PUDDLE_DAMPING;
                    motor.velocity.z *= BEE_HONEY_PUDDLE_DAMPING;
                    camera_effects.push_feedback_cue("impact_special_hazard", skill.source, 24);
                }
                skill.already_hit.push(target_entity);
                hit_this_frame = true;

                if bee_skill_consumed_on_hit(skill.kind) {
                    skill.lifetime = 0.0;
                    break;
                }
            }
        }

        let glob_grounded = honey_glob_touched_ground(&skill, transform.translation);
        if skill.lifetime <= 0.0 || glob_grounded || should_despawn_skill(transform.translation) {
            if skill.kind == BeeSkillKind::HoneyGlob {
                spawn_honey_puddle(
                    &mut commands,
                    &assets,
                    &effect_assets,
                    skill.owner,
                    skill.owner_id,
                    skill.owner_style,
                    transform.translation,
                    skill.facing,
                    skill.size_scale,
                );
            } else if !hit_this_frame {
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

    update_bee_swarm_orbiters(dt, &mut orbiters);
}

fn bee_skill_consumed_on_hit(kind: BeeSkillKind) -> bool {
    matches!(
        kind,
        BeeSkillKind::WorkerBee | BeeSkillKind::HoneyGlob | BeeSkillKind::HomingSting
    )
}

fn update_bee_swarm_orbiters(
    dt: f32,
    orbiters: &mut Query<
        (&mut BeeSwarmOrbiter, &mut Transform),
        (Without<ActiveBeeSkill>, Without<Fighter>),
    >,
) {
    for (mut orbiter, mut transform) in orbiters {
        orbiter.age += dt;
        *transform = bee_swarm_orbiter_transform(orbiter.index, orbiter.age);
    }
}

fn update_skill_repeat_window(skill: &mut ActiveBeeSkill, effects: &mut HitEffects) {
    let Some(interval) = skill.repeat_interval else {
        return;
    };
    let Some(mut next_repeat) = skill.next_repeat else {
        return;
    };
    while skill.age >= next_repeat {
        skill.already_hit.clear();
        effects.push_feedback_cue("pulse_special_hazard", skill.source, 24);
        next_repeat += interval;
    }
    skill.next_repeat = Some(next_repeat);
}

fn update_skill_motion(
    skill: &mut ActiveBeeSkill,
    transform: &mut Transform,
    dt: f32,
    targets: &Query<(&Fighter, &Transform), With<Fighter>>,
) {
    match skill.kind {
        BeeSkillKind::HoneyPuddle => {
            transform.scale =
                bee_skill_visual_scale(BeeSkillKind::HoneyPuddle, skill.size_scale, skill.age);
        }
        BeeSkillKind::UltimateSwarm => {
            transform.scale =
                bee_skill_visual_scale(BeeSkillKind::UltimateSwarm, skill.size_scale, skill.age);
        }
        BeeSkillKind::HoneyGlob => {
            skill.velocity.y -= BEE_HONEY_GLOB_GRAVITY * dt;
            transform.translation += skill.velocity * dt;
            transform.rotate_y(0.12);
            transform.rotate_x(0.08);
        }
        BeeSkillKind::WorkerBee | BeeSkillKind::HomingSting => {
            if let Some(target_entity) = skill.target
                && let Ok((_, target_transform)) = targets.get(target_entity)
            {
                steer_skill_toward(
                    skill,
                    transform.translation,
                    target_transform.translation + Vec3::Y * 0.85,
                    dt,
                );
            }
            transform.translation += skill.velocity * dt;
            if skill.facing.length_squared() > 0.01 {
                transform.rotation = projectile_rotation(skill.facing);
            }
        }
    }
}

fn steer_skill_toward(
    skill: &mut ActiveBeeSkill,
    current_position: Vec3,
    target_position: Vec3,
    dt: f32,
) {
    let desired = (target_position - current_position).normalize_or_zero();
    if desired.length_squared() <= 0.01 {
        return;
    }
    let turn = match skill.kind {
        BeeSkillKind::HomingSting => BEE_HOMING_TURN_RATE,
        BeeSkillKind::WorkerBee => BEE_HOMING_TURN_RATE * 0.45,
        _ => 0.0,
    };
    let speed = skill.velocity.length();
    skill.velocity = skill
        .velocity
        .lerp(desired * speed, (dt * turn).clamp(0.0, 1.0));
    skill.facing = skill.velocity.normalize_or_zero();
}

fn bee_skill_impact_profile(
    skill: &ActiveBeeSkill,
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

fn bee_skill_overlaps_target(
    skill: &ActiveBeeSkill,
    origin: Vec3,
    target_transform: &Transform,
) -> bool {
    match skill.kind {
        BeeSkillKind::HoneyPuddle => {
            let target_position = target_transform.translation;
            let rendered_radius =
                skill.radius * honey_puddle_visual_pulse(skill.age) + FIGHTER_RADIUS;
            let horizontal_overlap = flat_distance(origin, target_position) <= rendered_radius;
            let vertical_overlap = (target_position.y - origin.y).abs()
                <= BEE_HONEY_PUDDLE_VERTICAL_REACH * skill.size_scale;

            horizontal_overlap && vertical_overlap
        }
        BeeSkillKind::UltimateSwarm => {
            let target_position = target_transform.translation;
            let horizontal_overlap = flat_distance(origin, target_position)
                <= skill.radius * ultimate_swarm_visual_pulse(skill.age) + FIGHTER_RADIUS;
            let vertical_overlap = (target_position.y - origin.y).abs()
                <= BEE_ULTIMATE_SWARM_VERTICAL_REACH * skill.size_scale;

            horizontal_overlap && vertical_overlap
        }
        _ => {
            let target = target_transform.translation + Vec3::Y * (FIGHTER_HEIGHT * 0.58);
            target.distance(origin) <= skill.radius + FIGHTER_RADIUS
        }
    }
}

pub fn bee_skill_lock_target(
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
    let facing = facing.normalize_or_zero();
    if facing.length_squared() <= 0.01 {
        return None;
    }

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
            if distance > BEE_SKILL_LOCK_RANGE || distance <= 0.01 {
                return None;
            }
            let direction = offset / distance;
            (direction.dot(facing) >= BEE_SKILL_LOCK_CONE_DOT).then_some((target.entity, distance))
        })
        .min_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(entity, _)| entity)
}

fn bee_skill_can_hit_target(
    skill: &ActiveBeeSkill,
    target_entity: Entity,
    target_id: usize,
    state: &MatchState,
) -> bool {
    target_entity != skill.owner
        && target_id != skill.owner_id
        && state.combat_target_allowed_for_state(skill.owner_id, target_id)
}

fn bee_skill_target_for_mode(
    spawn_mode: BeeSkillSpawnMode,
    owner_id: usize,
    origin: Vec3,
    facing: Vec3,
    aim_held: bool,
    state: &MatchState,
    targets: &[BeeSkillTargetSnapshot],
) -> Option<Entity> {
    match spawn_mode {
        BeeSkillSpawnMode::Standard => {
            bee_skill_lock_target(owner_id, origin, facing, aim_held, state, targets)
        }
        BeeSkillSpawnMode::AreaSwarm => None,
    }
}

fn bee_skill_side_vec(facing: Vec3) -> Vec3 {
    Vec3::new(-facing.z, 0.0, facing.x).normalize_or_zero()
}

fn bee_ultimate_swarm_center(origin: Vec3, facing: Vec3, size_scale: f32) -> Vec3 {
    let facing = facing.normalize_or_zero();
    let offset = facing * BEE_ULTIMATE_SWARM_CENTER_OFFSET * bee_skill_size_scale(size_scale);
    let position = origin + offset;
    let ground = ground_height_at(position.x, position.z).unwrap_or(ARENA_TOP_Y);
    Vec3::new(position.x, ground + 0.045, position.z)
}

fn bee_swarm_orbiter_transform(index: usize, age: f32) -> Transform {
    let phase = bee_swarm_orbiter_phase(index) + age * BEE_ULTIMATE_SWARM_ORBIT_SPEED;
    let offset = bee_swarm_orbiter_offset(index, age);
    let tangent = Vec3::new(-phase.sin(), 0.0, phase.cos()).normalize_or_zero();
    Transform::from_translation(offset)
        .with_rotation(projectile_rotation(tangent))
        .with_scale(Vec3::splat(
            BEE_ULTIMATE_SWARM_BEE_SCALE * KENNEY_CUBE_PET_SCALE,
        ))
}

fn bee_swarm_orbiter_offset(index: usize, age: f32) -> Vec3 {
    let phase = bee_swarm_orbiter_phase(index) + age * BEE_ULTIMATE_SWARM_ORBIT_SPEED;
    Vec3::new(
        phase.cos() * BEE_ULTIMATE_SWARM_ORBIT_RADIUS,
        BEE_ULTIMATE_SWARM_BEE_HEIGHT
            + (phase * 1.7 + age * BEE_ULTIMATE_SWARM_BOB_SPEED).sin()
                * BEE_ULTIMATE_SWARM_BOB_HEIGHT,
        phase.sin() * BEE_ULTIMATE_SWARM_ORBIT_RADIUS,
    )
}

fn bee_swarm_orbiter_phase(index: usize) -> f32 {
    std::f32::consts::TAU * index as f32 / BEE_ULTIMATE_SWARM_BEE_COUNT as f32
}

fn target_position(entity: Entity, targets: &[BeeSkillTargetSnapshot]) -> Option<Vec3> {
    targets
        .iter()
        .find(|target| target.entity == entity)
        .map(|target| target.position)
}

fn flat_direction(origin: Vec3, target: Vec3) -> Vec3 {
    Vec3::new(target.x - origin.x, 0.0, target.z - origin.z).normalize_or_zero()
}

fn flat_distance(a: Vec3, b: Vec3) -> f32 {
    Vec2::new(a.x - b.x, a.z - b.z).length()
}

fn honey_glob_touched_ground(skill: &ActiveBeeSkill, position: Vec3) -> bool {
    if skill.kind != BeeSkillKind::HoneyGlob {
        return false;
    }
    let ground = ground_height_at(position.x, position.z).unwrap_or(ARENA_TOP_Y);
    position.y <= ground + 0.08 && skill.age > 0.08
}

fn should_despawn_skill(position: Vec3) -> bool {
    let arena = active_arena_definition();
    position.y < arena.ringout_y
        || Vec2::new(position.x, position.z).length() > arena.ringout_radius
}

fn impact_package(kind: BeeSkillKind) -> FeedbackPackageId {
    match kind {
        BeeSkillKind::HoneyPuddle | BeeSkillKind::UltimateSwarm => {
            FeedbackPackageId::SpecialHazardImpact
        }
        _ => FeedbackPackageId::SpecialProjectileImpact,
    }
}

fn despawn_package(kind: BeeSkillKind) -> FeedbackPackageId {
    match kind {
        BeeSkillKind::HoneyPuddle | BeeSkillKind::UltimateSwarm => {
            FeedbackPackageId::SpecialHazardFade
        }
        _ => FeedbackPackageId::SpecialProjectileRecover,
    }
}

fn projectile_rotation(facing: Vec3) -> Quat {
    let facing = facing.normalize_or_zero();
    if facing.length_squared() > 0.01 {
        Quat::from_rotation_arc(Vec3::Z, facing)
    } else {
        Quat::IDENTITY
    }
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

    fn ffa_state() -> MatchState {
        let mut state = MatchState::default();
        state.rules = crate::game_state::RULE_PRESETS[1];
        state.rule_index = 1;
        state.active_slots = [true, true, true, false];
        state.active_fighter_count = 3;
        state
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
            bee_skill_lock_target(0, Vec3::ZERO, Vec3::X, true, &state, &targets),
            Some(entity(1))
        );
        assert_eq!(
            bee_skill_lock_target(0, Vec3::ZERO, Vec3::X, false, &state, &targets),
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
                fighter_id: 1,
                position: Vec3::new(3.0, 0.0, 0.0),
            },
            BeeSkillTargetSnapshot {
                entity: entity(2),
                fighter_id: 2,
                position: Vec3::new(2.0, 0.0, 0.0),
            },
        ];

        assert_eq!(
            bee_skill_lock_target(0, Vec3::ZERO, Vec3::X, true, &state, &targets),
            Some(entity(1))
        );
    }

    #[test]
    fn lock_target_ignores_owner_even_when_ffa_allows_self_targets() {
        let state = ffa_state();
        let targets = [
            BeeSkillTargetSnapshot {
                entity: entity(1),
                fighter_id: 0,
                position: Vec3::new(1.0, 0.0, 0.0),
            },
            BeeSkillTargetSnapshot {
                entity: entity(2),
                fighter_id: 1,
                position: Vec3::new(3.0, 0.0, 0.0),
            },
        ];

        assert!(state.combat_target_allowed_for_state(0, 0));
        assert_eq!(
            bee_skill_lock_target(0, Vec3::ZERO, Vec3::X, true, &state, &targets),
            Some(entity(2))
        );
    }

    #[test]
    fn bee_skill_hits_reject_owner_even_when_ffa_allows_self_targets() {
        let state = ffa_state();
        let skill = active_bee_skill(
            BeeSkillKind::UltimateSwarm,
            entity(1),
            0,
            FighterStyleKind::Anchor,
            AttackPayloadId::BeeUltimateSwarmTick,
            Vec3::X,
            Vec3::ZERO,
            None,
            1.0,
        );

        assert!(state.combat_target_allowed_for_state(0, 0));
        assert!(!bee_skill_can_hit_target(&skill, entity(1), 0, &state));
        assert!(!bee_skill_can_hit_target(&skill, entity(2), 0, &state));
        assert!(bee_skill_can_hit_target(&skill, entity(3), 1, &state));
    }

    #[test]
    fn area_swarm_mode_does_not_take_aim_lock_target() {
        let mut state = MatchState::default();
        state.active_slots = [true, true, false, false];
        state.active_fighter_count = 2;
        let targets = [BeeSkillTargetSnapshot {
            entity: entity(1),
            fighter_id: 1,
            position: Vec3::new(3.0, 0.0, 0.0),
        }];

        assert_eq!(
            bee_skill_target_for_mode(
                BeeSkillSpawnMode::Standard,
                0,
                Vec3::ZERO,
                Vec3::X,
                true,
                &state,
                &targets,
            ),
            Some(entity(1))
        );
        assert_eq!(
            bee_skill_target_for_mode(
                BeeSkillSpawnMode::AreaSwarm,
                0,
                Vec3::ZERO,
                Vec3::X,
                true,
                &state,
                &targets,
            ),
            None
        );
    }

    #[test]
    fn homing_velocity_turns_toward_captured_target() {
        let mut skill = active_bee_skill(
            BeeSkillKind::HomingSting,
            entity(1),
            0,
            FighterStyleKind::Anchor,
            AttackPayloadId::BeeHomingSting,
            Vec3::X,
            Vec3::X * BEE_HOMING_SPEED,
            Some(entity(2)),
            1.0,
        );

        steer_skill_toward(&mut skill, Vec3::ZERO, Vec3::new(0.0, 0.0, 3.0), 0.08);

        assert!(skill.velocity.z > 0.0);
        assert!(skill.velocity.x < BEE_HOMING_SPEED);
    }

    #[test]
    fn puddle_repeat_window_clears_contact_memory() {
        let mut skill = active_bee_skill(
            BeeSkillKind::HoneyPuddle,
            entity(1),
            0,
            FighterStyleKind::Anchor,
            AttackPayloadId::BeeHoneyPuddle,
            Vec3::X,
            Vec3::ZERO,
            None,
            1.0,
        );
        skill.already_hit.push(entity(2));
        skill.age = BEE_HONEY_PUDDLE_TICK;
        let mut effects = HitEffects::default();

        update_skill_repeat_window(&mut skill, &mut effects);

        assert!(skill.already_hit.is_empty());
        assert!(skill.next_repeat.unwrap() > BEE_HONEY_PUDDLE_TICK);
    }

    #[test]
    fn ultimate_swarm_uses_repeating_hazard_tuning() {
        let skill = active_bee_skill(
            BeeSkillKind::UltimateSwarm,
            entity(1),
            0,
            FighterStyleKind::Anchor,
            AttackPayloadId::BeeUltimateSwarmTick,
            Vec3::X,
            Vec3::ZERO,
            None,
            1.0,
        );

        assert_eq!(skill.payload_id, AttackPayloadId::BeeUltimateSwarmTick);
        assert_eq!(skill.shape_id, AttackShapeId::HazardField);
        assert_eq!(skill.source, ImpactSource::Hazard);
        assert_eq!(skill.lifetime, BEE_ULTIMATE_SWARM_LIFETIME);
        assert_eq!(skill.radius, BEE_ULTIMATE_SWARM_RADIUS);
        assert_eq!(skill.repeat_interval, Some(BEE_ULTIMATE_SWARM_TICK));
        assert_eq!(skill.next_repeat, Some(BEE_ULTIMATE_SWARM_TICK));
    }

    #[test]
    fn honey_puddle_overlap_requires_floor_level_contact() {
        let skill = active_bee_skill(
            BeeSkillKind::HoneyPuddle,
            entity(1),
            0,
            FighterStyleKind::Anchor,
            AttackPayloadId::BeeHoneyPuddle,
            Vec3::X,
            Vec3::ZERO,
            None,
            1.0,
        );
        let puddle_origin = Vec3::new(0.0, 0.035, 0.0);
        let hit_radius =
            BEE_HONEY_PUDDLE_RADIUS * honey_puddle_visual_pulse(skill.age) + FIGHTER_RADIUS;
        let grounded_target = Transform::from_translation(Vec3::new(hit_radius - 0.01, 0.0, 0.0));
        let outside_rendered_body_edge =
            Transform::from_translation(Vec3::new(hit_radius + 0.01, 0.0, 0.0));
        let airborne_target =
            Transform::from_translation(Vec3::new(0.0, FIGHTER_HEIGHT * 2.0, 0.0));

        assert!(bee_skill_overlaps_target(
            &skill,
            puddle_origin,
            &grounded_target
        ));
        assert!(!bee_skill_overlaps_target(
            &skill,
            puddle_origin,
            &outside_rendered_body_edge
        ));
        assert!(!bee_skill_overlaps_target(
            &skill,
            puddle_origin,
            &airborne_target
        ));
    }

    #[test]
    fn mushroom_size_scale_enlarges_bee_skill_collision_radii() {
        let size_scale = crate::constants::ITEM_GIANT_SIZE_MULTIPLIER;
        let cases = [
            (
                BeeSkillKind::WorkerBee,
                AttackPayloadId::BeeWorkerSting,
                BEE_WORKER_RADIUS,
            ),
            (
                BeeSkillKind::HoneyGlob,
                AttackPayloadId::BeeHoneyGlob,
                BEE_HONEY_GLOB_RADIUS,
            ),
            (
                BeeSkillKind::HoneyPuddle,
                AttackPayloadId::BeeHoneyPuddle,
                BEE_HONEY_PUDDLE_RADIUS,
            ),
            (
                BeeSkillKind::HomingSting,
                AttackPayloadId::BeeHomingSting,
                BEE_HOMING_RADIUS,
            ),
            (
                BeeSkillKind::UltimateSwarm,
                AttackPayloadId::BeeUltimateSwarmTick,
                BEE_ULTIMATE_SWARM_RADIUS,
            ),
        ];

        for (kind, payload, base_radius) in cases {
            let skill = active_bee_skill(
                kind,
                entity(1),
                0,
                FighterStyleKind::Anchor,
                payload,
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
    fn mushroom_size_scale_enlarges_bee_skill_visuals() {
        let size_scale = crate::constants::ITEM_GIANT_SIZE_MULTIPLIER;

        assert_vec3_close(
            bee_skill_visual_scale(BeeSkillKind::WorkerBee, size_scale, 0.0),
            Vec3::new(1.25, 0.75, 0.75) * size_scale,
        );
        assert_vec3_close(
            bee_skill_visual_scale(BeeSkillKind::HoneyGlob, size_scale, 0.0),
            Vec3::splat(0.38 * size_scale),
        );
        assert_vec3_close(
            bee_skill_visual_scale(BeeSkillKind::HomingSting, size_scale, 0.0),
            Vec3::splat(size_scale),
        );
        assert_vec3_close(
            bee_skill_visual_scale(BeeSkillKind::HoneyPuddle, size_scale, 0.25),
            Vec3::splat(honey_puddle_visual_pulse(0.25) * size_scale),
        );
        assert_vec3_close(
            bee_skill_visual_scale(BeeSkillKind::UltimateSwarm, size_scale, 0.25),
            Vec3::splat(ultimate_swarm_visual_pulse(0.25) * size_scale),
        );
    }

    #[test]
    fn ultimate_swarm_orbiters_are_evenly_phased() {
        let first = bee_swarm_orbiter_offset(0, 0.0);
        assert_vec3_close(
            first,
            Vec3::new(
                BEE_ULTIMATE_SWARM_ORBIT_RADIUS,
                BEE_ULTIMATE_SWARM_BEE_HEIGHT,
                0.0,
            ),
        );

        for index in 0..BEE_ULTIMATE_SWARM_BEE_COUNT {
            let offset = bee_swarm_orbiter_offset(index, 0.0);
            let horizontal_radius = Vec2::new(offset.x, offset.z).length();
            assert!(
                (horizontal_radius - BEE_ULTIMATE_SWARM_ORBIT_RADIUS).abs() <= 0.0001,
                "orbiter {index} radius was {horizontal_radius}"
            );
        }

        let moved = bee_swarm_orbiter_offset(0, 0.1);
        assert!(moved.z.abs() > 0.1);
    }

    #[test]
    fn honey_asset_exists_for_runtime_loading() {
        assert!(std::path::Path::new("assets/food/kenney_food_kit/honey.glb").exists());
        assert!(std::path::Path::new("assets/characters/kenney_cube_pets/animal-bee.glb").exists());
    }
}
