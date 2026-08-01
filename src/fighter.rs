use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use crate::arena::{
    ArenaFighterBurn, ground_height_at_with_radius, ground_support_at_with_radius,
    resolve_platform_side_collision,
};
use crate::arena_defs::{ArenaDefinition, active_arena_definition};
use crate::body_collision::{
    FighterBodyBox, body_box_landing_correction, body_box_separation, fighter_body_box,
};
use crate::bot::default_bot_brain_for_fighter;
use crate::camera::{GameplayCameraControl, camera_relative_direction};
use crate::characters::{
    CharacterBodyDef, CharacterKind, CharacterMoveCatalog, CharacterMoveSlot, FighterCharacter,
    character_for_fighter_id, character_mesh_bounds, character_scene_model,
};
use crate::chick_skills::{ActiveChickSkill, ChickSkillKind};
use crate::combat::{
    DamageDefenderProfile, HitEffects, ImpactFeedbackIntensity, ImpactProfile, ImpactSource,
    apply_impact, impact_profile,
};
use crate::combat_sfx::{CombatSfxCue, CombatSfxKind, ground_impact_priority};
use crate::components::{
    Controller, DrunkStatus, Fighter, FighterAction, FighterActionState, FighterAimState,
    FighterBody, FighterGrabState, FighterHand, FighterHead, FighterInput, FighterInventory,
    FighterMarker, FighterMotor, FighterPoseRoot, FighterSceneModel, FighterSpecialState,
    FighterStats, FighterUltimateState, FighterVisualRoot, LocalInputAssignment,
    PlayerControlBindings, PlayerKeyBindings, PlayerSlotId, SpecialInputKind,
};
use crate::constants::*;
use crate::controller_haptics::{
    CombatHapticCue, CombatHapticQueue, HapticContactOutcome, HapticImpactWeight,
};
use crate::effects::{
    EffectAssets, spawn_aftermath_pulse, spawn_dash_trail, spawn_drunk_bubble, spawn_dust_puff,
    spawn_guard_flash, spawn_respawn_column, spawn_ringout_burst,
};
#[cfg(test)]
use crate::equipment::EquipmentKind;
use crate::equipment::{
    FighterEquipment, LoadoutContext, equipment_for_fighter_id, equipment_identity,
    loadout_heavy_armor, loadout_heavy_whiff_recovery_scale,
};
use crate::feel::CombatFeelTuning;
use crate::game_state::{Hitstop, LocalSetup, MatchAnnouncements, MatchState, MatchTelemetry};
use crate::penguin_skills::{ActivePenguinSurface, penguin_ice_modifier};
use crate::reactions::ReactionFamilyId;
use crate::styles::{
    FighterStyle, FighterStyleKind, style_for_fighter_id, style_identity, style_mechanics,
    style_tuning,
};
#[cfg(test)]
use crate::techniques::technique_definition_by_id;
use crate::techniques::{
    DamageElement, DamageProfileId, PIG_HEAVY_ATTACK_MS, PIG_HEAVY_FULL_CHARGE_MS, TechniqueButton,
    TechniqueDefinition, TechniqueId, TechniqueMatchContext, TechniqueRuntime,
    active_technique_definition_in_catalog, chained_technique_for_context_in_catalog,
    raw_technique_for_loadout_in_catalog, technique_definition_for_loadout_id_in_catalog,
    technique_slot_for_loadout,
};
use crate::user_mode::UserModeState;

const DOUBLE_TAP_DASH_WINDOW: f32 = 0.28;
pub(crate) const GAMEPAD_MOVEMENT_DEADZONE: f32 = 0.20;
const GAMEPAD_FLICK_DASH_THRESHOLD: f32 = 0.80;
const FIGHTER_BODY_SEPARATION_ITERATIONS: usize = 3;
const KNOCKDOWN_HEAD_LOW_PITCH: f32 = 2.72;
const JUMP_ATTACK_QUEUE_GRACE: f32 = 0.18;
const JUMP_ATTACK_QUEUE_TAKEOFF_REMAINING: f32 = 0.035;
const PENGUIN_HARD_ICE_SLIDE_MIN_SPEED: f32 = 5.35;
const PENGUIN_HARD_ICE_ENTRY_SPEED_MULTIPLIER: f32 = 2.0;
const PENGUIN_HARD_ICE_SLIDE_MAX_SPEED: f32 =
    DASH_HOLD_SPEED * PENGUIN_HARD_ICE_ENTRY_SPEED_MULTIPLIER;
const PENGUIN_HARD_ICE_ENTRY_SPEED_THRESHOLD: f32 = 0.22;
const PENGUIN_JUMP_SNOWFLAKE_MIN_FALL_SPEED: f32 = -0.8;
const CHICK_JUMP_C_FORWARD_SPEED: f32 = 3.2;
const CHICK_JUMP_C_MIN_UP_SPEED: f32 = 4.2;
const CHICK_FRESH_EGG_RIDE_FORWARD_SPEED: f32 = 8.8;
const CHICK_FRESH_EGG_RIDE_LIFT_SPEED: f32 = 0.9;
const CHICK_DASH_BACKSTEP_DURATION: f32 = 0.18;
const CHICK_DASH_C_BACKSTEP_DISTANCE: f32 = FIGHTER_RADIUS * 2.0 * 3.0;
const CHICK_DASH_X_BACKSTEP_DISTANCE: f32 = FIGHTER_RADIUS * 2.0 * 6.0;
const CHICK_DASH_C_BACKSTEP_SPEED: f32 =
    CHICK_DASH_C_BACKSTEP_DISTANCE / CHICK_DASH_BACKSTEP_DURATION;
const CHICK_DASH_X_BACKSTEP_SPEED: f32 =
    CHICK_DASH_X_BACKSTEP_DISTANCE / CHICK_DASH_BACKSTEP_DURATION;
const AIM_ACQUIRE_DISTANCE: f32 = 7.5;
const AIM_RETAIN_DISTANCE: f32 = 9.0;
const AIM_ACQUIRE_COS: f32 = 0.866_025_4;
const AIM_BREAK_COS: f32 = 0.5;
const AIM_FREE_DISTANCE: f32 = 2.4;
const AIM_TORSO_HEIGHT: f32 = 1.05;
const AIM_MARKER_SMOOTHING: f32 = 22.0;
const AIM_MARKER_FADE_IN: f32 = 0.06;
const AIM_MARKER_FADE_OUT: f32 = 0.14;
const AIM_MARKER_BASE_SCALE: f32 = 0.72;

#[derive(Component)]
pub(crate) struct FighterStyleAccent {
    fighter_id: usize,
    kind: crate::styles::FighterStyleKind,
}

#[derive(Component)]
pub(crate) struct FighterEquipmentChip {
    fighter_id: usize,
    kind: crate::equipment::EquipmentKind,
}

#[derive(Component)]
pub(crate) struct FighterGuardShield {
    fighter_id: usize,
}

#[derive(Component)]
pub(crate) struct FighterLightPunchCornerTint {
    fighter_id: usize,
    character: CharacterKind,
}

#[derive(Component)]
pub(crate) struct FighterTintMaterial {
    fighter_id: usize,
    original: Handle<StandardMaterial>,
    tint: Handle<StandardMaterial>,
}

#[derive(Component)]
pub(crate) struct FighterAimMarker {
    fighter_id: usize,
    material: Handle<StandardMaterial>,
}

pub fn spawn_fighters(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    character_catalog: Res<CharacterMoveCatalog>,
    setup: Res<LocalSetup>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let body_mesh = meshes.add(Capsule3d::new(0.34, 0.78));
    let head_mesh = meshes.add(Sphere::new(0.34).mesh().uv(24, 12));
    let hand_mesh = meshes.add(Sphere::new(0.14).mesh().uv(12, 6));
    let marker_mesh = meshes.add(Cone::new(0.22, 0.32));
    let face_mesh = meshes.add(Cuboid::new(0.34, 0.08, 0.05));
    let style_accent_mesh = meshes.add(Torus::new(0.38, 0.025));
    let equipment_chip_mesh = meshes.add(Cuboid::new(0.18, 0.16, 0.09));
    let guard_shield_mesh = meshes.add(Cuboid::new(1.28, 1.05, 0.045));
    let light_punch_corner_mesh = meshes.add(light_punch_corner_tint_mesh());
    let aim_ring_mesh = meshes.add(Torus::new(0.32, 0.025));
    let aim_tick_mesh = meshes.add(Cuboid::new(0.16, 0.035, 0.025));

    for id in 0..spawned_fighter_count() {
        let arena = active_arena_definition();
        let color = FIGHTER_COLORS[id];
        let slot = setup.slot(id);
        let character_kind = slot
            .map(|slot| slot.character)
            .unwrap_or_else(|| character_for_fighter_id(id));
        let style_kind = slot
            .map(|slot| slot.style)
            .unwrap_or_else(|| style_for_fighter_id(id));
        let style_identity = style_identity(style_kind);
        let equipment_kind = slot
            .map(|slot| slot.equipment)
            .unwrap_or_else(|| equipment_for_fighter_id(id));
        let equipment_identity = equipment_identity(equipment_kind);
        let scene_model = character_scene_model(&asset_server, &character_catalog, character_kind);
        let body_material = materials.add(StandardMaterial {
            base_color: color,
            perceptual_roughness: 0.68,
            ..default()
        });
        let style_material = materials.add(StandardMaterial {
            base_color: style_identity.accent,
            emissive: LinearRgba::from(style_identity.accent.to_linear()) * 0.16,
            perceptual_roughness: 0.58,
            ..default()
        });
        let equipment_material = materials.add(StandardMaterial {
            base_color: equipment_identity.accent,
            emissive: LinearRgba::from(equipment_identity.accent.to_linear()) * 0.12,
            perceptual_roughness: 0.5,
            ..default()
        });
        let head_material = materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.82, 0.62),
            perceptual_roughness: 0.72,
            ..default()
        });
        let marker_material = materials.add(StandardMaterial {
            base_color: color.lighter(0.25),
            emissive: LinearRgba::from(color.to_linear()) * 0.18,
            ..default()
        });
        let face_material = materials.add(StandardMaterial {
            base_color: Color::srgb(0.08, 0.07, 0.055),
            ..default()
        });
        let guard_shield_material = materials.add(StandardMaterial {
            base_color: Color::srgba(0.24, 0.78, 1.0, 0.28),
            emissive: LinearRgba::rgb(0.0, 0.12, 0.18),
            alpha_mode: AlphaMode::Blend,
            perceptual_roughness: 0.36,
            ..default()
        });
        let light_punch_corner_material =
            materials.add(light_punch_corner_tint_material(character_kind));
        let aim_marker_material = materials.add(aim_marker_material(color));

        let slot_id = PlayerSlotId::new(id).expect("spawned fighter id should be a valid slot");
        let controller = setup
            .controller_for_fighter(id)
            .unwrap_or_else(|| Controller::closed(slot_id));
        let active = setup.is_slot_occupied(id);
        let mut stats = FighterStats::default();
        if !active {
            stats.respawn_timer = f32::INFINITY;
        }
        let mut action = FighterActionState::default();
        if !active {
            action.action = FighterAction::RingOut;
        }
        let visibility = if active {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };

        let mut entity = commands.spawn((
            Fighter {
                id,
                name: FIGHTER_NAMES[id],
                color,
                spawn: arena.spawn_points[id],
            },
            stats,
            FighterMotor {
                facing: if id % 2 == 0 { Vec3::X } else { -Vec3::X },
                ..default()
            },
            FighterInput::default(),
            FighterInventory::default(),
            FighterGrabState::default(),
            FighterSpecialState::default(),
            FighterUltimateState::default(),
            FighterStyle { kind: style_kind },
            FighterEquipment::new(equipment_kind),
            action,
            controller,
            FighterVisualRoot,
            Transform::from_translation(arena.spawn_points[id]),
            visibility,
        ));
        entity.insert(FighterAimState {
            direction: if id % 2 == 0 { Vec3::X } else { -Vec3::X },
            marker_position: arena.spawn_points[id]
                + Vec3::Y * AIM_TORSO_HEIGHT
                + if id % 2 == 0 { Vec3::X } else { -Vec3::X } * AIM_FREE_DISTANCE,
            ..default()
        });
        entity.insert(DrunkStatus::default());
        entity.insert(FighterCharacter::new(character_kind));

        if controller.is_bot() {
            entity.insert(default_bot_brain_for_fighter(id));
        }

        entity.with_children(|parent| {
            parent
                .spawn((
                    Transform::from_translation(fighter_pose_root_translation()),
                    Visibility::Inherited,
                    FighterPoseRoot,
                    Name::new(format!("{} pose root", FIGHTER_NAMES[id])),
                ))
                .with_children(|pose_root| {
                    if let Some(scene) = scene_model {
                        pose_root.spawn((
                            SceneRoot(scene),
                            fighter_scene_model_transform(),
                            FighterSceneModel { fighter_id: id },
                            Name::new(format!("{} Kenney cube pet", FIGHTER_NAMES[id])),
                        ));
                    } else {
                        pose_root.spawn((
                            Mesh3d(body_mesh.clone()),
                            MeshMaterial3d(body_material.clone()),
                            Transform::from_translation(pose_local_translation(Vec3::new(
                                0.0,
                                FIGHTER_BODY_Y,
                                0.0,
                            ))),
                            FighterBody,
                        ));
                        pose_root.spawn((
                            Mesh3d(head_mesh.clone()),
                            MeshMaterial3d(head_material),
                            Transform::from_translation(pose_local_translation(Vec3::new(
                                0.0,
                                FIGHTER_HEAD_Y,
                                0.0,
                            ))),
                            FighterHead,
                        ));
                        pose_root.spawn((
                            Mesh3d(face_mesh.clone()),
                            MeshMaterial3d(face_material),
                            Transform::from_translation(pose_local_translation(Vec3::new(
                                0.0,
                                FIGHTER_HEAD_Y + 0.03,
                                0.31,
                            ))),
                        ));
                        for x in [-0.42, 0.42] {
                            pose_root.spawn((
                                Mesh3d(hand_mesh.clone()),
                                MeshMaterial3d(body_material.clone()),
                                Transform::from_translation(pose_local_translation(Vec3::new(
                                    x, 0.88, 0.08,
                                ))),
                                FighterHand,
                            ));
                        }
                    }
                    pose_root.spawn((
                        Mesh3d(style_accent_mesh.clone()),
                        MeshMaterial3d(style_material),
                        Transform::from_translation(pose_local_translation(Vec3::new(
                            0.0,
                            FIGHTER_BODY_Y + 0.1,
                            0.0,
                        )))
                        .with_rotation(Quat::from_rotation_x(std::f32::consts::FRAC_PI_2))
                        .with_scale(Vec3::splat(style_identity.marker_scale)),
                        FighterStyleAccent {
                            fighter_id: id,
                            kind: style_kind,
                        },
                    ));
                    pose_root.spawn((
                        Mesh3d(equipment_chip_mesh.clone()),
                        MeshMaterial3d(equipment_material),
                        Transform::from_translation(pose_local_translation(Vec3::new(
                            0.0,
                            FIGHTER_BODY_Y + 0.32,
                            -0.36,
                        ))),
                        FighterEquipmentChip {
                            fighter_id: id,
                            kind: equipment_kind,
                        },
                    ));
                    pose_root.spawn((
                        Mesh3d(light_punch_corner_mesh.clone()),
                        MeshMaterial3d(light_punch_corner_material.clone()),
                        Transform::default(),
                        FighterLightPunchCornerTint {
                            fighter_id: id,
                            character: character_kind,
                        },
                        Visibility::Hidden,
                        Name::new(format!("{} light punch corner tint", FIGHTER_NAMES[id])),
                    ));
                });
            parent.spawn((
                Mesh3d(marker_mesh.clone()),
                MeshMaterial3d(marker_material),
                fighter_marker_transform(),
                FighterMarker,
            ));
            parent.spawn((
                Mesh3d(guard_shield_mesh.clone()),
                MeshMaterial3d(guard_shield_material),
                guard_shield_transform(),
                FighterGuardShield { fighter_id: id },
                Visibility::Hidden,
                Name::new(format!("{} guard shield", FIGHTER_NAMES[id])),
            ));
        });
        commands
            .spawn((
                FighterAimMarker {
                    fighter_id: id,
                    material: aim_marker_material.clone(),
                },
                Transform::from_translation(arena.spawn_points[id]),
                Visibility::Hidden,
            ))
            .with_children(|parent| {
                parent.spawn((
                    Mesh3d(aim_ring_mesh.clone()),
                    MeshMaterial3d(aim_marker_material.clone()),
                    Transform::from_rotation(Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)),
                ));
                for (translation, rotation) in [
                    (Vec3::new(0.0, 0.42, 0.0), 0.0),
                    (Vec3::new(0.0, -0.42, 0.0), 0.0),
                    (Vec3::new(0.42, 0.0, 0.0), std::f32::consts::FRAC_PI_2),
                    (Vec3::new(-0.42, 0.0, 0.0), std::f32::consts::FRAC_PI_2),
                ] {
                    parent.spawn((
                        Mesh3d(aim_tick_mesh.clone()),
                        MeshMaterial3d(aim_marker_material.clone()),
                        Transform::from_translation(translation)
                            .with_rotation(Quat::from_rotation_z(rotation)),
                    ));
                }
            });
    }
}

fn spawned_fighter_count() -> usize {
    FIGHTER_COUNT
}

fn fighter_pose_root_translation() -> Vec3 {
    Vec3::Y * FIGHTER_BODY_Y
}

fn pose_local_translation(world_offset: Vec3) -> Vec3 {
    world_offset - fighter_pose_root_translation()
}

fn fighter_scene_model_transform() -> Transform {
    Transform::from_translation(pose_local_translation(Vec3::new(
        0.0,
        KENNEY_CUBE_PET_GROUND_OFFSET,
        0.0,
    )))
    .with_scale(Vec3::splat(KENNEY_CUBE_PET_SCALE))
}

fn light_punch_corner_tint_mesh() -> Mesh {
    let width: f32 = 0.38;
    let height: f32 = 0.44;
    let depth: f32 = 0.32;
    let half_height = height * 0.5;
    let positions = vec![
        [-width, -half_height, 0.0],
        [0.0, -half_height, 0.0],
        [0.0, half_height, 0.0],
        [-width, half_height, 0.0],
        [0.0, -half_height, -depth],
        [0.0, -half_height, 0.0],
        [0.0, half_height, 0.0],
        [0.0, half_height, -depth],
    ];
    let normals = vec![
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 1.0],
        [0.0, 0.0, 1.0],
        [1.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
    ];
    let uvs = vec![
        [0.0, 0.0],
        [1.0, 0.0],
        [1.0, 1.0],
        [0.0, 1.0],
        [0.0, 0.0],
        [1.0, 0.0],
        [1.0, 1.0],
        [0.0, 1.0],
    ];

    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_indices(Indices::U32(vec![0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7]))
}

fn light_punch_corner_tint_color(character: CharacterKind) -> Color {
    match character {
        CharacterKind::Pig => Color::srgba(0.1, 0.42, 1.0, 0.56),
        _ => Color::srgba(1.0, 0.035, 0.02, 0.56),
    }
}

fn light_punch_corner_tint_material(character: CharacterKind) -> StandardMaterial {
    let color = light_punch_corner_tint_color(character);
    StandardMaterial {
        base_color: color,
        emissive: LinearRgba::from(color.to_linear()) * 0.9,
        alpha_mode: AlphaMode::Blend,
        cull_mode: None,
        depth_bias: 80.0,
        unlit: true,
        perceptual_roughness: 0.42,
        ..default()
    }
}

fn aim_marker_material(color: Color) -> StandardMaterial {
    StandardMaterial {
        base_color: color.with_alpha(0.0),
        emissive: LinearRgba::from(color.to_linear()) * 0.8,
        alpha_mode: AlphaMode::Blend,
        cull_mode: None,
        depth_bias: 90.0,
        unlit: true,
        ..default()
    }
}

fn fighter_marker_transform() -> Transform {
    Transform::from_xyz(0.0, 2.28, 0.0).with_rotation(Quat::from_rotation_x(std::f32::consts::PI))
}

fn guard_shield_transform() -> Transform {
    Transform::from_xyz(0.0, FIGHTER_BODY_Y + 0.2, FIGHTER_RADIUS + 0.32)
}

pub fn collect_player_input(
    keys: Res<ButtonInput<KeyCode>>,
    gamepads: Query<&Gamepad>,
    time: Res<Time>,
    camera_control: Res<GameplayCameraControl>,
    user_mode: Res<UserModeState>,
    bindings: Res<PlayerKeyBindings>,
    mut trackers: Local<PlayerInputTrackers>,
    mut fighters: Query<(&Controller, &mut FighterInput)>,
) {
    let reserve_camera_inputs = !user_mode.blocks_dev_input();

    for (controller, mut input) in &mut fighters {
        if !controller.is_human() {
            continue;
        }

        *input = FighterInput::default();
        let slot_index = controller.slot.index();
        trackers.reset_if_assignment_changed(slot_index, controller.input);
        let sample = match controller.input {
            LocalInputAssignment::Keyboard(_) => {
                let Some(bindings) = bindings.bindings_for_assignment(controller.input) else {
                    continue;
                };
                keyboard_action_sample(&keys, bindings, reserve_camera_inputs)
            }
            LocalInputAssignment::Gamepad(entity) => {
                let Ok(gamepad) = gamepads.get(entity) else {
                    continue;
                };
                gamepad_action_sample(gamepad)
            }
            LocalInputAssignment::Unassigned => continue,
        };

        let PlayerInputTrackers {
            dash,
            guard,
            stick_flick,
            ..
        } = &mut *trackers;
        collect_device_player_input(
            sample,
            time.elapsed_secs(),
            camera_control.yaw,
            &mut dash[slot_index],
            &mut guard[slot_index],
            &mut stick_flick[slot_index],
            &mut input,
        );
    }
}

/// Gameplay input modifiers run after all human and bot input producers, but
/// before action interpretation and directional input is consumed.
pub fn apply_drunk_input_modifier(fighters: Query<(&DrunkStatus, &mut FighterInput)>) {
    for (status, mut input) in fighters {
        if status.active() {
            invert_directional_input(&mut input);
        }
    }
}

fn invert_directional_input(input: &mut FighterInput) {
    input.movement = -input.movement;
}

pub fn update_drunk_status(
    time: Res<Time>,
    hitstop: Res<Hitstop>,
    state: Res<MatchState>,
    mut previous_phase: Local<Option<crate::game_state::MatchPhase>>,
    mut commands: Commands,
    effect_assets: Res<EffectAssets>,
    mut fighters: Query<(
        Entity,
        &Fighter,
        &FighterActionState,
        &Transform,
        &mut DrunkStatus,
    )>,
) {
    let dt = time.delta_secs();
    let entering_fight = state.is_fighting() && *previous_phase != Some(state.phase);
    *previous_phase = Some(state.phase);
    for (_entity, fighter, action, transform, mut status) in &mut fighters {
        if entering_fight {
            *status = DrunkStatus::default();
        }
        if !state.is_fighting()
            || !state.fighter_can_participate(fighter.id)
            || matches!(
                action.action,
                FighterAction::RingOut | FighterAction::Respawning
            )
        {
            status.remaining = 0.0;
            status.bubble_timer = 0.0;
            continue;
        }
        if hitstop.active() || !status.active() {
            continue;
        }

        status.remaining = (status.remaining - dt).max(0.0);
        if status.remaining <= 0.0 {
            status.bubble_timer = 0.0;
            continue;
        }

        status.bubble_timer -= dt;
        while status.bubble_timer <= 0.0 {
            spawn_drunk_bubble(
                &mut commands,
                &effect_assets,
                transform.translation,
                fighter.id,
                status.bubble_phase,
            );
            status.bubble_timer += DRUNK_BUBBLE_CADENCE;
            status.bubble_phase += 1.0;
        }
    }
}

pub(crate) struct PlayerInputTrackers {
    dash: [DashTapTracker; FIGHTER_COUNT],
    guard: [GuardChordTracker; FIGHTER_COUNT],
    stick_flick: [StickFlickTracker; FIGHTER_COUNT],
    assignment: [LocalInputAssignment; FIGHTER_COUNT],
}

impl Default for PlayerInputTrackers {
    fn default() -> Self {
        Self {
            dash: std::array::from_fn(|_| DashTapTracker::default()),
            guard: std::array::from_fn(|_| GuardChordTracker::default()),
            stick_flick: std::array::from_fn(|_| StickFlickTracker::default()),
            assignment: [LocalInputAssignment::Unassigned; FIGHTER_COUNT],
        }
    }
}

impl PlayerInputTrackers {
    fn reset_if_assignment_changed(&mut self, slot_index: usize, assignment: LocalInputAssignment) {
        if self.assignment[slot_index] == assignment {
            return;
        }
        self.assignment[slot_index] = assignment;
        self.dash[slot_index] = DashTapTracker::default();
        self.guard[slot_index] = GuardChordTracker::default();
        self.stick_flick[slot_index] = StickFlickTracker::default();
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct DeviceActionSample {
    movement: Vec2,
    movement_just: [bool; 4],
    aim_held: bool,
    jump_just: bool,
    dash_just: bool,
    light_just: bool,
    light_held: bool,
    heavy_just: bool,
    heavy_held: bool,
    heavy_released: bool,
    grab_just: bool,
    grab_held: bool,
    guard_held: bool,
    ultimate_just: bool,
    special_just: bool,
    special_kind: Option<SpecialInputKind>,
}

fn keyboard_action_sample(
    keys: &ButtonInput<KeyCode>,
    bindings: PlayerControlBindings,
    reserve_camera_inputs: bool,
) -> DeviceActionSample {
    let camera_movement_blocked =
        reserve_camera_inputs && camera_shift_pressed(keys) && uses_camera_arrow_keys(bindings);
    let light_blocked =
        reserve_camera_inputs && camera_shift_pressed(keys) && bindings.light == KeyCode::KeyC;
    let special_just = SHARED_SPECIALS_ENABLED && keys.just_pressed(bindings.special);
    let special_kind = special_just.then(|| {
        if keys.pressed(bindings.light) {
            SpecialInputKind::Trap
        } else if keys.pressed(bindings.aim_grab) {
            SpecialInputKind::Shockwave
        } else if keys.pressed(bindings.heavy) {
            SpecialInputKind::Hazard
        } else {
            SpecialInputKind::Projectile
        }
    });
    DeviceActionSample {
        movement: if camera_movement_blocked {
            Vec2::ZERO
        } else {
            key_axis(
                keys,
                bindings.left,
                bindings.right,
                bindings.down,
                bindings.up,
            )
        },
        movement_just: if camera_movement_blocked {
            [false; 4]
        } else {
            [
                keys.just_pressed(bindings.left),
                keys.just_pressed(bindings.right),
                keys.just_pressed(bindings.down),
                keys.just_pressed(bindings.up),
            ]
        },
        aim_held: keys.pressed(bindings.aim_grab),
        jump_just: keys.just_pressed(bindings.jump),
        light_just: keys.just_pressed(bindings.light) && !light_blocked,
        light_held: keys.pressed(bindings.light) && !light_blocked,
        heavy_just: keys.just_pressed(bindings.heavy),
        heavy_held: keys.pressed(bindings.heavy),
        heavy_released: keys.just_released(bindings.heavy),
        grab_just: keys.just_pressed(bindings.aim_grab),
        grab_held: keys.pressed(bindings.aim_grab),
        special_just,
        special_kind,
        ..default()
    }
}

fn gamepad_action_sample(gamepad: &Gamepad) -> DeviceActionSample {
    let dpad = gamepad.dpad();
    let stick = apply_gamepad_movement_deadzone(gamepad.left_stick());
    let movement = if dpad.length_squared() > 0.0 {
        Vec2::new(dpad.x, -dpad.y).normalize_or_zero()
    } else {
        Vec2::new(stick.x, -stick.y)
    };
    let special_just = SHARED_SPECIALS_ENABLED && gamepad.just_pressed(GamepadButton::RightTrigger);
    let special_kind = special_just.then(|| {
        if gamepad.pressed(GamepadButton::LeftTrigger) {
            SpecialInputKind::Trap
        } else if gamepad.pressed(GamepadButton::North) {
            SpecialInputKind::Hazard
        } else if gamepad.pressed(GamepadButton::East) {
            SpecialInputKind::Shockwave
        } else {
            SpecialInputKind::Projectile
        }
    });

    DeviceActionSample {
        movement,
        movement_just: [
            gamepad.just_pressed(GamepadButton::DPadLeft),
            gamepad.just_pressed(GamepadButton::DPadRight),
            gamepad.just_pressed(GamepadButton::DPadDown),
            gamepad.just_pressed(GamepadButton::DPadUp),
        ],
        aim_held: gamepad.pressed(GamepadButton::East),
        jump_just: gamepad.just_pressed(GamepadButton::South),
        dash_just: gamepad.just_pressed(GamepadButton::RightTrigger2),
        light_just: gamepad.just_pressed(GamepadButton::West),
        light_held: gamepad.pressed(GamepadButton::West),
        heavy_just: gamepad.just_pressed(GamepadButton::North),
        heavy_held: gamepad.pressed(GamepadButton::North),
        heavy_released: gamepad.just_released(GamepadButton::North),
        grab_just: gamepad.just_pressed(GamepadButton::East),
        grab_held: gamepad.pressed(GamepadButton::East),
        guard_held: gamepad.pressed(GamepadButton::LeftTrigger),
        ultimate_just: gamepad.just_pressed(GamepadButton::LeftTrigger2),
        special_just,
        special_kind,
    }
}

fn apply_gamepad_movement_deadzone(axis: Vec2) -> Vec2 {
    let magnitude = axis.length();
    if magnitude <= GAMEPAD_MOVEMENT_DEADZONE {
        return Vec2::ZERO;
    }
    let scaled_magnitude =
        ((magnitude - GAMEPAD_MOVEMENT_DEADZONE) / (1.0 - GAMEPAD_MOVEMENT_DEADZONE)).min(1.0);
    axis.normalize_or_zero() * scaled_magnitude
}

fn collect_device_player_input(
    sample: DeviceActionSample,
    now: f32,
    camera_yaw: f32,
    dash_taps: &mut DashTapTracker,
    guard_chord: &mut GuardChordTracker,
    stick_flick: &mut StickFlickTracker,
    input: &mut FighterInput,
) {
    let chord = resolve_guard_chord_input(
        guard_chord,
        sample.light_just,
        sample.heavy_just,
        sample.grab_just,
        sample.light_held,
        sample.heavy_held,
        sample.grab_held,
        now,
    );

    input.movement = camera_relative_direction(sample.movement, camera_yaw).normalize_or_zero();
    input.aim = sample.aim_held;
    input.jump = sample.jump_just;
    input.dash = sample.dash_just
        || movement_double_tapped(sample.movement_just, now, dash_taps)
        || stick_flick.update(sample.movement);
    input.light = chord.light;
    input.light_held = sample.light_held;
    input.raw_light_pressed = sample.light_just;
    input.heavy = chord.heavy;
    input.heavy_held = sample.heavy_held;
    input.raw_heavy_pressed = sample.heavy_just;
    input.heavy_released = sample.heavy_released;
    input.grab = chord.grab;
    input.guard = sample.guard_held || chord.guard;
    input.ultimate = sample.ultimate_just || chord.ultimate;
    input.special = SHARED_SPECIALS_ENABLED && sample.special_just;
    input.special_kind = input.special.then_some(sample.special_kind).flatten();
}

#[cfg(test)]
fn collect_bound_player_input(
    keys: &ButtonInput<KeyCode>,
    now: f32,
    camera_yaw: f32,
    bindings: PlayerControlBindings,
    dash_taps: &mut DashTapTracker,
    guard_chord: &mut GuardChordTracker,
    reserve_camera_inputs: bool,
    input: &mut FighterInput,
) {
    let mut stick_flick = StickFlickTracker::default();
    collect_device_player_input(
        keyboard_action_sample(keys, bindings, reserve_camera_inputs),
        now,
        camera_yaw,
        dash_taps,
        guard_chord,
        &mut stick_flick,
        input,
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GuardChordButton {
    Light,
    Heavy,
    Grab,
}

#[derive(Clone, Copy)]
struct GuardChordPending {
    button: GuardChordButton,
    started_at: f32,
}

#[derive(Default)]
pub(crate) struct GuardChordTracker {
    pending: Option<GuardChordPending>,
    guard_latched: bool,
    ultimate_latched: bool,
    light_pressed_at: Option<f32>,
    heavy_pressed_at: Option<f32>,
    grab_pressed_at: Option<f32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct GuardChordOutput {
    light: bool,
    heavy: bool,
    grab: bool,
    guard: bool,
    ultimate: bool,
}

fn resolve_guard_chord_input(
    tracker: &mut GuardChordTracker,
    light_just: bool,
    heavy_just: bool,
    grab_just: bool,
    light_held: bool,
    heavy_held: bool,
    grab_held: bool,
    now: f32,
) -> GuardChordOutput {
    record_chord_press_times(tracker, light_just, heavy_just, grab_just, now);
    if tracker.ultimate_latched {
        if light_held && heavy_held && grab_held {
            return default();
        }
        tracker.ultimate_latched = false;
    }

    if ultimate_chord_pressed(tracker, light_held, heavy_held, grab_held, now) {
        return latch_ultimate_chord(tracker);
    }

    if tracker.guard_latched {
        if light_held && heavy_held {
            return GuardChordOutput {
                guard: true,
                ..default()
            };
        }
        tracker.guard_latched = false;
        tracker.pending = None;
        return default();
    }

    if light_just && heavy_just && !grab_just {
        return latch_guard_chord(tracker);
    }

    if let Some(pending) = tracker.pending {
        let elapsed = (now - pending.started_at).max(0.0);
        let opposite_arrived = match pending.button {
            GuardChordButton::Light => heavy_just && light_held,
            GuardChordButton::Heavy => light_just && heavy_held,
            GuardChordButton::Grab => false,
        };
        if opposite_arrived && elapsed <= GUARD_CHORD_GRACE {
            return latch_guard_chord(tracker);
        }

        if elapsed >= GUARD_CHORD_GRACE - f32::EPSILON {
            tracker.pending = match (pending.button, light_just, heavy_just, grab_just) {
                (_, _, _, true) => Some(GuardChordPending {
                    button: GuardChordButton::Grab,
                    started_at: now,
                }),
                (GuardChordButton::Light, _, true, _) => Some(GuardChordPending {
                    button: GuardChordButton::Heavy,
                    started_at: now,
                }),
                (GuardChordButton::Heavy, true, _, _) => Some(GuardChordPending {
                    button: GuardChordButton::Light,
                    started_at: now,
                }),
                (GuardChordButton::Grab, true, _, _) => Some(GuardChordPending {
                    button: GuardChordButton::Light,
                    started_at: now,
                }),
                (GuardChordButton::Grab, _, true, _) => Some(GuardChordPending {
                    button: GuardChordButton::Heavy,
                    started_at: now,
                }),
                _ => None,
            };
            return match pending.button {
                GuardChordButton::Light => GuardChordOutput {
                    light: true,
                    ..default()
                },
                GuardChordButton::Heavy => GuardChordOutput {
                    heavy: true,
                    ..default()
                },
                GuardChordButton::Grab => default(),
            };
        }

        return default();
    }

    if grab_just {
        tracker.pending = Some(GuardChordPending {
            button: GuardChordButton::Grab,
            started_at: now,
        });
    } else if light_just && heavy_just {
        return latch_guard_chord(tracker);
    } else if light_just {
        tracker.pending = Some(GuardChordPending {
            button: GuardChordButton::Light,
            started_at: now,
        });
    } else if heavy_just {
        tracker.pending = Some(GuardChordPending {
            button: GuardChordButton::Heavy,
            started_at: now,
        });
    }

    default()
}

fn record_chord_press_times(
    tracker: &mut GuardChordTracker,
    light_just: bool,
    heavy_just: bool,
    grab_just: bool,
    now: f32,
) {
    if light_just {
        tracker.light_pressed_at = Some(now);
    }
    if heavy_just {
        tracker.heavy_pressed_at = Some(now);
    }
    if grab_just {
        tracker.grab_pressed_at = Some(now);
    }
}

fn ultimate_chord_pressed(
    tracker: &GuardChordTracker,
    light_held: bool,
    heavy_held: bool,
    grab_held: bool,
    now: f32,
) -> bool {
    if !(light_held && heavy_held && grab_held) {
        return false;
    }
    let (Some(light), Some(heavy)) = (tracker.light_pressed_at, tracker.heavy_pressed_at) else {
        return false;
    };
    let light_heavy_latest = light.max(heavy);
    if (light - heavy).abs() <= GUARD_CHORD_GRACE && now - light_heavy_latest <= GUARD_CHORD_GRACE {
        return true;
    }
    let (Some(light), Some(heavy), Some(grab)) = (
        tracker.light_pressed_at,
        tracker.heavy_pressed_at,
        tracker.grab_pressed_at,
    ) else {
        return false;
    };
    let earliest = light.min(heavy).min(grab);
    let latest = light.max(heavy).max(grab);
    latest - earliest <= GUARD_CHORD_GRACE
}

fn latch_ultimate_chord(tracker: &mut GuardChordTracker) -> GuardChordOutput {
    tracker.pending = None;
    tracker.guard_latched = false;
    tracker.ultimate_latched = true;
    GuardChordOutput {
        ultimate: true,
        ..default()
    }
}

fn latch_guard_chord(tracker: &mut GuardChordTracker) -> GuardChordOutput {
    tracker.pending = None;
    tracker.guard_latched = true;
    GuardChordOutput {
        guard: true,
        ..default()
    }
}

#[derive(Default)]
pub(crate) struct DashTapTracker {
    left: Option<f32>,
    right: Option<f32>,
    down: Option<f32>,
    up: Option<f32>,
}

fn movement_double_tapped(just_pressed: [bool; 4], now: f32, tracker: &mut DashTapTracker) -> bool {
    double_tap_direction(just_pressed[0], now, &mut tracker.left)
        || double_tap_direction(just_pressed[1], now, &mut tracker.right)
        || double_tap_direction(just_pressed[2], now, &mut tracker.down)
        || double_tap_direction(just_pressed[3], now, &mut tracker.up)
}

fn double_tap_direction(just_pressed: bool, now: f32, last_tap: &mut Option<f32>) -> bool {
    if !just_pressed {
        return false;
    }

    let dash = last_tap.is_some_and(|last| now - last <= DOUBLE_TAP_DASH_WINDOW);
    *last_tap = Some(now);
    dash
}

#[derive(Default)]
struct StickFlickTracker {
    engaged: bool,
}

impl StickFlickTracker {
    fn update(&mut self, movement: Vec2) -> bool {
        let magnitude = movement.length();
        if magnitude <= f32::EPSILON {
            self.engaged = false;
            return false;
        }
        if self.engaged || magnitude < GAMEPAD_FLICK_DASH_THRESHOLD {
            return false;
        }
        self.engaged = true;
        true
    }
}

#[cfg(test)]
fn player_movement_input(
    keys: &ButtonInput<KeyCode>,
    camera_yaw: f32,
    bindings: PlayerControlBindings,
    reserve_camera_inputs: bool,
) -> Vec2 {
    if reserve_camera_inputs && camera_shift_pressed(keys) && uses_camera_arrow_keys(bindings) {
        return Vec2::ZERO;
    }

    let raw = key_axis(
        keys,
        bindings.left,
        bindings.right,
        bindings.down,
        bindings.up,
    );
    camera_relative_direction(raw, camera_yaw).normalize_or_zero()
}

#[cfg(test)]
fn player_dash_input(
    keys: &ButtonInput<KeyCode>,
    now: f32,
    tracker: &mut DashTapTracker,
    bindings: PlayerControlBindings,
    reserve_camera_inputs: bool,
) -> bool {
    let sample = keyboard_action_sample(keys, bindings, reserve_camera_inputs);
    movement_double_tapped(sample.movement_just, now, tracker)
}

fn camera_shift_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight)
}

fn uses_camera_arrow_keys(bindings: PlayerControlBindings) -> bool {
    [bindings.left, bindings.right, bindings.down, bindings.up]
        .into_iter()
        .any(|key| {
            matches!(
                key,
                KeyCode::ArrowLeft | KeyCode::ArrowRight | KeyCode::ArrowDown | KeyCode::ArrowUp
            )
        })
}

fn key_axis(
    keys: &ButtonInput<KeyCode>,
    left: KeyCode,
    right: KeyCode,
    down: KeyCode,
    up: KeyCode,
) -> Vec2 {
    let mut axis = Vec2::ZERO;
    if keys.pressed(left) {
        axis.x -= 1.0;
    }
    if keys.pressed(right) {
        axis.x += 1.0;
    }
    if keys.pressed(down) {
        axis.y += 1.0;
    }
    if keys.pressed(up) {
        axis.y -= 1.0;
    }
    axis.normalize_or_zero()
}

fn pressed_raw_technique_button(input: &FighterInput) -> Option<TechniqueButton> {
    if input.ultimate {
        Some(TechniqueButton::Ultimate)
    } else if input.grab {
        Some(TechniqueButton::Grab)
    } else if input.light {
        Some(TechniqueButton::A)
    } else if input.heavy {
        Some(TechniqueButton::B)
    } else {
        None
    }
}

fn guard_counter_trigger_pressed(input: &FighterInput) -> bool {
    (input.raw_light_pressed ^ input.raw_heavy_pressed) && !(input.light_held && input.heavy_held)
}

fn jump_chord_blocked(input: &FighterInput) -> bool {
    input.guard || input.ultimate || input.grab
}

fn jump_heavy_pressed(input: &FighterInput) -> bool {
    !jump_chord_blocked(input) && (input.heavy || input.heavy_held)
}

fn jump_light_pressed(input: &FighterInput) -> bool {
    !jump_chord_blocked(input) && !jump_heavy_pressed(input) && (input.light || input.light_held)
}

fn jump_attack_button(input: &FighterInput) -> Option<TechniqueButton> {
    if jump_heavy_pressed(input) {
        Some(TechniqueButton::B)
    } else if jump_light_pressed(input) {
        Some(TechniqueButton::A)
    } else {
        None
    }
}

fn queue_air_attack(motor: &mut FighterMotor, button: TechniqueButton) {
    motor.queued_air_attack = Some(button);
    motor.queued_air_attack_timer = JUMP_ATTACK_QUEUE_GRACE;
}

fn clear_queued_air_attack(motor: &mut FighterMotor) {
    motor.queued_air_attack = None;
    motor.queued_air_attack_timer = 0.0;
}

fn clear_bee_air_dash_state(motor: &mut FighterMotor) {
    motor.bee_air_dash_motion_active = false;
    motor.bee_air_dash_shot_available = false;
}

fn tick_queued_air_attack(motor: &mut FighterMotor, dt: f32) {
    if motor.queued_air_attack.is_some() {
        motor.queued_air_attack_timer = (motor.queued_air_attack_timer - dt).max(0.0);
        if motor.queued_air_attack_timer == 0.0 {
            clear_queued_air_attack(motor);
        }
    }
}

fn queued_air_attack_ready(motor: &FighterMotor) -> bool {
    motor.queued_air_attack.is_some()
        && !motor.grounded
        && !motor.air_attack_used
        && motor.jump_takeoff_timer <= JUMP_ATTACK_QUEUE_TAKEOFF_REMAINING
}

fn queue_chained_followup(
    action: &mut FighterActionState,
    input: &FighterInput,
    loadout: LoadoutContext,
    grounded: bool,
    dt: f32,
    feel: &CombatFeelTuning,
    character_catalog: &CharacterMoveCatalog,
) {
    if action.queued_technique.is_some() {
        return;
    }

    if !buffer_pressed_technique_button(action, input) && action.buffered_button.is_some() {
        action.buffered_button_elapsed += dt;
    }

    let Some(button) = action.buffered_button else {
        return;
    };

    let buffer_secs =
        technique_definition_for_action_state_with_feel(action, loadout, feel, character_catalog)
            .map_or(0.0, |technique| technique.input_buffer_ms as f32 / 1000.0);
    if action.buffered_button_elapsed > buffer_secs {
        clear_buffered_button(action);
        return;
    }

    let Some(next) = chained_technique_for_context_in_catalog(
        TechniqueMatchContext {
            previous: action.technique_id,
            button,
            elapsed: action.elapsed,
            style: loadout.style,
            loadout,
            grounded,
            confirmed_hit: action.confirmed_hit,
            cancel_window_open: action.cancel_window_open,
            branch_window_open: action.branch_window_open,
            current_action: action.action,
        },
        character_catalog,
    ) else {
        return;
    };

    action.queued_combo = true;
    action.queued_technique = Some(next.id);
    action.queued_button = Some(button);
    clear_buffered_button(action);
}

fn buffer_pressed_technique_button(action: &mut FighterActionState, input: &FighterInput) -> bool {
    let Some(button) = pressed_raw_technique_button(input) else {
        return false;
    };

    action.buffered_button = Some(button);
    action.buffered_button_elapsed = 0.0;
    true
}

fn buffer_hitstop_followup_input(
    action: &mut FighterActionState,
    input: &FighterInput,
    loadout: LoadoutContext,
    feel: &CombatFeelTuning,
    character_catalog: &CharacterMoveCatalog,
) {
    if action.queued_technique.is_some() {
        return;
    }
    let Some(technique) =
        technique_definition_for_action_state_with_feel(action, loadout, feel, character_catalog)
    else {
        return;
    };
    if technique.input_buffer_ms == 0 {
        return;
    }

    buffer_pressed_technique_button(action, input);
}

fn clear_buffered_button(action: &mut FighterActionState) {
    action.buffered_button = None;
    action.buffered_button_elapsed = 0.0;
}

fn movement_input_direction(movement: Vec2) -> Option<Vec3> {
    (movement.length_squared() > 0.01)
        .then(|| Vec3::new(movement.x, 0.0, movement.y).normalize_or_zero())
}

fn update_facing_from_movement(motor: &mut FighterMotor, input: &FighterInput) {
    if !input.aim {
        if let Some(direction) = movement_input_direction(input.movement) {
            motor.facing = direction;
        }
    }
}

fn dash_finisher_for_input(
    input: &FighterInput,
    loadout: LoadoutContext,
    character_catalog: &CharacterMoveCatalog,
) -> Option<TechniqueId> {
    if input.raw_heavy_pressed || input.heavy {
        character_catalog.slot_technique(loadout.character, CharacterMoveSlot::DashHeavy)
    } else if input.light {
        character_catalog.slot_technique(loadout.character, CharacterMoveSlot::DashLight)
    } else {
        None
    }
}

fn start_dash_finisher_from_dash(
    motor: &mut FighterMotor,
    action: &mut FighterActionState,
    input: &FighterInput,
    loadout: LoadoutContext,
    character_catalog: &CharacterMoveCatalog,
) -> bool {
    let Some(next) = dash_finisher_for_input(input, loadout, character_catalog) else {
        return false;
    };
    if loadout.character == CharacterKind::Penguin
        && matches!(
            next,
            TechniqueId::PenguinDashAttack | TechniqueId::PenguinDashHeavy
        )
    {
        if let Some(direction) = movement_input_direction(input.movement) {
            motor.facing = direction;
        }
    }
    if matches!(
        next,
        TechniqueId::ChickDashAttack | TechniqueId::ChickDashHeavy
    ) {
        let facing = motor.facing.normalize_or_zero();
        let backstep = -Vec2::new(facing.x, facing.z)
            * match next {
                TechniqueId::ChickDashAttack => CHICK_DASH_C_BACKSTEP_SPEED,
                TechniqueId::ChickDashHeavy => CHICK_DASH_X_BACKSTEP_SPEED,
                _ => unreachable!(),
            };
        set_planar_velocity(motor, backstep);
    } else if !(loadout.character == CharacterKind::Penguin
        && next == TechniqueId::PenguinDashAttack)
    {
        let extra_impulse = dash_finisher_extra_impulse(next, loadout);
        if extra_impulse == 0.0 {
            motor.velocity.x = 0.0;
            motor.velocity.z = 0.0;
        } else {
            motor.velocity.x += motor.facing.x * extra_impulse;
            motor.velocity.z += motor.facing.z * extra_impulse;
        }
    }
    start_technique_by_id(action, next, loadout, character_catalog);
    true
}

fn start_chick_dash_finisher_from_light_attack(
    motor: &mut FighterMotor,
    action: &mut FighterActionState,
    input: &FighterInput,
    loadout: LoadoutContext,
    character_catalog: &CharacterMoveCatalog,
) -> bool {
    if loadout.character != CharacterKind::Chick
        || action.technique_id != Some(TechniqueId::ChickLight1)
        || !motor.grounded
        || !input.dash
    {
        return false;
    }

    let dash_input = FighterInput {
        movement: input.movement,
        light: input.light || input.light_held || input.raw_light_pressed,
        heavy: input.heavy || input.heavy_held,
        raw_heavy_pressed: input.raw_heavy_pressed,
        ..default()
    };
    start_dash_finisher_from_dash(motor, action, &dash_input, loadout, character_catalog)
}

fn try_start_penguin_dash_ultimate(
    motor: &mut FighterMotor,
    stats: &mut FighterStats,
    action: &mut FighterActionState,
    input: &FighterInput,
    loadout: LoadoutContext,
    character_catalog: &CharacterMoveCatalog,
) -> bool {
    let requested = input.ultimate || penguin_dash_ultimate_shortcut(input, loadout);
    if loadout.character != CharacterKind::Penguin
        || !requested
        || !can_start_ultimate(motor, stats)
    {
        return false;
    }

    let Some(technique) =
        technique_slot_for_loadout(CharacterMoveSlot::UltimateRush, loadout, character_catalog)
    else {
        return false;
    };

    start_ultimate(motor, stats, action, technique);
    motor.velocity.x += motor.facing.x * DASH_ATTACK_EXTRA_IMPULSE;
    motor.velocity.z += motor.facing.z * DASH_ATTACK_EXTRA_IMPULSE;
    true
}

fn try_start_ultimate_from_input(
    motor: &mut FighterMotor,
    stats: &mut FighterStats,
    action: &mut FighterActionState,
    input: &FighterInput,
    loadout: LoadoutContext,
    character_catalog: &CharacterMoveCatalog,
) -> bool {
    if !ultimate_input_requested(input, loadout) || !can_start_ultimate(motor, stats) {
        return false;
    }

    let Some(technique) = technique_slot_for_loadout(
        CharacterMoveSlot::UltimateStartup,
        loadout,
        character_catalog,
    ) else {
        return false;
    };

    start_ultimate(motor, stats, action, technique);
    true
}

fn ultimate_input_requested(input: &FighterInput, loadout: LoadoutContext) -> bool {
    input.ultimate || held_ground_ultimate_shortcut(input, loadout)
}

fn held_ground_ultimate_shortcut(input: &FighterInput, loadout: LoadoutContext) -> bool {
    matches!(
        loadout.character,
        CharacterKind::Penguin | CharacterKind::Chick
    ) && input.aim
        && input.light_held
        && input.heavy_held
}

fn penguin_dash_ultimate_shortcut(input: &FighterInput, loadout: LoadoutContext) -> bool {
    loadout.character == CharacterKind::Penguin && input.light_held && input.heavy_held
}

fn dash_finisher_extra_impulse(next: TechniqueId, loadout: LoadoutContext) -> f32 {
    if loadout.character == CharacterKind::Pig && next == TechniqueId::PigComboFinisher {
        0.0
    } else {
        DASH_ATTACK_EXTRA_IMPULSE
    }
}

fn pig_heavy_full_charge_secs() -> f32 {
    PIG_HEAVY_FULL_CHARGE_MS as f32 / 1000.0
}

fn pig_heavy_attack_hold_secs() -> f32 {
    PIG_HEAVY_ATTACK_MS as f32 / 1000.0
}

fn pig_heavy_body_scale(charge_elapsed: f32, released: bool) -> Vec3 {
    let charge = (charge_elapsed / pig_heavy_full_charge_secs()).clamp(0.0, 1.0);
    if released {
        Vec3::new(1.18, 0.9, 1.22)
    } else {
        Vec3::new(
            1.06 + charge * 0.12,
            1.0 - charge * 0.08,
            1.08 + charge * 0.14,
        )
    }
}

fn tick_pig_heavy_charge(
    action: &mut FighterActionState,
    input: &FighterInput,
    dt: f32,
) -> (f32, f32) {
    let before = action.charge_elapsed;
    if action.technique_id != Some(TechniqueId::PigHeavy) || action.charge_release_requested {
        return (before, before);
    }

    if input.heavy_held {
        action.charge_elapsed = (action.charge_elapsed + dt).min(pig_heavy_full_charge_secs());
    }
    if input.heavy_released || !input.heavy_held {
        action.charge_release_requested = true;
    }
    (before, action.charge_elapsed)
}

fn pig_dash_heavy_charge_active(
    input: &FighterInput,
    action: &FighterActionState,
    loadout: LoadoutContext,
) -> bool {
    loadout.character == CharacterKind::Pig
        && (input.heavy_held || input.heavy_released || action.charge_elapsed > 0.0)
}

fn start_pig_dash_heavy_release(
    action: &mut FighterActionState,
    loadout: LoadoutContext,
    character_catalog: &CharacterMoveCatalog,
) {
    let charge_elapsed = action.charge_elapsed.min(pig_heavy_full_charge_secs());
    if let Some(technique) =
        technique_slot_for_loadout(CharacterMoveSlot::DashHeavy, loadout, character_catalog)
    {
        set_technique_action(action, technique);
        action.charge_elapsed = charge_elapsed;
        action.charge_release_requested = true;
    }
}

fn apply_dash_hold_motion(
    motor: &mut FighterMotor,
    movement: Vec2,
    dash_scale: f32,
    dt: f32,
) -> Option<Vec3> {
    let direction = movement_input_direction(movement)?;
    Some(apply_dash_hold_motion_direction(
        motor, direction, dash_scale, dt,
    ))
}

fn apply_dash_hold_motion_direction(
    motor: &mut FighterMotor,
    direction: Vec3,
    dash_scale: f32,
    dt: f32,
) -> Vec3 {
    motor.facing = direction;

    let current = Vec2::new(motor.velocity.x, motor.velocity.z);
    let target = Vec2::new(direction.x, direction.z) * DASH_HOLD_SPEED * dash_scale;
    let delta = target - current;
    let max_delta = DASH_HOLD_ACCEL * dash_scale * dt;
    let next = if delta.length() > max_delta {
        current + delta.normalize_or_zero() * max_delta
    } else {
        target
    };
    motor.velocity.x = next.x;
    motor.velocity.z = next.y;
    direction
}

fn dash_trail_tick(motor: &mut FighterMotor, dt: f32) -> bool {
    motor.dash_trail_timer -= dt;
    if motor.dash_trail_timer > 0.0 {
        return false;
    }

    motor.dash_trail_timer = DASH_TRAIL_REPEAT;
    true
}

fn dash_should_stop(elapsed: f32, movement: Vec2) -> bool {
    movement_input_direction(movement).is_none() && elapsed >= DASH_DURATION
}

fn planar_velocity(motor: &FighterMotor) -> Vec2 {
    Vec2::new(motor.velocity.x, motor.velocity.z)
}

fn set_planar_velocity(motor: &mut FighterMotor, velocity: Vec2) {
    motor.velocity.x = velocity.x;
    motor.velocity.z = velocity.y;
}

fn penguin_ice_slide_cleared_by_action(action: FighterAction) -> bool {
    matches!(
        action,
        FighterAction::Hitstun
            | FighterAction::Knockdown
            | FighterAction::Grabbed
            | FighterAction::GetUp
            | FighterAction::RingOut
            | FighterAction::Respawning
    )
}

fn normalized_planar_or_forward(value: Vec3) -> Vec3 {
    let planar = Vec3::new(value.x, 0.0, value.z).normalize_or_zero();
    if planar.length_squared() > 0.01 {
        planar
    } else {
        Vec3::Z
    }
}

fn clear_penguin_hard_ice_slide_state(motor: &mut FighterMotor) {
    motor.penguin_ice_slide_direction = None;
    motor.penguin_ice_slide_speed = 0.0;
}

fn penguin_hard_ice_accelerated_slide_speed(entry_speed: f32) -> f32 {
    (entry_speed * PENGUIN_HARD_ICE_ENTRY_SPEED_MULTIPLIER).clamp(
        PENGUIN_HARD_ICE_SLIDE_MIN_SPEED,
        PENGUIN_HARD_ICE_SLIDE_MAX_SPEED,
    )
}

fn penguin_hard_ice_slide_direction(motor: &mut FighterMotor, desired: Vec3) -> Vec3 {
    if let Some(direction) = motor.penguin_ice_slide_direction {
        return normalized_planar_or_forward(direction);
    }

    let planar = planar_velocity(motor);
    let velocity_direction = (planar.length() > PENGUIN_HARD_ICE_ENTRY_SPEED_THRESHOLD)
        .then(|| Vec3::new(planar.x, 0.0, planar.y).normalize_or_zero());
    let direction = velocity_direction
        .filter(|direction| direction.length_squared() > 0.01)
        .unwrap_or_else(|| {
            if desired.length_squared() > 0.01 {
                normalized_planar_or_forward(desired)
            } else {
                normalized_planar_or_forward(motor.facing)
            }
        });
    motor.penguin_ice_slide_direction = Some(direction);
    motor.penguin_ice_slide_speed = penguin_hard_ice_accelerated_slide_speed(planar.length());
    direction
}

fn update_penguin_hard_ice_slide_state(
    motor: &mut FighterMotor,
    desired: &mut Vec3,
    action: FighterAction,
    active: bool,
) -> Option<Vec3> {
    if !active || penguin_ice_slide_cleared_by_action(action) {
        clear_penguin_hard_ice_slide_state(motor);
        return None;
    }

    let direction = penguin_hard_ice_slide_direction(motor, *desired);
    *desired = Vec3::ZERO;
    Some(direction)
}

fn force_penguin_hard_ice_slide_velocity(motor: &mut FighterMotor, direction: Vec3) {
    let direction = normalized_planar_or_forward(direction);
    let speed = if motor.penguin_ice_slide_speed > 0.0 {
        motor.penguin_ice_slide_speed
    } else {
        penguin_hard_ice_accelerated_slide_speed(planar_velocity(motor).length())
    };
    set_planar_velocity(motor, Vec2::new(direction.x, direction.z) * speed);
    motor.dash_slide_timer = motor.dash_slide_timer.max(0.28);
    motor.impact_speed_limit_timer = motor.impact_speed_limit_timer.max(0.08);
    motor.impact_speed_limit = motor.impact_speed_limit.max(speed);
}

#[allow(dead_code)]
fn start_dash_slide(motor: &mut FighterMotor) {
    start_dash_slide_with_scale(motor, 1.0);
}

fn start_dash_slide_with_scale(motor: &mut FighterMotor, slide_scale: f32) {
    motor.dash_trail_timer = 0.0;
    motor.dash_jump_carry_timer = 0.0;
    motor.dash_jump_carry_speed_limit = 0.0;
    motor.dash_slide_timer = if planar_velocity(motor).length() > DASH_SLIDE_STOP_SPEED {
        DASH_SLIDE_DURATION * slide_scale
    } else {
        0.0
    };
}

pub(crate) fn cancel_dash_slide_for_action(motor: &mut FighterMotor) {
    if motor.dash_slide_timer <= 0.0 {
        return;
    }

    motor.dash_slide_timer = 0.0;
    let damped = planar_velocity(motor) * DASH_SLIDE_ACTION_DAMPING;
    if damped.length() <= DASH_SLIDE_STOP_SPEED {
        set_planar_velocity(motor, Vec2::ZERO);
    } else {
        set_planar_velocity(motor, damped);
    }
}

fn slide_cancel_requested(input: &FighterInput) -> bool {
    input.jump
        || input.light
        || input.heavy
        || input.grab
        || input.guard
        || input.ultimate
        || input.special
        || input.dash
}

#[derive(Clone, Copy, Debug)]
struct AimTargetSnapshot {
    entity: Entity,
    fighter_id: usize,
    position: Vec3,
    valid: bool,
}

fn planar_aim_direction(from: Vec3, to: Vec3) -> Option<Vec3> {
    let direction = Vec3::new(to.x - from.x, 0.0, to.z - from.z);
    (direction.length_squared() > f32::EPSILON).then(|| direction.normalize())
}

fn aim_target_is_opponent(state: &MatchState, owner_id: usize, target_id: usize) -> bool {
    owner_id != target_id
        && (!state.rules.team_scoring || !state.fighters_share_team(owner_id, target_id))
}

fn select_aim_target(
    owner_id: usize,
    owner_position: Vec3,
    aim_direction: Vec3,
    state: &MatchState,
    snapshots: &[Option<AimTargetSnapshot>; FIGHTER_COUNT],
) -> Option<Entity> {
    let aim_direction = normalized_planar_or_forward(aim_direction);
    let mut best: Option<(Entity, f32, f32)> = None;
    for snapshot in snapshots.iter().flatten().filter(|snapshot| {
        snapshot.valid && aim_target_is_opponent(state, owner_id, snapshot.fighter_id)
    }) {
        let distance_squared = owner_position.distance_squared(snapshot.position);
        if distance_squared > AIM_ACQUIRE_DISTANCE * AIM_ACQUIRE_DISTANCE {
            continue;
        }
        let Some(direction) = planar_aim_direction(owner_position, snapshot.position) else {
            continue;
        };
        let alignment = aim_direction.dot(direction);
        if alignment < AIM_ACQUIRE_COS {
            continue;
        }
        let replace = best.is_none_or(|(_, best_alignment, best_distance_squared)| {
            alignment > best_alignment
                || (alignment == best_alignment && distance_squared < best_distance_squared)
        });
        if replace {
            best = Some((snapshot.entity, alignment, distance_squared));
        }
    }
    best.map(|(entity, _, _)| entity)
}

pub fn apply_aim_assist(
    state: Res<MatchState>,
    mut fighters: Query<(
        Entity,
        &Fighter,
        &FighterInput,
        &mut FighterMotor,
        &mut FighterAimState,
        &Transform,
        &FighterActionState,
    )>,
) {
    let mut snapshots = [None; FIGHTER_COUNT];
    for (entity, fighter, _, _, _, transform, action) in fighters.iter() {
        snapshots[fighter.id] = Some(AimTargetSnapshot {
            entity,
            fighter_id: fighter.id,
            position: transform.translation,
            valid: state.fighter_can_participate(fighter.id)
                && !matches!(
                    action.action,
                    FighterAction::RingOut | FighterAction::Respawning
                ),
        });
    }

    for (_, fighter, input, mut motor, mut aim, transform, action) in &mut fighters {
        let owner_valid = state.fighter_can_participate(fighter.id)
            && !matches!(
                action.action,
                FighterAction::RingOut | FighterAction::Respawning
            );
        if !owner_valid || !input.aim {
            aim.locked_target = None;
            aim.aim_pressed = false;
            continue;
        }

        let movement_direction = movement_input_direction(input.movement);
        let just_pressed = !aim.aim_pressed;
        aim.aim_pressed = true;
        if just_pressed {
            aim.direction = normalized_planar_or_forward(motor.facing);
            if aim.marker_opacity <= 0.001 {
                aim.marker_position = transform.translation
                    + Vec3::Y * AIM_TORSO_HEIGHT
                    + aim.direction * AIM_FREE_DISTANCE;
            }
        }

        let mut broke_lock = false;
        if let Some(target_entity) = aim.locked_target {
            let retained = snapshots.iter().flatten().find(|snapshot| {
                snapshot.entity == target_entity
                    && snapshot.valid
                    && aim_target_is_opponent(&state, fighter.id, snapshot.fighter_id)
                    && transform.translation.distance_squared(snapshot.position)
                        <= AIM_RETAIN_DISTANCE * AIM_RETAIN_DISTANCE
            });
            if let Some(target) = retained {
                let target_direction = planar_aim_direction(transform.translation, target.position)
                    .unwrap_or(aim.direction);
                if movement_direction
                    .is_some_and(|movement| movement.dot(target_direction) < AIM_BREAK_COS)
                {
                    aim.locked_target = None;
                    aim.manual_unlock_count = aim.manual_unlock_count.saturating_add(1);
                    aim.direction = movement_direction.unwrap();
                    broke_lock = true;
                } else {
                    aim.direction = target_direction;
                }
            } else {
                aim.locked_target = None;
            }
        }

        if aim.locked_target.is_none() {
            if let Some(movement) = movement_direction {
                aim.direction = movement;
            }
            if !broke_lock {
                aim.locked_target = select_aim_target(
                    fighter.id,
                    transform.translation,
                    aim.direction,
                    &state,
                    &snapshots,
                );
                if let Some(target) = aim.locked_target.and_then(|target| {
                    snapshots
                        .iter()
                        .flatten()
                        .find(|item| item.entity == target)
                }) {
                    aim.direction = planar_aim_direction(transform.translation, target.position)
                        .unwrap_or(aim.direction);
                }
            }
        }
        motor.facing = aim.direction;
    }
}

#[derive(Clone, Copy)]
struct AimMarkerSnapshot {
    color: Color,
    human: bool,
    valid: bool,
    locked: bool,
    opacity: f32,
    position: Vec3,
}

fn advance_aim_marker_opacity(opacity: f32, held: bool, dt: f32) -> f32 {
    if held {
        (opacity + dt / AIM_MARKER_FADE_IN).min(1.0)
    } else {
        (opacity - dt / AIM_MARKER_FADE_OUT).max(0.0)
    }
}

fn aim_marker_smoothing(dt: f32) -> f32 {
    1.0 - (-AIM_MARKER_SMOOTHING * dt).exp()
}

pub fn update_aim_markers(
    time: Res<Time>,
    state: Res<MatchState>,
    camera: Query<&GlobalTransform, With<crate::camera::ArenaCamera>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut fighters: Query<
        (
            Entity,
            &Fighter,
            &Controller,
            &FighterInput,
            &FighterActionState,
            &Transform,
            &mut FighterAimState,
        ),
        Without<FighterAimMarker>,
    >,
    mut markers: Query<(&FighterAimMarker, &mut Transform, &mut Visibility), Without<Fighter>>,
) {
    let dt = time.delta_secs();
    let mut target_positions = [None; FIGHTER_COUNT];
    for (entity, fighter, _, _, action, transform, _) in fighters.iter() {
        if state.fighter_can_participate(fighter.id)
            && !matches!(
                action.action,
                FighterAction::RingOut | FighterAction::Respawning
            )
        {
            target_positions[fighter.id] = Some((entity, transform.translation));
        }
    }

    let mut visual_snapshots = [None; FIGHTER_COUNT];
    for (_, fighter, controller, input, action, transform, mut aim) in &mut fighters {
        let valid = state.is_fighting()
            && state.fighter_can_participate(fighter.id)
            && controller.is_human()
            && !matches!(
                action.action,
                FighterAction::RingOut | FighterAction::Respawning
            );
        if !valid {
            aim.locked_target = None;
            aim.aim_pressed = false;
            aim.marker_opacity = 0.0;
        } else {
            let target_position = aim
                .locked_target
                .and_then(|entity| {
                    target_positions
                        .iter()
                        .flatten()
                        .find_map(|(target_entity, position)| {
                            (*target_entity == entity).then_some(*position)
                        })
                })
                .unwrap_or(transform.translation + aim.direction * AIM_FREE_DISTANCE);
            if input.aim {
                aim.marker_opacity = advance_aim_marker_opacity(aim.marker_opacity, true, dt);
                let smoothing = aim_marker_smoothing(dt);
                aim.marker_position = aim
                    .marker_position
                    .lerp(target_position + Vec3::Y * AIM_TORSO_HEIGHT, smoothing);
            } else {
                aim.marker_opacity = advance_aim_marker_opacity(aim.marker_opacity, false, dt);
            }
        }
        visual_snapshots[fighter.id] = Some(AimMarkerSnapshot {
            color: fighter.color,
            human: controller.is_human(),
            valid,
            locked: aim.locked_target.is_some(),
            opacity: aim.marker_opacity,
            position: aim.marker_position,
        });
    }

    let camera_position = camera.single().ok().map(GlobalTransform::translation);
    let pulse = 0.88 + (time.elapsed_secs() * 8.0).sin() * 0.12;
    for (marker, mut transform, mut visibility) in &mut markers {
        let Some(snapshot) = visual_snapshots[marker.fighter_id] else {
            *visibility = Visibility::Hidden;
            continue;
        };
        let visible = snapshot.human && snapshot.valid && snapshot.opacity > 0.001;
        *visibility = if visible {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        transform.translation = snapshot.position;
        let scale = AIM_MARKER_BASE_SCALE
            * snapshot.opacity
            * if snapshot.locked {
                1.0 + pulse * 0.08
            } else {
                1.0
            };
        transform.scale = Vec3::splat(scale);
        if let Some(camera_position) = camera_position {
            transform.look_at(camera_position, Vec3::Y);
        }
        if let Some(material) = materials.get_mut(&marker.material) {
            let brightness = if snapshot.locked {
                1.0 + pulse * 0.55
            } else {
                0.8
            };
            material.base_color = snapshot.color.lighter(0.25).with_alpha(snapshot.opacity);
            material.emissive =
                LinearRgba::from(snapshot.color.to_linear()) * brightness * snapshot.opacity;
        }
    }
}

pub fn update_fighter_state(
    time: Res<Time>,
    mut commands: Commands,
    effect_assets: Res<EffectAssets>,
    hitstop: Res<Hitstop>,
    feel: Res<CombatFeelTuning>,
    character_catalog: Res<CharacterMoveCatalog>,
    mut feedback: ResMut<HitEffects>,
    active_chick_skills: Query<&ActiveChickSkill>,
    mut fighters: Query<(
        Entity,
        &mut FighterStats,
        &mut FighterMotor,
        &mut FighterInput,
        &mut FighterActionState,
        &FighterCharacter,
        &FighterStyle,
        &FighterEquipment,
        &Transform,
    )>,
) {
    if hitstop.active() {
        for (_, _, mut motor, input, mut action, character, style, equipment, _) in &mut fighters {
            if motor.guard_counter_window_timer > 0.0 && guard_counter_trigger_pressed(&input) {
                motor.guard_counter_buffered = true;
            }
            let loadout = LoadoutContext::for_character(character.kind, style.kind, equipment.kind);
            buffer_hitstop_followup_input(&mut action, &input, loadout, &feel, &character_catalog);
        }
        return;
    }

    let dt = time.delta_secs();

    for (entity, mut stats, mut motor, input, mut action, character, style, equipment, transform) in
        &mut fighters
    {
        let tuning = style_tuning(style.kind);
        let body = character_catalog.body(character.kind);
        let loadout = LoadoutContext::for_character(character.kind, style.kind, equipment.kind);
        let guard_pressed = tick_guard_input(&mut motor, input.guard, dt);
        tick_queued_air_attack(&mut motor, dt);
        tick_guard_counter_window(&mut motor, dt);
        refresh_technique_runtime(&mut action, loadout, &feel, &character_catalog);
        stats.invulnerability = (stats.invulnerability - dt).max(0.0);
        stats.hud_flash = (stats.hud_flash - dt).max(0.0);
        stats.element_carry_timer = (stats.element_carry_timer - dt).max(0.0);
        if stats.element_carry_timer <= 0.0 || stats.element_carry_strength <= 0.0 {
            stats.element_carry = None;
            stats.element_carry_strength = 0.0;
            stats.element_carry_timer = 0.0;
        } else {
            stats.element_carry_strength = (stats.element_carry_strength - dt * 0.22).max(0.0);
            if stats.element_carry_strength <= 0.0 {
                stats.element_carry = None;
                stats.element_carry_timer = 0.0;
            }
        }

        if matches!(
            action.action,
            FighterAction::RingOut | FighterAction::Respawning
        ) {
            clear_bee_air_dash_state(&mut motor);
            motor.clear_guard_counter_window();
            continue;
        }

        if !guard_counter_action_can_trigger(action.action)
            && action.action != FighterAction::GuardCounter
        {
            motor.clear_guard_counter_window();
        }

        let guard_counter_requested =
            guard_counter_trigger_pressed(&input) || motor.guard_counter_buffered;
        if try_start_guard_counter(
            &mut stats,
            &mut motor,
            &mut action,
            &input,
            transform.translation,
            loadout,
            &character_catalog,
            guard_counter_requested,
        ) {
            continue;
        }

        if motor.dash_slide_timer > 0.0
            && slide_cancel_requested(&input)
            && matches!(
                action.action,
                FighterAction::Idle | FighterAction::Moving | FighterAction::Guarding
            )
        {
            cancel_dash_slide_for_action(&mut motor);
        }

        if action.action == FighterAction::Guarding {
            action.elapsed += dt;
            motor.guard_active_timer += dt;
            motor.velocity.x *= 0.22;
            motor.velocity.z *= 0.22;
            if try_start_ultimate_from_input(
                &mut motor,
                &mut stats,
                &mut action,
                &input,
                loadout,
                &character_catalog,
            ) {
                continue;
            }
            if guard_should_end(&motor, input.guard) {
                finish_guard(&mut motor, &mut action);
                continue;
            }
            continue;
        }

        if matches!(action.action, FighterAction::Hitstun) {
            clear_bee_air_dash_state(&mut motor);
            action.elapsed += dt;
            if !motor.grounded && motor.landing_aftermath.is_some() {
                continue;
            }
            let duration = if let Some(recover_ms) = action.reaction_recover_ms {
                recover_ms as f32 / 1000.0
            } else if stats.health <= 0.0 {
                HITSTUN_HEAVY
            } else {
                HITSTUN_LIGHT
            };
            if action.elapsed >= duration {
                if motor.grounded {
                    set_action(&mut action, FighterAction::Idle);
                } else {
                    set_action(&mut action, FighterAction::Jumping);
                }
            }
            continue;
        }

        if matches!(action.action, FighterAction::Knockdown) {
            clear_bee_air_dash_state(&mut motor);
            action.elapsed += dt;
            let authored_down =
                action.reaction_getup_ms.is_some() || action.reaction_recover_ms.is_some();
            if !authored_down && action.elapsed >= QUICK_STAND_AFTER && input.jump {
                stats.invulnerability = 0.0;
                set_action(&mut action, FighterAction::QuickStand);
                continue;
            }
            if !authored_down && action.elapsed >= QUICK_STAND_AFTER && input.dash {
                let roll_dir = recovery_roll_direction(input.movement, motor.facing);
                motor.velocity.x = roll_dir.x * RECOVERY_ROLL_IMPULSE;
                motor.velocity.z = roll_dir.z * RECOVERY_ROLL_IMPULSE;
                stats.invulnerability = stats.invulnerability.max(RECOVERY_ROLL_INVULNERABLE);
                spawn_dash_trail(
                    &mut commands,
                    &effect_assets,
                    transform.translation,
                    roll_dir,
                );
                set_action(&mut action, FighterAction::RecoveryRoll);
                continue;
            }
            let getup_at = action
                .reaction_getup_ms
                .map(|ms| ms as f32 / 1000.0)
                .unwrap_or(KNOCKDOWN_DURATION);
            if action.elapsed >= getup_at {
                let recover_ms = action.reaction_recover_ms;
                let getup_ms = action.reaction_getup_ms;
                stats.invulnerability = stats.invulnerability.max(GETUP_INVULNERABLE);
                spawn_guard_flash(
                    &mut commands,
                    &effect_assets,
                    transform.translation + Vec3::Y * 1.0,
                );
                feedback.shake = feedback.shake.max(0.12);
                feedback.push_feedback_cue(
                    "reaction_getup_transition",
                    ImpactSource::FighterStrike,
                    44,
                );
                action.action = FighterAction::GetUp;
                action.elapsed = 0.0;
                action.hitbox_spawned = false;
                action.queued_combo = false;
                action.queued_technique = None;
                action.queued_button = None;
                clear_buffered_button(&mut action);
                action.timeline_events_fired = 0;
                action.reaction_getup_ms = None;
                action.reaction_recover_ms = recover_ms
                    .zip(getup_ms)
                    .map(|(recover, getup)| recover.saturating_sub(getup).max(1));
                action.clear_reaction_visual();
            }
            continue;
        }

        if matches!(action.action, FighterAction::GetUp) {
            action.elapsed += dt;
            let duration = action
                .reaction_recover_ms
                .map(|ms| ms as f32 / 1000.0)
                .unwrap_or(GETUP_DURATION);
            if action.elapsed >= duration {
                stats.invulnerability = 0.0;
                feedback.push_feedback_cue(
                    "reaction_recover_control",
                    ImpactSource::FighterStrike,
                    28,
                );
                set_action(&mut action, FighterAction::Idle);
            }
            continue;
        }

        if matches!(action.action, FighterAction::GuardBroken) {
            action.elapsed += dt;
            if action.elapsed >= GUARD_BREAK_DURATION {
                set_action(&mut action, FighterAction::Idle);
            }
            continue;
        }

        if matches!(action.action, FighterAction::UltimateVictim) {
            action.elapsed += dt;
            motor.velocity = Vec3::ZERO;
            continue;
        }

        if matches!(
            action.action,
            FighterAction::GrabHold | FighterAction::Grabbed
        ) {
            action.elapsed += dt;
            motor.velocity.x *= 0.65;
            motor.velocity.z *= 0.65;
            continue;
        }

        if matches!(
            action.action,
            FighterAction::LightAttack1 | FighterAction::LightAttack2
        ) {
            action.elapsed += dt;
            refresh_technique_runtime(&mut action, loadout, &feel, &character_catalog);
            if start_chick_dash_finisher_from_light_attack(
                &mut motor,
                &mut action,
                &input,
                loadout,
                &character_catalog,
            ) {
                continue;
            }
            if chick_light_recall_interrupt_requested(
                entity,
                &action,
                &input,
                active_chick_skills
                    .iter()
                    .map(|skill| (skill.owner, skill.kind)),
            ) {
                start_technique_by_id(
                    &mut action,
                    TechniqueId::ChickLight1,
                    loadout,
                    &character_catalog,
                );
                continue;
            }
            queue_chained_followup(
                &mut action,
                &input,
                loadout,
                motor.grounded,
                dt,
                &feel,
                &character_catalog,
            );
            if let Some(next) = action.queued_technique
                && technique_runtime_for_action_state_with_feel(
                    &action,
                    loadout,
                    &feel,
                    &character_catalog,
                )
                .next_tech_open
            {
                start_technique_by_id(&mut action, next, loadout, &character_catalog);
                continue;
            }
            let duration = attack_duration_for_state(&action, loadout, &feel, &character_catalog);
            if action.elapsed >= duration {
                set_action(&mut action, FighterAction::Idle);
            }
            continue;
        }

        if matches!(
            action.action,
            FighterAction::ComboFinisher
                | FighterAction::HeavyAttack
                | FighterAction::HeavyAttack2
                | FighterAction::UltimateStartup
                | FighterAction::UltimateRush
                | FighterAction::GrabStartup
                | FighterAction::Throwing
                | FighterAction::SpecialCast
                | FighterAction::ItemPickup
                | FighterAction::ItemSwing
                | FighterAction::ItemThrow
                | FighterAction::ItemDrop
                | FighterAction::DashAttack
                | FighterAction::JumpAttack
                | FighterAction::JumpHeavyAttack
                | FighterAction::LandingRecovery
                | FighterAction::GuardCounter
                | FighterAction::GuardStep
                | FighterAction::QuickStand
                | FighterAction::RecoveryRoll
        ) {
            action.elapsed += dt;
            tick_pig_heavy_charge(&mut action, &input, dt);
            if action.technique_id == Some(TechniqueId::PigHeavy)
                && !action.charge_release_requested
            {
                action.elapsed = action.elapsed.min(pig_heavy_attack_hold_secs());
            }
            refresh_technique_runtime(&mut action, loadout, &feel, &character_catalog);
            if action.action == FighterAction::DashAttack
                && input.light
                && action.branch_window_open
            {
                if let Some(technique) = technique_slot_for_loadout(
                    CharacterMoveSlot::DashLight,
                    loadout,
                    &character_catalog,
                ) {
                    set_technique_action(&mut action, technique);
                }
                continue;
            }
            update_bee_air_dash_facing(&mut motor, &action, input.movement);
            if try_start_bee_air_dash_x_shot(
                &mut motor,
                &mut action,
                &input,
                input.movement,
                loadout,
                &character_catalog,
            ) {
                continue;
            }
            if try_start_chick_air_attack_cancel(
                &mut motor,
                &mut action,
                &input,
                loadout,
                &character_catalog,
            ) {
                continue;
            }
            queue_chained_followup(
                &mut action,
                &input,
                loadout,
                motor.grounded,
                dt,
                &feel,
                &character_catalog,
            );
            if let Some(next) = action.queued_technique
                && technique_runtime_for_action_state_with_feel(
                    &action,
                    loadout,
                    &feel,
                    &character_catalog,
                )
                .next_tech_open
            {
                start_technique_by_id(&mut action, next, loadout, &character_catalog);
                continue;
            }
            let duration = attack_duration_for_state(&action, loadout, &feel, &character_catalog);
            if action.elapsed >= duration {
                if !motor.grounded && should_return_to_jumping_on_air_attack_completion(&action) {
                    set_action(&mut action, FighterAction::Jumping);
                    continue;
                } else if action.action == FighterAction::JumpAttack && !motor.grounded {
                    continue;
                } else {
                    if matches!(
                        action.action,
                        FighterAction::GuardStep
                            | FighterAction::QuickStand
                            | FighterAction::RecoveryRoll
                    ) {
                        stats.invulnerability = 0.0;
                    }
                    if should_return_to_dashing_on_dash_completion(&action) {
                        set_action(&mut action, FighterAction::Dashing);
                    } else {
                        set_action(&mut action, FighterAction::Idle);
                    }
                }
            }
            continue;
        }

        if matches!(action.action, FighterAction::Dashing) {
            action.elapsed += dt;
            refresh_technique_runtime(&mut action, loadout, &feel, &character_catalog);
            if pig_dash_heavy_charge_active(&input, &action, loadout) {
                if input.heavy_held {
                    action.charge_elapsed =
                        (action.charge_elapsed + dt).min(pig_heavy_full_charge_secs());
                }
                if input.heavy_released || !input.heavy_held {
                    start_pig_dash_heavy_release(&mut action, loadout, &character_catalog);
                    continue;
                }
                let dash_dir = movement_input_direction(input.movement)
                    .unwrap_or_else(|| motor.facing.normalize_or_zero());
                let dash_dir = apply_dash_hold_motion_direction(
                    &mut motor,
                    dash_dir,
                    tuning.dash_impulse * body.dash_impulse,
                    dt,
                );
                if dash_trail_tick(&mut motor, dt) {
                    spawn_dash_trail(
                        &mut commands,
                        &effect_assets,
                        transform.translation,
                        dash_dir,
                    );
                }
                continue;
            }
            if try_start_penguin_dash_ultimate(
                &mut motor,
                &mut stats,
                &mut action,
                &input,
                loadout,
                &character_catalog,
            ) {
                continue;
            }
            if start_dash_finisher_from_dash(
                &mut motor,
                &mut action,
                &input,
                loadout,
                &character_catalog,
            ) {
                continue;
            }
            if input.jump && can_start_ground_jump(&motor) {
                let queued_attack = jump_attack_button(&input);
                start_dash_jump_with_scale(
                    &mut motor,
                    &mut action,
                    body.jump_impulse,
                    body.dash_impulse,
                );
                if let Some(button) = queued_attack {
                    queue_air_attack(&mut motor, button);
                }
                continue;
            }
            if let Some(dash_dir) = apply_dash_hold_motion(
                &mut motor,
                input.movement,
                tuning.dash_impulse * body.dash_impulse,
                dt,
            ) {
                if dash_trail_tick(&mut motor, dt) {
                    spawn_dash_trail(
                        &mut commands,
                        &effect_assets,
                        transform.translation,
                        dash_dir,
                    );
                }
                continue;
            }
            if dash_should_stop(action.elapsed, input.movement) {
                start_dash_slide_with_scale(&mut motor, body.dash_slide);
                set_action(&mut action, FighterAction::Idle);
            }
            continue;
        }

        if input.guard && input.dash && motor.grounded && stats.stamina >= GUARD_STEP_STAMINA_COST {
            stats.stamina -= GUARD_STEP_STAMINA_COST;
            let step_dir = defensive_step_direction(input.movement, motor.facing);
            let mechanics = style_mechanics(style.kind);
            motor.velocity.x += step_dir.x * GUARD_STEP_IMPULSE;
            motor.velocity.z += step_dir.z * GUARD_STEP_IMPULSE;
            stats.invulnerability = stats
                .invulnerability
                .max(GUARD_STEP_INVULNERABLE * mechanics.guard_step_invulnerability);
            spawn_dash_trail(
                &mut commands,
                &effect_assets,
                transform.translation,
                step_dir,
            );
            set_action(&mut action, FighterAction::GuardStep);
            continue;
        }

        update_facing_from_movement(&mut motor, &input);

        if !motor.grounded {
            if input.jump && can_start_ground_jump(&motor) {
                let queued_attack = jump_attack_button(&input);
                start_jump_with_air_attack_queue(
                    &mut motor,
                    &mut action,
                    body.jump_impulse,
                    queued_attack,
                );
                continue;
            }
            if try_start_bee_air_dash_x_shot(
                &mut motor,
                &mut action,
                &input,
                input.movement,
                loadout,
                &character_catalog,
            ) {
                continue;
            }
            if queued_air_attack_ready(&motor) {
                if let Some(button) = motor.queued_air_attack {
                    start_air_attack_by_button(
                        &mut motor,
                        &mut action,
                        button,
                        loadout,
                        &character_catalog,
                    );
                }
            } else if motor.queued_air_attack.is_some() {
                set_action(&mut action, FighterAction::Jumping);
            } else if let Some(button) = jump_attack_button(&input)
                && !motor.air_attack_used
            {
                if motor.jump_takeoff_timer > JUMP_ATTACK_QUEUE_TAKEOFF_REMAINING {
                    queue_air_attack(&mut motor, button);
                    set_action(&mut action, FighterAction::Jumping);
                } else {
                    start_air_attack_by_button(
                        &mut motor,
                        &mut action,
                        button,
                        loadout,
                        &character_catalog,
                    );
                }
            } else {
                set_action(&mut action, FighterAction::Jumping);
            }
            continue;
        }

        if input.jump && can_start_ground_jump(&motor) {
            let queued_attack = jump_attack_button(&input);
            start_jump_with_air_attack_queue(
                &mut motor,
                &mut action,
                body.jump_impulse,
                queued_attack,
            );
            continue;
        }

        if try_start_ultimate_from_input(
            &mut motor,
            &mut stats,
            &mut action,
            &input,
            loadout,
            &character_catalog,
        ) {
            continue;
        }

        if let Some(button) = pressed_raw_technique_button(&input)
            && let Some(technique) = raw_technique_for_loadout_in_catalog(
                button,
                motor.grounded,
                loadout,
                &character_catalog,
            )
        {
            if technique.action == FighterAction::UltimateStartup {
                if !can_start_ultimate(&motor, &stats) {
                    continue;
                }
                start_ultimate(&mut motor, &mut stats, &mut action, technique);
                continue;
            }
            if !raw_technique_special_requirement_met(
                entity,
                technique,
                active_chick_skills
                    .iter()
                    .map(|skill| (skill.owner, skill.kind)),
            ) {
                continue;
            }
            if !try_start_raw_technique(&mut stats, &mut action, technique) {
                continue;
            }
            if technique.id == TechniqueId::CatHeavy
                && let Some(armor) = loadout_heavy_armor(loadout)
            {
                stats.invulnerability = stats.invulnerability.max(armor.invulnerability);
            }
            continue;
        }

        if input.dash {
            let dash_dir = if input.movement.length_squared() > 0.01 {
                Vec3::new(input.movement.x, 0.0, input.movement.y).normalize_or_zero()
            } else {
                motor.facing.normalize_or_zero()
            };
            motor.facing = dash_dir;
            motor.velocity.x += dash_dir.x * DASH_IMPULSE * tuning.dash_impulse * body.dash_impulse;
            motor.velocity.z += dash_dir.z * DASH_IMPULSE * tuning.dash_impulse * body.dash_impulse;
            spawn_dash_trail(
                &mut commands,
                &effect_assets,
                transform.translation,
                dash_dir,
            );
            motor.dash_trail_timer = DASH_TRAIL_REPEAT;
            motor.dash_slide_timer = 0.0;
            motor.dash_jump_carry_timer = 0.0;
            motor.dash_jump_carry_speed_limit = 0.0;
            set_action(&mut action, FighterAction::Dashing);
            continue;
        }

        if input.jump && can_start_ground_jump(&motor) {
            start_jump_with_scale(&mut motor, &mut action, body.jump_impulse);
            continue;
        }

        if can_start_guard(&motor, guard_pressed) {
            start_guard(&mut motor, &mut action);
            continue;
        } else if !motor.grounded {
            set_action(&mut action, FighterAction::Jumping);
        } else if input.movement.length_squared() > 0.01 {
            set_action(&mut action, FighterAction::Moving);
        } else if !matches!(action.action, FighterAction::Dashing) {
            set_action(&mut action, FighterAction::Idle);
        }
    }
}

#[derive(Clone, Copy)]
enum ThrowStrength {
    Quick,
    Standard,
    Heavy,
}

#[derive(Clone, Copy)]
struct GrabSnapshot {
    entity: Entity,
    fighter_id: usize,
    action: FighterAction,
    elapsed: f32,
    holding: Option<Entity>,
    held_by: Option<Entity>,
    position: Vec3,
    facing: Vec3,
    input_movement: Vec2,
    input_light: bool,
    input_heavy: bool,
    input_guard: bool,
    throw_knockback: f32,
}

#[derive(Clone, Copy)]
enum GrabResolution {
    Throw {
        holder: Entity,
        victim: Entity,
        owner_id: usize,
        direction: Vec3,
        strength: ThrowStrength,
        braced: bool,
        edge_scale: f32,
        style_scale: f32,
    },
    Release {
        holder: Entity,
        victim: Entity,
    },
}

#[derive(Clone, Copy)]
struct UltimateLockSnapshot {
    entity: Entity,
    action: FighterAction,
    technique_id: Option<TechniqueId>,
    elapsed: f32,
    target: Option<Entity>,
    owner: Option<Entity>,
    position: Vec3,
    facing: Vec3,
}

fn ultimate_lock_release_after(technique_id: Option<TechniqueId>) -> f32 {
    if technique_id == Some(TechniqueId::PigUltimateRush) {
        1.36
    } else {
        ULTIMATE_LOCK_RELEASE_AFTER
    }
}

pub fn update_grab_holds(
    time: Res<Time>,
    mut commands: Commands,
    effect_assets: Res<EffectAssets>,
    state: Res<MatchState>,
    feel: Res<CombatFeelTuning>,
    mut camera_effects: ResMut<HitEffects>,
    mut haptics: ResMut<CombatHapticQueue>,
    mut hitstop: ResMut<Hitstop>,
    mut telemetry: ResMut<MatchTelemetry>,
    mut fighters: Query<(
        Entity,
        &Fighter,
        &FighterInput,
        &mut FighterStats,
        &mut FighterMotor,
        &mut FighterActionState,
        &mut FighterGrabState,
        &FighterStyle,
        &FighterEquipment,
        &Transform,
    )>,
) {
    if hitstop.active() {
        return;
    }

    let dt = time.delta_secs();
    for (_, _, _, _, _, _, mut grab_state, _, _, _) in &mut fighters {
        grab_state.regrab_lockout = (grab_state.regrab_lockout - dt).max(0.0);
    }

    let snapshots: Vec<GrabSnapshot> = fighters
        .iter()
        .map(
            |(entity, fighter, input, _, motor, action, grab_state, style, _, transform)| {
                GrabSnapshot {
                    entity,
                    fighter_id: fighter.id,
                    action: action.action,
                    elapsed: action.elapsed,
                    holding: grab_state.holding,
                    held_by: grab_state.held_by,
                    position: transform.translation,
                    facing: motor.facing,
                    input_movement: input.movement,
                    input_light: input.light,
                    input_heavy: input.heavy,
                    input_guard: input.guard,
                    throw_knockback: style_tuning(style.kind).throw_knockback,
                }
            },
        )
        .collect();

    let mut resolutions = Vec::new();
    for holder in snapshots
        .iter()
        .filter(|snapshot| snapshot.action == FighterAction::GrabHold)
    {
        let Some(victim_entity) = holder.holding else {
            continue;
        };
        let Some(victim) = snapshots
            .iter()
            .find(|snapshot| snapshot.entity == victim_entity)
        else {
            resolutions.push(GrabResolution::Release {
                holder: holder.entity,
                victim: victim_entity,
            });
            continue;
        };

        if victim.action != FighterAction::Grabbed || victim.held_by != Some(holder.entity) {
            resolutions.push(GrabResolution::Release {
                holder: holder.entity,
                victim: victim.entity,
            });
            continue;
        }

        if victim_can_escape_grab(victim, holder.position) && holder.elapsed >= GRAB_ESCAPE_AFTER {
            resolutions.push(GrabResolution::Release {
                holder: holder.entity,
                victim: victim.entity,
            });
            continue;
        }

        let strength = if holder.input_light {
            Some(ThrowStrength::Quick)
        } else if holder.input_heavy {
            Some(ThrowStrength::Heavy)
        } else if holder.elapsed >= GRAB_HOLD_MAX {
            Some(ThrowStrength::Standard)
        } else {
            None
        };

        if let Some(strength) = strength {
            let direction = aimed_throw_direction(holder.input_movement, holder.facing);
            resolutions.push(GrabResolution::Throw {
                holder: holder.entity,
                victim: victim.entity,
                owner_id: holder.fighter_id,
                direction,
                strength,
                braced: victim.input_guard,
                edge_scale: throw_edge_scale(victim.position, direction),
                style_scale: holder.throw_knockback,
            });
        }
    }

    for victim in snapshots
        .iter()
        .filter(|snapshot| snapshot.action == FighterAction::Grabbed)
    {
        let Some(holder_entity) = victim.held_by else {
            continue;
        };
        let holder_is_active = snapshots.iter().any(|snapshot| {
            snapshot.entity == holder_entity
                && snapshot.action == FighterAction::GrabHold
                && snapshot.holding == Some(victim.entity)
        });
        if !holder_is_active {
            resolutions.push(GrabResolution::Release {
                holder: holder_entity,
                victim: victim.entity,
            });
        }
    }

    for resolution in resolutions {
        for (
            entity,
            fighter,
            _,
            mut stats,
            mut motor,
            mut action,
            mut grab_state,
            style,
            equipment,
            transform,
        ) in &mut fighters
        {
            match resolution {
                GrabResolution::Release { holder, victim } => {
                    if entity == holder {
                        grab_state.holding = None;
                        set_action(&mut action, FighterAction::Idle);
                    } else if entity == victim {
                        grab_state.held_by = None;
                        grab_state.regrab_lockout = GRAB_REGRAB_LOCKOUT * 0.5;
                        set_action(&mut action, FighterAction::Idle);
                    }
                }
                GrabResolution::Throw {
                    holder,
                    victim,
                    owner_id,
                    direction,
                    strength,
                    braced,
                    edge_scale,
                    style_scale,
                } => {
                    if entity == holder {
                        grab_state.holding = None;
                        set_action(&mut action, FighterAction::Throwing);
                    } else if entity == victim {
                        grab_state.held_by = None;
                        grab_state.regrab_lockout = GRAB_REGRAB_LOCKOUT;
                        let profile = throw_impact_profile(
                            owner_id,
                            strength,
                            braced,
                            edge_scale,
                            style_scale,
                        )
                        .with_hit_effects_enabled(feel.hit_effects_enabled());
                        apply_impact(
                            &mut commands,
                            &effect_assets,
                            &mut camera_effects,
                            &mut haptics,
                            &mut hitstop,
                            &state,
                            fighter.id,
                            &mut stats,
                            &mut motor,
                            &mut action,
                            transform,
                            None,
                            transform.translation - direction,
                            profile,
                            DamageDefenderProfile::from_loadout(style, equipment),
                            &mut telemetry,
                        );
                    }
                }
            }
        }
    }
}

pub fn update_ultimate_locks(
    mut fighters: Query<(
        Entity,
        &mut FighterMotor,
        &mut FighterActionState,
        &mut FighterUltimateState,
        &mut Transform,
    )>,
) {
    let snapshots: Vec<UltimateLockSnapshot> = fighters
        .iter()
        .map(
            |(entity, motor, action, ultimate_state, transform)| UltimateLockSnapshot {
                entity,
                action: action.action,
                technique_id: action.technique_id,
                elapsed: action.elapsed,
                target: ultimate_state.target,
                owner: ultimate_state.owner,
                position: transform.translation,
                facing: motor.facing,
            },
        )
        .collect();

    let mut lock_pairs = Vec::new();
    let mut stale_locks = Vec::new();
    for attacker in snapshots
        .iter()
        .filter(|snapshot| snapshot.action == FighterAction::UltimateRush)
    {
        let Some(victim_entity) = attacker.target else {
            continue;
        };
        let Some(victim) = snapshots
            .iter()
            .find(|snapshot| snapshot.entity == victim_entity)
        else {
            stale_locks.push(attacker.entity);
            continue;
        };
        if victim.owner != Some(attacker.entity)
            || victim.action != FighterAction::UltimateVictim
            || attacker.elapsed >= ultimate_lock_release_after(attacker.technique_id)
        {
            stale_locks.push(attacker.entity);
            stale_locks.push(victim.entity);
            continue;
        }
        lock_pairs.push((
            attacker.entity,
            victim.entity,
            attacker.position,
            attacker.facing,
        ));
    }
    for victim in snapshots
        .iter()
        .filter(|snapshot| snapshot.action == FighterAction::UltimateVictim)
    {
        let Some(owner) = victim.owner else {
            stale_locks.push(victim.entity);
            continue;
        };
        let owner_is_active = snapshots.iter().any(|snapshot| {
            snapshot.entity == owner
                && snapshot.action == FighterAction::UltimateRush
                && snapshot.target == Some(victim.entity)
        });
        if !owner_is_active {
            stale_locks.push(victim.entity);
        }
    }

    for (entity, mut motor, mut action, mut ultimate_state, mut transform) in &mut fighters {
        if stale_locks.contains(&entity) {
            ultimate_state.target = None;
            ultimate_state.owner = None;
            if action.action == FighterAction::UltimateVictim {
                set_action(&mut action, FighterAction::Hitstun);
            }
        }

        if let Some((_, _, _, attacker_facing)) = lock_pairs
            .iter()
            .find(|(attacker, _, _, _)| *attacker == entity)
        {
            motor.facing = attacker_facing.normalize_or_zero();
            motor.velocity.x *= 0.18;
            motor.velocity.z *= 0.18;
        } else if let Some((_, _, attacker_position, attacker_facing)) = lock_pairs
            .iter()
            .find(|(_, victim, _, _)| *victim == entity)
        {
            let facing = attacker_facing.normalize_or_zero();
            transform.translation = *attacker_position + facing * ULTIMATE_LOCK_DISTANCE;
            motor.facing = -facing;
            motor.velocity = Vec3::ZERO;
        }
    }
}

fn aimed_throw_direction(input_movement: Vec2, facing: Vec3) -> Vec3 {
    if input_movement.length_squared() > 0.01 {
        Vec3::new(input_movement.x, 0.0, input_movement.y).normalize_or_zero()
    } else {
        facing.normalize_or_zero()
    }
}

fn victim_can_escape_grab(victim: &GrabSnapshot, holder_position: Vec3) -> bool {
    if !victim.input_guard || victim.input_movement.length_squared() <= 0.01 {
        return false;
    }

    let away = Vec2::new(
        victim.position.x - holder_position.x,
        victim.position.z - holder_position.z,
    )
    .normalize_or_zero();
    victim.input_movement.normalize_or_zero().dot(away) > 0.25
}

fn throw_edge_scale(position: Vec3, direction: Vec3) -> f32 {
    let flat_position = Vec2::new(position.x, position.z);
    let flat_direction = Vec2::new(direction.x, direction.z).normalize_or_zero();
    if flat_position.length() >= THROW_EDGE_PRESSURE_START
        && flat_position.normalize_or_zero().dot(flat_direction) > 0.25
    {
        THROW_EDGE_PRESSURE_BONUS
    } else {
        1.0
    }
}

fn throw_impact_profile(
    owner_id: usize,
    strength: ThrowStrength,
    braced: bool,
    edge_scale: f32,
    style_scale: f32,
) -> ImpactProfile {
    let (damage, knockback, vertical_knockback, force_knockdown) = match strength {
        ThrowStrength::Quick => (THROW_QUICK_DAMAGE, THROW_QUICK_KNOCKBACK, 2.4, false),
        ThrowStrength::Standard => (THROW_STANDARD_DAMAGE, THROW_STANDARD_KNOCKBACK, 3.1, true),
        ThrowStrength::Heavy => (THROW_HEAVY_DAMAGE, THROW_HEAVY_KNOCKBACK, 3.8, true),
    };
    let brace_scale = if braced { THROW_BRACE_SCALE } else { 1.0 };

    let mut profile = impact_profile(
        owner_id,
        ImpactSource::GrabThrow,
        damage * brace_scale,
        knockback * brace_scale * edge_scale * style_scale,
        vertical_knockback,
        force_knockdown,
        false,
        0.0,
        if matches!(strength, ThrowStrength::Quick) {
            ImpactFeedbackIntensity::Light
        } else {
            ImpactFeedbackIntensity::Heavy
        },
        if force_knockdown {
            ReactionFamilyId::SlidingKnockdown
        } else {
            ReactionFamilyId::LightAirPop
        },
    );
    profile.damage_profile = DamageProfileId::GrabControl;
    profile.element = DamageElement::Neutral;
    profile
}

fn set_action(action: &mut FighterActionState, next: FighterAction) {
    if action.action != next {
        reset_action_state(action, next);
    }
}

fn set_technique_action(action: &mut FighterActionState, technique: TechniqueDefinition) {
    reset_action_state(action, technique.action);
    action.technique_id = Some(technique.id);
}

fn reset_action_state(action: &mut FighterActionState, next: FighterAction) {
    action.action = next;
    action.elapsed = 0.0;
    action.hitbox_spawned = false;
    action.queued_combo = false;
    action.queued_technique = None;
    action.queued_button = None;
    clear_buffered_button(action);
    action.confirmed_hit = false;
    action.technique_id = None;
    action.cancel_window_open = false;
    action.branch_window_open = false;
    action.timeline_events_fired = 0;
    action.reaction_getup_ms = None;
    action.reaction_recover_ms = None;
    action.clear_reaction_visual();
    action.charge_elapsed = 0.0;
    action.charge_release_requested = false;
}

fn tick_guard_input(motor: &mut FighterMotor, guard_requested: bool, dt: f32) -> bool {
    let pressed = guard_requested && !motor.guard_was_requested;
    motor.guard_was_requested = guard_requested;
    motor.guard_cooldown_timer = (motor.guard_cooldown_timer - dt).max(0.0);
    if pressed {
        motor.guard_start_buffer_timer = GUARD_START_BUFFER_SECONDS;
    } else if guard_requested {
        motor.guard_start_buffer_timer = (motor.guard_start_buffer_timer - dt).max(0.0);
    } else {
        motor.guard_start_buffer_timer = 0.0;
    }
    pressed
}

fn can_start_guard(motor: &FighterMotor, guard_pressed: bool) -> bool {
    (guard_pressed || motor.guard_start_buffer_timer > 0.0)
        && motor.grounded
        && motor.guard_cooldown_timer <= 0.0
        && motor.guard_active_timer <= 0.0
}

fn start_guard(motor: &mut FighterMotor, action: &mut FighterActionState) {
    motor.guard_active_timer = 0.0;
    motor.guard_cooldown_timer = 0.0;
    motor.guard_start_buffer_timer = 0.0;
    motor.velocity.x *= 0.12;
    motor.velocity.z *= 0.12;
    set_action(action, FighterAction::Guarding);
}

fn guard_should_end(motor: &FighterMotor, guard_requested: bool) -> bool {
    !guard_requested || !motor.grounded || motor.guard_active_timer >= GUARD_MAX_DURATION
}

fn finish_guard(motor: &mut FighterMotor, action: &mut FighterActionState) {
    motor.guard_active_timer = 0.0;
    motor.guard_cooldown_timer = GUARD_RESTART_COOLDOWN;
    motor.guard_start_buffer_timer = 0.0;
    set_action(action, FighterAction::Idle);
}

fn tick_guard_counter_window(motor: &mut FighterMotor, dt: f32) {
    if motor.guard_counter_window_timer <= 0.0 {
        motor.guard_counter_source = None;
        motor.guard_counter_buffered = false;
        return;
    }

    motor.guard_counter_window_timer = (motor.guard_counter_window_timer - dt).max(0.0);
    if motor.guard_counter_window_timer <= 0.0 {
        motor.guard_counter_source = None;
        motor.guard_counter_buffered = false;
    }
}

fn guard_counter_action_can_trigger(action: FighterAction) -> bool {
    matches!(
        action,
        FighterAction::Guarding | FighterAction::Idle | FighterAction::Moving
    )
}

fn guard_counter_source_direction(source: Vec3, position: Vec3) -> Option<Vec3> {
    let direction = Vec3::new(source.x - position.x, 0.0, source.z - position.z);
    (direction.length_squared() > 0.01).then(|| direction.normalize_or_zero())
}

fn try_start_guard_counter(
    stats: &mut FighterStats,
    motor: &mut FighterMotor,
    action: &mut FighterActionState,
    input: &FighterInput,
    position: Vec3,
    loadout: LoadoutContext,
    character_catalog: &CharacterMoveCatalog,
    requested: bool,
) -> bool {
    if !requested {
        return false;
    }
    if motor.guard_counter_window_timer <= 0.0
        || !motor.grounded
        || !guard_counter_action_can_trigger(action.action)
        || stats.health <= GUARD_COUNTER_HEALTH_COST
    {
        motor.guard_counter_buffered = false;
        return false;
    }

    let Some(technique) = technique_definition_for_loadout_id_in_catalog(
        TechniqueId::GuardCounter,
        loadout,
        character_catalog,
    ) else {
        motor.guard_counter_buffered = false;
        return false;
    };

    if let Some(direction) = movement_input_direction(input.movement).or_else(|| {
        motor
            .guard_counter_source
            .and_then(|source| guard_counter_source_direction(source, position))
    }) {
        motor.facing = direction;
    }

    stats.health -= GUARD_COUNTER_HEALTH_COST;
    motor.guard_active_timer = 0.0;
    motor.guard_cooldown_timer = GUARD_RESTART_COOLDOWN;
    motor.guard_start_buffer_timer = 0.0;
    motor.velocity *= 0.35;
    motor.clear_guard_counter_window();
    set_technique_action(action, technique);
    true
}

fn can_start_ultimate(motor: &FighterMotor, stats: &FighterStats) -> bool {
    motor.grounded && stats.stamina >= ULTIMATE_STAMINA_COST
}

fn start_ultimate(
    motor: &mut FighterMotor,
    stats: &mut FighterStats,
    action: &mut FighterActionState,
    technique: TechniqueDefinition,
) {
    stats.stamina -= ULTIMATE_STAMINA_COST;
    motor.guard_active_timer = 0.0;
    motor.guard_cooldown_timer = 0.0;
    motor.guard_start_buffer_timer = 0.0;
    motor.velocity.x *= 0.18;
    motor.velocity.z *= 0.18;
    set_technique_action(action, technique);
}

fn try_start_raw_technique(
    stats: &mut FighterStats,
    action: &mut FighterActionState,
    technique: TechniqueDefinition,
) -> bool {
    if technique.action == FighterAction::UltimateStartup {
        return false;
    }

    let stamina_cost = technique.stamina_cost.max(0.0);
    if stamina_cost > f32::EPSILON && stats.stamina < stamina_cost {
        return false;
    }

    if stamina_cost > f32::EPSILON {
        stats.stamina -= stamina_cost;
    }
    set_technique_action(action, technique);
    true
}

fn raw_technique_special_requirement_met<I>(
    owner: Entity,
    technique: TechniqueDefinition,
    active_chick_skills: I,
) -> bool
where
    I: IntoIterator<Item = (Entity, ChickSkillKind)>,
{
    if technique.id != TechniqueId::ChickLight1 {
        return true;
    }

    active_chick_skills
        .into_iter()
        .any(|(skill_owner, kind)| skill_owner == owner && chick_c_can_start_from_skill(kind))
}

fn chick_c_can_start_from_skill(kind: ChickSkillKind) -> bool {
    matches!(
        kind,
        ChickSkillKind::OrbitEgg | ChickSkillKind::OrbitEggLaunch
    )
}

fn chick_light_recall_interrupt_requested<I>(
    owner: Entity,
    action: &FighterActionState,
    input: &FighterInput,
    active_chick_skills: I,
) -> bool
where
    I: IntoIterator<Item = (Entity, ChickSkillKind)>,
{
    action.technique_id == Some(TechniqueId::ChickLight1)
        && input.raw_light_pressed
        && active_chick_skills.into_iter().any(|(skill_owner, kind)| {
            skill_owner == owner && kind == ChickSkillKind::OrbitEggLaunch
        })
}

#[cfg(test)]
mod ultimate_mp_tests {
    use super::*;

    #[test]
    fn ultimate_requires_half_max_mp() {
        let motor = FighterMotor::default();
        let mut stats = FighterStats::default();

        stats.stamina = ULTIMATE_STAMINA_COST - 0.1;
        assert!(!can_start_ultimate(&motor, &stats));

        stats.stamina = ULTIMATE_STAMINA_COST;
        assert!(can_start_ultimate(&motor, &stats));
    }

    #[test]
    fn ultimate_drains_half_max_mp() {
        let mut motor = FighterMotor::default();
        let mut stats = FighterStats::default();
        let mut action = FighterActionState::default();
        let technique = technique_definition_by_id(TechniqueId::CatUltimateStartup).unwrap();

        start_ultimate(&mut motor, &mut stats, &mut action, technique);

        assert_eq!(stats.stamina, MAX_STAMINA - ULTIMATE_STAMINA_COST);
    }

    #[test]
    fn bee_ultimate_drains_half_max_mp() {
        let mut motor = FighterMotor::default();
        let mut stats = FighterStats::default();
        let mut action = FighterActionState::default();
        let technique = technique_definition_by_id(TechniqueId::BeeUltimateStartup).unwrap();

        start_ultimate(&mut motor, &mut stats, &mut action, technique);

        assert_eq!(stats.stamina, MAX_STAMINA * 0.5);
        assert_eq!(stats.stamina, MAX_STAMINA - ULTIMATE_STAMINA_COST);
        assert_eq!(action.technique_id, Some(TechniqueId::BeeUltimateStartup));
    }

    #[test]
    fn roster_ultimate_inputs_spend_half_max_mp() {
        let catalog = CharacterMoveCatalog::default();
        for character in [
            CharacterKind::Cat,
            CharacterKind::Pig,
            CharacterKind::Dog,
            CharacterKind::Fox,
            CharacterKind::Panda,
            CharacterKind::Bee,
            CharacterKind::Penguin,
            CharacterKind::Chick,
        ] {
            let loadout = LoadoutContext::for_character(
                character,
                FighterStyleKind::Anchor,
                EquipmentKind::CounterCell,
            );
            let expected =
                technique_slot_for_loadout(CharacterMoveSlot::UltimateStartup, loadout, &catalog)
                    .unwrap_or_else(|| panic!("{character:?} should have an ultimate startup"));
            let mut motor = FighterMotor {
                grounded: true,
                ..default()
            };
            let mut stats = FighterStats::default();
            let mut action = FighterActionState::default();

            assert_eq!(expected.stamina_cost, ULTIMATE_STAMINA_COST);
            assert!(try_start_ultimate_from_input(
                &mut motor,
                &mut stats,
                &mut action,
                &FighterInput {
                    ultimate: true,
                    ..default()
                },
                loadout,
                &catalog,
            ));

            assert_eq!(stats.stamina, MAX_STAMINA * 0.5, "{character:?}");
            assert_eq!(
                stats.stamina,
                MAX_STAMINA - ULTIMATE_STAMINA_COST,
                "{character:?}"
            );
            assert_eq!(action.technique_id, Some(expected.id), "{character:?}");
        }
    }
}

#[cfg(test)]
mod raw_technique_mp_tests {
    use super::*;

    fn loadout(character: CharacterKind) -> LoadoutContext {
        LoadoutContext::for_character(
            character,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        )
    }

    fn entity(index: u32) -> Entity {
        Entity::from_raw_u32(index).expect("test entity index should be valid")
    }

    #[test]
    fn chick_grounded_x_starts_and_drains_fifteen_percent_mp() {
        let catalog = CharacterMoveCatalog::default();
        let technique = raw_technique_for_loadout_in_catalog(
            TechniqueButton::B,
            true,
            loadout(CharacterKind::Chick),
            &catalog,
        )
        .unwrap();
        let mut stats = FighterStats::default();
        let mut action = FighterActionState::default();

        assert_eq!(technique.id, TechniqueId::ChickHeavy);
        assert_eq!(technique.stamina_cost, CHICK_X_STAMINA_COST);
        assert!(try_start_raw_technique(&mut stats, &mut action, technique));

        assert_eq!(action.technique_id, Some(TechniqueId::ChickHeavy));
        assert_eq!(stats.stamina, MAX_STAMINA - CHICK_X_STAMINA_COST);
    }

    #[test]
    fn chick_grounded_x_does_not_start_below_cost() {
        let catalog = CharacterMoveCatalog::default();
        let technique = raw_technique_for_loadout_in_catalog(
            TechniqueButton::B,
            true,
            loadout(CharacterKind::Chick),
            &catalog,
        )
        .unwrap();
        let mut stats = FighterStats {
            stamina: CHICK_X_STAMINA_COST - 0.1,
            ..default()
        };
        let mut action = FighterActionState::default();

        assert!(!try_start_raw_technique(&mut stats, &mut action, technique));
        assert_eq!(action.technique_id, None);
        assert_eq!(action.action, FighterAction::Idle);
        assert_eq!(stats.stamina, CHICK_X_STAMINA_COST - 0.1);
    }

    #[test]
    fn other_grounded_heavy_attacks_remain_free() {
        let catalog = CharacterMoveCatalog::default();
        let technique = raw_technique_for_loadout_in_catalog(
            TechniqueButton::B,
            true,
            loadout(CharacterKind::Cat),
            &catalog,
        )
        .unwrap();
        let mut stats = FighterStats::default();
        let mut action = FighterActionState::default();

        assert_eq!(technique.id, TechniqueId::CatHeavy);
        assert_eq!(technique.stamina_cost, 0.0);
        assert!(try_start_raw_technique(&mut stats, &mut action, technique));

        assert_eq!(action.technique_id, Some(TechniqueId::CatHeavy));
        assert_eq!(stats.stamina, MAX_STAMINA);
    }

    #[test]
    fn chick_grounded_c_requires_owned_controllable_egg() {
        let catalog = CharacterMoveCatalog::default();
        let technique = raw_technique_for_loadout_in_catalog(
            TechniqueButton::A,
            true,
            loadout(CharacterKind::Chick),
            &catalog,
        )
        .unwrap();
        let owner = entity(1);
        let other_owner = entity(2);

        assert_eq!(technique.id, TechniqueId::ChickLight1);
        assert!(!raw_technique_special_requirement_met(owner, technique, []));
        assert!(!raw_technique_special_requirement_met(
            owner,
            technique,
            [(other_owner, ChickSkillKind::OrbitEgg)]
        ));
        assert!(!raw_technique_special_requirement_met(
            owner,
            technique,
            [(owner, ChickSkillKind::ShellChip)]
        ));
        assert!(raw_technique_special_requirement_met(
            owner,
            technique,
            [(owner, ChickSkillKind::OrbitEgg)]
        ));
        assert!(raw_technique_special_requirement_met(
            owner,
            technique,
            [(owner, ChickSkillKind::OrbitEggLaunch)]
        ));
        assert!(!raw_technique_special_requirement_met(
            owner,
            technique,
            [(owner, ChickSkillKind::OrbitEggReturn)]
        ));
        assert!(!raw_technique_special_requirement_met(
            owner,
            technique,
            [(other_owner, ChickSkillKind::OrbitEggLaunch)]
        ));
    }

    #[test]
    fn other_grounded_light_attacks_do_not_require_orbit_egg() {
        let catalog = CharacterMoveCatalog::default();
        let technique = raw_technique_for_loadout_in_catalog(
            TechniqueButton::A,
            true,
            loadout(CharacterKind::Cat),
            &catalog,
        )
        .unwrap();

        assert_eq!(technique.id, TechniqueId::CatLight1);
        assert!(raw_technique_special_requirement_met(
            entity(1),
            technique,
            []
        ));
    }

    #[test]
    fn chick_c_recall_interrupt_restarts_only_from_owned_launch() {
        let owner = entity(1);
        let other_owner = entity(2);
        let chick_action = FighterActionState {
            action: FighterAction::LightAttack1,
            technique_id: Some(TechniqueId::ChickLight1),
            ..default()
        };
        let input = FighterInput {
            raw_light_pressed: true,
            ..default()
        };

        assert!(chick_light_recall_interrupt_requested(
            owner,
            &chick_action,
            &input,
            [(owner, ChickSkillKind::OrbitEggLaunch)]
        ));
        assert!(!chick_light_recall_interrupt_requested(
            owner,
            &chick_action,
            &input,
            []
        ));
        assert!(!chick_light_recall_interrupt_requested(
            owner,
            &chick_action,
            &input,
            [(other_owner, ChickSkillKind::OrbitEggLaunch)]
        ));
        assert!(!chick_light_recall_interrupt_requested(
            owner,
            &chick_action,
            &input,
            [(owner, ChickSkillKind::OrbitEgg)]
        ));
        assert!(!chick_light_recall_interrupt_requested(
            owner,
            &chick_action,
            &input,
            [(owner, ChickSkillKind::OrbitEggReturn)]
        ));
        assert!(!chick_light_recall_interrupt_requested(
            owner,
            &FighterActionState {
                action: FighterAction::LightAttack1,
                technique_id: Some(TechniqueId::CatLight1),
                ..default()
            },
            &input,
            [(owner, ChickSkillKind::OrbitEggLaunch)]
        ));
        assert!(!chick_light_recall_interrupt_requested(
            owner,
            &chick_action,
            &FighterInput::default(),
            [(owner, ChickSkillKind::OrbitEggLaunch)]
        ));
    }
}

fn start_technique_by_id(
    action: &mut FighterActionState,
    id: TechniqueId,
    loadout: LoadoutContext,
    character_catalog: &CharacterMoveCatalog,
) {
    if let Some(technique) =
        technique_definition_for_loadout_id_in_catalog(id, loadout, character_catalog)
    {
        set_technique_action(action, technique);
    }
}

#[cfg(test)]
fn attack_duration(
    action: FighterAction,
    loadout: LoadoutContext,
    feel: &CombatFeelTuning,
    character_catalog: &CharacterMoveCatalog,
) -> f32 {
    if matches!(action, FighterAction::GrabHold | FighterAction::Grabbed) {
        return GRAB_HOLD_MAX;
    }
    technique_definition_for_loadout_with_feel(action, loadout, feel, character_catalog)
        .map_or(0.0, |technique| technique.duration())
}

fn attack_duration_for_state(
    action: &FighterActionState,
    loadout: LoadoutContext,
    feel: &CombatFeelTuning,
    character_catalog: &CharacterMoveCatalog,
) -> f32 {
    if action.technique_id == Some(TechniqueId::PigHeavy) && !action.charge_release_requested {
        return f32::INFINITY;
    }
    let mut duration =
        technique_definition_for_action_state_with_feel(action, loadout, feel, character_catalog)
            .map_or(0.0, |technique| technique.duration());
    let whiff_scale = loadout_heavy_whiff_recovery_scale(loadout);
    if matches!(
        action.action,
        FighterAction::HeavyAttack | FighterAction::HeavyAttack2
    ) && !action.confirmed_hit
        && whiff_scale > 1.0
    {
        duration *= whiff_scale;
    }
    duration
}

fn should_return_to_jumping_on_air_attack_completion(action: &FighterActionState) -> bool {
    match action.action {
        FighterAction::JumpHeavyAttack => true,
        FighterAction::JumpAttack => matches!(
            action.technique_id,
            Some(
                TechniqueId::BeeJumpAttack
                    | TechniqueId::PenguinJumpAttack
                    | TechniqueId::ChickJumpAttack
            )
        ),
        _ => false,
    }
}

fn refresh_technique_runtime(
    action: &mut FighterActionState,
    loadout: LoadoutContext,
    feel: &CombatFeelTuning,
    character_catalog: &CharacterMoveCatalog,
) {
    let runtime =
        technique_runtime_for_action_state_with_feel(action, loadout, feel, character_catalog);
    action.technique_id = runtime.id;
    action.cancel_window_open = runtime.cancel_open;
    action.branch_window_open = runtime.branch_open;
}

#[cfg(test)]
fn technique_definition_for_loadout_with_feel(
    action: FighterAction,
    loadout: LoadoutContext,
    feel: &CombatFeelTuning,
    character_catalog: &CharacterMoveCatalog,
) -> Option<TechniqueDefinition> {
    crate::techniques::technique_definition_for_loadout_in_catalog(
        action,
        loadout,
        character_catalog,
    )
    .map(|technique| feel.apply_technique(technique))
}

fn technique_definition_for_action_state_with_feel(
    action: &FighterActionState,
    loadout: LoadoutContext,
    feel: &CombatFeelTuning,
    character_catalog: &CharacterMoveCatalog,
) -> Option<TechniqueDefinition> {
    let definition = active_technique_definition_in_catalog(
        action.action,
        action.technique_id,
        loadout,
        character_catalog,
    )?;
    Some(feel.apply_technique(definition))
}

fn technique_runtime_for_action_state_with_feel(
    action: &FighterActionState,
    loadout: LoadoutContext,
    feel: &CombatFeelTuning,
    character_catalog: &CharacterMoveCatalog,
) -> TechniqueRuntime {
    let Some(definition) =
        technique_definition_for_action_state_with_feel(action, loadout, feel, character_catalog)
    else {
        return TechniqueRuntime {
            id: None,
            cancel_open: false,
            branch_open: false,
            next_tech_open: false,
            recovered: false,
        };
    };

    TechniqueRuntime {
        id: Some(definition.id),
        cancel_open: definition.cancel_open(action.elapsed),
        branch_open: definition.branch_open(action.elapsed),
        next_tech_open: definition.script.next_tech_open(action.elapsed),
        recovered: definition.script.recovered(action.elapsed),
    }
}

fn defensive_step_direction(input_movement: Vec2, facing: Vec3) -> Vec3 {
    let facing = facing.normalize_or_zero();
    if input_movement.length_squared() <= 0.01 {
        return -facing;
    }

    let requested = Vec3::new(input_movement.x, 0.0, input_movement.y).normalize_or_zero();
    if requested.dot(facing) <= 0.1 {
        requested
    } else {
        let right = Vec3::new(facing.z, 0.0, -facing.x).normalize_or_zero();
        if requested.dot(right) >= 0.0 {
            right
        } else {
            -right
        }
    }
}

fn recovery_roll_direction(input_movement: Vec2, facing: Vec3) -> Vec3 {
    if input_movement.length_squared() > 0.01 {
        Vec3::new(input_movement.x, 0.0, input_movement.y).normalize_or_zero()
    } else {
        -facing.normalize_or_zero()
    }
}

fn can_start_ground_jump(motor: &FighterMotor) -> bool {
    motor.grounded || motor.ledge_grace_timer > 0.0
}

#[allow(dead_code)]
fn start_jump(motor: &mut FighterMotor, action: &mut FighterActionState) {
    start_jump_with_scale(motor, action, 1.0);
}

fn start_jump_with_air_attack_queue(
    motor: &mut FighterMotor,
    action: &mut FighterActionState,
    jump_scale: f32,
    button: Option<TechniqueButton>,
) {
    start_jump_with_scale(motor, action, jump_scale);
    if let Some(button) = button {
        queue_air_attack(motor, button);
    }
}

fn start_jump_with_scale(
    motor: &mut FighterMotor,
    action: &mut FighterActionState,
    jump_scale: f32,
) {
    motor.velocity.y = JUMP_SPEED * jump_scale;
    motor.grounded = false;
    motor.ledge_grace_timer = 0.0;
    motor.jump_takeoff_timer = 0.09;
    motor.dash_slide_timer = 0.0;
    motor.dash_jump_carry_timer = 0.0;
    motor.dash_jump_carry_speed_limit = 0.0;
    motor.air_attack_used = false;
    clear_queued_air_attack(motor);
    motor.jump_attack_landing_recovery = false;
    clear_bee_air_dash_state(motor);
    set_action(action, FighterAction::Jumping);
}

#[allow(dead_code)]
fn start_dash_jump(motor: &mut FighterMotor, action: &mut FighterActionState) {
    start_dash_jump_with_scale(motor, action, 1.0, 1.0);
}

fn start_dash_jump_with_scale(
    motor: &mut FighterMotor,
    action: &mut FighterActionState,
    jump_scale: f32,
    dash_carry_scale: f32,
) {
    let facing = motor.facing.normalize_or_zero();
    let forward = Vec2::new(facing.x, facing.z).normalize_or_zero();
    let current = planar_velocity(motor);
    let min_carry = DASH_JUMP_MIN_FORWARD_SPEED * dash_carry_scale;
    let max_carry = DASH_JUMP_MAX_FORWARD_SPEED * dash_carry_scale;
    let forward_speed = current.dot(forward).max(min_carry);
    let carry_speed = forward_speed.min(max_carry);

    start_jump_with_scale(motor, action, jump_scale);
    motor.dash_jump_carry_timer = DASH_JUMP_CARRY_DURATION;
    motor.dash_jump_carry_speed_limit = max_carry;
    set_planar_velocity(motor, forward * carry_speed);
}

fn start_air_attack_by_button(
    motor: &mut FighterMotor,
    action: &mut FighterActionState,
    button: TechniqueButton,
    loadout: LoadoutContext,
    character_catalog: &CharacterMoveCatalog,
) {
    clear_queued_air_attack(motor);
    match button {
        TechniqueButton::B => start_jump_heavy_attack(motor, action, loadout, character_catalog),
        TechniqueButton::A => start_jump_attack(motor, action, loadout, character_catalog),
        _ => {}
    }
}

fn start_jump_attack(
    motor: &mut FighterMotor,
    action: &mut FighterActionState,
    loadout: LoadoutContext,
    character_catalog: &CharacterMoveCatalog,
) {
    let Some(technique) =
        technique_slot_for_loadout(CharacterMoveSlot::JumpLight, loadout, character_catalog)
    else {
        return;
    };
    motor.grounded = false;
    motor.ledge_grace_timer = 0.0;
    let facing = motor.facing.normalize_or_zero();
    if technique.id == TechniqueId::BeeJumpAttack {
        motor.velocity.x = facing.x * BEE_JUMP_ATTACK_FORWARD_SPEED;
        motor.velocity.z = facing.z * BEE_JUMP_ATTACK_FORWARD_SPEED;
        motor.velocity.y = BEE_JUMP_ATTACK_UP_SPEED;
        motor.jump_attack_landing_recovery = false;
        motor.bee_air_dash_motion_active = true;
        motor.bee_air_dash_shot_available = true;
    } else if technique.id == TechniqueId::PenguinJumpAttack {
        motor.velocity.y = motor.velocity.y.max(PENGUIN_JUMP_SNOWFLAKE_MIN_FALL_SPEED);
        motor.jump_attack_landing_recovery = false;
        clear_bee_air_dash_state(motor);
    } else if technique.id == TechniqueId::ChickJumpAttack {
        motor.velocity.x = facing.x * CHICK_JUMP_C_FORWARD_SPEED;
        motor.velocity.z = facing.z * CHICK_JUMP_C_FORWARD_SPEED;
        motor.velocity.y = motor.velocity.y.max(CHICK_JUMP_C_MIN_UP_SPEED);
        motor.jump_attack_landing_recovery = false;
        clear_bee_air_dash_state(motor);
    } else {
        motor.velocity.x = facing.x * JUMP_ATTACK_DIVE_FORWARD_SPEED;
        motor.velocity.z = facing.z * JUMP_ATTACK_DIVE_FORWARD_SPEED;
        motor.velocity.y = motor.velocity.y.min(-JUMP_ATTACK_DIVE_DOWN_SPEED);
        motor.jump_attack_landing_recovery = true;
        clear_bee_air_dash_state(motor);
    }
    motor.dash_jump_carry_timer = 0.0;
    motor.dash_jump_carry_speed_limit = 0.0;
    clear_queued_air_attack(motor);
    motor.air_attack_used = technique.id != TechniqueId::PenguinJumpAttack;
    set_technique_action(action, technique);
}

fn start_jump_heavy_attack(
    motor: &mut FighterMotor,
    action: &mut FighterActionState,
    loadout: LoadoutContext,
    character_catalog: &CharacterMoveCatalog,
) {
    let Some(technique) =
        technique_slot_for_loadout(CharacterMoveSlot::JumpHeavy, loadout, character_catalog)
    else {
        return;
    };
    motor.grounded = false;
    motor.ledge_grace_timer = 0.0;
    if technique.id == TechniqueId::PigJumpHeavy {
        let facing = motor.facing.normalize_or_zero();
        motor.velocity.x = facing.x * JUMP_ATTACK_DIVE_FORWARD_SPEED * 0.22;
        motor.velocity.z = facing.z * JUMP_ATTACK_DIVE_FORWARD_SPEED * 0.22;
        motor.velocity.y = motor.velocity.y.min(-JUMP_ATTACK_DIVE_DOWN_SPEED * 1.08);
        motor.jump_attack_landing_recovery = true;
    } else if technique.id == TechniqueId::PenguinJumpHeavy {
        motor.velocity.y = motor.velocity.y.max(PENGUIN_JUMP_SNOWFLAKE_MIN_FALL_SPEED);
        motor.jump_attack_landing_recovery = false;
    } else if technique.id == TechniqueId::ChickJumpHeavy {
        let facing = motor.facing.normalize_or_zero();
        motor.velocity.x = facing.x * CHICK_FRESH_EGG_RIDE_FORWARD_SPEED;
        motor.velocity.z = facing.z * CHICK_FRESH_EGG_RIDE_FORWARD_SPEED;
        motor.velocity.y = CHICK_FRESH_EGG_RIDE_LIFT_SPEED;
        motor.jump_attack_landing_recovery = false;
    } else {
        motor.velocity.x *= JUMP_HEAVY_AIR_STALL_PLANAR_SCALE;
        motor.velocity.z *= JUMP_HEAVY_AIR_STALL_PLANAR_SCALE;
        motor.velocity.y = motor.velocity.y.clamp(
            JUMP_HEAVY_AIR_STALL_DOWN_SPEED,
            JUMP_HEAVY_AIR_STALL_UP_SPEED,
        );
        motor.jump_attack_landing_recovery = false;
    }
    motor.dash_jump_carry_timer = 0.0;
    motor.dash_jump_carry_speed_limit = 0.0;
    clear_queued_air_attack(motor);
    motor.air_attack_used = true;
    clear_bee_air_dash_state(motor);
    set_technique_action(action, technique);
}

fn try_start_chick_air_attack_cancel(
    motor: &mut FighterMotor,
    action: &mut FighterActionState,
    input: &FighterInput,
    loadout: LoadoutContext,
    character_catalog: &CharacterMoveCatalog,
) -> bool {
    if motor.grounded {
        return false;
    }

    match (action.action, action.technique_id) {
        (FighterAction::JumpAttack, Some(TechniqueId::ChickJumpAttack))
            if input.raw_heavy_pressed =>
        {
            start_jump_heavy_attack(motor, action, loadout, character_catalog);
            true
        }
        (FighterAction::JumpHeavyAttack, Some(TechniqueId::ChickJumpHeavy))
            if input.raw_light_pressed =>
        {
            start_jump_attack(motor, action, loadout, character_catalog);
            true
        }
        _ => false,
    }
}

fn try_start_bee_air_dash_x_shot(
    motor: &mut FighterMotor,
    action: &mut FighterActionState,
    input: &FighterInput,
    input_movement: Vec2,
    loadout: LoadoutContext,
    character_catalog: &CharacterMoveCatalog,
) -> bool {
    if motor.grounded || !motor.bee_air_dash_shot_available || !jump_heavy_pressed(input) {
        return false;
    }
    let Some(technique) =
        technique_slot_for_loadout(CharacterMoveSlot::JumpHeavy, loadout, character_catalog)
    else {
        return false;
    };
    if technique.id != TechniqueId::BeeJumpHeavy {
        return false;
    }

    if let Some(direction) = movement_input_direction(input_movement) {
        motor.facing = direction;
    }
    motor.grounded = false;
    motor.ledge_grace_timer = 0.0;
    motor.jump_attack_landing_recovery = false;
    motor.dash_jump_carry_timer = 0.0;
    motor.dash_jump_carry_speed_limit = 0.0;
    clear_queued_air_attack(motor);
    motor.air_attack_used = true;
    motor.bee_air_dash_motion_active = true;
    motor.bee_air_dash_shot_available = false;
    set_technique_action(action, technique);
    true
}

fn update_bee_air_dash_facing(
    motor: &mut FighterMotor,
    action: &FighterActionState,
    input_movement: Vec2,
) {
    if action.technique_id != Some(TechniqueId::BeeJumpAttack) {
        return;
    }
    if let Some(direction) = movement_input_direction(input_movement) {
        motor.facing = direction;
    }
}

fn should_return_to_dashing_on_dash_completion(action: &FighterActionState) -> bool {
    action.action == FighterAction::DashAttack
        && matches!(
            action.technique_id,
            Some(TechniqueId::PenguinDashAttack | TechniqueId::PenguinDashHeavy)
        )
}

#[derive(Clone, Copy)]
struct BodyMotionProfile {
    input_scale: f32,
    max_speed_bonus: f32,
    ground_friction: f32,
    air_friction: f32,
    stop_friction: f32,
    turn_brake: f32,
    landing_input_scale: f32,
    landing_brake: f32,
    gravity_scale: f32,
    fall_gravity_scale: f32,
    takeoff_gravity_scale: f32,
    terminal_fall_speed: f32,
    stop_snap_speed: f32,
}

fn body_motion_profile(action: FighterAction) -> BodyMotionProfile {
    let mut profile = BodyMotionProfile {
        input_scale: 1.0,
        max_speed_bonus: 0.0,
        ground_friction: GROUND_FRICTION,
        air_friction: AIR_FRICTION,
        stop_friction: 16.0,
        turn_brake: 4.0,
        landing_input_scale: 0.18,
        landing_brake: 12.0,
        gravity_scale: 1.0,
        fall_gravity_scale: 1.18,
        takeoff_gravity_scale: 0.55,
        terminal_fall_speed: 17.0,
        stop_snap_speed: 0.08,
    };

    match action {
        FighterAction::Idle | FighterAction::Moving => {
            profile.ground_friction = 18.0;
            profile.stop_friction = 22.0;
            profile.turn_brake = 8.4;
            profile.stop_snap_speed = 0.16;
        }
        FighterAction::Jumping => {
            profile.input_scale = 0.72;
            profile.air_friction = 0.78;
            profile.stop_friction = 0.92;
            profile.gravity_scale = 0.94;
            profile.fall_gravity_scale = 1.22;
            profile.takeoff_gravity_scale = 0.5;
            profile.terminal_fall_speed = 13.8;
        }
        FighterAction::Dashing => {
            profile.input_scale = 0.18;
            profile.max_speed_bonus = 3.4;
            profile.ground_friction = 5.8;
            profile.stop_friction = 8.8;
            profile.turn_brake = 1.8;
            profile.landing_input_scale = 0.24;
            profile.stop_snap_speed = 0.05;
        }
        FighterAction::DashAttack => {
            profile.input_scale = 0.06;
            profile.max_speed_bonus = 2.6;
            profile.ground_friction = 8.5;
            profile.stop_friction = 12.5;
            profile.turn_brake = 1.0;
            profile.landing_input_scale = 0.12;
        }
        FighterAction::Guarding => {
            profile.input_scale = 0.0;
            profile.ground_friction = 20.0;
            profile.stop_friction = 24.0;
            profile.turn_brake = 10.0;
            profile.landing_input_scale = 0.08;
            profile.stop_snap_speed = 0.2;
        }
        FighterAction::LightAttack1 | FighterAction::LightAttack2 => {
            profile.input_scale = 0.12;
            profile.ground_friction = 13.5;
            profile.stop_friction = 16.5;
            profile.turn_brake = 2.4;
            profile.landing_input_scale = 0.1;
        }
        FighterAction::ComboFinisher => {
            profile.input_scale = 0.04;
            profile.max_speed_bonus = 12.0;
            profile.ground_friction = 1.8;
            profile.stop_friction = 3.0;
            profile.turn_brake = 0.2;
            profile.landing_input_scale = 0.04;
        }
        FighterAction::HeavyAttack => {
            profile.input_scale = 0.04;
            profile.max_speed_bonus = 5.2;
            profile.ground_friction = 4.2;
            profile.stop_friction = 5.8;
            profile.turn_brake = 0.6;
            profile.landing_input_scale = 0.06;
        }
        FighterAction::HeavyAttack2 => {
            profile.input_scale = 0.04;
            profile.max_speed_bonus = 2.4;
            profile.ground_friction = 6.8;
            profile.air_friction = 0.9;
            profile.stop_friction = 8.2;
            profile.turn_brake = 0.45;
            profile.gravity_scale = 0.82;
            profile.landing_input_scale = 0.06;
        }
        FighterAction::UltimateStartup | FighterAction::UltimateRush => {
            profile.input_scale = 0.0;
            profile.max_speed_bonus = 3.8;
            profile.ground_friction = 7.0;
            profile.stop_friction = 9.0;
            profile.turn_brake = 0.0;
            profile.landing_input_scale = 0.0;
            profile.stop_snap_speed = 0.02;
        }
        FighterAction::GrabStartup
        | FighterAction::Throwing
        | FighterAction::SpecialCast
        | FighterAction::ItemSwing
        | FighterAction::ItemThrow
        | FighterAction::GuardCounter => {
            profile.input_scale = 0.05;
            profile.ground_friction = 11.0;
            profile.stop_friction = 13.0;
            profile.turn_brake = 1.5;
            profile.landing_input_scale = 0.06;
        }
        FighterAction::ItemPickup | FighterAction::ItemDrop | FighterAction::LandingRecovery => {
            profile.input_scale = 0.1;
            profile.ground_friction = 22.0;
            profile.stop_friction = 27.0;
            profile.turn_brake = 8.0;
            profile.landing_input_scale = 0.04;
            profile.landing_brake = 18.0;
            profile.stop_snap_speed = 0.24;
        }
        FighterAction::JumpAttack => {
            profile.input_scale = 0.0;
            profile.air_friction = 0.38;
            profile.stop_friction = 0.58;
            profile.gravity_scale = 0.72;
            profile.fall_gravity_scale = 1.45;
            profile.takeoff_gravity_scale = 0.48;
            profile.terminal_fall_speed = 16.2;
            profile.landing_input_scale = 0.05;
        }
        FighterAction::JumpHeavyAttack => {
            profile.input_scale = 0.02;
            profile.air_friction = 3.4;
            profile.stop_friction = 4.0;
            profile.gravity_scale = 0.28;
            profile.fall_gravity_scale = 0.74;
            profile.takeoff_gravity_scale = 0.24;
            profile.terminal_fall_speed = 8.8;
            profile.landing_input_scale = 0.04;
        }
        FighterAction::GuardStep | FighterAction::RecoveryRoll => {
            profile.input_scale = 0.0;
            profile.max_speed_bonus = 2.6;
            profile.ground_friction = 5.0;
            profile.stop_friction = 7.2;
            profile.turn_brake = 0.0;
            profile.landing_input_scale = 0.0;
            profile.stop_snap_speed = 0.04;
        }
        FighterAction::Hitstun => {
            profile.input_scale = 0.0;
            profile.ground_friction = 5.0;
            profile.air_friction = 0.28;
            profile.stop_friction = 4.8;
            profile.gravity_scale = 0.96;
            profile.fall_gravity_scale = 1.08;
            profile.terminal_fall_speed = 18.5;
            profile.stop_snap_speed = 0.03;
        }
        FighterAction::Knockdown
        | FighterAction::RingOut
        | FighterAction::Grabbed
        | FighterAction::UltimateVictim
        | FighterAction::GetUp
        | FighterAction::GuardBroken => {
            profile.input_scale = 0.0;
            profile.ground_friction = 9.0;
            profile.air_friction = 0.32;
            profile.stop_friction = 18.0;
            profile.gravity_scale = 1.05;
            profile.fall_gravity_scale = 1.22;
            profile.landing_input_scale = 0.0;
            profile.landing_brake = 20.0;
            profile.stop_snap_speed = 0.18;
        }
        FighterAction::GrabHold => {
            profile.input_scale = 0.08;
            profile.ground_friction = 16.0;
            profile.stop_friction = 20.0;
            profile.landing_input_scale = 0.08;
        }
        FighterAction::QuickStand => {
            profile.input_scale = 0.12;
            profile.ground_friction = 18.0;
            profile.stop_friction = 24.0;
            profile.landing_input_scale = 0.08;
            profile.stop_snap_speed = 0.2;
        }
        FighterAction::Respawning => {}
    }

    profile
}

fn styled_body_motion_profile(action: FighterAction, style: FighterStyleKind) -> BodyMotionProfile {
    let mut profile = body_motion_profile(action);
    match style {
        FighterStyleKind::Anchor => {
            profile.stop_friction += 1.6;
            profile.landing_brake += 2.0;
            profile.terminal_fall_speed *= 1.06;
            if matches!(
                action,
                FighterAction::HeavyAttack
                    | FighterAction::HeavyAttack2
                    | FighterAction::ComboFinisher
            ) {
                profile.input_scale *= 0.82;
                profile.turn_brake *= 0.8;
            }
        }
        FighterStyleKind::Vector => {
            profile.turn_brake += 1.4;
            profile.landing_input_scale = (profile.landing_input_scale + 0.06).min(0.32);
            profile.takeoff_gravity_scale *= 0.92;
            profile.stop_snap_speed *= 0.82;
        }
        FighterStyleKind::Catalyst => {
            profile.air_friction += 0.08;
            profile.stop_friction += 0.8;
            profile.gravity_scale *= 0.98;
            profile.fall_gravity_scale *= 0.98;
        }
    }
    profile
}

fn character_body_motion_profile(
    action: FighterAction,
    style: FighterStyleKind,
    body: CharacterBodyDef,
) -> BodyMotionProfile {
    let mut profile = styled_body_motion_profile(action, style);
    profile.stop_friction *= body.stop_friction;
    profile.landing_brake *= body.landing_stick;
    profile.gravity_scale *= body.gravity;
    profile.fall_gravity_scale *= body.fall_gravity;
    profile.terminal_fall_speed *= body.fall_gravity.max(0.2);
    profile
}

fn character_body_motion_profile_for_state(
    action: &FighterActionState,
    motor: &FighterMotor,
    style: FighterStyleKind,
    body: CharacterBodyDef,
) -> BodyMotionProfile {
    let mut profile = character_body_motion_profile(action.action, style, body);
    if action.technique_id == Some(TechniqueId::CatUltimateStartup) {
        let dash_c = character_body_motion_profile(FighterAction::ComboFinisher, style, body);
        profile.input_scale = 0.0;
        profile.max_speed_bonus = dash_c.max_speed_bonus * 2.0;
        profile.ground_friction = dash_c.ground_friction;
        profile.stop_friction = dash_c.stop_friction;
        profile.turn_brake = dash_c.turn_brake;
        profile.landing_input_scale = 0.0;
        profile.stop_snap_speed = dash_c.stop_snap_speed;
    } else if matches!(
        action.technique_id,
        Some(
            TechniqueId::PenguinDashHeavy
                | TechniqueId::PenguinUltimateStartup
                | TechniqueId::PenguinUltimateRush
        )
    ) {
        profile.input_scale = 0.0;
        profile.max_speed_bonus = 10.5;
        profile.ground_friction = 1.1;
        profile.air_friction = 0.18;
        profile.stop_friction = 1.4;
        profile.turn_brake = 0.1;
        profile.gravity_scale *= 0.86;
        profile.fall_gravity_scale *= 1.08;
        profile.landing_input_scale = 0.0;
        profile.stop_snap_speed = 0.01;
    } else if action.technique_id == Some(TechniqueId::ChickJumpHeavy) {
        profile.input_scale = 0.0;
        profile.max_speed_bonus = CHICK_FRESH_EGG_RIDE_FORWARD_SPEED;
        profile.air_friction = 0.08;
        profile.stop_friction = 0.08;
        profile.gravity_scale *= 0.38;
        profile.fall_gravity_scale *= 0.66;
        profile.landing_input_scale = 0.0;
        profile.stop_snap_speed = 0.01;
    } else if action.technique_id == Some(TechniqueId::ChickJumpAttack) {
        profile.input_scale = 0.0;
        profile.max_speed_bonus = CHICK_JUMP_C_FORWARD_SPEED;
        profile.air_friction = 0.12;
        profile.stop_friction = 0.12;
        profile.gravity_scale *= 0.34;
        profile.fall_gravity_scale *= 0.58;
        profile.landing_input_scale = 0.0;
        profile.stop_snap_speed = 0.01;
    } else if matches!(
        action.technique_id,
        Some(TechniqueId::ChickDashAttack | TechniqueId::ChickDashHeavy)
    ) {
        profile.input_scale = 0.0;
        profile.max_speed_bonus = CHICK_DASH_X_BACKSTEP_SPEED;
        profile.ground_friction = 0.0;
        profile.air_friction = 0.0;
        profile.stop_friction = 0.0;
        profile.turn_brake = 0.0;
        profile.landing_input_scale = 0.0;
        profile.stop_snap_speed = 0.01;
    } else if (motor.bee_air_dash_motion_active
        && matches!(
            action.action,
            FighterAction::Jumping | FighterAction::JumpAttack | FighterAction::JumpHeavyAttack
        ))
        || matches!(
            action.technique_id,
            Some(TechniqueId::BeeJumpAttack | TechniqueId::BeeJumpHeavy)
        )
    {
        profile.input_scale = 0.0;
        profile.max_speed_bonus = BEE_JUMP_ATTACK_FORWARD_SPEED;
        profile.air_friction = 0.08;
        profile.stop_friction = 0.08;
        profile.gravity_scale *= 0.42;
        profile.fall_gravity_scale *= 0.82;
        profile.landing_input_scale = 0.0;
        profile.stop_snap_speed = 0.01;
    }
    profile
}

fn planar_speed_limit(
    motor: &FighterMotor,
    grounded: bool,
    style_speed: f32,
    profile: BodyMotionProfile,
) -> f32 {
    let mut base = if grounded {
        MAX_GROUND_SPEED * style_speed
    } else {
        MAX_AIR_SPEED * style_speed
    } + profile.max_speed_bonus;
    if !grounded && motor.dash_jump_carry_timer > 0.0 {
        base = base.max(motor.dash_jump_carry_speed_limit);
    }
    if motor.impact_speed_limit_timer > 0.0 {
        base.max(motor.impact_speed_limit)
    } else {
        base
    }
}

fn authored_gravity_velocity_y(
    velocity_y: f32,
    dt: f32,
    jump_takeoff_timer: f32,
    profile: BodyMotionProfile,
) -> f32 {
    let takeoff_scale = if jump_takeoff_timer > 0.0 {
        profile.takeoff_gravity_scale
    } else {
        1.0
    };
    let fall_scale = if velocity_y < 0.0 {
        profile.fall_gravity_scale
    } else {
        profile.gravity_scale
    };
    (velocity_y - GRAVITY * takeoff_scale * fall_scale * dt).max(-profile.terminal_fall_speed)
}

fn settle_planar_velocity(motor: &mut FighterMotor, desired: Vec3, profile: BodyMotionProfile) {
    let planar = Vec2::new(motor.velocity.x, motor.velocity.z);
    if desired.length_squared() <= 0.01 && planar.length() <= profile.stop_snap_speed {
        motor.velocity.x = 0.0;
        motor.velocity.z = 0.0;
    }
}

fn should_cancel_axis_velocity(velocity: f32, correction: f32) -> bool {
    correction.abs() > 0.001 && velocity.abs() > 0.001 && velocity.signum() != correction.signum()
}

fn ground_bounce_velocity(fall_velocity: f32) -> f32 {
    (-fall_velocity * 0.42).clamp(2.2, 4.8)
}

fn landing_stick_duration(fall_velocity: f32) -> f32 {
    if fall_velocity < -8.0 {
        0.1
    } else if fall_velocity < -4.5 {
        0.065
    } else if fall_velocity < -2.0 {
        0.035
    } else {
        0.0
    }
}

fn aftermath_feedback_priority(family: ReactionFamilyId) -> u8 {
    match family {
        ReactionFamilyId::AirFishKnockdown => 54,
        ReactionFamilyId::GroundBounceDown => 52,
        ReactionFamilyId::AerialSpikeDown => 50,
        ReactionFamilyId::GroundedDownGetup => 46,
        _ => 42,
    }
}

fn aftermath_landing_shake(family: ReactionFamilyId) -> f32 {
    match family {
        ReactionFamilyId::AirFishKnockdown => 0.38,
        ReactionFamilyId::GroundBounceDown => 0.34,
        ReactionFamilyId::AerialSpikeDown => 0.28,
        ReactionFamilyId::GroundedDownGetup => 0.24,
        _ => 0.18,
    }
}

fn wall_bounce_velocity(velocity: Vec3, correction: Vec3) -> Option<Vec3> {
    let mut bounced = velocity;
    let mut bounced_any = false;
    if should_cancel_axis_velocity(velocity.x, correction.x) {
        bounced.x = correction.x.signum() * velocity.x.abs() * 0.56;
        bounced_any = true;
    }
    if should_cancel_axis_velocity(velocity.z, correction.z) {
        bounced.z = correction.z.signum() * velocity.z.abs() * 0.56;
        bounced_any = true;
    }
    if bounced_any {
        bounced.y = bounced.y.max(2.1);
        Some(bounced)
    } else {
        None
    }
}

fn should_defer_knockout_resolution(motor: &FighterMotor, action: &FighterActionState) -> bool {
    matches!(action.action, FighterAction::Hitstun)
        && (!motor.grounded
            || motor.landing_aftermath.is_some()
            || motor.knockdown_on_land
            || motor.reaction_bounces > 0)
}

pub fn apply_fighter_movement(
    time: Res<Time>,
    mut commands: Commands,
    effect_assets: Res<EffectAssets>,
    hitstop: Res<Hitstop>,
    character_catalog: Res<CharacterMoveCatalog>,
    mut feedback: ResMut<HitEffects>,
    mut haptics: ResMut<CombatHapticQueue>,
    penguin_surfaces: Query<(&ActivePenguinSurface, &Transform), Without<Fighter>>,
    mut fighters: Query<
        (
            &Fighter,
            &FighterInput,
            &FighterStyle,
            &FighterCharacter,
            &mut FighterStats,
            &mut FighterMotor,
            &mut FighterActionState,
            &mut Transform,
        ),
        With<Fighter>,
    >,
) {
    if hitstop.active() {
        return;
    }

    let dt = time.delta_secs();

    for (fighter, input, style, character, mut stats, mut motor, mut action, mut transform) in
        &mut fighters
    {
        let tuning = style_tuning(style.kind);
        let body = character_catalog.body(character.kind);
        stats.item_speed_timer = (stats.item_speed_timer - dt).max(0.0);
        stats.item_giant_timer = (stats.item_giant_timer - dt).max(0.0);
        motor.landing_stick_timer = (motor.landing_stick_timer - dt).max(0.0);
        motor.jump_takeoff_timer = (motor.jump_takeoff_timer - dt).max(0.0);
        motor.dash_slide_timer = (motor.dash_slide_timer - dt).max(0.0);
        motor.dash_jump_carry_timer = (motor.dash_jump_carry_timer - dt).max(0.0);
        motor.impact_speed_limit_timer = (motor.impact_speed_limit_timer - dt).max(0.0);
        if motor.dash_jump_carry_timer == 0.0 {
            motor.dash_jump_carry_speed_limit = 0.0;
        }
        if motor.impact_speed_limit_timer == 0.0 {
            motor.impact_speed_limit = 0.0;
        }
        if matches!(
            action.action,
            FighterAction::RingOut | FighterAction::Respawning
        ) {
            clear_penguin_hard_ice_slide_state(&mut motor);
            clear_bee_air_dash_state(&mut motor);
            continue;
        }

        let was_grounded = motor.grounded;
        let previous_y_velocity = motor.velocity.y;
        if motor.grounded {
            motor.ledge_grace_timer = LEDGE_GRACE_SECONDS;
        } else {
            motor.ledge_grace_timer = (motor.ledge_grace_timer - dt).max(0.0);
        }

        let mut desired = Vec3::new(input.movement.x, 0.0, input.movement.y);
        desired = desired.normalize_or_zero();
        let mut profile =
            character_body_motion_profile_for_state(&action, &motor, style.kind, body);
        let ice = penguin_ice_modifier(transform.translation, character.kind, &penguin_surfaces);
        let hard_ice_slide_direction = update_penguin_hard_ice_slide_state(
            &mut motor,
            &mut desired,
            action.action,
            ice.is_some_and(|ice| ice.hard_slide),
        );
        if motor.grounded
            && let Some(ice) = ice
        {
            profile.ground_friction *= ice.ground_friction_scale;
            profile.stop_friction *= ice.stop_friction_scale;
            profile.turn_brake *= ice.turn_brake_scale;
            profile.input_scale *= ice.input_scale;
            motor.dash_slide_timer = motor.dash_slide_timer.max(ice.dash_slide_timer);
        }
        if motor.grounded && motor.landing_stick_timer > 0.0 {
            desired *= profile.landing_input_scale;
        }

        let character_speed = if motor.grounded {
            body.ground_speed
        } else {
            body.air_speed
        } * stats.item_speed_multiplier();
        let accel = if motor.grounded {
            GROUND_ACCEL
        } else {
            AIR_ACCEL
        } * profile.input_scale
            * if motor.grounded {
                tuning.ground_speed
            } else {
                tuning.air_speed
            }
            * character_speed;
        motor.velocity.x += desired.x * accel * dt;
        motor.velocity.z += desired.z * accel * dt;

        let max_speed = planar_speed_limit(
            &motor,
            motor.grounded,
            if motor.grounded {
                tuning.ground_speed
            } else {
                tuning.air_speed
            } * character_speed,
            profile,
        );
        let xz = Vec2::new(motor.velocity.x, motor.velocity.z);
        if xz.length() > max_speed {
            let clamped = xz.normalize() * max_speed;
            motor.velocity.x = clamped.x;
            motor.velocity.z = clamped.y;
        }

        let friction = if motor.grounded {
            profile.ground_friction
        } else {
            profile.air_friction
        };
        let velocity_xz = Vec2::new(motor.velocity.x, motor.velocity.z);
        let desired_xz = Vec2::new(desired.x, desired.z);
        let turning_against_velocity = desired.length_squared() > 0.01
            && velocity_xz.length_squared() > 0.01
            && desired_xz.dot(velocity_xz.normalize_or_zero()) < -0.15;
        if desired.length_squared() < 0.01 || profile.input_scale < 0.5 || turning_against_velocity
        {
            let brake = if turning_against_velocity {
                friction + profile.turn_brake
            } else if motor.grounded && motor.landing_stick_timer > 0.0 {
                friction + profile.landing_brake
            } else if motor.grounded
                && motor.dash_slide_timer > 0.0
                && desired.length_squared() < 0.01
            {
                DASH_SLIDE_FRICTION
            } else if desired.length_squared() < 0.01 {
                profile.stop_friction
            } else {
                friction
            };
            let damp = (1.0 - brake * dt).clamp(0.0, 1.0);
            motor.velocity.x *= damp;
            motor.velocity.z *= damp;
            settle_planar_velocity(&mut motor, desired, profile);
            if motor.grounded
                && motor.dash_slide_timer > 0.0
                && planar_velocity(&motor).length() <= DASH_SLIDE_STOP_SPEED
            {
                set_planar_velocity(&mut motor, Vec2::ZERO);
                motor.dash_slide_timer = 0.0;
            }
        }
        if let Some(direction) = hard_ice_slide_direction {
            force_penguin_hard_ice_slide_velocity(&mut motor, direction);
        }

        let ground = ground_height_at_with_radius(
            transform.translation.x,
            transform.translation.z,
            FIGHTER_RADIUS * stats.item_size_multiplier(),
        );
        if !motor.grounded
            || ground.is_none()
            || transform.translation.y > ground.unwrap_or(-99.0) + LANDING_SNAP_TOLERANCE
        {
            motor.velocity.y = authored_gravity_velocity_y(
                motor.velocity.y,
                dt,
                motor.jump_takeoff_timer,
                profile,
            );
        }

        transform.translation += motor.velocity * dt;
        let before_collision = transform.translation;
        transform.translation = resolve_platform_side_collision(
            transform.translation,
            FIGHTER_RADIUS * stats.item_size_multiplier(),
        );
        let correction = transform.translation - before_collision;
        let mut did_wall_bounce = false;
        if action.action == FighterAction::Hitstun
            && motor.reaction_bounces > 0
            && let Some(bounced) = wall_bounce_velocity(motor.velocity, correction)
        {
            motor.velocity = bounced;
            motor.reaction_bounces = motor.reaction_bounces.saturating_sub(1);
            motor.knockdown_on_land = true;
            action.elapsed = 0.0;
            did_wall_bounce = true;
            spawn_dust_puff(&mut commands, &effect_assets, transform.translation);
            feedback.push_combat_sfx(CombatSfxCue::new(
                CombatSfxKind::WallImpact,
                transform.translation,
                52,
            ));
            haptics.push(CombatHapticCue::secondary(fighter.id));
        }
        if !did_wall_bounce && should_cancel_axis_velocity(motor.velocity.x, correction.x) {
            motor.velocity.x = 0.0;
        }
        if !did_wall_bounce && should_cancel_axis_velocity(motor.velocity.z, correction.z) {
            motor.velocity.z = 0.0;
        }

        if let Some(ground_y) = ground_support_at_with_radius(
            transform.translation.x,
            transform.translation.z,
            FIGHTER_RADIUS,
        )
        .height()
        {
            if transform.translation.y <= ground_y + LANDING_SNAP_TOLERANCE
                && motor.velocity.y <= 0.0
            {
                transform.translation.y = ground_y;
                if !was_grounded && previous_y_velocity < -2.0 {
                    spawn_dust_puff(&mut commands, &effect_assets, transform.translation);
                }
                if !was_grounded {
                    motor.landing_stick_timer = motor
                        .landing_stick_timer
                        .max(landing_stick_duration(previous_y_velocity));
                }
                if let Some(aftermath) = motor.landing_aftermath.take() {
                    spawn_aftermath_pulse(
                        &mut commands,
                        &effect_assets,
                        transform.translation,
                        aftermath.family,
                    );
                    motor.knockdown_on_land = false;
                    motor.reaction_bounces = 0;
                    motor.pig_air_meat_slam_air_hits = 0;
                    motor.jump_attack_landing_recovery = false;
                    clear_bee_air_dash_state(&mut motor);
                    motor.velocity.x *= aftermath.horizontal_damping;
                    motor.velocity.z *= aftermath.horizontal_damping;
                    motor.landing_stick_timer = motor
                        .landing_stick_timer
                        .max(aftermath.landing_stick_ms as f32 / 1000.0);
                    action.action = FighterAction::Knockdown;
                    action.elapsed = 0.0;
                    action.hitbox_spawned = false;
                    action.queued_combo = false;
                    action.queued_technique = None;
                    action.queued_button = None;
                    clear_buffered_button(&mut action);
                    action.timeline_events_fired = 0;
                    action.reaction_getup_ms = Some(aftermath.getup_transition_ms);
                    action.reaction_recover_ms = Some(aftermath.recover_ms);
                    action.clear_reaction_visual();
                    feedback.shake = feedback
                        .shake
                        .max(aftermath_landing_shake(aftermath.family));
                    feedback.push_feedback_cue(
                        aftermath.cue,
                        ImpactSource::FighterStrike,
                        aftermath_feedback_priority(aftermath.family),
                    );
                    feedback.push_combat_sfx(CombatSfxCue::new(
                        CombatSfxKind::GroundImpact,
                        transform.translation,
                        ground_impact_priority(aftermath.family),
                    ));
                    haptics.push(CombatHapticCue::secondary(fighter.id));
                } else if motor.knockdown_on_land {
                    if motor.reaction_bounces > 0 && previous_y_velocity < -4.6 {
                        motor.reaction_bounces = motor.reaction_bounces.saturating_sub(1);
                        motor.velocity.y = ground_bounce_velocity(previous_y_velocity);
                        motor.velocity.x *= 0.72;
                        motor.velocity.z *= 0.72;
                        motor.grounded = false;
                        clear_bee_air_dash_state(&mut motor);
                        action.action = FighterAction::Hitstun;
                        action.elapsed = 0.0;
                        action.hitbox_spawned = false;
                        action.queued_combo = false;
                        action.queued_technique = None;
                        action.queued_button = None;
                        clear_buffered_button(&mut action);
                        action.timeline_events_fired = 0;
                        let reaction_visual_side = action.reaction_visual_side;
                        action.set_reaction_visual(
                            ReactionFamilyId::GroundBounceDown,
                            reaction_visual_side,
                        );
                        spawn_dust_puff(&mut commands, &effect_assets, transform.translation);
                        feedback.push_combat_sfx(CombatSfxCue::new(
                            CombatSfxKind::GroundImpact,
                            transform.translation,
                            50,
                        ));
                        haptics.push(CombatHapticCue::secondary(fighter.id));
                        continue;
                    }
                    motor.knockdown_on_land = false;
                    motor.reaction_bounces = 0;
                    motor.pig_air_meat_slam_air_hits = 0;
                    motor.jump_attack_landing_recovery = false;
                    clear_bee_air_dash_state(&mut motor);
                    action.action = FighterAction::Knockdown;
                    action.elapsed = 0.0;
                    action.hitbox_spawned = false;
                    action.queued_combo = false;
                    action.queued_technique = None;
                    action.queued_button = None;
                    clear_buffered_button(&mut action);
                    action.timeline_events_fired = 0;
                    action.clear_reaction_visual();
                    feedback.push_combat_sfx(CombatSfxCue::new(
                        CombatSfxKind::GroundImpact,
                        transform.translation,
                        46,
                    ));
                    haptics.push(CombatHapticCue::secondary(fighter.id));
                } else if motor.jump_attack_landing_recovery {
                    motor.jump_attack_landing_recovery = false;
                    clear_bee_air_dash_state(&mut motor);
                    action.action = FighterAction::LandingRecovery;
                    action.elapsed = 0.0;
                    action.hitbox_spawned = false;
                    action.queued_combo = false;
                    action.queued_technique = None;
                    action.queued_button = None;
                    clear_buffered_button(&mut action);
                    action.timeline_events_fired = 0;
                    action.clear_reaction_visual();
                }
                motor.air_attack_used = false;
                clear_queued_air_attack(&mut motor);
                clear_bee_air_dash_state(&mut motor);
                motor.velocity.y = 0.0;
                motor.grounded = true;
                motor.pig_air_meat_slam_air_hits = 0;
                motor.ledge_grace_timer = LEDGE_GRACE_SECONDS;
                motor.dash_jump_carry_timer = 0.0;
                motor.dash_jump_carry_speed_limit = 0.0;
            }
        } else {
            if motor.grounded {
                motor.ledge_grace_timer = LEDGE_GRACE_SECONDS;
            }
            motor.grounded = false;
        }
    }
}

pub fn separate_fighters(
    character_catalog: Res<CharacterMoveCatalog>,
    mut fighters: Query<
        (
            Entity,
            &mut Transform,
            &mut FighterMotor,
            &FighterCharacter,
            &FighterStats,
            &FighterActionState,
        ),
        With<Fighter>,
    >,
) {
    for _ in 0..FIGHTER_BODY_SEPARATION_ITERATIONS {
        let snapshots = fighter_body_snapshots(&fighters, &character_catalog);
        let mut corrections: Vec<(Entity, Vec3)> = Vec::new();

        for i in 0..snapshots.len() {
            for j in (i + 1)..snapshots.len() {
                let a = snapshots[i];
                let b = snapshots[j];
                if a.velocity_y <= 0.0
                    && let Some(rise) =
                        body_box_landing_correction(a.body, b.body, FIGHTER_HEIGHT * 0.42)
                {
                    corrections.push((a.entity, Vec3::Y * rise));
                    continue;
                }
                if b.velocity_y <= 0.0
                    && let Some(rise) =
                        body_box_landing_correction(b.body, a.body, FIGHTER_HEIGHT * 0.42)
                {
                    corrections.push((b.entity, Vec3::Y * rise));
                    continue;
                }
                let Some(separation) = body_box_separation(a.body, b.body) else {
                    continue;
                };
                let correction = Vec3::new(separation.x * 0.5, 0.0, separation.y * 0.5);
                corrections.push((a.entity, correction));
                corrections.push((b.entity, -correction));
            }
        }

        if corrections.is_empty() {
            break;
        }

        for (entity, mut transform, mut motor, _, _, action) in &mut fighters {
            if !fighter_body_blocks_overlap(action.action) {
                continue;
            }
            let correction = corrections
                .iter()
                .filter(|(target, _)| *target == entity)
                .fold(Vec3::ZERO, |mut acc, (_, delta)| {
                    acc.x += delta.x;
                    acc.z += delta.z;
                    acc.y = acc.y.max(delta.y);
                    acc
                });
            if correction.length_squared() <= 0.000001 {
                continue;
            }
            transform.translation += correction;
            if correction.y > 0.0 {
                motor.velocity.y = 0.0;
                motor.grounded = true;
                motor.ledge_grace_timer = LEDGE_GRACE_SECONDS;
            }
            cancel_velocity_into_body_overlap(&mut motor, correction);
        }
    }
}

fn fighter_body_snapshots(
    fighters: &Query<
        (
            Entity,
            &mut Transform,
            &mut FighterMotor,
            &FighterCharacter,
            &FighterStats,
            &FighterActionState,
        ),
        With<Fighter>,
    >,
    character_catalog: &CharacterMoveCatalog,
) -> Vec<FighterBodySnapshot> {
    fighters
        .iter()
        .filter(|(_, _, _, _, _, action)| fighter_body_blocks_overlap(action.action))
        .map(
            |(entity, transform, motor, character, stats, _)| FighterBodySnapshot {
                entity,
                body: fighter_body_box(
                    transform.translation,
                    motor.facing,
                    character_catalog.body(character.kind),
                    stats.item_size_multiplier(),
                ),
                velocity_y: motor.velocity.y,
            },
        )
        .collect()
}

#[derive(Clone, Copy)]
struct FighterBodySnapshot {
    entity: Entity,
    body: FighterBodyBox,
    velocity_y: f32,
}

fn fighter_body_blocks_overlap(action: FighterAction) -> bool {
    !matches!(
        action,
        FighterAction::RingOut
            | FighterAction::Respawning
            | FighterAction::Grabbed
            | FighterAction::GrabHold
            | FighterAction::Throwing
            | FighterAction::UltimateVictim
            | FighterAction::UltimateRush
    )
}

fn cancel_velocity_into_body_overlap(motor: &mut FighterMotor, correction: Vec3) {
    let outward = Vec2::new(correction.x, correction.z).normalize_or_zero();
    if outward.length_squared() <= 0.0001 {
        return;
    }
    let velocity = Vec2::new(motor.velocity.x, motor.velocity.z);
    let inward_speed = velocity.dot(outward);
    if inward_speed < 0.0 {
        let adjusted = velocity - outward * inward_speed;
        motor.velocity.x = adjusted.x;
        motor.velocity.z = adjusted.y;
    }
}

pub fn ringout_and_respawn(
    time: Res<Time>,
    mut commands: Commands,
    effect_assets: Res<EffectAssets>,
    mut feedback: ResMut<HitEffects>,
    mut haptics: ResMut<CombatHapticQueue>,
    mut state: ResMut<MatchState>,
    mut telemetry: ResMut<MatchTelemetry>,
    mut announcements: ResMut<MatchAnnouncements>,
    mut fighters: Query<(
        &Fighter,
        &mut FighterStats,
        &mut FighterMotor,
        &mut FighterActionState,
        &mut FighterUltimateState,
        &mut DrunkStatus,
        &mut Transform,
        &mut Visibility,
    )>,
) {
    let dt = time.delta_secs();
    let mut awards = Vec::new();

    for (
        fighter,
        mut stats,
        mut motor,
        mut action,
        mut ultimate_state,
        mut drunk,
        transform,
        mut visibility,
    ) in &mut fighters
    {
        if state.fighter_eliminated(fighter.id) {
            continue;
        }

        let arena = active_arena_definition();
        let out = is_ringout_position(transform.translation, arena);
        let knocked_out = stats.health <= 0.0;

        if (out || (knocked_out && !should_defer_knockout_resolution(&motor, &action)))
            && !matches!(
                action.action,
                FighterAction::RingOut | FighterAction::Respawning
            )
        {
            action.action = FighterAction::RingOut;
            action.elapsed = 0.0;
            action.hitbox_spawned = false;
            action.queued_combo = false;
            action.queued_technique = None;
            action.queued_button = None;
            clear_buffered_button(&mut action);
            action.timeline_events_fired = 0;
            action.reaction_getup_ms = None;
            action.reaction_recover_ms = None;
            action.clear_reaction_visual();
            *drunk = DrunkStatus::default();
            let resolution = if out {
                let resolution = state.record_ringout(fighter.id, stats.last_attacker);
                telemetry.record_ringout(resolution.awarded_to.is_some());
                resolution
            } else {
                state.record_life_loss(
                    fighter.id,
                    stats.last_attacker,
                    crate::game_state::LifeLossCause::Knockout,
                )
            };
            stats.respawn_timer = if resolution.eliminated {
                0.0
            } else {
                RESPAWN_DELAY
            };
            stats.health_refill_timer = 0.0;
            motor.velocity = Vec3::ZERO;
            motor.knockdown_on_land = false;
            motor.landing_aftermath = None;
            motor.reaction_bounces = 0;
            motor.pig_air_meat_slam_air_hits = 0;
            motor.air_attack_used = false;
            motor.jump_attack_landing_recovery = false;
            clear_bee_air_dash_state(&mut motor);
            motor.landing_stick_timer = 0.0;
            motor.jump_takeoff_timer = 0.0;
            motor.dash_trail_timer = 0.0;
            motor.dash_slide_timer = 0.0;
            motor.dash_jump_carry_timer = 0.0;
            motor.dash_jump_carry_speed_limit = 0.0;
            motor.impact_speed_limit_timer = 0.0;
            motor.impact_speed_limit = 0.0;
            motor.guard_active_timer = 0.0;
            motor.guard_cooldown_timer = 0.0;
            motor.guard_start_buffer_timer = 0.0;
            motor.guard_was_requested = false;
            ultimate_state.target = None;
            ultimate_state.owner = None;
            if out {
                haptics.push(CombatHapticCue::contact(
                    resolution.awarded_to,
                    fighter.id,
                    HapticContactOutcome::Clean,
                    HapticImpactWeight::Terminal,
                ));
                spawn_ringout_burst(&mut commands, &effect_assets, transform.translation);
                let ringout_feedback = crate::combat::impact_feedback_profile(
                    ImpactSource::RingOut,
                    ImpactFeedbackIntensity::Heavy,
                );
                feedback.shake = feedback.shake.max(ringout_feedback.shake);
                feedback.push_feedback_cue(
                    ringout_feedback.cue,
                    ImpactSource::RingOut,
                    ringout_feedback.priority,
                );
            }
            let message = if resolution.match_finished {
                "Life match decided - press R".to_string()
            } else if resolution.eliminated {
                format!("{} eliminated", fighter.name)
            } else if let Some(stock) = resolution.remaining_stock {
                let label = if stock == 1 { "life" } else { "lives" };
                if out {
                    format!("{} ring out - {stock} {label} left", fighter.name)
                } else {
                    format!("{} KO - {stock} {label} left", fighter.name)
                }
            } else {
                if out {
                    format!("{} ring out", fighter.name)
                } else {
                    format!("{} KO", fighter.name)
                }
            };
            announcements.show(message, 1.1);
            *visibility = if out {
                Visibility::Hidden
            } else {
                Visibility::Visible
            };
            if let Some(attacker) = resolution.awarded_to {
                awards.push(attacker);
            }
        }
    }

    if !awards.is_empty() {
        for attacker in awards {
            for (fighter, mut stats, _, _, _, _, _, _) in &mut fighters {
                if fighter.id == attacker {
                    stats.score += 1;
                }
            }
        }
    }

    for (
        fighter,
        mut stats,
        mut motor,
        mut action,
        mut ultimate_state,
        mut drunk,
        mut transform,
        mut visibility,
    ) in &mut fighters
    {
        if state.fighter_eliminated(fighter.id) {
            continue;
        }

        match action.action {
            FighterAction::RingOut => {
                *drunk = DrunkStatus::default();
                stats.respawn_timer -= dt;
                if stats.respawn_timer <= 0.0 {
                    transform.translation = fighter.spawn;
                    transform.rotation = Quat::IDENTITY;
                    stats.health = MAX_HEALTH;
                    stats.health_refill_timer = 0.0;
                    stats.stamina = MAX_STAMINA;
                    stats.item_speed_timer = 0.0;
                    stats.item_giant_timer = 0.0;
                    stats.last_attacker = None;
                    stats.invulnerability = RESPAWN_INVULNERABLE;
                    stats.respawn_timer = 0.0;
                    stats.element_carry = None;
                    stats.element_carry_strength = 0.0;
                    stats.element_carry_timer = 0.0;
                    motor.velocity = Vec3::ZERO;
                    motor.grounded = true;
                    motor.knockdown_on_land = false;
                    motor.landing_aftermath = None;
                    motor.reaction_bounces = 0;
                    motor.pig_air_meat_slam_air_hits = 0;
                    motor.air_attack_used = false;
                    motor.jump_attack_landing_recovery = false;
                    clear_bee_air_dash_state(&mut motor);
                    motor.ledge_grace_timer = 0.0;
                    motor.landing_stick_timer = 0.0;
                    motor.jump_takeoff_timer = 0.0;
                    motor.dash_trail_timer = 0.0;
                    motor.dash_slide_timer = 0.0;
                    motor.dash_jump_carry_timer = 0.0;
                    motor.dash_jump_carry_speed_limit = 0.0;
                    motor.impact_speed_limit_timer = 0.0;
                    motor.impact_speed_limit = 0.0;
                    motor.guard_active_timer = 0.0;
                    motor.guard_cooldown_timer = 0.0;
                    motor.guard_start_buffer_timer = 0.0;
                    motor.guard_was_requested = false;
                    ultimate_state.target = None;
                    ultimate_state.owner = None;
                    action.action = FighterAction::Respawning;
                    action.elapsed = 0.0;
                    action.queued_combo = false;
                    action.queued_technique = None;
                    action.queued_button = None;
                    clear_buffered_button(&mut action);
                    action.timeline_events_fired = 0;
                    action.reaction_getup_ms = None;
                    action.reaction_recover_ms = None;
                    action.clear_reaction_visual();
                    *visibility = Visibility::Visible;
                    spawn_respawn_column(&mut commands, &effect_assets, transform.translation);
                    feedback.push_feedback_cue("respawn_return", ImpactSource::MatchFlow, 16);
                    announcements.show(format!("{} returns", fighter.name), 0.9);
                }
            }
            FighterAction::Respawning => {
                action.elapsed += dt;
                if action.elapsed >= 0.45 {
                    action.action = FighterAction::Idle;
                    action.elapsed = 0.0;
                    action.queued_combo = false;
                    action.queued_technique = None;
                    action.queued_button = None;
                    clear_buffered_button(&mut action);
                    action.timeline_events_fired = 0;
                    action.reaction_getup_ms = None;
                    action.reaction_recover_ms = None;
                    action.clear_reaction_visual();
                }
            }
            _ => {}
        }
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn refill_depleted_practice_health(
    state: Res<MatchState>,
    user_mode: Res<crate::user_mode::UserModeState>,
    control: Res<crate::bot::BotActionControl>,
    mut fighters: Query<(&Fighter, &mut FighterStats, &FighterActionState)>,
) {
    if user_mode.blocks_practice_health_refill() {
        return;
    }

    let Some(refill_bot_id) = control.refill_bot_id() else {
        return;
    };

    for (fighter, mut stats, action) in &mut fighters {
        if fighter.id != refill_bot_id || state.fighter_eliminated(fighter.id) {
            continue;
        }
        tick_practice_health_refill(&mut stats, &action);
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn tick_practice_health_refill(stats: &mut FighterStats, action: &FighterActionState) {
    if matches!(
        action.action,
        FighterAction::RingOut | FighterAction::Respawning
    ) {
        stats.health_refill_timer = 0.0;
        return;
    }

    if stats.health <= 0.0 {
        stats.health_refill_timer = 0.0;
        return;
    }

    if stats.health < MAX_HEALTH {
        stats.health = MAX_HEALTH;
        stats.health_refill_timer = 0.0;
        stats.element_carry = None;
        stats.element_carry_strength = 0.0;
        stats.element_carry_timer = 0.0;
    }
}

pub fn is_ringout_position(position: Vec3, arena: &ArenaDefinition) -> bool {
    position.y < arena.ringout_y
        || Vec2::new(position.x, position.z).length() > arena.ringout_radius
}

pub fn ringout_danger_level(position: Vec3, arena: &ArenaDefinition) -> f32 {
    let radial_distance = Vec2::new(position.x, position.z).length();
    let radial = ((radial_distance - (arena.ringout_radius - RINGOUT_RADIAL_WARNING_BAND))
        / RINGOUT_RADIAL_WARNING_BAND)
        .clamp(0.0, 1.0);
    let vertical = (((arena.ringout_y + RINGOUT_VERTICAL_WARNING_BAND) - position.y)
        / RINGOUT_VERTICAL_WARNING_BAND)
        .clamp(0.0, 1.0);
    radial.max(vertical)
}

pub fn sync_fighter_visuals(
    mut fighters: Query<
        (
            &FighterMotor,
            &FighterStats,
            &FighterActionState,
            &Children,
            &mut Transform,
        ),
        With<Fighter>,
    >,
    mut pose_roots: Query<&mut Transform, (With<FighterPoseRoot>, Without<Fighter>)>,
    time: Res<Time>,
    feel: Res<CombatFeelTuning>,
) {
    for (motor, stats, action, children, mut transform) in &mut fighters {
        transform.rotation = fighter_facing_rotation(motor.facing, transform.rotation);
        let speed_pulse = if stats.item_speed_timer > 0.0 {
            1.0 + (time.elapsed_secs() * 18.0).sin().abs() * 0.04
        } else {
            1.0
        };
        transform.scale = Vec3::splat(stats.item_size_multiplier() * speed_pulse);

        for child in children {
            let Ok(mut pose_transform) = pose_roots.get_mut(*child) else {
                continue;
            };
            *pose_transform = fighter_pose_root_transform(
                action,
                stats.invulnerability,
                time.elapsed_secs(),
                &feel,
            );
            break;
        }
    }
}

pub fn sync_guard_shield_visuals(
    fighters: Query<(&Fighter, &FighterActionState)>,
    mut shields: Query<(&FighterGuardShield, &mut Visibility, &mut Transform)>,
    time: Res<Time>,
) {
    let states: Vec<_> = fighters
        .iter()
        .map(|(fighter, action)| (fighter.id, action.action, action.elapsed))
        .collect();

    for (shield, mut visibility, mut transform) in &mut shields {
        let Some((_, action, elapsed)) = states
            .iter()
            .find(|(fighter_id, _, _)| *fighter_id == shield.fighter_id)
        else {
            *visibility = Visibility::Hidden;
            continue;
        };

        if guard_shield_visible(*action) {
            let pulse = 1.0 + (time.elapsed_secs() * 18.0).sin().abs() * 0.035;
            let settle = (*elapsed / 0.12).clamp(0.0, 1.0);
            *visibility = Visibility::Visible;
            *transform = guard_shield_transform()
                .with_scale(Vec3::new(0.88 + 0.12 * settle, 1.0, 1.0) * pulse);
        } else {
            *visibility = Visibility::Hidden;
            *transform = guard_shield_transform();
        }
    }
}

fn guard_shield_visible(action: FighterAction) -> bool {
    action == FighterAction::Guarding
}

pub fn sync_light_punch_corner_cues(
    fighters: Query<(&Fighter, &FighterCharacter, &FighterActionState)>,
    mut corner_cues: Query<(
        &mut FighterLightPunchCornerTint,
        &mut Visibility,
        &mut Transform,
        &mut MeshMaterial3d<StandardMaterial>,
    )>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let states: Vec<_> = fighters
        .iter()
        .map(|(fighter, character, action)| {
            (
                fighter.id,
                character.kind,
                action.action,
                action.technique_id,
                action.elapsed,
            )
        })
        .collect();

    for (mut cue, mut visibility, mut transform, mut mesh_material) in &mut corner_cues {
        let Some((_, character, action, technique_id, elapsed)) = states
            .iter()
            .find(|(fighter_id, _, _, _, _)| *fighter_id == cue.fighter_id)
        else {
            *visibility = Visibility::Hidden;
            continue;
        };
        sync_light_punch_corner_tint_material(
            &mut cue,
            &mut mesh_material,
            &mut materials,
            *character,
        );
        let Some((side, amount)) = light_punch_corner_cue(*action, *technique_id, *elapsed) else {
            *visibility = Visibility::Hidden;
            continue;
        };

        *visibility = Visibility::Visible;
        *transform = light_punch_corner_cue_transform(*character, side, amount);
    }
}

fn sync_light_punch_corner_tint_material(
    cue: &mut FighterLightPunchCornerTint,
    mesh_material: &mut MeshMaterial3d<StandardMaterial>,
    materials: &mut Assets<StandardMaterial>,
    character: CharacterKind,
) {
    if cue.character == character {
        return;
    }
    mesh_material.0 = materials.add(light_punch_corner_tint_material(character));
    cue.character = character;
}

fn light_punch_corner_cue(
    action: FighterAction,
    technique_id: Option<TechniqueId>,
    elapsed: f32,
) -> Option<(f32, f32)> {
    if !matches!(
        action,
        FighterAction::LightAttack1 | FighterAction::LightAttack2
    ) {
        return None;
    }
    let side = light_attack_pose_side(technique_id?)?;
    let amount = light_punch_corner_cue_amount(elapsed);
    (amount > 0.05).then_some((side, amount))
}

fn light_punch_corner_cue_amount(elapsed: f32) -> f32 {
    let fade_in = eased01(elapsed / 0.035);
    let fade_out = 1.0 - eased01((elapsed - 0.18) / 0.08);
    (fade_in * fade_out).clamp(0.0, 1.0)
}

fn light_punch_corner_cue_transform(
    character: CharacterKind,
    visual_side: f32,
    amount: f32,
) -> Transform {
    let bounds = character_mesh_bounds(character);
    let min = Vec3::from_array(bounds.min);
    let max = Vec3::from_array(bounds.max);
    let corner_side = light_attack_forward_corner_side(visual_side);
    let x = if corner_side > 0.0 { max.x } else { min.x };
    let y = min.y + (max.y - min.y) * 0.64;
    let z = max.z;
    let scene_offset = Vec3::new(0.0, KENNEY_CUBE_PET_GROUND_OFFSET - FIGHTER_BODY_Y, 0.0);
    let mesh_corner = scene_offset + Vec3::new(x, y, z) * KENNEY_CUBE_PET_SCALE;
    let outward_offset = Vec3::new(corner_side * 0.018, 0.0, 0.018);
    let pulse = 0.9 + amount.clamp(0.0, 1.0) * 0.12;

    Transform::from_translation(mesh_corner + outward_offset).with_scale(Vec3::new(
        corner_side * pulse,
        pulse,
        pulse,
    ))
}

pub fn sync_fighter_tint_visuals(
    mut commands: Commands,
    fighters: Query<(
        &Fighter,
        &FighterCharacter,
        &FighterActionState,
        Option<&ArenaFighterBurn>,
        Option<&DrunkStatus>,
        &Children,
    )>,
    pose_roots: Query<(), With<FighterPoseRoot>>,
    child_query: Query<&Children>,
    tint_skip: Query<
        (),
        Or<(
            With<FighterStyleAccent>,
            With<FighterEquipmentChip>,
            With<FighterLightPunchCornerTint>,
        )>,
    >,
    mut mesh_materials: Query<&mut MeshMaterial3d<StandardMaterial>>,
    tint_materials: Query<&FighterTintMaterial>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    time: Res<Time>,
) {
    for (fighter, character, action, burn, drunk, children) in &fighters {
        let tint = active_fighter_tint(
            character.kind,
            action,
            burn.copied(),
            drunk.copied(),
            time.elapsed_secs(),
        );
        for child in children {
            if pose_roots.get(*child).is_err() {
                continue;
            }
            if let Some(tint) = tint {
                apply_fighter_tint_recursive(
                    *child,
                    fighter.id,
                    tint,
                    &child_query,
                    &tint_skip,
                    &mut mesh_materials,
                    &tint_materials,
                    &mut materials,
                    &mut commands,
                );
            } else {
                restore_fighter_tint_recursive(
                    *child,
                    fighter.id,
                    &child_query,
                    &mut mesh_materials,
                    &tint_materials,
                    &mut commands,
                );
            }
        }
    }
}

#[derive(Clone, Copy)]
struct FighterTint {
    amount: f32,
    palette: FighterTintPalette,
}

#[derive(Clone, Copy)]
enum FighterTintPalette {
    Burning,
    PigCharge,
    CounterFlash,
    Drunk,
}

fn active_fighter_tint(
    character: CharacterKind,
    action: &FighterActionState,
    burn: Option<ArenaFighterBurn>,
    drunk: Option<DrunkStatus>,
    elapsed: f32,
) -> Option<FighterTint> {
    if let Some(burn) = burn {
        return Some(FighterTint {
            amount: burn.visual_amount(),
            palette: FighterTintPalette::Burning,
        });
    }

    let counter = guard_counter_flash_tint_amount(action);
    if counter > 0.0 {
        return Some(FighterTint {
            amount: counter,
            palette: FighterTintPalette::CounterFlash,
        });
    }

    if let Some(drunk) = drunk {
        let amount = drunk_tint_amount(&drunk, elapsed);
        if amount > 0.0 {
            return Some(FighterTint {
                amount,
                palette: FighterTintPalette::Drunk,
            });
        }
    }

    let pig = pig_charge_tint_amount(character, action);
    (pig > 0.0).then_some(FighterTint {
        amount: pig,
        palette: FighterTintPalette::PigCharge,
    })
}

fn pig_charge_tint_amount(character: CharacterKind, action: &FighterActionState) -> f32 {
    if character != CharacterKind::Pig || action.charge_release_requested {
        return 0.0;
    }
    let charging_ground_heavy = action.technique_id == Some(TechniqueId::PigHeavy);
    let charging_dash_heavy =
        action.action == FighterAction::Dashing && action.charge_elapsed > 0.0;
    if charging_ground_heavy || charging_dash_heavy {
        (action.charge_elapsed / pig_heavy_full_charge_secs()).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn drunk_tint_amount(status: &DrunkStatus, elapsed: f32) -> f32 {
    if !status.active() {
        return 0.0;
    }
    let pulse = 0.78 + (elapsed * 11.0).sin().abs() * 0.22;
    let fade = (status.remaining / 0.5).min(1.0);
    pulse * fade
}

fn guard_counter_flash_tint_amount(action: &FighterActionState) -> f32 {
    if action.action != FighterAction::GuardCounter
        || action.elapsed >= GUARD_COUNTER_FLASH_DURATION
    {
        return 0.0;
    }
    1.0 - (action.elapsed / GUARD_COUNTER_FLASH_DURATION).clamp(0.0, 1.0)
}

#[allow(clippy::too_many_arguments)]
fn apply_fighter_tint_recursive(
    entity: Entity,
    fighter_id: usize,
    tint: FighterTint,
    child_query: &Query<&Children>,
    tint_skip: &Query<
        (),
        Or<(
            With<FighterStyleAccent>,
            With<FighterEquipmentChip>,
            With<FighterLightPunchCornerTint>,
        )>,
    >,
    mesh_materials: &mut Query<&mut MeshMaterial3d<StandardMaterial>>,
    tint_materials: &Query<&FighterTintMaterial>,
    materials: &mut Assets<StandardMaterial>,
    commands: &mut Commands,
) {
    if tint_skip.get(entity).is_err()
        && let Ok(mut mesh_material) = mesh_materials.get_mut(entity)
    {
        if let Ok(existing_tint) = tint_materials.get(entity) {
            if existing_tint.fighter_id == fighter_id {
                refresh_fighter_tint_material(
                    materials,
                    &existing_tint.original,
                    &existing_tint.tint,
                    tint,
                );
                mesh_material.0 = existing_tint.tint.clone();
            }
        } else {
            let original = mesh_material.0.clone();
            let base = materials
                .get(&original)
                .cloned()
                .unwrap_or_else(|| fallback_charge_tint_base(Color::WHITE));
            let tint_handle = materials.add(fighter_tinted_material(&base, tint));
            mesh_material.0 = tint_handle.clone();
            commands.entity(entity).insert(FighterTintMaterial {
                fighter_id,
                original,
                tint: tint_handle,
            });
        }
    }

    if let Ok(children) = child_query.get(entity) {
        for child in children {
            apply_fighter_tint_recursive(
                *child,
                fighter_id,
                tint,
                child_query,
                tint_skip,
                mesh_materials,
                tint_materials,
                materials,
                commands,
            );
        }
    }
}

fn restore_fighter_tint_recursive(
    entity: Entity,
    fighter_id: usize,
    child_query: &Query<&Children>,
    mesh_materials: &mut Query<&mut MeshMaterial3d<StandardMaterial>>,
    tint_materials: &Query<&FighterTintMaterial>,
    commands: &mut Commands,
) {
    if let Ok(tint) = tint_materials.get(entity)
        && tint.fighter_id == fighter_id
    {
        if let Ok(mut mesh_material) = mesh_materials.get_mut(entity) {
            mesh_material.0 = tint.original.clone();
        }
        commands.entity(entity).remove::<FighterTintMaterial>();
    }

    if let Ok(children) = child_query.get(entity) {
        for child in children {
            restore_fighter_tint_recursive(
                *child,
                fighter_id,
                child_query,
                mesh_materials,
                tint_materials,
                commands,
            );
        }
    }
}

fn refresh_fighter_tint_material(
    materials: &mut Assets<StandardMaterial>,
    original: &Handle<StandardMaterial>,
    tint: &Handle<StandardMaterial>,
    fighter_tint: FighterTint,
) {
    let base = materials
        .get(original)
        .cloned()
        .unwrap_or_else(|| fallback_charge_tint_base(Color::WHITE));
    if let Some(tint_material) = materials.get_mut(tint) {
        *tint_material = fighter_tinted_material(&base, fighter_tint);
    }
}

fn fallback_charge_tint_base(color: Color) -> StandardMaterial {
    StandardMaterial {
        base_color: color,
        perceptual_roughness: 0.65,
        ..default()
    }
}

fn fighter_tinted_material(base: &StandardMaterial, tint: FighterTint) -> StandardMaterial {
    match tint.palette {
        FighterTintPalette::Burning => burning_tinted_material(base, tint.amount),
        FighterTintPalette::PigCharge => charge_tinted_material(base, tint.amount),
        FighterTintPalette::CounterFlash => counter_flash_tinted_material(base, tint.amount),
        FighterTintPalette::Drunk => drunk_tinted_material(base, tint.amount),
    }
}

fn burning_tinted_material(base: &StandardMaterial, amount: f32) -> StandardMaterial {
    let amount = amount.clamp(0.0, 1.0);
    let mut material = base.clone();
    material.base_color = burning_tinted_color(base.base_color, amount);
    material.emissive =
        LinearRgba::from(Color::srgb(1.0, 0.12, 0.01).to_linear()) * (0.5 + amount * 3.2);
    material
}

fn burning_tinted_color(base: Color, amount: f32) -> Color {
    let amount = amount.clamp(0.0, 1.0);
    let base = base.to_srgba();
    Color::srgba(
        base.red + (1.0 - base.red) * (0.88 * amount),
        base.green * (1.0 - 0.72 * amount) + 0.12 * amount,
        base.blue * (1.0 - 0.9 * amount) + 0.02 * amount,
        base.alpha,
    )
}

fn charge_tinted_material(base: &StandardMaterial, amount: f32) -> StandardMaterial {
    let amount = amount.clamp(0.0, 1.0);
    let mut material = base.clone();
    material.base_color = charge_tinted_color(base.base_color, amount);
    material.emissive =
        LinearRgba::from(Color::srgb(1.0, 0.035, 0.02).to_linear()) * (0.15 + amount * 1.85);
    material
}

fn charge_tinted_color(base: Color, amount: f32) -> Color {
    let amount = amount.clamp(0.0, 1.0);
    let base = base.to_srgba();
    let hot = amount * amount;
    Color::srgba(
        base.red + (1.0 - base.red) * (0.95 * amount),
        base.green * (1.0 - 0.86 * amount) + 0.05 * hot,
        base.blue * (1.0 - 0.94 * amount) + 0.025 * hot,
        base.alpha,
    )
}

fn counter_flash_tinted_material(base: &StandardMaterial, amount: f32) -> StandardMaterial {
    let amount = amount.clamp(0.0, 1.0);
    let mut material = base.clone();
    material.base_color = counter_flash_tinted_color(base.base_color, amount);
    material.emissive =
        LinearRgba::from(Color::srgb(0.72, 0.95, 1.0).to_linear()) * (0.35 + amount * 2.65);
    material
}

fn counter_flash_tinted_color(base: Color, amount: f32) -> Color {
    let amount = amount.clamp(0.0, 1.0);
    let base = base.to_srgba();
    Color::srgba(
        base.red + (1.0 - base.red) * (0.82 * amount),
        base.green + (1.0 - base.green) * (0.9 * amount),
        base.blue + (1.0 - base.blue) * amount,
        base.alpha,
    )
}

fn drunk_tinted_material(base: &StandardMaterial, amount: f32) -> StandardMaterial {
    let amount = amount.clamp(0.0, 1.0);
    let mut material = base.clone();
    material.base_color = drunk_tinted_color(base.base_color, amount);
    material.emissive =
        LinearRgba::from(Color::srgb(0.34, 0.08, 0.72).to_linear()) * (0.18 + amount * 0.8);
    material
}

fn drunk_tinted_color(base: Color, amount: f32) -> Color {
    let amount = amount.clamp(0.0, 1.0);
    let base = base.to_srgba();
    let purple = Color::srgb(0.58, 0.24, 0.86).to_srgba();
    let mix = amount * 0.72;
    Color::srgba(
        base.red + (purple.red - base.red) * mix,
        base.green + (purple.green - base.green) * mix,
        base.blue + (purple.blue - base.blue) * mix,
        base.alpha,
    )
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct FighterVisualPose {
    pitch: f32,
    yaw: f32,
    roll: f32,
    scale: Vec3,
    translation: Vec3,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct HitReactionVisualProfile {
    pitch: f32,
    yaw: f32,
    roll: f32,
    scale: Vec3,
    translation: Vec3,
}

fn timeline_pulse(elapsed: f32, center: f32, half_width: f32) -> f32 {
    (1.0 - ((elapsed - center).abs() / half_width.max(0.001))).clamp(0.0, 1.0)
}

fn eased01(value: f32) -> f32 {
    let t = value.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn light_attack_pose_side(technique_id: TechniqueId) -> Option<f32> {
    match technique_id {
        TechniqueId::CatLight1 | TechniqueId::PigLight1 => Some(-1.0),
        TechniqueId::CatLight2 | TechniqueId::PigLight2 => Some(1.0),
        _ => None,
    }
}

fn light_attack_forward_corner_side(visual_side: f32) -> f32 {
    if visual_side < 0.0 { 1.0 } else { -1.0 }
}

fn hit_reaction_visual_profile(family: ReactionFamilyId) -> HitReactionVisualProfile {
    match family {
        ReactionFamilyId::ShortStandingStagger => HitReactionVisualProfile {
            pitch: 0.34,
            yaw: 0.18,
            roll: 0.22,
            scale: Vec3::new(1.08, 0.82, 1.08),
            translation: Vec3::new(0.0, 0.02, -0.035),
        },
        ReactionFamilyId::MediumStandingStagger => HitReactionVisualProfile {
            pitch: 0.46,
            yaw: 0.28,
            roll: 0.36,
            scale: Vec3::new(1.12, 0.76, 1.12),
            translation: Vec3::new(0.0, 0.025, -0.055),
        },
        ReactionFamilyId::HeavyStandingStagger => HitReactionVisualProfile {
            pitch: 0.64,
            yaw: 0.4,
            roll: 0.56,
            scale: Vec3::new(1.18, 0.68, 1.18),
            translation: Vec3::new(0.0, 0.03, -0.08),
        },
        ReactionFamilyId::FrozenStun => HitReactionVisualProfile {
            pitch: 0.08,
            yaw: 0.0,
            roll: 0.0,
            scale: Vec3::new(1.04, 0.92, 1.04),
            translation: Vec3::new(0.0, 0.01, -0.02),
        },
        ReactionFamilyId::LightAirPop | ReactionFamilyId::CounterPop => HitReactionVisualProfile {
            pitch: -0.42,
            yaw: 0.3,
            roll: 0.42,
            scale: Vec3::new(0.94, 1.2, 0.94),
            translation: Vec3::new(0.0, 0.16, -0.04),
        },
        ReactionFamilyId::LauncherDown => HitReactionVisualProfile {
            pitch: -0.56,
            yaw: 0.34,
            roll: 0.52,
            scale: Vec3::new(0.92, 1.28, 0.92),
            translation: Vec3::new(0.0, 0.2, -0.045),
        },
        ReactionFamilyId::GroundBounceDown
        | ReactionFamilyId::AerialSpikeDown
        | ReactionFamilyId::AirFishKnockdown => HitReactionVisualProfile {
            pitch: 0.82,
            yaw: 0.5,
            roll: 0.82,
            scale: Vec3::new(1.16, 0.72, 1.22),
            translation: Vec3::new(0.0, 0.04, -0.09),
        },
        ReactionFamilyId::UltimateLockedStagger => HitReactionVisualProfile {
            pitch: 0.52,
            yaw: 0.48,
            roll: 0.64,
            scale: Vec3::new(1.16, 0.72, 1.16),
            translation: Vec3::new(0.0, 0.03, -0.075),
        },
        ReactionFamilyId::GroundedDownGetup
        | ReactionFamilyId::SlidingKnockdown
        | ReactionFamilyId::UltimateBombDown => HitReactionVisualProfile {
            pitch: 1.02,
            yaw: 0.58,
            roll: 0.96,
            scale: Vec3::new(1.22, 0.62, 1.24),
            translation: Vec3::new(0.0, 0.02, -0.11),
        },
    }
}

fn hit_reaction_pose(
    elapsed: f32,
    reaction_family: Option<ReactionFamilyId>,
    visual_side: f32,
) -> FighterVisualPose {
    let profile = hit_reaction_visual_profile(
        reaction_family.unwrap_or(ReactionFamilyId::ShortStandingStagger),
    );
    let side = if visual_side < 0.0 { -1.0 } else { 1.0 };
    let snap = timeline_pulse(elapsed, 0.055, 0.075);
    let secondary = timeline_pulse(elapsed, 0.16, 0.16) * 0.28;
    let recover = eased01((elapsed - 0.14) / 0.28);
    let amount = (snap + secondary).clamp(0.0, 1.0) * (1.0 - recover * 0.5);
    let base = action_visual_scale(FighterAction::Hitstun);

    FighterVisualPose {
        pitch: profile.pitch * amount,
        yaw: side * profile.yaw * amount,
        roll: side * profile.roll * amount,
        scale: base.lerp(profile.scale, amount),
        translation: profile.translation * amount,
    }
}

fn light_attack_pose(elapsed: f32, side: f32) -> FighterVisualPose {
    let base = action_visual_scale(FighterAction::LightAttack1);
    let windup = eased01(elapsed / 0.05);
    let snap = timeline_pulse(elapsed, 0.08, 0.09);
    let recover = eased01((elapsed - 0.12) / 0.16);
    let impact = (snap + windup * 0.45).clamp(0.0, 1.0) * (1.0 - recover * 0.35);

    FighterVisualPose {
        pitch: 0.08 + impact * 0.14 - recover * 0.04,
        yaw: side * (0.3 * windup + 0.6 * snap) * (1.0 - recover * 0.55),
        roll: side * (0.12 + 0.36 * impact) * (1.0 - recover * 0.5),
        scale: base.lerp(Vec3::new(1.17, 0.88, 1.22), impact),
        translation: Vec3::ZERO,
    }
}

fn penguin_snowflake_cast_pose() -> FighterVisualPose {
    FighterVisualPose {
        pitch: 0.0,
        yaw: 0.0,
        roll: 0.0,
        scale: Vec3::ONE,
        translation: Vec3::ZERO,
    }
}

fn heavy_step_pose(elapsed: f32) -> FighterVisualPose {
    let base = action_visual_scale(FighterAction::HeavyAttack);
    let windup = eased01(elapsed / 0.12);
    let impact = timeline_pulse(elapsed, 0.16, 0.11);
    let recover = eased01((elapsed - 0.24) / 0.16);
    let weight = (windup * 0.45 + impact).clamp(0.0, 1.0);

    FighterVisualPose {
        pitch: -0.1 - windup * 0.18 - impact * 0.12 + recover * 0.12,
        yaw: -0.1 * windup + impact * 0.18,
        roll: -0.08 * windup - impact * 0.28 + recover * 0.1,
        scale: base.lerp(Vec3::new(1.2, 0.84, 1.28), weight),
        translation: Vec3::ZERO,
    }
}

fn launcher_pose(elapsed: f32) -> FighterVisualPose {
    let base = action_visual_scale(FighterAction::HeavyAttack2);
    let coil = eased01(elapsed / 0.16);
    let rise = eased01((elapsed - 0.14) / 0.2);
    let impact = timeline_pulse(elapsed, 0.3, 0.13);
    let weight = (rise * 0.7 + impact * 0.5).clamp(0.0, 1.0);

    FighterVisualPose {
        pitch: -0.08 - coil * 0.2 - rise * 0.62 + impact * 0.06,
        yaw: 0.12 * coil + 0.28 * impact,
        roll: -0.1 - rise * 0.28 - impact * 0.18,
        scale: base.lerp(Vec3::new(1.14, 1.1, 1.0), weight),
        translation: Vec3::ZERO,
    }
}

fn pig_ham_launcher_pose(elapsed: f32) -> FighterVisualPose {
    let brace = eased01(elapsed / 0.18);
    let swing = timeline_pulse(elapsed, 0.42, 0.2);
    let launch = timeline_pulse(elapsed, 0.66, 0.16);
    let recover = eased01((elapsed - 0.84) / 0.28);
    let impact = swing.max(launch);

    FighterVisualPose {
        pitch: -0.04 - brace * 0.12 - launch * 0.18 + recover * 0.08,
        yaw: 0.18 * brace + swing * 0.86 + launch * 0.22 - recover * 0.16,
        roll: -0.08 - swing * 0.76 - launch * 0.24 + recover * 0.18,
        scale: Vec3::new(
            1.12 + impact * 0.14,
            0.96 - brace * 0.06 - impact * 0.12,
            1.16 + brace * 0.06 + impact * 0.18,
        ),
        translation: Vec3::ZERO,
    }
}

fn combo_finisher_pose(elapsed: f32) -> FighterVisualPose {
    let base = action_visual_scale(FighterAction::ComboFinisher);
    let windup = eased01(elapsed / 0.12);
    let twist = timeline_pulse(elapsed, 0.24, 0.18);
    let slam = timeline_pulse(elapsed, 0.3, 0.12);
    let recover = eased01((elapsed - 0.4) / 0.24);
    let impact = twist.max(slam);

    FighterVisualPose {
        pitch: -0.1 * windup - 0.56 * slam + 0.2 * recover,
        yaw: -0.22 * windup + 1.15 * twist - 0.18 * recover,
        roll: -0.2 * windup + 0.95 * twist - 0.16 * recover,
        scale: base.lerp(Vec3::new(1.28, 0.74, 1.34), impact),
        translation: Vec3::ZERO,
    }
}

fn ultimate_startup_pose(elapsed: f32) -> FighterVisualPose {
    let base = action_visual_scale(FighterAction::UltimateStartup);
    let charge = eased01(elapsed / 0.32);
    let catch = timeline_pulse(elapsed, 0.08, 0.08);
    let flicker = (elapsed * 28.0).sin().abs();
    let weight = charge.max(catch);

    FighterVisualPose {
        pitch: -0.28 - charge * 0.18 - catch * 0.1 + flicker * 0.04,
        yaw: (elapsed * 18.0).sin() * 0.24 * (0.4 + charge * 0.6),
        roll: -0.12 - charge * 0.14 + (elapsed * 22.0).sin() * 0.1,
        scale: base.lerp(Vec3::new(1.24, 0.78, 1.32), weight),
        translation: Vec3::ZERO,
    }
}

fn ultimate_rush_pose(elapsed: f32) -> FighterVisualPose {
    let base = action_visual_scale(FighterAction::UltimateRush);
    let light =
        (timeline_pulse(elapsed, 0.09, 0.06) + timeline_pulse(elapsed, 0.24, 0.06)).clamp(0.0, 1.0);
    let heavy =
        (timeline_pulse(elapsed, 0.39, 0.08) + timeline_pulse(elapsed, 0.54, 0.08)).clamp(0.0, 1.0);
    let bomb = timeline_pulse(elapsed, 0.82, 0.2);
    let impact = (light * 0.45 + heavy * 0.7 + bomb).clamp(0.0, 1.0);

    FighterVisualPose {
        pitch: -0.34 - heavy * 0.12 - bomb * 0.28 + (elapsed * 26.0).sin() * 0.06,
        yaw: (elapsed * 32.0).sin() * 0.26 + light * 0.18 - heavy * 0.2,
        roll: (elapsed * 30.0).sin() * 0.22 + heavy * 0.24 - bomb * 0.12,
        scale: base.lerp(Vec3::new(1.32, 0.74, 1.38), impact),
        translation: Vec3::ZERO,
    }
}

fn bee_ultimate_swarm_pose(elapsed: f32) -> FighterVisualPose {
    let base = action_visual_scale(FighterAction::UltimateStartup);
    let charge = eased01(elapsed / 0.24);
    let release = timeline_pulse(elapsed, 0.12, 0.12)
        + timeline_pulse(elapsed, 0.36, 0.12)
        + timeline_pulse(elapsed, 0.62, 0.14);
    let burst = timeline_pulse(elapsed, 0.72, 0.22);
    let buzz = (elapsed * 46.0).sin();
    let impact = (release * 0.45 + burst).clamp(0.0, 1.0);

    FighterVisualPose {
        pitch: -0.18 - charge * 0.12 - burst * 0.24 + buzz * 0.05,
        yaw: buzz * 0.18 + release * 0.08,
        roll: (elapsed * 52.0).sin() * 0.18 - burst * 0.1,
        scale: base.lerp(Vec3::new(1.28, 0.72, 1.36), impact),
        translation: Vec3::ZERO,
    }
}

fn pig_ultimate_rush_pose(elapsed: f32) -> FighterVisualPose {
    let brace = (elapsed / 0.22).clamp(0.0, 1.0);
    let crush = timeline_pulse(elapsed, 0.24, 0.16);
    let slam = timeline_pulse(elapsed, 0.64, 0.18);
    let bomb = timeline_pulse(elapsed, 1.08, 0.26);
    let impact_weight = (crush * 0.6 + slam * 0.8 + bomb).clamp(0.0, 1.0);

    FighterVisualPose {
        pitch: -0.28 - brace * 0.16 - crush * 0.16 - slam * 0.24 - bomb * 0.34,
        yaw: -crush * 0.08 + slam * 0.1 + bomb * 0.04,
        roll: -0.06 - crush * 0.08 + slam * 0.1 - bomb * 0.05,
        scale: Vec3::new(
            1.22 + brace * 0.05 + impact_weight * 0.08,
            0.86 - brace * 0.07 - impact_weight * 0.1,
            1.26 + brace * 0.12 + impact_weight * 0.16,
        ),
        translation: Vec3::ZERO,
    }
}

fn authored_visual_action_requires_technique_id(action: FighterAction) -> bool {
    matches!(
        action,
        FighterAction::LightAttack1
            | FighterAction::LightAttack2
            | FighterAction::ComboFinisher
            | FighterAction::HeavyAttack
            | FighterAction::HeavyAttack2
            | FighterAction::UltimateStartup
            | FighterAction::UltimateRush
            | FighterAction::DashAttack
            | FighterAction::JumpAttack
            | FighterAction::JumpHeavyAttack
    )
}

fn technique_visual_pose(action: &FighterActionState) -> Option<FighterVisualPose> {
    let technique_id = action.technique_id?;
    if let Some(side) = light_attack_pose_side(technique_id) {
        return Some(light_attack_pose(action.elapsed, side));
    }

    Some(match technique_id {
        TechniqueId::PenguinLight1 | TechniqueId::PenguinLight2 => penguin_snowflake_cast_pose(),
        TechniqueId::CatComboFinisher | TechniqueId::CatDashComboFinisher => {
            combo_finisher_pose(action.elapsed)
        }
        TechniqueId::CatHeavy => heavy_step_pose(action.elapsed),
        TechniqueId::CatHeavy2 => launcher_pose(action.elapsed),
        TechniqueId::CatUltimateStartup => ultimate_startup_pose(action.elapsed),
        TechniqueId::CatUltimateRush => ultimate_rush_pose(action.elapsed),
        TechniqueId::BeeUltimateStartup
        | TechniqueId::BeeLegacyUltimateStartup
        | TechniqueId::BeeLegacyUltimateRush => bee_ultimate_swarm_pose(action.elapsed),
        TechniqueId::CatDashAttack => FighterVisualPose {
            pitch: -0.34,
            yaw: 0.0,
            roll: 0.0,
            scale: action_visual_scale(FighterAction::DashAttack),
            translation: Vec3::ZERO,
        },
        TechniqueId::CatJumpAttack => FighterVisualPose {
            pitch: 0.32,
            yaw: 0.0,
            roll: 0.0,
            scale: action_visual_scale(FighterAction::JumpAttack),
            translation: Vec3::ZERO,
        },
        TechniqueId::CatJumpHeavy => FighterVisualPose {
            pitch: -0.12,
            yaw: 0.18,
            roll: 0.1,
            scale: action_visual_scale(FighterAction::JumpHeavyAttack),
            translation: Vec3::ZERO,
        },
        TechniqueId::PigHeavy => FighterVisualPose {
            pitch: 0.0,
            yaw: 0.0,
            roll: 0.0,
            scale: pig_heavy_body_scale(action.charge_elapsed, action.charge_release_requested),
            translation: Vec3::ZERO,
        },
        TechniqueId::PigHeavy2 => pig_ham_launcher_pose(action.elapsed),
        TechniqueId::PigJumpHeavy => FighterVisualPose {
            pitch: 0.72,
            yaw: 0.0,
            roll: -0.24,
            scale: Vec3::new(1.18, 0.82, 1.2),
            translation: Vec3::ZERO,
        },
        TechniqueId::PigUltimateStartup => {
            let windup = (action.elapsed / 0.98).clamp(0.0, 1.0);
            FighterVisualPose {
                pitch: -0.28 - windup * 0.22,
                yaw: 0.0,
                roll: -0.04,
                scale: Vec3::new(
                    1.1 + windup * 0.1,
                    0.9 - windup * 0.08,
                    1.16 + windup * 0.08,
                ),
                translation: Vec3::ZERO,
            }
        }
        TechniqueId::PigUltimateRush => pig_ultimate_rush_pose(action.elapsed),
        _ => return None,
    })
}

fn fighter_visual_pose(
    action: &FighterActionState,
    time_secs: f32,
    feel: &CombatFeelTuning,
) -> FighterVisualPose {
    let mut pose = if action.action == FighterAction::Hitstun {
        hit_reaction_pose(
            action.elapsed,
            action.reaction_family,
            action.reaction_visual_side,
        )
    } else {
        technique_visual_pose(action).unwrap_or_else(|| {
            if authored_visual_action_requires_technique_id(action.action) {
                FighterVisualPose {
                    pitch: 0.0,
                    yaw: 0.0,
                    roll: 0.0,
                    scale: action_visual_scale(action.action),
                    translation: Vec3::ZERO,
                }
            } else {
                FighterVisualPose {
                    pitch: match action.action {
                        FighterAction::Moving => (time_secs * 11.0).sin() * 0.06,
                        FighterAction::Dashing => -0.22,
                        FighterAction::LandingRecovery => 0.24,
                        FighterAction::UltimateVictim => 0.32,
                        FighterAction::GrabStartup => -0.18,
                        FighterAction::GrabHold => -0.24,
                        FighterAction::Grabbed => 0.4,
                        FighterAction::Throwing => 0.34,
                        FighterAction::SpecialCast => 0.24,
                        FighterAction::ItemPickup => -0.1,
                        FighterAction::ItemSwing => 0.3,
                        FighterAction::ItemThrow => 0.22,
                        FighterAction::ItemDrop => -0.08,
                        FighterAction::Guarding => -0.12,
                        FighterAction::GuardCounter => 0.18,
                        FighterAction::GuardStep => -0.26,
                        FighterAction::Hitstun => 0.22,
                        FighterAction::Knockdown | FighterAction::RingOut => {
                            KNOCKDOWN_HEAD_LOW_PITCH
                        }
                        FighterAction::QuickStand => 0.82,
                        FighterAction::RecoveryRoll => 1.05,
                        FighterAction::GetUp => getup_belly_up_pitch(action),
                        FighterAction::GuardBroken => -0.28,
                        _ => 0.0,
                    },
                    yaw: match action.action {
                        FighterAction::UltimateVictim => std::f32::consts::PI,
                        _ => 0.0,
                    },
                    roll: match action.action {
                        FighterAction::UltimateVictim => 0.24,
                        _ => 0.0,
                    },
                    scale: action_visual_scale(action.action),
                    translation: Vec3::ZERO,
                }
            }
        })
    };
    if let Some(override_def) = feel.pose_override(action.action) {
        if let Some(value) = override_def.pitch {
            pose.pitch = value;
        }
        if let Some(value) = override_def.yaw {
            pose.yaw = value;
        }
        if let Some(value) = override_def.roll {
            pose.roll = value;
        }
        if let Some([x, y, z]) = override_def.scale {
            pose.scale = Vec3::new(x, y, z);
        }
    }
    pose
}

fn fighter_facing_rotation(facing: Vec3, fallback: Quat) -> Quat {
    let facing = facing.normalize_or_zero();
    if facing.length_squared() > 0.1 {
        Quat::from_rotation_y(facing.x.atan2(facing.z))
    } else {
        fallback
    }
}

fn fighter_pose_root_transform(
    action: &FighterActionState,
    invulnerability: f32,
    time_secs: f32,
    feel: &CombatFeelTuning,
) -> Transform {
    let pose = fighter_visual_pose(action, time_secs, feel);
    let pulse = if invulnerability > 0.0 {
        1.0 + (time_secs * 18.0).sin().abs() * 0.08
    } else {
        1.0
    };

    Transform::from_translation(fighter_pose_root_translation() + pose.translation)
        .with_rotation(
            Quat::from_rotation_y(pose.yaw)
                * Quat::from_rotation_z(pose.roll)
                * Quat::from_rotation_x(pose.pitch),
        )
        .with_scale(pose.scale * pulse)
}

fn action_visual_scale(action: FighterAction) -> Vec3 {
    match action {
        FighterAction::Dashing => Vec3::new(1.12, 0.86, 1.12),
        FighterAction::DashAttack => Vec3::new(1.18, 0.82, 1.22),
        FighterAction::JumpAttack => Vec3::new(1.08, 0.9, 1.14),
        FighterAction::JumpHeavyAttack => Vec3::new(1.08, 0.94, 1.16),
        FighterAction::LandingRecovery => Vec3::new(1.02, 0.82, 1.02),
        FighterAction::Guarding => Vec3::new(0.94, 0.94, 1.08),
        FighterAction::LightAttack1 => Vec3::new(1.03, 0.99, 1.06),
        FighterAction::LightAttack2 => Vec3::new(1.05, 0.98, 1.05),
        FighterAction::ComboFinisher => Vec3::new(1.1, 0.94, 1.1),
        FighterAction::HeavyAttack => Vec3::new(1.12, 0.96, 1.16),
        FighterAction::HeavyAttack2 => Vec3::new(1.08, 1.04, 1.08),
        FighterAction::UltimateStartup => Vec3::new(1.16, 0.88, 1.2),
        FighterAction::UltimateRush => Vec3::new(1.22, 0.84, 1.26),
        FighterAction::UltimateVictim => Vec3::new(1.04, 0.78, 1.04),
        FighterAction::GrabStartup => Vec3::new(1.04, 0.92, 1.12),
        FighterAction::GrabHold => Vec3::new(1.06, 0.9, 1.08),
        FighterAction::Grabbed => Vec3::new(1.05, 0.76, 1.05),
        FighterAction::Throwing => Vec3::new(1.12, 0.92, 1.06),
        FighterAction::SpecialCast => Vec3::new(1.08, 0.96, 1.12),
        FighterAction::ItemPickup => Vec3::new(1.02, 0.88, 1.02),
        FighterAction::ItemSwing => Vec3::new(1.14, 0.95, 1.18),
        FighterAction::ItemThrow => Vec3::new(1.1, 0.94, 1.14),
        FighterAction::ItemDrop => Vec3::new(0.98, 0.94, 1.02),
        FighterAction::Hitstun => Vec3::new(1.05, 0.9, 1.05),
        FighterAction::GuardCounter => Vec3::new(1.1, 0.94, 1.12),
        FighterAction::GuardStep => Vec3::new(1.08, 0.84, 1.14),
        FighterAction::Knockdown | FighterAction::RingOut => Vec3::new(1.12, 0.62, 1.12),
        FighterAction::QuickStand => Vec3::new(1.08, 0.68, 1.08),
        FighterAction::RecoveryRoll => Vec3::new(1.16, 0.42, 1.16),
        FighterAction::GetUp => Vec3::new(1.08, 0.74, 1.08),
        FighterAction::GuardBroken => Vec3::new(0.9, 0.82, 0.9),
        _ => Vec3::ONE,
    }
}

fn getup_belly_up_pitch(action: &FighterActionState) -> f32 {
    let duration = action
        .reaction_recover_ms
        .map(|ms| ms as f32 / 1000.0)
        .unwrap_or(GETUP_DURATION)
        .max(0.001);
    let progress = (action.elapsed / duration).clamp(0.0, 1.0);
    KNOCKDOWN_HEAD_LOW_PITCH * (1.0 - progress)
}

pub fn sync_loadout_visuals(
    fighters: Query<(&Fighter, &FighterStyle, &FighterEquipment)>,
    mut style_accents: Query<
        (
            &mut FighterStyleAccent,
            &mut MeshMaterial3d<StandardMaterial>,
            &mut Transform,
        ),
        Without<FighterEquipmentChip>,
    >,
    mut equipment_chips: Query<
        (
            &mut FighterEquipmentChip,
            &mut MeshMaterial3d<StandardMaterial>,
        ),
        Without<FighterStyleAccent>,
    >,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let loadouts: Vec<_> = fighters
        .iter()
        .map(|(fighter, style, equipment)| (fighter.id, style.kind, equipment.kind))
        .collect();

    for (mut accent, mut material, mut transform) in &mut style_accents {
        let Some((_, style_kind, _)) = loadouts
            .iter()
            .find(|(fighter_id, _, _)| *fighter_id == accent.fighter_id)
        else {
            continue;
        };
        let identity = style_identity(*style_kind);
        transform.scale = Vec3::splat(identity.marker_scale);
        if accent.kind != *style_kind {
            accent.kind = *style_kind;
            material.0 = materials.add(StandardMaterial {
                base_color: identity.accent,
                emissive: LinearRgba::from(identity.accent.to_linear()) * 0.16,
                perceptual_roughness: 0.58,
                ..default()
            });
        }
    }

    for (mut chip, mut material) in &mut equipment_chips {
        let Some((_, _, equipment_kind)) = loadouts
            .iter()
            .find(|(fighter_id, _, _)| *fighter_id == chip.fighter_id)
        else {
            continue;
        };
        if chip.kind != *equipment_kind {
            chip.kind = *equipment_kind;
            let identity = equipment_identity(*equipment_kind);
            material.0 = materials.add(StandardMaterial {
                base_color: identity.accent,
                emissive: LinearRgba::from(identity.accent.to_linear()) * 0.12,
                perceptual_roughness: 0.5,
                ..default()
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gamepad_with_buttons(buttons: &[GamepadButton]) -> Gamepad {
        let mut gamepad = Gamepad::default();
        for button in buttons {
            gamepad.digital_mut().press(*button);
        }
        gamepad
    }

    #[test]
    fn gamepad_movement_deadzone_is_radial_and_rescaled() {
        assert_eq!(
            apply_gamepad_movement_deadzone(Vec2::new(GAMEPAD_MOVEMENT_DEADZONE, 0.0)),
            Vec2::ZERO
        );
        assert_eq!(
            apply_gamepad_movement_deadzone(Vec2::new(0.12, 0.12)),
            Vec2::ZERO
        );
        let outside = apply_gamepad_movement_deadzone(Vec2::new(0.6, 0.0));
        assert_vec2_close(outside, Vec2::new(0.5, 0.0), 0.001);
    }

    #[test]
    fn xbox_buttons_map_to_fixed_actions() {
        let gamepad = gamepad_with_buttons(&[
            GamepadButton::South,
            GamepadButton::West,
            GamepadButton::North,
            GamepadButton::East,
            GamepadButton::RightTrigger2,
            GamepadButton::LeftTrigger,
            GamepadButton::LeftTrigger2,
            GamepadButton::RightTrigger,
        ]);
        let sample = gamepad_action_sample(&gamepad);

        assert!(sample.jump_just);
        assert!(sample.light_just);
        assert!(sample.heavy_just);
        assert!(sample.grab_just);
        assert!(sample.aim_held);
        assert!(sample.dash_just);
        assert!(sample.guard_held);
        assert!(sample.ultimate_just);
        assert!(!sample.special_just);
        assert_eq!(sample.special_kind, None);
    }

    #[test]
    fn xbox_right_bumper_and_all_former_modifiers_produce_no_special_request() {
        let modifiers = [
            GamepadButton::East,
            GamepadButton::North,
            GamepadButton::LeftTrigger,
        ];
        for mask in 0..(1 << modifiers.len()) {
            let mut buttons = vec![GamepadButton::RightTrigger];
            for (index, modifier) in modifiers.iter().enumerate() {
                if mask & (1 << index) != 0 {
                    buttons.push(*modifier);
                }
            }
            let sample = gamepad_action_sample(&gamepad_with_buttons(&buttons));
            assert!(!sample.special_just);
            assert_eq!(sample.special_kind, None);
        }
    }

    #[test]
    fn default_e_and_all_former_keyboard_modifiers_produce_no_special_request() {
        let bindings = PlayerControlBindings::player_one_default();
        assert_eq!(bindings.special, KeyCode::KeyE);
        let modifiers = [bindings.light, bindings.aim_grab, bindings.heavy];
        for mask in 0..(1 << modifiers.len()) {
            let mut keys = ButtonInput::default();
            for (index, modifier) in modifiers.iter().enumerate() {
                if mask & (1 << index) != 0 {
                    keys.press(*modifier);
                }
            }
            keys.press(KeyCode::KeyE);
            let sample = keyboard_action_sample(&keys, bindings, false);
            assert!(!sample.special_just);
            assert_eq!(sample.special_kind, None);
        }
    }

    #[test]
    fn normalized_device_input_rejects_injected_special_samples() {
        let mut dash = DashTapTracker::default();
        let mut guard = GuardChordTracker::default();
        let mut flick = StickFlickTracker::default();
        let mut input = FighterInput::default();
        collect_device_player_input(
            DeviceActionSample {
                special_just: true,
                special_kind: Some(SpecialInputKind::Hazard),
                ..default()
            },
            1.0,
            0.0,
            &mut dash,
            &mut guard,
            &mut flick,
            &mut input,
        );

        assert!(!input.special);
        assert_eq!(input.special_kind, None);
    }

    #[test]
    fn xbox_shortcuts_keep_guard_and_ultimate_chords() {
        let mut dash = DashTapTracker::default();
        let mut guard = GuardChordTracker::default();
        let mut flick = StickFlickTracker::default();
        let mut input = FighterInput::default();
        collect_device_player_input(
            gamepad_action_sample(&gamepad_with_buttons(&[
                GamepadButton::West,
                GamepadButton::North,
            ])),
            1.0,
            0.0,
            &mut dash,
            &mut guard,
            &mut flick,
            &mut input,
        );
        assert!(input.guard);

        let mut input = FighterInput::default();
        collect_device_player_input(
            gamepad_action_sample(&gamepad_with_buttons(&[
                GamepadButton::East,
                GamepadButton::West,
                GamepadButton::North,
            ])),
            2.0,
            0.0,
            &mut dash,
            &mut guard,
            &mut flick,
            &mut input,
        );
        assert!(input.ultimate);
    }

    #[test]
    fn gamepad_flick_dash_requires_neutral_reset() {
        let mut tracker = StickFlickTracker::default();
        assert!(!tracker.update(Vec2::splat(0.4)));
        assert!(tracker.update(Vec2::X));
        assert!(!tracker.update(Vec2::X));
        assert!(!tracker.update(Vec2::ZERO));
        assert!(tracker.update(Vec2::NEG_X));
    }

    #[test]
    fn changing_seat_device_resets_all_gesture_trackers() {
        let first = Entity::from_raw_u32(11).expect("valid entity");
        let second = Entity::from_raw_u32(12).expect("valid entity");
        let mut trackers = PlayerInputTrackers::default();
        trackers.reset_if_assignment_changed(0, LocalInputAssignment::Gamepad(first));
        trackers.dash[0].left = Some(1.0);
        trackers.guard[0].guard_latched = true;
        trackers.stick_flick[0].engaged = true;

        trackers.reset_if_assignment_changed(0, LocalInputAssignment::Gamepad(second));

        assert_eq!(trackers.dash[0].left, None);
        assert!(!trackers.guard[0].guard_latched);
        assert!(!trackers.stick_flick[0].engaged);
    }

    fn assert_vec3_close(actual: Vec3, expected: Vec3, tolerance: f32) {
        assert!(
            actual.distance(expected) <= tolerance,
            "expected {actual:?} to be within {tolerance} of {expected:?}"
        );
    }

    fn assert_vec2_close(actual: Vec2, expected: Vec2, tolerance: f32) {
        assert!(
            actual.distance(expected) <= tolerance,
            "expected {actual:?} to be within {tolerance} of {expected:?}"
        );
    }

    fn chord_guard() -> GuardChordOutput {
        GuardChordOutput {
            guard: true,
            ..default()
        }
    }

    fn chord_light() -> GuardChordOutput {
        GuardChordOutput {
            light: true,
            ..default()
        }
    }

    fn chord_heavy() -> GuardChordOutput {
        GuardChordOutput {
            heavy: true,
            ..default()
        }
    }

    fn chord_ultimate() -> GuardChordOutput {
        GuardChordOutput {
            ultimate: true,
            ..default()
        }
    }

    fn resolve_guard_chord_input(
        tracker: &mut GuardChordTracker,
        light_just: bool,
        heavy_just: bool,
        light_held: bool,
        heavy_held: bool,
        now: f32,
    ) -> GuardChordOutput {
        super::resolve_guard_chord_input(
            tracker, light_just, heavy_just, false, light_held, heavy_held, false, now,
        )
    }

    #[test]
    fn ultimate_chord_beats_guard_and_grab_when_three_buttons_are_close() {
        let mut tracker = GuardChordTracker::default();

        assert_eq!(
            super::resolve_guard_chord_input(
                &mut tracker,
                true,
                false,
                false,
                true,
                false,
                false,
                1.0
            ),
            GuardChordOutput::default()
        );
        assert_eq!(
            super::resolve_guard_chord_input(
                &mut tracker,
                false,
                true,
                false,
                true,
                true,
                false,
                1.02
            ),
            chord_guard()
        );
        assert_eq!(
            super::resolve_guard_chord_input(
                &mut tracker,
                false,
                false,
                true,
                true,
                true,
                true,
                1.04
            ),
            chord_ultimate()
        );
        assert_eq!(
            super::resolve_guard_chord_input(
                &mut tracker,
                false,
                false,
                false,
                true,
                true,
                true,
                1.05
            ),
            GuardChordOutput::default()
        );
    }

    #[test]
    fn z_alone_waits_for_ultimate_grace_then_stays_aim_only() {
        let mut tracker = GuardChordTracker::default();

        assert_eq!(
            super::resolve_guard_chord_input(
                &mut tracker,
                false,
                false,
                true,
                false,
                false,
                true,
                2.0
            ),
            GuardChordOutput::default()
        );
        assert_eq!(
            super::resolve_guard_chord_input(
                &mut tracker,
                false,
                false,
                false,
                false,
                false,
                true,
                2.0 + GUARD_CHORD_GRACE
            ),
            GuardChordOutput::default()
        );
        assert_eq!(
            super::resolve_guard_chord_input(
                &mut tracker,
                true,
                false,
                false,
                true,
                false,
                true,
                2.0 + GUARD_CHORD_GRACE + 0.01
            ),
            GuardChordOutput::default()
        );
        assert_eq!(
            super::resolve_guard_chord_input(
                &mut tracker,
                false,
                false,
                false,
                true,
                false,
                true,
                2.0 + GUARD_CHORD_GRACE * 2.0 + 0.01
            ),
            chord_light()
        );
    }

    #[test]
    fn held_z_plus_later_light_heavy_chord_fires_ultimate() {
        let mut tracker = GuardChordTracker::default();

        assert_eq!(
            super::resolve_guard_chord_input(
                &mut tracker,
                false,
                false,
                true,
                false,
                false,
                true,
                6.0
            ),
            GuardChordOutput::default()
        );
        assert_eq!(
            super::resolve_guard_chord_input(
                &mut tracker,
                true,
                true,
                false,
                true,
                true,
                true,
                6.0 + GUARD_CHORD_GRACE * 3.0
            ),
            chord_ultimate()
        );
    }

    #[test]
    fn held_z_plus_staggered_light_heavy_chord_fires_ultimate() {
        let mut tracker = GuardChordTracker::default();

        assert_eq!(
            super::resolve_guard_chord_input(
                &mut tracker,
                false,
                false,
                true,
                false,
                false,
                true,
                7.0
            ),
            GuardChordOutput::default()
        );
        assert_eq!(
            super::resolve_guard_chord_input(
                &mut tracker,
                true,
                false,
                false,
                true,
                false,
                true,
                7.0 + GUARD_CHORD_GRACE * 3.0
            ),
            GuardChordOutput::default()
        );
        assert_eq!(
            super::resolve_guard_chord_input(
                &mut tracker,
                false,
                true,
                false,
                true,
                true,
                true,
                7.0 + GUARD_CHORD_GRACE * 3.5
            ),
            chord_ultimate()
        );
    }

    #[test]
    fn old_guard_does_not_upgrade_to_ultimate_from_late_z() {
        let mut tracker = GuardChordTracker::default();

        assert_eq!(
            resolve_guard_chord_input(&mut tracker, true, false, true, false, 8.0),
            GuardChordOutput::default()
        );
        assert_eq!(
            resolve_guard_chord_input(
                &mut tracker,
                false,
                true,
                true,
                true,
                8.0 + GUARD_CHORD_GRACE * 0.5
            ),
            chord_guard()
        );
        assert_eq!(
            super::resolve_guard_chord_input(
                &mut tracker,
                false,
                false,
                true,
                true,
                true,
                true,
                8.0 + GUARD_CHORD_GRACE * 3.0
            ),
            chord_guard()
        );
    }

    #[test]
    fn guard_chord_accepts_light_then_heavy_within_grace() {
        let mut tracker = GuardChordTracker::default();

        assert_eq!(
            resolve_guard_chord_input(&mut tracker, true, false, true, false, 1.0),
            GuardChordOutput::default()
        );
        assert_eq!(
            resolve_guard_chord_input(
                &mut tracker,
                false,
                true,
                true,
                true,
                1.0 + GUARD_CHORD_GRACE * 0.5
            ),
            chord_guard()
        );
        assert_eq!(
            resolve_guard_chord_input(&mut tracker, false, false, true, true, 1.1),
            chord_guard()
        );
        assert_eq!(
            resolve_guard_chord_input(&mut tracker, false, false, true, false, 1.12),
            GuardChordOutput::default()
        );
    }

    #[test]
    fn guard_chord_accepts_heavy_then_light_within_grace() {
        let mut tracker = GuardChordTracker::default();

        assert_eq!(
            resolve_guard_chord_input(&mut tracker, false, true, false, true, 2.0),
            GuardChordOutput::default()
        );
        assert_eq!(
            resolve_guard_chord_input(
                &mut tracker,
                true,
                false,
                true,
                true,
                2.0 + GUARD_CHORD_GRACE * 0.5
            ),
            chord_guard()
        );
    }

    #[test]
    fn solo_light_waits_for_guard_chord_grace() {
        let mut tracker = GuardChordTracker::default();

        assert_eq!(
            resolve_guard_chord_input(&mut tracker, true, false, true, false, 3.0),
            GuardChordOutput::default()
        );
        assert_eq!(
            resolve_guard_chord_input(
                &mut tracker,
                false,
                false,
                true,
                false,
                3.0 + GUARD_CHORD_GRACE * 0.5
            ),
            GuardChordOutput::default()
        );
        assert_eq!(
            resolve_guard_chord_input(
                &mut tracker,
                false,
                false,
                true,
                false,
                3.0 + GUARD_CHORD_GRACE
            ),
            chord_light()
        );
    }

    #[test]
    fn shift_arrows_are_camera_input_not_player_movement() {
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::ArrowRight);
        assert_eq!(
            player_movement_input(
                &keys,
                0.0,
                PlayerControlBindings::player_one_default(),
                true
            ),
            Vec2::X
        );

        keys.press(KeyCode::ShiftLeft);
        assert_eq!(
            player_movement_input(
                &keys,
                0.0,
                PlayerControlBindings::player_one_default(),
                true
            ),
            Vec2::ZERO
        );
        assert_eq!(
            player_movement_input(
                &keys,
                0.0,
                PlayerControlBindings::player_one_default(),
                false
            ),
            Vec2::X
        );
    }

    #[test]
    fn player_movement_follows_camera_yaw() {
        let mut right = ButtonInput::<KeyCode>::default();
        right.press(KeyCode::ArrowRight);
        assert_vec2_close(
            player_movement_input(
                &right,
                std::f32::consts::FRAC_PI_2,
                PlayerControlBindings::player_one_default(),
                true,
            ),
            Vec2::NEG_Y,
            0.001,
        );

        let mut up = ButtonInput::<KeyCode>::default();
        up.press(KeyCode::ArrowUp);
        assert_vec2_close(
            player_movement_input(
                &up,
                std::f32::consts::FRAC_PI_2,
                PlayerControlBindings::player_one_default(),
                true,
            ),
            Vec2::NEG_X,
            0.001,
        );
    }

    #[test]
    fn player_three_and_four_bindings_produce_movement_and_actions() {
        for bindings in [
            PlayerControlBindings::player_three_default(),
            PlayerControlBindings::player_four_default(),
        ] {
            let mut keys = ButtonInput::<KeyCode>::default();
            keys.press(bindings.right);
            keys.press(bindings.heavy);
            keys.press(bindings.jump);
            let mut dash = DashTapTracker::default();
            let mut guard = GuardChordTracker::default();
            let mut input = FighterInput::default();

            collect_bound_player_input(
                &keys, 1.0, 0.0, bindings, &mut dash, &mut guard, false, &mut input,
            );

            assert_eq!(input.movement, Vec2::X);
            assert!(input.jump);
            assert!(input.raw_heavy_pressed);
            assert!(input.heavy_held);
        }
    }

    #[test]
    fn shift_arrows_do_not_trigger_player_dash() {
        let mut tracker = DashTapTracker::default();
        let mut first = ButtonInput::<KeyCode>::default();
        first.press(KeyCode::ArrowRight);
        assert!(!player_dash_input(
            &first,
            1.0,
            &mut tracker,
            PlayerControlBindings::player_one_default(),
            true,
        ));

        let mut second = ButtonInput::<KeyCode>::default();
        second.press(KeyCode::ArrowRight);
        assert!(player_dash_input(
            &second,
            1.1,
            &mut tracker,
            PlayerControlBindings::player_one_default(),
            true,
        ));

        let mut shifted = ButtonInput::<KeyCode>::default();
        shifted.press(KeyCode::ShiftLeft);
        shifted.press(KeyCode::ArrowRight);
        assert!(!player_dash_input(
            &shifted,
            1.2,
            &mut tracker,
            PlayerControlBindings::player_one_default(),
            true,
        ));

        let mut user_tracker = DashTapTracker::default();
        assert!(!player_dash_input(
            &first,
            1.0,
            &mut user_tracker,
            PlayerControlBindings::player_one_default(),
            false,
        ));
        assert!(player_dash_input(
            &shifted,
            1.1,
            &mut user_tracker,
            PlayerControlBindings::player_one_default(),
            false,
        ));
    }

    #[test]
    fn user_mode_shift_c_is_player_light_not_camera_filter() {
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::ShiftLeft);
        keys.press(KeyCode::KeyC);
        let mut dev_dash = DashTapTracker::default();
        let mut dev_guard = GuardChordTracker::default();
        let mut dev_input = FighterInput::default();

        collect_bound_player_input(
            &keys,
            1.0,
            0.0,
            PlayerControlBindings::player_one_default(),
            &mut dev_dash,
            &mut dev_guard,
            true,
            &mut dev_input,
        );

        assert!(!dev_input.light);
        assert!(!dev_input.light_held);

        let mut user_dash = DashTapTracker::default();
        let mut user_guard = GuardChordTracker::default();
        let mut user_input = FighterInput::default();

        collect_bound_player_input(
            &keys,
            1.0,
            0.0,
            PlayerControlBindings::player_one_default(),
            &mut user_dash,
            &mut user_guard,
            false,
            &mut user_input,
        );

        assert!(user_input.light_held);

        let mut resolved_input = FighterInput::default();
        collect_bound_player_input(
            &keys,
            1.0 + GUARD_CHORD_GRACE,
            0.0,
            PlayerControlBindings::player_one_default(),
            &mut user_dash,
            &mut user_guard,
            false,
            &mut resolved_input,
        );

        assert!(resolved_input.light);
        assert!(resolved_input.light_held);
    }

    #[test]
    fn solo_heavy_waits_for_guard_chord_grace() {
        let mut tracker = GuardChordTracker::default();

        assert_eq!(
            resolve_guard_chord_input(&mut tracker, false, true, false, true, 4.0),
            GuardChordOutput::default()
        );
        assert_eq!(
            resolve_guard_chord_input(
                &mut tracker,
                false,
                false,
                false,
                true,
                4.0 + GUARD_CHORD_GRACE
            ),
            chord_heavy()
        );
    }

    #[test]
    fn counter_trigger_accepts_single_raw_attack_only() {
        let light = FighterInput {
            raw_light_pressed: true,
            light_held: true,
            ..default()
        };
        let heavy = FighterInput {
            raw_heavy_pressed: true,
            heavy_held: true,
            ..default()
        };
        let chord = FighterInput {
            raw_light_pressed: true,
            raw_heavy_pressed: true,
            light_held: true,
            heavy_held: true,
            guard: true,
            ..default()
        };
        let late_chord = FighterInput {
            raw_heavy_pressed: true,
            light_held: true,
            heavy_held: true,
            guard: true,
            ..default()
        };

        assert!(guard_counter_trigger_pressed(&light));
        assert!(guard_counter_trigger_pressed(&heavy));
        assert!(!guard_counter_trigger_pressed(&chord));
        assert!(!guard_counter_trigger_pressed(&late_chord));
    }

    #[test]
    fn collected_input_preserves_raw_attack_for_counter_detection() {
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::KeyC);
        let mut dash = DashTapTracker::default();
        let mut guard = GuardChordTracker::default();
        let mut input = FighterInput::default();

        collect_bound_player_input(
            &keys,
            1.0,
            0.0,
            PlayerControlBindings::player_one_default(),
            &mut dash,
            &mut guard,
            false,
            &mut input,
        );

        assert!(input.raw_light_pressed);
        assert!(guard_counter_trigger_pressed(&input));

        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::KeyC);
        keys.press(KeyCode::KeyX);
        let mut dash = DashTapTracker::default();
        let mut guard = GuardChordTracker::default();
        let mut input = FighterInput::default();

        collect_bound_player_input(
            &keys,
            2.0,
            0.0,
            PlayerControlBindings::player_one_default(),
            &mut dash,
            &mut guard,
            false,
            &mut input,
        );

        assert!(input.raw_light_pressed);
        assert!(input.raw_heavy_pressed);
        assert!(input.guard);
        assert!(!guard_counter_trigger_pressed(&input));
    }

    #[test]
    fn jump_attack_selection_uses_held_attack_buttons_on_jump_frame() {
        let heavy_jump = FighterInput {
            jump: true,
            heavy_held: true,
            ..default()
        };
        let light_jump = FighterInput {
            jump: true,
            light_held: true,
            ..default()
        };
        let guarded_jump = FighterInput {
            jump: true,
            light_held: true,
            heavy_held: true,
            guard: true,
            ..default()
        };

        assert!(jump_heavy_pressed(&heavy_jump));
        assert!(!jump_light_pressed(&heavy_jump));
        assert!(jump_light_pressed(&light_jump));
        assert!(!jump_heavy_pressed(&guarded_jump));
        assert!(!jump_light_pressed(&guarded_jump));
    }

    #[test]
    fn jump_attack_pressed_on_takeoff_is_queued_until_airborne() {
        let catalog = CharacterMoveCatalog::default();
        let loadout = LoadoutContext::for_character(
            CharacterKind::Pig,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let mut action = FighterActionState::default();
        let mut motor = FighterMotor::default();

        start_jump_with_air_attack_queue(&mut motor, &mut action, 1.0, Some(TechniqueButton::B));

        assert_eq!(action.action, FighterAction::Jumping);
        assert_eq!(motor.queued_air_attack, Some(TechniqueButton::B));
        assert!(motor.velocity.y > 0.0);
        assert!(!queued_air_attack_ready(&motor));

        motor.jump_takeoff_timer = JUMP_ATTACK_QUEUE_TAKEOFF_REMAINING;
        assert!(queued_air_attack_ready(&motor));

        start_air_attack_by_button(
            &mut motor,
            &mut action,
            TechniqueButton::B,
            loadout,
            &catalog,
        );

        assert_eq!(action.action, FighterAction::JumpHeavyAttack);
        assert_eq!(action.technique_id, Some(TechniqueId::PigJumpHeavy));
        assert_eq!(motor.queued_air_attack, None);
        assert!(motor.velocity.y < 0.0);
    }

    #[test]
    fn late_second_guard_button_does_not_override_solo_attack() {
        let mut tracker = GuardChordTracker::default();

        assert_eq!(
            resolve_guard_chord_input(&mut tracker, true, false, true, false, 5.0),
            GuardChordOutput::default()
        );
        assert_eq!(
            resolve_guard_chord_input(
                &mut tracker,
                false,
                true,
                true,
                true,
                5.0 + GUARD_CHORD_GRACE + 0.01
            ),
            chord_light()
        );
        assert_eq!(
            resolve_guard_chord_input(
                &mut tracker,
                false,
                false,
                true,
                true,
                5.0 + GUARD_CHORD_GRACE * 2.5
            ),
            chord_heavy()
        );
    }

    #[test]
    fn releasing_first_guard_button_prevents_chord_conversion() {
        let mut tracker = GuardChordTracker::default();

        assert_eq!(
            resolve_guard_chord_input(&mut tracker, true, false, true, false, 6.0),
            GuardChordOutput::default()
        );
        assert_eq!(
            resolve_guard_chord_input(&mut tracker, false, false, false, false, 6.02),
            GuardChordOutput::default()
        );
        assert_eq!(
            resolve_guard_chord_input(&mut tracker, false, true, false, true, 6.04),
            GuardChordOutput::default()
        );
        assert_eq!(
            resolve_guard_chord_input(&mut tracker, false, false, false, true, 6.08),
            chord_light()
        );
    }

    #[test]
    fn defensive_step_never_moves_forward() {
        assert_eq!(defensive_step_direction(Vec2::ZERO, Vec3::Z), -Vec3::Z);
        assert_eq!(
            defensive_step_direction(Vec2::new(0.0, -1.0), Vec3::Z),
            -Vec3::Z
        );
        assert_eq!(
            defensive_step_direction(Vec2::new(0.0, 1.0), Vec3::Z),
            Vec3::X
        );
    }

    #[test]
    fn recovery_roll_uses_requested_direction_or_backstep() {
        assert_eq!(recovery_roll_direction(Vec2::ZERO, Vec3::Z), -Vec3::Z);
        assert_eq!(
            recovery_roll_direction(Vec2::new(1.0, 0.0), Vec3::Z),
            Vec3::X
        );
    }

    #[test]
    fn ledge_grace_allows_one_late_ground_jump() {
        let mut motor = FighterMotor {
            grounded: false,
            ledge_grace_timer: 0.06,
            ..default()
        };
        let mut action = FighterActionState::default();

        assert!(can_start_ground_jump(&motor));
        start_jump(&mut motor, &mut action);
        assert!(!can_start_ground_jump(&motor));
        assert_eq!(action.action, FighterAction::Jumping);
        assert_eq!(motor.velocity.y, JUMP_SPEED);
    }

    #[test]
    fn side_collision_only_cancels_velocity_into_wall() {
        assert!(should_cancel_axis_velocity(-2.0, 0.1));
        assert!(!should_cancel_axis_velocity(2.0, 0.1));
        assert!(!should_cancel_axis_velocity(0.0, 0.1));
    }

    #[test]
    fn bounce_helpers_reflect_wall_and_ground_pressure() {
        let wall =
            wall_bounce_velocity(Vec3::new(-6.0, -1.0, 0.5), Vec3::new(0.2, 0.0, 0.0)).unwrap();
        assert!(wall.x > 0.0);
        assert!(wall.y > 0.0);
        assert!(ground_bounce_velocity(-8.0) > ground_bounce_velocity(-5.0));
    }

    #[test]
    fn body_motion_profiles_are_state_authored() {
        let idle = body_motion_profile(FighterAction::Idle);
        let dash_attack = body_motion_profile(FighterAction::DashAttack);
        let jump_attack = body_motion_profile(FighterAction::JumpAttack);
        let combo_finisher = body_motion_profile(FighterAction::ComboFinisher);
        let hitstun = body_motion_profile(FighterAction::Hitstun);

        assert!(idle.input_scale > dash_attack.input_scale);
        assert!(dash_attack.max_speed_bonus > idle.max_speed_bonus);
        assert!(combo_finisher.max_speed_bonus > idle.max_speed_bonus);
        assert!(combo_finisher.max_speed_bonus > dash_attack.max_speed_bonus);
        assert!(
            combo_finisher.stop_friction
                < body_motion_profile(FighterAction::HeavyAttack).stop_friction
        );
        assert!(jump_attack.fall_gravity_scale > idle.fall_gravity_scale);
        assert_eq!(hitstun.input_scale, 0.0);
        assert_eq!(
            body_motion_profile(FighterAction::Guarding).input_scale,
            0.0
        );
        assert!(idle.stop_friction > dash_attack.stop_friction);
        assert!(jump_attack.terminal_fall_speed > idle.terminal_fall_speed * 0.9);
    }

    #[test]
    fn cat_ultimate_startup_uses_twice_dash_c_motion_profile_without_steering() {
        let body = CharacterBodyDef::default();
        let mut cat_ult = FighterActionState {
            action: FighterAction::UltimateStartup,
            technique_id: Some(TechniqueId::CatUltimateStartup),
            ..default()
        };
        let pig_ult = FighterActionState {
            action: FighterAction::UltimateStartup,
            technique_id: Some(TechniqueId::PigUltimateStartup),
            ..default()
        };

        let motor = FighterMotor::default();
        let cat_profile = character_body_motion_profile_for_state(
            &cat_ult,
            &motor,
            FighterStyleKind::Anchor,
            body,
        );
        let pig_profile = character_body_motion_profile_for_state(
            &pig_ult,
            &motor,
            FighterStyleKind::Anchor,
            body,
        );
        let combo_profile = character_body_motion_profile(
            FighterAction::ComboFinisher,
            FighterStyleKind::Anchor,
            body,
        );
        let generic_ult = character_body_motion_profile(
            FighterAction::UltimateStartup,
            FighterStyleKind::Anchor,
            body,
        );

        assert_eq!(cat_profile.input_scale, 0.0);
        assert_eq!(cat_profile.landing_input_scale, 0.0);
        assert_eq!(
            cat_profile.max_speed_bonus,
            combo_profile.max_speed_bonus * 2.0
        );
        assert_eq!(cat_profile.ground_friction, combo_profile.ground_friction);
        assert_eq!(cat_profile.turn_brake, combo_profile.turn_brake);
        assert_eq!(pig_profile.max_speed_bonus, generic_ult.max_speed_bonus);
        assert_eq!(pig_profile.ground_friction, generic_ult.ground_friction);

        cat_ult.technique_id = Some(TechniqueId::PigUltimateStartup);
        let recategorized_profile = character_body_motion_profile_for_state(
            &cat_ult,
            &motor,
            FighterStyleKind::Anchor,
            body,
        );
        assert_eq!(
            recategorized_profile.max_speed_bonus,
            generic_ult.max_speed_bonus
        );
    }

    #[test]
    fn penguin_slope_moves_use_quicker_fall_landing_profile() {
        let body = CharacterBodyDef::default();
        let dash = FighterActionState {
            action: FighterAction::DashAttack,
            technique_id: Some(TechniqueId::PenguinDashHeavy),
            ..default()
        };
        let ultimate_startup = FighterActionState {
            action: FighterAction::UltimateStartup,
            technique_id: Some(TechniqueId::PenguinUltimateStartup),
            ..default()
        };
        let ultimate_rush = FighterActionState {
            action: FighterAction::UltimateRush,
            technique_id: Some(TechniqueId::PenguinUltimateRush),
            ..default()
        };
        let motor = FighterMotor::default();

        let dash_profile =
            character_body_motion_profile_for_state(&dash, &motor, FighterStyleKind::Anchor, body);
        let base = character_body_motion_profile(
            FighterAction::DashAttack,
            FighterStyleKind::Anchor,
            body,
        );

        for action in [ultimate_startup, ultimate_rush] {
            let profile = character_body_motion_profile_for_state(
                &action,
                &motor,
                FighterStyleKind::Anchor,
                body,
            );
            assert_eq!(profile.input_scale, dash_profile.input_scale);
            assert_eq!(profile.max_speed_bonus, dash_profile.max_speed_bonus);
            assert_eq!(profile.air_friction, dash_profile.air_friction);
            assert_eq!(profile.stop_friction, dash_profile.stop_friction);
            assert_eq!(profile.stop_snap_speed, dash_profile.stop_snap_speed);
        }
        assert!(dash_profile.fall_gravity_scale > base.fall_gravity_scale);
        assert!(dash_profile.air_friction < base.air_friction);
        assert!(dash_profile.stop_friction < base.stop_friction);
    }

    #[test]
    fn held_guard_expires_and_requires_release_after_cooldown() {
        let mut motor = FighterMotor::default();
        let mut action = FighterActionState::default();

        let pressed = tick_guard_input(&mut motor, true, 0.0);
        assert!(can_start_guard(&motor, pressed));
        start_guard(&mut motor, &mut action);
        assert_eq!(action.action, FighterAction::Guarding);
        assert_eq!(motor.guard_start_buffer_timer, 0.0);

        motor.guard_active_timer = GUARD_MAX_DURATION;
        assert!(guard_should_end(&motor, true));
        finish_guard(&mut motor, &mut action);
        assert_eq!(action.action, FighterAction::Idle);
        assert_eq!(motor.guard_cooldown_timer, GUARD_RESTART_COOLDOWN);
        assert_eq!(motor.guard_start_buffer_timer, 0.0);

        let pressed_while_held = tick_guard_input(&mut motor, true, GUARD_RESTART_COOLDOWN);
        assert!(!pressed_while_held);
        assert!(!can_start_guard(&motor, pressed_while_held));

        let released = tick_guard_input(&mut motor, false, 0.0);
        assert!(!released);
        let pressed_again = tick_guard_input(&mut motor, true, 0.0);
        assert!(can_start_guard(&motor, pressed_again));
    }

    #[test]
    fn guard_press_buffers_until_grounded_while_held() {
        let mut motor = FighterMotor {
            grounded: false,
            ..default()
        };
        let pressed = tick_guard_input(&mut motor, true, 0.0);

        assert!(pressed);
        assert!(!can_start_guard(&motor, pressed));
        assert!(motor.guard_start_buffer_timer > 0.0);

        let held = tick_guard_input(&mut motor, true, GUARD_START_BUFFER_SECONDS * 0.5);
        motor.grounded = true;

        assert!(!held);
        assert!(can_start_guard(&motor, held));
    }

    #[test]
    fn releasing_guard_clears_start_buffer() {
        let mut motor = FighterMotor {
            grounded: false,
            ..default()
        };
        let pressed = tick_guard_input(&mut motor, true, 0.0);

        assert!(!can_start_guard(&motor, pressed));
        let released = tick_guard_input(&mut motor, false, GUARD_START_BUFFER_SECONDS * 0.25);
        motor.grounded = true;

        assert!(!released);
        assert_eq!(motor.guard_start_buffer_timer, 0.0);
        assert!(!can_start_guard(&motor, released));
    }

    #[test]
    fn guard_press_during_dash_can_start_after_dash_branch_returns() {
        let mut motor = FighterMotor::default();
        let mut action = FighterActionState {
            action: FighterAction::Dashing,
            ..default()
        };

        let pressed = tick_guard_input(&mut motor, true, 0.0);
        assert!(pressed);
        assert!(can_start_guard(&motor, pressed));

        let held = tick_guard_input(&mut motor, true, GUARD_START_BUFFER_SECONDS * 0.5);
        action.action = FighterAction::Idle;

        assert!(!held);
        assert!(can_start_guard(&motor, held));
        start_guard(&mut motor, &mut action);
        assert_eq!(action.action, FighterAction::Guarding);
    }

    #[test]
    fn guard_shield_visibility_tracks_guard_action() {
        assert!(guard_shield_visible(FighterAction::Guarding));
        assert!(!guard_shield_visible(FighterAction::Idle));
        assert!(!guard_shield_visible(FighterAction::LightAttack1));
    }

    #[test]
    fn guard_counter_window_ticks_and_expires() {
        let mut motor = FighterMotor::default();
        let source = Vec3::new(2.0, 0.0, 0.0);

        motor.open_guard_counter_window(source);
        motor.guard_counter_buffered = true;
        assert_eq!(motor.guard_counter_source, Some(source));
        assert_eq!(motor.guard_counter_window_timer, GUARD_COUNTER_WINDOW);

        tick_guard_counter_window(&mut motor, GUARD_COUNTER_WINDOW * 0.5);
        assert!(motor.guard_counter_window_timer > 0.0);
        assert_eq!(motor.guard_counter_source, Some(source));

        tick_guard_counter_window(&mut motor, GUARD_COUNTER_WINDOW);
        assert_eq!(motor.guard_counter_window_timer, 0.0);
        assert_eq!(motor.guard_counter_source, None);
        assert!(!motor.guard_counter_buffered);
    }

    #[test]
    fn guard_counter_refreshes_existing_window() {
        let mut motor = FighterMotor::default();
        let first = Vec3::new(1.0, 0.0, 0.0);
        let second = Vec3::new(-1.0, 0.0, 0.0);

        motor.open_guard_counter_window(first);
        tick_guard_counter_window(&mut motor, GUARD_COUNTER_WINDOW * 0.5);
        motor.open_guard_counter_window(second);

        assert_eq!(motor.guard_counter_window_timer, GUARD_COUNTER_WINDOW);
        assert_eq!(motor.guard_counter_source, Some(second));
    }

    #[test]
    fn guard_counter_starts_from_light_or_heavy_and_spends_health() {
        let catalog = CharacterMoveCatalog::default();
        let loadout = LoadoutContext::for_character(
            CharacterKind::Cat,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let mut stats = FighterStats {
            health: 20.0,
            ..default()
        };
        let mut motor = FighterMotor::default();
        let mut action = FighterActionState {
            action: FighterAction::Guarding,
            ..default()
        };
        let input = FighterInput {
            raw_light_pressed: true,
            light_held: true,
            ..default()
        };

        motor.open_guard_counter_window(Vec3::new(1.0, 0.0, 0.0));
        assert!(try_start_guard_counter(
            &mut stats,
            &mut motor,
            &mut action,
            &input,
            Vec3::ZERO,
            loadout,
            &catalog,
            guard_counter_trigger_pressed(&input),
        ));

        assert_eq!(action.action, FighterAction::GuardCounter);
        assert_eq!(action.technique_id, Some(TechniqueId::GuardCounter));
        assert_eq!(stats.health, 20.0 - GUARD_COUNTER_HEALTH_COST);
        assert_eq!(motor.guard_counter_window_timer, 0.0);
        assert_eq!(motor.facing, Vec3::X);

        let mut stats = FighterStats {
            health: 20.0,
            ..default()
        };
        let mut motor = FighterMotor::default();
        let mut action = FighterActionState {
            action: FighterAction::Idle,
            ..default()
        };
        let input = FighterInput {
            raw_heavy_pressed: true,
            heavy_held: true,
            ..default()
        };

        motor.open_guard_counter_window(Vec3::new(1.0, 0.0, 0.0));
        assert!(try_start_guard_counter(
            &mut stats,
            &mut motor,
            &mut action,
            &input,
            Vec3::ZERO,
            loadout,
            &catalog,
            guard_counter_trigger_pressed(&input),
        ));
        assert_eq!(action.action, FighterAction::GuardCounter);
    }

    #[test]
    fn guard_counter_rejects_chord_and_low_health() {
        let catalog = CharacterMoveCatalog::default();
        let loadout = LoadoutContext::for_character(
            CharacterKind::Cat,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let chord = FighterInput {
            raw_light_pressed: true,
            raw_heavy_pressed: true,
            light_held: true,
            heavy_held: true,
            guard: true,
            ..default()
        };
        let mut stats = FighterStats {
            health: 20.0,
            ..default()
        };
        let mut motor = FighterMotor::default();
        let mut action = FighterActionState {
            action: FighterAction::Guarding,
            ..default()
        };

        motor.open_guard_counter_window(Vec3::X);
        assert!(!try_start_guard_counter(
            &mut stats,
            &mut motor,
            &mut action,
            &chord,
            Vec3::ZERO,
            loadout,
            &catalog,
            guard_counter_trigger_pressed(&chord),
        ));
        assert_eq!(action.action, FighterAction::Guarding);

        let input = FighterInput {
            raw_light_pressed: true,
            light_held: true,
            ..default()
        };
        stats.health = GUARD_COUNTER_HEALTH_COST;
        assert!(!try_start_guard_counter(
            &mut stats,
            &mut motor,
            &mut action,
            &input,
            Vec3::ZERO,
            loadout,
            &catalog,
            guard_counter_trigger_pressed(&input),
        ));
        assert_eq!(action.action, FighterAction::Guarding);
    }

    #[test]
    fn guard_counter_movement_input_overrides_source_facing() {
        let catalog = CharacterMoveCatalog::default();
        let loadout = LoadoutContext::for_character(
            CharacterKind::Cat,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let mut stats = FighterStats {
            health: 20.0,
            ..default()
        };
        let mut motor = FighterMotor::default();
        let mut action = FighterActionState {
            action: FighterAction::Moving,
            ..default()
        };
        let input = FighterInput {
            raw_light_pressed: true,
            light_held: true,
            movement: Vec2::new(0.0, 1.0),
            ..default()
        };

        motor.open_guard_counter_window(Vec3::X);
        assert!(try_start_guard_counter(
            &mut stats,
            &mut motor,
            &mut action,
            &input,
            Vec3::ZERO,
            loadout,
            &catalog,
            guard_counter_trigger_pressed(&input),
        ));
        assert_eq!(motor.facing, Vec3::Z);
    }

    #[test]
    fn style_body_profiles_keep_movement_identity() {
        let anchor_heavy =
            styled_body_motion_profile(FighterAction::HeavyAttack, FighterStyleKind::Anchor);
        let vector_jump =
            styled_body_motion_profile(FighterAction::Jumping, FighterStyleKind::Vector);
        let base_jump = body_motion_profile(FighterAction::Jumping);

        assert!(
            anchor_heavy.input_scale
                < styled_body_motion_profile(FighterAction::HeavyAttack, FighterStyleKind::Vector)
                    .input_scale
        );
        assert!(vector_jump.landing_input_scale > base_jump.landing_input_scale);
        assert!(vector_jump.takeoff_gravity_scale < base_jump.takeoff_gravity_scale);
    }

    #[test]
    fn character_body_profiles_layer_after_style_motion() {
        let pig = crate::characters::pig_body_profile();
        let base = styled_body_motion_profile(FighterAction::Jumping, FighterStyleKind::Anchor);
        let pig_jump =
            character_body_motion_profile(FighterAction::Jumping, FighterStyleKind::Anchor, pig);
        let pig_idle =
            character_body_motion_profile(FighterAction::Idle, FighterStyleKind::Anchor, pig);

        assert!(pig_jump.fall_gravity_scale > base.fall_gravity_scale);
        assert!(pig_jump.terminal_fall_speed > base.terminal_fall_speed);
        assert!(
            pig_idle.stop_friction
                < styled_body_motion_profile(FighterAction::Idle, FighterStyleKind::Anchor)
                    .stop_friction
        );
        assert!(pig_idle.landing_brake > body_motion_profile(FighterAction::Idle).landing_brake);
    }

    #[test]
    fn hit_reaction_visuals_scale_by_reaction_family() {
        let feel = CombatFeelTuning::from_overrides(crate::feel::CombatFeelFile::default());
        let short = fighter_visual_pose(
            &FighterActionState {
                action: FighterAction::Hitstun,
                elapsed: 0.055,
                reaction_family: Some(ReactionFamilyId::ShortStandingStagger),
                ..default()
            },
            0.0,
            &feel,
        );
        let heavy = fighter_visual_pose(
            &FighterActionState {
                action: FighterAction::Hitstun,
                elapsed: 0.055,
                reaction_family: Some(ReactionFamilyId::HeavyStandingStagger),
                ..default()
            },
            0.0,
            &feel,
        );
        let launch = fighter_visual_pose(
            &FighterActionState {
                action: FighterAction::Hitstun,
                elapsed: 0.055,
                reaction_family: Some(ReactionFamilyId::LauncherDown),
                ..default()
            },
            0.0,
            &feel,
        );

        assert!(heavy.pitch.abs() > short.pitch.abs() * 1.5);
        assert!(heavy.roll.abs() > short.roll.abs() * 2.0);
        assert!(heavy.scale.y < short.scale.y);
        assert!(launch.scale.y > short.scale.y);
        assert!(launch.translation.y > short.translation.y);
    }

    #[test]
    fn hit_reaction_visual_side_flips_yaw_and_roll() {
        let feel = CombatFeelTuning::from_overrides(crate::feel::CombatFeelFile::default());
        let right = fighter_visual_pose(
            &FighterActionState {
                action: FighterAction::Hitstun,
                elapsed: 0.055,
                reaction_family: Some(ReactionFamilyId::HeavyStandingStagger),
                reaction_visual_side: 1.0,
                ..default()
            },
            0.0,
            &feel,
        );
        let left = fighter_visual_pose(
            &FighterActionState {
                action: FighterAction::Hitstun,
                elapsed: 0.055,
                reaction_family: Some(ReactionFamilyId::HeavyStandingStagger),
                reaction_visual_side: -1.0,
                ..default()
            },
            0.0,
            &feel,
        );

        assert!((right.pitch - left.pitch).abs() < 0.001);
        assert!((right.yaw + left.yaw).abs() < 0.001);
        assert!((right.roll + left.roll).abs() < 0.001);
    }

    #[test]
    fn hit_reaction_visual_fallback_uses_short_stagger() {
        let feel = CombatFeelTuning::from_overrides(crate::feel::CombatFeelFile::default());
        let fallback = fighter_visual_pose(
            &FighterActionState {
                action: FighterAction::Hitstun,
                elapsed: 0.055,
                ..default()
            },
            0.0,
            &feel,
        );
        let short = fighter_visual_pose(
            &FighterActionState {
                action: FighterAction::Hitstun,
                elapsed: 0.055,
                reaction_family: Some(ReactionFamilyId::ShortStandingStagger),
                ..default()
            },
            0.0,
            &feel,
        );

        assert_eq!(fallback, short);
    }

    #[test]
    fn reset_action_state_clears_reaction_visual_context() {
        let mut action = FighterActionState {
            action: FighterAction::Hitstun,
            reaction_family: Some(ReactionFamilyId::HeavyStandingStagger),
            reaction_visual_side: -1.0,
            ..default()
        };

        reset_action_state(&mut action, FighterAction::Idle);

        assert_eq!(action.reaction_family, None);
        assert_eq!(action.reaction_visual_side, 1.0);
    }

    #[test]
    fn fighter_visual_poses_distinguish_light_string_and_launcher() {
        let feel = CombatFeelTuning::from_overrides(crate::feel::CombatFeelFile::default());
        let light_left = fighter_visual_pose(
            &FighterActionState {
                action: FighterAction::LightAttack1,
                technique_id: Some(TechniqueId::CatLight1),
                elapsed: 0.08,
                ..default()
            },
            0.0,
            &feel,
        );
        let light_right = fighter_visual_pose(
            &FighterActionState {
                action: FighterAction::LightAttack2,
                technique_id: Some(TechniqueId::CatLight2),
                elapsed: 0.08,
                ..default()
            },
            0.0,
            &feel,
        );
        let pig_light_left = fighter_visual_pose(
            &FighterActionState {
                action: FighterAction::LightAttack1,
                technique_id: Some(TechniqueId::PigLight1),
                elapsed: 0.08,
                ..default()
            },
            0.0,
            &feel,
        );
        let pig_light_right = fighter_visual_pose(
            &FighterActionState {
                action: FighterAction::LightAttack2,
                technique_id: Some(TechniqueId::PigLight2),
                elapsed: 0.08,
                ..default()
            },
            0.0,
            &feel,
        );
        let penguin_snowflake_cast = fighter_visual_pose(
            &FighterActionState {
                action: FighterAction::LightAttack1,
                technique_id: Some(TechniqueId::PenguinLight1),
                elapsed: 0.08,
                ..default()
            },
            0.0,
            &feel,
        );
        let slam = fighter_visual_pose(
            &FighterActionState {
                action: FighterAction::ComboFinisher,
                technique_id: Some(TechniqueId::CatComboFinisher),
                elapsed: 0.24,
                ..default()
            },
            0.0,
            &feel,
        );
        let uppercut = fighter_visual_pose(
            &FighterActionState {
                action: FighterAction::HeavyAttack2,
                technique_id: Some(TechniqueId::CatHeavy2),
                elapsed: 0.9,
                ..default()
            },
            0.0,
            &feel,
        );

        assert!(light_left.yaw < 0.0);
        assert!(light_right.yaw > 0.0);
        assert!(pig_light_left.yaw < 0.0);
        assert!(pig_light_right.yaw > 0.0);
        assert!(light_right.yaw.abs() > 0.85);
        assert!(light_right.roll.abs() > 0.45);
        assert!(pig_light_right.yaw.abs() > 0.85);
        assert!(pig_light_right.roll.abs() > 0.45);
        assert_eq!(penguin_snowflake_cast.yaw, 0.0);
        assert_eq!(penguin_snowflake_cast.roll, 0.0);
        assert_eq!(penguin_snowflake_cast.pitch, 0.0);
        assert_eq!(penguin_snowflake_cast.scale, Vec3::ONE);
        assert!(slam.pitch < 0.0);
        assert!(slam.yaw.abs() > light_right.yaw.abs());
        assert!(slam.roll.abs() > light_right.roll.abs());
        assert!(uppercut.pitch < -0.5);
    }

    #[test]
    fn light_punch_corner_cue_uses_forward_rotated_mesh_corner() {
        let first = light_punch_corner_cue_transform(CharacterKind::Cat, -1.0, 1.0);
        let second = light_punch_corner_cue_transform(CharacterKind::Cat, 1.0, 1.0);
        let pig = light_punch_corner_cue_transform(CharacterKind::Pig, -1.0, 1.0);
        let mesh = light_punch_corner_tint_mesh();

        assert!(first.translation.x > 0.0);
        assert!(second.translation.x < 0.0);
        assert!(first.translation.z > 0.5);
        assert!(pig.translation.z > first.translation.z);
        assert!(first.translation.y > -0.05);
        assert!(first.scale.x > 1.0);
        assert!(second.scale.x < -1.0);
        assert!(first.rotation == Quat::IDENTITY);
        assert_eq!(mesh.count_vertices(), 8);
        assert!(mesh.indices().is_some());
    }

    #[test]
    fn light_punch_corner_cue_only_lives_during_light_punch() {
        let active = light_punch_corner_cue(
            FighterAction::LightAttack1,
            Some(TechniqueId::CatLight1),
            0.08,
        );
        let recovered = light_punch_corner_cue(
            FighterAction::LightAttack1,
            Some(TechniqueId::CatLight1),
            0.32,
        );
        let heavy = light_punch_corner_cue(
            FighterAction::HeavyAttack,
            Some(TechniqueId::CatLight1),
            0.08,
        );
        let penguin_snowflake = light_punch_corner_cue(
            FighterAction::LightAttack1,
            Some(TechniqueId::PenguinLight1),
            0.08,
        );

        assert_eq!(active.map(|(side, _)| side), Some(-1.0));
        assert!(active.unwrap().1 > 0.9);
        assert!(recovered.is_none());
        assert!(heavy.is_none());
        assert!(penguin_snowflake.is_none());
    }

    #[test]
    fn pig_light_punch_corner_tint_is_blue() {
        let cat = light_punch_corner_tint_color(CharacterKind::Cat).to_srgba();
        let pig = light_punch_corner_tint_color(CharacterKind::Pig).to_srgba();

        assert!(cat.red > cat.blue);
        assert!(pig.blue > pig.red);
        assert_eq!(cat.alpha, pig.alpha);
    }

    #[test]
    fn light_punch_corner_tint_updates_when_character_changes() {
        let mut materials = Assets::<StandardMaterial>::default();
        let cat_handle = materials.add(light_punch_corner_tint_material(CharacterKind::Cat));
        let mut mesh_material = MeshMaterial3d(cat_handle.clone());
        let mut cue = FighterLightPunchCornerTint {
            fighter_id: 0,
            character: CharacterKind::Cat,
        };

        sync_light_punch_corner_tint_material(
            &mut cue,
            &mut mesh_material,
            &mut materials,
            CharacterKind::Pig,
        );

        assert_eq!(cue.character, CharacterKind::Pig);
        assert_ne!(mesh_material.0, cat_handle);
        let color = materials
            .get(&mesh_material.0)
            .unwrap()
            .base_color
            .to_srgba();
        assert!(color.blue > color.red);
    }

    #[test]
    fn pig_heavy_followup_uses_pig_launcher_pose() {
        let feel = CombatFeelTuning::from_overrides(crate::feel::CombatFeelFile::default());
        let cat_launcher = fighter_visual_pose(
            &FighterActionState {
                action: FighterAction::HeavyAttack2,
                technique_id: Some(TechniqueId::CatHeavy2),
                elapsed: 0.52,
                ..default()
            },
            0.0,
            &feel,
        );
        let pig_launcher = fighter_visual_pose(
            &FighterActionState {
                action: FighterAction::HeavyAttack2,
                technique_id: Some(TechniqueId::PigHeavy2),
                elapsed: 0.52,
                ..default()
            },
            0.0,
            &feel,
        );

        assert!(pig_launcher.pitch > cat_launcher.pitch + 0.35);
        assert!(pig_launcher.scale.y < action_visual_scale(FighterAction::HeavyAttack2).y);
        assert!(pig_launcher.roll.abs() > cat_launcher.roll.abs());

        let missing_id_pose = fighter_visual_pose(
            &FighterActionState {
                action: FighterAction::HeavyAttack2,
                elapsed: 0.52,
                ..default()
            },
            0.0,
            &feel,
        );
        assert_eq!(missing_id_pose.pitch, 0.0);
        assert_eq!(missing_id_pose.roll, 0.0);
    }

    #[test]
    fn ultimate_visual_poses_have_charge_and_rush_pulses() {
        let feel = CombatFeelTuning::from_overrides(crate::feel::CombatFeelFile::default());
        let startup = fighter_visual_pose(
            &FighterActionState {
                action: FighterAction::UltimateStartup,
                technique_id: Some(TechniqueId::CatUltimateStartup),
                elapsed: 0.32,
                ..default()
            },
            0.0,
            &feel,
        );
        let rush_bomb = fighter_visual_pose(
            &FighterActionState {
                action: FighterAction::UltimateRush,
                technique_id: Some(TechniqueId::CatUltimateRush),
                elapsed: 0.82,
                ..default()
            },
            0.0,
            &feel,
        );

        assert!(startup.scale.y < action_visual_scale(FighterAction::UltimateStartup).y);
        assert!(rush_bomb.pitch < startup.pitch);
        assert!(rush_bomb.scale.z > startup.scale.z);
        assert!(rush_bomb.roll.abs() > startup.roll.abs() * 0.5);
    }

    #[test]
    fn downed_visuals_are_belly_up_then_rotate_to_standing() {
        let feel = CombatFeelTuning::from_overrides(crate::feel::CombatFeelFile::default());
        let down = fighter_visual_pose(
            &FighterActionState {
                action: FighterAction::Knockdown,
                ..default()
            },
            0.0,
            &feel,
        );
        let rising = fighter_visual_pose(
            &FighterActionState {
                action: FighterAction::GetUp,
                elapsed: 0.35,
                reaction_recover_ms: Some(700),
                ..default()
            },
            0.0,
            &feel,
        );
        let stood = fighter_visual_pose(
            &FighterActionState {
                action: FighterAction::GetUp,
                elapsed: 0.7,
                reaction_recover_ms: Some(700),
                ..default()
            },
            0.0,
            &feel,
        );

        assert!((down.pitch - KNOCKDOWN_HEAD_LOW_PITCH).abs() < 0.001);
        assert!(rising.pitch < down.pitch);
        assert!(stood.pitch.abs() < 0.001);
    }

    #[test]
    fn knockdown_pose_flips_visible_root_without_pitching_gameplay_root() {
        let feel = CombatFeelTuning::from_overrides(crate::feel::CombatFeelFile::default());
        let action = FighterActionState {
            action: FighterAction::Knockdown,
            ..default()
        };
        let gameplay_rotation = fighter_facing_rotation(Vec3::X, Quat::IDENTITY);
        let pose_transform = fighter_pose_root_transform(&action, 0.0, 0.0, &feel);

        assert_vec3_close(gameplay_rotation.mul_vec3(Vec3::Z), Vec3::X, 0.001);
        assert_vec3_close(
            pose_transform.translation,
            fighter_pose_root_translation(),
            0.001,
        );
        assert!(pose_transform.rotation.mul_vec3(Vec3::Y).y < -0.8);
    }

    #[test]
    fn ringout_pose_reuses_knockdown_flip_for_death_visuals() {
        let feel = CombatFeelTuning::from_overrides(crate::feel::CombatFeelFile::default());
        let action = FighterActionState {
            action: FighterAction::RingOut,
            ..default()
        };
        let pose_transform = fighter_pose_root_transform(&action, 0.0, 0.0, &feel);

        assert_vec3_close(
            pose_transform.translation,
            fighter_pose_root_translation(),
            0.001,
        );
        assert!(pose_transform.rotation.mul_vec3(Vec3::Y).y < -0.8);
    }

    #[test]
    fn pose_root_offsets_model_children_but_not_overhead_marker() {
        let body_local = pose_local_translation(Vec3::new(0.0, FIGHTER_BODY_Y, 0.0));
        let scene_transform = fighter_scene_model_transform();
        let marker_transform = fighter_marker_transform();

        assert_vec3_close(body_local, Vec3::ZERO, 0.001);
        assert!(scene_transform.translation.y < 0.0);
        assert!(marker_transform.translation.y > FIGHTER_BODY_Y);
    }

    #[test]
    fn held_dash_updates_direction_speed_and_trail_timer() {
        let mut motor = FighterMotor::default();
        let direction = apply_dash_hold_motion(&mut motor, Vec2::X, 1.0, 0.1).unwrap();

        assert_eq!(direction, Vec3::X);
        assert_eq!(motor.facing, Vec3::X);
        assert!(motor.velocity.x > DASH_IMPULSE * 0.3);
        assert_eq!(motor.velocity.z, 0.0);

        motor.dash_trail_timer = DASH_TRAIL_REPEAT;
        assert!(!dash_trail_tick(&mut motor, DASH_TRAIL_REPEAT * 0.5));
        assert!(dash_trail_tick(&mut motor, DASH_TRAIL_REPEAT * 0.6));
    }

    #[test]
    fn held_dash_only_stops_after_arrow_input_is_released() {
        assert!(!dash_should_stop(DASH_DURATION * 4.0, Vec2::X));
        assert!(!dash_should_stop(DASH_DURATION * 0.5, Vec2::ZERO));
        assert!(dash_should_stop(DASH_DURATION, Vec2::ZERO));
    }

    #[test]
    fn dash_release_starts_inertial_slide_when_moving_fast_enough() {
        let mut motor = FighterMotor {
            velocity: Vec3::new(5.0, 0.0, 0.0),
            ..default()
        };
        start_dash_slide(&mut motor);
        assert_eq!(motor.dash_slide_timer, DASH_SLIDE_DURATION);

        motor.velocity = Vec3::new(DASH_SLIDE_STOP_SPEED * 0.5, 0.0, 0.0);
        start_dash_slide(&mut motor);
        assert_eq!(motor.dash_slide_timer, 0.0);
    }

    #[test]
    fn penguin_hard_ice_slide_locks_double_entry_velocity_until_exit() {
        let mut motor = FighterMotor {
            velocity: Vec3::new(3.0, 0.0, 0.0),
            facing: Vec3::Z,
            ..default()
        };
        let mut desired = Vec3::Z;

        let direction = update_penguin_hard_ice_slide_state(
            &mut motor,
            &mut desired,
            FighterAction::Moving,
            true,
        )
        .unwrap();

        assert_vec3_close(direction, Vec3::X, 0.001);
        assert_eq!(desired, Vec3::ZERO);
        assert_eq!(motor.penguin_ice_slide_speed, 6.0);
        force_penguin_hard_ice_slide_velocity(&mut motor, direction);
        assert_eq!(motor.velocity.x, 6.0);
        assert_eq!(motor.velocity.z, 0.0);

        motor.velocity = Vec3::new(0.4, 0.0, 0.0);
        let mut changed_input = Vec3::Z;
        let locked_direction = update_penguin_hard_ice_slide_state(
            &mut motor,
            &mut changed_input,
            FighterAction::JumpAttack,
            true,
        )
        .unwrap();
        assert_vec3_close(locked_direction, Vec3::X, 0.001);
        assert_eq!(changed_input, Vec3::ZERO);
        force_penguin_hard_ice_slide_velocity(&mut motor, locked_direction);
        assert_eq!(motor.velocity.x, 6.0);

        let mut exit_input = Vec3::Z;
        assert!(
            update_penguin_hard_ice_slide_state(
                &mut motor,
                &mut exit_input,
                FighterAction::JumpAttack,
                false,
            )
            .is_none()
        );
        assert!(motor.penguin_ice_slide_direction.is_none());
        assert_eq!(motor.penguin_ice_slide_speed, 0.0);
    }

    #[test]
    fn penguin_hard_ice_slide_uses_input_or_facing_when_entering_slowly() {
        let mut motor = FighterMotor {
            velocity: Vec3::ZERO,
            facing: Vec3::NEG_X,
            ..default()
        };
        let mut desired = Vec3::Z;

        let input_direction = update_penguin_hard_ice_slide_state(
            &mut motor,
            &mut desired,
            FighterAction::Moving,
            true,
        )
        .unwrap();
        assert_vec3_close(input_direction, Vec3::Z, 0.001);
        assert_eq!(
            motor.penguin_ice_slide_speed,
            PENGUIN_HARD_ICE_SLIDE_MIN_SPEED
        );

        clear_penguin_hard_ice_slide_state(&mut motor);
        let mut no_input = Vec3::ZERO;
        let facing_direction = update_penguin_hard_ice_slide_state(
            &mut motor,
            &mut no_input,
            FighterAction::Moving,
            true,
        )
        .unwrap();
        assert_vec3_close(facing_direction, Vec3::NEG_X, 0.001);

        let mut disabled_input = Vec3::Z;
        assert!(
            update_penguin_hard_ice_slide_state(
                &mut motor,
                &mut disabled_input,
                FighterAction::Hitstun,
                true,
            )
            .is_none()
        );
        assert!(motor.penguin_ice_slide_direction.is_none());
        assert_eq!(motor.penguin_ice_slide_speed, 0.0);
    }

    #[test]
    fn penguin_hard_ice_slide_doubles_dash_entry_speed() {
        let mut motor = FighterMotor {
            velocity: Vec3::new(8.0, 0.0, 0.0),
            facing: Vec3::Z,
            ..default()
        };
        let mut desired = Vec3::ZERO;

        let direction = update_penguin_hard_ice_slide_state(
            &mut motor,
            &mut desired,
            FighterAction::Dashing,
            true,
        )
        .unwrap();

        assert_vec3_close(direction, Vec3::X, 0.001);
        assert_eq!(motor.penguin_ice_slide_speed, 16.0);
        force_penguin_hard_ice_slide_velocity(&mut motor, direction);
        assert_eq!(motor.velocity.x, 16.0);
        assert_eq!(motor.velocity.z, 0.0);
    }

    #[test]
    fn dash_jump_carries_forward_speed_with_authored_caps() {
        let mut action = FighterActionState {
            action: FighterAction::Dashing,
            ..default()
        };
        let mut motor = FighterMotor {
            facing: Vec3::X,
            velocity: Vec3::new(20.0, 0.0, 0.0),
            ..default()
        };

        start_dash_jump(&mut motor, &mut action);

        assert_eq!(action.action, FighterAction::Jumping);
        assert_eq!(motor.velocity.y, JUMP_SPEED);
        assert!((motor.velocity.x - DASH_JUMP_MAX_FORWARD_SPEED).abs() < 0.001);
        assert_eq!(DASH_JUMP_MAX_FORWARD_SPEED, DASH_HOLD_SPEED);
        assert_eq!(motor.dash_jump_carry_timer, DASH_JUMP_CARRY_DURATION);
        assert_eq!(
            motor.dash_jump_carry_speed_limit,
            DASH_JUMP_MAX_FORWARD_SPEED
        );

        motor.velocity = Vec3::ZERO;
        motor.grounded = true;
        action.action = FighterAction::Dashing;
        start_dash_jump(&mut motor, &mut action);
        assert!((motor.velocity.x - DASH_JUMP_MIN_FORWARD_SPEED).abs() < 0.001);
        assert_eq!(DASH_JUMP_MIN_FORWARD_SPEED, DASH_HOLD_SPEED);
    }

    #[test]
    fn pig_dash_jump_carry_scales_with_body_dash_impulse() {
        let pig = crate::characters::pig_body_profile();
        let mut action = FighterActionState {
            action: FighterAction::Dashing,
            ..default()
        };
        let mut motor = FighterMotor {
            facing: Vec3::X,
            velocity: Vec3::new(20.0, 0.0, 0.0),
            ..default()
        };

        start_dash_jump_with_scale(&mut motor, &mut action, pig.jump_impulse, pig.dash_impulse);

        assert_eq!(action.action, FighterAction::Jumping);
        assert_eq!(motor.velocity.y, JUMP_SPEED * pig.jump_impulse);
        assert!((motor.velocity.x - DASH_JUMP_MAX_FORWARD_SPEED * pig.dash_impulse).abs() < 0.001);
        assert!(
            (motor.dash_jump_carry_speed_limit - DASH_JUMP_MAX_FORWARD_SPEED * pig.dash_impulse)
                .abs()
                < 0.001
        );
    }

    #[test]
    fn slide_cancel_damps_momentum_for_standing_actions() {
        let mut motor = FighterMotor {
            velocity: Vec3::new(8.0, 0.0, 0.0),
            dash_slide_timer: 0.12,
            ..default()
        };

        cancel_dash_slide_for_action(&mut motor);

        assert_eq!(motor.dash_slide_timer, 0.0);
        assert!(motor.velocity.x < 2.0);
    }

    #[test]
    fn dash_jump_carry_extends_airborne_speed_limit_temporarily() {
        let profile = body_motion_profile(FighterAction::Jumping);
        let mut motor = FighterMotor {
            grounded: false,
            dash_jump_carry_timer: 0.12,
            dash_jump_carry_speed_limit: DASH_JUMP_MAX_FORWARD_SPEED,
            ..default()
        };
        let carry_limit = planar_speed_limit(&motor, false, 1.0, profile);
        motor.dash_jump_carry_timer = 0.0;
        let normal_limit = planar_speed_limit(&motor, false, 1.0, profile);

        assert_eq!(carry_limit, DASH_JUMP_MAX_FORWARD_SPEED);
        assert!(carry_limit > normal_limit);
    }

    #[test]
    fn jump_attack_starts_locked_diagonal_dive() {
        let catalog = CharacterMoveCatalog::default();
        let loadout = LoadoutContext::from_style(FighterStyleKind::Anchor);
        let mut action = FighterActionState {
            action: FighterAction::Jumping,
            ..default()
        };
        let mut motor = FighterMotor {
            grounded: false,
            facing: Vec3::X,
            velocity: Vec3::new(1.0, 2.0, 0.0),
            dash_jump_carry_timer: 0.2,
            ..default()
        };

        start_jump_attack(&mut motor, &mut action, loadout, &catalog);

        assert_eq!(action.action, FighterAction::JumpAttack);
        assert!(motor.air_attack_used);
        assert!(motor.jump_attack_landing_recovery);
        assert_eq!(motor.dash_jump_carry_timer, 0.0);
        assert_eq!(motor.velocity.x, JUMP_ATTACK_DIVE_FORWARD_SPEED);
        assert_eq!(motor.velocity.z, 0.0);
        assert_eq!(motor.velocity.y, -JUMP_ATTACK_DIVE_DOWN_SPEED);
        assert_eq!(
            body_motion_profile(FighterAction::JumpAttack).input_scale,
            0.0
        );
    }

    #[test]
    fn bee_jump_attack_flies_forward_and_slightly_upward() {
        let catalog = CharacterMoveCatalog::default();
        let loadout = LoadoutContext::for_character(
            CharacterKind::Bee,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let mut action = FighterActionState {
            action: FighterAction::Jumping,
            ..default()
        };
        let mut motor = FighterMotor {
            grounded: false,
            facing: Vec3::Z,
            velocity: Vec3::new(0.0, -3.0, 1.0),
            dash_jump_carry_timer: 0.2,
            jump_attack_landing_recovery: true,
            ..default()
        };

        start_jump_attack(&mut motor, &mut action, loadout, &catalog);

        assert_eq!(action.action, FighterAction::JumpAttack);
        assert_eq!(action.technique_id, Some(TechniqueId::BeeJumpAttack));
        assert!(motor.air_attack_used);
        assert!(!motor.jump_attack_landing_recovery);
        assert!(motor.bee_air_dash_motion_active);
        assert!(motor.bee_air_dash_shot_available);
        assert_eq!(motor.dash_jump_carry_timer, 0.0);
        assert_eq!(motor.velocity.x, 0.0);
        assert_eq!(motor.velocity.z, BEE_JUMP_ATTACK_FORWARD_SPEED);
        assert_eq!(motor.velocity.y, BEE_JUMP_ATTACK_UP_SPEED);
    }

    #[test]
    fn penguin_jump_attack_shoots_snowflake_without_forcing_dive() {
        let catalog = CharacterMoveCatalog::default();
        let loadout = LoadoutContext::for_character(
            CharacterKind::Penguin,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let mut action = FighterActionState {
            action: FighterAction::Jumping,
            ..default()
        };
        let mut motor = FighterMotor {
            grounded: false,
            facing: Vec3::Z,
            velocity: Vec3::new(1.0, -3.0, 0.5),
            dash_jump_carry_timer: 0.2,
            jump_attack_landing_recovery: true,
            bee_air_dash_motion_active: true,
            ..default()
        };

        start_jump_attack(&mut motor, &mut action, loadout, &catalog);

        assert_eq!(action.action, FighterAction::JumpAttack);
        assert_eq!(action.technique_id, Some(TechniqueId::PenguinJumpAttack));
        assert!(!motor.air_attack_used);
        assert!(!motor.jump_attack_landing_recovery);
        assert!(!motor.bee_air_dash_motion_active);
        assert_eq!(motor.dash_jump_carry_timer, 0.0);
        assert_eq!(motor.velocity.x, 1.0);
        assert_eq!(motor.velocity.z, 0.5);
        assert_eq!(motor.velocity.y, PENGUIN_JUMP_SNOWFLAKE_MIN_FALL_SPEED);
    }

    #[test]
    fn chick_jump_c_starts_updraft_glide_and_preserves_rising_speed() {
        let catalog = CharacterMoveCatalog::default();
        let loadout = LoadoutContext::for_character(
            CharacterKind::Chick,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let starting_up_speed = CHICK_JUMP_C_MIN_UP_SPEED + 1.6;
        let mut action = FighterActionState {
            action: FighterAction::Jumping,
            ..default()
        };
        let mut motor = FighterMotor {
            grounded: false,
            facing: Vec3::Z,
            velocity: Vec3::new(2.0, starting_up_speed, 0.5),
            dash_jump_carry_timer: 0.2,
            dash_jump_carry_speed_limit: 6.0,
            jump_attack_landing_recovery: true,
            bee_air_dash_motion_active: true,
            ..default()
        };

        start_jump_attack(&mut motor, &mut action, loadout, &catalog);

        assert_eq!(action.action, FighterAction::JumpAttack);
        assert_eq!(action.technique_id, Some(TechniqueId::ChickJumpAttack));
        assert!(!motor.grounded);
        assert!(motor.air_attack_used);
        assert!(!motor.jump_attack_landing_recovery);
        assert!(!motor.bee_air_dash_motion_active);
        assert_eq!(motor.dash_jump_carry_timer, 0.0);
        assert_eq!(motor.dash_jump_carry_speed_limit, 0.0);
        assert_eq!(motor.velocity.x, 0.0);
        assert_eq!(motor.velocity.z, CHICK_JUMP_C_FORWARD_SPEED);
        assert_eq!(motor.velocity.y, starting_up_speed);
        assert!(motor.velocity.y > CHICK_JUMP_C_MIN_UP_SPEED);
    }

    #[test]
    fn chick_jump_c_from_fall_raises_to_hover_climb_speed() {
        let catalog = CharacterMoveCatalog::default();
        let loadout = LoadoutContext::for_character(
            CharacterKind::Chick,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let mut action = FighterActionState {
            action: FighterAction::Jumping,
            ..default()
        };
        let mut motor = FighterMotor {
            grounded: false,
            facing: Vec3::X,
            velocity: Vec3::new(0.5, -3.0, 2.0),
            dash_jump_carry_timer: 0.2,
            dash_jump_carry_speed_limit: 6.0,
            jump_attack_landing_recovery: true,
            ..default()
        };

        start_jump_attack(&mut motor, &mut action, loadout, &catalog);

        assert_eq!(action.action, FighterAction::JumpAttack);
        assert_eq!(action.technique_id, Some(TechniqueId::ChickJumpAttack));
        assert!(motor.air_attack_used);
        assert!(!motor.jump_attack_landing_recovery);
        assert_eq!(motor.dash_jump_carry_timer, 0.0);
        assert_eq!(motor.dash_jump_carry_speed_limit, 0.0);
        assert_eq!(motor.velocity.x, CHICK_JUMP_C_FORWARD_SPEED);
        assert_eq!(motor.velocity.z, 0.0);
        assert_eq!(motor.velocity.y, CHICK_JUMP_C_MIN_UP_SPEED);
    }

    #[test]
    fn penguin_jump_heavy_prepares_snowflake_teleport_without_dive() {
        let catalog = CharacterMoveCatalog::default();
        let loadout = LoadoutContext::for_character(
            CharacterKind::Penguin,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let mut action = FighterActionState {
            action: FighterAction::Jumping,
            ..default()
        };
        let mut motor = FighterMotor {
            grounded: false,
            facing: Vec3::Z,
            velocity: Vec3::new(1.0, -3.0, 0.5),
            dash_jump_carry_timer: 0.2,
            jump_attack_landing_recovery: true,
            bee_air_dash_motion_active: true,
            ..default()
        };

        start_jump_heavy_attack(&mut motor, &mut action, loadout, &catalog);

        assert_eq!(action.action, FighterAction::JumpHeavyAttack);
        assert_eq!(action.technique_id, Some(TechniqueId::PenguinJumpHeavy));
        assert!(motor.air_attack_used);
        assert!(!motor.jump_attack_landing_recovery);
        assert!(!motor.bee_air_dash_motion_active);
        assert_eq!(motor.dash_jump_carry_timer, 0.0);
        assert_eq!(motor.velocity.x, 1.0);
        assert_eq!(motor.velocity.z, 0.5);
        assert_eq!(motor.velocity.y, PENGUIN_JUMP_SNOWFLAKE_MIN_FALL_SPEED);
    }

    #[test]
    fn bee_jump_attack_motion_profile_preserves_air_dash_speed() {
        let action = FighterActionState {
            action: FighterAction::JumpAttack,
            technique_id: Some(TechniqueId::BeeJumpAttack),
            ..default()
        };
        let body = crate::characters::bee_body_profile();
        let mut motor = FighterMotor {
            grounded: false,
            bee_air_dash_motion_active: true,
            velocity: Vec3::new(BEE_JUMP_ATTACK_FORWARD_SPEED, BEE_JUMP_ATTACK_UP_SPEED, 0.0),
            ..default()
        };
        let profile = character_body_motion_profile_for_state(
            &action,
            &motor,
            FighterStyleKind::Anchor,
            body,
        );
        let speed_limit = planar_speed_limit(&motor, false, body.air_speed, profile);
        let before_speed = planar_velocity(&motor).length();
        let damp = (1.0 - profile.stop_friction * 0.016).clamp(0.0, 1.0);
        motor.velocity.x *= damp;

        assert!(speed_limit > BEE_JUMP_ATTACK_FORWARD_SPEED);
        assert!(
            profile.stop_friction < body_motion_profile(FighterAction::JumpAttack).stop_friction
        );
        assert!(
            profile.gravity_scale < body_motion_profile(FighterAction::JumpAttack).gravity_scale
        );
        assert!(before_speed - planar_velocity(&motor).length() < 0.03);
    }

    #[test]
    fn chick_jump_x_starts_fresh_egg_ride_flight() {
        let catalog = CharacterMoveCatalog::default();
        let loadout = LoadoutContext::for_character(
            CharacterKind::Chick,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let mut action = FighterActionState {
            action: FighterAction::Jumping,
            ..default()
        };
        let mut motor = FighterMotor {
            grounded: false,
            facing: Vec3::X,
            velocity: Vec3::new(1.0, -4.0, 2.0),
            dash_jump_carry_timer: 0.2,
            dash_jump_carry_speed_limit: 6.0,
            jump_attack_landing_recovery: true,
            bee_air_dash_motion_active: true,
            ..default()
        };

        start_jump_heavy_attack(&mut motor, &mut action, loadout, &catalog);

        assert_eq!(action.action, FighterAction::JumpHeavyAttack);
        assert_eq!(action.technique_id, Some(TechniqueId::ChickJumpHeavy));
        assert!(!motor.grounded);
        assert!(motor.air_attack_used);
        assert!(!motor.jump_attack_landing_recovery);
        assert!(!motor.bee_air_dash_motion_active);
        assert_eq!(motor.dash_jump_carry_timer, 0.0);
        assert_eq!(motor.dash_jump_carry_speed_limit, 0.0);
        assert_eq!(motor.velocity.x, CHICK_FRESH_EGG_RIDE_FORWARD_SPEED);
        assert_eq!(motor.velocity.z, 0.0);
        assert_eq!(motor.velocity.y, CHICK_FRESH_EGG_RIDE_LIFT_SPEED);
    }

    #[test]
    fn chick_jump_x_can_cancel_to_jump_c_climb() {
        let catalog = CharacterMoveCatalog::default();
        let loadout = LoadoutContext::for_character(
            CharacterKind::Chick,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let mut action = FighterActionState {
            action: FighterAction::JumpHeavyAttack,
            technique_id: Some(TechniqueId::ChickJumpHeavy),
            ..default()
        };
        let mut motor = FighterMotor {
            grounded: false,
            facing: Vec3::Z,
            velocity: Vec3::new(
                0.0,
                CHICK_FRESH_EGG_RIDE_LIFT_SPEED,
                CHICK_FRESH_EGG_RIDE_FORWARD_SPEED,
            ),
            air_attack_used: true,
            ..default()
        };
        let input = FighterInput {
            raw_light_pressed: true,
            heavy_held: true,
            ..default()
        };

        assert!(try_start_chick_air_attack_cancel(
            &mut motor,
            &mut action,
            &input,
            loadout,
            &catalog,
        ));

        assert_eq!(action.action, FighterAction::JumpAttack);
        assert_eq!(action.technique_id, Some(TechniqueId::ChickJumpAttack));
        assert!(motor.air_attack_used);
        assert!(!motor.jump_attack_landing_recovery);
        assert_eq!(motor.velocity.x, 0.0);
        assert_eq!(motor.velocity.z, CHICK_JUMP_C_FORWARD_SPEED);
        assert_eq!(motor.velocity.y, CHICK_JUMP_C_MIN_UP_SPEED);
        assert!(motor.velocity.y > CHICK_FRESH_EGG_RIDE_LIFT_SPEED);
    }

    #[test]
    fn chick_jump_c_can_cancel_to_jump_x_ride() {
        let catalog = CharacterMoveCatalog::default();
        let loadout = LoadoutContext::for_character(
            CharacterKind::Chick,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let mut action = FighterActionState {
            action: FighterAction::JumpAttack,
            technique_id: Some(TechniqueId::ChickJumpAttack),
            ..default()
        };
        let mut motor = FighterMotor {
            grounded: false,
            facing: Vec3::X,
            velocity: Vec3::new(CHICK_JUMP_C_FORWARD_SPEED, CHICK_JUMP_C_MIN_UP_SPEED, 0.0),
            air_attack_used: true,
            ..default()
        };
        let input = FighterInput {
            raw_heavy_pressed: true,
            light_held: true,
            ..default()
        };

        assert!(try_start_chick_air_attack_cancel(
            &mut motor,
            &mut action,
            &input,
            loadout,
            &catalog,
        ));

        assert_eq!(action.action, FighterAction::JumpHeavyAttack);
        assert_eq!(action.technique_id, Some(TechniqueId::ChickJumpHeavy));
        assert!(motor.air_attack_used);
        assert!(!motor.jump_attack_landing_recovery);
        assert_eq!(motor.velocity.x, CHICK_FRESH_EGG_RIDE_FORWARD_SPEED);
        assert_eq!(motor.velocity.z, 0.0);
        assert_eq!(motor.velocity.y, CHICK_FRESH_EGG_RIDE_LIFT_SPEED);
    }

    #[test]
    fn chick_air_attack_cancel_requires_raw_opposite_press() {
        let catalog = CharacterMoveCatalog::default();
        let loadout = LoadoutContext::for_character(
            CharacterKind::Chick,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let mut jump_c_action = FighterActionState {
            action: FighterAction::JumpAttack,
            technique_id: Some(TechniqueId::ChickJumpAttack),
            ..default()
        };
        let mut jump_c_motor = FighterMotor {
            grounded: false,
            air_attack_used: true,
            ..default()
        };
        let held_heavy = FighterInput {
            heavy_held: true,
            ..default()
        };

        assert!(!try_start_chick_air_attack_cancel(
            &mut jump_c_motor,
            &mut jump_c_action,
            &held_heavy,
            loadout,
            &catalog,
        ));
        assert_eq!(
            jump_c_action.technique_id,
            Some(TechniqueId::ChickJumpAttack)
        );

        let mut jump_x_action = FighterActionState {
            action: FighterAction::JumpHeavyAttack,
            technique_id: Some(TechniqueId::ChickJumpHeavy),
            ..default()
        };
        let mut jump_x_motor = FighterMotor {
            grounded: false,
            air_attack_used: true,
            ..default()
        };
        let held_light = FighterInput {
            light_held: true,
            ..default()
        };

        assert!(!try_start_chick_air_attack_cancel(
            &mut jump_x_motor,
            &mut jump_x_action,
            &held_light,
            loadout,
            &catalog,
        ));
        assert_eq!(
            jump_x_action.technique_id,
            Some(TechniqueId::ChickJumpHeavy)
        );
    }

    #[test]
    fn chick_jump_c_motion_profile_hover_glides_and_recovers_airborne() {
        let action = FighterActionState {
            action: FighterAction::JumpAttack,
            technique_id: Some(TechniqueId::ChickJumpAttack),
            ..default()
        };
        let body = crate::characters::chick_body_profile();
        let mut motor = FighterMotor {
            grounded: false,
            velocity: Vec3::new(CHICK_JUMP_C_FORWARD_SPEED, CHICK_JUMP_C_MIN_UP_SPEED, 0.0),
            ..default()
        };
        let profile = character_body_motion_profile_for_state(
            &action,
            &motor,
            FighterStyleKind::Anchor,
            body,
        );
        let base_profile = character_body_motion_profile(
            FighterAction::JumpAttack,
            FighterStyleKind::Anchor,
            body,
        );
        let speed_limit = planar_speed_limit(&motor, false, body.air_speed, profile);
        let before_speed = planar_velocity(&motor).length();
        let damp = (1.0 - profile.stop_friction * 0.016).clamp(0.0, 1.0);
        motor.velocity.x *= damp;

        assert!(speed_limit > CHICK_JUMP_C_FORWARD_SPEED);
        assert_eq!(profile.input_scale, 0.0);
        assert!(profile.air_friction < base_profile.air_friction);
        assert!(profile.stop_friction < base_profile.stop_friction);
        assert!(profile.gravity_scale < base_profile.gravity_scale);
        assert!(profile.fall_gravity_scale < base_profile.fall_gravity_scale);
        assert!(before_speed - planar_velocity(&motor).length() < 0.02);
        assert!(should_return_to_jumping_on_air_attack_completion(&action));
    }

    #[test]
    fn chick_jump_x_motion_profile_preserves_fresh_egg_ride_flight() {
        let action = FighterActionState {
            action: FighterAction::JumpHeavyAttack,
            technique_id: Some(TechniqueId::ChickJumpHeavy),
            ..default()
        };
        let body = crate::characters::chick_body_profile();
        let mut motor = FighterMotor {
            grounded: false,
            velocity: Vec3::new(
                CHICK_FRESH_EGG_RIDE_FORWARD_SPEED,
                CHICK_FRESH_EGG_RIDE_LIFT_SPEED,
                0.0,
            ),
            ..default()
        };
        let profile = character_body_motion_profile_for_state(
            &action,
            &motor,
            FighterStyleKind::Anchor,
            body,
        );
        let base_profile = character_body_motion_profile(
            FighterAction::JumpHeavyAttack,
            FighterStyleKind::Anchor,
            body,
        );
        let speed_limit = planar_speed_limit(&motor, false, body.air_speed, profile);
        let before_speed = planar_velocity(&motor).length();
        let damp = (1.0 - profile.stop_friction * 0.016).clamp(0.0, 1.0);
        motor.velocity.x *= damp;

        assert!(speed_limit > CHICK_FRESH_EGG_RIDE_FORWARD_SPEED);
        assert_eq!(profile.input_scale, 0.0);
        assert!(profile.stop_friction < base_profile.stop_friction);
        assert!(profile.gravity_scale < base_profile.gravity_scale);
        assert!(profile.fall_gravity_scale < base_profile.fall_gravity_scale);
        assert!(before_speed - planar_velocity(&motor).length() < 0.02);
    }

    #[test]
    fn bee_jump_attack_can_rotate_facing_without_changing_trajectory() {
        let mut action = FighterActionState {
            action: FighterAction::JumpAttack,
            technique_id: Some(TechniqueId::BeeJumpAttack),
            ..default()
        };
        let mut motor = FighterMotor {
            grounded: false,
            facing: Vec3::Z,
            velocity: Vec3::new(0.0, BEE_JUMP_ATTACK_UP_SPEED, BEE_JUMP_ATTACK_FORWARD_SPEED),
            ..default()
        };
        let velocity_before = motor.velocity;

        update_bee_air_dash_facing(&mut motor, &action, Vec2::X);

        assert_eq!(motor.facing, Vec3::X);
        assert_eq!(motor.velocity, velocity_before);

        action.technique_id = Some(TechniqueId::BeeJumpHeavy);
        update_bee_air_dash_facing(&mut motor, &action, -Vec2::X);

        assert_eq!(motor.facing, Vec3::X);
        assert_eq!(motor.velocity, velocity_before);
    }

    #[test]
    fn bee_jump_attack_can_shoot_rotated_jump_x_without_stopping() {
        let catalog = CharacterMoveCatalog::default();
        let loadout = LoadoutContext::for_character(
            CharacterKind::Bee,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let mut action = FighterActionState {
            action: FighterAction::JumpAttack,
            technique_id: Some(TechniqueId::BeeJumpAttack),
            ..default()
        };
        let mut motor = FighterMotor {
            grounded: false,
            facing: Vec3::Z,
            velocity: Vec3::new(0.0, BEE_JUMP_ATTACK_UP_SPEED, BEE_JUMP_ATTACK_FORWARD_SPEED),
            air_attack_used: true,
            jump_attack_landing_recovery: true,
            bee_air_dash_motion_active: true,
            bee_air_dash_shot_available: true,
            dash_jump_carry_timer: 0.2,
            dash_jump_carry_speed_limit: 6.0,
            ..default()
        };
        let velocity_before = motor.velocity;
        let input = FighterInput {
            movement: Vec2::X,
            heavy: true,
            ..default()
        };

        assert!(try_start_bee_air_dash_x_shot(
            &mut motor,
            &mut action,
            &input,
            input.movement,
            loadout,
            &catalog,
        ));

        assert_eq!(action.action, FighterAction::JumpHeavyAttack);
        assert_eq!(action.technique_id, Some(TechniqueId::BeeJumpHeavy));
        assert_eq!(motor.facing, Vec3::X);
        assert_eq!(motor.velocity, velocity_before);
        assert!(motor.air_attack_used);
        assert!(!motor.jump_attack_landing_recovery);
        assert!(motor.bee_air_dash_motion_active);
        assert!(!motor.bee_air_dash_shot_available);
        assert_eq!(motor.dash_jump_carry_timer, 0.0);
        assert_eq!(motor.dash_jump_carry_speed_limit, 0.0);
    }

    #[test]
    fn bee_air_dash_jump_x_stays_available_after_jump_c_recovers() {
        let catalog = CharacterMoveCatalog::default();
        let loadout = LoadoutContext::for_character(
            CharacterKind::Bee,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let mut action = FighterActionState {
            action: FighterAction::Jumping,
            ..default()
        };
        let mut motor = FighterMotor {
            grounded: false,
            facing: Vec3::Z,
            velocity: Vec3::new(0.0, BEE_JUMP_ATTACK_UP_SPEED, BEE_JUMP_ATTACK_FORWARD_SPEED),
            air_attack_used: true,
            bee_air_dash_motion_active: true,
            bee_air_dash_shot_available: true,
            ..default()
        };
        let input = FighterInput {
            heavy: true,
            ..default()
        };

        assert!(try_start_bee_air_dash_x_shot(
            &mut motor,
            &mut action,
            &input,
            input.movement,
            loadout,
            &catalog,
        ));

        assert_eq!(action.action, FighterAction::JumpHeavyAttack);
        assert_eq!(action.technique_id, Some(TechniqueId::BeeJumpHeavy));
        assert!(motor.bee_air_dash_motion_active);
        assert!(!motor.bee_air_dash_shot_available);
    }

    #[test]
    fn jump_heavy_attack_starts_air_stall_fish_throw() {
        let catalog = CharacterMoveCatalog::default();
        let loadout = LoadoutContext::from_style(FighterStyleKind::Anchor);
        let mut action = FighterActionState {
            action: FighterAction::Jumping,
            ..default()
        };
        let mut motor = FighterMotor {
            grounded: false,
            velocity: Vec3::new(6.0, 4.0, -2.0),
            dash_jump_carry_timer: 0.2,
            ..default()
        };

        start_jump_heavy_attack(&mut motor, &mut action, loadout, &catalog);

        assert_eq!(action.action, FighterAction::JumpHeavyAttack);
        assert!(motor.air_attack_used);
        assert!(!motor.jump_attack_landing_recovery);
        assert_eq!(motor.dash_jump_carry_timer, 0.0);
        assert_eq!(motor.velocity.x, 6.0 * JUMP_HEAVY_AIR_STALL_PLANAR_SCALE);
        assert_eq!(motor.velocity.z, -2.0 * JUMP_HEAVY_AIR_STALL_PLANAR_SCALE);
        assert_eq!(motor.velocity.y, JUMP_HEAVY_AIR_STALL_UP_SPEED);
        assert!(
            body_motion_profile(FighterAction::JumpHeavyAttack).gravity_scale
                < body_motion_profile(FighterAction::Jumping).gravity_scale
        );
    }

    #[test]
    fn pig_jump_heavy_starts_downward_meat_slam_from_jump_command() {
        let catalog = CharacterMoveCatalog::default();
        let loadout = LoadoutContext::for_character(
            CharacterKind::Pig,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let mut action = FighterActionState::default();
        let mut motor = FighterMotor {
            grounded: true,
            facing: Vec3::Z,
            velocity: Vec3::ZERO,
            ledge_grace_timer: 0.2,
            ..default()
        };

        start_jump_heavy_attack(&mut motor, &mut action, loadout, &catalog);

        assert_eq!(action.action, FighterAction::JumpHeavyAttack);
        assert_eq!(action.technique_id, Some(TechniqueId::PigJumpHeavy));
        assert!(!motor.grounded);
        assert_eq!(motor.ledge_grace_timer, 0.0);
        assert!(motor.air_attack_used);
        assert!(motor.jump_attack_landing_recovery);
        assert_eq!(motor.velocity.z, JUMP_ATTACK_DIVE_FORWARD_SPEED * 0.22);
        assert_eq!(motor.velocity.y, -JUMP_ATTACK_DIVE_DOWN_SPEED * 1.08);
    }

    #[test]
    fn dash_attack_inputs_route_to_combo_finishers() {
        let catalog = CharacterMoveCatalog::default();
        let loadout = LoadoutContext::from_style(FighterStyleKind::Anchor);
        let pig = LoadoutContext::for_character(
            CharacterKind::Pig,
            FighterStyleKind::Anchor,
            crate::equipment::EquipmentKind::CounterCell,
        );
        let chick = LoadoutContext::for_character(
            CharacterKind::Chick,
            FighterStyleKind::Anchor,
            crate::equipment::EquipmentKind::CounterCell,
        );
        assert_eq!(
            dash_finisher_for_input(
                &FighterInput {
                    light: true,
                    ..default()
                },
                loadout,
                &catalog
            ),
            Some(TechniqueId::CatDashComboFinisher)
        );
        assert_eq!(
            dash_finisher_for_input(
                &FighterInput {
                    heavy: true,
                    ..default()
                },
                loadout,
                &catalog
            ),
            Some(TechniqueId::CatHeavy2)
        );
        assert_eq!(
            dash_finisher_for_input(
                &FighterInput {
                    raw_heavy_pressed: true,
                    ..default()
                },
                loadout,
                &catalog
            ),
            Some(TechniqueId::CatHeavy2)
        );
        assert_eq!(
            dash_finisher_for_input(
                &FighterInput {
                    heavy: true,
                    ..default()
                },
                pig,
                &catalog
            ),
            Some(TechniqueId::PigHeavy)
        );
        assert_eq!(
            dash_finisher_extra_impulse(TechniqueId::PigComboFinisher, pig),
            0.0
        );
        assert_eq!(
            dash_finisher_extra_impulse(TechniqueId::CatDashComboFinisher, loadout),
            DASH_ATTACK_EXTRA_IMPULSE
        );
        assert_eq!(
            dash_finisher_for_input(
                &FighterInput {
                    light: true,
                    ..default()
                },
                chick,
                &catalog
            ),
            Some(TechniqueId::ChickDashAttack)
        );
        assert_eq!(
            dash_finisher_for_input(
                &FighterInput {
                    heavy: true,
                    ..default()
                },
                chick,
                &catalog
            ),
            Some(TechniqueId::ChickDashHeavy)
        );
    }

    #[test]
    fn penguin_dash_ultimate_routes_before_dash_x_and_spends_mp() {
        let catalog = CharacterMoveCatalog::default();
        let penguin = LoadoutContext::for_character(
            CharacterKind::Penguin,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let mut motor = FighterMotor {
            velocity: Vec3::Z * 6.0,
            facing: Vec3::Z,
            grounded: true,
            ..default()
        };
        let mut stats = FighterStats::default();
        let mut action = FighterActionState {
            action: FighterAction::Dashing,
            ..default()
        };

        assert!(try_start_penguin_dash_ultimate(
            &mut motor,
            &mut stats,
            &mut action,
            &FighterInput {
                light_held: true,
                heavy_held: true,
                ..default()
            },
            penguin,
            &catalog,
        ));
        assert_eq!(action.action, FighterAction::UltimateRush);
        assert_eq!(action.technique_id, Some(TechniqueId::PenguinUltimateRush));
        assert_eq!(stats.stamina, MAX_STAMINA - ULTIMATE_STAMINA_COST);
        assert!(motor.velocity.z >= DASH_ATTACK_EXTRA_IMPULSE);
    }

    #[test]
    fn penguin_dash_c_shoots_snowflake_shot_in_current_dash_direction() {
        let catalog = CharacterMoveCatalog::default();
        let penguin = LoadoutContext::for_character(
            CharacterKind::Penguin,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let mut motor = FighterMotor {
            facing: Vec3::Z,
            velocity: Vec3::new(2.5, 0.0, 5.0),
            ..default()
        };
        let velocity_before = motor.velocity;
        let mut action = FighterActionState {
            action: FighterAction::Dashing,
            ..default()
        };

        assert!(start_dash_finisher_from_dash(
            &mut motor,
            &mut action,
            &FighterInput {
                light: true,
                movement: Vec2::new(1.0, 0.0),
                ..default()
            },
            penguin,
            &catalog,
        ));
        assert_eq!(action.action, FighterAction::DashAttack);
        assert_eq!(action.technique_id, Some(TechniqueId::PenguinDashAttack));
        assert_eq!(motor.facing, Vec3::X);
        assert_eq!(motor.velocity, velocity_before);
    }

    #[test]
    fn chick_dash_finishers_backstep_without_turning() {
        let catalog = CharacterMoveCatalog::default();
        let chick = LoadoutContext::for_character(
            CharacterKind::Chick,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );

        let mut dash_c_motor = FighterMotor {
            facing: Vec3::Z,
            velocity: Vec3::new(3.0, 1.25, 2.0),
            grounded: true,
            ..default()
        };
        let mut dash_c_action = FighterActionState {
            action: FighterAction::Dashing,
            ..default()
        };

        assert!(start_dash_finisher_from_dash(
            &mut dash_c_motor,
            &mut dash_c_action,
            &FighterInput {
                light: true,
                movement: Vec2::X,
                ..default()
            },
            chick,
            &catalog,
        ));
        assert_eq!(dash_c_action.action, FighterAction::DashAttack);
        assert_eq!(
            dash_c_action.technique_id,
            Some(TechniqueId::ChickDashAttack)
        );
        assert_eq!(dash_c_motor.facing, Vec3::Z);
        assert_vec3_close(
            dash_c_motor.velocity,
            Vec3::new(0.0, 1.25, -CHICK_DASH_C_BACKSTEP_SPEED),
            0.001,
        );

        let mut dash_x_motor = FighterMotor {
            facing: Vec3::X,
            velocity: Vec3::new(2.0, -0.5, 3.0),
            grounded: true,
            ..default()
        };
        let mut dash_x_action = FighterActionState {
            action: FighterAction::Dashing,
            ..default()
        };

        assert!(start_dash_finisher_from_dash(
            &mut dash_x_motor,
            &mut dash_x_action,
            &FighterInput {
                heavy: true,
                movement: -Vec2::Y,
                ..default()
            },
            chick,
            &catalog,
        ));
        assert_eq!(dash_x_action.action, FighterAction::DashAttack);
        assert_eq!(
            dash_x_action.technique_id,
            Some(TechniqueId::ChickDashHeavy)
        );
        assert_eq!(dash_x_motor.facing, Vec3::X);
        assert_vec3_close(
            dash_x_motor.velocity,
            Vec3::new(-CHICK_DASH_X_BACKSTEP_SPEED, -0.5, 0.0),
            0.001,
        );
        assert!((CHICK_DASH_X_BACKSTEP_SPEED - CHICK_DASH_C_BACKSTEP_SPEED * 2.0).abs() < 0.001);
        assert!(
            (CHICK_DASH_X_BACKSTEP_DISTANCE - CHICK_DASH_C_BACKSTEP_DISTANCE * 2.0).abs() < 0.001
        );
    }

    #[test]
    fn chick_orbit_egg_c_can_interrupt_to_dash_finishers() {
        let catalog = CharacterMoveCatalog::default();
        let chick = LoadoutContext::for_character(
            CharacterKind::Chick,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );

        let mut dash_c_motor = FighterMotor {
            facing: Vec3::Z,
            velocity: Vec3::new(4.0, 0.0, 1.0),
            grounded: true,
            ..default()
        };
        let mut dash_c_action = FighterActionState {
            action: FighterAction::LightAttack1,
            technique_id: Some(TechniqueId::ChickLight1),
            ..default()
        };

        assert!(start_chick_dash_finisher_from_light_attack(
            &mut dash_c_motor,
            &mut dash_c_action,
            &FighterInput {
                dash: true,
                light_held: true,
                movement: Vec2::X,
                ..default()
            },
            chick,
            &catalog,
        ));
        assert_eq!(dash_c_action.action, FighterAction::DashAttack);
        assert_eq!(
            dash_c_action.technique_id,
            Some(TechniqueId::ChickDashAttack)
        );
        assert_eq!(dash_c_motor.facing, Vec3::Z);
        assert_vec3_close(
            dash_c_motor.velocity,
            Vec3::new(0.0, 0.0, -CHICK_DASH_C_BACKSTEP_SPEED),
            0.001,
        );

        let mut dash_x_motor = FighterMotor {
            facing: Vec3::X,
            velocity: Vec3::new(1.0, 0.0, 4.0),
            grounded: true,
            ..default()
        };
        let mut dash_x_action = FighterActionState {
            action: FighterAction::LightAttack1,
            technique_id: Some(TechniqueId::ChickLight1),
            ..default()
        };

        assert!(start_chick_dash_finisher_from_light_attack(
            &mut dash_x_motor,
            &mut dash_x_action,
            &FighterInput {
                dash: true,
                light_held: true,
                heavy_held: true,
                movement: -Vec2::Y,
                ..default()
            },
            chick,
            &catalog,
        ));
        assert_eq!(dash_x_action.action, FighterAction::DashAttack);
        assert_eq!(
            dash_x_action.technique_id,
            Some(TechniqueId::ChickDashHeavy)
        );
        assert_eq!(dash_x_motor.facing, Vec3::X);
        assert_vec3_close(
            dash_x_motor.velocity,
            Vec3::new(-CHICK_DASH_X_BACKSTEP_SPEED, 0.0, 0.0),
            0.001,
        );
    }

    #[test]
    fn chick_orbit_egg_dash_interrupt_requires_dash_and_chick_c() {
        let catalog = CharacterMoveCatalog::default();
        let chick = LoadoutContext::for_character(
            CharacterKind::Chick,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let mut motor = FighterMotor {
            facing: Vec3::Z,
            grounded: true,
            ..default()
        };
        let mut action = FighterActionState {
            action: FighterAction::LightAttack1,
            technique_id: Some(TechniqueId::ChickLight1),
            ..default()
        };

        assert!(!start_chick_dash_finisher_from_light_attack(
            &mut motor,
            &mut action,
            &FighterInput {
                light_held: true,
                ..default()
            },
            chick,
            &catalog,
        ));
        assert_eq!(action.technique_id, Some(TechniqueId::ChickLight1));

        action.technique_id = Some(TechniqueId::ChickLight2);
        assert!(!start_chick_dash_finisher_from_light_attack(
            &mut motor,
            &mut action,
            &FighterInput {
                dash: true,
                light_held: true,
                ..default()
            },
            chick,
            &catalog,
        ));
        assert_eq!(action.technique_id, Some(TechniqueId::ChickLight2));
    }

    #[test]
    fn chick_dash_backstep_profile_preserves_authored_speed_until_stop() {
        let catalog = CharacterMoveCatalog::default();
        let body = catalog.body(CharacterKind::Chick);
        let motor = FighterMotor {
            grounded: true,
            ..default()
        };

        for (technique_id, expected_speed) in [
            (TechniqueId::ChickDashAttack, CHICK_DASH_C_BACKSTEP_SPEED),
            (TechniqueId::ChickDashHeavy, CHICK_DASH_X_BACKSTEP_SPEED),
        ] {
            let action = FighterActionState {
                action: FighterAction::DashAttack,
                technique_id: Some(technique_id),
                ..default()
            };
            let profile = character_body_motion_profile_for_state(
                &action,
                &motor,
                FighterStyleKind::Anchor,
                body,
            );
            let speed_limit = planar_speed_limit(&motor, true, body.ground_speed, profile);

            assert_eq!(profile.input_scale, 0.0);
            assert_eq!(profile.ground_friction, 0.0);
            assert_eq!(profile.stop_friction, 0.0);
            assert!(speed_limit > expected_speed);
            assert!(speed_limit > CHICK_DASH_X_BACKSTEP_SPEED);
        }
    }

    #[test]
    fn penguin_dash_snowflake_shots_recover_to_dashing_instead_of_idle() {
        let catalog = CharacterMoveCatalog::default();
        let feel = CombatFeelTuning::from_overrides(crate::feel::CombatFeelFile::default());
        let penguin = LoadoutContext::for_character(
            CharacterKind::Penguin,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );

        for (technique_id, expected_duration) in [
            (TechniqueId::PenguinDashAttack, 0.16),
            (TechniqueId::PenguinDashHeavy, 0.18),
        ] {
            let mut action = FighterActionState {
                action: FighterAction::DashAttack,
                technique_id: Some(technique_id),
                ..default()
            };
            action.elapsed = attack_duration_for_state(&action, penguin, &feel, &catalog);
            assert_eq!(action.elapsed, expected_duration);
            if should_return_to_dashing_on_dash_completion(&action) {
                set_action(&mut action, FighterAction::Dashing);
            } else {
                set_action(&mut action, FighterAction::Idle);
            }

            assert_eq!(action.action, FighterAction::Dashing);
            assert!(action.technique_id.is_none());
        }
    }

    #[test]
    fn penguin_ground_ultimate_accepts_held_z_x_c_shortcut() {
        let catalog = CharacterMoveCatalog::default();
        let penguin = LoadoutContext::for_character(
            CharacterKind::Penguin,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let mut motor = FighterMotor {
            grounded: true,
            ..default()
        };
        let mut stats = FighterStats::default();
        let mut action = FighterActionState::default();

        assert!(try_start_ultimate_from_input(
            &mut motor,
            &mut stats,
            &mut action,
            &FighterInput {
                aim: true,
                light_held: true,
                heavy_held: true,
                ..default()
            },
            penguin,
            &catalog,
        ));
        assert_eq!(action.action, FighterAction::UltimateStartup);
        assert_eq!(
            action.technique_id,
            Some(TechniqueId::PenguinUltimateStartup)
        );
        assert_eq!(stats.stamina, MAX_STAMINA - ULTIMATE_STAMINA_COST);
    }

    #[test]
    fn chick_ground_ultimate_accepts_held_z_x_c_shortcut() {
        let catalog = CharacterMoveCatalog::default();
        let chick = LoadoutContext::for_character(
            CharacterKind::Chick,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let mut motor = FighterMotor {
            grounded: true,
            ..default()
        };
        let mut stats = FighterStats::default();
        let mut action = FighterActionState::default();

        assert!(try_start_ultimate_from_input(
            &mut motor,
            &mut stats,
            &mut action,
            &FighterInput {
                aim: true,
                light_held: true,
                heavy_held: true,
                ..default()
            },
            chick,
            &catalog,
        ));
        assert_eq!(action.action, FighterAction::UltimateStartup);
        assert_eq!(action.technique_id, Some(TechniqueId::ChickUltimateStartup));
        assert_eq!(stats.stamina, MAX_STAMINA - ULTIMATE_STAMINA_COST);
    }

    #[test]
    fn pig_heavy_charge_tracks_hold_release_and_dash_release() {
        let catalog = CharacterMoveCatalog::default();
        let pig = LoadoutContext::for_character(
            CharacterKind::Pig,
            FighterStyleKind::Anchor,
            crate::equipment::EquipmentKind::CounterCell,
        );
        let mut action = FighterActionState {
            action: FighterAction::HeavyAttack,
            technique_id: Some(TechniqueId::PigHeavy),
            ..default()
        };

        tick_pig_heavy_charge(
            &mut action,
            &FighterInput {
                heavy_held: true,
                ..default()
            },
            0.3,
        );
        assert!(action.charge_elapsed > 0.29);
        assert!(!action.charge_release_requested);

        tick_pig_heavy_charge(
            &mut action,
            &FighterInput {
                heavy_held: true,
                ..default()
            },
            2.0,
        );
        assert_eq!(action.charge_elapsed, pig_heavy_full_charge_secs());
        assert!(!action.charge_release_requested);

        tick_pig_heavy_charge(
            &mut action,
            &FighterInput {
                heavy_released: true,
                ..default()
            },
            0.0,
        );
        assert!(action.charge_release_requested);

        let mut dash_action = FighterActionState {
            action: FighterAction::Dashing,
            charge_elapsed: 0.52,
            ..default()
        };
        start_pig_dash_heavy_release(&mut dash_action, pig, &catalog);
        assert_eq!(dash_action.action, FighterAction::HeavyAttack);
        assert_eq!(dash_action.technique_id, Some(TechniqueId::PigHeavy));
        assert!(dash_action.charge_release_requested);
        assert!(dash_action.charge_elapsed > 0.5);
    }

    #[test]
    fn pig_heavy_hold_pauses_duration_until_release() {
        let feel = CombatFeelTuning::from_overrides(crate::feel::CombatFeelFile::default());
        let catalog = CharacterMoveCatalog::default();
        let pig = LoadoutContext::for_character(
            CharacterKind::Pig,
            FighterStyleKind::Anchor,
            crate::equipment::EquipmentKind::CounterCell,
        );
        let mut action = FighterActionState {
            action: FighterAction::HeavyAttack,
            technique_id: Some(TechniqueId::PigHeavy),
            charge_elapsed: pig_heavy_full_charge_secs(),
            elapsed: 10.0,
            ..default()
        };

        assert!(attack_duration_for_state(&action, pig, &feel, &catalog).is_infinite());
        action.charge_release_requested = true;
        assert!(attack_duration_for_state(&action, pig, &feel, &catalog).is_finite());
    }

    #[test]
    fn pig_heavy_pose_uses_tint_weight_without_body_rotation() {
        let feel = CombatFeelTuning::from_overrides(crate::feel::CombatFeelFile::default());
        let charging = FighterActionState {
            action: FighterAction::HeavyAttack,
            technique_id: Some(TechniqueId::PigHeavy),
            charge_elapsed: pig_heavy_full_charge_secs() * 0.5,
            ..default()
        };
        let released = FighterActionState {
            action: FighterAction::HeavyAttack,
            technique_id: Some(TechniqueId::PigHeavy),
            charge_elapsed: pig_heavy_full_charge_secs(),
            charge_release_requested: true,
            ..default()
        };

        let charging_pose = fighter_visual_pose(&charging, 0.0, &feel);
        let released_pose = fighter_visual_pose(&released, 0.0, &feel);

        assert!(charging_pose.pitch.abs() < 0.001);
        assert!(charging_pose.yaw.abs() < 0.001);
        assert!(charging_pose.roll.abs() < 0.001);
        assert!(released_pose.pitch.abs() < 0.001);
        assert!(released_pose.yaw.abs() < 0.001);
        assert!(released_pose.roll.abs() < 0.001);
        assert!(charging_pose.scale.x > action_visual_scale(FighterAction::HeavyAttack).x * 0.95);
        assert!(charging_pose.scale.z > charging_pose.scale.y);
    }

    #[test]
    fn pig_ultimate_pose_is_weighted_instead_of_scratchy() {
        let feel = CombatFeelTuning::from_overrides(crate::feel::CombatFeelFile::default());
        let startup = FighterActionState {
            action: FighterAction::UltimateStartup,
            technique_id: Some(TechniqueId::PigUltimateStartup),
            elapsed: 0.98,
            ..default()
        };
        let rush_brace = FighterActionState {
            action: FighterAction::UltimateRush,
            technique_id: Some(TechniqueId::PigUltimateRush),
            elapsed: 0.2,
            ..default()
        };
        let rush_bomb = FighterActionState {
            action: FighterAction::UltimateRush,
            technique_id: Some(TechniqueId::PigUltimateRush),
            elapsed: 1.08,
            ..default()
        };

        let startup_pose = fighter_visual_pose(&startup, 0.0, &feel);
        let brace_pose = fighter_visual_pose(&rush_brace, 0.0, &feel);
        let bomb_pose = fighter_visual_pose(&rush_bomb, 0.0, &feel);

        assert!(startup_pose.yaw.abs() < 0.001);
        assert!(startup_pose.pitch < -0.45);
        assert!(brace_pose.yaw.abs() < 0.08);
        assert!(brace_pose.roll.abs() < 0.18);
        assert!(bomb_pose.pitch < brace_pose.pitch);
        assert!(bomb_pose.scale.z > brace_pose.scale.z);
        assert!(bomb_pose.scale.y < brace_pose.scale.y);
    }

    #[test]
    fn pig_ultimate_lock_lasts_until_heavy_finisher() {
        assert_eq!(
            ultimate_lock_release_after(Some(TechniqueId::CatUltimateRush)),
            0.9
        );
        assert!(ultimate_lock_release_after(Some(TechniqueId::PigUltimateRush)) > 1.18);
    }

    #[test]
    fn pig_charge_tint_tracks_only_active_pig_charge() {
        let charging = FighterActionState {
            action: FighterAction::HeavyAttack,
            technique_id: Some(TechniqueId::PigHeavy),
            charge_elapsed: pig_heavy_full_charge_secs() * 0.5,
            ..default()
        };
        let released = FighterActionState {
            action: FighterAction::HeavyAttack,
            technique_id: Some(TechniqueId::PigHeavy),
            charge_elapsed: pig_heavy_full_charge_secs() * 0.5,
            charge_release_requested: true,
            ..default()
        };
        let dash_charging = FighterActionState {
            action: FighterAction::Dashing,
            charge_elapsed: pig_heavy_full_charge_secs(),
            ..default()
        };

        assert!((pig_charge_tint_amount(CharacterKind::Pig, &charging) - 0.5).abs() < 0.001);
        assert_eq!(pig_charge_tint_amount(CharacterKind::Cat, &charging), 0.0);
        assert_eq!(pig_charge_tint_amount(CharacterKind::Pig, &released), 0.0);
        assert_eq!(
            pig_charge_tint_amount(CharacterKind::Pig, &dash_charging),
            1.0
        );
    }

    #[test]
    fn drunk_input_inverts_both_axes_without_changing_actions() {
        let mut input = FighterInput {
            movement: Vec2::new(0.75, -0.4),
            aim: true,
            jump: true,
            dash: true,
            light: true,
            heavy: true,
            grab: true,
            guard: true,
            ultimate: true,
            special: true,
            ..default()
        };
        invert_directional_input(&mut input);

        assert_eq!(input.movement, Vec2::new(-0.75, 0.4));
        assert!(input.aim && input.jump && input.dash);
        assert!(input.light && input.heavy && input.grab && input.guard);
        assert!(input.ultimate && input.special);
    }

    #[test]
    fn drunk_contacts_refresh_to_five_seconds_without_stacking() {
        let mut status = DrunkStatus {
            remaining: 1.2,
            ..default()
        };
        status.refresh();
        assert_eq!(status.remaining, DRUNK_DURATION);
        status.remaining = DRUNK_DURATION + 1.0;
        status.refresh();
        assert_eq!(status.remaining, DRUNK_DURATION + 1.0);
    }

    #[test]
    fn drunk_tint_pulses_and_fades_during_final_half_second() {
        let status = DrunkStatus {
            remaining: DRUNK_DURATION,
            ..default()
        };
        let full = drunk_tint_amount(&status, 0.0);
        let pulsed = drunk_tint_amount(&status, 0.15);
        let fading = drunk_tint_amount(
            &DrunkStatus {
                remaining: 0.2,
                ..status
            },
            0.15,
        );
        assert!(full > 0.0 && pulsed != full);
        assert!(fading < full);
    }

    #[test]
    fn tint_priority_keeps_counter_flash_above_drunk() {
        let drunk = Some(DrunkStatus {
            remaining: DRUNK_DURATION,
            ..default()
        });
        let counter_action = FighterActionState {
            action: FighterAction::GuardCounter,
            elapsed: 0.0,
            ..default()
        };
        let counter = active_fighter_tint(CharacterKind::Cat, &counter_action, None, drunk, 0.0);
        assert!(matches!(
            counter,
            Some(FighterTint {
                palette: FighterTintPalette::CounterFlash,
                ..
            })
        ));

        let drunk_tint = active_fighter_tint(
            CharacterKind::Cat,
            &FighterActionState::default(),
            None,
            drunk,
            0.0,
        );
        assert!(matches!(
            drunk_tint,
            Some(FighterTint {
                palette: FighterTintPalette::Drunk,
                ..
            })
        ));
    }

    #[test]
    fn burning_tint_turns_the_fighter_hot_orange() {
        let base = Color::srgb(0.28, 0.42, 0.68);
        let burning = burning_tinted_color(base, 1.0).to_srgba();
        let base = base.to_srgba();

        assert!(burning.red > base.red);
        assert!(burning.green < base.green);
        assert!(burning.blue < base.blue);
    }

    #[test]
    fn guard_counter_flash_tint_fades_quickly() {
        let startup = FighterActionState {
            action: FighterAction::GuardCounter,
            elapsed: 0.0,
            ..default()
        };
        let half = FighterActionState {
            action: FighterAction::GuardCounter,
            elapsed: GUARD_COUNTER_FLASH_DURATION * 0.5,
            ..default()
        };
        let ended = FighterActionState {
            action: FighterAction::GuardCounter,
            elapsed: GUARD_COUNTER_FLASH_DURATION,
            ..default()
        };

        assert_eq!(guard_counter_flash_tint_amount(&startup), 1.0);
        assert!((guard_counter_flash_tint_amount(&half) - 0.5).abs() < 0.001);
        assert_eq!(guard_counter_flash_tint_amount(&ended), 0.0);
    }

    #[test]
    fn pig_charge_tint_color_moves_toward_hot_red() {
        let base = Color::srgb(0.2, 0.5, 0.8);
        let cold = charge_tinted_color(base, 0.0).to_srgba();
        let hot = charge_tinted_color(base, 1.0).to_srgba();

        assert!(hot.red > cold.red);
        assert!(hot.green < cold.green);
        assert!(hot.blue < cold.blue);
    }

    #[test]
    fn authored_gravity_respects_takeoff_float_and_terminal_fall() {
        let profile = body_motion_profile(FighterAction::Jumping);
        let takeoff = authored_gravity_velocity_y(0.0, 0.016, 0.05, profile);
        let normal = authored_gravity_velocity_y(0.0, 0.016, 0.0, profile);
        let clamped = authored_gravity_velocity_y(-99.0, 0.016, 0.0, profile);

        assert!(takeoff > normal);
        assert_eq!(clamped, -profile.terminal_fall_speed);
    }

    #[test]
    fn landing_stick_scales_with_fall_speed() {
        assert_eq!(landing_stick_duration(-1.0), 0.0);
        assert_eq!(landing_stick_duration(-3.0), 0.035);
        assert_eq!(landing_stick_duration(-5.0), 0.065);
        assert_eq!(landing_stick_duration(-9.0), 0.1);
        assert!(landing_stick_duration(-5.0) > landing_stick_duration(-3.0));
        assert!(landing_stick_duration(-9.0) > landing_stick_duration(-5.0));
    }

    #[test]
    fn aftermath_feedback_weights_distinguish_floor_outcomes() {
        assert!(
            aftermath_feedback_priority(ReactionFamilyId::GroundBounceDown)
                > aftermath_feedback_priority(ReactionFamilyId::GroundedDownGetup)
        );
        assert!(
            aftermath_landing_shake(ReactionFamilyId::GroundBounceDown)
                > aftermath_landing_shake(ReactionFamilyId::AerialSpikeDown)
        );
    }

    #[test]
    fn vector_style_shortens_dash_attack_flow() {
        let feel = CombatFeelTuning::from_overrides(crate::feel::CombatFeelFile::default());
        let catalog = CharacterMoveCatalog::default();
        let anchor = LoadoutContext::new(
            crate::styles::FighterStyleKind::Anchor,
            crate::equipment::EquipmentKind::CounterCell,
        );
        let vector = LoadoutContext::new(
            crate::styles::FighterStyleKind::Vector,
            crate::equipment::EquipmentKind::CounterCell,
        );

        assert!(
            attack_duration(FighterAction::DashAttack, vector, &feel, &catalog)
                < attack_duration(FighterAction::DashAttack, anchor, &feel, &catalog)
        );
    }

    #[test]
    fn light_followup_can_buffer_before_chain_window_opens() {
        let feel = CombatFeelTuning::from_overrides(crate::feel::CombatFeelFile {
            techniques: vec![crate::feel::TechniqueOverride {
                id: TechniqueId::CatLight1,
                input_buffer_ms: Some(240),
                ..default()
            }],
            ..default()
        });
        let catalog = CharacterMoveCatalog::default();
        let loadout = LoadoutContext::from_style(FighterStyleKind::Anchor);
        let mut action = FighterActionState {
            action: FighterAction::LightAttack1,
            elapsed: 0.05,
            technique_id: Some(TechniqueId::CatLight1),
            branch_window_open: false,
            ..default()
        };

        queue_chained_followup(
            &mut action,
            &FighterInput {
                light: true,
                ..default()
            },
            loadout,
            true,
            0.016,
            &feel,
            &catalog,
        );
        assert_eq!(action.buffered_button, Some(TechniqueButton::A));
        assert_eq!(action.queued_technique, None);

        action.elapsed = 0.2;
        action.branch_window_open = true;
        queue_chained_followup(
            &mut action,
            &FighterInput::default(),
            loadout,
            true,
            0.15,
            &feel,
            &catalog,
        );

        assert_eq!(action.queued_technique, Some(TechniqueId::CatLight2));
        assert_eq!(action.buffered_button, None);
    }

    #[test]
    fn heavy_followup_can_buffer_before_chain_window_opens() {
        let feel = CombatFeelTuning::from_overrides(crate::feel::CombatFeelFile::default());
        let catalog = CharacterMoveCatalog::default();
        let loadout = LoadoutContext::from_style(FighterStyleKind::Anchor);
        let mut action = FighterActionState {
            action: FighterAction::HeavyAttack,
            elapsed: 0.03,
            technique_id: Some(TechniqueId::CatHeavy),
            branch_window_open: false,
            ..default()
        };

        queue_chained_followup(
            &mut action,
            &FighterInput {
                heavy: true,
                ..default()
            },
            loadout,
            true,
            0.016,
            &feel,
            &catalog,
        );
        assert_eq!(action.buffered_button, Some(TechniqueButton::B));
        assert_eq!(action.queued_technique, None);

        action.elapsed = 0.08;
        action.branch_window_open = true;
        queue_chained_followup(
            &mut action,
            &FighterInput::default(),
            loadout,
            true,
            0.08,
            &feel,
            &catalog,
        );

        assert_eq!(action.queued_technique, Some(TechniqueId::CatHeavy2));
        assert_eq!(action.buffered_button, None);
    }

    #[test]
    fn pig_light_chain_uses_same_preinput_buffer_as_cat() {
        let feel = CombatFeelTuning::default();
        let catalog = CharacterMoveCatalog::default();
        let loadout = LoadoutContext::for_character(
            CharacterKind::Pig,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let mut first = FighterActionState {
            action: FighterAction::LightAttack1,
            elapsed: 0.05,
            technique_id: Some(TechniqueId::PigLight1),
            branch_window_open: false,
            ..default()
        };

        queue_chained_followup(
            &mut first,
            &FighterInput {
                light: true,
                ..default()
            },
            loadout,
            true,
            0.016,
            &feel,
            &catalog,
        );
        assert_eq!(first.buffered_button, Some(TechniqueButton::A));
        assert_eq!(first.queued_technique, None);

        first.elapsed = 0.2;
        first.branch_window_open = true;
        queue_chained_followup(
            &mut first,
            &FighterInput::default(),
            loadout,
            true,
            0.08,
            &feel,
            &catalog,
        );
        assert_eq!(first.queued_technique, Some(TechniqueId::PigLight2));

        let mut second = FighterActionState {
            action: FighterAction::LightAttack2,
            elapsed: 0.05,
            technique_id: Some(TechniqueId::PigLight2),
            branch_window_open: false,
            ..default()
        };
        queue_chained_followup(
            &mut second,
            &FighterInput {
                light: true,
                ..default()
            },
            loadout,
            true,
            0.016,
            &feel,
            &catalog,
        );
        assert_eq!(second.buffered_button, Some(TechniqueButton::A));
        assert_eq!(second.queued_technique, None);

        second.elapsed = 0.16;
        second.branch_window_open = true;
        assert!(second.elapsed < attack_duration_for_state(&second, loadout, &feel, &catalog));
        queue_chained_followup(
            &mut second,
            &FighterInput::default(),
            loadout,
            true,
            0.08,
            &feel,
            &catalog,
        );
        assert_eq!(second.queued_technique, Some(TechniqueId::PigComboFinisher));
    }

    #[test]
    fn pig_heavy_charge_does_not_route_to_launcher_followup() {
        let feel = CombatFeelTuning::from_overrides(crate::feel::CombatFeelFile::default());
        let catalog = CharacterMoveCatalog::default();
        let loadout = LoadoutContext::for_character(
            CharacterKind::Pig,
            FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let mut action = FighterActionState {
            action: FighterAction::HeavyAttack,
            elapsed: 0.05,
            technique_id: Some(TechniqueId::PigHeavy),
            branch_window_open: false,
            ..default()
        };

        queue_chained_followup(
            &mut action,
            &FighterInput {
                heavy: true,
                ..default()
            },
            loadout,
            true,
            0.016,
            &feel,
            &catalog,
        );
        assert_eq!(action.buffered_button, Some(TechniqueButton::B));
        assert_eq!(action.queued_technique, None);

        action.elapsed = 0.16;
        action.branch_window_open = true;
        queue_chained_followup(
            &mut action,
            &FighterInput::default(),
            loadout,
            true,
            0.08,
            &feel,
            &catalog,
        );
        assert_eq!(action.queued_technique, None);
        assert_eq!(action.technique_id, Some(TechniqueId::PigHeavy));
    }

    #[test]
    fn roster_light_followups_use_universal_buffered_chain_path() {
        let feel = CombatFeelTuning::default();
        let catalog = CharacterMoveCatalog::default();
        let cases = [
            (
                CharacterKind::Cat,
                TechniqueId::CatLight1,
                TechniqueId::CatLight2,
            ),
            (
                CharacterKind::Pig,
                TechniqueId::PigLight1,
                TechniqueId::PigLight2,
            ),
            (
                CharacterKind::Dog,
                TechniqueId::DogLight1,
                TechniqueId::DogLight2,
            ),
            (
                CharacterKind::Fox,
                TechniqueId::FoxLight1,
                TechniqueId::FoxLight2,
            ),
            (
                CharacterKind::Panda,
                TechniqueId::PandaLight1,
                TechniqueId::PandaLight2,
            ),
        ];

        for (character, previous, expected) in cases {
            let loadout = LoadoutContext::for_character(
                character,
                FighterStyleKind::Anchor,
                EquipmentKind::CounterCell,
            );
            let mut action = FighterActionState {
                action: FighterAction::LightAttack1,
                elapsed: 0.05,
                technique_id: Some(previous),
                branch_window_open: false,
                ..default()
            };

            queue_chained_followup(
                &mut action,
                &FighterInput {
                    light: true,
                    ..default()
                },
                loadout,
                true,
                0.016,
                &feel,
                &catalog,
            );
            assert_eq!(action.buffered_button, Some(TechniqueButton::A));
            assert_eq!(action.queued_technique, None);

            action.elapsed = 0.25;
            action.branch_window_open = true;
            queue_chained_followup(
                &mut action,
                &FighterInput::default(),
                loadout,
                true,
                0.05,
                &feel,
                &catalog,
            );
            assert_eq!(action.queued_technique, Some(expected));
        }
    }

    #[test]
    fn heavy_followup_pressed_during_hitstop_is_preserved() {
        let feel = CombatFeelTuning::from_overrides(crate::feel::CombatFeelFile::default());
        let catalog = CharacterMoveCatalog::default();
        let loadout = LoadoutContext::from_style(FighterStyleKind::Anchor);
        let mut action = FighterActionState {
            action: FighterAction::HeavyAttack,
            elapsed: 0.18,
            technique_id: Some(TechniqueId::CatHeavy),
            branch_window_open: true,
            confirmed_hit: true,
            ..default()
        };

        buffer_hitstop_followup_input(
            &mut action,
            &FighterInput {
                heavy: true,
                ..default()
            },
            loadout,
            &feel,
            &catalog,
        );
        assert_eq!(action.buffered_button, Some(TechniqueButton::B));
        assert_eq!(action.queued_technique, None);

        queue_chained_followup(
            &mut action,
            &FighterInput::default(),
            loadout,
            true,
            0.016,
            &feel,
            &catalog,
        );

        assert_eq!(action.queued_technique, Some(TechniqueId::CatHeavy2));
    }

    #[test]
    fn anchor_heavy_whiff_has_extra_recovery() {
        let feel = CombatFeelTuning::from_overrides(crate::feel::CombatFeelFile::default());
        let catalog = CharacterMoveCatalog::default();
        let anchor = LoadoutContext::new(
            crate::styles::FighterStyleKind::Anchor,
            crate::equipment::EquipmentKind::CounterCell,
        );
        let mut action = FighterActionState {
            action: FighterAction::HeavyAttack,
            technique_id: Some(TechniqueId::CatHeavy),
            ..default()
        };
        let whiff = attack_duration_for_state(&action, anchor, &feel, &catalog);
        action.confirmed_hit = true;
        let hit = attack_duration_for_state(&action, anchor, &feel, &catalog);

        assert!(whiff > hit);
    }

    #[test]
    fn throw_edge_pressure_only_boosts_outward_throws_near_edge() {
        assert_eq!(
            throw_edge_scale(Vec3::new(6.2, 0.0, 0.0), Vec3::X),
            THROW_EDGE_PRESSURE_BONUS
        );
        assert_eq!(throw_edge_scale(Vec3::new(6.2, 0.0, 0.0), -Vec3::X), 1.0);
        assert_eq!(throw_edge_scale(Vec3::new(2.0, 0.0, 0.0), Vec3::X), 1.0);
    }

    #[test]
    fn bracing_reduces_throw_profile_pressure() {
        let open = throw_impact_profile(1, ThrowStrength::Heavy, false, 1.0, 1.0);
        let braced = throw_impact_profile(1, ThrowStrength::Heavy, true, 1.0, 1.0);

        assert!(braced.damage < open.damage);
        assert!(braced.knockback < open.knockback);
        assert_eq!(braced.source, ImpactSource::GrabThrow);
    }

    #[test]
    fn ringout_bounds_use_selected_arena_definition() {
        let crown = crate::arena_defs::arena_definition(0);
        let split = crate::arena_defs::arena_definition(1);
        let between_radii = Vec3::new(crown.ringout_radius + 0.25, 0.0, 0.0);

        assert!(is_ringout_position(between_radii, crown));
        assert!(!is_ringout_position(between_radii, split));
        assert!(is_ringout_position(
            Vec3::new(0.0, crown.ringout_y - 0.1, 0.0),
            crown
        ));
    }

    #[test]
    fn ringout_danger_ramps_before_ringout() {
        let crown = crate::arena_defs::arena_definition(0);
        assert_eq!(ringout_danger_level(Vec3::ZERO, crown), 0.0);

        let near_edge = Vec3::new(crown.ringout_radius - 1.2, 0.0, 0.0);
        let edge = Vec3::new(crown.ringout_radius, 0.0, 0.0);
        assert!(ringout_danger_level(near_edge, crown) > 0.0);
        assert!((ringout_danger_level(edge, crown) - 1.0).abs() < 0.001);

        let falling = Vec3::new(0.0, crown.ringout_y + 0.6, 0.0);
        assert!(ringout_danger_level(falling, crown) > 0.5);
    }

    #[test]
    fn ringout_danger_respects_arena_radius() {
        let crown = crate::arena_defs::arena_definition(0);
        let split = crate::arena_defs::arena_definition(1);
        let position = Vec3::new(crown.ringout_radius - 0.4, 0.0, 0.0);

        assert!(ringout_danger_level(position, crown) > ringout_danger_level(position, split));
    }

    #[test]
    fn knockout_resolution_waits_for_airborne_hit_reaction() {
        let mut motor = FighterMotor {
            grounded: false,
            landing_aftermath: crate::reactions::reaction_profile_for_family(
                ReactionFamilyId::LauncherDown,
            )
            .landing_aftermath,
            ..default()
        };
        let mut action = FighterActionState {
            action: FighterAction::Hitstun,
            ..default()
        };

        assert!(should_defer_knockout_resolution(&motor, &action));

        motor.grounded = true;
        motor.landing_aftermath = None;
        motor.knockdown_on_land = false;
        motor.reaction_bounces = 0;
        action.action = FighterAction::Knockdown;

        assert!(!should_defer_knockout_resolution(&motor, &action));
    }

    #[test]
    fn knockout_resolution_waits_for_pending_ground_bounce() {
        let motor = FighterMotor {
            grounded: true,
            knockdown_on_land: true,
            reaction_bounces: 1,
            ..default()
        };
        let action = FighterActionState {
            action: FighterAction::Hitstun,
            ..default()
        };

        assert!(should_defer_knockout_resolution(&motor, &action));
    }

    #[test]
    fn selected_refill_does_not_revive_zero_health() {
        let mut stats = FighterStats::default();
        stats.health = 0.0;
        stats.health_refill_timer = 0.45;
        let action = FighterActionState::default();

        tick_practice_health_refill(&mut stats, &action);

        assert_eq!(stats.health, 0.0);
        assert_eq!(stats.health_refill_timer, 0.0);
    }

    #[test]
    fn selected_refill_restores_damaged_health() {
        let mut stats = FighterStats::default();
        stats.health = MAX_HEALTH * 0.5;
        stats.health_refill_timer = 0.5;
        let action = FighterActionState::default();

        tick_practice_health_refill(&mut stats, &action);
        assert_eq!(stats.health, MAX_HEALTH);
        assert_eq!(stats.health_refill_timer, 0.0);
    }

    #[test]
    fn selected_refill_clears_carryover_state() {
        let mut stats = FighterStats::default();
        stats.health = MAX_HEALTH * 0.25;
        stats.element_carry_strength = 0.8;
        stats.element_carry_timer = 1.4;
        let action = FighterActionState::default();

        tick_practice_health_refill(&mut stats, &action);

        assert_eq!(stats.health, MAX_HEALTH);
        assert_eq!(stats.health_refill_timer, 0.0);
        assert_eq!(stats.element_carry, None);
        assert_eq!(stats.element_carry_strength, 0.0);
        assert_eq!(stats.element_carry_timer, 0.0);
    }

    #[test]
    fn selected_refill_does_not_restart_zero_health_timer() {
        let mut stats = FighterStats::default();
        stats.health = 0.0;
        stats.health_refill_timer = 0.0;
        let action = FighterActionState::default();

        tick_practice_health_refill(&mut stats, &action);
        tick_practice_health_refill(&mut stats, &action);

        assert_eq!(stats.health_refill_timer, 0.0);
        assert_eq!(stats.health, 0.0);
    }

    #[test]
    fn selected_refill_skips_ringout_state() {
        let mut stats = FighterStats::default();
        let mut action = FighterActionState::default();
        stats.health = MAX_HEALTH * 0.5;

        action.action = FighterAction::RingOut;
        stats.health_refill_timer = 0.4;
        tick_practice_health_refill(&mut stats, &action);
        assert_eq!(stats.health, MAX_HEALTH * 0.5);
        assert_eq!(stats.health_refill_timer, 0.0);
    }

    fn aim_test_snapshot(
        entity: Entity,
        fighter_id: usize,
        position: Vec3,
    ) -> Option<AimTargetSnapshot> {
        Some(AimTargetSnapshot {
            entity,
            fighter_id,
            position,
            valid: true,
        })
    }

    #[test]
    fn aim_acquisition_uses_thirty_degree_cone_and_distance_limit() {
        let mut world = World::new();
        let inside = world.spawn_empty().id();
        let outside_angle = world.spawn_empty().id();
        let outside_distance = world.spawn_empty().id();
        let state = MatchState::default();
        let angle_29 = 29.0_f32.to_radians();
        let angle_31 = 31.0_f32.to_radians();
        let snapshots = [
            None,
            aim_test_snapshot(
                inside,
                1,
                Vec3::new(angle_29.cos(), 0.0, angle_29.sin()) * 7.49,
            ),
            aim_test_snapshot(
                outside_angle,
                2,
                Vec3::new(angle_31.cos(), 0.0, angle_31.sin()) * 4.0,
            ),
            aim_test_snapshot(outside_distance, 3, Vec3::X * 7.51),
        ];

        assert_eq!(
            select_aim_target(0, Vec3::ZERO, Vec3::X, &state, &snapshots),
            Some(inside)
        );
    }

    #[test]
    fn aim_acquisition_prefers_angle_then_distance_and_filters_invalid_opponents() {
        let mut world = World::new();
        let near_wider = world.spawn_empty().id();
        let far_centered = world.spawn_empty().id();
        let same_angle_near = world.spawn_empty().id();
        let mut state = MatchState::default();
        state.active_slots = [true; FIGHTER_COUNT];
        state.rules.team_scoring = false;
        let snapshots = [
            None,
            aim_test_snapshot(near_wider, 1, Vec3::new(2.0, 0.0, 0.3)),
            aim_test_snapshot(far_centered, 2, Vec3::X * 7.0),
            aim_test_snapshot(same_angle_near, 3, Vec3::X * 3.0),
        ];

        assert_eq!(
            select_aim_target(0, Vec3::ZERO, Vec3::X, &state, &snapshots),
            Some(same_angle_near),
            "equal angular error must use nearest distance"
        );

        let mut invalid_snapshots = snapshots;
        invalid_snapshots[3].as_mut().unwrap().valid = false;
        assert_eq!(
            select_aim_target(0, Vec3::ZERO, Vec3::X, &state, &invalid_snapshots),
            Some(far_centered),
            "a smaller angular error wins even when farther away"
        );
    }

    fn spawn_aim_test_fighter(
        world: &mut World,
        id: usize,
        position: Vec3,
        facing: Vec3,
        input: FighterInput,
    ) -> Entity {
        world
            .spawn((
                Fighter {
                    id,
                    name: "Aim test",
                    color: Color::WHITE,
                    spawn: position,
                },
                input,
                FighterMotor {
                    facing,
                    ..default()
                },
                FighterAimState {
                    direction: facing,
                    ..default()
                },
                FighterActionState::default(),
                Transform::from_translation(position),
            ))
            .id()
    }

    #[test]
    fn aim_lock_retains_to_nine_meters_then_cleans_up() {
        let mut app = App::new();
        let mut state = MatchState::default();
        state.active_slots = [true, true, false, false];
        state.stocks = [STOCK_LIVES; FIGHTER_COUNT];
        app.insert_resource(state)
            .add_systems(Update, apply_aim_assist);
        let owner = spawn_aim_test_fighter(
            app.world_mut(),
            0,
            Vec3::ZERO,
            Vec3::X,
            FighterInput {
                aim: true,
                ..default()
            },
        );
        let target = spawn_aim_test_fighter(
            app.world_mut(),
            1,
            Vec3::X * 7.0,
            Vec3::NEG_X,
            FighterInput::default(),
        );
        app.update();
        assert_eq!(
            app.world()
                .get::<FighterAimState>(owner)
                .unwrap()
                .locked_target,
            Some(target)
        );

        app.world_mut()
            .entity_mut(target)
            .get_mut::<Transform>()
            .unwrap()
            .translation = Vec3::X * 8.9;
        app.update();
        assert_eq!(
            app.world()
                .get::<FighterAimState>(owner)
                .unwrap()
                .locked_target,
            Some(target)
        );

        app.world_mut()
            .entity_mut(target)
            .get_mut::<Transform>()
            .unwrap()
            .translation = Vec3::X * 9.1;
        app.update();
        assert_eq!(
            app.world()
                .get::<FighterAimState>(owner)
                .unwrap()
                .locked_target,
            None
        );
    }

    #[test]
    fn aim_movement_retains_toward_target_and_breaks_beyond_sixty_degrees() {
        let mut app = App::new();
        let mut state = MatchState::default();
        state.active_slots = [true, true, false, false];
        state.stocks = [STOCK_LIVES; FIGHTER_COUNT];
        app.insert_resource(state)
            .add_systems(Update, apply_aim_assist);
        let owner = spawn_aim_test_fighter(
            app.world_mut(),
            0,
            Vec3::ZERO,
            Vec3::X,
            FighterInput {
                aim: true,
                movement: Vec2::X,
                ..default()
            },
        );
        let target = spawn_aim_test_fighter(
            app.world_mut(),
            1,
            Vec3::X * 4.0,
            Vec3::NEG_X,
            FighterInput::default(),
        );
        app.update();
        assert_eq!(
            app.world()
                .get::<FighterAimState>(owner)
                .unwrap()
                .locked_target,
            Some(target)
        );

        app.world_mut()
            .entity_mut(owner)
            .get_mut::<FighterInput>()
            .unwrap()
            .movement = Vec2::Y;
        app.update();
        let aim = app.world().get::<FighterAimState>(owner).unwrap();
        assert_eq!(aim.locked_target, None);
        assert_eq!(aim.manual_unlock_count, 1);
        assert_vec3_close(aim.direction, Vec3::Z, 0.001);
        assert_vec3_close(
            app.world().get::<FighterMotor>(owner).unwrap().facing,
            Vec3::Z,
            0.001,
        );
    }

    #[test]
    fn ordinary_movement_facing_does_not_overwrite_aim_owned_facing() {
        let mut motor = FighterMotor {
            facing: Vec3::X,
            ..default()
        };
        update_facing_from_movement(
            &mut motor,
            &FighterInput {
                aim: true,
                movement: Vec2::Y,
                ..default()
            },
        );
        assert_eq!(motor.facing, Vec3::X);
    }

    #[test]
    fn aim_marker_transitions_are_frame_rate_independent_reversible_and_pause_safe() {
        let half_faded = advance_aim_marker_opacity(1.0, false, AIM_MARKER_FADE_OUT * 0.5);
        assert!((half_faded - 0.5).abs() < 0.001);
        let reversed = advance_aim_marker_opacity(half_faded, true, AIM_MARKER_FADE_IN * 0.25);
        assert!((reversed - 0.75).abs() < 0.001);
        assert_eq!(advance_aim_marker_opacity(reversed, true, 0.0), reversed);

        let one_frame = aim_marker_smoothing(1.0 / 60.0);
        let two_half_frames = 1.0 - (1.0 - aim_marker_smoothing(1.0 / 120.0)).powi(2);
        assert!((one_frame - two_half_frames).abs() < 0.000_01);
    }

    #[test]
    fn aim_markers_are_human_only_player_colored_and_clean_up_outside_fighting() {
        use crate::components::ParticipantKind;
        use crate::game_state::MatchPhase;

        let mut app = App::new();
        let mut state = MatchState::default();
        state.phase = MatchPhase::Fighting;
        state.active_slots = [true, true, false, false];
        state.stocks = [STOCK_LIVES; FIGHTER_COUNT];
        app.insert_resource(state)
            .insert_resource(Time::<()>::default())
            .init_resource::<Assets<StandardMaterial>>()
            .add_systems(Update, update_aim_markers);

        let red = Color::srgb(0.9, 0.1, 0.2);
        let blue = Color::srgb(0.1, 0.3, 0.9);
        for (id, color, participant) in [
            (0, red, ParticipantKind::Human),
            (1, blue, ParticipantKind::Bot),
        ] {
            app.world_mut().spawn((
                Fighter {
                    id,
                    name: "Marker test",
                    color,
                    spawn: Vec3::ZERO,
                },
                Controller::new(
                    PlayerSlotId::new(id).unwrap(),
                    participant,
                    LocalInputAssignment::Unassigned,
                ),
                FighterInput {
                    aim: true,
                    ..default()
                },
                FighterActionState::default(),
                Transform::from_translation(Vec3::X * id as f32),
                FighterAimState {
                    direction: Vec3::X,
                    marker_position: Vec3::X * id as f32,
                    marker_opacity: 1.0,
                    aim_pressed: true,
                    ..default()
                },
            ));
        }

        let handles: Vec<_> = [0usize, 1]
            .into_iter()
            .map(|id| {
                let color = if id == 0 { red } else { blue };
                let handle = app
                    .world_mut()
                    .resource_mut::<Assets<StandardMaterial>>()
                    .add(aim_marker_material(color));
                let entity = app
                    .world_mut()
                    .spawn((
                        FighterAimMarker {
                            fighter_id: id,
                            material: handle.clone(),
                        },
                        Transform::default(),
                        Visibility::Hidden,
                    ))
                    .id();
                (entity, handle)
            })
            .collect();

        app.update();
        assert_eq!(
            app.world().get::<Visibility>(handles[0].0),
            Some(&Visibility::Visible)
        );
        assert_eq!(
            app.world().get::<Visibility>(handles[1].0),
            Some(&Visibility::Hidden),
            "bots must never display an aim marker"
        );
        let human_material = app
            .world()
            .resource::<Assets<StandardMaterial>>()
            .get(&handles[0].1)
            .unwrap();
        let tint = human_material.base_color.to_srgba();
        assert!(tint.red > tint.blue, "the marker must retain P1's red tint");

        app.world_mut().resource_mut::<MatchState>().phase = MatchPhase::Results;
        app.update();
        assert_eq!(
            app.world().get::<Visibility>(handles[0].0),
            Some(&Visibility::Hidden)
        );
        let mut aims = app
            .world_mut()
            .query_filtered::<&FighterAimState, With<Fighter>>();
        assert!(aims.iter(app.world()).all(|aim| aim.marker_opacity == 0.0));
    }
}
