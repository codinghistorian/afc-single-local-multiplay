use bevy::gltf::GltfAssetLabel;
use bevy::math::EulerRot;
use bevy::prelude::*;
use serde::Deserialize;
use std::collections::HashMap;
use std::f32::consts::PI;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use std::fs;

use crate::arena_defs::{
    ArenaBackgroundDefinition, ArenaDefinition, ArenaHazardDefinition, ArenaHazardKind,
    PlatformDefinition,
    active_arena_definition, active_arena_index, arena_definitions,
};
use crate::combat::{
    DamageDefenderProfile, HitEffects, ImpactFeedbackIntensity, ImpactProfile, ImpactSource,
    NEUTRAL_IMPACT_OWNER_ID, apply_impact, can_receive_impact, impact_profile,
};
use crate::components::{Fighter, FighterActionState, FighterMotor, FighterStats};
use crate::constants::{
    ARENA_HEIGHT, ARENA_RADIUS, ARENA_TOP_Y, FIGHTER_COUNT, FIGHTER_RADIUS,
    LEDGE_SUPPORT_GRACE_MAX, LEDGE_SUPPORT_GRACE_SCALE,
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
const MINI_ARENA_FLOOR_SPACING: f32 = 1.6;
const MINI_ARENA_FLOOR_SCALE: f32 = 1.62;
const MINI_ARENA_FLOOR_RADIUS: f32 = ARENA_RADIUS - 0.65;
const CHAMPIONS_COURT_ARENA_INDEX: usize = 0;
const CHAMPIONS_COURT_RON_PATH: &str = "arts/champions_court.ron";
const CHAMPIONS_COURT_LIGHT_SCALE: f32 = 1_000.0;
const CHAMPIONS_COURT_MAP_LIGHTS_ENABLED: bool = false;
const PLATFORM_SIDE_COLLISION_MIN_TOP_Y: f32 = ARENA_TOP_Y + 0.08;

#[derive(Component)]
pub struct ArenaGeometry;

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

impl ArenaAssetProp {
    fn transform(self) -> Transform {
        Transform::from_xyz(self.x, self.y, self.z)
            .with_rotation(Quat::from_rotation_y(self.yaw))
            .with_scale(Vec3::splat(self.scale))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MiniArenaFloorAsset {
    Floor,
    FloorDetail,
}

#[derive(Clone, Copy)]
struct MiniArenaFloorTile {
    asset: MiniArenaFloorAsset,
    x: f32,
    z: f32,
    yaw: f32,
    scale: f32,
}

impl MiniArenaFloorTile {
    fn transform(self) -> Transform {
        Transform::from_xyz(self.x, mini_arena_floor_y(self.asset), self.z)
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
    let stone = materials.add(StandardMaterial {
        base_color: Color::srgb(0.66, 0.57, 0.42),
        perceptual_roughness: 0.92,
        ..default()
    });
    let stone_dark = materials.add(StandardMaterial {
        base_color: Color::srgb(0.34, 0.27, 0.19),
        perceptual_roughness: 0.96,
        ..default()
    });
    let stone_light = materials.add(StandardMaterial {
        base_color: Color::srgb(0.82, 0.76, 0.59),
        perceptual_roughness: 0.86,
        ..default()
    });
    let red = materials.add(StandardMaterial {
        base_color: Color::srgb(0.76, 0.08, 0.055),
        perceptual_roughness: 0.78,
        ..default()
    });
    let panel = materials.add(StandardMaterial {
        base_color: Color::srgb(0.09, 0.11, 0.18),
        perceptual_roughness: 0.7,
        ..default()
    });
    let panel_trim = materials.add(StandardMaterial {
        base_color: Color::srgb(0.74, 0.57, 0.29),
        metallic: 0.15,
        perceptual_roughness: 0.55,
        ..default()
    });
    let letter = materials.add(StandardMaterial {
        base_color: Color::srgb(0.96, 0.82, 0.24),
        emissive: LinearRgba::rgb(0.08, 0.045, 0.0),
        ..default()
    });
    let hazard_material = materials.add(StandardMaterial {
        base_color: Color::srgba(0.1, 0.8, 0.65, 0.28),
        emissive: LinearRgba::rgb(0.0, 0.12, 0.08),
        alpha_mode: AlphaMode::Blend,
        ..default()
    });
    let arena = active_arena_definition();
    spawn_arena_background(commands, asset_server, meshes, materials, arena.background);

    let arena_index = active_arena_index();
    let _arena_count = arena_definitions().len();

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

    commands.spawn((
        Mesh3d(meshes.add(Cylinder::new(ARENA_RADIUS, ARENA_HEIGHT))),
        MeshMaterial3d(stone.clone()),
        Transform::from_xyz(0.0, 0.0, 0.0),
        Name::new(arena.name),
        ArenaGeometry,
    ));

    spawn_mini_arena_floor_tiles(commands, asset_server, arena_index);
    spawn_floor_markings(commands, meshes, red.clone());
    spawn_stone_lines(commands, meshes, stone_dark.clone());
    spawn_side_blocks(
        commands,
        meshes,
        stone.clone(),
        stone_dark.clone(),
        stone_light.clone(),
        arena.platforms,
    );
    spawn_billboard(commands, meshes, panel, panel_trim, letter, stone_dark);
    spawn_arena_hazard_markers(commands, meshes, hazard_material, arena.hazards);
    spawn_mini_arena_props(commands, asset_server, arena_index);
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
    Transform::from_translation(champions_stage_position(object.position))
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
        champions_stage_y(translation.y),
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
        let asset_path = format!("{MINI_ARENA_ASSET_ROOT}/{}", prop.file);
        commands.spawn((
            SceneRoot(asset_server.load(GltfAssetLabel::Scene(0).from_asset(asset_path))),
            prop.transform(),
            ArenaGeometry,
            Name::new(prop.name),
        ));
    }
}

fn spawn_mini_arena_floor_tiles(
    commands: &mut Commands,
    asset_server: &AssetServer,
    arena_index: usize,
) {
    let floor_scene = mini_arena_scene(asset_server, "floor.glb");
    let detail_scene = mini_arena_scene(asset_server, "floor-detail.glb");

    for tile in mini_arena_floor_tiles(arena_index) {
        let scene = match tile.asset {
            MiniArenaFloorAsset::Floor => floor_scene.clone(),
            MiniArenaFloorAsset::FloorDetail => detail_scene.clone(),
        };
        commands.spawn((
            SceneRoot(scene),
            tile.transform(),
            ArenaGeometry,
            Name::new(match tile.asset {
                MiniArenaFloorAsset::Floor => "Mini arena floor tile",
                MiniArenaFloorAsset::FloorDetail => "Mini arena floor detail",
            }),
        ));
    }
}

fn mini_arena_scene(asset_server: &AssetServer, file: &str) -> Handle<Scene> {
    asset_server
        .load(GltfAssetLabel::Scene(0).from_asset(format!("{MINI_ARENA_ASSET_ROOT}/{file}")))
}

fn mini_arena_floor_tiles(arena_index: usize) -> Vec<MiniArenaFloorTile> {
    let mut tiles = Vec::new();
    for x_index in -4_i32..=4 {
        for z_index in -4_i32..=4 {
            let x = x_index as f32 * MINI_ARENA_FLOOR_SPACING;
            let z = z_index as f32 * MINI_ARENA_FLOOR_SPACING;
            if Vec2::new(x, z).length() > MINI_ARENA_FLOOR_RADIUS {
                continue;
            }

            let yaw = if (x_index + z_index) % 2 == 0 {
                0.0
            } else {
                PI * 0.5
            };
            tiles.push(MiniArenaFloorTile {
                asset: MiniArenaFloorAsset::Floor,
                x,
                z,
                yaw,
                scale: MINI_ARENA_FLOOR_SCALE,
            });
        }
    }

    tiles.extend(
        mini_arena_floor_details(arena_index)
            .iter()
            .map(|(x, z, yaw)| MiniArenaFloorTile {
                asset: MiniArenaFloorAsset::FloorDetail,
                x: *x,
                z: *z,
                yaw: *yaw,
                scale: 1.42,
            }),
    );

    tiles.retain(|tile| floor_tile_is_firm_supported(arena_index, tile.x, tile.z));
    tiles
}

fn floor_tile_is_firm_supported(arena_index: usize, x: f32, z: f32) -> bool {
    let definitions = arena_definitions();
    let Some(arena) = definitions.get(arena_index.min(definitions.len().saturating_sub(1))) else {
        return false;
    };
    arena_position_is_firm_supported(arena, x, z)
}

fn arena_position_is_firm_supported(arena: &ArenaDefinition, x: f32, z: f32) -> bool {
    if Vec2::new(x, z).length() <= ARENA_RADIUS {
        return true;
    }

    arena
        .platforms
        .iter()
        .any(|platform| platform_contains_firm_support(platform, x, z))
}

fn platform_contains_firm_support(platform: &PlatformDefinition, x: f32, z: f32) -> bool {
    let dx = (x - platform.center.x).abs();
    let dz = (z - platform.center.y).abs();
    dx <= platform.half_extents.x && dz <= platform.half_extents.y
}

fn mini_arena_floor_y(asset: MiniArenaFloorAsset) -> f32 {
    match asset {
        MiniArenaFloorAsset::Floor => ARENA_TOP_Y + 0.018,
        MiniArenaFloorAsset::FloorDetail => ARENA_TOP_Y + 0.036,
    }
}

fn mini_arena_floor_details(arena_index: usize) -> &'static [(f32, f32, f32)] {
    match arena_index {
        0 => &[
            (-3.2, 3.2, 0.0),
            (3.2, -3.2, PI),
            (0.0, 4.8, PI * 0.5),
            (0.0, -4.8, -PI * 0.5),
        ],
        1 => &[
            (-4.8, 0.0, 0.0),
            (4.8, 0.0, PI),
            (0.0, 3.2, PI * 0.5),
            (0.0, -3.2, -PI * 0.5),
        ],
        2 => &[
            (0.0, 0.0, PI * 0.25),
            (-3.2, -4.8, -PI * 0.25),
            (3.2, 4.8, PI * 0.75),
            (4.8, -3.2, PI),
        ],
        _ => &[
            (-3.2, 0.0, 0.0),
            (3.2, 0.0, PI),
            (0.0, -4.8, PI * 0.5),
            (0.0, 4.8, -PI * 0.5),
        ],
    }
}

fn arena_asset_props(arena_index: usize) -> &'static [ArenaAssetProp] {
    match arena_index {
        0 => CROWN_ASSET_PROPS,
        1 => SPLIT_ASSET_PROPS,
        2 => LOW_TIDE_ASSET_PROPS,
        _ => CRANK_ASSET_PROPS,
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
        name: "Split north gate",
        file: "wall-gate.glb",
        x: 0.0,
        y: ARENA_TOP_Y,
        z: 8.55,
        yaw: PI,
        scale: 1.95,
    },
    ArenaAssetProp {
        name: "Split south gate",
        file: "wall-gate.glb",
        x: 0.0,
        y: ARENA_TOP_Y,
        z: -8.55,
        yaw: 0.0,
        scale: 1.95,
    },
    ArenaAssetProp {
        name: "Split west column",
        file: "column.glb",
        x: -8.2,
        y: ARENA_TOP_Y,
        z: 2.4,
        yaw: -PI * 0.5,
        scale: 1.75,
    },
    ArenaAssetProp {
        name: "Split east column",
        file: "column.glb",
        x: 8.2,
        y: ARENA_TOP_Y,
        z: -2.4,
        yaw: PI * 0.5,
        scale: 1.75,
    },
    ArenaAssetProp {
        name: "Split loose bricks",
        file: "bricks.glb",
        x: -1.7,
        y: ARENA_TOP_Y + 0.02,
        z: -5.25,
        yaw: 0.35,
        scale: 1.1,
    },
];

const LOW_TIDE_ASSET_PROPS: &[ArenaAssetProp] = &[
    ArenaAssetProp {
        name: "Low tide west tree",
        file: "tree.glb",
        x: -8.6,
        y: ARENA_TOP_Y,
        z: -1.8,
        yaw: 0.2,
        scale: 1.45,
    },
    ArenaAssetProp {
        name: "Low tide east tree",
        file: "tree.glb",
        x: 8.6,
        y: ARENA_TOP_Y,
        z: 2.1,
        yaw: -0.35,
        scale: 1.35,
    },
    ArenaAssetProp {
        name: "Low tide damaged column",
        file: "column-damaged.glb",
        x: -5.85,
        y: ARENA_TOP_Y + 0.52,
        z: 3.95,
        yaw: 0.6,
        scale: 1.25,
    },
    ArenaAssetProp {
        name: "Low tide raised stairs",
        file: "stairs.glb",
        x: 5.95,
        y: ARENA_TOP_Y + 0.52,
        z: -4.05,
        yaw: -PI * 0.25,
        scale: 1.15,
    },
    ArenaAssetProp {
        name: "Low tide wall remnant",
        file: "wall-corner.glb",
        x: 0.0,
        y: ARENA_TOP_Y,
        z: -8.85,
        yaw: PI,
        scale: 1.6,
    },
];

const CRANK_ASSET_PROPS: &[ArenaAssetProp] = &[
    ArenaAssetProp {
        name: "Crank yard weapon rack",
        file: "weapon-rack.glb",
        x: -7.9,
        y: ARENA_TOP_Y,
        z: 5.2,
        yaw: PI * 0.35,
        scale: 1.55,
    },
    ArenaAssetProp {
        name: "Crank yard spare spear",
        file: "weapon-spear.glb",
        x: -6.9,
        y: ARENA_TOP_Y + 0.05,
        z: 5.95,
        yaw: -0.65,
        scale: 1.55,
    },
    ArenaAssetProp {
        name: "Crank yard spare sword",
        file: "weapon-sword.glb",
        x: 7.25,
        y: ARENA_TOP_Y + 0.55,
        z: -5.9,
        yaw: 0.8,
        scale: 1.65,
    },
    ArenaAssetProp {
        name: "Crank yard block stack",
        file: "block.glb",
        x: 7.7,
        y: ARENA_TOP_Y,
        z: -5.2,
        yaw: -0.25,
        scale: 1.1,
    },
    ArenaAssetProp {
        name: "Crank yard back wall",
        file: "wall.glb",
        x: 0.0,
        y: ARENA_TOP_Y,
        z: -9.15,
        yaw: 0.0,
        scale: 1.9,
    },
    ArenaAssetProp {
        name: "Crank yard banner",
        file: "banner.glb",
        x: 0.0,
        y: ARENA_TOP_Y,
        z: 9.05,
        yaw: PI,
        scale: 1.55,
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
        let marker_height = match hazard.kind {
            ArenaHazardKind::PulseVent => 0.025,
            ArenaHazardKind::SnareField => 0.018,
            ArenaHazardKind::BumperNode => 0.08,
        };
        commands.spawn((
            Mesh3d(meshes.add(Cylinder::new(hazard.radius, marker_height))),
            MeshMaterial3d(material.clone()),
            Transform::from_translation(hazard.center)
                .with_scale(Vec3::splat((hazard.pulse_seconds / 2.2).clamp(0.8, 1.3))),
            ArenaGeometry,
        ));
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

fn spawn_floor_markings(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    red: Handle<StandardMaterial>,
) {
    let pip_mesh = meshes.add(Cuboid::new(0.34, 0.018, 1.15));
    for i in 0..16 {
        let angle = i as f32 * PI * 2.0 / 16.0;
        let radius = if i % 2 == 0 { 5.85 } else { 4.65 };
        let pos = Vec3::new(
            angle.cos() * radius,
            ARENA_TOP_Y + 0.035,
            angle.sin() * radius,
        );
        commands.spawn((
            Mesh3d(pip_mesh.clone()),
            MeshMaterial3d(red.clone()),
            Transform::from_translation(pos).with_rotation(Quat::from_rotation_y(-angle)),
            ArenaGeometry,
        ));
    }
}

fn spawn_stone_lines(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    material: Handle<StandardMaterial>,
) {
    let seam_mesh = meshes.add(Cuboid::new(0.055, 0.035, ARENA_RADIUS * 1.72));
    for i in 0..20 {
        let angle = i as f32 * PI / 20.0;
        commands.spawn((
            Mesh3d(seam_mesh.clone()),
            MeshMaterial3d(material.clone()),
            Transform::from_xyz(0.0, ARENA_TOP_Y + 0.05, 0.0)
                .with_rotation(Quat::from_rotation_y(angle)),
            ArenaGeometry,
        ));
    }

    // Keep the center open; full circular torus seams read as a raised donut from the game camera.
}

fn spawn_side_blocks(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    stone: Handle<StandardMaterial>,
    dark: Handle<StandardMaterial>,
    light: Handle<StandardMaterial>,
    platforms: &[PlatformDefinition],
) {
    for platform in platforms {
        commands.spawn((
            Mesh3d(meshes.add(Cuboid::new(
                platform.half_extents.x * 2.0,
                platform.top_y * 2.0 + 0.1,
                platform.half_extents.y * 2.0,
            ))),
            MeshMaterial3d(stone.clone()),
            Transform::from_xyz(
                platform.center.x,
                platform.top_y - (platform.top_y * 0.5),
                platform.center.y,
            ),
            ArenaGeometry,
        ));
    }

    let wall_mesh = meshes.add(Cuboid::new(2.2, 1.25, 0.58));
    for i in 0..12 {
        let angle = i as f32 * PI * 2.0 / 12.0;
        let radius = ARENA_RADIUS + 0.62;
        let pos = Vec3::new(
            angle.cos() * radius,
            ARENA_TOP_Y + 0.34,
            angle.sin() * radius,
        );
        commands.spawn((
            Mesh3d(wall_mesh.clone()),
            MeshMaterial3d(if i % 2 == 0 {
                light.clone()
            } else {
                dark.clone()
            }),
            Transform::from_translation(pos).with_rotation(Quat::from_rotation_y(-angle)),
            ArenaGeometry,
        ));
    }

    for (x, z, rot) in [
        (0.0, 7.95, 0.0),
        (0.0, -7.95, PI),
        (-7.95, 0.0, PI * 0.5),
        (7.95, 0.0, -PI * 0.5),
    ] {
        commands.spawn((
            Mesh3d(meshes.add(Cuboid::new(2.4, 0.28, 2.0))),
            MeshMaterial3d(light.clone()),
            Transform::from_xyz(x, ARENA_TOP_Y + 0.03, z)
                .with_rotation(Quat::from_rotation_y(rot))
                .with_scale(Vec3::new(1.0, 1.0, 0.72)),
            ArenaGeometry,
        ));
    }
}

fn spawn_billboard(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    panel: Handle<StandardMaterial>,
    trim: Handle<StandardMaterial>,
    letter: Handle<StandardMaterial>,
    pillar: Handle<StandardMaterial>,
) {
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(13.8, 3.5, 0.28))),
        MeshMaterial3d(panel),
        Transform::from_xyz(0.0, 4.25, -11.2),
        ArenaGeometry,
    ));

    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(14.4, 0.22, 0.36))),
        MeshMaterial3d(trim.clone()),
        Transform::from_xyz(0.0, 6.08, -11.05),
        ArenaGeometry,
    ));
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(14.4, 0.22, 0.36))),
        MeshMaterial3d(trim.clone()),
        Transform::from_xyz(0.0, 2.42, -11.05),
        ArenaGeometry,
    ));

    for x in [-7.5, 7.5] {
        commands.spawn((
            Mesh3d(meshes.add(Cylinder::new(0.42, 4.7))),
            MeshMaterial3d(pillar.clone()),
            Transform::from_xyz(x, 3.9, -10.95),
            ArenaGeometry,
        ));
    }

    spawn_block_text(
        commands,
        meshes,
        letter,
        "ANIMAL FIGHTER CLUB",
        Vec3::new(-4.2, 4.38, -10.84),
        0.22,
    );
}

fn spawn_block_text(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    material: Handle<StandardMaterial>,
    text: &str,
    origin: Vec3,
    cell: f32,
) {
    let cube = meshes.add(Cuboid::new(cell * 0.82, cell * 0.82, 0.08));
    let mut cursor = 0.0;

    for ch in text.chars() {
        if ch == ' ' {
            cursor += cell * 2.2;
            continue;
        }

        if let Some(pattern) = letter_pattern(ch) {
            for (row, line) in pattern.iter().enumerate() {
                for (col, byte) in line.bytes().enumerate() {
                    if byte == b'1' {
                        let x = origin.x + cursor + col as f32 * cell;
                        let y = origin.y + (6 - row) as f32 * cell;
                        commands.spawn((
                            Mesh3d(cube.clone()),
                            MeshMaterial3d(material.clone()),
                            Transform::from_xyz(x, y, origin.z),
                            ArenaGeometry,
                        ));
                    }
                }
            }
            cursor += cell * 4.3;
        }
    }
}

fn letter_pattern(ch: char) -> Option<[&'static str; 7]> {
    match ch {
        'A' => Some(["0110", "1001", "1001", "1111", "1001", "1001", "1001"]),
        'C' => Some(["1111", "1000", "1000", "1000", "1000", "1000", "1111"]),
        'E' => Some(["1111", "1000", "1000", "1110", "1000", "1000", "1111"]),
        'F' => Some(["1111", "1000", "1000", "1110", "1000", "1000", "1000"]),
        'N' => Some(["1001", "1101", "1101", "1011", "1011", "1001", "1001"]),
        'R' => Some(["1110", "1001", "1001", "1110", "1010", "1001", "1001"]),
        _ => None,
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
    let radius = Vec2::new(x, z).length();
    let arena = active_arena_definition();
    let ledge_grace =
        (support_radius * LEDGE_SUPPORT_GRACE_SCALE).clamp(0.0, LEDGE_SUPPORT_GRACE_MAX);
    let mut best = if radius <= ARENA_RADIUS {
        Some(GroundSupport::Firm(ARENA_TOP_Y))
    } else if radius <= ARENA_RADIUS + ledge_grace {
        Some(GroundSupport::Grace(ARENA_TOP_Y))
    } else {
        None
    };

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
            best = Some(match best {
                Some(current) if current.height().unwrap_or(f32::NEG_INFINITY) > platform.top_y => {
                    current
                }
                _ => support,
            });
        }
    }

    best.unwrap_or(GroundSupport::Airborne)
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
    elapsed.rem_euclid(cycle) <= cycle * arena_hazard_active_fraction(hazard.kind)
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
        };
        assert!(arena_hazard_is_active_for_kind(1.2, &snare));
    }

    #[test]
    fn arena_hazard_overlap_includes_fighter_radius() {
        let hazard = ArenaHazardDefinition {
            kind: ArenaHazardKind::PulseVent,
            center: Vec3::ZERO,
            radius: 1.0,
            pulse_seconds: 2.0,
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
            assert!(props.iter().all(|prop| prop.y >= ARENA_TOP_Y));
        }
    }

    #[test]
    fn mini_arena_floor_tiles_include_base_and_detail_layers() {
        for index in 0..arena_definitions().len() {
            let tiles = mini_arena_floor_tiles(index);
            let floor_count = tiles
                .iter()
                .filter(|tile| tile.asset == MiniArenaFloorAsset::Floor)
                .count();
            let detail_count = tiles
                .iter()
                .filter(|tile| tile.asset == MiniArenaFloorAsset::FloorDetail)
                .count();

            assert!(floor_count >= 60);
            assert!(detail_count >= 4);
            assert!(
                tiles
                    .iter()
                    .all(|tile| Vec2::new(tile.x, tile.z).length() <= MINI_ARENA_FLOOR_RADIUS)
            );
            assert!(
                tiles
                    .iter()
                    .all(|tile| floor_tile_is_firm_supported(index, tile.x, tile.z))
            );
        }
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
        assert!((transform.translation.y - (ARENA_TOP_Y + 1.5)).abs() < 0.001);
        assert!((transform.translation.z + 2.0).abs() < 0.001);
        assert_eq!(transform.scale, Vec3::new(1.0, 0.5, 1.0));
    }
}
