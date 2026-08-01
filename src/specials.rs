use bevy::prelude::*;

use crate::arena::ground_height_at;
use crate::arena_defs::active_arena_definition;
use crate::combat::{
    DamageDefenderProfile, HitEffects, ImpactProfile, ImpactSource, apply_impact,
    can_receive_impact, impact_profile_from_payload, impact_profile_from_payload_with_feel,
    radial_falloff,
};
use crate::components::{
    Fighter, FighterAction, FighterActionState, FighterInput, FighterMotor, FighterSpecialState,
    FighterStats, SpecialInputKind,
};
use crate::constants::*;
use crate::controller_haptics::CombatHapticQueue;
use crate::effects::{EffectAssets, FeedbackPackageId, spawn_feedback_package};
use crate::equipment::{
    FighterEquipment, LoadoutContext, loadout_special_cooldown_scale, loadout_special_cost_scale,
    loadout_special_stamina_disrupt,
};
use crate::feel::CombatFeelTuning;
use crate::fighter::cancel_dash_slide_for_action;
use crate::game_state::{Hitstop, MatchState, MatchTelemetry};
use crate::styles::{FighterStyle, FighterStyleKind};
use crate::techniques::{
    AttackPayloadId, AttackShapeId, MsTimingWindow, attack_payload_definition, elapsed_secs_to_ms,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpecialKind {
    Projectile,
    Trap,
    Shockwave,
    Hazard,
}

#[derive(Clone, Copy)]
struct SpecialDefinition {
    #[allow(dead_code)]
    cost: f32,
    lifetime: f32,
    radius: f32,
    payload_id: AttackPayloadId,
    shape_id: AttackShapeId,
    source: ImpactSource,
    timing: SpecialTimingDef,
    presentation: SpecialPresentationDef,
    guard_stamina_damage: f32,
}

#[derive(Clone, Copy)]
struct SpecialTimingDef {
    launch_ms: u32,
    active_window: MsTimingWindow,
    repeat_ms: Option<u32>,
    aftermath_ms: Option<u32>,
    startup_cue: &'static str,
    active_cue: &'static str,
    aftermath_cue: &'static str,
}

#[derive(Clone, Copy)]
struct SpecialPresentationDef {
    startup_package: FeedbackPackageId,
    active_package: FeedbackPackageId,
    repeat_package: Option<FeedbackPackageId>,
    impact_package: FeedbackPackageId,
    aftermath_package: FeedbackPackageId,
    despawn_package: FeedbackPackageId,
}

#[derive(Component)]
pub struct ActiveSpecial {
    pub kind: SpecialKind,
    pub owner: Entity,
    pub owner_id: usize,
    pub owner_style: FighterStyleKind,
    pub payload_id: AttackPayloadId,
    pub shape_id: AttackShapeId,
    pub source: ImpactSource,
    pub facing: Vec3,
    pub velocity: Vec3,
    pub lifetime: f32,
    pub age: f32,
    pub total_lifetime_ms: u32,
    pub radius: f32,
    pub grace: f32,
    pub launch_ms: u32,
    pub active_window: MsTimingWindow,
    pub repeat_ms: Option<u32>,
    pub next_repeat_ms: Option<u32>,
    pub active_feedback_sent: bool,
    pub aftermath_ms: Option<u32>,
    pub aftermath_feedback_sent: bool,
    pub active_cue: &'static str,
    pub aftermath_cue: &'static str,
    pub active_package: FeedbackPackageId,
    pub repeat_package: Option<FeedbackPackageId>,
    pub impact_package: FeedbackPackageId,
    pub aftermath_package: FeedbackPackageId,
    pub despawn_package: FeedbackPackageId,
    pub stamina_disrupt: f32,
    pub guard_stamina_damage: f32,
    pub already_hit: Vec<Entity>,
}

#[derive(Resource)]
pub struct SpecialAssets {
    projectile_mesh: Handle<Mesh>,
    trap_mesh: Handle<Mesh>,
    shockwave_mesh: Handle<Mesh>,
    hazard_mesh: Handle<Mesh>,
    projectile_material: Handle<StandardMaterial>,
    trap_material: Handle<StandardMaterial>,
    shockwave_material: Handle<StandardMaterial>,
    hazard_material: Handle<StandardMaterial>,
}

pub fn setup_special_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(SpecialAssets {
        projectile_mesh: meshes.add(Sphere::new(0.24).mesh().uv(16, 8)),
        trap_mesh: meshes.add(Cylinder::new(0.52, 0.08)),
        shockwave_mesh: meshes.add(Torus::new(0.48, 0.035)),
        hazard_mesh: meshes.add(Cylinder::new(0.72, 0.1)),
        projectile_material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.45, 0.95, 1.0),
            emissive: LinearRgba::rgb(0.05, 0.22, 0.28),
            ..default()
        }),
        trap_material: materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.72, 0.18),
            emissive: LinearRgba::rgb(0.2, 0.08, 0.01),
            ..default()
        }),
        shockwave_material: materials.add(StandardMaterial {
            base_color: Color::srgba(0.8, 1.0, 0.72, 0.72),
            emissive: LinearRgba::rgb(0.08, 0.2, 0.05),
            alpha_mode: AlphaMode::Blend,
            ..default()
        }),
        hazard_material: materials.add(StandardMaterial {
            base_color: Color::srgba(0.25, 0.95, 0.48, 0.7),
            emissive: LinearRgba::rgb(0.02, 0.2, 0.06),
            alpha_mode: AlphaMode::Blend,
            ..default()
        }),
    });
}

pub fn handle_special_inputs(
    hitstop: Res<Hitstop>,
    assets: Res<SpecialAssets>,
    effect_assets: Res<EffectAssets>,
    mut feedback: ResMut<HitEffects>,
    mut commands: Commands,
    mut fighters: Query<(
        Entity,
        &Fighter,
        &mut FighterInput,
        &mut FighterMotor,
        &mut FighterActionState,
        &mut FighterSpecialState,
        &FighterStyle,
        &FighterEquipment,
        &Transform,
    )>,
) {
    if !SHARED_SPECIALS_ENABLED {
        for (_, _, mut input, _, _, _, _, _, _) in &mut fighters {
            input.special = false;
            input.special_kind = None;
        }
        return;
    }

    if hitstop.active() {
        return;
    }

    for (
        entity,
        fighter,
        mut input,
        mut motor,
        mut action,
        mut special_state,
        style,
        equipment,
        transform,
    ) in &mut fighters
    {
        if !input.special || special_state.cooldown > 0.0 || !can_cast_special(action.action) {
            continue;
        }

        let kind = requested_special_kind(&input);
        let loadout = LoadoutContext::new(style.kind, equipment.kind);

        cancel_dash_slide_for_action(&mut motor);
        special_state.cooldown = styled_special_cooldown(loadout);
        spawn_special(
            &mut commands,
            &assets,
            entity,
            fighter.id,
            kind,
            style.kind,
            loadout,
            transform.translation,
            motor.facing,
            &effect_assets,
        );
        feedback.push_feedback_cue(special_cue(kind), ImpactSource::MatchFlow, 20);
        set_special_action(&mut action);
        input.special = false;
        input.light = false;
        input.light_held = false;
        input.raw_light_pressed = false;
        input.heavy = false;
        input.heavy_held = false;
        input.raw_heavy_pressed = false;
        input.grab = false;
    }
}

fn special_cue(kind: SpecialKind) -> &'static str {
    special_definition(kind).timing.startup_cue
}

pub fn tick_special_cooldowns(
    time: Res<Time>,
    hitstop: Res<Hitstop>,
    mut fighters: Query<&mut FighterSpecialState>,
) {
    if hitstop.active() {
        return;
    }

    let dt = time.delta_secs();
    for mut special_state in &mut fighters {
        special_state.cooldown = (special_state.cooldown - dt).max(0.0);
    }
}

fn can_cast_special(action: FighterAction) -> bool {
    matches!(
        action,
        FighterAction::Idle | FighterAction::Moving | FighterAction::Guarding
    )
}

fn requested_special_kind(input: &FighterInput) -> SpecialKind {
    if let Some(kind) = input.special_kind {
        return match kind {
            SpecialInputKind::Projectile => SpecialKind::Projectile,
            SpecialInputKind::Trap => SpecialKind::Trap,
            SpecialInputKind::Hazard => SpecialKind::Hazard,
            SpecialInputKind::Shockwave => SpecialKind::Shockwave,
        };
    }
    if input.guard {
        SpecialKind::Trap
    } else if input.heavy {
        SpecialKind::Hazard
    } else if input.grab {
        SpecialKind::Shockwave
    } else {
        SpecialKind::Projectile
    }
}

fn special_definition(kind: SpecialKind) -> SpecialDefinition {
    match kind {
        SpecialKind::Projectile => SpecialDefinition {
            cost: SPECIAL_PROJECTILE_COST,
            lifetime: SPECIAL_PROJECTILE_LIFETIME,
            radius: SPECIAL_PROJECTILE_RADIUS,
            payload_id: AttackPayloadId::SpecialProjectile,
            shape_id: AttackShapeId::ProjectileBolt,
            source: ImpactSource::Projectile,
            timing: SpecialTimingDef {
                launch_ms: 90,
                active_window: MsTimingWindow::closed(120, 980),
                repeat_ms: None,
                aftermath_ms: Some(1120),
                startup_cue: "startup_special_projectile",
                active_cue: "release_special_projectile",
                aftermath_cue: "recover_special_projectile",
            },
            presentation: SpecialPresentationDef {
                startup_package: FeedbackPackageId::SpecialProjectileStartup,
                active_package: FeedbackPackageId::SpecialProjectileRelease,
                repeat_package: None,
                impact_package: FeedbackPackageId::SpecialProjectileImpact,
                aftermath_package: FeedbackPackageId::SpecialProjectileRecover,
                despawn_package: FeedbackPackageId::SpecialProjectileRecover,
            },
            guard_stamina_damage: 18.0,
        },
        SpecialKind::Trap => SpecialDefinition {
            cost: SPECIAL_TRAP_COST,
            lifetime: SPECIAL_TRAP_LIFETIME,
            radius: SPECIAL_TRAP_RADIUS,
            payload_id: AttackPayloadId::SpecialTrap,
            shape_id: AttackShapeId::TrapPlate,
            source: ImpactSource::Trap,
            timing: SpecialTimingDef {
                launch_ms: 0,
                active_window: MsTimingWindow::open_ended(260),
                repeat_ms: None,
                aftermath_ms: Some(520),
                startup_cue: "startup_special_trap",
                active_cue: "arm_special_trap",
                aftermath_cue: "recover_special_trap",
            },
            presentation: SpecialPresentationDef {
                startup_package: FeedbackPackageId::SpecialTrapStartup,
                active_package: FeedbackPackageId::SpecialTrapArm,
                repeat_package: None,
                impact_package: FeedbackPackageId::SpecialTrapImpact,
                aftermath_package: FeedbackPackageId::SpecialTrapRecover,
                despawn_package: FeedbackPackageId::SpecialTrapRecover,
            },
            guard_stamina_damage: 22.0,
        },
        SpecialKind::Shockwave => SpecialDefinition {
            cost: SPECIAL_SHOCKWAVE_COST,
            lifetime: SPECIAL_SHOCKWAVE_LIFETIME,
            radius: SPECIAL_SHOCKWAVE_RADIUS,
            payload_id: AttackPayloadId::SpecialShockwave,
            shape_id: AttackShapeId::ShockwaveRing,
            source: ImpactSource::Shockwave,
            timing: SpecialTimingDef {
                launch_ms: 0,
                active_window: MsTimingWindow::closed(70, 300),
                repeat_ms: None,
                aftermath_ms: Some(300),
                startup_cue: "startup_special_shockwave",
                active_cue: "release_special_shockwave",
                aftermath_cue: "recover_special_shockwave",
            },
            presentation: SpecialPresentationDef {
                startup_package: FeedbackPackageId::SpecialShockwaveStartup,
                active_package: FeedbackPackageId::SpecialShockwaveRelease,
                repeat_package: None,
                impact_package: FeedbackPackageId::SpecialShockwaveImpact,
                aftermath_package: FeedbackPackageId::SpecialShockwaveRecover,
                despawn_package: FeedbackPackageId::SpecialShockwaveRecover,
            },
            guard_stamina_damage: 24.0,
        },
        SpecialKind::Hazard => SpecialDefinition {
            cost: SPECIAL_HAZARD_COST,
            lifetime: SPECIAL_HAZARD_LIFETIME,
            radius: SPECIAL_HAZARD_RADIUS,
            payload_id: AttackPayloadId::SpecialHazard,
            shape_id: AttackShapeId::HazardField,
            source: ImpactSource::Hazard,
            timing: SpecialTimingDef {
                launch_ms: 0,
                active_window: MsTimingWindow::closed(340, 3600),
                repeat_ms: Some(420),
                aftermath_ms: Some(3600),
                startup_cue: "startup_special_hazard",
                active_cue: "pulse_special_hazard",
                aftermath_cue: "fade_special_hazard",
            },
            presentation: SpecialPresentationDef {
                startup_package: FeedbackPackageId::SpecialHazardStartup,
                active_package: FeedbackPackageId::SpecialHazardPulse,
                repeat_package: Some(FeedbackPackageId::SpecialHazardPulse),
                impact_package: FeedbackPackageId::SpecialHazardImpact,
                aftermath_package: FeedbackPackageId::SpecialHazardFade,
                despawn_package: FeedbackPackageId::SpecialHazardFade,
            },
            guard_stamina_damage: 14.0,
        },
    }
}

fn set_special_action(action: &mut FighterActionState) {
    action.action = FighterAction::SpecialCast;
    action.elapsed = 0.0;
    action.hitbox_spawned = false;
    action.queued_combo = false;
    action.queued_technique = None;
    action.queued_button = None;
    action.buffered_button = None;
    action.buffered_button_elapsed = 0.0;
    action.timeline_events_fired = 0;
    action.reaction_getup_ms = None;
    action.reaction_recover_ms = None;
    action.clear_reaction_visual();
}

fn spawn_special(
    commands: &mut Commands,
    assets: &SpecialAssets,
    owner: Entity,
    owner_id: usize,
    kind: SpecialKind,
    owner_style: FighterStyleKind,
    loadout: LoadoutContext,
    origin: Vec3,
    facing: Vec3,
    effect_assets: &EffectAssets,
) {
    let facing = facing.normalize_or_zero();
    let definition = special_definition(kind);
    let payload = attack_payload_definition(definition.payload_id);
    debug_assert_eq!(payload.shape_id, definition.shape_id);
    let (mesh, material, position, velocity, lifetime, radius, scale) = match kind {
        SpecialKind::Projectile => (
            assets.projectile_mesh.clone(),
            assets.projectile_material.clone(),
            origin + Vec3::Y * 0.9 + facing * 0.75,
            facing * SPECIAL_PROJECTILE_SPEED,
            definition.lifetime,
            definition.radius,
            Vec3::ONE,
        ),
        SpecialKind::Trap => {
            let flat = origin + facing * 0.85;
            let ground = ground_height_at(flat.x, flat.z).unwrap_or(ARENA_TOP_Y);
            (
                assets.trap_mesh.clone(),
                assets.trap_material.clone(),
                Vec3::new(flat.x, ground + 0.06, flat.z),
                Vec3::ZERO,
                definition.lifetime,
                definition.radius,
                Vec3::splat(1.0),
            )
        }
        SpecialKind::Shockwave => (
            assets.shockwave_mesh.clone(),
            assets.shockwave_material.clone(),
            origin + Vec3::Y * 0.08,
            Vec3::ZERO,
            definition.lifetime,
            definition.radius,
            Vec3::splat(0.4),
        ),
        SpecialKind::Hazard => {
            let flat = origin + facing * 1.15;
            let ground = ground_height_at(flat.x, flat.z).unwrap_or(ARENA_TOP_Y);
            (
                assets.hazard_mesh.clone(),
                assets.hazard_material.clone(),
                Vec3::new(flat.x, ground + 0.08, flat.z),
                Vec3::ZERO,
                definition.lifetime,
                definition.radius,
                Vec3::splat(1.0),
            )
        }
    };

    commands.spawn((
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::from_translation(position).with_scale(scale),
        ActiveSpecial {
            kind,
            owner,
            owner_id,
            owner_style,
            payload_id: definition.payload_id,
            shape_id: definition.shape_id,
            source: definition.source,
            facing,
            velocity,
            lifetime,
            age: 0.0,
            total_lifetime_ms: (definition.lifetime * 1000.0) as u32,
            radius,
            grace: SPECIAL_OWNER_GRACE,
            launch_ms: definition.timing.launch_ms,
            active_window: definition.timing.active_window,
            repeat_ms: definition.timing.repeat_ms,
            next_repeat_ms: definition
                .timing
                .repeat_ms
                .map(|repeat_ms| definition.timing.active_window.start_ms + repeat_ms),
            active_feedback_sent: false,
            aftermath_ms: definition.timing.aftermath_ms,
            aftermath_feedback_sent: false,
            active_cue: definition.timing.active_cue,
            aftermath_cue: definition.timing.aftermath_cue,
            active_package: definition.presentation.active_package,
            repeat_package: definition.presentation.repeat_package,
            impact_package: definition.presentation.impact_package,
            aftermath_package: definition.presentation.aftermath_package,
            despawn_package: definition.presentation.despawn_package,
            stamina_disrupt: loadout_special_stamina_disrupt(loadout),
            guard_stamina_damage: definition.guard_stamina_damage,
            already_hit: Vec::new(),
        },
        Name::new(match kind {
            SpecialKind::Projectile => "Pulse Dart",
            SpecialKind::Trap => "Trip Plate",
            SpecialKind::Shockwave => "Snap Wave",
            SpecialKind::Hazard => "Drift Field",
        }),
    ));
    spawn_feedback_package(
        commands,
        effect_assets,
        origin,
        facing,
        definition.presentation.startup_package,
    );
}

pub fn update_specials(
    time: Res<Time>,
    mut commands: Commands,
    effect_assets: Res<EffectAssets>,
    state: Res<MatchState>,
    feel: Res<CombatFeelTuning>,
    mut hitstop: ResMut<Hitstop>,
    mut camera_effects: ResMut<HitEffects>,
    mut haptics: ResMut<CombatHapticQueue>,
    mut telemetry: ResMut<MatchTelemetry>,
    mut specials: Query<(Entity, &mut ActiveSpecial, &mut Transform), Without<Fighter>>,
    mut fighters: Query<
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
        Without<ActiveSpecial>,
    >,
) {
    if hitstop.active() {
        return;
    }

    let dt = time.delta_secs();
    for (special_entity, mut special, mut transform) in &mut specials {
        special.age += dt;
        special.lifetime -= dt;
        special.grace = (special.grace - dt).max(0.0);
        let age_ms = elapsed_secs_to_ms(special.age);

        update_special_timing_feedback(
            &mut special,
            age_ms,
            &mut camera_effects,
            &mut commands,
            &effect_assets,
            transform.translation,
        );
        update_special_repeat_window(
            &mut special,
            age_ms,
            &mut camera_effects,
            &mut commands,
            &effect_assets,
            transform.translation,
        );

        if age_ms >= special.launch_ms {
            transform.translation += special.velocity * dt;
        }
        update_special_visual(&special, &mut transform);

        let active_radius = active_special_radius(&special);
        let mut hit_this_frame = false;
        for (
            target_entity,
            target,
            mut stats,
            mut motor,
            mut action,
            target_style,
            target_equipment,
            target_transform,
        ) in &mut fighters
        {
            if target_entity == special.owner && special.grace > 0.0 {
                continue;
            }
            if !special_active_at(&special, age_ms) {
                continue;
            }
            if !state.combat_target_allowed_for_state(special.owner_id, target.id) {
                continue;
            }
            if special.already_hit.contains(&target_entity)
                || !can_receive_impact(&stats, &action)
                || !special_overlaps_target(
                    &special,
                    active_radius,
                    transform.translation,
                    target_transform,
                )
            {
                continue;
            }

            let falloff = if matches!(special.kind, SpecialKind::Shockwave) {
                radial_falloff(
                    flat_distance(transform.translation, target_transform.translation),
                    active_radius,
                )
            } else {
                1.0
            };
            let profile = special_impact_profile_with_feel(&special, falloff, &feel);
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
                special.facing,
                special.impact_package,
            );
            hit_this_frame = true;
            if special.stamina_disrupt > 0.0 {
                stats.stamina =
                    (stats.stamina - special.stamina_disrupt * falloff.max(0.5)).max(0.0);
                stats.hud_flash = stats.hud_flash.max(0.18);
                camera_effects.push_feedback_cue(
                    "special_stamina_disrupt",
                    ImpactSource::Shockwave,
                    36,
                );
            }
            special.already_hit.push(target_entity);

            if matches!(special.kind, SpecialKind::Projectile | SpecialKind::Trap) {
                special.lifetime = 0.0;
                break;
            }
        }

        if special.lifetime <= 0.0 || should_despawn_special(transform.translation) {
            if !hit_this_frame {
                spawn_feedback_package(
                    &mut commands,
                    &effect_assets,
                    transform.translation,
                    special.facing,
                    special.despawn_package,
                );
            }
            commands.entity(special_entity).despawn();
        }
    }
}

fn update_special_visual(special: &ActiveSpecial, transform: &mut Transform) {
    match special.kind {
        SpecialKind::Projectile => {
            transform.rotate_y(if elapsed_secs_to_ms(special.age) >= special.launch_ms {
                0.18
            } else {
                0.04
            });
        }
        SpecialKind::Shockwave => {
            let t = special_active_progress(special);
            transform.scale = Vec3::splat(0.35 + t * SPECIAL_SHOCKWAVE_RADIUS * 2.0);
        }
        SpecialKind::Hazard => {
            let pulse = if special_active_at(special, elapsed_secs_to_ms(special.age)) {
                1.0 + (special.age * 9.0).sin().abs() * 0.16
            } else {
                0.74 + (special.age * 5.0).sin().abs() * 0.08
            };
            transform.scale = Vec3::splat(pulse);
        }
        SpecialKind::Trap => {
            transform.rotate_y(0.04);
            if !special_active_at(special, elapsed_secs_to_ms(special.age)) {
                transform.scale = Vec3::splat(0.72);
            }
        }
    }
}

fn active_special_radius(special: &ActiveSpecial) -> f32 {
    if special.kind == SpecialKind::Shockwave {
        let t = special_active_progress(special);
        (SPECIAL_SHOCKWAVE_RADIUS * t).max(0.45)
    } else {
        special.radius
    }
}

fn update_special_timing_feedback(
    special: &mut ActiveSpecial,
    age_ms: u32,
    effects: &mut HitEffects,
    commands: &mut Commands,
    effect_assets: &EffectAssets,
    position: Vec3,
) {
    if !special.active_feedback_sent && age_ms >= special.active_window.start_ms {
        effects.push_feedback_cue(special.active_cue, special.source, 28);
        spawn_feedback_package(
            commands,
            effect_assets,
            position,
            special.facing,
            special.active_package,
        );
        special.active_feedback_sent = true;
    }
    if let Some(aftermath_ms) = special.aftermath_ms
        && !special.aftermath_feedback_sent
        && age_ms >= aftermath_ms
    {
        effects.push_feedback_cue(special.aftermath_cue, special.source, 22);
        spawn_feedback_package(
            commands,
            effect_assets,
            position,
            special.facing,
            special.aftermath_package,
        );
        special.aftermath_feedback_sent = true;
    }
}

fn update_special_repeat_window(
    special: &mut ActiveSpecial,
    age_ms: u32,
    effects: &mut HitEffects,
    commands: &mut Commands,
    effect_assets: &EffectAssets,
    position: Vec3,
) {
    if let Some(package) = advance_special_repeat_window(special, age_ms, effects) {
        spawn_feedback_package(commands, effect_assets, position, special.facing, package);
    }
}

fn advance_special_repeat_window(
    special: &mut ActiveSpecial,
    age_ms: u32,
    effects: &mut HitEffects,
) -> Option<FeedbackPackageId> {
    let Some(repeat_ms) = special.repeat_ms else {
        return None;
    };
    let Some(mut next_repeat_ms) = special.next_repeat_ms else {
        return None;
    };
    if !special_active_at(special, age_ms) {
        return None;
    }
    let mut package_to_spawn = None;
    while age_ms >= next_repeat_ms {
        special.already_hit.clear();
        effects.push_feedback_cue(special.active_cue, special.source, 24);
        package_to_spawn = special.repeat_package;
        next_repeat_ms += repeat_ms;
    }
    special.next_repeat_ms = Some(next_repeat_ms);
    package_to_spawn
}

fn special_active_at(special: &ActiveSpecial, age_ms: u32) -> bool {
    special.active_window.contains_ms(age_ms)
}

fn special_active_progress(special: &ActiveSpecial) -> f32 {
    let age_ms = elapsed_secs_to_ms(special.age);
    let start_ms = special.active_window.start_ms;
    let end_ms = special
        .active_window
        .end_ms
        .unwrap_or(special.total_lifetime_ms)
        .max(start_ms + 1);
    ((age_ms.saturating_sub(start_ms)) as f32 / (end_ms - start_ms) as f32).clamp(0.0, 1.0)
}

fn special_overlaps_target(
    special: &ActiveSpecial,
    radius: f32,
    origin: Vec3,
    target_transform: &Transform,
) -> bool {
    let target = target_transform.translation + Vec3::Y * (FIGHTER_HEIGHT * 0.58);
    match special.kind {
        SpecialKind::Projectile => target.distance(origin) <= radius + FIGHTER_RADIUS,
        _ => flat_distance(origin, target_transform.translation) <= radius + FIGHTER_RADIUS,
    }
}

fn flat_distance(a: Vec3, b: Vec3) -> f32 {
    Vec2::new(a.x - b.x, a.z - b.z).length()
}

#[cfg(test)]
fn special_impact_profile(special: &ActiveSpecial, falloff: f32) -> ImpactProfile {
    special_impact_profile_from_payload(special, falloff, None)
}

fn special_impact_profile_with_feel(
    special: &ActiveSpecial,
    falloff: f32,
    feel: &CombatFeelTuning,
) -> ImpactProfile {
    special_impact_profile_from_payload(special, falloff, Some(feel))
}

fn special_impact_profile_from_payload(
    special: &ActiveSpecial,
    falloff: f32,
    feel: Option<&CombatFeelTuning>,
) -> ImpactProfile {
    let (damage_scale, knockback_scale) = match special.kind {
        SpecialKind::Shockwave => (falloff.max(0.45), falloff.max(0.55)),
        _ => (1.0, 1.0),
    };

    let mut profile = if let Some(feel) = feel {
        impact_profile_from_payload_with_feel(
            special.owner_id,
            special.source,
            special.payload_id,
            damage_scale,
            knockback_scale,
            1.0,
            special.guard_stamina_damage,
            feel,
        )
    } else {
        impact_profile_from_payload(
            special.owner_id,
            special.source,
            special.payload_id,
            damage_scale,
            knockback_scale,
            1.0,
            special.guard_stamina_damage,
        )
    };
    profile.shape_id = Some(special.shape_id);
    profile.attacker_style = Some(special.owner_style);
    profile
}

fn should_despawn_special(position: Vec3) -> bool {
    let arena = active_arena_definition();
    position.y < arena.ringout_y
        || Vec2::new(position.x, position.z).length() > arena.ringout_radius
}

#[allow(dead_code)]
fn special_cost(kind: SpecialKind) -> f32 {
    special_definition(kind).cost
}

#[allow(dead_code)]
fn styled_special_cost(kind: SpecialKind, loadout: LoadoutContext) -> f32 {
    special_cost(kind) * loadout_special_cost_scale(loadout)
}

fn styled_special_cooldown(loadout: LoadoutContext) -> f32 {
    SPECIAL_COOLDOWN * loadout_special_cooldown_scale(loadout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directly_injected_special_request_is_rejected_by_authoritative_handler() {
        let mut app = App::new();
        app.insert_resource(Hitstop::default())
            .insert_resource(HitEffects::default())
            .insert_resource(EffectAssets::default())
            .insert_resource(SpecialAssets {
                projectile_mesh: Handle::default(),
                trap_mesh: Handle::default(),
                shockwave_mesh: Handle::default(),
                hazard_mesh: Handle::default(),
                projectile_material: Handle::default(),
                trap_material: Handle::default(),
                shockwave_material: Handle::default(),
                hazard_material: Handle::default(),
            })
            .add_systems(Update, handle_special_inputs);
        let fighter = app
            .world_mut()
            .spawn((
                Fighter {
                    id: 0,
                    name: "Injected",
                    color: Color::WHITE,
                    spawn: Vec3::ZERO,
                },
                FighterInput {
                    special: true,
                    special_kind: Some(SpecialInputKind::Projectile),
                    ..default()
                },
                FighterMotor::default(),
                FighterActionState::default(),
                FighterSpecialState::default(),
                FighterStyle {
                    kind: FighterStyleKind::Catalyst,
                },
                FighterEquipment::new(crate::equipment::EquipmentKind::CounterCell),
                Transform::default(),
            ))
            .id();

        app.update();

        assert_eq!(
            app.world_mut()
                .query::<&ActiveSpecial>()
                .iter(app.world())
                .count(),
            0
        );
        let input = app.world().get::<FighterInput>(fighter).unwrap();
        assert!(!input.special);
        assert_eq!(input.special_kind, None);
        assert_eq!(
            app.world()
                .get::<FighterActionState>(fighter)
                .unwrap()
                .action,
            FighterAction::Idle
        );
        assert_eq!(
            app.world()
                .get::<FighterSpecialState>(fighter)
                .unwrap()
                .cooldown,
            0.0
        );
    }

    #[test]
    fn shockwave_radius_expands_over_lifetime() {
        let mut special = test_special(SpecialKind::Shockwave);
        special.age = SPECIAL_SHOCKWAVE_LIFETIME * 0.5;
        let mid = active_special_radius(&special);
        special.age = SPECIAL_SHOCKWAVE_LIFETIME;
        assert!(active_special_radius(&special) > mid);
    }

    #[test]
    fn special_profiles_use_distinct_sources() {
        let projectile = special_impact_profile(&test_special(SpecialKind::Projectile), 1.0);
        let trap = special_impact_profile(&test_special(SpecialKind::Trap), 1.0);

        assert_eq!(projectile.source, ImpactSource::Projectile);
        assert_eq!(
            projectile.payload_id,
            Some(AttackPayloadId::SpecialProjectile)
        );
        assert_eq!(trap.source, ImpactSource::Trap);
        assert_eq!(trap.payload_id, Some(AttackPayloadId::SpecialTrap));
        assert_eq!(trap.shape_id, Some(AttackShapeId::TrapPlate));
    }

    #[test]
    fn specials_use_authored_activation_windows() {
        let projectile = test_special(SpecialKind::Projectile);
        assert_eq!(projectile.launch_ms, 90);
        assert!(!special_active_at(&projectile, 119));
        assert!(special_active_at(&projectile, 120));

        let trap = test_special(SpecialKind::Trap);
        assert!(!special_active_at(&trap, 259));
        assert!(special_active_at(&trap, 260));

        let shockwave = test_special(SpecialKind::Shockwave);
        assert!(special_active_at(&shockwave, 70));
        assert!(!special_active_at(&shockwave, 301));
    }

    #[test]
    fn repeating_specials_clear_contact_memory_on_authored_ticks() {
        let mut hazard = test_special(SpecialKind::Hazard);
        hazard
            .already_hit
            .push(Entity::from_raw_u32(2).expect("valid entity"));
        let mut effects = HitEffects::default();

        let package = advance_special_repeat_window(&mut hazard, 759, &mut effects);
        assert_eq!(package, None);
        assert_eq!(hazard.already_hit.len(), 1);

        let package = advance_special_repeat_window(&mut hazard, 760, &mut effects);
        assert!(hazard.already_hit.is_empty());
        assert_eq!(effects.last_cue.unwrap().id, "pulse_special_hazard");
        assert_eq!(package, Some(FeedbackPackageId::SpecialHazardPulse));
    }

    #[test]
    fn specials_carry_authored_presentation_packages() {
        let projectile = special_definition(SpecialKind::Projectile).presentation;
        let shockwave = special_definition(SpecialKind::Shockwave).presentation;
        let hazard = special_definition(SpecialKind::Hazard).presentation;

        assert_eq!(
            projectile.impact_package,
            FeedbackPackageId::SpecialProjectileImpact
        );
        assert_eq!(
            shockwave.active_package,
            FeedbackPackageId::SpecialShockwaveRelease
        );
        assert_eq!(
            hazard.repeat_package,
            Some(FeedbackPackageId::SpecialHazardPulse)
        );
    }

    #[test]
    fn catalyst_style_cycles_specials_faster() {
        let catalyst = LoadoutContext::new(
            crate::styles::FighterStyleKind::Catalyst,
            crate::equipment::EquipmentKind::CounterCell,
        );
        let anchor = LoadoutContext::new(
            crate::styles::FighterStyleKind::Anchor,
            crate::equipment::EquipmentKind::CounterCell,
        );
        assert!(
            styled_special_cost(SpecialKind::Projectile, catalyst)
                < styled_special_cost(SpecialKind::Projectile, anchor)
        );
        assert!(styled_special_cooldown(catalyst) < styled_special_cooldown(anchor));
    }

    #[test]
    fn catalyst_specials_apply_disruption_without_damage_bonus() {
        let mut special = test_special(SpecialKind::Projectile);
        special.stamina_disrupt = loadout_special_stamina_disrupt(LoadoutContext::new(
            crate::styles::FighterStyleKind::Catalyst,
            crate::equipment::EquipmentKind::CounterCell,
        ));
        let profile = special_impact_profile(&special, 1.0);

        assert!(special.stamina_disrupt > 0.0);
        assert_eq!(profile.damage, SPECIAL_PROJECTILE_DAMAGE);
    }

    #[test]
    fn explicit_controller_special_kind_overrides_combat_button_state() {
        let input = FighterInput {
            special: true,
            guard: true,
            heavy: true,
            grab: true,
            special_kind: Some(SpecialInputKind::Shockwave),
            ..default()
        };
        assert_eq!(requested_special_kind(&input), SpecialKind::Shockwave);
    }

    fn test_special(kind: SpecialKind) -> ActiveSpecial {
        ActiveSpecial {
            kind,
            owner: Entity::from_raw_u32(1).expect("test entity index should be valid"),
            owner_id: 0,
            owner_style: FighterStyleKind::Anchor,
            payload_id: special_definition(kind).payload_id,
            shape_id: special_definition(kind).shape_id,
            source: special_definition(kind).source,
            facing: Vec3::Z,
            velocity: Vec3::ZERO,
            lifetime: 1.0,
            age: 0.0,
            total_lifetime_ms: 1000,
            radius: 1.0,
            grace: 0.0,
            launch_ms: special_definition(kind).timing.launch_ms,
            active_window: special_definition(kind).timing.active_window,
            repeat_ms: special_definition(kind).timing.repeat_ms,
            next_repeat_ms: special_definition(kind).timing.repeat_ms.map(|repeat_ms| {
                special_definition(kind).timing.active_window.start_ms + repeat_ms
            }),
            active_feedback_sent: false,
            aftermath_ms: special_definition(kind).timing.aftermath_ms,
            aftermath_feedback_sent: false,
            active_cue: special_definition(kind).timing.active_cue,
            aftermath_cue: special_definition(kind).timing.aftermath_cue,
            active_package: special_definition(kind).presentation.active_package,
            repeat_package: special_definition(kind).presentation.repeat_package,
            impact_package: special_definition(kind).presentation.impact_package,
            aftermath_package: special_definition(kind).presentation.aftermath_package,
            despawn_package: special_definition(kind).presentation.despawn_package,
            stamina_disrupt: 0.0,
            guard_stamina_damage: special_definition(kind).guard_stamina_damage,
            already_hit: Vec::new(),
        }
    }
}
