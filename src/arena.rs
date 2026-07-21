use bevy::gltf::GltfAssetLabel;
use bevy::math::EulerRot;
use bevy::prelude::*;
use serde::Deserialize;
use std::collections::HashMap;
use std::f32::consts::{PI, TAU};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use std::fs;

use crate::arena_defs::{
    active_arena_definition, active_arena_index, arena_definitions, ArenaBackgroundDefinition,
    ArenaDefinition, ArenaGroundShape, ArenaHazardDefinition, ArenaHazardKind, ArenaVisualTheme,
    PlatformDefinition,
};
use crate::combat::{
    apply_impact, can_receive_impact, impact_profile, DamageDefenderProfile, HitEffects,
    ImpactFeedbackIntensity, ImpactProfile, ImpactSource, NEUTRAL_IMPACT_OWNER_ID,
};
use crate::components::{Fighter, FighterActionState, FighterMotor, FighterStats};
#[cfg(test)]
use crate::constants::ARENA_RADIUS;
use crate::constants::{
    ARENA_HEIGHT, ARENA_TOP_Y, FIGHTER_COUNT, FIGHTER_RADIUS, LEDGE_SUPPORT_GRACE_MAX,
    LEDGE_SUPPORT_GRACE_SCALE,
};
use crate::effects::EffectAssets;
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
const MINI_ARENA_ASSET_ROOT: &str = "arena/kenney_mini_arena";
const ARENA_KIT_ASSET_ROOT: &str = "arena/kits";
const MINI_ARENA_FLOOR_SPACING: f32 = 1.6;
const MINI_ARENA_FLOOR_SCALE: f32 = 1.62;
const CHAMPIONS_COURT_ARENA_INDEX: usize = 0;
const CHAMPIONS_COURT_RON_PATH: &str = "arts/champions_court.ron";
const CHAMPIONS_COURT_LIGHT_SCALE: f32 = 1_000.0;
const CHAMPIONS_COURT_MAP_LIGHTS_ENABLED: bool = false;
const PLATFORM_SIDE_COLLISION_MIN_TOP_Y: f32 = ARENA_TOP_Y + 0.08;
const ARENA_GROUND_DEPTH_BIAS_BASE: f32 = -2_048.0;
const ARENA_GROUND_DEPTH_BIAS_STEP: f32 = 128.0;
const ARENA_PLATFORM_DEPTH_BIAS_BASE: f32 = -768.0;
const ARENA_PLATFORM_DEPTH_BIAS_STEP: f32 = 64.0;
const ARENA_PROP_SURFACE_CLEARANCE: f32 = 0.012;

#[derive(Component)]
pub struct ArenaGeometry;

#[derive(Component)]
pub struct ArenaHazardMarker {
    kind: ArenaHazardKind,
    pulse_seconds: f32,
    phase: f32,
    base_scale: f32,
    base_y: f32,
}

#[allow(dead_code)]
#[derive(Clone)]
struct ChampionsCourtFloorRenderAsset {
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
}

#[derive(Debug, Deserialize)]
struct ChampionsCourtRon {
    map: ChampionsCourtMap,
    assets: HashMap<String, String>,
    floor_shapes: Vec<ChampionsCourtFloorShape>,
    #[serde(default)]
    prefabs: HashMap<String, Vec<ChampionsCourtObject>>,
    #[serde(default)]
    instances: Vec<ChampionsCourtObject>,
    #[serde(default)]
    prefab_instances: Vec<ChampionsCourtPrefabInstance>,
    #[serde(default)]
    lights: Vec<ChampionsCourtLight>,
}

#[derive(Debug, Deserialize)]
struct ChampionsCourtMap {
    tile_size: f32,
}

#[derive(Debug, Deserialize)]
struct ChampionsCourtFloorShape {
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
struct ChampionsCourtObject {
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
struct ChampionsCourtPrefabInstance {
    id: String,
    prefab: String,
    position: (f32, f32, f32),
    #[serde(default)]
    rotation_y: f32,
    #[serde(default = "unit_tuple3")]
    scale: (f32, f32, f32),
}

#[derive(Debug, Deserialize)]
struct ChampionsCourtLight {
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

#[derive(Resource)]
pub struct ArenaScene {
    index: usize,
}

#[derive(Resource)]
pub struct ArenaHazardState {
    arena_index: usize,
    elapsed: f32,
    hit_cooldowns: Vec<[f32; FIGHTER_COUNT]>,
}

impl ArenaHazardState {
    fn new(arena_index: usize, hazard_count: usize) -> Self {
        Self {
            arena_index,
            elapsed: 0.0,
            hit_cooldowns: vec![[0.0; FIGHTER_COUNT]; hazard_count],
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
    spawn_arena_background(commands, asset_server, meshes, materials, arena.background);

    let arena_index = active_arena_index();
    let palette = arena_theme_palette(arena.visual_theme);
    let hazard_material = materials.add(StandardMaterial {
        base_color: palette.hazard.with_alpha(0.34),
        emissive: LinearRgba::from(palette.hazard) * 0.16,
        alpha_mode: AlphaMode::Blend,
        depth_bias: 16.0,
        ..default()
    });

    if arena_index == CHAMPIONS_COURT_ARENA_INDEX {
        match spawn_champions_court_map(commands, asset_server) {
            Ok(()) => {
                spawn_arena_hazard_markers(commands, meshes, hazard_material, arena.hazards);
                return;
            }
            Err(error) => {
                warn!("Could not load {CHAMPIONS_COURT_RON_PATH}: {error}");
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
    spawn_platform_blocks(commands, meshes, materials, secondary, arena.platforms);
    spawn_arena_theme_accents(commands, meshes, trim, arena.visual_theme);
    spawn_arena_hazard_markers(commands, meshes, hazard_material, arena.hazards);
    spawn_mini_arena_props(commands, asset_server, arena_index);
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
            hazard: Color::srgb(0.2, 0.76, 0.94),
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
            primary: Color::srgb(0.2, 0.25, 0.34),
            secondary: Color::srgb(0.34, 0.4, 0.46),
            trim: Color::srgb(0.2, 0.9, 0.78),
            hazard: Color::srgb(0.88, 0.25, 0.86),
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
            Mesh3d(meshes.add(Cuboid::new(
                platform.half_extents.x * 2.0,
                height,
                platform.half_extents.y * 2.0,
            ))),
            MeshMaterial3d(platform_material),
            Transform::from_xyz(
                platform.center.x,
                platform.top_y - height * 0.5,
                platform.center.y,
            ),
            Name::new(format!("Arena platform {index}")),
            ArenaGeometry,
        ));
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
        ArenaVisualTheme::Reactor => (&[(0.0, 0.0)][..], Vec2::new(4.0, 0.14)),
        ArenaVisualTheme::Toybox => (&[(-2.8, 0.0), (2.8, 0.0)][..], Vec2::new(0.2, 15.2)),
        ArenaVisualTheme::Market => (&[(0.0, 0.0)][..], Vec2::new(8.8, 0.18)),
        ArenaVisualTheme::Garden => (&[(-3.4, 0.0), (3.4, 0.0)][..], Vec2::new(0.12, 2.8)),
        ArenaVisualTheme::Snow => (&[(-2.9, -2.3), (2.9, 2.3)][..], Vec2::new(3.4, 0.14)),
        ArenaVisualTheme::Powder => (&[(-3.8, 0.0), (3.8, 0.0)][..], Vec2::new(0.18, 12.0)),
        ArenaVisualTheme::Crown => return,
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

fn spawn_arena_background(
    commands: &mut Commands,
    asset_server: &AssetServer,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    background: ArenaBackgroundDefinition,
) {
    let size = arena_background_wallpaper_size(background);
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
        Transform::from_translation(background.position),
        Name::new("Arena anime sky wallpaper"),
        ArenaGeometry,
    ));
}

fn spawn_champions_court_map(
    commands: &mut Commands,
    asset_server: &AssetServer,
) -> Result<(), String> {
    let map = load_champions_court_map()?;
    let mut scenes = HashMap::new();

    spawn_champions_floor_shapes(commands, asset_server, &map, &mut scenes);

    for object in &map.instances {
        spawn_champions_object(
            commands,
            asset_server,
            &map,
            &mut scenes,
            &object.asset,
            champions_object_transform(object),
            champions_object_name("Champions Court object", &object.id, &object.asset),
        );
    }

    for prefab_instance in &map.prefab_instances {
        let Some(prefab) = map.prefabs.get(&prefab_instance.prefab) else {
            warn!(
                "Champion's Court prefab instance '{}' references missing prefab '{}'",
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
                champions_prefab_object_name(prefab_instance, object),
            );
        }
    }

    if CHAMPIONS_COURT_MAP_LIGHTS_ENABLED {
        spawn_champions_lights(commands, &map.lights);
    }

    Ok(())
}

fn load_champions_court_map() -> Result<ChampionsCourtRon, String> {
    #[cfg(target_arch = "wasm32")]
    {
        let contents = include_str!("../arts/champions_court.ron");
        return ron::from_str(contents).map_err(|error| format!("RON parse failed: {error}"));
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    {
        let contents = fs::read_to_string(CHAMPIONS_COURT_RON_PATH)
            .map_err(|error| format!("read failed: {error}"))?;
        ron::from_str(&contents).map_err(|error| format!("RON parse failed: {error}"))
    }
}

fn spawn_champions_floor_shapes(
    commands: &mut Commands,
    asset_server: &AssetServer,
    map: &ChampionsCourtRon,
    scenes: &mut HashMap<String, Handle<Scene>>,
) {
    for shape in &map.floor_shapes {
        let Some(scene) = champions_scene_handle(asset_server, map, scenes, &shape.asset) else {
            warn!(
                "Champion's Court floor shape '{}' references missing asset '{}'",
                shape.id, shape.asset
            );
            continue;
        };

        let scale = Vec3::splat(champions_floor_asset_scale(&shape.asset, map.map.tile_size));
        for tile in champions_floor_shape_render_positions(
            shape,
            map.map.tile_size,
            CHAMPIONS_COURT_ARENA_INDEX,
        ) {
            let x = tile.x;
            let z = tile.y;
            commands.spawn((
                SceneRoot(scene.clone()),
                Transform::from_xyz(x, champions_stage_y(shape.y), z)
                    .with_rotation(champions_yaw(shape.rotation_y))
                    .with_scale(scale),
                ArenaGeometry,
                Name::new(format!("Champion's Court floor {}", shape.id)),
            ));
        }
    }
}

#[allow(dead_code)]
fn champions_floor_render_asset(
    asset_server: &AssetServer,
    map: &ChampionsCourtRon,
    render_assets: &mut HashMap<String, ChampionsCourtFloorRenderAsset>,
    asset_key: &str,
) -> Option<ChampionsCourtFloorRenderAsset> {
    let path = champions_runtime_asset_path(&map.assets, asset_key)?;
    if let Some(asset) = render_assets.get(&path) {
        return Some(asset.clone());
    }

    let asset = ChampionsCourtFloorRenderAsset {
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
    map: &ChampionsCourtRon,
    scenes: &mut HashMap<String, Handle<Scene>>,
    asset_key: &str,
    transform: Transform,
    name: String,
) {
    let Some(scene) = champions_scene_handle(asset_server, map, scenes, asset_key) else {
        warn!("Champion's Court object '{name}' references missing asset '{asset_key}'");
        return;
    };

    commands.spawn((SceneRoot(scene), transform, ArenaGeometry, Name::new(name)));
}

fn champions_scene_handle(
    asset_server: &AssetServer,
    map: &ChampionsCourtRon,
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

fn champions_object_transform(object: &ChampionsCourtObject) -> Transform {
    Transform::from_translation(champions_object_position(object.position))
        .with_rotation(champions_yaw(object.rotation_y))
        .with_scale(champions_scale(object.scale))
}

fn champions_prefab_object_transform(
    prefab_instance: &ChampionsCourtPrefabInstance,
    object: &ChampionsCourtObject,
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

fn champions_prefab_object_name(
    prefab_instance: &ChampionsCourtPrefabInstance,
    object: &ChampionsCourtObject,
) -> String {
    if object.id.is_empty() {
        format!(
            "Champions Court prefab {} {}",
            prefab_instance.id, object.asset
        )
    } else {
        format!(
            "Champions Court prefab {} {}",
            prefab_instance.id, object.id
        )
    }
}

fn champions_floor_shape_tiles(shape: &ChampionsCourtFloorShape) -> Vec<Vec2> {
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
    shape: &ChampionsCourtFloorShape,
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

fn spawn_champions_lights(commands: &mut Commands, lights: &[ChampionsCourtLight]) {
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
                    Name::new(format!("Champion's Court light {}", light.id)),
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
                    Name::new(format!("Champion's Court light {}", light.id)),
                ));
            }
            _ => {}
        }
    }
}

fn champions_light_transform(light: &ChampionsCourtLight) -> Transform {
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
        _ => CROWN_ASSET_PROPS,
    }
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
        name: "Split north stone crossing",
        file: "tower/tile-straight.glb",
        x: 0.0,
        y: ARENA_TOP_Y,
        z: 4.7,
        yaw: PI * 0.5,
        scale: 3.25,
    },
    ArenaAssetProp {
        name: "Split south stone crossing",
        file: "tower/tile-straight.glb",
        x: 0.0,
        y: ARENA_TOP_Y,
        z: -4.7,
        yaw: PI * 0.5,
        scale: 3.25,
    },
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
        name: "Sunstone central dais",
        file: "tower/tile-rock.glb",
        x: 0.0,
        y: ARENA_TOP_Y - 0.1,
        z: 0.0,
        yaw: PI * 0.25,
        scale: 3.3,
    },
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
    ArenaAssetProp {
        name: "Sunstone rear rise",
        file: "tower/tile-hill.glb",
        x: 0.0,
        y: ARENA_TOP_Y - 0.5,
        z: 8.4,
        yaw: PI,
        scale: 3.0,
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
        name: "Crank yard center saw",
        file: "platformer/saw.glb",
        x: 0.0,
        y: ARENA_TOP_Y + 0.04,
        z: 0.0,
        yaw: 0.0,
        scale: 2.5,
    },
    ArenaAssetProp {
        name: "Crank yard north pipe",
        file: "platformer/pipe.glb",
        x: -1.7,
        y: ARENA_TOP_Y,
        z: 7.0,
        yaw: PI,
        scale: 2.0,
    },
    ArenaAssetProp {
        name: "Crank yard south pipe",
        file: "platformer/pipe.glb",
        x: 1.7,
        y: ARENA_TOP_Y,
        z: -7.0,
        yaw: 0.0,
        scale: 2.0,
    },
    ArenaAssetProp {
        name: "Crank yard control lever",
        file: "platformer/lever.glb",
        x: 6.7,
        y: ARENA_TOP_Y,
        z: 1.7,
        yaw: -PI * 0.5,
        scale: 2.0,
    },
    ArenaAssetProp {
        name: "Crank yard crate stack",
        file: "platformer/crate-strong.glb",
        x: -6.2,
        y: ARENA_TOP_Y,
        z: -1.6,
        yaw: 0.3,
        scale: 1.7,
    },
];

const VENT_SPIRAL_ASSET_PROPS: &[ArenaAssetProp] = &[
    ArenaAssetProp {
        name: "Vent spiral reactor base",
        file: "tower/tower-round-base.glb",
        x: 0.0,
        y: ARENA_TOP_Y + 0.02,
        z: 0.0,
        yaw: 0.0,
        scale: 3.1,
    },
    ArenaAssetProp {
        name: "Vent spiral crystal core",
        file: "tower/tower-round-crystals.glb",
        x: 0.0,
        y: ARENA_TOP_Y + 0.08,
        z: 0.0,
        yaw: PI * 0.25,
        scale: 2.35,
    },
    ArenaAssetProp {
        name: "Vent spiral west crystal bank",
        file: "tower/detail-crystal-large.glb",
        x: -5.25,
        y: ARENA_TOP_Y + 0.34,
        z: 1.2,
        yaw: 0.2,
        scale: 2.6,
    },
    ArenaAssetProp {
        name: "Vent spiral east crystal bank",
        file: "tower/detail-crystal-large.glb",
        x: 5.0,
        y: ARENA_TOP_Y + 0.08,
        z: 2.5,
        yaw: -0.35,
        scale: 2.6,
    },
    ArenaAssetProp {
        name: "Vent spiral hovering ufo",
        file: "tower/enemy-ufo-a.glb",
        x: 0.0,
        y: ARENA_TOP_Y + 4.0,
        z: -1.0,
        yaw: 0.25,
        scale: 2.2,
    },
    ArenaAssetProp {
        name: "Vent spiral ufo beam",
        file: "tower/enemy-ufo-beam.glb",
        x: 0.0,
        y: ARENA_TOP_Y + 0.12,
        z: -1.0,
        yaw: 0.0,
        scale: 1.55,
    },
];

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
    ArenaAssetProp {
        name: "Bumper alley west crate",
        file: "blaster/crate-wide.glb",
        x: -4.0,
        y: ARENA_TOP_Y,
        z: -5.8,
        yaw: 0.15,
        scale: 2.0,
    },
    ArenaAssetProp {
        name: "Bumper alley east crate",
        file: "blaster/crate-medium.glb",
        x: 4.0,
        y: ARENA_TOP_Y,
        z: 5.8,
        yaw: -0.2,
        scale: 2.0,
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
        name: "Snare garden north hedge",
        file: "platformer/hedge.glb",
        x: 0.0,
        y: ARENA_TOP_Y,
        z: 7.0,
        yaw: 0.0,
        scale: 2.5,
    },
    ArenaAssetProp {
        name: "Snare garden south hedge",
        file: "platformer/hedge.glb",
        x: 0.0,
        y: ARENA_TOP_Y,
        z: -7.0,
        yaw: PI,
        scale: 2.5,
    },
    ArenaAssetProp {
        name: "Snare garden west hedge corner",
        file: "platformer/hedge-corner.glb",
        x: -7.0,
        y: ARENA_TOP_Y,
        z: 0.0,
        yaw: PI * 0.5,
        scale: 2.5,
    },
    ArenaAssetProp {
        name: "Snare garden east hedge corner",
        file: "platformer/hedge-corner.glb",
        x: 7.0,
        y: ARENA_TOP_Y,
        z: 0.0,
        yaw: -PI * 0.5,
        scale: 2.5,
    },
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
    ArenaAssetProp {
        name: "Powder keg timber barricade",
        file: "tower/wood-structure-high.glb",
        x: -5.4,
        y: ARENA_TOP_Y,
        z: -5.6,
        yaw: PI * 0.25,
        scale: 2.1,
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
    ));

    commands.spawn((
        PointLight {
            intensity: 1_100_000.0,
            range: 36.0,
            shadows_enabled: false,
            ..default()
        },
        Transform::from_xyz(0.0, 9.0, 4.5),
    ));
}

fn spawn_arena_hazard_markers(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    material: Handle<StandardMaterial>,
    hazards: &[ArenaHazardDefinition],
) {
    for hazard in hazards {
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
    mut markers: Query<(&ArenaHazardMarker, &mut Transform)>,
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
}

fn arena_hazard_marker_scale(kind: ArenaHazardKind, wave: f32) -> f32 {
    match kind {
        ArenaHazardKind::PulseVent => 1.0 + wave.max(0.0) * 0.28,
        ArenaHazardKind::SnareField => 0.94 + (wave + 1.0) * 0.08,
        ArenaHazardKind::BumperNode => 0.96 + wave.max(0.0) * 0.2,
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
    mut telemetry: ResMut<MatchTelemetry>,
    mut fighters: Query<(
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

    for (hazard_index, hazard) in arena.hazards.iter().enumerate() {
        if !arena_hazard_is_active_for_kind(state.elapsed, hazard) {
            continue;
        }

        let Some(cooldowns) = state.hit_cooldowns.get_mut(hazard_index) else {
            continue;
        };

        for (fighter, mut stats, mut motor, mut action, style, equipment, transform) in
            &mut fighters
        {
            if fighter.id >= FIGHTER_COUNT
                || !match_state.fighter_can_participate(fighter.id)
                || cooldowns[fighter.id] > 0.0
                || !can_receive_impact(&stats, &action)
                || !arena_hazard_overlaps(hazard, transform.translation)
            {
                continue;
            }

            if hazard.kind == ArenaHazardKind::SnareField {
                motor.velocity.x *= 0.55;
                motor.velocity.z *= 0.55;
            }

            apply_impact(
                &mut commands,
                &effect_assets,
                &mut camera_effects,
                &mut hitstop,
                &match_state,
                &mut stats,
                &mut motor,
                &mut action,
                transform,
                None,
                hazard.center,
                arena_hazard_impact_profile(hazard.kind)
                    .with_hit_effects_enabled(feel.hit_effects_enabled()),
                DamageDefenderProfile::from_loadout(style, equipment),
                &mut telemetry,
            );
            cooldowns[fighter.id] = arena_hazard_hit_cooldown(hazard.kind);
        }
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

    for platform in arena.platforms {
        let dx = (x - platform.center.x).abs();
        let dz = (z - platform.center.y).abs();
        let support = if dx <= platform.half_extents.x && dz <= platform.half_extents.y {
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
    let mut resolved = position;
    for platform in active_arena_definition().platforms {
        resolved = resolve_platform_side_collision_against(resolved, radius, platform);
    }
    resolved
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
        ArenaHazardKind::BumperNode => 0.24,
    }
}

fn arena_hazard_hit_cooldown(kind: ArenaHazardKind) -> f32 {
    match kind {
        ArenaHazardKind::PulseVent => 1.05,
        ArenaHazardKind::SnareField => 0.56,
        ArenaHazardKind::BumperNode => 0.82,
    }
}

fn arena_hazard_overlaps(hazard: &ArenaHazardDefinition, fighter_position: Vec3) -> bool {
    let flat = Vec2::new(
        fighter_position.x - hazard.center.x,
        fighter_position.z - hazard.center.z,
    );
    flat.length() <= hazard.radius + FIGHTER_RADIUS
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
        ArenaHazardKind::BumperNode => impact_profile(
            NEUTRAL_IMPACT_OWNER_ID,
            ImpactSource::Hazard,
            ARENA_HAZARD_BUMPER_DAMAGE,
            ARENA_HAZARD_BUMPER_KNOCKBACK,
            2.8,
            false,
            true,
            20.0,
            ImpactFeedbackIntensity::Heavy,
            ReactionFamilyId::LightAirPop,
        ),
    };
    profile.element = DamageElement::Hazard;
    profile
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn arena_hazard_profiles_vary_by_kind() {
        let pulse = arena_hazard_impact_profile(ArenaHazardKind::PulseVent);
        let snare = arena_hazard_impact_profile(ArenaHazardKind::SnareField);
        let bumper = arena_hazard_impact_profile(ArenaHazardKind::BumperNode);

        assert!(pulse.force_knockdown);
        assert!(!snare.force_knockdown);
        assert!(snare.knockback < pulse.knockback);
        assert!(bumper.knockback > pulse.knockback);
        assert!(arena_hazard_hit_cooldown(ArenaHazardKind::SnareField) < 1.0);
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
    }

    #[test]
    fn arena_background_wallpaper_uses_anime_sky_aspect() {
        let background = arena_definitions()[0].background;
        assert_eq!(background.asset_path, "backgrounds/beautiful_sky_anime.png");

        let size = arena_background_wallpaper_size(background);
        assert!((size.x / size.y - 1.5).abs() < 0.001);
        assert!(size.x > ARENA_RADIUS * 2.0);
    }

    #[test]
    fn mini_arena_props_cover_stage_variants() {
        for index in 0..arena_definitions().len() {
            let props = arena_asset_props(index);
            assert!(props.len() >= 5);
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
            assert!(arena_asset_props(index)
                .iter()
                .all(|prop| !prop.file.contains("river")));
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

        let map = load_champions_court_map().expect("champions court RON should parse");
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
        let octagon = ChampionsCourtFloorShape {
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

        let rectangle = ChampionsCourtFloorShape {
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

        let far_rectangle = ChampionsCourtFloorShape {
            center: (64, 64),
            ..rectangle
        };
        assert!(champions_floor_shape_render_positions(&far_rectangle, 2.0, 0).is_empty());
    }

    #[test]
    fn champions_prefab_transform_combines_parent_and_child() {
        let prefab_instance = ChampionsCourtPrefabInstance {
            id: "rotated_prefab".to_string(),
            prefab: "weapon_corner".to_string(),
            position: (10.0, 1.0, 0.0),
            rotation_y: 90.0,
            scale: (2.0, 1.0, 2.0),
        };
        let object = ChampionsCourtObject {
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
