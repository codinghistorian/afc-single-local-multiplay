use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::RenderLayers;
use bevy::gltf::GltfAssetLabel;
use bevy::math::EulerRot;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::scene::SceneInstanceReady;
use serde::Deserialize;
use std::collections::HashMap;
use std::f32::consts::{PI, TAU};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use std::fs;
use std::sync::OnceLock;

use crate::arena_barriers::ArenaBarrierDefinition;
use crate::arena_defs::{
    ArenaBackgroundDefinition, ArenaDefinition, ArenaGroundShape, ArenaHazardDefinition,
    ArenaHazardKind, ArenaPipePairDefinition, ArenaVisualTheme, CRANK_PIPE_VISUAL_SCALE,
    PlatformDefinition, TRAINING_GROUND_ARENA_INDEX, active_arena_definition, active_arena_index,
    arena_definitions,
};
use crate::arena_prop_colliders::{
    LocalPropBarrier, PropBarrierBehavior, WorldPropBarrier, prop_collision_profile,
};
use crate::camera::ArenaCamera;
use crate::combat::{
    DamageDefenderProfile, HitEffects, ImpactFeedbackIntensity, ImpactProfile, ImpactSource,
    NEUTRAL_IMPACT_OWNER_ID, apply_impact, can_receive_impact, impact_profile,
};
use crate::components::{
    Fighter, FighterAction, FighterActionState, FighterInput, FighterMotor, FighterStats,
};
#[cfg(test)]
use crate::constants::ARENA_RADIUS;
use crate::constants::{
    ARENA_HEIGHT, ARENA_TOP_Y, FIGHTER_COUNT, FIGHTER_RADIUS, GRAVITY, LEDGE_SUPPORT_GRACE_MAX,
    LEDGE_SUPPORT_GRACE_SCALE,
};
use crate::controller_haptics::CombatHapticQueue;
use crate::effects::{EffectAssets, spawn_burning_fighter_effect, spawn_machine_scratch};
use crate::equipment::FighterEquipment;
use crate::feel::CombatFeelTuning;
use crate::game_state::{Hitstop, MatchState, MatchTelemetry};
use crate::reactions::ReactionFamilyId;
use crate::styles::FighterStyle;
use crate::techniques::DamageElement;

const ARENA_HAZARD_PULSE_DAMAGE: f32 = 7.0;
const ARENA_HAZARD_PULSE_KNOCKBACK: f32 = 5.8;
const ARENA_HAZARD_SNARE_DAMAGE: f32 = 3.0;
const ARENA_HAZARD_SNARE_KNOCKBACK: f32 = 2.2;
const ARENA_HAZARD_BUMPER_DAMAGE: f32 = 9.0;
const ARENA_HAZARD_BUMPER_KNOCKBACK: f32 = 7.6;
const ARENA_HAZARD_CAMPFIRE_DAMAGE: f32 = 4.0;
const ARENA_HAZARD_CAMPFIRE_KNOCKBACK: f32 = 8.8;
const ARENA_HAZARD_CAMPFIRE_LAUNCH: f32 = 4.6;
const ARENA_HAZARD_CAMPFIRE_BURN_SECONDS: f32 = 1.35;
const ARENA_HAZARD_SAW_DAMAGE: f32 = 5.0;
const ARENA_HAZARD_SAW_KNOCKBACK: f32 = 12.0;
const ARENA_HAZARD_SAW_LAUNCH: f32 = 4.8;
const PIPE_ENTRY_DWELL_SECONDS: f32 = 0.25;
const PIPE_ENTER_SECONDS: f32 = 0.32;
const PIPE_TRAVEL_SECONDS: f32 = 0.12;
const PIPE_EXIT_SECONDS: f32 = 0.34;
const PIPE_REENTRY_COOLDOWN_SECONDS: f32 = 0.9;
const PIPE_SINK_DEPTH: f32 = 1.15;
const PIPE_EXIT_CLEARANCE_RADIUS: f32 = 1.05;
const PIPE_EXIT_HOP_SPEED: f32 = 3.2;
const PIPE_EXIT_INWARD_SPEED: f32 = 2.4;
const MINI_ARENA_ASSET_ROOT: &str = "arena/kenney_mini_arena";
const ARENA_KIT_ASSET_ROOT: &str = "arena/kits";
const MINI_ARENA_FLOOR_SPACING: f32 = 1.6;
const MINI_ARENA_FLOOR_SCALE: f32 = 1.62;
const CHAMPIONS_COURT_ARENA_INDEX: usize = 0;
const CRANK_YARD_ARENA_INDEX: usize = 3;
const VENT_SPIRAL_ARENA_INDEX: usize = 4;
const POWDER_KEG_ARENA_INDEX: usize = 9;
const CRANK_SAW_VISUAL_Y: f32 = ARENA_TOP_Y + 0.72;
const CRANK_LEVER_POSITION: Vec3 = Vec3::new(6.7, ARENA_TOP_Y, 1.7);
const CRANK_LEVER_ATTACK_RADIUS: f32 = 1.85;
const POWDER_CANNON_INTERVAL_SECONDS: f32 = 2.6;
const POWDER_CANNON_BOMB_DAMAGE: f32 = 9.0;
const POWDER_CANNON_BOMB_RADIUS: f32 = 1.05;
const CHAMPIONS_COURT_RON_PATH: &str = "assets/maps/champions_court.ron";
const TRAINING_GROUND_RON_PATH: &str = "assets/maps/training_ground.ron";
const CHAMPIONS_COURT_LIGHT_SCALE: f32 = 1_000.0;
const CHAMPIONS_COURT_MAP_LIGHTS_ENABLED: bool = false;
const PLATFORM_SIDE_COLLISION_MIN_TOP_Y: f32 = ARENA_TOP_Y + 0.08;
const ARENA_GROUND_DEPTH_BIAS_BASE: f32 = -2_048.0;
const ARENA_GROUND_DEPTH_BIAS_STEP: f32 = 128.0;
const ARENA_PLATFORM_DEPTH_BIAS_BASE: f32 = -768.0;
const ARENA_PLATFORM_DEPTH_BIAS_STEP: f32 = 64.0;
const ARENA_PROP_SURFACE_CLEARANCE: f32 = 0.012;
pub(crate) const ARENA_PREVIEW_RENDER_LAYER: usize = 21;

#[derive(Component)]
pub struct ArenaGeometry;

#[derive(Component)]
pub(crate) struct ArenaGlobalDirectionalLight;

#[derive(Component)]
pub(crate) struct ArenaGlobalPointLight;

fn arena_geometry_render_layers() -> RenderLayers {
    RenderLayers::from_layers(&[0, ARENA_PREVIEW_RENDER_LAYER])
}

fn apply_arena_geometry_render_layers(
    scene_ready: On<SceneInstanceReady>,
    children: Query<&Children>,
    mut commands: Commands,
) {
    commands
        .entity(scene_ready.entity)
        .insert(arena_geometry_render_layers());
    for descendant in children.iter_descendants(scene_ready.entity) {
        commands
            .entity(descendant)
            .insert(arena_geometry_render_layers());
    }
}

pub fn sync_arena_preview_render_layers(
    mut commands: Commands,
    children: Query<&Children>,
    geometry: Query<Entity, (Added<ArenaGeometry>, Without<ArenaBackgroundWallpaper>)>,
) {
    for entity in &geometry {
        commands
            .entity(entity)
            .insert(arena_geometry_render_layers())
            .observe(apply_arena_geometry_render_layers);
        for descendant in children.iter_descendants(entity) {
            commands
                .entity(descendant)
                .insert(arena_geometry_render_layers());
        }
    }
}

#[derive(Component)]
pub(crate) struct ArenaBackgroundWallpaper(ArenaBackgroundDefinition);

#[derive(Component)]
pub struct ArenaHazardMarker {
    kind: ArenaHazardKind,
    pulse_seconds: f32,
    phase: f32,
    base_scale: f32,
    base_y: f32,
}

#[derive(Component)]
pub struct ArenaCampfireFlame {
    base_scale: Vec3,
    phase: f32,
}

#[derive(Component)]
pub struct ArenaDecorativeFlame {
    base_scale: Vec3,
    phase: f32,
}

#[derive(Component)]
pub struct ArenaPipePortalRing {
    endpoint: usize,
    phase: f32,
    base_scale: Vec3,
}

#[derive(Component)]
pub struct ArenaPipePortalParticle {
    endpoint: usize,
    phase: f32,
    radius: f32,
    base_y: f32,
}

#[derive(Component)]
pub struct ArenaSawBladeVisual {
    spin_speed: f32,
}

#[derive(Component)]
pub struct ArenaSawWarningLight {
    phase: f32,
    base_scale: Vec3,
}

#[derive(Component)]
pub struct ArenaSawAmbientSpark {
    center: Vec3,
    phase: f32,
}

#[derive(Component)]
pub(crate) struct CrankLeverVisual {
    running_rotation: Quat,
    stopped_rotation: Quat,
}

#[derive(Component)]
pub(crate) struct ArenaCannonBomb {
    velocity: Vec3,
    lifetime: f32,
}

#[derive(Component)]
pub struct ArenaVentRotor {
    pulse_seconds: f32,
    phase: f32,
    spin_direction: f32,
}

#[derive(Component)]
pub struct ArenaVentWarning {
    pulse_seconds: f32,
    phase: f32,
    base_scale: Vec3,
}

#[derive(Component)]
pub struct ArenaVentPlume {
    pulse_seconds: f32,
    phase: f32,
    base_y: f32,
    full_height: f32,
    base_scale: Vec3,
}

#[derive(Component)]
pub struct ArenaVentUfo {
    base_y: f32,
}

#[derive(Component)]
pub struct ArenaVentUfoBeam {
    base_y: f32,
    base_scale: Vec3,
}

#[derive(Component, Clone, Copy, Debug)]
pub struct ArenaFighterBurn {
    remaining: f32,
    duration: f32,
}

impl ArenaFighterBurn {
    fn new(duration: f32) -> Self {
        Self {
            remaining: duration,
            duration,
        }
    }

    pub fn visual_amount(self) -> f32 {
        let fade = (self.remaining / self.duration.max(0.01)).clamp(0.0, 1.0);
        let flicker = 0.76 + (self.remaining * 19.0).sin().abs() * 0.24;
        fade.sqrt() * flicker
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum FighterPipeState {
    Ready {
        candidate: Option<usize>,
        dwell: f32,
        cooldown: f32,
    },
    Transit {
        source: usize,
        destination: usize,
        elapsed: f32,
        entry_y: f32,
        base_scale: Vec3,
    },
}

impl Default for FighterPipeState {
    fn default() -> Self {
        Self::Ready {
            candidate: None,
            dwell: 0.0,
            cooldown: 0.0,
        }
    }
}

#[derive(Resource)]
pub struct ArenaPipeState {
    arena_index: usize,
    fighters: [FighterPipeState; FIGHTER_COUNT],
}

impl ArenaPipeState {
    fn new(arena_index: usize) -> Self {
        Self {
            arena_index,
            fighters: [FighterPipeState::default(); FIGHTER_COUNT],
        }
    }

    fn sync_to_arena(&mut self, arena_index: usize) {
        if self.arena_index != arena_index {
            *self = Self::new(arena_index);
        }
    }

    fn endpoint_active(&self, endpoint: usize) -> bool {
        self.fighters.iter().any(|state| {
            matches!(
                state,
                FighterPipeState::Transit {
                    source,
                    destination,
                    ..
                } if *source == endpoint || *destination == endpoint
            )
        })
    }
}

#[allow(dead_code)]
#[derive(Clone)]
struct AuthoredFloorRenderAsset {
    mesh: Handle<Mesh>,
    material: Handle<StandardMaterial>,
}

#[derive(Clone, Copy)]
struct ArenaAssetProp {
    name: &'static str,
    file: &'static str,
    x: f32,
    y: f32,
    z: f32,
    yaw: f32,
    scale: f32,
}

#[derive(Clone, Copy)]
struct ArenaThemePalette {
    primary: Color,
    secondary: Color,
    trim: Color,
    hazard: Color,
}

impl ArenaAssetProp {
    fn transform(self) -> Transform {
        Transform::from_xyz(self.x, self.y + ARENA_PROP_SURFACE_CLEARANCE, self.z)
            .with_rotation(Quat::from_rotation_y(self.yaw))
            .with_scale(Vec3::splat(self.scale))
    }

    fn collision_barriers(self) -> impl Iterator<Item = WorldPropBarrier> {
        prop_collision_profile(self.file)
            .iter()
            .copied()
            .map(move |barrier: LocalPropBarrier| {
                barrier.to_world(
                    Vec3::new(self.x, self.y + ARENA_PROP_SURFACE_CLEARANCE, self.z),
                    self.yaw,
                    self.scale,
                )
            })
    }
}

#[derive(Debug, Deserialize)]
struct AuthoredArenaRon {
    map: AuthoredArenaMap,
    assets: HashMap<String, String>,
    #[serde(default)]
    floor_shapes: Vec<AuthoredFloorShape>,
    #[serde(default)]
    prefabs: HashMap<String, Vec<AuthoredObject>>,
    #[serde(default)]
    instances: Vec<AuthoredObject>,
    #[serde(default)]
    prefab_instances: Vec<AuthoredPrefabInstance>,
    #[serde(default)]
    lights: Vec<AuthoredLight>,
    #[serde(default)]
    materials: HashMap<String, AuthoredMaterial>,
    #[serde(default)]
    primitives: Vec<AuthoredPrimitive>,
    #[serde(default)]
    primitive_prefabs: HashMap<String, Vec<AuthoredPrimitive>>,
    #[serde(default)]
    primitive_prefab_instances: Vec<AuthoredPrimitivePrefabInstance>,
    #[serde(default)]
    floor_rows: Vec<AuthoredFloorRow>,
    #[serde(default)]
    floor_pattern: Option<AuthoredFloorPattern>,
    #[serde(default)]
    floor_materials: Vec<String>,
    #[serde(default)]
    colliders: Vec<AuthoredCollider>,
}

#[derive(Debug, Deserialize)]
struct AuthoredArenaMap {
    tile_size: f32,
    #[serde(default)]
    floor_width: f32,
    #[serde(default)]
    floor_depth: f32,
}

#[derive(Debug, Deserialize)]
struct AuthoredFloorShape {
    id: String,
    kind: String,
    asset: String,
    center: (i32, i32),
    #[serde(default)]
    radius_tiles: i32,
    #[serde(default)]
    inner_radius_tiles: i32,
    #[serde(default)]
    outer_radius_tiles: i32,
    #[serde(default)]
    size_tiles: (i32, i32),
    #[serde(default)]
    y: f32,
    #[serde(default)]
    rotation_y: f32,
}

#[derive(Clone, Debug, Deserialize)]
struct AuthoredObject {
    #[serde(default)]
    id: String,
    asset: String,
    position: (f32, f32, f32),
    #[serde(default)]
    rotation_y: f32,
    #[serde(default = "unit_tuple3")]
    scale: (f32, f32, f32),
}

#[derive(Debug, Deserialize)]
struct AuthoredPrefabInstance {
    id: String,
    prefab: String,
    position: (f32, f32, f32),
    #[serde(default)]
    rotation_y: f32,
    #[serde(default = "unit_tuple3")]
    scale: (f32, f32, f32),
}

#[derive(Debug, Deserialize)]
struct AuthoredLight {
    id: String,
    kind: String,
    #[serde(default)]
    position: (f32, f32, f32),
    #[serde(default)]
    rotation_euler_degrees: (f32, f32, f32),
    #[serde(default = "white_tuple3")]
    color: (f32, f32, f32),
    #[serde(default)]
    intensity: f32,
    #[serde(default)]
    illuminance: f32,
    #[serde(default)]
    range: f32,
    #[serde(default)]
    shadows: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct AuthoredMaterial {
    color: (f32, f32, f32),
    #[serde(default)]
    emissive: (f32, f32, f32),
    #[serde(default = "default_roughness")]
    roughness: f32,
    #[serde(default)]
    metallic: f32,
}

#[derive(Clone, Debug, Deserialize)]
struct AuthoredPrimitive {
    #[serde(default)]
    id: String,
    kind: String,
    position: (f32, f32, f32),
    #[serde(default)]
    rotation_y: f32,
    #[serde(default = "unit_tuple3")]
    scale: (f32, f32, f32),
    material: String,
    #[serde(default)]
    effect: String,
}

#[derive(Debug, Deserialize)]
struct AuthoredPrimitivePrefabInstance {
    id: String,
    prefab: String,
    position: (f32, f32, f32),
    #[serde(default)]
    rotation_y: f32,
    #[serde(default = "unit_tuple3")]
    scale: (f32, f32, f32),
}

#[derive(Debug, Deserialize)]
struct AuthoredFloorRow {
    z: f32,
    depth: f32,
    #[serde(default)]
    offset: f32,
    widths: Vec<f32>,
}

#[derive(Debug, Deserialize)]
struct AuthoredFloorPattern {
    rows: usize,
    columns: usize,
    gap: f32,
    bevel: f32,
}

#[derive(Default)]
struct AuthoredFloorMeshBuffers {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<u32>,
}

#[derive(Debug, Deserialize)]
struct AuthoredCollider {
    id: String,
    center: (f32, f32),
    half_extents: (f32, f32),
    #[serde(default)]
    rotation_y: f32,
    top_y: f32,
}

#[derive(Resource)]
pub struct ArenaScene {
    index: usize,
}

#[derive(Resource)]
pub struct ArenaHazardState {
    arena_index: usize,
    elapsed: f32,
    hit_cooldowns: Vec<[f32; FIGHTER_COUNT]>,
    crank_saws_stopped: bool,
    crank_lever_toggle_cooldown: f32,
}

impl ArenaHazardState {
    fn new(arena_index: usize, hazard_count: usize) -> Self {
        Self {
            arena_index,
            elapsed: 0.0,
            hit_cooldowns: vec![[0.0; FIGHTER_COUNT]; hazard_count],
            crank_saws_stopped: false,
            crank_lever_toggle_cooldown: 0.0,
        }
    }

    fn sync_to_arena(&mut self, arena_index: usize, hazard_count: usize) {
        if self.arena_index == arena_index && self.hit_cooldowns.len() == hazard_count {
            return;
        }

        *self = Self::new(arena_index, hazard_count);
    }

    fn tick_cooldowns(&mut self, dt: f32) {
        for hazard_cooldowns in &mut self.hit_cooldowns {
            for cooldown in hazard_cooldowns {
                *cooldown = (*cooldown - dt).max(0.0);
            }
        }
    }

    pub fn elapsed(&self) -> f32 {
        self.elapsed
    }
}

#[derive(Resource)]
pub(crate) struct ArenaOrdnanceAssets {
    bomb_mesh: Handle<Mesh>,
    bomb_material: Handle<StandardMaterial>,
}

#[derive(Resource)]
pub(crate) struct PowderKegCannonState {
    arena_index: usize,
    fire_timer: f32,
    next_cannon: usize,
}

impl PowderKegCannonState {
    fn new(arena_index: usize) -> Self {
        Self {
            arena_index,
            fire_timer: 0.8,
            next_cannon: 0,
        }
    }

    fn sync_to_arena(&mut self, arena_index: usize) {
        if self.arena_index != arena_index {
            *self = Self::new(arena_index);
        }
    }
}

pub fn setup_arena(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let arena = active_arena_definition();
    commands.insert_resource(ArenaScene {
        index: active_arena_index(),
    });
    commands.insert_resource(ArenaHazardState::new(
        active_arena_index(),
        arena.hazards.len(),
    ));
    commands.insert_resource(ArenaPipeState::new(active_arena_index()));
    commands.insert_resource(PowderKegCannonState::new(active_arena_index()));
    commands.insert_resource(ArenaOrdnanceAssets {
        bomb_mesh: meshes.add(Sphere::new(0.34).mesh().uv(14, 8)),
        bomb_material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.08, 0.07, 0.065),
            emissive: LinearRgba::rgb(0.5, 0.11, 0.015),
            metallic: 0.48,
            perceptual_roughness: 0.34,
            ..default()
        }),
    });
    spawn_arena_geometry(&mut commands, &asset_server, &mut meshes, &mut materials);
    spawn_arena_lights(&mut commands);
}

pub fn sync_arena_visuals(
    mut commands: Commands,
    mut scene: ResMut<ArenaScene>,
    geometry: Query<Entity, With<ArenaGeometry>>,
    asset_server: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let selected = active_arena_index();
    if scene.index == selected {
        return;
    }

    for entity in &geometry {
        commands.entity(entity).despawn();
    }
    scene.index = selected;
    spawn_arena_geometry(&mut commands, &asset_server, &mut meshes, &mut materials);
}

fn spawn_arena_geometry(
    commands: &mut Commands,
    asset_server: &AssetServer,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) {
    let arena = active_arena_definition();
    if arena.background.gameplay_visible {
        spawn_arena_background(
            commands,
            asset_server,
            meshes,
            materials,
            arena.background,
            arena.camera_offset,
        );
    }

    let arena_index = active_arena_index();
    let palette = arena_theme_palette(arena.visual_theme);
    let hazard_material = materials.add(StandardMaterial {
        base_color: palette.hazard.with_alpha(0.34),
        emissive: LinearRgba::from(palette.hazard) * 0.16,
        alpha_mode: AlphaMode::Blend,
        depth_bias: 16.0,
        ..default()
    });

    if matches!(
        arena_index,
        CHAMPIONS_COURT_ARENA_INDEX | TRAINING_GROUND_ARENA_INDEX
    ) {
        match spawn_authored_arena_map(commands, asset_server, meshes, materials, arena_index) {
            Ok(()) => {
                spawn_arena_hazard_markers(
                    commands,
                    meshes,
                    hazard_material,
                    arena_index,
                    arena.hazards,
                );
                return;
            }
            Err(error) => {
                warn!("Could not load authored scene for {}: {error}", arena.name);
            }
        }
    }

    let primary = materials.add(StandardMaterial {
        base_color: palette.primary,
        perceptual_roughness: 0.9,
        ..default()
    });
    let secondary = materials.add(StandardMaterial {
        base_color: palette.secondary,
        perceptual_roughness: 0.84,
        ..default()
    });
    let trim = materials.add(StandardMaterial {
        base_color: palette.trim,
        metallic: matches!(
            arena.visual_theme,
            ArenaVisualTheme::Industrial | ArenaVisualTheme::Reactor
        )
        .then_some(0.22)
        .unwrap_or(0.02),
        perceptual_roughness: 0.64,
        ..default()
    });

    spawn_arena_ground_shapes(
        commands,
        meshes,
        materials,
        primary.clone(),
        secondary.clone(),
        arena,
    );
    if arena_index == VENT_SPIRAL_ARENA_INDEX {
        spawn_vent_spiral_platform_blocks(commands, meshes, materials, arena.platforms);
    } else {
        spawn_platform_blocks(commands, meshes, materials, secondary, arena.platforms);
    }
    spawn_arena_theme_accents(commands, meshes, trim, arena.visual_theme);
    spawn_arena_hazard_markers(
        commands,
        meshes,
        hazard_material,
        arena_index,
        arena.hazards,
    );
    spawn_campfire_props(commands, meshes, materials, arena.hazards);
    spawn_mini_arena_props(commands, asset_server, arena_index);
    spawn_vent_spiral_machinery(
        commands,
        asset_server,
        meshes,
        materials,
        arena_index,
        arena.hazards,
    );
    spawn_crank_yard_machinery(
        commands,
        asset_server,
        meshes,
        materials,
        arena_index,
        arena.hazards,
    );
}

fn arena_theme_palette(theme: ArenaVisualTheme) -> ArenaThemePalette {
    match theme {
        ArenaVisualTheme::Crown => ArenaThemePalette {
            primary: Color::srgb(0.66, 0.57, 0.42),
            secondary: Color::srgb(0.82, 0.76, 0.59),
            trim: Color::srgb(0.76, 0.08, 0.055),
            hazard: Color::srgb(0.1, 0.8, 0.65),
        },
        ArenaVisualTheme::Causeway => ArenaThemePalette {
            primary: Color::srgb(0.36, 0.55, 0.34),
            secondary: Color::srgb(0.48, 0.34, 0.2),
            trim: Color::srgb(0.86, 0.67, 0.24),
            hazard: Color::srgb(0.98, 0.28, 0.08),
        },
        ArenaVisualTheme::Terrace => ArenaThemePalette {
            primary: Color::srgb(0.58, 0.62, 0.36),
            secondary: Color::srgb(0.76, 0.61, 0.35),
            trim: Color::srgb(0.86, 0.42, 0.16),
            hazard: Color::srgb(0.8, 0.24, 0.16),
        },
        ArenaVisualTheme::Industrial => ArenaThemePalette {
            primary: Color::srgb(0.31, 0.35, 0.38),
            secondary: Color::srgb(0.48, 0.52, 0.52),
            trim: Color::srgb(0.96, 0.66, 0.12),
            hazard: Color::srgb(0.98, 0.28, 0.12),
        },
        ArenaVisualTheme::Reactor => ArenaThemePalette {
            primary: Color::srgb(0.16, 0.22, 0.21),
            secondary: Color::srgb(0.42, 0.47, 0.46),
            trim: Color::srgb(0.18, 0.9, 0.78),
            hazard: Color::srgb(1.0, 0.32, 0.12),
        },
        ArenaVisualTheme::Toybox => ArenaThemePalette {
            primary: Color::srgb(0.2, 0.55, 0.86),
            secondary: Color::srgb(0.96, 0.38, 0.2),
            trim: Color::srgb(1.0, 0.82, 0.18),
            hazard: Color::srgb(1.0, 0.32, 0.52),
        },
        ArenaVisualTheme::Market => ArenaThemePalette {
            primary: Color::srgb(0.72, 0.52, 0.3),
            secondary: Color::srgb(0.4, 0.61, 0.38),
            trim: Color::srgb(0.9, 0.18, 0.13),
            hazard: Color::srgb(0.96, 0.64, 0.12),
        },
        ArenaVisualTheme::Garden => ArenaThemePalette {
            primary: Color::srgb(0.34, 0.63, 0.3),
            secondary: Color::srgb(0.55, 0.72, 0.4),
            trim: Color::srgb(0.94, 0.48, 0.62),
            hazard: Color::srgb(0.63, 0.24, 0.76),
        },
        ArenaVisualTheme::Snow => ArenaThemePalette {
            primary: Color::srgb(0.84, 0.92, 0.94),
            secondary: Color::srgb(0.49, 0.72, 0.79),
            trim: Color::srgb(0.92, 0.24, 0.2),
            hazard: Color::srgb(0.2, 0.72, 0.96),
        },
        ArenaVisualTheme::Powder => ArenaThemePalette {
            primary: Color::srgb(0.43, 0.32, 0.23),
            secondary: Color::srgb(0.24, 0.25, 0.25),
            trim: Color::srgb(0.9, 0.55, 0.12),
            hazard: Color::srgb(0.96, 0.2, 0.08),
        },
        ArenaVisualTheme::Training => ArenaThemePalette {
            primary: Color::srgb(0.62, 0.43, 0.25),
            secondary: Color::srgb(0.82, 0.62, 0.38),
            trim: Color::srgb(0.95, 0.58, 0.2),
            hazard: Color::srgb(1.0, 0.32, 0.06),
        },
    }
}

fn spawn_arena_ground_shapes(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    primary: Handle<StandardMaterial>,
    secondary: Handle<StandardMaterial>,
    arena: &ArenaDefinition,
) {
    for (index, shape) in arena.ground_shapes.iter().enumerate() {
        let base_material = if index % 2 == 0 { &primary } else { &secondary };
        let material =
            material_with_depth_bias(materials, base_material, arena_ground_depth_bias(index));
        let (mesh, transform) = match *shape {
            ArenaGroundShape::Circle {
                center,
                radius,
                top_y,
            } => (
                meshes.add(Cylinder::new(radius, ARENA_HEIGHT)),
                Transform::from_xyz(center.x, top_y - ARENA_HEIGHT * 0.5, center.y),
            ),
            ArenaGroundShape::Rectangle {
                center,
                half_extents,
                yaw,
                top_y,
            } => (
                meshes.add(Cuboid::new(
                    half_extents.x * 2.0,
                    ARENA_HEIGHT,
                    half_extents.y * 2.0,
                )),
                Transform::from_xyz(center.x, top_y - ARENA_HEIGHT * 0.5, center.y)
                    .with_rotation(Quat::from_rotation_y(yaw)),
            ),
        };
        commands.spawn((
            Mesh3d(mesh),
            MeshMaterial3d(material),
            transform,
            Name::new(format!("{} ground {index}", arena.name)),
            ArenaGeometry,
        ));
    }
}

fn spawn_platform_blocks(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    material: Handle<StandardMaterial>,
    platforms: &[PlatformDefinition],
) {
    for (index, platform) in platforms.iter().enumerate() {
        let height = ARENA_HEIGHT + (platform.top_y - ARENA_TOP_Y).max(0.0);
        let platform_material =
            material_with_depth_bias(materials, &material, arena_platform_depth_bias(index));
        commands.spawn((
            Mesh3d(meshes.add(platform.block_mesh(height))),
            MeshMaterial3d(platform_material),
            platform.block_transform(height),
            Name::new(format!("Arena platform {index}")),
            ArenaGeometry,
        ));
    }
}

fn spawn_vent_spiral_platform_blocks(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    platforms: &[PlatformDefinition],
) {
    let tier_materials = [
        materials.add(StandardMaterial {
            base_color: Color::srgb(0.34, 0.39, 0.38),
            metallic: 0.18,
            perceptual_roughness: 0.72,
            ..default()
        }),
        materials.add(StandardMaterial {
            base_color: Color::srgb(0.25, 0.38, 0.46),
            metallic: 0.16,
            perceptual_roughness: 0.7,
            ..default()
        }),
        materials.add(StandardMaterial {
            base_color: Color::srgb(0.25, 0.43, 0.34),
            metallic: 0.14,
            perceptual_roughness: 0.74,
            ..default()
        }),
        materials.add(StandardMaterial {
            base_color: Color::srgb(0.5, 0.46, 0.35),
            metallic: 0.12,
            perceptual_roughness: 0.76,
            ..default()
        }),
    ];

    for (index, platform) in platforms.iter().enumerate() {
        let tier = (((platform.top_y - ARENA_TOP_Y) / 0.65).round() as usize).min(3);
        let height = ARENA_HEIGHT + (platform.top_y - ARENA_TOP_Y).max(0.0);
        let material = material_with_depth_bias(
            materials,
            &tier_materials[tier],
            arena_platform_depth_bias(index),
        );
        commands.spawn((
            Mesh3d(meshes.add(platform.block_mesh(height))),
            MeshMaterial3d(material),
            platform.block_transform(height),
            Name::new(format!("Vent spiral tier {tier} block {index}")),
            ArenaGeometry,
        ));
    }

    spawn_vent_spiral_transition_marks(commands, meshes, materials);
}

fn spawn_vent_spiral_transition_marks(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
) {
    let warning_material = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.62, 0.08),
        emissive: LinearRgba::rgb(0.45, 0.19, 0.015),
        metallic: 0.12,
        perceptual_roughness: 0.58,
        ..default()
    });
    let marker_mesh = meshes.add(Cuboid::new(0.42, 0.045, 0.13));
    let transitions = [
        (Vec3::new(4.15, ARENA_TOP_Y + 0.678, 3.16), 0.0),
        (Vec3::new(-3.9, ARENA_TOP_Y + 1.328, 3.56), PI * 0.5),
        (Vec3::new(-3.75, ARENA_TOP_Y + 1.978, -2.48), 0.0),
    ];

    for (transition_index, (center, yaw)) in transitions.into_iter().enumerate() {
        for stripe in -1..=1 {
            let offset = Quat::from_rotation_y(yaw) * Vec3::new(stripe as f32 * 0.52, 0.0, 0.0);
            commands.spawn((
                Mesh3d(marker_mesh.clone()),
                MeshMaterial3d(warning_material.clone()),
                Transform::from_translation(center + offset)
                    .with_rotation(Quat::from_rotation_y(yaw)),
                Name::new(format!(
                    "Vent spiral jump marker {transition_index}-{stripe}"
                )),
                ArenaGeometry,
            ));
        }
    }
}

fn material_with_depth_bias(
    materials: &mut Assets<StandardMaterial>,
    source: &Handle<StandardMaterial>,
    depth_bias: f32,
) -> Handle<StandardMaterial> {
    let mut material = materials
        .get(source)
        .cloned()
        .expect("arena base material should exist before geometry is spawned");
    material.depth_bias = depth_bias;
    materials.add(material)
}

fn arena_ground_depth_bias(index: usize) -> f32 {
    ARENA_GROUND_DEPTH_BIAS_BASE + index as f32 * ARENA_GROUND_DEPTH_BIAS_STEP
}

fn arena_platform_depth_bias(index: usize) -> f32 {
    ARENA_PLATFORM_DEPTH_BIAS_BASE + index as f32 * ARENA_PLATFORM_DEPTH_BIAS_STEP
}

fn spawn_arena_theme_accents(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    trim: Handle<StandardMaterial>,
    theme: ArenaVisualTheme,
) {
    let (positions, size) = match theme {
        ArenaVisualTheme::Causeway => (&[(-4.7, 0.0), (4.7, 0.0)][..], Vec2::new(0.16, 11.5)),
        ArenaVisualTheme::Terrace => (&[(-2.2, 1.45), (2.2, -1.45)][..], Vec2::new(3.6, 0.12)),
        ArenaVisualTheme::Industrial => (&[(0.0, -1.8), (0.0, 1.8)][..], Vec2::new(13.0, 0.16)),
        ArenaVisualTheme::Reactor => return,
        ArenaVisualTheme::Toybox => (&[(-2.8, 0.0), (2.8, 0.0)][..], Vec2::new(0.2, 15.2)),
        ArenaVisualTheme::Market => (&[(0.0, 0.0)][..], Vec2::new(8.8, 0.18)),
        ArenaVisualTheme::Garden => (&[(-3.4, 0.0), (3.4, 0.0)][..], Vec2::new(0.12, 2.8)),
        ArenaVisualTheme::Snow => (&[(-2.9, -2.3), (2.9, 2.3)][..], Vec2::new(3.4, 0.14)),
        ArenaVisualTheme::Powder => (&[(-3.8, 0.0), (3.8, 0.0)][..], Vec2::new(0.18, 12.0)),
        ArenaVisualTheme::Crown => return,
        ArenaVisualTheme::Training => return,
    };

    for (x, z) in positions {
        commands.spawn((
            Mesh3d(meshes.add(Cuboid::new(size.x, 0.035, size.y))),
            MeshMaterial3d(trim.clone()),
            Transform::from_xyz(*x, ARENA_TOP_Y + 0.025, *z),
            ArenaGeometry,
        ));
    }
}

fn arena_background_wallpaper_size(background: ArenaBackgroundDefinition) -> Vec2 {
    Vec2::new(
        background.world_height * background.image_size.x / background.image_size.y,
        background.world_height,
    )
}

fn arena_background_wallpaper_transform(
    background: ArenaBackgroundDefinition,
    camera_transform: &Transform,
) -> Transform {
    Transform::from_translation(
        camera_transform.translation + camera_transform.forward() * background.distance,
    )
    .with_rotation(camera_transform.rotation)
}

fn spawn_arena_background(
    commands: &mut Commands,
    asset_server: &AssetServer,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    background: ArenaBackgroundDefinition,
    camera_offset: Vec3,
) {
    let size = arena_background_wallpaper_size(background);
    let camera_transform =
        Transform::from_translation(camera_offset).looking_at(Vec3::Y * 0.6, Vec3::Y);
    let material = materials.add(StandardMaterial {
        base_color_texture: Some(asset_server.load(background.asset_path)),
        unlit: true,
        cull_mode: None,
        perceptual_roughness: 1.0,
        ..default()
    });

    commands.spawn((
        Mesh3d(meshes.add(Rectangle::new(size.x, size.y))),
        MeshMaterial3d(material),
        arena_background_wallpaper_transform(background, &camera_transform),
        ArenaBackgroundWallpaper(background),
        Name::new("Arena scenic wallpaper"),
        ArenaGeometry,
    ));
}

pub fn sync_arena_background_to_camera(
    camera: Query<&Transform, (With<ArenaCamera>, Without<ArenaBackgroundWallpaper>)>,
    mut backgrounds: Query<
        (&ArenaBackgroundWallpaper, &mut Transform),
        (Without<ArenaCamera>, With<ArenaGeometry>),
    >,
) {
    let Ok(camera_transform) = camera.single() else {
        return;
    };

    for (background, mut transform) in &mut backgrounds {
        *transform = arena_background_wallpaper_transform(background.0, camera_transform);
    }
}

fn spawn_authored_arena_map(
    commands: &mut Commands,
    asset_server: &AssetServer,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    arena_index: usize,
) -> Result<(), String> {
    let map = load_authored_arena_map(arena_index)?;
    let arena_name = arena_definitions()[arena_index].name;
    let mut scenes = HashMap::new();

    spawn_authored_floor_shapes(
        commands,
        asset_server,
        &map,
        &mut scenes,
        arena_index,
        arena_name,
    );

    for object in &map.instances {
        spawn_champions_object(
            commands,
            asset_server,
            &map,
            &mut scenes,
            &object.asset,
            champions_object_transform(object),
            champions_object_name(arena_name, &object.id, &object.asset),
        );
    }

    for prefab_instance in &map.prefab_instances {
        let Some(prefab) = map.prefabs.get(&prefab_instance.prefab) else {
            warn!(
                "{arena_name} prefab instance '{}' references missing prefab '{}'",
                prefab_instance.id, prefab_instance.prefab
            );
            continue;
        };

        for object in prefab {
            spawn_champions_object(
                commands,
                asset_server,
                &map,
                &mut scenes,
                &object.asset,
                champions_prefab_object_transform(prefab_instance, object),
                authored_prefab_object_name(arena_name, prefab_instance, object),
            );
        }
    }

    spawn_authored_primitives(commands, meshes, materials, &map, arena_name);

    if arena_index != CHAMPIONS_COURT_ARENA_INDEX || CHAMPIONS_COURT_MAP_LIGHTS_ENABLED {
        spawn_authored_lights(commands, &map.lights, arena_name);
    }

    Ok(())
}

fn load_authored_arena_map(arena_index: usize) -> Result<AuthoredArenaRon, String> {
    #[cfg(target_arch = "wasm32")]
    {
        let contents = match arena_index {
            CHAMPIONS_COURT_ARENA_INDEX => include_str!("../assets/maps/champions_court.ron"),
            TRAINING_GROUND_ARENA_INDEX => include_str!("../assets/maps/training_ground.ron"),
            _ => return Err(format!("arena {arena_index} has no authored scene")),
        };
        return ron::from_str(contents).map_err(|error| format!("RON parse failed: {error}"));
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    {
        let path = match arena_index {
            CHAMPIONS_COURT_ARENA_INDEX => CHAMPIONS_COURT_RON_PATH,
            TRAINING_GROUND_ARENA_INDEX => TRAINING_GROUND_RON_PATH,
            _ => return Err(format!("arena {arena_index} has no authored scene")),
        };
        let contents = fs::read_to_string(path).map_err(|error| format!("read failed: {error}"))?;
        ron::from_str(&contents).map_err(|error| format!("RON parse failed: {error}"))
    }
}

fn spawn_authored_floor_shapes(
    commands: &mut Commands,
    asset_server: &AssetServer,
    map: &AuthoredArenaRon,
    scenes: &mut HashMap<String, Handle<Scene>>,
    arena_index: usize,
    arena_name: &str,
) {
    for shape in &map.floor_shapes {
        let Some(scene) = champions_scene_handle(asset_server, map, scenes, &shape.asset) else {
            warn!(
                "{arena_name} floor shape '{}' references missing asset '{}'",
                shape.id, shape.asset
            );
            continue;
        };

        let scale = Vec3::splat(champions_floor_asset_scale(&shape.asset, map.map.tile_size));
        for tile in champions_floor_shape_render_positions(shape, map.map.tile_size, arena_index) {
            let x = tile.x;
            let z = tile.y;
            commands.spawn((
                SceneRoot(scene.clone()),
                Transform::from_xyz(x, champions_stage_y(shape.y), z)
                    .with_rotation(champions_yaw(shape.rotation_y))
                    .with_scale(scale),
                ArenaGeometry,
                Name::new(format!("{arena_name} floor {}", shape.id)),
            ));
        }
    }
}

#[allow(dead_code)]
fn champions_floor_render_asset(
    asset_server: &AssetServer,
    map: &AuthoredArenaRon,
    render_assets: &mut HashMap<String, AuthoredFloorRenderAsset>,
    asset_key: &str,
) -> Option<AuthoredFloorRenderAsset> {
    let path = champions_runtime_asset_path(&map.assets, asset_key)?;
    if let Some(asset) = render_assets.get(&path) {
        return Some(asset.clone());
    }

    let asset = AuthoredFloorRenderAsset {
        mesh: asset_server.load(
            GltfAssetLabel::Primitive {
                mesh: 0,
                primitive: 0,
            }
            .from_asset(path.clone()),
        ),
        material: asset_server.load(
            GltfAssetLabel::Material {
                index: 0,
                is_scale_inverted: false,
            }
            .from_asset(path.clone()),
        ),
    };
    render_assets.insert(path, asset.clone());
    Some(asset)
}

fn spawn_champions_object(
    commands: &mut Commands,
    asset_server: &AssetServer,
    map: &AuthoredArenaRon,
    scenes: &mut HashMap<String, Handle<Scene>>,
    asset_key: &str,
    transform: Transform,
    name: String,
) {
    let Some(scene) = champions_scene_handle(asset_server, map, scenes, asset_key) else {
        warn!("Authored arena object '{name}' references missing asset '{asset_key}'");
        return;
    };

    commands.spawn((SceneRoot(scene), transform, ArenaGeometry, Name::new(name)));
}

fn champions_scene_handle(
    asset_server: &AssetServer,
    map: &AuthoredArenaRon,
    scenes: &mut HashMap<String, Handle<Scene>>,
    asset_key: &str,
) -> Option<Handle<Scene>> {
    let path = champions_runtime_asset_path(&map.assets, asset_key)?;
    if let Some(scene) = scenes.get(&path) {
        return Some(scene.clone());
    }

    let scene = asset_server.load(GltfAssetLabel::Scene(0).from_asset(path.clone()));
    scenes.insert(path, scene.clone());
    Some(scene)
}

fn champions_runtime_asset_path(
    assets: &HashMap<String, String>,
    asset_key: &str,
) -> Option<String> {
    assets
        .get(asset_key)
        .map(|file| format!("{MINI_ARENA_ASSET_ROOT}/{file}"))
}

fn champions_object_transform(object: &AuthoredObject) -> Transform {
    Transform::from_translation(champions_object_position(object.position))
        .with_rotation(champions_yaw(object.rotation_y))
        .with_scale(champions_scale(object.scale))
}

fn champions_prefab_object_transform(
    prefab_instance: &AuthoredPrefabInstance,
    object: &AuthoredObject,
) -> Transform {
    let parent_rotation = champions_yaw(prefab_instance.rotation_y);
    let child_rotation = champions_yaw(object.rotation_y);
    let parent_scale = champions_scale(prefab_instance.scale);
    let child_scale = champions_scale(object.scale);
    let parent_position = champions_raw_position(prefab_instance.position);
    let child_position = champions_raw_position(object.position);
    let translation = parent_position + parent_rotation * (child_position * parent_scale);

    Transform::from_translation(Vec3::new(
        translation.x,
        champions_stage_y(translation.y) + ARENA_PROP_SURFACE_CLEARANCE,
        translation.z,
    ))
    .with_rotation(parent_rotation * child_rotation)
    .with_scale(parent_scale * child_scale)
}

fn champions_object_name(prefix: &str, id: &str, asset_key: &str) -> String {
    if id.is_empty() {
        format!("{prefix} {asset_key}")
    } else {
        format!("{prefix} {id}")
    }
}

fn authored_prefab_object_name(
    arena_name: &str,
    prefab_instance: &AuthoredPrefabInstance,
    object: &AuthoredObject,
) -> String {
    if object.id.is_empty() {
        format!(
            "{arena_name} prefab {} {}",
            prefab_instance.id, object.asset
        )
    } else {
        format!("{arena_name} prefab {} {}", prefab_instance.id, object.id)
    }
}

fn spawn_authored_primitives(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    map: &AuthoredArenaRon,
    arena_name: &str,
) {
    let material_handles = map
        .materials
        .iter()
        .map(|(name, material)| {
            let emissive = material.emissive;
            let handle = materials.add(StandardMaterial {
                base_color: champions_color(material.color),
                emissive: LinearRgba::rgb(emissive.0, emissive.1, emissive.2),
                perceptual_roughness: material.roughness,
                metallic: material.metallic,
                ..default()
            });
            (name.clone(), handle)
        })
        .collect::<HashMap<_, _>>();
    let mut mesh_handles = HashMap::new();
    let mut ordinal = 0usize;

    spawn_authored_floor_pattern(commands, meshes, map, &material_handles, arena_name);
    spawn_authored_floor_rows(
        commands,
        meshes,
        map,
        &material_handles,
        &mut mesh_handles,
        arena_name,
    );

    for primitive in &map.primitives {
        spawn_authored_primitive(
            commands,
            meshes,
            primitive,
            authored_primitive_transform(primitive),
            &material_handles,
            &mut mesh_handles,
            format!("{arena_name} primitive {}", primitive.id),
            ordinal,
        );
        ordinal += 1;
    }

    for instance in &map.primitive_prefab_instances {
        let Some(prefab) = map.primitive_prefabs.get(&instance.prefab) else {
            warn!(
                "{arena_name} primitive prefab instance '{}' references missing prefab '{}'",
                instance.id, instance.prefab
            );
            continue;
        };

        for primitive in prefab {
            spawn_authored_primitive(
                commands,
                meshes,
                primitive,
                authored_primitive_prefab_transform(instance, primitive),
                &material_handles,
                &mut mesh_handles,
                format!("{arena_name} prefab {} {}", instance.id, primitive.id),
                ordinal,
            );
            ordinal += 1;
        }
    }
}

fn spawn_authored_floor_pattern(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    map: &AuthoredArenaRon,
    material_handles: &HashMap<String, Handle<StandardMaterial>>,
    arena_name: &str,
) {
    let Some(pattern) = &map.floor_pattern else {
        return;
    };
    if pattern.rows == 0 || pattern.columns == 0 || map.floor_materials.is_empty() {
        warn!("{arena_name} floor pattern needs rows, columns, and materials");
        return;
    }

    let floor_width = if map.map.floor_width > 0.0 {
        map.map.floor_width
    } else {
        16.0
    };
    let floor_depth = if map.map.floor_depth > 0.0 {
        map.map.floor_depth
    } else {
        12.0
    };
    let unit_width = floor_width / pattern.columns as f32;
    let unit_depth = floor_depth / pattern.rows as f32;
    let gap = pattern.gap.clamp(0.0, unit_width.min(unit_depth) * 0.38);
    let mut occupied = vec![false; pattern.rows * pattern.columns];
    let mut buffers = (0..map.floor_materials.len())
        .map(|_| AuthoredFloorMeshBuffers::default())
        .collect::<Vec<_>>();

    for row in 0..pattern.rows {
        for column in 0..pattern.columns {
            let index = row * pattern.columns + column;
            if occupied[index] {
                continue;
            }

            let roll = authored_floor_hash(row, column, 0) % 100;
            let requested_span = match roll {
                0 => (3, 3),
                1..=2 => (3, 2),
                3..=7 => (2, 2),
                8..=18 => (2, 1),
                19..=25 => (1, 2),
                _ => (1, 1),
            };
            let span = authored_floor_available_span(
                &occupied,
                pattern.rows,
                pattern.columns,
                row,
                column,
                requested_span,
            );
            for occupied_row in row..row + span.1 {
                for occupied_column in column..column + span.0 {
                    occupied[occupied_row * pattern.columns + occupied_column] = true;
                }
            }

            let width = unit_width * span.0 as f32 - gap;
            let depth = unit_depth * span.1 as f32 - gap;
            let x = -floor_width * 0.5 + (column as f32 + span.0 as f32 * 0.5) * unit_width;
            let z = -floor_depth * 0.5 + (row as f32 + span.1 as f32 * 0.5) * unit_depth;
            let variant = authored_floor_hash(row, column, 1) % buffers.len();
            let height_step = (authored_floor_hash(row, column, 2) % 4) as f32;
            let height = 0.09 + height_step * 0.004;
            let top_y = height_step * 0.0012;
            append_authored_floor_stone(
                &mut buffers[variant],
                Vec3::new(x, top_y, z),
                Vec3::new(width, height, depth),
                pattern.bevel,
            );
        }
    }

    for (material_index, buffers) in buffers.into_iter().enumerate() {
        if buffers.indices.is_empty() {
            continue;
        }
        let material_name = &map.floor_materials[material_index];
        let Some(material) = material_handles.get(material_name) else {
            warn!("{arena_name} floor pattern references missing material '{material_name}'");
            continue;
        };
        let mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, buffers.positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, buffers.normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, buffers.uvs)
        .with_inserted_indices(Indices::U32(buffers.indices));
        commands.spawn((
            Mesh3d(meshes.add(mesh)),
            MeshMaterial3d(material.clone()),
            Transform::from_xyz(0.0, ARENA_TOP_Y, 0.0),
            ArenaGeometry,
            Name::new(format!("{arena_name} dense floor {material_index}")),
        ));
    }
}

fn authored_floor_hash(row: usize, column: usize, salt: usize) -> usize {
    let mut value = (row as u32).wrapping_mul(0x9E37_79B9)
        ^ (column as u32).wrapping_mul(0x85EB_CA6B)
        ^ (salt as u32).wrapping_mul(0xC2B2_AE35);
    value ^= value >> 16;
    value = value.wrapping_mul(0x7FEB_352D);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846C_A68B);
    (value ^ (value >> 16)) as usize
}

fn authored_floor_available_span(
    occupied: &[bool],
    rows: usize,
    columns: usize,
    row: usize,
    column: usize,
    requested: (usize, usize),
) -> (usize, usize) {
    for depth in (1..=requested.1.min(rows - row)).rev() {
        for width in (1..=requested.0.min(columns - column)).rev() {
            if (row..row + depth).all(|candidate_row| {
                (column..column + width)
                    .all(|candidate_column| !occupied[candidate_row * columns + candidate_column])
            }) {
                return (width, depth);
            }
        }
    }
    (1, 1)
}

fn append_authored_floor_stone(
    buffers: &mut AuthoredFloorMeshBuffers,
    center: Vec3,
    size: Vec3,
    authored_bevel: f32,
) {
    let half_width = size.x * 0.5;
    let half_depth = size.z * 0.5;
    let bevel = authored_bevel
        .max(0.0)
        .min(half_width * 0.32)
        .min(half_depth * 0.32)
        .min(size.y * 0.42);
    let top_y = center.y;
    let shoulder_y = top_y - bevel;
    let bottom_y = top_y - size.y;
    let outer_cut = bevel * 0.52;
    let inner_half_width = half_width - bevel;
    let inner_half_depth = half_depth - bevel;
    let inner_cut = (outer_cut * 0.72)
        .min(inner_half_width * 0.45)
        .min(inner_half_depth * 0.45);
    let outer = authored_floor_octagon(half_width, half_depth, outer_cut);
    let inner = authored_floor_octagon(inner_half_width, inner_half_depth, inner_cut);

    let top_center_index = buffers.positions.len() as u32;
    buffers.positions.push([center.x, top_y, center.z]);
    buffers.normals.push([0.0, 1.0, 0.0]);
    buffers.uvs.push([0.5, 0.5]);
    for point in inner {
        buffers
            .positions
            .push([center.x + point.x, top_y, center.z + point.y]);
        buffers.normals.push([0.0, 1.0, 0.0]);
        buffers.uvs.push([
            0.5 + point.x / size.x.max(f32::EPSILON),
            0.5 + point.y / size.z.max(f32::EPSILON),
        ]);
    }
    for index in 0..8_u32 {
        let current = top_center_index + 1 + index;
        let next = top_center_index + 1 + (index + 1) % 8;
        buffers
            .indices
            .extend_from_slice(&[top_center_index, next, current]);
    }

    for index in 0..8 {
        let next = (index + 1) % 8;
        let edge = outer[next] - outer[index];
        let outward = Vec3::new(edge.y, 0.0, -edge.x).normalize_or_zero();
        let bevel_normal = (outward + Vec3::Y * 0.9).normalize_or_zero();
        append_authored_floor_quad(
            buffers,
            [
                Vec3::new(center.x + inner[index].x, top_y, center.z + inner[index].y),
                Vec3::new(center.x + inner[next].x, top_y, center.z + inner[next].y),
                Vec3::new(
                    center.x + outer[next].x,
                    shoulder_y,
                    center.z + outer[next].y,
                ),
                Vec3::new(
                    center.x + outer[index].x,
                    shoulder_y,
                    center.z + outer[index].y,
                ),
            ],
            bevel_normal,
        );
        append_authored_floor_quad(
            buffers,
            [
                Vec3::new(
                    center.x + outer[index].x,
                    shoulder_y,
                    center.z + outer[index].y,
                ),
                Vec3::new(
                    center.x + outer[next].x,
                    shoulder_y,
                    center.z + outer[next].y,
                ),
                Vec3::new(center.x + outer[next].x, bottom_y, center.z + outer[next].y),
                Vec3::new(
                    center.x + outer[index].x,
                    bottom_y,
                    center.z + outer[index].y,
                ),
            ],
            outward,
        );
    }
}

fn authored_floor_octagon(half_width: f32, half_depth: f32, cut: f32) -> [Vec2; 8] {
    [
        Vec2::new(-half_width + cut, -half_depth),
        Vec2::new(half_width - cut, -half_depth),
        Vec2::new(half_width, -half_depth + cut),
        Vec2::new(half_width, half_depth - cut),
        Vec2::new(half_width - cut, half_depth),
        Vec2::new(-half_width + cut, half_depth),
        Vec2::new(-half_width, half_depth - cut),
        Vec2::new(-half_width, -half_depth + cut),
    ]
}

fn append_authored_floor_quad(
    buffers: &mut AuthoredFloorMeshBuffers,
    mut points: [Vec3; 4],
    normal: Vec3,
) {
    let face_normal = (points[1] - points[0])
        .cross(points[2] - points[0])
        .normalize_or_zero();
    if face_normal.dot(normal) < 0.0 {
        points.reverse();
    }
    let start = buffers.positions.len() as u32;
    for (index, point) in points.into_iter().enumerate() {
        buffers.positions.push(point.to_array());
        buffers.normals.push(normal.to_array());
        buffers.uvs.push(match index {
            0 => [0.0, 0.0],
            1 => [1.0, 0.0],
            2 => [1.0, 1.0],
            _ => [0.0, 1.0],
        });
    }
    buffers
        .indices
        .extend_from_slice(&[start, start + 1, start + 2, start, start + 2, start + 3]);
}

fn spawn_authored_floor_rows(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    map: &AuthoredArenaRon,
    material_handles: &HashMap<String, Handle<StandardMaterial>>,
    mesh_handles: &mut HashMap<String, Handle<Mesh>>,
    arena_name: &str,
) {
    if map.floor_rows.is_empty() || map.floor_materials.is_empty() {
        return;
    }

    let floor_width = if map.map.floor_width > 0.0 {
        map.map.floor_width
    } else {
        16.0
    };
    let floor_depth = if map.map.floor_depth > 0.0 {
        map.map.floor_depth
    } else {
        12.0
    };
    let gap = 0.055;

    for (row_index, row) in map.floor_rows.iter().enumerate() {
        if row.z.abs() + row.depth * 0.5 > floor_depth * 0.5 + 0.08 {
            warn!("{arena_name} floor row {row_index} extends beyond its authored depth");
        }
        if row.widths.is_empty() {
            continue;
        }
        let width_sum = row.widths.iter().sum::<f32>();
        if width_sum <= f32::EPSILON {
            continue;
        }
        let row_width = floor_width - row.offset.abs() * 2.0;
        let usable_width = row_width - gap * (row.widths.len().saturating_sub(1) as f32);
        let width_scale = usable_width / width_sum;
        let mut cursor = -row_width * 0.5 + row.offset;

        for (column_index, authored_width) in row.widths.iter().copied().enumerate() {
            let width = authored_width * width_scale;
            let x = cursor + width * 0.5;
            cursor += width + gap;
            let variant = (row_index * 3 + column_index * 2) % map.floor_materials.len();
            let Some(material) = material_handles.get(&map.floor_materials[variant]) else {
                warn!(
                    "{arena_name} floor row references missing material '{}'",
                    map.floor_materials[variant]
                );
                continue;
            };
            let height_variant = ((row_index * 5 + column_index * 7) % 3) as f32 * 0.004;
            let height = 0.105 + height_variant;
            let yaw_step = ((row_index * 7 + column_index * 11) % 5) as f32 - 2.0;
            let depth_step = ((row_index * 13 + column_index * 7) % 5) as f32;
            let depth = row.depth * (0.92 + depth_step * 0.035);
            let z_jitter = (depth_step - 2.0) * 0.022;
            let transform = Transform::from_xyz(
                x,
                ARENA_TOP_Y - height * 0.5 + height_variant * 0.25,
                row.z + z_jitter,
            )
            .with_rotation(Quat::from_rotation_y(yaw_step * 0.006))
            .with_scale(Vec3::new(width, height, depth));
            let mesh = authored_primitive_mesh(meshes, mesh_handles, "cuboid");
            commands.spawn((
                Mesh3d(mesh),
                MeshMaterial3d(material.clone()),
                transform,
                ArenaGeometry,
                Name::new(format!(
                    "{arena_name} floor slab {row_index}-{column_index}"
                )),
            ));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_authored_primitive(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    primitive: &AuthoredPrimitive,
    transform: Transform,
    material_handles: &HashMap<String, Handle<StandardMaterial>>,
    mesh_handles: &mut HashMap<String, Handle<Mesh>>,
    name: String,
    ordinal: usize,
) {
    let Some(material) = material_handles.get(&primitive.material) else {
        warn!(
            "{name} references missing material '{}'",
            primitive.material
        );
        return;
    };
    let mesh = authored_primitive_mesh(meshes, mesh_handles, &primitive.kind);
    let base_scale = transform.scale;
    let mut entity = commands.spawn((
        Mesh3d(mesh),
        MeshMaterial3d(material.clone()),
        transform,
        ArenaGeometry,
        Name::new(name),
    ));
    if primitive.effect == "flame" {
        entity.insert(ArenaDecorativeFlame {
            base_scale,
            phase: ordinal as f32 * 0.47,
        });
    }
}

fn authored_primitive_mesh(
    meshes: &mut Assets<Mesh>,
    mesh_handles: &mut HashMap<String, Handle<Mesh>>,
    kind: &str,
) -> Handle<Mesh> {
    if let Some(mesh) = mesh_handles.get(kind) {
        return mesh.clone();
    }
    let mesh = match kind {
        "cylinder" => meshes.add(Cylinder::new(0.5, 1.0)),
        "cone" => meshes.add(Cone::new(0.5, 1.0)),
        "sphere" => meshes.add(Sphere::new(0.5).mesh().uv(16, 8)),
        _ => meshes.add(Cuboid::new(1.0, 1.0, 1.0)),
    };
    mesh_handles.insert(kind.to_string(), mesh.clone());
    mesh
}

fn authored_primitive_transform(primitive: &AuthoredPrimitive) -> Transform {
    Transform::from_translation(champions_stage_position(primitive.position))
        .with_rotation(champions_yaw(primitive.rotation_y))
        .with_scale(champions_scale(primitive.scale))
}

fn authored_primitive_prefab_transform(
    instance: &AuthoredPrimitivePrefabInstance,
    primitive: &AuthoredPrimitive,
) -> Transform {
    let parent_rotation = champions_yaw(instance.rotation_y);
    let child_rotation = champions_yaw(primitive.rotation_y);
    let parent_scale = champions_scale(instance.scale);
    let child_scale = champions_scale(primitive.scale);
    let parent_position = champions_raw_position(instance.position);
    let child_position = champions_raw_position(primitive.position);
    let translation = parent_position + parent_rotation * (child_position * parent_scale);

    Transform::from_translation(Vec3::new(
        translation.x,
        champions_stage_y(translation.y),
        translation.z,
    ))
    .with_rotation(parent_rotation * child_rotation)
    .with_scale(parent_scale * child_scale)
}

fn champions_floor_shape_tiles(shape: &AuthoredFloorShape) -> Vec<Vec2> {
    match shape.kind.as_str() {
        "filled_octagon" => {
            if shape.radius_tiles <= 0 {
                return Vec::new();
            }
            champions_octagon_tiles(shape.radius_tiles, None)
        }
        "octagon_ring" => {
            if shape.outer_radius_tiles <= 0 {
                return Vec::new();
            }
            champions_octagon_tiles(
                shape.outer_radius_tiles,
                (shape.inner_radius_tiles > 0).then_some(shape.inner_radius_tiles),
            )
        }
        "rectangle" => {
            let (width, depth) = shape.size_tiles;
            if width <= 0 || depth <= 0 {
                return Vec::new();
            }
            champions_rectangle_tiles(width, depth)
        }
        _ => Vec::new(),
    }
}

fn champions_floor_shape_render_positions(
    shape: &AuthoredFloorShape,
    tile_size: f32,
    arena_index: usize,
) -> Vec<Vec2> {
    champions_floor_shape_tiles(shape)
        .into_iter()
        .map(|tile| {
            Vec2::new(
                (shape.center.0 as f32 + tile.x) * tile_size,
                (shape.center.1 as f32 + tile.y) * tile_size,
            )
        })
        .filter(|position| floor_tile_is_firm_supported(arena_index, position.x, position.y))
        .collect()
}

fn champions_octagon_tiles(outer_radius: i32, inner_radius: Option<i32>) -> Vec<Vec2> {
    let outer_radius = outer_radius.max(0);
    let mut tiles = Vec::new();
    for x in -outer_radius..=outer_radius {
        for z in -outer_radius..=outer_radius {
            let distance = champions_octagon_distance(x, z);
            if distance > outer_radius as f32 {
                continue;
            }
            if let Some(inner_radius) = inner_radius {
                if distance <= inner_radius.max(0) as f32 {
                    continue;
                }
            }
            tiles.push(Vec2::new(x as f32, z as f32));
        }
    }
    tiles
}

fn champions_octagon_distance(x: i32, z: i32) -> f32 {
    let abs_x = x.abs() as f32;
    let abs_z = z.abs() as f32;
    abs_x.max(abs_z) + abs_x.min(abs_z) * 0.414
}

fn champions_rectangle_tiles(width: i32, depth: i32) -> Vec<Vec2> {
    let width = width.max(0);
    let depth = depth.max(0);
    let x_offset = (width - 1) as f32 * 0.5;
    let z_offset = (depth - 1) as f32 * 0.5;
    let mut tiles = Vec::new();
    for x in 0..width {
        for z in 0..depth {
            tiles.push(Vec2::new(x as f32 - x_offset, z as f32 - z_offset));
        }
    }
    tiles
}

fn spawn_authored_lights(commands: &mut Commands, lights: &[AuthoredLight], arena_name: &str) {
    for light in lights {
        match light.kind.as_str() {
            "directional" => {
                commands.spawn((
                    DirectionalLight {
                        illuminance: if light.illuminance > 0.0 {
                            light.illuminance
                        } else {
                            12_500.0
                        },
                        color: champions_color(light.color),
                        shadows_enabled: light.shadows,
                        ..default()
                    },
                    champions_light_transform(light),
                    ArenaGeometry,
                    Name::new(format!("{arena_name} light {}", light.id)),
                ));
            }
            "point" => {
                commands.spawn((
                    PointLight {
                        intensity: if light.intensity > 0.0 {
                            light.intensity
                        } else {
                            850.0
                        } * CHAMPIONS_COURT_LIGHT_SCALE,
                        range: if light.range > 0.0 { light.range } else { 8.0 },
                        color: champions_color(light.color),
                        shadows_enabled: light.shadows,
                        ..default()
                    },
                    Transform::from_translation(champions_stage_position(light.position)),
                    ArenaGeometry,
                    Name::new(format!("{arena_name} light {}", light.id)),
                ));
            }
            _ => {}
        }
    }
}

fn champions_light_transform(light: &AuthoredLight) -> Transform {
    let (x, y, z) = light.rotation_euler_degrees;
    Transform::from_rotation(Quat::from_euler(
        EulerRot::XYZ,
        x.to_radians(),
        y.to_radians(),
        z.to_radians(),
    ))
}

fn champions_color(color: (f32, f32, f32)) -> Color {
    let (r, g, b) = color;
    Color::srgb(r, g, b)
}

fn champions_floor_asset_scale(asset_key: &str, tile_size: f32) -> f32 {
    let base_scale = match asset_key {
        "floor_detail" => 1.42,
        _ => MINI_ARENA_FLOOR_SCALE,
    };
    base_scale * tile_size / MINI_ARENA_FLOOR_SPACING
}

fn champions_stage_position(position: (f32, f32, f32)) -> Vec3 {
    let position = champions_raw_position(position);
    Vec3::new(position.x, champions_stage_y(position.y), position.z)
}

fn champions_object_position(position: (f32, f32, f32)) -> Vec3 {
    champions_stage_position(position) + Vec3::Y * ARENA_PROP_SURFACE_CLEARANCE
}

fn champions_raw_position(position: (f32, f32, f32)) -> Vec3 {
    Vec3::new(position.0, position.1, position.2)
}

fn champions_stage_y(y: f32) -> f32 {
    ARENA_TOP_Y + y
}

fn champions_yaw(degrees: f32) -> Quat {
    Quat::from_rotation_y(degrees.to_radians())
}

fn champions_scale(scale: (f32, f32, f32)) -> Vec3 {
    Vec3::new(scale.0, scale.1, scale.2)
}

fn unit_tuple3() -> (f32, f32, f32) {
    (1.0, 1.0, 1.0)
}

fn white_tuple3() -> (f32, f32, f32) {
    (1.0, 1.0, 1.0)
}

fn default_roughness() -> f32 {
    1.0
}

fn spawn_mini_arena_props(commands: &mut Commands, asset_server: &AssetServer, arena_index: usize) {
    for prop in arena_asset_props(arena_index) {
        let asset_path = arena_prop_asset_path(prop.file);
        commands.spawn((
            SceneRoot(asset_server.load(GltfAssetLabel::Scene(0).from_asset(asset_path))),
            prop.transform(),
            ArenaGeometry,
            Name::new(prop.name),
        ));
    }
}

fn arena_prop_asset_path(file: &str) -> String {
    if file.contains('/') {
        format!("{ARENA_KIT_ASSET_ROOT}/{file}")
    } else {
        format!("{MINI_ARENA_ASSET_ROOT}/{file}")
    }
}

fn floor_tile_is_firm_supported(arena_index: usize, x: f32, z: f32) -> bool {
    let definitions = arena_definitions();
    let Some(arena) = definitions.get(arena_index.min(definitions.len().saturating_sub(1))) else {
        return false;
    };
    arena_position_is_firm_supported(arena, x, z)
}

fn arena_position_is_firm_supported(arena: &ArenaDefinition, x: f32, z: f32) -> bool {
    arena
        .ground_shapes
        .iter()
        .any(|shape| ground_shape_contains_firm_support(shape, x, z))
        || arena
            .platforms
            .iter()
            .any(|platform| platform_contains_firm_support(platform, x, z))
}

fn ground_shape_contains_firm_support(shape: &ArenaGroundShape, x: f32, z: f32) -> bool {
    ground_shape_support(shape, x, z, 0.0).is_some()
}

fn platform_contains_firm_support(platform: &PlatformDefinition, x: f32, z: f32) -> bool {
    let dx = (x - platform.center.x).abs();
    let dz = (z - platform.center.y).abs();
    dx <= platform.half_extents.x && dz <= platform.half_extents.y
}

fn arena_asset_props(arena_index: usize) -> &'static [ArenaAssetProp] {
    match arena_index {
        0 => CROWN_ASSET_PROPS,
        1 => SPLIT_ASSET_PROPS,
        2 => SUNSTONE_ASSET_PROPS,
        3 => CRANK_ASSET_PROPS,
        4 => VENT_SPIRAL_ASSET_PROPS,
        5 => BUMPER_ALLEY_ASSET_PROPS,
        6 => FEAST_MARKET_ASSET_PROPS,
        7 => SNARE_GARDEN_ASSET_PROPS,
        8 => SKY_STEPS_ASSET_PROPS,
        9 => POWDER_KEG_ASSET_PROPS,
        TRAINING_GROUND_ARENA_INDEX => &[],
        _ => CROWN_ASSET_PROPS,
    }
}

fn arena_asset_props_for_definition(arena: &ArenaDefinition) -> &'static [ArenaAssetProp] {
    let arena_index = arena_definitions()
        .iter()
        .position(|candidate| candidate.name == arena.name)
        .unwrap_or_else(active_arena_index);

    // Authored RON scenes are rendered instead of their fallback prop lists.
    if matches!(
        arena_index,
        CHAMPIONS_COURT_ARENA_INDEX | TRAINING_GROUND_ARENA_INDEX
    ) {
        &[]
    } else {
        arena_asset_props(arena_index)
    }
}

fn authored_arena_collision_barriers(arena_index: usize) -> &'static [WorldPropBarrier] {
    static BARRIERS: OnceLock<Vec<Vec<WorldPropBarrier>>> = OnceLock::new();
    &BARRIERS.get_or_init(|| {
        arena_definitions()
            .iter()
            .enumerate()
            .map(|(index, _)| build_authored_arena_collision_barriers(index))
            .collect()
    })[arena_index]
}

fn build_authored_arena_collision_barriers(arena_index: usize) -> Vec<WorldPropBarrier> {
    let contents = match arena_index {
        CHAMPIONS_COURT_ARENA_INDEX => include_str!("../assets/maps/champions_court.ron"),
        TRAINING_GROUND_ARENA_INDEX => include_str!("../assets/maps/training_ground.ron"),
        _ => return Vec::new(),
    };
    let map: AuthoredArenaRon =
        ron::from_str(contents).expect("embedded authored arena RON should parse");
    let mut barriers = Vec::new();

    for object in &map.instances {
        let transform = Transform::from_xyz(
            object.position.0,
            ARENA_TOP_Y + object.position.1 + ARENA_PROP_SURFACE_CLEARANCE,
            object.position.2,
        )
        .with_rotation(Quat::from_rotation_y(object.rotation_y.to_radians()))
        .with_scale(Vec3::new(object.scale.0, object.scale.1, object.scale.2));
        append_champions_object_barriers(&map.assets, object, transform, &mut barriers);
    }

    for prefab_instance in &map.prefab_instances {
        let Some(objects) = map.prefabs.get(&prefab_instance.prefab) else {
            continue;
        };
        for object in objects {
            append_champions_object_barriers(
                &map.assets,
                object,
                champions_prefab_object_transform(prefab_instance, object),
                &mut barriers,
            );
        }
    }

    barriers.extend(map.colliders.iter().map(|collider| {
        debug_assert!(!collider.id.is_empty());
        WorldPropBarrier {
            definition: ArenaBarrierDefinition::rectangle(
                collider.center.0,
                collider.center.1,
                collider.half_extents.0,
                collider.half_extents.1,
                collider.rotation_y.to_radians(),
                ARENA_TOP_Y + collider.top_y,
            ),
            behavior: PropBarrierBehavior::Solid,
        }
    }));
    barriers
}

fn append_champions_object_barriers(
    assets: &HashMap<String, String>,
    object: &AuthoredObject,
    transform: Transform,
    barriers: &mut Vec<WorldPropBarrier>,
) {
    let Some(asset) = assets.get(&object.asset) else {
        return;
    };
    let (yaw, _, _) = transform.rotation.to_euler(EulerRot::YXZ);
    barriers.extend(
        prop_collision_profile(asset)
            .iter()
            .copied()
            .map(|barrier| barrier.to_world_scaled(transform.translation, yaw, transform.scale)),
    );
}

/// Immutable collision data derived from the geometry rendered for one arena.
///
/// Prop profiles are authored in model-local space. Converting them to world space
/// requires scale and rotation work that used to happen for every ground and side
/// collision probe. The arena catalog is static, so the converted barriers can be
/// built once and safely shared by fighters, bots, and tests.
#[allow(dead_code)]
pub struct ArenaCollisionWorld {
    arena_index: usize,
    prop_barriers: Vec<WorldPropBarrier>,
}

#[allow(dead_code)]
impl ArenaCollisionWorld {
    pub fn arena_index(&self) -> usize {
        self.arena_index
    }

    pub fn prop_barrier_count(&self) -> usize {
        self.prop_barriers.len()
    }
}

fn arena_collision_worlds() -> &'static [ArenaCollisionWorld] {
    static WORLDS: OnceLock<Vec<ArenaCollisionWorld>> = OnceLock::new();
    WORLDS.get_or_init(|| {
        arena_definitions()
            .iter()
            .enumerate()
            .map(|(arena_index, arena)| {
                let mut prop_barriers: Vec<_> = arena_asset_props_for_definition(arena)
                    .iter()
                    .copied()
                    .flat_map(ArenaAssetProp::collision_barriers)
                    .collect();
                if matches!(
                    arena_index,
                    CHAMPIONS_COURT_ARENA_INDEX | TRAINING_GROUND_ARENA_INDEX
                ) {
                    prop_barriers.extend(
                        authored_arena_collision_barriers(arena_index)
                            .iter()
                            .copied(),
                    );
                }
                ArenaCollisionWorld {
                    arena_index,
                    prop_barriers,
                }
            })
            .collect()
    })
}

pub fn arena_collision_world(arena: &ArenaDefinition) -> &'static ArenaCollisionWorld {
    let arena_index = arena_definitions()
        .iter()
        .position(|candidate| std::ptr::eq(candidate, arena))
        .or_else(|| {
            arena_definitions()
                .iter()
                .position(|candidate| candidate.name == arena.name)
        })
        .unwrap_or_else(active_arena_index);
    &arena_collision_worlds()[arena_index]
}

fn arena_prop_barriers(arena: &ArenaDefinition) -> impl Iterator<Item = WorldPropBarrier> + '_ {
    arena_collision_world(arena).prop_barriers.iter().copied()
}

const CROWN_ASSET_PROPS: &[ArenaAssetProp] = &[
    ArenaAssetProp {
        name: "Crown north statue",
        file: "statue.glb",
        x: -2.2,
        y: ARENA_TOP_Y,
        z: 8.85,
        yaw: PI,
        scale: 1.65,
    },
    ArenaAssetProp {
        name: "Crown south statue",
        file: "statue.glb",
        x: 2.2,
        y: ARENA_TOP_Y,
        z: -8.85,
        yaw: 0.0,
        scale: 1.65,
    },
    ArenaAssetProp {
        name: "Crown rear banner left",
        file: "banner.glb",
        x: -5.7,
        y: ARENA_TOP_Y,
        z: -8.9,
        yaw: 0.0,
        scale: 1.55,
    },
    ArenaAssetProp {
        name: "Crown rear banner right",
        file: "banner.glb",
        x: 5.7,
        y: ARENA_TOP_Y,
        z: -8.9,
        yaw: 0.0,
        scale: 1.55,
    },
    ArenaAssetProp {
        name: "Crown prize trophy",
        file: "trophy.glb",
        x: 0.0,
        y: ARENA_TOP_Y + 0.12,
        z: 9.8,
        yaw: PI,
        scale: 1.45,
    },
];

const SPLIT_ASSET_PROPS: &[ArenaAssetProp] = &[
    ArenaAssetProp {
        name: "Split west bridge frame",
        file: "tower/wood-structure-high.glb",
        x: -7.2,
        y: ARENA_TOP_Y,
        z: 0.0,
        yaw: PI * 0.5,
        scale: 2.4,
    },
    ArenaAssetProp {
        name: "Split east bridge frame",
        file: "tower/wood-structure-high.glb",
        x: 7.2,
        y: ARENA_TOP_Y,
        z: 0.0,
        yaw: PI * 0.5,
        scale: 2.4,
    },
    ArenaAssetProp {
        name: "Split west watch tree",
        file: "tower/detail-tree-large.glb",
        x: -8.4,
        y: ARENA_TOP_Y - 0.15,
        z: 5.6,
        yaw: 0.25,
        scale: 2.25,
    },
    ArenaAssetProp {
        name: "Split east watch tree",
        file: "tower/detail-tree-large.glb",
        x: 8.4,
        y: ARENA_TOP_Y - 0.15,
        z: -5.6,
        yaw: -0.35,
        scale: 2.0,
    },
];

const SUNSTONE_ASSET_PROPS: &[ArenaAssetProp] = &[
    ArenaAssetProp {
        name: "Sunstone west timber lookout",
        file: "tower/wood-structure.glb",
        x: -6.0,
        y: ARENA_TOP_Y + 0.08,
        z: 3.8,
        yaw: -0.55,
        scale: 2.4,
    },
    ArenaAssetProp {
        name: "Sunstone east timber lookout",
        file: "tower/wood-structure.glb",
        x: 6.0,
        y: ARENA_TOP_Y + 0.08,
        z: -3.8,
        yaw: -0.55,
        scale: 2.4,
    },
    ArenaAssetProp {
        name: "Sunstone west rocks",
        file: "tower/detail-rocks-large.glb",
        x: -6.0,
        y: ARENA_TOP_Y,
        z: -4.7,
        yaw: 0.2,
        scale: 2.2,
    },
    ArenaAssetProp {
        name: "Sunstone east rocks",
        file: "tower/detail-rocks-large.glb",
        x: 6.0,
        y: ARENA_TOP_Y,
        z: 4.7,
        yaw: -0.25,
        scale: 2.2,
    },
];

const CRANK_ASSET_PROPS: &[ArenaAssetProp] = &[
    ArenaAssetProp {
        name: "Crank yard west conveyor",
        file: "platformer/conveyor-belt.glb",
        x: -3.15,
        y: ARENA_TOP_Y + 0.01,
        z: 0.0,
        yaw: PI * 0.5,
        scale: 2.4,
    },
    ArenaAssetProp {
        name: "Crank yard east conveyor",
        file: "platformer/conveyor-belt.glb",
        x: 3.15,
        y: ARENA_TOP_Y + 0.01,
        z: 0.0,
        yaw: -PI * 0.5,
        scale: 2.4,
    },
    ArenaAssetProp {
        name: "Crank yard north pipe",
        file: "platformer/pipe.glb",
        x: -1.7,
        y: ARENA_TOP_Y,
        z: 7.0,
        yaw: PI,
        scale: CRANK_PIPE_VISUAL_SCALE,
    },
    ArenaAssetProp {
        name: "Crank yard south pipe",
        file: "platformer/pipe.glb",
        x: 1.7,
        y: ARENA_TOP_Y,
        z: -7.0,
        yaw: 0.0,
        scale: CRANK_PIPE_VISUAL_SCALE,
    },
];

const VENT_SPIRAL_ASSET_PROPS: &[ArenaAssetProp] = &[ArenaAssetProp {
    name: "Vent spiral crystal core",
    file: "tower/tower-round-crystals.glb",
    x: 0.0,
    y: crate::arena_defs::VENT_SPIRAL_REACTOR_BASE_Y,
    z: 0.0,
    yaw: crate::arena_defs::VENT_SPIRAL_REACTOR_YAW,
    scale: crate::arena_defs::VENT_SPIRAL_REACTOR_SCALE,
}];

const BUMPER_ALLEY_ASSET_PROPS: &[ArenaAssetProp] = &[
    ArenaAssetProp {
        name: "Bumper alley north spring",
        file: "platformer/spring.glb",
        x: 0.0,
        y: ARENA_TOP_Y + 0.02,
        z: 4.25,
        yaw: 0.0,
        scale: 2.2,
    },
    ArenaAssetProp {
        name: "Bumper alley center spring",
        file: "platformer/spring.glb",
        x: 0.0,
        y: ARENA_TOP_Y + 0.02,
        z: 0.0,
        yaw: PI * 0.5,
        scale: 2.2,
    },
    ArenaAssetProp {
        name: "Bumper alley south spring",
        file: "platformer/spring.glb",
        x: 0.0,
        y: ARENA_TOP_Y + 0.02,
        z: -4.25,
        yaw: PI,
        scale: 2.2,
    },
    ArenaAssetProp {
        name: "Bumper alley west target",
        file: "blaster/target-large.glb",
        x: -4.15,
        y: ARENA_TOP_Y + 0.03,
        z: 7.6,
        yaw: PI * 0.5,
        scale: 2.5,
    },
    ArenaAssetProp {
        name: "Bumper alley east target",
        file: "blaster/target-large.glb",
        x: 4.15,
        y: ARENA_TOP_Y + 0.03,
        z: -7.6,
        yaw: -PI * 0.5,
        scale: 2.5,
    },
];

const FEAST_MARKET_ASSET_PROPS: &[ArenaAssetProp] = &[
    ArenaAssetProp {
        name: "Feast market burger stall",
        file: "food/burger-cheese-double.glb",
        x: -5.8,
        y: ARENA_TOP_Y + 0.02,
        z: 3.4,
        yaw: 0.25,
        scale: 3.2,
    },
    ArenaAssetProp {
        name: "Feast market cake stall",
        file: "food/cake.glb",
        x: 5.8,
        y: ARENA_TOP_Y + 0.02,
        z: -3.4,
        yaw: -0.25,
        scale: 3.3,
    },
    ArenaAssetProp {
        name: "Feast market pizza sign",
        file: "food/pizza.glb",
        x: 3.0,
        y: ARENA_TOP_Y + 0.05,
        z: 6.2,
        yaw: 0.1,
        scale: 3.3,
    },
    ArenaAssetProp {
        name: "Feast market watermelon stand",
        file: "food/watermelon.glb",
        x: -3.0,
        y: ARENA_TOP_Y + 0.02,
        z: -6.2,
        yaw: -0.15,
        scale: 3.0,
    },
    ArenaAssetProp {
        name: "Feast market stew pot",
        file: "food/pot-stew.glb",
        x: 5.7,
        y: ARENA_TOP_Y + 0.02,
        z: 3.9,
        yaw: 0.3,
        scale: 3.0,
    },
    ArenaAssetProp {
        name: "Feast market supply crate",
        file: "platformer/crate.glb",
        x: -5.9,
        y: ARENA_TOP_Y,
        z: -3.9,
        yaw: 0.2,
        scale: 1.8,
    },
];

const SNARE_GARDEN_ASSET_PROPS: &[ArenaAssetProp] = &[
    ArenaAssetProp {
        name: "Snare garden west flowers",
        file: "platformer/flowers-tall.glb",
        x: -5.1,
        y: ARENA_TOP_Y + 0.02,
        z: 1.7,
        yaw: 0.35,
        scale: 2.1,
    },
    ArenaAssetProp {
        name: "Snare garden east flowers",
        file: "platformer/flowers.glb",
        x: 5.1,
        y: ARENA_TOP_Y + 0.02,
        z: -1.7,
        yaw: -0.4,
        scale: 2.2,
    },
    ArenaAssetProp {
        name: "Snare garden old tree",
        file: "platformer/tree.glb",
        x: -7.8,
        y: ARENA_TOP_Y - 0.08,
        z: 6.8,
        yaw: 0.2,
        scale: 2.4,
    },
];

const SKY_STEPS_ASSET_PROPS: &[ArenaAssetProp] = &[
    ArenaAssetProp {
        name: "Sky steps west pine",
        file: "platformer/tree-pine-snow.glb",
        x: -8.0,
        y: ARENA_TOP_Y - 0.18,
        z: -6.4,
        yaw: 0.1,
        scale: 1.8,
    },
    ArenaAssetProp {
        name: "Sky steps east pine",
        file: "platformer/tree-pine-snow-small.glb",
        x: 6.8,
        y: ARENA_TOP_Y + 0.8,
        z: 5.5,
        yaw: -0.2,
        scale: 2.4,
    },
    ArenaAssetProp {
        name: "Sky steps snowman",
        file: "holiday/snowman.glb",
        x: -5.8,
        y: ARENA_TOP_Y + 0.28,
        z: 4.7,
        yaw: 0.35,
        scale: 2.0,
    },
    ArenaAssetProp {
        name: "Sky steps signal lantern",
        file: "holiday/lantern.glb",
        x: 5.6,
        y: ARENA_TOP_Y + 0.31,
        z: -4.7,
        yaw: 0.0,
        scale: 1.8,
    },
    ArenaAssetProp {
        name: "Sky steps timber shelter",
        file: "tower/snow-wood-structure.glb",
        x: 0.0,
        y: ARENA_TOP_Y + 0.38,
        z: 0.0,
        yaw: PI * 0.25,
        scale: 1.8,
    },
    ArenaAssetProp {
        name: "Sky steps west snow bank",
        file: "holiday/snow-pile.glb",
        x: -3.0,
        y: ARENA_TOP_Y + 0.18,
        z: -2.4,
        yaw: 0.1,
        scale: 2.6,
    },
    ArenaAssetProp {
        name: "Sky steps east snow bank",
        file: "tower/snow-detail-rocks-large.glb",
        x: 3.0,
        y: ARENA_TOP_Y + 0.58,
        z: 2.4,
        yaw: -0.15,
        scale: 2.4,
    },
];

const POWDER_KEG_ASSET_PROPS: &[ArenaAssetProp] = &[
    ArenaAssetProp {
        name: "Powder keg west cannon",
        file: "tower/weapon-cannon.glb",
        x: -6.7,
        y: ARENA_TOP_Y,
        z: 1.8,
        yaw: PI * 0.5,
        scale: 2.5,
    },
    ArenaAssetProp {
        name: "Powder keg east cannon",
        file: "tower/weapon-cannon.glb",
        x: 6.7,
        y: ARENA_TOP_Y,
        z: -1.8,
        yaw: -PI * 0.75,
        scale: 2.5,
    },
    ArenaAssetProp {
        name: "Powder keg catapult",
        file: "tower/weapon-catapult.glb",
        x: 0.0,
        y: ARENA_TOP_Y,
        z: 6.8,
        yaw: PI,
        scale: 2.3,
    },
    ArenaAssetProp {
        name: "Powder keg bomb cache",
        file: "platformer/bomb.glb",
        x: -3.8,
        y: ARENA_TOP_Y + 0.02,
        z: -5.8,
        yaw: 0.25,
        scale: 2.3,
    },
    ArenaAssetProp {
        name: "Powder keg barrel cache",
        file: "platformer/barrel.glb",
        x: 3.8,
        y: ARENA_TOP_Y,
        z: 5.8,
        yaw: -0.25,
        scale: 2.1,
    },
    ArenaAssetProp {
        name: "Powder keg cannonballs",
        file: "tower/weapon-ammo-cannonball.glb",
        x: 5.4,
        y: ARENA_TOP_Y + 0.02,
        z: 5.6,
        yaw: 0.0,
        scale: 2.5,
    },
];

fn spawn_arena_lights(commands: &mut Commands) {
    commands.spawn((
        DirectionalLight {
            illuminance: 12_500.0,
            shadows_enabled: true,
            ..default()
        },
        Transform::from_xyz(-5.0, 12.0, 7.0).looking_at(Vec3::ZERO, Vec3::Y),
        ArenaGlobalDirectionalLight,
    ));

    commands.spawn((
        PointLight {
            intensity: 1_100_000.0,
            range: 36.0,
            shadows_enabled: false,
            ..default()
        },
        Transform::from_xyz(0.0, 9.0, 4.5),
        ArenaGlobalPointLight,
    ));
}

pub fn sync_arena_lighting(
    scene: Res<ArenaScene>,
    mut ambient: ResMut<GlobalAmbientLight>,
    mut directional_lights: Query<
        &mut DirectionalLight,
        (
            With<ArenaGlobalDirectionalLight>,
            Without<ArenaGlobalPointLight>,
        ),
    >,
    mut point_lights: Query<
        (&mut PointLight, &mut Transform),
        (
            With<ArenaGlobalPointLight>,
            Without<ArenaGlobalDirectionalLight>,
        ),
    >,
) {
    if !scene.is_changed() {
        return;
    }

    let training = scene.index == TRAINING_GROUND_ARENA_INDEX;
    if training {
        ambient.color = Color::srgb(0.68, 0.64, 0.58);
        ambient.brightness = 220.0;
    } else {
        ambient.color = Color::srgb(0.85, 0.78, 0.68);
        ambient.brightness = 430.0;
    }

    for mut light in &mut directional_lights {
        light.illuminance = if training { 8_000.0 } else { 12_500.0 };
        light.color = if training {
            Color::srgb(1.0, 0.96, 0.90)
        } else {
            Color::WHITE
        };
    }
    for (mut light, mut transform) in &mut point_lights {
        light.intensity = if training { 1_600_000.0 } else { 1_100_000.0 };
        light.range = if training { 20.0 } else { 36.0 };
        light.color = if training {
            Color::srgb(1.0, 0.86, 0.70)
        } else {
            Color::WHITE
        };
        transform.translation = if training {
            Vec3::new(0.0, 8.0, 4.0)
        } else {
            Vec3::new(0.0, 9.0, 4.5)
        };
    }
}

fn spawn_campfire_props(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    hazards: &[ArenaHazardDefinition],
) {
    if !hazards
        .iter()
        .any(|hazard| hazard.kind == ArenaHazardKind::Campfire)
    {
        return;
    }

    let stone_mesh = meshes.add(Cuboid::new(0.34, 0.2, 0.27));
    let log_mesh = meshes.add(Cylinder::new(0.12, 1.05));
    let outer_flame_mesh = meshes.add(Cone::new(0.38, 0.95));
    let inner_flame_mesh = meshes.add(Cone::new(0.2, 0.58));
    let stone_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.32, 0.29, 0.26),
        perceptual_roughness: 0.96,
        ..default()
    });
    let log_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.24, 0.09, 0.035),
        perceptual_roughness: 0.92,
        ..default()
    });
    let outer_flame_material = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.19, 0.025),
        emissive: LinearRgba::from(Color::srgb(1.0, 0.08, 0.01)) * 5.0,
        perceptual_roughness: 0.48,
        ..default()
    });
    let inner_flame_material = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.76, 0.08),
        emissive: LinearRgba::from(Color::srgb(1.0, 0.48, 0.025)) * 7.0,
        perceptual_roughness: 0.42,
        ..default()
    });

    for hazard in hazards
        .iter()
        .filter(|hazard| hazard.kind == ArenaHazardKind::Campfire)
    {
        for stone_index in 0..8 {
            let angle = stone_index as f32 / 8.0 * TAU;
            commands.spawn((
                Mesh3d(stone_mesh.clone()),
                MeshMaterial3d(stone_material.clone()),
                Transform::from_xyz(
                    hazard.center.x + angle.cos() * 0.62,
                    hazard.center.y + 0.1,
                    hazard.center.z + angle.sin() * 0.62,
                )
                .with_rotation(Quat::from_rotation_y(-angle)),
                Name::new("Campfire stone"),
                ArenaGeometry,
            ));
        }

        for yaw in [PI * 0.25, -PI * 0.25] {
            commands.spawn((
                Mesh3d(log_mesh.clone()),
                MeshMaterial3d(log_material.clone()),
                Transform::from_xyz(hazard.center.x, hazard.center.y + 0.24, hazard.center.z)
                    .with_rotation(Quat::from_rotation_y(yaw) * Quat::from_rotation_z(PI * 0.5)),
                Name::new("Campfire log"),
                ArenaGeometry,
            ));
        }

        let outer_scale = Vec3::new(1.0, 1.0, 1.0);
        commands.spawn((
            Mesh3d(outer_flame_mesh.clone()),
            MeshMaterial3d(outer_flame_material.clone()),
            Transform::from_xyz(hazard.center.x, hazard.center.y + 0.63, hazard.center.z)
                .with_scale(outer_scale),
            ArenaCampfireFlame {
                base_scale: outer_scale,
                phase: hazard.phase,
            },
            Name::new("Campfire outer flame"),
            ArenaGeometry,
        ));

        let inner_scale = Vec3::new(0.92, 1.0, 0.92);
        commands.spawn((
            Mesh3d(inner_flame_mesh.clone()),
            MeshMaterial3d(inner_flame_material.clone()),
            Transform::from_xyz(
                hazard.center.x,
                hazard.center.y + 0.48,
                hazard.center.z - 0.03,
            )
            .with_scale(inner_scale),
            ArenaCampfireFlame {
                base_scale: inner_scale,
                phase: hazard.phase + 1.7,
            },
            Name::new("Campfire inner flame"),
            ArenaGeometry,
        ));

        commands.spawn((
            PointLight {
                color: Color::srgb(1.0, 0.32, 0.06),
                intensity: 180_000.0,
                range: 4.5,
                shadows_enabled: false,
                ..default()
            },
            Transform::from_xyz(hazard.center.x, hazard.center.y + 1.05, hazard.center.z),
            Name::new("Campfire light"),
            ArenaGeometry,
        ));
    }
}

#[allow(dead_code)]
fn spawn_pipe_portal_visuals(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    pipe_pair: Option<ArenaPipePairDefinition>,
) {
    let Some(pipe_pair) = pipe_pair else {
        return;
    };

    let ring_mesh = meshes.add(Torus::new(0.66, 0.055));
    let particle_mesh = meshes.add(Sphere::new(0.075).mesh().uv(8, 5));
    let portal_material = materials.add(StandardMaterial {
        base_color: Color::srgba(0.12, 1.0, 0.72, 0.82),
        emissive: LinearRgba::from(Color::srgb(0.04, 1.0, 0.58)) * 5.0,
        alpha_mode: AlphaMode::Blend,
        perceptual_roughness: 0.32,
        ..default()
    });

    for (endpoint, center) in pipe_pair.endpoints.into_iter().enumerate() {
        let base_scale = Vec3::splat(1.0);
        commands.spawn((
            Mesh3d(ring_mesh.clone()),
            MeshMaterial3d(portal_material.clone()),
            Transform::from_xyz(center.x, pipe_pair.top_y + 0.045, center.y).with_scale(base_scale),
            ArenaPipePortalRing {
                endpoint,
                phase: endpoint as f32 * PI,
                base_scale,
            },
            Name::new(format!("Crank pipe portal ring {endpoint}")),
            ArenaGeometry,
        ));

        for particle_index in 0..5 {
            let phase = particle_index as f32 / 5.0 * TAU + endpoint as f32 * 0.8;
            let radius = 0.32 + (particle_index % 2) as f32 * 0.16;
            commands.spawn((
                Mesh3d(particle_mesh.clone()),
                MeshMaterial3d(portal_material.clone()),
                Transform::from_xyz(
                    center.x + phase.cos() * radius,
                    pipe_pair.top_y + 0.12,
                    center.y + phase.sin() * radius,
                ),
                ArenaPipePortalParticle {
                    endpoint,
                    phase,
                    radius,
                    base_y: pipe_pair.top_y + 0.08,
                },
                Name::new("Crank pipe portal mote"),
                ArenaGeometry,
            ));
        }

        commands.spawn((
            PointLight {
                color: Color::srgb(0.08, 1.0, 0.62),
                intensity: 70_000.0,
                range: 3.0,
                shadows_enabled: false,
                ..default()
            },
            Transform::from_xyz(center.x, pipe_pair.top_y + 0.55, center.y),
            Name::new("Crank pipe portal light"),
            ArenaGeometry,
        ));
    }
}

fn spawn_crank_yard_machinery(
    commands: &mut Commands,
    asset_server: &AssetServer,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    arena_index: usize,
    hazards: &[ArenaHazardDefinition],
) {
    if arena_index != CRANK_YARD_ARENA_INDEX {
        return;
    }

    let running_rotation = Quat::from_rotation_y(-PI * 0.5);
    commands.spawn((
        SceneRoot(asset_server.load(
            GltfAssetLabel::Scene(0).from_asset(arena_prop_asset_path("platformer/lever.glb")),
        )),
        Transform::from_translation(CRANK_LEVER_POSITION)
            .with_rotation(running_rotation)
            .with_scale(Vec3::splat(2.0)),
        CrankLeverVisual {
            running_rotation,
            stopped_rotation: running_rotation * Quat::from_rotation_z(-0.82),
        },
        Name::new("Crank yard saw stop lever"),
        ArenaGeometry,
    ));

    let saw_scene = asset_server
        .load(GltfAssetLabel::Scene(0).from_asset(arena_prop_asset_path("platformer/saw.glb")));
    let housing_mesh = meshes.add(Cuboid::new(1.75, 0.28, 0.2));
    let light_mesh = meshes.add(Sphere::new(0.13).mesh().uv(10, 6));
    let spark_mesh = meshes.add(Sphere::new(0.045).mesh().uv(6, 4));
    let housing_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.1, 0.12, 0.14),
        metallic: 0.72,
        perceptual_roughness: 0.38,
        ..default()
    });
    let warning_material = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.035, 0.015),
        emissive: LinearRgba::from(Color::srgb(1.0, 0.015, 0.005)) * 7.0,
        perceptual_roughness: 0.25,
        ..default()
    });
    let spark_material = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.72, 0.08),
        emissive: LinearRgba::from(Color::srgb(1.0, 0.36, 0.015)) * 6.0,
        perceptual_roughness: 0.3,
        ..default()
    });

    for (index, hazard) in hazards
        .iter()
        .filter(|hazard| hazard.kind == ArenaHazardKind::SawBlade)
        .enumerate()
    {
        let spin_speed = if index % 2 == 0 { 9.5 } else { -9.5 };
        commands.spawn((
            SceneRoot(saw_scene.clone()),
            Transform::from_xyz(hazard.center.x, CRANK_SAW_VISUAL_Y, hazard.center.z)
                .with_scale(Vec3::splat(2.5)),
            ArenaSawBladeVisual { spin_speed },
            Name::new(format!("Crank yard active saw {index}")),
            ArenaGeometry,
        ));

        for side in [-1.0, 1.0] {
            commands.spawn((
                Mesh3d(housing_mesh.clone()),
                MeshMaterial3d(housing_material.clone()),
                Transform::from_xyz(
                    hazard.center.x,
                    ARENA_TOP_Y + 0.5,
                    hazard.center.z + side * 0.68,
                ),
                Name::new("Crank saw housing rail"),
                ArenaGeometry,
            ));
        }

        let warning_scale = Vec3::splat(1.35);
        commands.spawn((
            Mesh3d(light_mesh.clone()),
            MeshMaterial3d(warning_material.clone()),
            Transform::from_xyz(hazard.center.x, ARENA_TOP_Y + 1.22, hazard.center.z + 0.72)
                .with_scale(warning_scale),
            ArenaSawWarningLight {
                phase: index as f32 * PI,
                base_scale: warning_scale,
            },
            Name::new("Crank saw warning lamp"),
            ArenaGeometry,
        ));

        for spark_index in 0..5 {
            commands.spawn((
                Mesh3d(spark_mesh.clone()),
                MeshMaterial3d(spark_material.clone()),
                Transform::from_xyz(hazard.center.x, ARENA_TOP_Y + 0.92, hazard.center.z),
                ArenaSawAmbientSpark {
                    center: Vec3::new(hazard.center.x, ARENA_TOP_Y + 0.92, hazard.center.z),
                    phase: spark_index as f32 / 5.0 * TAU + index as f32,
                },
                Name::new("Crank saw tooth spark"),
                ArenaGeometry,
            ));
        }
    }
}

fn spawn_vent_spiral_machinery(
    commands: &mut Commands,
    asset_server: &AssetServer,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    arena_index: usize,
    hazards: &[ArenaHazardDefinition],
) {
    if arena_index != VENT_SPIRAL_ARENA_INDEX {
        return;
    }

    let housing_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.08, 0.11, 0.12),
        metallic: 0.68,
        perceptual_roughness: 0.36,
        ..default()
    });
    let rotor_material = materials.add(StandardMaterial {
        base_color: Color::srgb(0.58, 0.66, 0.64),
        metallic: 0.82,
        perceptual_roughness: 0.26,
        ..default()
    });
    let warning_material = materials.add(StandardMaterial {
        base_color: Color::srgb(1.0, 0.34, 0.06),
        emissive: LinearRgba::rgb(1.6, 0.18, 0.01),
        metallic: 0.08,
        perceptual_roughness: 0.42,
        ..default()
    });
    let plume_material = materials.add(StandardMaterial {
        base_color: Color::srgba(0.28, 0.96, 0.88, 0.48),
        emissive: LinearRgba::rgb(0.16, 1.3, 1.0),
        alpha_mode: AlphaMode::Blend,
        cull_mode: None,
        perceptual_roughness: 0.22,
        ..default()
    });
    let blade_mesh = meshes.add(Cuboid::new(0.5, 0.055, 0.16));
    let hub_mesh = meshes.add(Cylinder::new(0.15, 0.1));
    let warning_bulb_mesh = meshes.add(Sphere::new(0.065).mesh().uv(8, 5));
    let plume_mesh = meshes.add(Cone::new(0.22, 1.0));

    for (index, hazard) in hazards
        .iter()
        .filter(|hazard| hazard.kind == ArenaHazardKind::PulseVent)
        .enumerate()
    {
        commands.spawn((
            Mesh3d(meshes.add(Cylinder::new(hazard.radius * 0.93, 0.18))),
            MeshMaterial3d(housing_material.clone()),
            Transform::from_xyz(hazard.center.x, hazard.center.y + 0.03, hazard.center.z),
            Name::new(format!("Vent spiral turbine housing {index}")),
            ArenaGeometry,
        ));

        commands
            .spawn((
                Transform::from_xyz(hazard.center.x, hazard.center.y + 0.14, hazard.center.z),
                Visibility::Visible,
                ArenaVentRotor {
                    pulse_seconds: hazard.pulse_seconds,
                    phase: hazard.phase,
                    spin_direction: if index % 2 == 0 { 1.0 } else { -1.0 },
                },
                Name::new(format!("Vent spiral turbine rotor {index}")),
                ArenaGeometry,
            ))
            .with_children(|parent| {
                for blade_index in 0..5 {
                    let angle = blade_index as f32 / 5.0 * TAU;
                    parent.spawn((
                        Mesh3d(blade_mesh.clone()),
                        MeshMaterial3d(rotor_material.clone()),
                        Transform::from_xyz(angle.cos() * 0.27, 0.0, angle.sin() * 0.27)
                            .with_rotation(Quat::from_rotation_y(-angle)),
                        Name::new("Vent turbine fan blade"),
                    ));
                }
                parent.spawn((
                    Mesh3d(hub_mesh.clone()),
                    MeshMaterial3d(warning_material.clone()),
                    Transform::from_xyz(0.0, 0.055, 0.0),
                    Name::new("Vent turbine energy hub"),
                ));
            });

        let warning_scale = Vec3::splat(1.0);
        commands.spawn((
            Mesh3d(meshes.add(Annulus::new(hazard.radius * 0.98, hazard.radius * 1.14))),
            MeshMaterial3d(warning_material.clone()),
            Transform::from_xyz(hazard.center.x, hazard.center.y + 0.155, hazard.center.z)
                .with_rotation(Quat::from_rotation_x(-PI * 0.5)),
            ArenaVentWarning {
                pulse_seconds: hazard.pulse_seconds,
                phase: hazard.phase,
                base_scale: warning_scale,
            },
            Name::new(format!("Vent spiral warning ring {index}")),
            ArenaGeometry,
        ));

        for bulb_index in 0..8 {
            let angle = bulb_index as f32 / 8.0 * TAU;
            let radius = hazard.radius * 1.08;
            let base_scale = Vec3::splat(if bulb_index % 2 == 0 { 1.0 } else { 0.76 });
            commands.spawn((
                Mesh3d(warning_bulb_mesh.clone()),
                MeshMaterial3d(warning_material.clone()),
                Transform::from_xyz(
                    hazard.center.x + angle.cos() * radius,
                    hazard.center.y + 0.19,
                    hazard.center.z + angle.sin() * radius,
                )
                .with_scale(base_scale),
                ArenaVentWarning {
                    pulse_seconds: hazard.pulse_seconds,
                    phase: hazard.phase + bulb_index as f32 * 0.025,
                    base_scale,
                },
                Name::new("Vent turbine warning lamp"),
                ArenaGeometry,
            ));
        }

        for plume_index in 0..3 {
            let angle = plume_index as f32 / 3.0 * TAU + index as f32 * 0.7;
            let base_y = hazard.center.y + 0.2;
            let full_height = 1.65 + plume_index as f32 * 0.18;
            let base_scale = Vec3::new(0.76, full_height, 0.76);
            commands.spawn((
                Mesh3d(plume_mesh.clone()),
                MeshMaterial3d(plume_material.clone()),
                Transform::from_xyz(
                    hazard.center.x + angle.cos() * 0.2,
                    base_y,
                    hazard.center.z + angle.sin() * 0.2,
                )
                .with_scale(Vec3::new(base_scale.x, 0.001, base_scale.z)),
                ArenaVentPlume {
                    pulse_seconds: hazard.pulse_seconds,
                    phase: hazard.phase + plume_index as f32 * 0.035,
                    base_y,
                    full_height,
                    base_scale,
                },
                Name::new("Vent turbine energy plume"),
                ArenaGeometry,
            ));
        }
    }

    let ufo_position = Vec3::new(5.8, ARENA_TOP_Y + 4.15, -7.0);
    commands.spawn((
        SceneRoot(asset_server.load(
            GltfAssetLabel::Scene(0).from_asset(arena_prop_asset_path("tower/enemy-ufo-a.glb")),
        )),
        Transform::from_translation(ufo_position)
            .with_rotation(Quat::from_rotation_y(0.25))
            .with_scale(Vec3::splat(2.2)),
        ArenaVentUfo {
            base_y: ufo_position.y,
        },
        Name::new("Vent spiral background ufo"),
        ArenaGeometry,
    ));

    let beam_scale = Vec3::new(1.75, 4.0, 1.75);
    commands.spawn((
        SceneRoot(asset_server.load(
            GltfAssetLabel::Scene(0).from_asset(arena_prop_asset_path("tower/enemy-ufo-beam.glb")),
        )),
        Transform::from_xyz(5.8, ARENA_TOP_Y + 0.08, -7.0).with_scale(beam_scale),
        ArenaVentUfoBeam {
            base_y: ARENA_TOP_Y + 0.08,
            base_scale: beam_scale,
        },
        Name::new("Vent spiral background ufo beam"),
        ArenaGeometry,
    ));
}

fn spawn_arena_hazard_markers(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    material: Handle<StandardMaterial>,
    arena_index: usize,
    hazards: &[ArenaHazardDefinition],
) {
    for hazard in hazards.iter().filter(|hazard| {
        hazard.kind != ArenaHazardKind::SawBlade
            && !(arena_index == VENT_SPIRAL_ARENA_INDEX
                && hazard.kind == ArenaHazardKind::PulseVent)
    }) {
        let base_scale = (hazard.pulse_seconds / 2.2).clamp(0.8, 1.3);
        commands.spawn((
            Mesh3d(meshes.add(Annulus::new(hazard.radius * 0.68, hazard.radius))),
            MeshMaterial3d(material.clone()),
            Transform::from_translation(hazard.center)
                .with_rotation(Quat::from_rotation_x(-PI * 0.5))
                .with_scale(Vec3::splat(base_scale)),
            ArenaHazardMarker {
                kind: hazard.kind,
                pulse_seconds: hazard.pulse_seconds,
                phase: hazard.phase,
                base_scale,
                base_y: hazard.center.y,
            },
            ArenaGeometry,
        ));
    }
}

pub fn update_arena_hazard_visuals(
    state: Res<ArenaHazardState>,
    mut markers: Query<(&ArenaHazardMarker, &mut Transform), Without<ArenaCampfireFlame>>,
    mut flames: Query<(&ArenaCampfireFlame, &mut Transform), Without<ArenaHazardMarker>>,
    mut decorative_flames: Query<
        (&ArenaDecorativeFlame, &mut Transform),
        (Without<ArenaHazardMarker>, Without<ArenaCampfireFlame>),
    >,
) {
    for (marker, mut transform) in &mut markers {
        let wave = ((state.elapsed + marker.phase) / marker.pulse_seconds.max(0.1) * TAU).sin();
        let scale = marker.base_scale * arena_hazard_marker_scale(marker.kind, wave);
        transform.scale = Vec3::new(scale, marker.base_scale, scale);
        transform.translation.y = marker.base_y
            + if marker.kind == ArenaHazardKind::BumperNode {
                wave.max(0.0) * 0.14
            } else {
                0.0
            };
    }

    for (flame, mut transform) in &mut flames {
        animate_flame_transform(state.elapsed, flame.phase, flame.base_scale, &mut transform);
    }

    for (flame, mut transform) in &mut decorative_flames {
        animate_flame_transform(state.elapsed, flame.phase, flame.base_scale, &mut transform);
    }
}

fn animate_flame_transform(elapsed: f32, phase: f32, base_scale: Vec3, transform: &mut Transform) {
    let flicker = (elapsed * 9.0 + phase).sin();
    let flutter = (elapsed * 13.0 + phase * 0.7).sin();
    transform.scale = base_scale
        * Vec3::new(
            1.0 - flicker * 0.055,
            1.0 + flicker * 0.1,
            1.0 + flutter * 0.045,
        );
}

pub fn update_arena_pipe_visuals(
    time: Res<Time>,
    state: Res<ArenaPipeState>,
    mut rings: Query<(&ArenaPipePortalRing, &mut Transform), Without<ArenaPipePortalParticle>>,
    mut particles: Query<(&ArenaPipePortalParticle, &mut Transform), Without<ArenaPipePortalRing>>,
) {
    let Some(pipe_pair) = active_arena_definition().pipe_pair else {
        return;
    };
    let elapsed = time.elapsed_secs();

    for (ring, mut transform) in &mut rings {
        let active = state.endpoint_active(ring.endpoint);
        let pulse = (elapsed * if active { 8.0 } else { 3.2 } + ring.phase).sin();
        let scale = 1.0 + pulse * 0.06 + if active { 0.16 } else { 0.0 };
        transform.scale = ring.base_scale * scale;
        transform.rotate_y(time.delta_secs() * if active { 2.8 } else { 1.1 });
    }

    for (particle, mut transform) in &mut particles {
        let Some(center) = pipe_pair.endpoints.get(particle.endpoint).copied() else {
            continue;
        };
        let active = state.endpoint_active(particle.endpoint);
        let speed = if active { 3.8 } else { 1.65 };
        let angle = elapsed * speed + particle.phase;
        let rise =
            (elapsed * if active { 1.9 } else { 1.15 } + particle.phase / TAU).rem_euclid(1.0);
        transform.translation = Vec3::new(
            center.x + angle.cos() * particle.radius,
            particle.base_y + rise * if active { 1.05 } else { 0.62 },
            center.y + angle.sin() * particle.radius,
        );
        transform.scale = Vec3::splat((1.0 - rise).max(0.08) * if active { 1.35 } else { 1.0 });
    }
}

pub fn update_crank_yard_machinery(
    time: Res<Time>,
    mut state: ResMut<ArenaHazardState>,
    fighters: Query<(&FighterInput, &Transform), With<Fighter>>,
    mut levers: Query<
        (&CrankLeverVisual, &mut Transform),
        (
            Without<Fighter>,
            Without<ArenaSawBladeVisual>,
            Without<ArenaSawWarningLight>,
            Without<ArenaSawAmbientSpark>,
        ),
    >,
    mut blades: Query<
        (&ArenaSawBladeVisual, &mut Transform),
        (
            Without<Fighter>,
            Without<CrankLeverVisual>,
            Without<ArenaSawWarningLight>,
            Without<ArenaSawAmbientSpark>,
        ),
    >,
    mut warning_lights: Query<
        (&ArenaSawWarningLight, &mut Transform),
        (
            Without<Fighter>,
            Without<CrankLeverVisual>,
            Without<ArenaSawBladeVisual>,
            Without<ArenaSawAmbientSpark>,
        ),
    >,
    mut sparks: Query<
        (&ArenaSawAmbientSpark, &mut Transform),
        (
            Without<Fighter>,
            Without<CrankLeverVisual>,
            Without<ArenaSawBladeVisual>,
            Without<ArenaSawWarningLight>,
        ),
    >,
) {
    let dt = time.delta_secs();
    let elapsed = time.elapsed_secs();
    let arena_index = active_arena_index();
    state.sync_to_arena(arena_index, active_arena_definition().hazards.len());
    state.crank_lever_toggle_cooldown = (state.crank_lever_toggle_cooldown - dt).max(0.0);

    if arena_index == CRANK_YARD_ARENA_INDEX
        && state.crank_lever_toggle_cooldown <= 0.0
        && fighters.iter().any(|(input, transform)| {
            (input.raw_light_pressed || input.raw_heavy_pressed)
                && Vec2::new(
                    transform.translation.x - CRANK_LEVER_POSITION.x,
                    transform.translation.z - CRANK_LEVER_POSITION.z,
                )
                .length()
                    <= CRANK_LEVER_ATTACK_RADIUS
        })
    {
        state.crank_saws_stopped = !state.crank_saws_stopped;
        state.crank_lever_toggle_cooldown = 0.3;
    }

    for (lever, mut transform) in &mut levers {
        let target = if state.crank_saws_stopped {
            lever.stopped_rotation
        } else {
            lever.running_rotation
        };
        transform.rotation = transform.rotation.slerp(target, (dt * 9.0).min(1.0));
    }

    for (blade, mut transform) in &mut blades {
        if !state.crank_saws_stopped {
            transform.rotate_local_z(blade.spin_speed * dt);
        }
    }

    for (warning, mut transform) in &mut warning_lights {
        let pulse = ((elapsed * 7.0 + warning.phase).sin() * 0.5 + 0.5).powf(3.0);
        transform.scale = warning.base_scale
            * if state.crank_saws_stopped {
                0.28
            } else {
                0.72 + pulse * 0.5
            };
    }

    for (spark, mut transform) in &mut sparks {
        let cycle = elapsed * 2.7 + spark.phase;
        let flare = (cycle.sin() * 0.5 + 0.5).powf(9.0);
        let angle = cycle * 2.3;
        transform.translation = spark.center
            + Vec3::new(
                angle.cos() * (0.46 + flare * 0.28),
                flare * 0.52,
                angle.sin() * 0.34,
            );
        transform.scale = Vec3::splat(if state.crank_saws_stopped {
            0.0
        } else {
            flare * 1.6
        });
    }
}

pub fn update_powder_keg_cannons(
    time: Res<Time>,
    mut commands: Commands,
    assets: Res<ArenaOrdnanceAssets>,
    mut cannon_state: ResMut<PowderKegCannonState>,
    match_state: Res<MatchState>,
    feel: Res<CombatFeelTuning>,
    effect_assets: Res<EffectAssets>,
    mut hitstop: ResMut<Hitstop>,
    mut feedback: ResMut<HitEffects>,
    mut haptics: ResMut<CombatHapticQueue>,
    mut telemetry: ResMut<MatchTelemetry>,
    mut bombs: Query<(Entity, &mut ArenaCannonBomb, &mut Transform)>,
    mut fighters: Query<
        (
            &Fighter,
            &mut FighterStats,
            &mut FighterMotor,
            &mut FighterActionState,
            &FighterStyle,
            &FighterEquipment,
            &Transform,
        ),
        Without<ArenaCannonBomb>,
    >,
) {
    if hitstop.active() {
        return;
    }

    let arena_index = active_arena_index();
    cannon_state.sync_to_arena(arena_index);
    if arena_index != POWDER_KEG_ARENA_INDEX {
        return;
    }

    let dt = time.delta_secs();
    cannon_state.fire_timer -= dt;
    if cannon_state.fire_timer <= 0.0 {
        let (origin, velocity) = powder_cannon_shot(cannon_state.next_cannon);
        commands.spawn((
            Mesh3d(assets.bomb_mesh.clone()),
            MeshMaterial3d(assets.bomb_material.clone()),
            Transform::from_translation(origin),
            ArenaCannonBomb {
                velocity,
                lifetime: 3.4,
            },
            ArenaGeometry,
            Name::new("Powder keg cannon bomb"),
        ));
        cannon_state.next_cannon = (cannon_state.next_cannon + 1) % 2;
        cannon_state.fire_timer += POWDER_CANNON_INTERVAL_SECONDS;
    }

    for (bomb_entity, mut bomb, mut bomb_transform) in &mut bombs {
        bomb.lifetime -= dt;
        bomb.velocity.y -= GRAVITY * dt;
        bomb_transform.translation += bomb.velocity * dt;
        bomb_transform.rotate_x(dt * 8.0);
        bomb_transform.rotate_z(dt * 5.0);

        let position = bomb_transform.translation;
        let ground_hit = ground_height_at(position.x, position.z)
            .is_some_and(|ground_y| position.y <= ground_y + 0.22 && bomb.velocity.y <= 0.0);
        let expired = bomb.lifetime <= 0.0;
        let mut detonated = ground_hit || expired;

        for (fighter, mut stats, mut motor, mut action, style, equipment, transform) in
            &mut fighters
        {
            if !match_state.fighter_can_participate(fighter.id)
                || !can_receive_impact(&stats, &action)
            {
                continue;
            }
            let fighter_center = transform.translation + Vec3::Y * 0.72;
            let hit_radius = if ground_hit || expired {
                POWDER_CANNON_BOMB_RADIUS
            } else {
                0.34 + FIGHTER_RADIUS * stats.item_size_multiplier()
            };
            if fighter_center.distance(position) > hit_radius {
                continue;
            }

            let mut impact =
                powder_cannon_impact_profile().with_hit_effects_enabled(feel.hit_effects_enabled());
            impact.knockback_direction = Some(
                Vec3::new(
                    transform.translation.x - position.x,
                    0.0,
                    transform.translation.z - position.z,
                )
                .normalize_or(Vec3::Z),
            );
            apply_impact(
                &mut commands,
                &effect_assets,
                &mut feedback,
                &mut haptics,
                &mut hitstop,
                &match_state,
                fighter.id,
                &mut stats,
                &mut motor,
                &mut action,
                transform,
                None,
                position,
                impact,
                DamageDefenderProfile::from_loadout(style, equipment),
                &mut telemetry,
            );
            detonated = true;
        }

        if detonated {
            commands.entity(bomb_entity).despawn();
        }
    }
}

fn powder_cannon_shot(index: usize) -> (Vec3, Vec3) {
    let cannon = if index % 2 == 0 {
        Vec3::new(-6.7, ARENA_TOP_Y + 1.05, 1.8)
    } else {
        Vec3::new(6.7, ARENA_TOP_Y + 1.05, -1.8)
    };
    let direction = Vec3::new(-cannon.x, 0.0, -cannon.z).normalize_or(Vec3::X);
    (cannon + direction * 1.0, direction * 7.8 + Vec3::Y * 5.0)
}

fn powder_cannon_impact_profile() -> ImpactProfile {
    let mut profile = impact_profile(
        NEUTRAL_IMPACT_OWNER_ID,
        ImpactSource::Hazard,
        POWDER_CANNON_BOMB_DAMAGE,
        8.4,
        4.2,
        true,
        true,
        18.0,
        ImpactFeedbackIntensity::Heavy,
        ReactionFamilyId::LauncherDown,
    );
    profile.element = DamageElement::Hazard;
    profile
}

pub fn update_vent_spiral_machinery(
    time: Res<Time>,
    state: Res<ArenaHazardState>,
    mut visuals: ParamSet<(
        Query<(&ArenaVentRotor, &mut Transform)>,
        Query<(&ArenaVentWarning, &mut Transform)>,
        Query<(&ArenaVentPlume, &mut Transform)>,
        Query<(&ArenaVentUfo, &mut Transform)>,
        Query<(&ArenaVentUfoBeam, &mut Transform)>,
    )>,
) {
    let dt = time.delta_secs();
    let elapsed = state.elapsed();

    for (rotor, mut transform) in &mut visuals.p0() {
        let active = vent_active_visual_amount(elapsed, rotor.pulse_seconds, rotor.phase);
        let charge = vent_charge_visual_amount(elapsed, rotor.pulse_seconds, rotor.phase);
        transform.rotate_y(rotor.spin_direction * (2.2 + charge * 4.0 + active * 14.0) * dt);
    }

    for (warning, mut transform) in &mut visuals.p1() {
        let active = vent_active_visual_amount(elapsed, warning.pulse_seconds, warning.phase);
        let charge = vent_charge_visual_amount(elapsed, warning.pulse_seconds, warning.phase);
        let pulse = (elapsed * 15.0 + warning.phase * 3.0).sin().abs();
        transform.scale = warning.base_scale
            * (0.72 + charge * (0.32 + pulse * 0.2) + active * (0.35 + pulse * 0.16));
    }

    for (plume, mut transform) in &mut visuals.p2() {
        let active = vent_active_visual_amount(elapsed, plume.pulse_seconds, plume.phase);
        let flutter = (elapsed * 18.0 + plume.phase * 5.0).sin() * 0.06;
        let height_amount = (active + flutter * active).clamp(0.001, 1.0);
        transform.translation.y = plume.base_y + plume.full_height * height_amount * 0.5;
        transform.scale = Vec3::new(
            plume.base_scale.x * (0.58 + active * 0.42),
            plume.base_scale.y * height_amount,
            plume.base_scale.z * (0.58 + active * 0.42),
        );
    }

    for (ufo, mut transform) in &mut visuals.p3() {
        transform.translation.y = ufo.base_y + (elapsed * 1.7).sin() * 0.16;
        transform.rotate_y(dt * 0.42);
    }

    let sequence_amount = active_arena_definition()
        .hazards
        .iter()
        .filter(|hazard| hazard.kind == ArenaHazardKind::PulseVent)
        .map(|hazard| {
            vent_active_visual_amount(elapsed, hazard.pulse_seconds, hazard.phase)
                + vent_charge_visual_amount(elapsed, hazard.pulse_seconds, hazard.phase) * 0.28
        })
        .fold(0.0_f32, f32::max)
        .clamp(0.0, 1.0);
    for (beam, mut transform) in &mut visuals.p4() {
        let shimmer = (elapsed * 8.0).sin() * 0.04;
        let amount = (0.58 + sequence_amount * 0.42 + shimmer).clamp(0.5, 1.05);
        transform.translation.y = beam.base_y;
        transform.scale = Vec3::new(
            beam.base_scale.x * amount,
            beam.base_scale.y * (0.94 + sequence_amount * 0.06),
            beam.base_scale.z * amount,
        );
    }
}

fn vent_cycle_progress(elapsed: f32, pulse_seconds: f32, phase: f32) -> f32 {
    (elapsed + phase).rem_euclid(pulse_seconds.max(0.1)) / pulse_seconds.max(0.1)
}

fn vent_active_visual_amount(elapsed: f32, pulse_seconds: f32, phase: f32) -> f32 {
    let progress = vent_cycle_progress(elapsed, pulse_seconds, phase);
    let active_fraction = arena_hazard_active_fraction(ArenaHazardKind::PulseVent);
    if progress > active_fraction {
        return 0.0;
    }
    let active_progress = progress / active_fraction;
    0.25 + (active_progress * PI).sin().max(0.0) * 0.75
}

fn vent_charge_visual_amount(elapsed: f32, pulse_seconds: f32, phase: f32) -> f32 {
    let progress = vent_cycle_progress(elapsed, pulse_seconds, phase);
    ((progress - 0.72) / 0.28).clamp(0.0, 1.0)
}

pub fn update_arena_pipe_transits(
    time: Res<Time>,
    mut state: ResMut<ArenaPipeState>,
    mut fighters: ParamSet<(
        Query<(&Fighter, &Transform)>,
        Query<(
            &Fighter,
            &mut FighterStats,
            &mut FighterMotor,
            &mut FighterActionState,
            &mut Transform,
        )>,
    )>,
) {
    let arena_index = active_arena_index();
    state.sync_to_arena(arena_index);
    let Some(pipe_pair) = active_arena_definition().pipe_pair else {
        return;
    };
    let dt = time.delta_secs();
    let snapshots: Vec<(usize, Vec3)> = fighters
        .p0()
        .iter()
        .map(|(fighter, transform)| (fighter.id, transform.translation))
        .collect();

    for (fighter, mut stats, mut motor, mut action, mut transform) in &mut fighters.p1() {
        if fighter.id >= FIGHTER_COUNT {
            continue;
        }

        match state.fighters[fighter.id] {
            FighterPipeState::Ready {
                candidate,
                dwell,
                cooldown,
            } => {
                let cooldown = (cooldown - dt).max(0.0);
                let endpoint = if cooldown == 0.0 {
                    pipe_entry_endpoint(pipe_pair, transform.translation, &motor, action.action)
                } else {
                    None
                };
                let descending_entry = endpoint.is_some()
                    && !motor.grounded
                    && action.action == FighterAction::Jumping
                    && motor.velocity.y <= 0.0;
                let next_dwell = if descending_entry {
                    PIPE_ENTRY_DWELL_SECONDS
                } else if endpoint.is_some() && endpoint == candidate {
                    dwell + dt
                } else {
                    0.0
                };

                if let Some(source) = endpoint
                    && next_dwell >= PIPE_ENTRY_DWELL_SECONDS
                {
                    let destination = 1 - source;
                    state.fighters[fighter.id] = FighterPipeState::Transit {
                        source,
                        destination,
                        elapsed: 0.0,
                        entry_y: transform.translation.y,
                        base_scale: transform.scale,
                    };
                    motor.velocity = Vec3::ZERO;
                    motor.grounded = false;
                    *action = FighterActionState::default();
                    action.action = FighterAction::Respawning;
                    stats.invulnerability = stats.invulnerability.max(0.25);
                } else {
                    state.fighters[fighter.id] = FighterPipeState::Ready {
                        candidate: endpoint,
                        dwell: next_dwell,
                        cooldown,
                    };
                }
            }
            FighterPipeState::Transit {
                source,
                destination,
                elapsed,
                entry_y,
                base_scale,
            } => {
                let destination_center = pipe_pair.endpoints[destination];
                let exit_occupied = snapshots.iter().any(|(other_id, position)| {
                    *other_id != fighter.id
                        && Vec2::new(
                            position.x - destination_center.x,
                            position.z - destination_center.y,
                        )
                        .length()
                            < PIPE_EXIT_CLEARANCE_RADIUS
                        && position.y >= pipe_pair.top_y - 0.2
                });
                let hidden_boundary = PIPE_ENTER_SECONDS + PIPE_TRAVEL_SECONDS;
                let next_elapsed = if exit_occupied && elapsed >= hidden_boundary {
                    hidden_boundary
                } else {
                    elapsed + dt
                };
                let sample = pipe_transit_sample(
                    pipe_pair,
                    source,
                    destination,
                    next_elapsed,
                    entry_y,
                    base_scale,
                );

                transform.translation = sample.position;
                transform.scale = sample.scale;
                motor.velocity = Vec3::ZERO;
                motor.grounded = false;
                *action = FighterActionState::default();
                action.action = FighterAction::Respawning;
                stats.invulnerability = stats.invulnerability.max(0.25);

                if sample.complete {
                    transform.translation = Vec3::new(
                        destination_center.x,
                        pipe_pair.top_y + 0.06,
                        destination_center.y,
                    );
                    transform.scale = base_scale;
                    motor.facing = Vec3::new(-destination_center.x, 0.0, -destination_center.y)
                        .normalize_or_zero();
                    motor.velocity = motor.facing * PIPE_EXIT_INWARD_SPEED;
                    motor.velocity.y = PIPE_EXIT_HOP_SPEED;
                    motor.grounded = false;
                    *action = FighterActionState::default();
                    action.action = FighterAction::Jumping;
                    stats.invulnerability = stats.invulnerability.max(0.35);
                    state.fighters[fighter.id] = FighterPipeState::Ready {
                        candidate: None,
                        dwell: 0.0,
                        cooldown: PIPE_REENTRY_COOLDOWN_SECONDS,
                    };
                } else {
                    state.fighters[fighter.id] = FighterPipeState::Transit {
                        source,
                        destination,
                        elapsed: next_elapsed,
                        entry_y,
                        base_scale,
                    };
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PipeTransitSample {
    position: Vec3,
    scale: Vec3,
    complete: bool,
}

fn pipe_entry_endpoint(
    pipe_pair: ArenaPipePairDefinition,
    position: Vec3,
    motor: &FighterMotor,
    action: FighterAction,
) -> Option<usize> {
    let grounded_entry = motor.grounded
        && matches!(action, FighterAction::Idle | FighterAction::Moving)
        && (position.y - pipe_pair.top_y).abs() <= 0.18;
    let descending_entry = !motor.grounded
        && action == FighterAction::Jumping
        && motor.velocity.y <= 0.0
        && position.y >= pipe_pair.top_y - 0.12
        && position.y <= pipe_pair.top_y + 0.58;
    if !grounded_entry && !descending_entry {
        return None;
    }

    pipe_pair.endpoints.iter().position(|center| {
        Vec2::new(position.x - center.x, position.z - center.y).length() <= pipe_pair.trigger_radius
    })
}

fn pipe_transit_sample(
    pipe_pair: ArenaPipePairDefinition,
    source: usize,
    destination: usize,
    elapsed: f32,
    entry_y: f32,
    base_scale: Vec3,
) -> PipeTransitSample {
    let source_center = pipe_pair.endpoints[source];
    let destination_center = pipe_pair.endpoints[destination];
    let hidden_y = pipe_pair.top_y - PIPE_SINK_DEPTH;
    let total = PIPE_ENTER_SECONDS + PIPE_TRAVEL_SECONDS + PIPE_EXIT_SECONDS;

    if elapsed < PIPE_ENTER_SECONDS {
        let t = smooth_step(elapsed / PIPE_ENTER_SECONDS);
        return PipeTransitSample {
            position: Vec3::new(
                source_center.x,
                entry_y + (hidden_y - entry_y) * t,
                source_center.y,
            ),
            scale: base_scale * (1.0 + (0.45 - 1.0) * t),
            complete: false,
        };
    }

    if elapsed < PIPE_ENTER_SECONDS + PIPE_TRAVEL_SECONDS {
        return PipeTransitSample {
            position: Vec3::new(destination_center.x, hidden_y, destination_center.y),
            scale: base_scale * 0.45,
            complete: false,
        };
    }

    let t = smooth_step(
        ((elapsed - PIPE_ENTER_SECONDS - PIPE_TRAVEL_SECONDS) / PIPE_EXIT_SECONDS).clamp(0.0, 1.0),
    );
    PipeTransitSample {
        position: Vec3::new(
            destination_center.x,
            hidden_y + (pipe_pair.top_y - hidden_y) * t,
            destination_center.y,
        ),
        scale: base_scale * (0.45 + (1.0 - 0.45) * t),
        complete: elapsed >= total,
    }
}

fn smooth_step(value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    value * value * (3.0 - 2.0 * value)
}

fn arena_hazard_marker_scale(kind: ArenaHazardKind, wave: f32) -> f32 {
    match kind {
        ArenaHazardKind::PulseVent => 1.0 + wave.max(0.0) * 0.28,
        ArenaHazardKind::SnareField => 0.94 + (wave + 1.0) * 0.08,
        ArenaHazardKind::BumperNode => 0.96 + wave.max(0.0) * 0.2,
        ArenaHazardKind::Campfire => 0.98 + (wave + 1.0) * 0.035,
        ArenaHazardKind::SawBlade => 1.0,
    }
}

pub fn update_arena_hazards(
    time: Res<Time>,
    mut commands: Commands,
    effect_assets: Res<EffectAssets>,
    mut state: ResMut<ArenaHazardState>,
    match_state: Res<MatchState>,
    feel: Res<CombatFeelTuning>,
    mut hitstop: ResMut<Hitstop>,
    mut camera_effects: ResMut<HitEffects>,
    mut haptics: ResMut<CombatHapticQueue>,
    mut telemetry: ResMut<MatchTelemetry>,
    mut burns: Query<(Entity, &mut ArenaFighterBurn)>,
    mut fighters: Query<(
        Entity,
        &Fighter,
        &mut FighterStats,
        &mut FighterMotor,
        &mut FighterActionState,
        &FighterStyle,
        &FighterEquipment,
        &Transform,
    )>,
) {
    if hitstop.active() {
        return;
    }

    let arena_index = active_arena_index();
    let arena = active_arena_definition();
    let dt = time.delta_secs();
    state.sync_to_arena(arena_index, arena.hazards.len());
    state.elapsed += dt;
    state.tick_cooldowns(dt);

    for (fighter_entity, mut burn) in &mut burns {
        burn.remaining = (burn.remaining - dt).max(0.0);
        if burn.remaining <= 0.0 {
            commands.entity(fighter_entity).remove::<ArenaFighterBurn>();
        }
    }

    for (hazard_index, hazard) in arena.hazards.iter().enumerate() {
        if hazard.kind == ArenaHazardKind::SawBlade && state.crank_saws_stopped {
            continue;
        }
        if !arena_hazard_is_active_for_kind(state.elapsed, hazard) {
            continue;
        }

        let Some(cooldowns) = state.hit_cooldowns.get_mut(hazard_index) else {
            continue;
        };

        for (
            fighter_entity,
            fighter,
            mut stats,
            mut motor,
            mut action,
            style,
            equipment,
            transform,
        ) in &mut fighters
        {
            if fighter.id >= FIGHTER_COUNT
                || !match_state.fighter_can_participate(fighter.id)
                || cooldowns[fighter.id] > 0.0
                || !can_receive_impact(&stats, &action)
                || !arena_hazard_overlaps(hazard, transform.translation)
            {
                continue;
            }

            if hazard.kind == ArenaHazardKind::Campfire {
                commands
                    .entity(fighter_entity)
                    .insert(ArenaFighterBurn::new(ARENA_HAZARD_CAMPFIRE_BURN_SECONDS));
                spawn_burning_fighter_effect(
                    &mut commands,
                    &effect_assets,
                    fighter_entity,
                    ARENA_HAZARD_CAMPFIRE_BURN_SECONDS,
                );
            }

            if hazard.kind == ArenaHazardKind::SnareField {
                motor.velocity.x *= 0.55;
                motor.velocity.z *= 0.55;
            }

            if hazard.kind == ArenaHazardKind::SawBlade {
                spawn_machine_scratch(
                    &mut commands,
                    &effect_assets,
                    fighter_entity,
                    transform.translation,
                );
            }

            let mut impact = if hazard.kind == ArenaHazardKind::BumperNode {
                bumper_impact_profile(Vec2::new(motor.velocity.x, motor.velocity.z).length())
            } else {
                arena_hazard_impact_profile(hazard.kind)
            }
            .with_hit_effects_enabled(feel.hit_effects_enabled());
            if matches!(
                hazard.kind,
                ArenaHazardKind::SawBlade | ArenaHazardKind::BumperNode
            ) {
                impact.knockback_direction = Some(saw_knockback_direction(
                    transform.translation,
                    hazard.center,
                    motor.facing,
                ));
            }

            apply_impact(
                &mut commands,
                &effect_assets,
                &mut camera_effects,
                &mut haptics,
                &mut hitstop,
                &match_state,
                fighter.id,
                &mut stats,
                &mut motor,
                &mut action,
                transform,
                None,
                hazard.center,
                impact,
                DamageDefenderProfile::from_loadout(style, equipment),
                &mut telemetry,
            );
            cooldowns[fighter.id] = arena_hazard_hit_cooldown(hazard.kind);
        }
    }
}

fn saw_knockback_direction(
    fighter_position: Vec3,
    hazard_center: Vec3,
    fighter_facing: Vec3,
) -> Vec3 {
    let away_from_blade = Vec3::new(
        fighter_position.x - hazard_center.x,
        0.0,
        fighter_position.z - hazard_center.z,
    )
    .normalize_or_zero();
    if away_from_blade.length_squared() > 0.0 {
        return away_from_blade;
    }

    let away_from_arena_center =
        Vec3::new(hazard_center.x, 0.0, hazard_center.z).normalize_or_zero();
    if away_from_arena_center.length_squared() > 0.0 {
        away_from_arena_center
    } else {
        Vec3::new(fighter_facing.x, 0.0, fighter_facing.z).normalize_or(Vec3::X)
    }
}

pub fn ground_height_at(x: f32, z: f32) -> Option<f32> {
    ground_height_at_with_radius(x, z, 0.0)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GroundSupport {
    Firm(f32),
    Grace(f32),
    Airborne,
}

impl GroundSupport {
    pub fn height(self) -> Option<f32> {
        match self {
            Self::Firm(height) | Self::Grace(height) => Some(height),
            Self::Airborne => None,
        }
    }
}

pub fn ground_height_at_with_radius(x: f32, z: f32, support_radius: f32) -> Option<f32> {
    ground_support_at_with_radius(x, z, support_radius).height()
}

pub fn ground_support_at_with_radius(x: f32, z: f32, support_radius: f32) -> GroundSupport {
    let arena = active_arena_definition();
    ground_support_for_arena_with_radius(arena, x, z, support_radius)
}

pub fn ground_support_for_arena_with_radius(
    arena: &ArenaDefinition,
    x: f32,
    z: f32,
    support_radius: f32,
) -> GroundSupport {
    let ledge_grace =
        (support_radius * LEDGE_SUPPORT_GRACE_SCALE).clamp(0.0, LEDGE_SUPPORT_GRACE_MAX);
    let mut best = None;

    for shape in arena.ground_shapes {
        if let Some(support) = ground_shape_support(shape, x, z, ledge_grace) {
            best = Some(prefer_ground_support(best, support));
        }
    }

    for platform in arena.gameplay_platforms() {
        let dx = (x - platform.center.x).abs();
        let dz = (z - platform.center.y).abs();
        let support = if is_authored_platform(arena, platform)
            || arena.visual_theme == ArenaVisualTheme::Reactor
        {
            match platform.support_at(Vec2::new(x, z), ledge_grace) {
                Some(crate::arena_barriers::BarrierSupport::Firm) => {
                    Some(GroundSupport::Firm(platform.top_y))
                }
                Some(crate::arena_barriers::BarrierSupport::Grace) => {
                    Some(GroundSupport::Grace(platform.top_y))
                }
                None => None,
            }
        } else if let Some((outer_radius, opening_radius)) =
            circular_platform_profile(arena, platform)
        {
            let distance = Vec2::new(x - platform.center.x, z - platform.center.y).length();
            if opening_radius > 0.0 && distance <= opening_radius {
                None
            } else if distance <= outer_radius {
                Some(GroundSupport::Firm(platform.top_y))
            } else if distance <= outer_radius + ledge_grace {
                Some(GroundSupport::Grace(platform.top_y))
            } else {
                None
            }
        } else if dx <= platform.half_extents.x && dz <= platform.half_extents.y {
            Some(GroundSupport::Firm(platform.top_y))
        } else if dx <= platform.half_extents.x + ledge_grace
            && dz <= platform.half_extents.y + ledge_grace
        {
            Some(GroundSupport::Grace(platform.top_y))
        } else {
            None
        };
        if let Some(support) = support {
            best = Some(prefer_ground_support(best, support));
        }
    }

    for collider in arena_prop_barriers(arena) {
        let support = match collider.definition.support_at(Vec2::new(x, z), ledge_grace) {
            Some(crate::arena_barriers::BarrierSupport::Firm) => {
                Some(GroundSupport::Firm(collider.definition.top_y))
            }
            Some(crate::arena_barriers::BarrierSupport::Grace) => {
                Some(GroundSupport::Grace(collider.definition.top_y))
            }
            None => None,
        };
        if let Some(support) = support {
            best = Some(prefer_ground_support(best, support));
        }
    }

    if let Some(pipe_pair) = arena.pipe_pair {
        for endpoint in pipe_pair.endpoints {
            let pipe = pipe_barrier(pipe_pair, endpoint);
            let support = match pipe.support_at(Vec2::new(x, z), ledge_grace) {
                Some(crate::arena_barriers::BarrierSupport::Firm) => {
                    Some(GroundSupport::Firm(pipe.top_y))
                }
                Some(crate::arena_barriers::BarrierSupport::Grace) => {
                    Some(GroundSupport::Grace(pipe.top_y))
                }
                None => None,
            };
            if let Some(support) = support {
                best = Some(prefer_ground_support(best, support));
            }
        }
    }

    best.unwrap_or(GroundSupport::Airborne)
}

fn ground_shape_support(
    shape: &ArenaGroundShape,
    x: f32,
    z: f32,
    ledge_grace: f32,
) -> Option<GroundSupport> {
    match *shape {
        ArenaGroundShape::Circle {
            center,
            radius,
            top_y,
        } => {
            let distance = Vec2::new(x - center.x, z - center.y).length();
            if distance <= radius {
                Some(GroundSupport::Firm(top_y))
            } else if distance <= radius + ledge_grace {
                Some(GroundSupport::Grace(top_y))
            } else {
                None
            }
        }
        ArenaGroundShape::Rectangle {
            center,
            half_extents,
            yaw,
            top_y,
        } => {
            let offset = Vec2::new(x - center.x, z - center.y);
            let cos = yaw.cos();
            let sin = yaw.sin();
            let local = Vec2::new(
                cos * offset.x + sin * offset.y,
                -sin * offset.x + cos * offset.y,
            );
            if local.x.abs() <= half_extents.x && local.y.abs() <= half_extents.y {
                Some(GroundSupport::Firm(top_y))
            } else if local.x.abs() <= half_extents.x + ledge_grace
                && local.y.abs() <= half_extents.y + ledge_grace
            {
                Some(GroundSupport::Grace(top_y))
            } else {
                None
            }
        }
    }
}

fn prefer_ground_support(
    current: Option<GroundSupport>,
    candidate: GroundSupport,
) -> GroundSupport {
    let Some(current) = current else {
        return candidate;
    };
    let current_height = current.height().unwrap_or(f32::NEG_INFINITY);
    let candidate_height = candidate.height().unwrap_or(f32::NEG_INFINITY);
    if candidate_height > current_height
        || (candidate_height == current_height
            && matches!(candidate, GroundSupport::Firm(_))
            && matches!(current, GroundSupport::Grace(_)))
    {
        candidate
    } else {
        current
    }
}

pub fn resolve_platform_side_collision(position: Vec3, radius: f32) -> Vec3 {
    let arena = active_arena_definition();
    resolve_platform_side_collision_for_arena(arena, position, radius)
}

fn resolve_platform_side_collision_for_arena(
    arena: &ArenaDefinition,
    position: Vec3,
    radius: f32,
) -> Vec3 {
    let mut resolved = position;
    for platform in arena.gameplay_platforms() {
        resolved = if let Some((collider_radius, opening_radius)) =
            circular_platform_profile(arena, platform)
        {
            resolve_circular_platform_side_collision_against(
                resolved,
                radius,
                platform,
                collider_radius,
                opening_radius,
            )
        } else if is_authored_platform(arena, platform)
            || arena.visual_theme == ArenaVisualTheme::Reactor
        {
            if platform.top_y <= PLATFORM_SIDE_COLLISION_MIN_TOP_Y {
                resolved
            } else {
                platform.resolve_side_collision(
                    resolved,
                    radius,
                    crate::constants::LANDING_SNAP_TOLERANCE,
                )
            }
        } else {
            resolve_platform_side_collision_against(resolved, radius, platform)
        };
    }

    if let Some(pipe_pair) = arena.pipe_pair {
        for endpoint in pipe_pair.endpoints {
            let pipe = pipe_barrier(pipe_pair, endpoint);
            resolved = resolve_circular_platform_side_collision_against(
                resolved,
                radius,
                &pipe,
                pipe_pair.collider_radius,
                0.0,
            );
        }
    }
    for collider in arena_prop_barriers(arena) {
        if collider.behavior == PropBarrierBehavior::OneWayTop
            || collider.definition.top_y <= PLATFORM_SIDE_COLLISION_MIN_TOP_Y
        {
            continue;
        }
        resolved = collider.definition.resolve_side_collision(
            resolved,
            radius,
            crate::constants::LANDING_SNAP_TOLERANCE,
        );
    }
    resolved
}

fn is_authored_platform(arena: &ArenaDefinition, candidate: &PlatformDefinition) -> bool {
    arena
        .platforms
        .iter()
        .any(|platform| std::ptr::eq(platform, candidate))
}

fn circular_platform_profile(
    arena: &ArenaDefinition,
    platform: &PlatformDefinition,
) -> Option<(f32, f32)> {
    if let Some(pipe_pair) = arena.pipe_pair
        && platform.top_y == pipe_pair.top_y
        && pipe_pair.endpoints.contains(&platform.center)
    {
        // The teleport still uses trigger_radius, but the visible pipe top is a full landing disc.
        return Some((pipe_pair.collider_radius, 0.0));
    }

    None
}

fn resolve_circular_platform_side_collision_against(
    position: Vec3,
    fighter_radius: f32,
    platform: &PlatformDefinition,
    platform_radius: f32,
    opening_radius: f32,
) -> Vec3 {
    let offset = Vec2::new(
        position.x - platform.center.x,
        position.z - platform.center.y,
    );
    let distance = offset.length();
    let expanded_radius = platform_radius + fighter_radius;
    let clears_lip = position.y >= platform.top_y - crate::constants::LANDING_SNAP_TOLERANCE * 2.0;

    if (opening_radius > 0.0 && distance <= opening_radius)
        || distance >= expanded_radius
        || clears_lip
        || position.y > platform.top_y + 0.7
    {
        return position;
    }

    let direction = offset.normalize_or(Vec2::X);
    Vec3::new(
        platform.center.x + direction.x * expanded_radius,
        position.y,
        platform.center.y + direction.y * expanded_radius,
    )
}

fn pipe_barrier(pipe_pair: ArenaPipePairDefinition, endpoint: Vec2) -> PlatformDefinition {
    PlatformDefinition::circle(
        endpoint.x,
        endpoint.y,
        pipe_pair.collider_radius,
        pipe_pair.top_y,
    )
}

pub fn resolve_platform_side_collision_against(
    position: Vec3,
    radius: f32,
    platform: &PlatformDefinition,
) -> Vec3 {
    if platform.top_y <= PLATFORM_SIDE_COLLISION_MIN_TOP_Y {
        return position;
    }

    let dx = position.x - platform.center.x;
    let dz = position.z - platform.center.y;
    let expanded_x = platform.half_extents.x + radius;
    let expanded_z = platform.half_extents.y + radius;
    let inside_expanded = dx.abs() < expanded_x && dz.abs() < expanded_z;
    let inside_top = dx.abs() <= platform.half_extents.x
        && dz.abs() <= platform.half_extents.y
        && position.y >= platform.top_y - 0.05;

    if !inside_expanded || inside_top || position.y > platform.top_y + 0.7 {
        return position;
    }

    let push_x = expanded_x - dx.abs();
    let push_z = expanded_z - dz.abs();
    if push_x < push_z {
        Vec3::new(
            platform.center.x + expanded_x * dx.signum(),
            position.y,
            position.z,
        )
    } else {
        Vec3::new(
            position.x,
            position.y,
            platform.center.y + expanded_z * dz.signum(),
        )
    }
}

#[cfg(test)]
pub fn arena_hazard_is_active(elapsed: f32, pulse_seconds: f32) -> bool {
    let cycle = pulse_seconds.max(0.1);
    elapsed.rem_euclid(cycle) <= cycle * 0.36
}

pub fn arena_hazard_is_active_for_kind(elapsed: f32, hazard: &ArenaHazardDefinition) -> bool {
    let cycle = hazard.pulse_seconds.max(0.1);
    (elapsed + hazard.phase).rem_euclid(cycle) <= cycle * arena_hazard_active_fraction(hazard.kind)
}

fn arena_hazard_active_fraction(kind: ArenaHazardKind) -> f32 {
    match kind {
        ArenaHazardKind::PulseVent => 0.32,
        ArenaHazardKind::SnareField => 0.68,
        ArenaHazardKind::BumperNode => 1.0,
        ArenaHazardKind::Campfire => 1.0,
        ArenaHazardKind::SawBlade => 1.0,
    }
}

fn arena_hazard_hit_cooldown(kind: ArenaHazardKind) -> f32 {
    match kind {
        ArenaHazardKind::PulseVent => 1.05,
        ArenaHazardKind::SnareField => 0.56,
        ArenaHazardKind::BumperNode => 0.82,
        ArenaHazardKind::Campfire => 0.82,
        ArenaHazardKind::SawBlade => 0.68,
    }
}

fn arena_hazard_overlaps(hazard: &ArenaHazardDefinition, fighter_position: Vec3) -> bool {
    let flat = Vec2::new(
        fighter_position.x - hazard.center.x,
        fighter_position.z - hazard.center.z,
    );
    flat.length() <= hazard.radius + FIGHTER_RADIUS
        && arena_hazard_affects_height(hazard, fighter_position.y)
}

pub fn arena_hazard_affects_height(hazard: &ArenaHazardDefinition, fighter_y: f32) -> bool {
    let offset = fighter_y - hazard.center.y;
    let (below, above) = match hazard.kind {
        ArenaHazardKind::PulseVent => (0.32, 2.35),
        ArenaHazardKind::SnareField => (0.35, 0.8),
        ArenaHazardKind::BumperNode => (0.45, 1.45),
        ArenaHazardKind::Campfire => (0.3, 1.55),
        ArenaHazardKind::SawBlade => (0.4, 1.35),
    };
    offset >= -below && offset <= above
}

fn arena_hazard_impact_profile(kind: ArenaHazardKind) -> ImpactProfile {
    let mut profile = match kind {
        ArenaHazardKind::PulseVent => impact_profile(
            NEUTRAL_IMPACT_OWNER_ID,
            ImpactSource::Hazard,
            ARENA_HAZARD_PULSE_DAMAGE,
            ARENA_HAZARD_PULSE_KNOCKBACK,
            4.1,
            true,
            true,
            16.0,
            ImpactFeedbackIntensity::Heavy,
            ReactionFamilyId::LauncherDown,
        ),
        ArenaHazardKind::SnareField => impact_profile(
            NEUTRAL_IMPACT_OWNER_ID,
            ImpactSource::Hazard,
            ARENA_HAZARD_SNARE_DAMAGE,
            ARENA_HAZARD_SNARE_KNOCKBACK,
            1.0,
            false,
            true,
            10.0,
            ImpactFeedbackIntensity::Light,
            ReactionFamilyId::ShortStandingStagger,
        ),
        ArenaHazardKind::BumperNode => bumper_impact_profile(0.0),
        ArenaHazardKind::Campfire => impact_profile(
            NEUTRAL_IMPACT_OWNER_ID,
            ImpactSource::Hazard,
            ARENA_HAZARD_CAMPFIRE_DAMAGE,
            ARENA_HAZARD_CAMPFIRE_KNOCKBACK,
            ARENA_HAZARD_CAMPFIRE_LAUNCH,
            true,
            false,
            12.0,
            ImpactFeedbackIntensity::Heavy,
            ReactionFamilyId::LauncherDown,
        ),
        ArenaHazardKind::SawBlade => impact_profile(
            NEUTRAL_IMPACT_OWNER_ID,
            ImpactSource::Hazard,
            ARENA_HAZARD_SAW_DAMAGE,
            ARENA_HAZARD_SAW_KNOCKBACK,
            ARENA_HAZARD_SAW_LAUNCH,
            true,
            false,
            18.0,
            ImpactFeedbackIntensity::Heavy,
            ReactionFamilyId::LauncherDown,
        ),
    };
    profile.element = DamageElement::Hazard;
    profile
}

fn bumper_impact_profile(planar_speed: f32) -> ImpactProfile {
    let speed_factor = ((planar_speed - 2.0) / 9.0).clamp(0.0, 1.0);
    impact_profile(
        NEUTRAL_IMPACT_OWNER_ID,
        ImpactSource::Hazard,
        ARENA_HAZARD_BUMPER_DAMAGE * (0.45 + speed_factor * 1.55),
        ARENA_HAZARD_BUMPER_KNOCKBACK * (0.8 + speed_factor * 1.0),
        2.4 + speed_factor * 4.2,
        speed_factor >= 0.62,
        true,
        16.0 + speed_factor * 12.0,
        ImpactFeedbackIntensity::Heavy,
        if speed_factor >= 0.62 {
            ReactionFamilyId::LauncherDown
        } else {
            ReactionFamilyId::LightAirPop
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arena_preview_layers_include_gameplay_and_preview_cameras() {
        let layers = arena_geometry_render_layers();

        assert!(layers.intersects(&RenderLayers::default()));
        assert!(layers.intersects(&RenderLayers::layer(ARENA_PREVIEW_RENDER_LAYER)));
        assert_ne!(ARENA_PREVIEW_RENDER_LAYER, 0);
    }

    #[test]
    fn radius_support_extends_platform_ground_query_slightly() {
        let platform = active_arena_definition().platforms[0];
        let x = platform.center.x + platform.half_extents.x + 0.08;
        assert_eq!(ground_height_at(x, platform.center.y), None);
        assert_eq!(
            ground_height_at_with_radius(x, platform.center.y, 0.4),
            Some(platform.top_y)
        );
        assert_eq!(
            ground_support_at_with_radius(x, platform.center.y, 0.4),
            GroundSupport::Grace(platform.top_y)
        );
        assert_eq!(
            ground_height_at_with_radius(
                platform.center.x + platform.half_extents.x + 0.2,
                platform.center.y,
                0.4,
            ),
            None
        );
    }

    #[test]
    fn platform_side_collision_pushes_out_of_margin_not_top() {
        let platform = PlatformDefinition::new(0.0, 0.0, 1.0, 1.0, ARENA_TOP_Y + 0.4);
        let side = resolve_platform_side_collision_against(
            Vec3::new(1.2, ARENA_TOP_Y, 0.0),
            0.4,
            &platform,
        );
        assert!(side.x > 1.2);

        let top = resolve_platform_side_collision_against(
            Vec3::new(0.0, platform.top_y, 0.0),
            0.4,
            &platform,
        );
        assert_eq!(top, Vec3::new(0.0, platform.top_y, 0.0));
    }

    #[test]
    fn round_pipe_collision_does_not_create_invisible_square_corners() {
        let crank = &arena_definitions()[CRANK_YARD_ARENA_INDEX];
        let pipe_pair = crank.pipe_pair.expect("Crank Yard pipe pair");
        let pipe = pipe_barrier(pipe_pair, pipe_pair.endpoints[0]);
        let corner = Vec3::new(pipe.center.x + 1.1, ARENA_TOP_Y, pipe.center.y + 1.1);
        let side = Vec3::new(pipe.center.x + 0.9, ARENA_TOP_Y, pipe.center.y);
        let landing_approach = Vec3::new(
            pipe.center.x + pipe_pair.collider_radius + FIGHTER_RADIUS * 0.5,
            pipe.top_y - crate::constants::LANDING_SNAP_TOLERANCE * 2.0,
            pipe.center.y,
        );

        assert_eq!(
            resolve_circular_platform_side_collision_against(
                corner,
                FIGHTER_RADIUS,
                &pipe,
                pipe_pair.collider_radius,
                pipe_pair.trigger_radius,
            ),
            corner
        );
        assert!(
            resolve_circular_platform_side_collision_against(
                side,
                FIGHTER_RADIUS,
                &pipe,
                pipe_pair.collider_radius,
                pipe_pair.trigger_radius,
            )
            .x > side.x
        );
        assert_eq!(
            resolve_circular_platform_side_collision_against(
                landing_approach,
                FIGHTER_RADIUS,
                &pipe,
                pipe_pair.collider_radius,
                0.0,
            ),
            landing_approach
        );

        let opening = Vec3::new(pipe.center.x, ARENA_TOP_Y, pipe.center.y);
        assert_ne!(
            resolve_circular_platform_side_collision_against(
                opening,
                FIGHTER_RADIUS,
                &pipe,
                pipe_pair.collider_radius,
                0.0,
            ),
            opening
        );

        let corner_support = ground_support_for_arena_with_radius(
            crank,
            pipe.center.x + pipe_pair.collider_radius * 0.8,
            pipe.center.y + pipe_pair.collider_radius * 0.8,
            0.0,
        );
        assert_ne!(corner_support.height(), Some(pipe.top_y));
        assert_eq!(
            ground_support_for_arena_with_radius(crank, pipe.center.x, pipe.center.y, 0.0,)
                .height(),
            Some(pipe.top_y)
        );
        assert_eq!(
            ground_support_for_arena_with_radius(crank, pipe.center.x + 0.62, pipe.center.y, 0.0,)
                .height(),
            Some(pipe.top_y)
        );
    }

    #[test]
    fn vent_tier_side_collision_opens_at_landing_height() {
        let vent = &arena_definitions()[VENT_SPIRAL_ARENA_INDEX];
        let tier = &vent.platforms[0];
        let approach = Vec3::new(
            4.15,
            ARENA_TOP_Y,
            tier.center.y - tier.half_extents.y - FIGHTER_RADIUS * 0.5,
        );
        assert_ne!(
            tier.resolve_side_collision(
                approach,
                FIGHTER_RADIUS,
                crate::constants::LANDING_SNAP_TOLERANCE,
            ),
            approach
        );

        let landing = Vec3::new(
            approach.x,
            tier.top_y - crate::constants::LANDING_SNAP_TOLERANCE,
            approach.z,
        );
        assert_eq!(
            tier.resolve_side_collision(
                landing,
                FIGHTER_RADIUS,
                crate::constants::LANDING_SNAP_TOLERANCE,
            ),
            landing
        );
    }

    #[test]
    fn raised_walkable_platforms_open_at_landing_height_across_arenas() {
        let platform_cases = [
            (1, 2, "Split Causeway"),
            (2, 0, "Sunstone Steps"),
            (3, 0, "Crank Yard"),
            (4, 0, "Vent Spiral"),
            (8, 0, "Sky Steps"),
        ];

        for (arena_index, platform_index, arena_name) in platform_cases {
            let arena = &arena_definitions()[arena_index];
            let platform = &arena.platforms[platform_index];
            let approach = Vec3::new(
                platform.center.x + platform.half_extents.x + FIGHTER_RADIUS * 0.5,
                platform.top_y - crate::constants::LANDING_SNAP_TOLERANCE - 0.01,
                platform.center.y,
            );
            assert_ne!(
                resolve_platform_side_collision_for_arena(arena, approach, FIGHTER_RADIUS),
                approach,
                "{arena_name} should block below its visible platform top"
            );

            let landing = Vec3::new(
                approach.x,
                platform.top_y - crate::constants::LANDING_SNAP_TOLERANCE,
                approach.z,
            );
            assert_eq!(
                resolve_platform_side_collision_for_arena(arena, landing, FIGHTER_RADIUS),
                landing,
                "{arena_name} should open at landing height"
            );
        }
    }

    #[test]
    fn floor_level_platforms_remain_free_of_side_barriers() {
        for arena_index in [0, 5, 6, 7, 9] {
            let arena = &arena_definitions()[arena_index];
            let platform = &arena.platforms[0];
            let position = Vec3::new(
                platform.center.x - platform.half_extents.x - FIGHTER_RADIUS * 0.5,
                ARENA_TOP_Y,
                platform.center.y,
            );
            assert_eq!(
                resolve_platform_side_collision_for_arena(arena, position, FIGHTER_RADIUS),
                position,
                "{} should not gain a floor-level side wall",
                arena.name
            );
        }
    }

    #[test]
    fn floor_level_platform_side_collision_does_not_block_walkable_extensions() {
        let platform = PlatformDefinition::new(0.0, 0.0, 1.0, 1.0, ARENA_TOP_Y - 0.05);
        let side = resolve_platform_side_collision_against(
            Vec3::new(1.2, ARENA_TOP_Y, 0.0),
            0.4,
            &platform,
        );

        assert_eq!(side, Vec3::new(1.2, ARENA_TOP_Y, 0.0));
    }

    #[test]
    fn arena_hazard_pulse_uses_active_window() {
        assert!(arena_hazard_is_active(0.1, 2.0));
        assert!(!arena_hazard_is_active(1.2, 2.0));
        assert!(arena_hazard_is_active(2.05, 2.0));

        let snare = ArenaHazardDefinition {
            kind: ArenaHazardKind::SnareField,
            center: Vec3::ZERO,
            radius: 1.0,
            pulse_seconds: 2.0,
            phase: 0.0,
        };
        assert!(arena_hazard_is_active_for_kind(1.2, &snare));

        let phased_pulse = ArenaHazardDefinition {
            kind: ArenaHazardKind::PulseVent,
            center: Vec3::ZERO,
            radius: 1.0,
            pulse_seconds: 2.0,
            phase: 1.6,
        };
        assert!(arena_hazard_is_active_for_kind(0.5, &phased_pulse));

        let campfire = ArenaHazardDefinition {
            kind: ArenaHazardKind::Campfire,
            center: Vec3::ZERO,
            radius: 1.0,
            pulse_seconds: 1.4,
            phase: 0.0,
        };
        assert!(arena_hazard_is_active_for_kind(0.1, &campfire));
        assert!(arena_hazard_is_active_for_kind(1.3, &campfire));
    }

    #[test]
    fn arena_hazard_overlap_includes_fighter_radius() {
        let hazard = ArenaHazardDefinition {
            kind: ArenaHazardKind::PulseVent,
            center: Vec3::ZERO,
            radius: 1.0,
            pulse_seconds: 2.0,
            phase: 0.0,
        };
        assert!(arena_hazard_overlaps(
            &hazard,
            Vec3::new(1.0 + FIGHTER_RADIUS * 0.8, 0.0, 0.0),
        ));
        assert!(!arena_hazard_overlaps(
            &hazard,
            Vec3::new(1.0 + FIGHTER_RADIUS * 1.4, 0.0, 0.0),
        ));
    }

    #[test]
    fn raised_vent_hazards_do_not_hit_through_lower_tiers() {
        let hazard = ArenaHazardDefinition {
            kind: ArenaHazardKind::PulseVent,
            center: Vec3::new(0.0, ARENA_TOP_Y + 1.36, 0.0),
            radius: 0.82,
            pulse_seconds: 3.6,
            phase: 0.0,
        };

        assert!(arena_hazard_overlaps(
            &hazard,
            Vec3::new(0.0, ARENA_TOP_Y + 1.3, 0.0)
        ));
        assert!(!arena_hazard_overlaps(
            &hazard,
            Vec3::new(0.0, ARENA_TOP_Y + 0.65, 0.0)
        ));
        assert!(arena_hazard_affects_height(&hazard, hazard.center.y + 1.8));
    }

    #[test]
    fn vent_visual_clock_warns_before_matching_active_window() {
        let cycle = 3.6;
        assert_eq!(vent_charge_visual_amount(1.8, cycle, 0.0), 0.0);
        assert!(vent_charge_visual_amount(3.4, cycle, 0.0) > 0.7);
        assert!(vent_active_visual_amount(0.2, cycle, 0.0) > 0.25);
        assert_eq!(vent_active_visual_amount(1.8, cycle, 0.0), 0.0);
    }

    #[test]
    fn fighter_burn_visual_starts_hot_and_fades_out() {
        let fresh = ArenaFighterBurn::new(ARENA_HAZARD_CAMPFIRE_BURN_SECONDS);
        let ending = ArenaFighterBurn {
            remaining: 0.01,
            duration: ARENA_HAZARD_CAMPFIRE_BURN_SECONDS,
        };

        assert!(fresh.visual_amount() > 0.7);
        assert!(ending.visual_amount() < 0.15);
    }

    #[test]
    fn arena_hazard_profiles_vary_by_kind() {
        let pulse = arena_hazard_impact_profile(ArenaHazardKind::PulseVent);
        let snare = arena_hazard_impact_profile(ArenaHazardKind::SnareField);
        let bumper = arena_hazard_impact_profile(ArenaHazardKind::BumperNode);
        let campfire = arena_hazard_impact_profile(ArenaHazardKind::Campfire);
        let saw = arena_hazard_impact_profile(ArenaHazardKind::SawBlade);

        assert!(pulse.force_knockdown);
        assert!(!snare.force_knockdown);
        assert!(snare.knockback < pulse.knockback);
        assert!(bumper.knockback > pulse.knockback);
        assert!(campfire.knockback > snare.knockback);
        assert!(campfire.force_knockdown);
        assert!(!campfire.guardable);
        assert_eq!(campfire.reaction_family, ReactionFamilyId::LauncherDown);
        assert!(campfire.reaction.landing_aftermath.is_some());
        assert_eq!(saw.damage, ARENA_HAZARD_SAW_DAMAGE);
        assert!(saw.knockback > campfire.knockback);
        assert!(saw.vertical_knockback > campfire.vertical_knockback);
        assert!(saw.force_knockdown);
        assert!(!saw.guardable);
        assert!(saw.feedback.heavy_spark);
        assert_eq!(saw.reaction_family, ReactionFamilyId::LauncherDown);
        assert!(saw.reaction.landing_aftermath.is_some());
        assert!(arena_hazard_hit_cooldown(ArenaHazardKind::SawBlade) < 0.7);
        assert!(arena_hazard_hit_cooldown(ArenaHazardKind::SnareField) < 1.0);
    }

    #[test]
    fn saw_knockback_always_points_away_from_the_blade() {
        let center = Vec3::new(-3.1, ARENA_TOP_Y, 0.0);
        assert_eq!(
            saw_knockback_direction(center + Vec3::Z, center, -Vec3::Z),
            Vec3::Z
        );
        assert_eq!(saw_knockback_direction(center, center, Vec3::Z), -Vec3::X);
    }

    #[test]
    fn crank_pipe_accepts_a_grounded_fighter_or_descending_jump() {
        let pipe_pair = arena_definitions()[CRANK_YARD_ARENA_INDEX]
            .pipe_pair
            .expect("Crank Yard pipe pair");
        let center = pipe_pair.endpoints[0];
        let position = Vec3::new(center.x, pipe_pair.top_y, center.y);
        let grounded_motor = FighterMotor {
            grounded: true,
            ..default()
        };
        let airborne_motor = FighterMotor {
            grounded: false,
            ..default()
        };
        let descending_motor = FighterMotor {
            velocity: Vec3::NEG_Y,
            grounded: false,
            ..default()
        };
        let ascending_motor = FighterMotor {
            velocity: Vec3::Y,
            grounded: false,
            ..default()
        };

        assert_eq!(
            pipe_entry_endpoint(pipe_pair, position, &grounded_motor, FighterAction::Idle),
            Some(0)
        );
        assert_eq!(
            pipe_entry_endpoint(pipe_pair, position, &airborne_motor, FighterAction::Idle),
            None
        );
        assert_eq!(
            pipe_entry_endpoint(
                pipe_pair,
                position + Vec3::Y * 0.35,
                &descending_motor,
                FighterAction::Jumping,
            ),
            Some(0)
        );
        assert_eq!(
            pipe_entry_endpoint(
                pipe_pair,
                position + Vec3::Y * 0.35,
                &ascending_motor,
                FighterAction::Jumping,
            ),
            None
        );
        assert_eq!(
            pipe_entry_endpoint(
                pipe_pair,
                position,
                &grounded_motor,
                FighterAction::HeavyAttack
            ),
            None
        );
    }

    #[test]
    fn crank_pipe_transit_sinks_then_emerges_at_the_other_endpoint() {
        let pipe_pair = arena_definitions()[CRANK_YARD_ARENA_INDEX]
            .pipe_pair
            .expect("Crank Yard pipe pair");
        let base_scale = Vec3::splat(1.2);
        let entering = pipe_transit_sample(
            pipe_pair,
            0,
            1,
            PIPE_ENTER_SECONDS * 0.5,
            pipe_pair.top_y,
            base_scale,
        );
        assert_eq!(entering.position.x, pipe_pair.endpoints[0].x);
        assert!(entering.position.y < pipe_pair.top_y);
        assert!(entering.scale.x < base_scale.x);

        let exiting = pipe_transit_sample(
            pipe_pair,
            0,
            1,
            PIPE_ENTER_SECONDS + PIPE_TRAVEL_SECONDS + PIPE_EXIT_SECONDS * 0.5,
            pipe_pair.top_y,
            base_scale,
        );
        assert_eq!(exiting.position.x, pipe_pair.endpoints[1].x);
        assert!(exiting.position.y < pipe_pair.top_y);

        let complete = pipe_transit_sample(
            pipe_pair,
            0,
            1,
            PIPE_ENTER_SECONDS + PIPE_TRAVEL_SECONDS + PIPE_EXIT_SECONDS,
            pipe_pair.top_y,
            base_scale,
        );
        assert!(complete.complete);
        assert_eq!(complete.position.y, pipe_pair.top_y);
        assert_eq!(complete.scale, base_scale);
    }

    #[test]
    fn crank_yard_has_no_harmless_static_saw_decoy() {
        assert!(
            CRANK_ASSET_PROPS
                .iter()
                .all(|prop| prop.name != "Crank yard center saw")
        );
    }

    #[test]
    fn arena_hazard_markers_telegraph_active_wave_peaks() {
        assert!(
            arena_hazard_marker_scale(ArenaHazardKind::PulseVent, 1.0)
                > arena_hazard_marker_scale(ArenaHazardKind::PulseVent, -1.0)
        );
        assert!(
            arena_hazard_marker_scale(ArenaHazardKind::BumperNode, 1.0)
                > arena_hazard_marker_scale(ArenaHazardKind::BumperNode, 0.0)
        );
        assert!(
            arena_hazard_marker_scale(ArenaHazardKind::SnareField, 1.0)
                > arena_hazard_marker_scale(ArenaHazardKind::SnareField, -1.0)
        );
        assert!(
            arena_hazard_marker_scale(ArenaHazardKind::Campfire, 1.0)
                > arena_hazard_marker_scale(ArenaHazardKind::Campfire, -1.0)
        );
    }

    #[test]
    fn arena_background_wallpapers_use_authored_three_to_two_aspect() {
        for arena in arena_definitions() {
            let size = arena_background_wallpaper_size(arena.background);
            let authored_aspect = arena.background.image_size.x / arena.background.image_size.y;
            assert!(
                (size.x / size.y - authored_aspect).abs() < 0.001,
                "{}",
                arena.name
            );
            assert!(size.x > ARENA_RADIUS * 2.0, "{}", arena.name);

            let camera_transform =
                Transform::from_translation(arena.camera_offset).looking_at(Vec3::Y * 0.6, Vec3::Y);
            let transform =
                arena_background_wallpaper_transform(arena.background, &camera_transform);
            let to_camera = (arena.camera_offset - transform.translation).normalize();
            let normal = transform.rotation * Vec3::Z;
            assert!(
                (transform.translation.distance(arena.camera_offset) - arena.background.distance)
                    .abs()
                    < 0.001,
                "{}",
                arena.name
            );
            assert!(normal.dot(to_camera) > 0.999, "{}", arena.name);
        }
    }

    #[test]
    fn mini_arena_props_cover_stage_variants() {
        for index in 0..arena_definitions().len() {
            let props = arena_asset_props(index);
            let expected_minimum = match index {
                1 | 2 => 4,
                CRANK_YARD_ARENA_INDEX => 4,
                VENT_SPIRAL_ARENA_INDEX => 1,
                TRAINING_GROUND_ARENA_INDEX => 0,
                7 => 3,
                _ => 5,
            };
            assert!(props.len() >= expected_minimum);
            assert!(props.iter().all(|prop| prop.file.ends_with(".glb")));
            assert!(props.iter().all(|prop| prop.scale > 0.0));
            assert!(props.iter().all(|prop| prop.y >= ARENA_TOP_Y - 0.6));
            #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
            assert!(props.iter().all(|prop| {
                std::path::Path::new("assets")
                    .join(arena_prop_asset_path(prop.file))
                    .is_file()
            }));
        }
    }

    #[test]
    fn training_ground_has_the_low_poly_reference_perimeter_and_decor() {
        let map = load_authored_arena_map(TRAINING_GROUND_ARENA_INDEX)
            .expect("training ground RON should parse");
        let instances = &map.primitive_prefab_instances;
        assert_eq!(
            instances
                .iter()
                .filter(|instance| instance.prefab == "torch_tower")
                .count(),
            6
        );
        assert_eq!(
            instances
                .iter()
                .filter(|instance| instance.prefab == "pine")
                .count(),
            4
        );
        assert_eq!(
            instances
                .iter()
                .filter(|instance| instance.prefab == "buttress")
                .count(),
            16
        );
        let gate = instances
            .iter()
            .find(|instance| instance.id == "gate")
            .expect("training ground should have a main gate");
        assert!(gate.position.2 < 0.0, "gate must be on the camera-far side");

        let floor_pattern = map
            .floor_pattern
            .as_ref()
            .expect("training ground should use low-poly paving");
        assert_eq!((floor_pattern.rows, floor_pattern.columns), (30, 30));
        assert_eq!(floor_pattern.gap, 0.025);
        assert_eq!(floor_pattern.bevel, 0.045);
        assert_eq!(map.floor_materials.len(), 3);
        assert!(map.floor_rows.is_empty());

        assert_eq!(map.colliders.len(), 4);
        assert_eq!(
            map.lights
                .iter()
                .filter(|light| light.kind == "point")
                .count(),
            6
        );
        assert!(map.lights.iter().all(|light| light.intensity <= 100.0));
        for corner in [
            "torch_corner_far_west",
            "torch_corner_far_east",
            "torch_corner_near_west",
            "torch_corner_near_east",
        ] {
            assert!(instances.iter().any(|instance| instance.id == corner));
        }
        for near_corner in ["torch_corner_near_west", "torch_corner_near_east"] {
            let tower = instances
                .iter()
                .find(|instance| instance.id == near_corner)
                .expect("near corner tower should exist");
            assert!(tower.position.0.abs() < 9.0);
            assert!(tower.position.2 < 9.0);
        }
    }

    #[test]
    fn training_ground_perimeter_is_a_continuous_solid_boundary() {
        let barriers = authored_arena_collision_barriers(TRAINING_GROUND_ARENA_INDEX);
        assert_eq!(barriers.len(), 4);
        assert!(
            barriers
                .iter()
                .all(|barrier| barrier.behavior == PropBarrierBehavior::Solid
                    && barrier.definition.top_y >= ARENA_TOP_Y + 4.0)
        );

        let training = &arena_definitions()[TRAINING_GROUND_ARENA_INDEX];
        for position in [
            Vec3::new(0.0, ARENA_TOP_Y, -9.18),
            Vec3::new(0.0, ARENA_TOP_Y, 9.18),
            Vec3::new(-9.18, ARENA_TOP_Y, 0.0),
            Vec3::new(9.18, ARENA_TOP_Y, 0.0),
        ] {
            assert_ne!(
                resolve_platform_side_collision_for_arena(training, position, FIGHTER_RADIUS,),
                position
            );
        }
    }

    #[test]
    fn vent_spiral_uses_one_non_overlapping_static_reactor_mesh() {
        let props = arena_asset_props(VENT_SPIRAL_ARENA_INDEX);
        assert_eq!(props.len(), 1);
        assert_eq!(props[0].file, "tower/tower-round-crystals.glb");
        assert_eq!(props[0].scale, 3.1);
    }

    #[test]
    fn arena_render_depth_bias_separates_coplanar_surfaces() {
        for arena in arena_definitions() {
            for index in 1..arena.ground_shapes.len() {
                assert!(arena_ground_depth_bias(index) > arena_ground_depth_bias(index - 1));
            }
            for index in 1..arena.platforms.len() {
                assert!(arena_platform_depth_bias(index) > arena_platform_depth_bias(index - 1));
            }
            if !arena.ground_shapes.is_empty() && !arena.platforms.is_empty() {
                assert!(
                    arena_ground_depth_bias(arena.ground_shapes.len() - 1)
                        < arena_platform_depth_bias(0),
                    "{} ground surfaces must render behind platform surfaces",
                    arena.name
                );
            }
        }
    }

    #[test]
    fn arena_props_clear_the_floor_contact_plane() {
        let prop = CROWN_ASSET_PROPS[0];
        let transform = prop.transform();

        assert!((transform.translation.y - prop.y - ARENA_PROP_SURFACE_CLEARANCE).abs() < 0.001);
    }

    #[test]
    fn dry_arena_props_do_not_use_river_assets() {
        for index in [1, 2] {
            assert!(
                arena_asset_props(index)
                    .iter()
                    .all(|prop| !prop.file.contains("river"))
            );
        }
    }

    #[test]
    fn arena_footprints_support_every_fighter_spawn() {
        for arena in arena_definitions() {
            assert!(!arena.ground_shapes.is_empty());
            for spawn in arena.spawn_points {
                assert!(
                    arena_position_is_firm_supported(arena, spawn.x, spawn.z),
                    "{} spawn {spawn:?} must be supported",
                    arena.name
                );
            }
        }
    }

    #[test]
    fn arena_footprints_support_items_and_hazards() {
        for arena in arena_definitions() {
            for anchor in arena.item_anchors {
                assert!(
                    arena_position_is_firm_supported(arena, anchor.position.x, anchor.position.z),
                    "{} item at {:?} must be supported",
                    arena.name,
                    anchor.position
                );
            }
            for hazard in arena.hazards {
                assert!(
                    arena_position_is_firm_supported(arena, hazard.center.x, hazard.center.z),
                    "{} hazard at {:?} must be supported",
                    arena.name,
                    hazard.center
                );
            }
        }
    }

    #[test]
    fn every_rendered_prop_has_an_explicit_collision_policy() {
        for arena_index in 1..arena_definitions().len() {
            for prop in arena_asset_props(arena_index) {
                let _ = prop_collision_profile(prop.file);
            }
        }
    }

    #[test]
    fn snare_garden_has_no_hedge_or_bush_props() {
        assert!(
            SNARE_GARDEN_ASSET_PROPS
                .iter()
                .all(|prop| !prop.file.contains("hedge") && !prop.file.contains("bush"))
        );
    }

    #[test]
    fn champions_court_objects_generate_shared_prop_barriers() {
        let barriers = authored_arena_collision_barriers(CHAMPIONS_COURT_ARENA_INDEX);
        assert!(!barriers.is_empty());
        assert!(barriers.iter().any(|barrier| {
            barrier.definition.center.distance(Vec2::ZERO) < 0.01
                && barrier.definition.top_y > ARENA_TOP_Y
        }));
        assert!(
            barriers
                .iter()
                .any(|barrier| barrier.behavior == PropBarrierBehavior::OneWayTop)
        );
    }

    #[test]
    fn hollow_structure_center_stays_open_while_posts_block() {
        let sunstone = &arena_definitions()[2];
        let structure = SUNSTONE_ASSET_PROPS[0];
        let inside = Vec3::new(structure.x, ARENA_TOP_Y, structure.z);
        assert_eq!(
            resolve_platform_side_collision_for_arena(sunstone, inside, FIGHTER_RADIUS,),
            inside
        );

        let post = structure
            .collision_barriers()
            .find(|barrier| barrier.behavior == PropBarrierBehavior::Solid)
            .expect("wood structure should have solid posts");
        let post_position = Vec3::new(
            post.definition.center.x,
            ARENA_TOP_Y,
            post.definition.center.y,
        );
        assert_ne!(
            resolve_platform_side_collision_for_arena(sunstone, post_position, FIGHTER_RADIUS,),
            post_position
        );
    }

    #[test]
    fn rotated_ground_rectangles_use_local_shape_axes() {
        let shape = ArenaGroundShape::rectangle(0.0, 0.0, 3.0, 0.5, PI * 0.5, 1.2);

        assert_eq!(
            ground_shape_support(&shape, 0.0, 2.5, 0.0),
            Some(GroundSupport::Firm(1.2))
        );
        assert_eq!(ground_shape_support(&shape, 2.5, 0.0, 0.0), None);
    }

    #[test]
    fn unsupported_floor_tiles_are_not_renderable() {
        assert!(!floor_tile_is_firm_supported(
            0,
            ARENA_RADIUS + 4.0,
            ARENA_RADIUS + 4.0
        ));
    }

    #[test]
    fn champions_court_ron_parses_when_present() {
        if fs::metadata(CHAMPIONS_COURT_RON_PATH).is_err() {
            return;
        }

        let map = load_authored_arena_map(CHAMPIONS_COURT_ARENA_INDEX)
            .expect("champions court RON should parse");
        assert_eq!(map.map.tile_size, 2.0);
        assert!(map.assets.contains_key("floor"));
        assert!(!map.floor_shapes.is_empty());
        assert!(!map.instances.is_empty());
        assert!(!map.prefab_instances.is_empty());
    }

    #[test]
    fn champions_asset_paths_use_runtime_asset_root() {
        let assets = HashMap::from([("floor".to_string(), "floor.glb".to_string())]);
        assert_eq!(
            champions_runtime_asset_path(&assets, "floor"),
            Some("arena/kenney_mini_arena/floor.glb".to_string())
        );
        assert_eq!(champions_runtime_asset_path(&assets, "missing"), None);
    }

    #[test]
    fn champions_floor_shapes_expand_octagons_and_even_rectangles() {
        let octagon = AuthoredFloorShape {
            id: "test_octagon".to_string(),
            kind: "filled_octagon".to_string(),
            asset: "floor".to_string(),
            center: (0, 0),
            radius_tiles: 2,
            inner_radius_tiles: 0,
            outer_radius_tiles: 0,
            size_tiles: (0, 0),
            y: 0.0,
            rotation_y: 0.0,
        };
        let octagon_tiles = champions_floor_shape_tiles(&octagon);
        assert!(octagon_tiles.contains(&Vec2::ZERO));
        assert!(octagon_tiles.contains(&Vec2::new(2.0, 0.0)));
        assert!(!octagon_tiles.contains(&Vec2::new(2.0, 2.0)));

        let rectangle = AuthoredFloorShape {
            id: "test_rect".to_string(),
            kind: "rectangle".to_string(),
            asset: "floor_detail".to_string(),
            center: (0, 0),
            radius_tiles: 0,
            inner_radius_tiles: 0,
            outer_radius_tiles: 0,
            size_tiles: (4, 2),
            y: 0.0,
            rotation_y: 0.0,
        };
        let rectangle_tiles = champions_floor_shape_tiles(&rectangle);
        assert_eq!(rectangle_tiles.len(), 8);
        assert!(rectangle_tiles.contains(&Vec2::new(-1.5, -0.5)));
        assert!(rectangle_tiles.contains(&Vec2::new(1.5, 0.5)));

        let far_rectangle = AuthoredFloorShape {
            center: (64, 64),
            ..rectangle
        };
        assert!(champions_floor_shape_render_positions(&far_rectangle, 2.0, 0).is_empty());
    }

    #[test]
    fn champions_prefab_transform_combines_parent_and_child() {
        let prefab_instance = AuthoredPrefabInstance {
            id: "rotated_prefab".to_string(),
            prefab: "weapon_corner".to_string(),
            position: (10.0, 1.0, 0.0),
            rotation_y: 90.0,
            scale: (2.0, 1.0, 2.0),
        };
        let object = AuthoredObject {
            id: "child".to_string(),
            asset: "weapon_spear".to_string(),
            position: (1.0, 0.5, 0.0),
            rotation_y: 30.0,
            scale: (0.5, 0.5, 0.5),
        };

        let transform = champions_prefab_object_transform(&prefab_instance, &object);
        assert!((transform.translation.x - 10.0).abs() < 0.001);
        assert!(
            (transform.translation.y - (ARENA_TOP_Y + 1.5 + ARENA_PROP_SURFACE_CLEARANCE)).abs()
                < 0.001
        );
        assert!((transform.translation.z + 2.0).abs() < 0.001);
        assert_eq!(transform.scale, Vec3::new(1.0, 0.5, 1.0));
    }
}
