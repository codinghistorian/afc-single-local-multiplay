use bevy::gltf::GltfAssetLabel;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use bevy::input::mouse::{MouseScrollUnit, MouseWheel};
use bevy::prelude::*;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use bevy::window::PrimaryWindow;
use serde::{Deserialize, Serialize};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use std::fs;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use std::path::{Path, PathBuf};

use crate::arena::ArenaGeometry;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::arena_defs::active_arena_definition;
use crate::arena_defs::active_arena_index;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::camera::ArenaCamera;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::constants::{ARENA_RADIUS, ARENA_TOP_Y};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::game_state::{MatchPhase, MatchState};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::user_mode::UserModeState;

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const ARENA_ASSET_ROOT: &str = "assets/arena";
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const OVERLAY_ROOT: &str = "assets/maps/overlays";
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const SNAP_VALUES: [f32; 4] = [0.5, 1.0, 0.25, 0.0];
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const ROTATE_STEP: f32 = std::f32::consts::FRAC_PI_8;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const SCALE_STEP: f32 = 0.1;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const MIN_SCALE: f32 = 0.15;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const MAX_SCALE: f32 = 4.0;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const SELECT_RADIUS: f32 = 1.0;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const UNDO_LIMIT: usize = 64;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const EDITOR_CAMERA_PAN_SPEED: f32 = 8.0;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const EDITOR_CAMERA_DRAG_ZOOM: f32 = 0.004;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const EDITOR_CAMERA_SCROLL_LINE_ZOOM: f32 = 0.12;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const EDITOR_CAMERA_SCROLL_PIXEL_ZOOM: f32 = 0.004;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const EDITOR_CAMERA_ROTATE_SPEED: f32 = 1.6;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const EDITOR_CAMERA_MIN_ZOOM: f32 = 0.55;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const EDITOR_CAMERA_MAX_ZOOM: f32 = 2.2;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const ASSET_PALETTE_SIZE: usize = 9;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MapObjectDef {
    pub asset_path: String,
    pub position: [f32; 3],
    pub yaw: f32,
    pub scale: f32,
}

impl MapObjectDef {
    fn transform(&self) -> Transform {
        Transform::from_xyz(self.position[0], self.position[1], self.position[2])
            .with_rotation(Quat::from_rotation_y(self.yaw))
            .with_scale(Vec3::splat(self.scale))
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    fn flat_position(&self) -> Vec2 {
        Vec2::new(self.position[0], self.position[2])
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MapOverlayDef {
    pub arena_index: usize,
    pub objects: Vec<MapObjectDef>,
}

impl MapOverlayDef {
    pub fn empty(arena_index: usize) -> Self {
        Self {
            arena_index,
            objects: Vec::new(),
        }
    }
}

#[derive(Component)]
pub struct MapOverlayObject;

#[cfg(target_arch = "wasm32")]
#[derive(Resource)]
pub struct MapOverlayState {
    loaded_arena_index: Option<usize>,
    overlay: MapOverlayDef,
    spawned_entities: Vec<Entity>,
}

#[cfg(target_arch = "wasm32")]
impl Default for MapOverlayState {
    fn default() -> Self {
        Self {
            loaded_arena_index: None,
            overlay: MapOverlayDef::empty(active_arena_index()),
            spawned_entities: Vec::new(),
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub fn setup_map_overlay(mut commands: Commands) {
    commands.insert_resource(MapOverlayState::default());
}

#[derive(Component)]
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub struct MapEditorPreview;

#[derive(Component)]
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub struct MapEditorPanel;

#[derive(Component)]
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub struct MapEditorText;

#[derive(Resource)]
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub struct MapEditorPreviewAssets {
    ghost_material: Handle<StandardMaterial>,
}

#[derive(Resource)]
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub struct MapEditorState {
    pub active: bool,
    catalog: Vec<String>,
    selected_asset: usize,
    palette_offset: usize,
    selected_object: Option<usize>,
    preview_position: Vec3,
    preview_yaw: f32,
    preview_scale: f32,
    snap_index: usize,
    loaded_arena_index: Option<usize>,
    overlay: MapOverlayDef,
    spawned_entities: Vec<Entity>,
    preview_entity: Option<Entity>,
    preview_asset_path: Option<String>,
    undo_stack: Vec<MapOverlayDef>,
    camera_focus: Vec2,
    camera_zoom: f32,
    camera_yaw: f32,
    camera_drag_cursor: Option<Vec2>,
    dirty: bool,
    status: String,
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
impl MapEditorState {
    fn new(catalog: Vec<String>) -> Self {
        Self {
            active: false,
            catalog,
            selected_asset: 0,
            palette_offset: 0,
            selected_object: None,
            preview_position: Vec3::new(0.0, ARENA_TOP_Y + 0.04, 0.0),
            preview_yaw: 0.0,
            preview_scale: 1.0,
            snap_index: 0,
            loaded_arena_index: None,
            overlay: MapOverlayDef::empty(active_arena_index()),
            spawned_entities: Vec::new(),
            preview_entity: None,
            preview_asset_path: None,
            undo_stack: Vec::new(),
            camera_focus: Vec2::ZERO,
            camera_zoom: 1.0,
            camera_yaw: 0.0,
            camera_drag_cursor: None,
            dirty: false,
            status: "Editor ready".to_string(),
        }
    }

    pub fn active(&self) -> bool {
        self.active
    }

    fn selected_asset_path(&self) -> Option<&str> {
        self.catalog
            .get(
                self.selected_asset
                    .min(self.catalog.len().saturating_sub(1)),
            )
            .map(String::as_str)
    }

    fn snap_value(&self) -> f32 {
        SNAP_VALUES[self.snap_index.min(SNAP_VALUES.len() - 1)]
    }

    fn cycle_asset(&mut self, delta: i32) {
        if self.catalog.is_empty() {
            return;
        }
        let len = self.catalog.len() as i32;
        self.select_asset((self.selected_asset as i32 + delta).rem_euclid(len) as usize);
    }

    fn page_asset_palette(&mut self, delta: i32) {
        if self.catalog.is_empty() {
            return;
        }
        let max_page = (self.catalog.len() - 1) / ASSET_PALETTE_SIZE;
        let current_page = self.palette_offset / ASSET_PALETTE_SIZE;
        let next_page = (current_page as i32 + delta).clamp(0, max_page as i32) as usize;
        self.select_asset((next_page * ASSET_PALETTE_SIZE).min(self.catalog.len() - 1));
    }

    fn select_asset(&mut self, index: usize) {
        if index >= self.catalog.len() {
            return;
        }
        self.selected_asset = index;
        self.palette_offset = asset_palette_offset_for(index);
        self.selected_object = None;
        self.status = format!("Selected {}", asset_display_name(&self.catalog[index]));
    }

    fn cycle_snap(&mut self) {
        self.snap_index = (self.snap_index + 1) % SNAP_VALUES.len();
        self.status = if self.snap_value() > 0.0 {
            format!("Grid snap {:.2}", self.snap_value())
        } else {
            "Grid snap off".to_string()
        };
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn setup_map_editor(mut commands: Commands, mut materials: ResMut<Assets<StandardMaterial>>) {
    let ghost_material = materials.add(StandardMaterial {
        base_color: Color::srgba(0.28, 0.78, 1.0, 0.34),
        emissive: LinearRgba::rgb(0.0, 0.09, 0.13),
        alpha_mode: AlphaMode::Blend,
        perceptual_roughness: 0.72,
        ..default()
    });
    commands.insert_resource(MapEditorPreviewAssets { ghost_material });
    commands.insert_resource(MapEditorState::new(scan_arena_asset_catalog()));
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn setup_map_editor_ui(mut commands: Commands) {
    commands.spawn((
        Node {
            display: Display::None,
            position_type: PositionType::Absolute,
            top: Val::Px(140.0),
            left: Val::Px(18.0),
            width: Val::Px(470.0),
            padding: UiRect::all(Val::Px(10.0)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.015, 0.018, 0.026, 0.86)),
        BorderColor::all(Color::srgb(0.5, 0.62, 0.78)),
        MapEditorPanel,
        Pickable::IGNORE,
        children![(
            Text::new("Map editor"),
            TextFont {
                font_size: 13.0,
                ..default()
            },
            TextColor(Color::srgb(0.86, 0.94, 1.0)),
            TextShadow::default(),
            MapEditorText,
        )],
    ));
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn toggle_map_editor(
    keys: Res<ButtonInput<KeyCode>>,
    match_state: Res<MatchState>,
    user_mode: Res<UserModeState>,
    mut editor: ResMut<MapEditorState>,
) {
    if match_state.phase != MatchPhase::Setup || user_mode.blocks_dev_input() {
        editor.active = false;
        editor.selected_object = None;
        return;
    }

    if keys.just_pressed(KeyCode::F2) {
        editor.active = !editor.active;
        editor.selected_object = None;
        editor.status = if editor.active {
            "Editor enabled".to_string()
        } else {
            "Editor disabled".to_string()
        };
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn sync_map_overlay_visuals(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut editor: ResMut<MapEditorState>,
) {
    let arena_index = active_arena_index();
    if editor.loaded_arena_index == Some(arena_index) {
        return;
    }

    if editor.dirty {
        if let Err(error) = save_overlay(&editor.overlay) {
            editor.status = format!("Auto-save before arena switch failed: {error}");
        }
    }

    if editor.loaded_arena_index.is_some() {
        editor.spawned_entities.clear();
    } else {
        despawn_overlay_entities(&mut commands, &mut editor.spawned_entities);
    }
    editor.overlay = load_overlay(arena_index).unwrap_or_else(|message| {
        editor.status = message;
        MapOverlayDef::empty(arena_index)
    });
    editor.overlay.arena_index = arena_index;
    let overlay = editor.overlay.clone();
    respawn_overlay_entities(
        &mut commands,
        &asset_server,
        &overlay,
        &mut editor.spawned_entities,
    );
    editor.loaded_arena_index = Some(arena_index);
    editor.selected_object = None;
    editor.undo_stack.clear();
    editor.dirty = false;
}

#[cfg(target_arch = "wasm32")]
pub fn sync_map_overlay_visuals(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut overlay_state: ResMut<MapOverlayState>,
) {
    let arena_index = active_arena_index();
    if overlay_state.loaded_arena_index == Some(arena_index) {
        return;
    }

    despawn_overlay_entities(&mut commands, &mut overlay_state.spawned_entities);
    overlay_state.overlay = load_overlay(arena_index).unwrap_or_else(|message| {
        warn!("Map overlay load failed for arena {arena_index}: {message}");
        MapOverlayDef::empty(arena_index)
    });
    overlay_state.overlay.arena_index = arena_index;
    let overlay = overlay_state.overlay.clone();
    respawn_overlay_entities(
        &mut commands,
        &asset_server,
        &overlay,
        &mut overlay_state.spawned_entities,
    );
    overlay_state.loaded_arena_index = Some(arena_index);
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn map_editor_input(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut scroll_events: MessageReader<MouseWheel>,
    windows: Query<&Window, With<PrimaryWindow>>,
    cameras: Query<(&Camera, &GlobalTransform), With<ArenaCamera>>,
    match_state: Res<MatchState>,
    user_mode: Res<UserModeState>,
    asset_server: Res<AssetServer>,
    mut commands: Commands,
    mut editor: ResMut<MapEditorState>,
) {
    if user_mode.blocks_dev_input() {
        editor.active = false;
        editor.selected_object = None;
        return;
    }

    if !editor.active || match_state.phase != MatchPhase::Setup {
        return;
    }

    let scroll_zoom = scroll_events
        .read()
        .map(mouse_wheel_zoom_delta)
        .sum::<f32>();

    if keys.just_pressed(KeyCode::Escape) {
        editor.active = false;
        editor.selected_object = None;
        editor.status = "Editor disabled".to_string();
        return;
    }

    if let Some(cursor) = cursor_floor_position(&windows, &cameras) {
        editor.preview_position = clamp_to_arena(snap_position(cursor, editor.snap_value()));
    }

    update_editor_camera_controls(&time, &keys, &mouse, scroll_zoom, &windows, &mut editor);

    if undo_pressed(&keys) {
        undo_editor_action(&mut commands, &asset_server, &mut editor);
        return;
    }

    if keys.just_pressed(KeyCode::Tab) {
        let reverse = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
        editor.cycle_asset(if reverse { -1 } else { 1 });
    }
    if keys.just_pressed(KeyCode::PageUp) {
        editor.page_asset_palette(-1);
    }
    if keys.just_pressed(KeyCode::PageDown) {
        editor.page_asset_palette(1);
    }

    if keys.just_pressed(KeyCode::KeyG) {
        editor.cycle_snap();
    }

    if keys.just_pressed(KeyCode::KeyQ) {
        rotate_selection_or_preview(&mut editor, -ROTATE_STEP, &mut commands);
    }
    if keys.just_pressed(KeyCode::KeyE) {
        rotate_selection_or_preview(&mut editor, ROTATE_STEP, &mut commands);
    }
    if keys.just_pressed(KeyCode::BracketLeft) {
        scale_selection_or_preview(&mut editor, -SCALE_STEP, &mut commands);
    }
    if keys.just_pressed(KeyCode::BracketRight) {
        scale_selection_or_preview(&mut editor, SCALE_STEP, &mut commands);
    }

    if mouse.just_pressed(MouseButton::Left) {
        place_selected_asset(&mut commands, &asset_server, &mut editor);
    }
    if mouse.just_pressed(MouseButton::Right) {
        select_nearest_object(&mut editor);
    }
    if keys.just_pressed(KeyCode::Delete) || keys.just_pressed(KeyCode::Backspace) {
        delete_selected_object(&mut commands, &asset_server, &mut editor);
    }

    if save_pressed(&keys) {
        match save_overlay(&editor.overlay) {
            Ok(()) => {
                editor.dirty = false;
                editor.status = format!(
                    "Saved {}",
                    overlay_path(editor.overlay.arena_index).display()
                );
            }
            Err(error) => {
                editor.status = format!("Save failed: {error}");
            }
        }
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn update_map_editor_ui(
    editor: Res<MapEditorState>,
    mut panels: Query<&mut Node, With<MapEditorPanel>>,
    mut texts: Query<&mut Text, With<MapEditorText>>,
) {
    for mut panel in &mut panels {
        panel.display = if editor.active {
            Display::Flex
        } else {
            Display::None
        };
    }

    if !editor.active {
        return;
    }

    let selected_asset = editor
        .selected_asset_path()
        .map(|path| format!("{} ({path})", asset_display_name(path)))
        .unwrap_or_else(|| "<no arena GLBs>".to_string());
    let selected_object = editor
        .selected_object
        .map_or("none".to_string(), |index| index.to_string());
    let snap = if editor.snap_value() > 0.0 {
        format!("{:.2}", editor.snap_value())
    } else {
        "off".to_string()
    };
    let dirty = if editor.dirty { "dirty" } else { "saved" };
    let palette = asset_palette_lines(
        &editor.catalog,
        editor.selected_asset,
        editor.palette_offset,
    );
    let text = format!(
        "MAP EDITOR\nArena: {} / Objects: {} / {} / Undo: {}\nAsset: {}\nSelected: {} | Snap: {} | Yaw: {:.1} | Scale: {:.2}\nCursor: {:.2}, {:.2} | Camera: {:.1}, {:.1} / {:.2}x / {:.0} deg\n\n{}\n\nF2 toggle | Tab next asset | Shift+Tab previous asset | PgUp/PgDn page\nArrow keys pan camera | Shift+Left/Right rotate camera | Shift+R reset camera\nWheel/trackpad scroll zoom | MMB or Shift+RMB drag zoom\nLMB place | RMB select | Delete remove | Q/E rotate | [/ ] scale\nG snap | Cmd/Ctrl+Z undo | Ctrl+S save | Esc exit\n{}",
        editor.overlay.arena_index,
        editor.overlay.objects.len(),
        dirty,
        editor.undo_stack.len(),
        selected_asset,
        selected_object,
        snap,
        editor.preview_yaw.to_degrees(),
        editor.preview_scale,
        editor.preview_position.x,
        editor.preview_position.z,
        editor.camera_focus.x,
        editor.camera_focus.y,
        editor.camera_zoom,
        editor.camera_yaw.to_degrees(),
        palette,
        editor.status,
    );

    for mut ui_text in &mut texts {
        **ui_text = text.clone();
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn draw_map_editor_gizmos(editor: Res<MapEditorState>, mut gizmos: Gizmos) {
    if !editor.active {
        return;
    }

    let cursor = editor.preview_position + Vec3::Y * 0.05;
    gizmos.circle(
        Isometry3d::new(cursor, Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)),
        0.38,
        Color::srgb(0.35, 0.9, 1.0),
    );
    gizmos.line(
        cursor,
        cursor + Quat::from_rotation_y(editor.preview_yaw) * Vec3::Z * 0.75,
        Color::srgb(0.35, 0.9, 1.0),
    );

    if let Some(index) = editor.selected_object
        && let Some(object) = editor.overlay.objects.get(index)
    {
        let selected = Vec3::new(
            object.position[0],
            object.position[1] + 0.1,
            object.position[2],
        );
        gizmos.circle(
            Isometry3d::new(selected, Quat::from_rotation_x(std::f32::consts::FRAC_PI_2)),
            0.62,
            Color::srgb(1.0, 0.86, 0.24),
        );
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn sync_map_editor_preview(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    preview_assets: Res<MapEditorPreviewAssets>,
    match_state: Res<MatchState>,
    mut editor: ResMut<MapEditorState>,
    preview_entities: Query<Entity, With<MapEditorPreview>>,
    mut preview_roots: Query<&mut Transform, With<MapEditorPreview>>,
    children: Query<&Children>,
    mut mesh_materials: Query<&mut MeshMaterial3d<StandardMaterial>>,
) {
    if !editor.active || match_state.phase != MatchPhase::Setup {
        despawn_map_preview(&mut commands, &mut editor, &preview_entities);
        return;
    }

    let Some(preview_object) = preview_object_def(&editor) else {
        despawn_map_preview(&mut commands, &mut editor, &preview_entities);
        return;
    };
    let asset_path = preview_object.asset_path.clone();

    if editor.preview_entity.is_none() || editor.preview_asset_path.as_deref() != Some(&asset_path)
    {
        despawn_map_preview(&mut commands, &mut editor, &preview_entities);
        let entity = commands
            .spawn((
                SceneRoot(
                    asset_server.load(GltfAssetLabel::Scene(0).from_asset(asset_path.clone())),
                ),
                preview_object.transform(),
                MapEditorPreview,
                Name::new(format!("Map editor preview: {asset_path}")),
            ))
            .id();
        editor.preview_entity = Some(entity);
        editor.preview_asset_path = Some(asset_path);
    }

    if let Some(entity) = editor.preview_entity {
        if let Ok(mut transform) = preview_roots.get_mut(entity) {
            *transform = preview_object.transform();
        }
        apply_preview_material_recursive(
            entity,
            &children,
            &mut mesh_materials,
            &preview_assets.ghost_material,
        );
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn update_map_editor_camera(
    editor: Res<MapEditorState>,
    match_state: Res<MatchState>,
    mut cameras: Query<&mut Transform, With<ArenaCamera>>,
) {
    if !editor.active || match_state.phase != MatchPhase::Setup {
        return;
    }

    let focus = editor_camera_focus(editor.camera_focus);
    let position =
        editor_camera_position(editor.camera_focus, editor.camera_zoom, editor.camera_yaw);
    for mut camera in &mut cameras {
        camera.translation = position;
        camera.look_at(focus + Vec3::Y * 0.6, Vec3::Y);
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn map_editor_allows_setup_input(editor: Option<Res<MapEditorState>>) -> bool {
    !editor.as_ref().is_some_and(|state| state.active())
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn place_selected_asset(
    commands: &mut Commands,
    asset_server: &AssetServer,
    editor: &mut MapEditorState,
) {
    let Some(asset_path) = editor.selected_asset_path().map(str::to_string) else {
        editor.status = "No arena GLBs found under assets/arena".to_string();
        return;
    };

    let object = MapObjectDef {
        asset_path,
        position: [
            editor.preview_position.x,
            editor.preview_position.y,
            editor.preview_position.z,
        ],
        yaw: editor.preview_yaw,
        scale: editor.preview_scale,
    };
    let index = editor.overlay.objects.len();
    push_undo_snapshot(editor);
    let entity = spawn_overlay_object(commands, asset_server, index, &object);
    editor.overlay.objects.push(object);
    editor.spawned_entities.push(entity);
    editor.selected_object = Some(index);
    editor.dirty = true;
    editor.status = format!("Placed object {index}");
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn select_nearest_object(editor: &mut MapEditorState) {
    let cursor = Vec2::new(editor.preview_position.x, editor.preview_position.z);
    let selected = editor
        .overlay
        .objects
        .iter()
        .enumerate()
        .filter_map(|(index, object)| {
            let distance = object.flat_position().distance(cursor);
            (distance <= SELECT_RADIUS).then_some((index, distance))
        })
        .min_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(index, _)| index);

    editor.selected_object = selected;
    if let Some(index) = selected {
        if let Some(object) = editor.overlay.objects.get(index) {
            editor.preview_yaw = object.yaw;
            editor.preview_scale = object.scale;
        }
        editor.status = format!("Selected object {index}");
    } else {
        editor.status = "No nearby object selected".to_string();
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn delete_selected_object(
    commands: &mut Commands,
    asset_server: &AssetServer,
    editor: &mut MapEditorState,
) {
    let Some(index) = editor
        .selected_object
        .or_else(|| editor.overlay.objects.len().checked_sub(1))
    else {
        editor.status = "No object to delete".to_string();
        return;
    };

    if index >= editor.overlay.objects.len() {
        editor.selected_object = None;
        editor.status = "Selection no longer exists".to_string();
        return;
    }

    push_undo_snapshot(editor);
    editor.overlay.objects.remove(index);
    despawn_overlay_entities(commands, &mut editor.spawned_entities);
    let overlay = editor.overlay.clone();
    respawn_overlay_entities(
        commands,
        asset_server,
        &overlay,
        &mut editor.spawned_entities,
    );
    editor.selected_object = if editor.overlay.objects.is_empty() {
        None
    } else {
        Some(index.min(editor.overlay.objects.len() - 1))
    };
    editor.dirty = true;
    editor.status = format!("Deleted object {index}");
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn rotate_selection_or_preview(editor: &mut MapEditorState, delta: f32, commands: &mut Commands) {
    if let Some(index) = editor.selected_object
        && index < editor.overlay.objects.len()
    {
        push_undo_snapshot(editor);
        let object = &mut editor.overlay.objects[index];
        object.yaw += delta;
        editor.preview_yaw = object.yaw;
        update_spawned_transform(commands, editor.spawned_entities.get(index), object);
        editor.dirty = true;
        return;
    }

    editor.preview_yaw += delta;
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn scale_selection_or_preview(editor: &mut MapEditorState, delta: f32, commands: &mut Commands) {
    if let Some(index) = editor.selected_object
        && let Some(current_scale) = editor.overlay.objects.get(index).map(|object| object.scale)
    {
        let scale = (current_scale + delta).clamp(MIN_SCALE, MAX_SCALE);
        if (scale - current_scale).abs() <= f32::EPSILON {
            return;
        }
        push_undo_snapshot(editor);
        let object = &mut editor.overlay.objects[index];
        object.scale = scale;
        editor.preview_scale = object.scale;
        update_spawned_transform(commands, editor.spawned_entities.get(index), object);
        editor.dirty = true;
        return;
    }

    editor.preview_scale = (editor.preview_scale + delta).clamp(MIN_SCALE, MAX_SCALE);
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn update_spawned_transform(
    commands: &mut Commands,
    entity: Option<&Entity>,
    object: &MapObjectDef,
) {
    if let Some(entity) = entity {
        commands.entity(*entity).insert(object.transform());
    }
}

fn despawn_overlay_entities(commands: &mut Commands, spawned_entities: &mut Vec<Entity>) {
    for entity in spawned_entities.drain(..) {
        commands.entity(entity).despawn();
    }
}

fn respawn_overlay_entities(
    commands: &mut Commands,
    asset_server: &AssetServer,
    overlay: &MapOverlayDef,
    spawned_entities: &mut Vec<Entity>,
) {
    *spawned_entities = overlay
        .objects
        .iter()
        .enumerate()
        .map(|(index, object)| spawn_overlay_object(commands, asset_server, index, object))
        .collect();
}

fn spawn_overlay_object(
    commands: &mut Commands,
    asset_server: &AssetServer,
    index: usize,
    object: &MapObjectDef,
) -> Entity {
    commands
        .spawn((
            SceneRoot(
                asset_server.load(GltfAssetLabel::Scene(0).from_asset(object.asset_path.clone())),
            ),
            object.transform(),
            ArenaGeometry,
            MapOverlayObject,
            Name::new(format!("Map overlay {index}: {}", object.asset_path)),
        ))
        .id()
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn preview_object_def(editor: &MapEditorState) -> Option<MapObjectDef> {
    Some(MapObjectDef {
        asset_path: editor.selected_asset_path()?.to_string(),
        position: [
            editor.preview_position.x,
            editor.preview_position.y,
            editor.preview_position.z,
        ],
        yaw: editor.preview_yaw,
        scale: editor.preview_scale,
    })
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn despawn_map_preview(
    commands: &mut Commands,
    editor: &mut MapEditorState,
    preview_entities: &Query<Entity, With<MapEditorPreview>>,
) {
    if let Some(entity) = editor.preview_entity.take() {
        commands.entity(entity).despawn();
    } else {
        for entity in preview_entities {
            commands.entity(entity).despawn();
        }
    }
    editor.preview_asset_path = None;
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn apply_preview_material_recursive(
    entity: Entity,
    child_query: &Query<&Children>,
    mesh_materials: &mut Query<&mut MeshMaterial3d<StandardMaterial>>,
    ghost_material: &Handle<StandardMaterial>,
) {
    if let Ok(mut material) = mesh_materials.get_mut(entity) {
        material.0 = ghost_material.clone();
    }

    if let Ok(children) = child_query.get(entity) {
        for child in children {
            apply_preview_material_recursive(*child, child_query, mesh_materials, ghost_material);
        }
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn cursor_floor_position(
    windows: &Query<&Window, With<PrimaryWindow>>,
    cameras: &Query<(&Camera, &GlobalTransform), With<ArenaCamera>>,
) -> Option<Vec3> {
    let window = windows.iter().next()?;
    let cursor_position = window.cursor_position()?;
    let (camera, camera_transform) = cameras.iter().next()?;
    let ray = camera
        .viewport_to_world(camera_transform, cursor_position)
        .ok()?;
    ray.plane_intersection_point(
        Vec3::new(0.0, ARENA_TOP_Y + 0.04, 0.0),
        InfinitePlane3d::new(Vec3::Y),
    )
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn update_editor_camera_controls(
    time: &Time,
    keys: &ButtonInput<KeyCode>,
    mouse: &ButtonInput<MouseButton>,
    scroll_zoom: f32,
    windows: &Query<&Window, With<PrimaryWindow>>,
    editor: &mut MapEditorState,
) {
    if update_editor_camera_keyboard_controls(editor, keys, scroll_zoom, time.delta_secs()) {
        return;
    }

    let cursor = windows.iter().next().and_then(Window::cursor_position);
    apply_editor_camera_zoom_drag(editor, cursor, editor_camera_zoom_drag_active(keys, mouse));
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn update_editor_camera_keyboard_controls(
    editor: &mut MapEditorState,
    keys: &ButtonInput<KeyCode>,
    scroll_zoom: f32,
    dt: f32,
) -> bool {
    if editor_camera_reset_pressed(keys) {
        reset_editor_camera(editor);
        return true;
    }

    apply_editor_camera_pan(editor, editor_camera_pan_direction(keys), dt);
    apply_editor_camera_rotation(editor, editor_camera_rotation_direction(keys), dt);
    apply_editor_camera_scroll_zoom(editor, scroll_zoom);
    false
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn editor_camera_pan_direction(keys: &ButtonInput<KeyCode>) -> Vec2 {
    let mut direction = Vec2::ZERO;
    let shift = shift_pressed(keys);
    if keys.pressed(KeyCode::ArrowLeft) && !shift {
        direction.x -= 1.0;
    }
    if keys.pressed(KeyCode::ArrowRight) && !shift {
        direction.x += 1.0;
    }
    if keys.pressed(KeyCode::ArrowUp) {
        direction.y -= 1.0;
    }
    if keys.pressed(KeyCode::ArrowDown) {
        direction.y += 1.0;
    }
    direction
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn editor_camera_rotation_direction(keys: &ButtonInput<KeyCode>) -> f32 {
    if !shift_pressed(keys) {
        return 0.0;
    }

    let mut direction = 0.0;
    if keys.pressed(KeyCode::ArrowLeft) {
        direction -= 1.0;
    }
    if keys.pressed(KeyCode::ArrowRight) {
        direction += 1.0;
    }
    direction
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn shift_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight)
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn editor_camera_reset_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    shift_pressed(keys) && keys.just_pressed(KeyCode::KeyR)
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn editor_camera_zoom_drag_active(
    keys: &ButtonInput<KeyCode>,
    mouse: &ButtonInput<MouseButton>,
) -> bool {
    mouse.pressed(MouseButton::Middle) || (mouse.pressed(MouseButton::Right) && shift_pressed(keys))
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn apply_editor_camera_pan(editor: &mut MapEditorState, direction: Vec2, dt: f32) {
    if direction == Vec2::ZERO {
        return;
    }

    let relative_direction =
        camera_relative_pan_direction(direction.normalize_or_zero(), editor.camera_yaw);
    editor.camera_focus += relative_direction * EDITOR_CAMERA_PAN_SPEED * editor.camera_zoom * dt;
    editor.camera_focus = clamp_camera_focus(editor.camera_focus);
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn camera_relative_pan_direction(direction: Vec2, yaw: f32) -> Vec2 {
    let (sin, cos) = (-yaw).sin_cos();
    Vec2::new(
        direction.x * cos - direction.y * sin,
        direction.x * sin + direction.y * cos,
    )
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn apply_editor_camera_scroll_zoom(editor: &mut MapEditorState, scroll_zoom: f32) {
    if scroll_zoom.abs() <= f32::EPSILON {
        return;
    }

    editor.camera_zoom =
        (editor.camera_zoom + scroll_zoom).clamp(EDITOR_CAMERA_MIN_ZOOM, EDITOR_CAMERA_MAX_ZOOM);
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn apply_editor_camera_rotation(editor: &mut MapEditorState, direction: f32, dt: f32) {
    if direction.abs() <= f32::EPSILON {
        return;
    }

    editor.camera_yaw = (editor.camera_yaw + direction * EDITOR_CAMERA_ROTATE_SPEED * dt)
        .rem_euclid(std::f32::consts::TAU);
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn apply_editor_camera_zoom_drag(
    editor: &mut MapEditorState,
    cursor: Option<Vec2>,
    drag_active: bool,
) {
    if !drag_active {
        editor.camera_drag_cursor = None;
        return;
    }

    let Some(cursor) = cursor else {
        editor.camera_drag_cursor = None;
        return;
    };

    if let Some(previous) = editor.camera_drag_cursor {
        let delta_y = cursor.y - previous.y;
        editor.camera_zoom = (editor.camera_zoom - delta_y * EDITOR_CAMERA_DRAG_ZOOM)
            .clamp(EDITOR_CAMERA_MIN_ZOOM, EDITOR_CAMERA_MAX_ZOOM);
    }
    editor.camera_drag_cursor = Some(cursor);
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn reset_editor_camera(editor: &mut MapEditorState) {
    editor.camera_focus = Vec2::ZERO;
    editor.camera_zoom = 1.0;
    editor.camera_yaw = 0.0;
    editor.camera_drag_cursor = None;
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn clamp_camera_focus(focus: Vec2) -> Vec2 {
    if focus.length() <= ARENA_RADIUS {
        return focus;
    }
    focus.normalize_or_zero() * ARENA_RADIUS
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn editor_camera_focus(focus: Vec2) -> Vec3 {
    Vec3::new(focus.x, 0.0, focus.y)
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn editor_camera_position(focus: Vec2, zoom: f32, yaw: f32) -> Vec3 {
    editor_camera_focus(focus)
        + Quat::from_rotation_y(yaw) * active_arena_definition().camera_offset * zoom
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn mouse_wheel_zoom_delta(event: &MouseWheel) -> f32 {
    let scale = match event.unit {
        MouseScrollUnit::Line => EDITOR_CAMERA_SCROLL_LINE_ZOOM,
        MouseScrollUnit::Pixel => EDITOR_CAMERA_SCROLL_PIXEL_ZOOM,
    };
    event.y * scale
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn snap_position(position: Vec3, snap: f32) -> Vec3 {
    if snap <= 0.0 {
        return position;
    }
    Vec3::new(
        (position.x / snap).round() * snap,
        position.y,
        (position.z / snap).round() * snap,
    )
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn clamp_to_arena(position: Vec3) -> Vec3 {
    let flat = Vec2::new(position.x, position.z);
    if flat.length() <= ARENA_RADIUS {
        return position;
    }
    let clamped = flat.normalize_or_zero() * ARENA_RADIUS;
    Vec3::new(clamped.x, position.y, clamped.y)
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn save_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    keys.just_pressed(KeyCode::KeyS)
        && (keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight))
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn undo_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    keys.just_pressed(KeyCode::KeyZ)
        && (keys.pressed(KeyCode::SuperLeft)
            || keys.pressed(KeyCode::SuperRight)
            || keys.pressed(KeyCode::ControlLeft)
            || keys.pressed(KeyCode::ControlRight))
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn push_undo_snapshot(editor: &mut MapEditorState) {
    if editor.undo_stack.last() == Some(&editor.overlay) {
        return;
    }

    editor.undo_stack.push(editor.overlay.clone());
    if editor.undo_stack.len() > UNDO_LIMIT {
        editor.undo_stack.remove(0);
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn restore_last_overlay(editor: &mut MapEditorState) -> bool {
    let Some(previous) = editor.undo_stack.pop() else {
        editor.status = "Nothing to undo".to_string();
        return false;
    };

    editor.overlay = previous;
    editor.selected_object = None;
    editor.dirty = true;
    editor.status = "Undid last map edit".to_string();
    true
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn undo_editor_action(
    commands: &mut Commands,
    asset_server: &AssetServer,
    editor: &mut MapEditorState,
) {
    if restore_last_overlay(editor) {
        despawn_overlay_entities(commands, &mut editor.spawned_entities);
        let overlay = editor.overlay.clone();
        respawn_overlay_entities(
            commands,
            asset_server,
            &overlay,
            &mut editor.spawned_entities,
        );
    }
}

fn load_overlay(arena_index: usize) -> Result<MapOverlayDef, String> {
    #[cfg(target_arch = "wasm32")]
    {
        let contents = match arena_index {
            0 => include_str!("../assets/maps/overlays/arena_0.ron"),
            1 => include_str!("../assets/maps/overlays/arena_1.ron"),
            2 => include_str!("../assets/maps/overlays/arena_2.ron"),
            3 => include_str!("../assets/maps/overlays/arena_3.ron"),
            _ => return Ok(MapOverlayDef::empty(arena_index)),
        };
        let mut overlay: MapOverlayDef =
            ron::from_str(contents).map_err(|error| format!("RON parse failed: {error}"))?;
        overlay.arena_index = arena_index;
        return Ok(overlay);
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    {
        let path = overlay_path(arena_index);
        if !path.exists() {
            return Ok(MapOverlayDef::empty(arena_index));
        }

        let contents =
            fs::read_to_string(&path).map_err(|error| format!("Load failed: {error}"))?;
        let mut overlay: MapOverlayDef =
            ron::from_str(&contents).map_err(|error| format!("RON parse failed: {error}"))?;
        overlay.arena_index = arena_index;
        Ok(overlay)
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn save_overlay(overlay: &MapOverlayDef) -> Result<(), String> {
    let path = overlay_path(overlay.arena_index);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }

    let pretty = ron::ser::PrettyConfig::new();
    let contents =
        ron::ser::to_string_pretty(overlay, pretty).map_err(|error| error.to_string())?;
    fs::write(path, contents).map_err(|error| error.to_string())
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn overlay_path(arena_index: usize) -> PathBuf {
    Path::new(OVERLAY_ROOT).join(format!("arena_{arena_index}.ron"))
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn scan_arena_asset_catalog() -> Vec<String> {
    let mut assets = Vec::new();
    collect_glb_assets(Path::new(ARENA_ASSET_ROOT), &mut assets);
    assets.sort();
    assets
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn asset_palette_offset_for(index: usize) -> usize {
    (index / ASSET_PALETTE_SIZE) * ASSET_PALETTE_SIZE
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn asset_display_name(path: &str) -> String {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    leaf.strip_suffix(".glb").unwrap_or(leaf).replace('_', " ")
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn asset_palette_lines(catalog: &[String], selected_asset: usize, palette_offset: usize) -> String {
    if catalog.is_empty() {
        return "AVAILABLE OBJECTS\n  <no GLB assets found under assets/arena>".to_string();
    }

    let page_count = (catalog.len() + ASSET_PALETTE_SIZE - 1) / ASSET_PALETTE_SIZE;
    let page = (palette_offset / ASSET_PALETTE_SIZE).min(page_count.saturating_sub(1)) + 1;
    let mut lines = vec![format!(
        "AVAILABLE OBJECTS page {page}/{page_count} ({})",
        catalog.len()
    )];
    for slot in 0..ASSET_PALETTE_SIZE {
        let index = palette_offset + slot;
        let Some(path) = catalog.get(index) else {
            continue;
        };
        let marker = if index == selected_asset { ">" } else { " " };
        lines.push(format!("{marker} {}", asset_display_name(path)));
    }
    lines.join("\n")
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn collect_glb_assets(root: &Path, assets: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_glb_assets(&path, assets);
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) != Some("glb") {
            continue;
        }
        if let Ok(relative) = path.strip_prefix("assets") {
            assets.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_roundtrips_through_ron() {
        let overlay = MapOverlayDef {
            arena_index: 2,
            objects: vec![MapObjectDef {
                asset_path: "arena/kenney_mini_arena/floor.glb".to_string(),
                position: [1.0, 0.5, -2.0],
                yaw: 0.5,
                scale: 1.25,
            }],
        };

        let ron = ron::ser::to_string(&overlay).unwrap();
        let restored: MapOverlayDef = ron::from_str(&ron).unwrap();
        assert_eq!(restored, overlay);
    }

    #[test]
    fn scan_catalog_finds_arena_glbs_only() {
        let catalog = scan_arena_asset_catalog();
        assert!(catalog.iter().any(|path| path.ends_with("floor.glb")));
        assert!(catalog.iter().all(|path| path.starts_with("arena/")));
        assert!(catalog.iter().all(|path| path.ends_with(".glb")));
    }

    #[test]
    fn asset_palette_lists_visible_assets_and_marks_selection() {
        let catalog = vec![
            "arena/kenney_mini_arena/banner.glb".to_string(),
            "arena/kenney_mini_arena/block.glb".to_string(),
            "arena/kenney_mini_arena/floor-detail.glb".to_string(),
            "arena/kenney_mini_arena/floor.glb".to_string(),
            "arena/kenney_mini_arena/statue.glb".to_string(),
            "arena/kenney_mini_arena/tree.glb".to_string(),
            "arena/kenney_mini_arena/wall.glb".to_string(),
            "arena/kenney_mini_arena/weapon-rack.glb".to_string(),
            "arena/kenney_mini_arena/weapon-sword.glb".to_string(),
            "arena/kenney_mini_arena/weapon-spear.glb".to_string(),
        ];

        let text = asset_palette_lines(&catalog, 2, 0);

        assert!(text.contains("AVAILABLE OBJECTS page 1/2 (10)"));
        assert!(text.contains("> floor-detail"));
        assert!(text.contains("  banner"));
        assert!(!text.contains("weapon-spear"));
    }

    #[test]
    fn tab_asset_navigation_updates_visible_palette_page() {
        let catalog = (0..12)
            .map(|index| format!("arena/test/object-{index}.glb"))
            .collect();
        let mut editor = MapEditorState::new(catalog);

        editor.cycle_asset(1);
        assert_eq!(editor.selected_asset, 1);
        assert_eq!(
            editor.selected_asset_path(),
            Some("arena/test/object-1.glb")
        );
        assert_eq!(editor.palette_offset, 0);

        editor.cycle_asset(-1);
        assert_eq!(editor.selected_asset, 0);
        assert_eq!(editor.palette_offset, 0);

        editor.cycle_asset(-1);
        assert_eq!(editor.selected_asset, 11);
        assert_eq!(editor.palette_offset, 9);
    }

    #[test]
    fn preview_object_uses_selected_asset_and_cursor_transform() {
        let mut editor = MapEditorState::new(vec![
            "arena/test/block.glb".to_string(),
            "arena/test/statue.glb".to_string(),
        ]);
        editor.select_asset(1);
        editor.preview_position = Vec3::new(1.25, ARENA_TOP_Y + 0.04, -2.5);
        editor.preview_yaw = 0.75;
        editor.preview_scale = 1.4;

        let preview = preview_object_def(&editor).unwrap();

        assert_eq!(preview.asset_path, "arena/test/statue.glb");
        assert_eq!(preview.position, [1.25, ARENA_TOP_Y + 0.04, -2.5]);
        assert_eq!(preview.yaw, 0.75);
        assert_eq!(preview.scale, 1.4);
    }

    #[test]
    fn undo_restores_previous_overlay_snapshot() {
        let mut editor = MapEditorState::new(vec!["arena/test/block.glb".to_string()]);
        editor.overlay = MapOverlayDef {
            arena_index: 0,
            objects: vec![MapObjectDef {
                asset_path: "arena/test/block.glb".to_string(),
                position: [0.0, ARENA_TOP_Y + 0.04, 0.0],
                yaw: 0.0,
                scale: 1.0,
            }],
        };

        push_undo_snapshot(&mut editor);
        editor.overlay.objects.push(MapObjectDef {
            asset_path: "arena/test/statue.glb".to_string(),
            position: [1.0, ARENA_TOP_Y + 0.04, 1.0],
            yaw: 0.5,
            scale: 1.25,
        });

        assert!(restore_last_overlay(&mut editor));
        assert_eq!(editor.overlay.objects.len(), 1);
        assert!(editor.dirty);
        assert_eq!(editor.selected_object, None);
        assert!(editor.status.contains("Undid"));
        assert!(!restore_last_overlay(&mut editor));
        assert!(editor.status.contains("Nothing to undo"));
    }

    #[test]
    fn editor_camera_pan_scales_with_zoom_and_clamps_to_arena() {
        let mut editor = MapEditorState::new(Vec::new());
        editor.camera_zoom = 1.5;

        apply_editor_camera_pan(&mut editor, Vec2::X, 0.5);
        assert_eq!(editor.camera_focus, Vec2::new(6.0, 0.0));

        apply_editor_camera_pan(&mut editor, Vec2::new(200.0, 0.0), 1.0);
        assert!(editor.camera_focus.length() <= ARENA_RADIUS);
    }

    #[test]
    fn editor_camera_pan_is_relative_to_orbit_yaw() {
        let forward = camera_relative_pan_direction(Vec2::NEG_Y, std::f32::consts::FRAC_PI_2);
        let right = camera_relative_pan_direction(Vec2::X, std::f32::consts::FRAC_PI_2);

        assert!((forward - Vec2::NEG_X).length() < 0.001);
        assert!((right - Vec2::NEG_Y).length() < 0.001);

        let mut editor = MapEditorState::new(Vec::new());
        editor.camera_yaw = std::f32::consts::FRAC_PI_2;
        apply_editor_camera_pan(&mut editor, Vec2::NEG_Y, 0.5);

        assert!(editor.camera_focus.x < 0.0);
        assert!(editor.camera_focus.y.abs() < 0.001);
    }

    #[test]
    fn editor_camera_arrow_y_matches_screen_direction() {
        let mut keys = ButtonInput::default();
        keys.press(KeyCode::ArrowUp);
        assert_eq!(editor_camera_pan_direction(&keys), Vec2::NEG_Y);

        let mut keys = ButtonInput::default();
        keys.press(KeyCode::ArrowDown);
        assert_eq!(editor_camera_pan_direction(&keys), Vec2::Y);
    }

    #[test]
    fn shift_horizontal_arrows_rotate_without_panning() {
        let mut keys = ButtonInput::default();
        keys.press(KeyCode::ShiftLeft);
        keys.press(KeyCode::ArrowLeft);
        assert_eq!(editor_camera_pan_direction(&keys), Vec2::ZERO);
        assert_eq!(editor_camera_rotation_direction(&keys), -1.0);

        let mut keys = ButtonInput::default();
        keys.press(KeyCode::ShiftLeft);
        keys.press(KeyCode::ArrowRight);
        assert_eq!(editor_camera_pan_direction(&keys), Vec2::ZERO);
        assert_eq!(editor_camera_rotation_direction(&keys), 1.0);
    }

    #[test]
    fn editor_camera_rotation_orbits_position_around_focus() {
        let mut editor = MapEditorState::new(Vec::new());

        apply_editor_camera_rotation(&mut editor, 1.0, 0.5);
        assert!(editor.camera_yaw > 0.0);

        let base = editor_camera_position(Vec2::ZERO, 1.0, 0.0);
        let rotated = editor_camera_position(Vec2::ZERO, 1.0, editor.camera_yaw);
        assert_ne!(rotated.x, base.x);
        assert!((rotated.y - base.y).abs() < 0.001);
    }

    #[test]
    fn editor_camera_scroll_zoom_changes_distance() {
        let mut editor = MapEditorState::new(Vec::new());

        apply_editor_camera_scroll_zoom(&mut editor, 0.25);
        assert!(editor.camera_zoom > 1.0);

        let zoomed_out = editor.camera_zoom;
        apply_editor_camera_scroll_zoom(&mut editor, -0.5);
        assert!(editor.camera_zoom < zoomed_out);
    }

    #[test]
    fn editor_camera_drag_up_zooms_out_and_drag_down_zooms_in() {
        let mut editor = MapEditorState::new(Vec::new());
        editor.camera_drag_cursor = Some(Vec2::new(50.0, 100.0));

        apply_editor_camera_zoom_drag(&mut editor, Some(Vec2::new(50.0, 50.0)), true);
        assert!(editor.camera_zoom > 1.0);

        let zoomed_out = editor.camera_zoom;
        apply_editor_camera_zoom_drag(&mut editor, Some(Vec2::new(50.0, 130.0)), true);
        assert!(editor.camera_zoom < zoomed_out);

        apply_editor_camera_zoom_drag(&mut editor, None, false);
        assert_eq!(editor.camera_drag_cursor, None);
    }

    #[test]
    fn editor_camera_reset_restores_default_view_state() {
        let mut editor = MapEditorState::new(Vec::new());
        editor.camera_focus = Vec2::new(3.0, -2.0);
        editor.camera_zoom = 1.7;
        editor.camera_yaw = 1.4;
        editor.camera_drag_cursor = Some(Vec2::new(50.0, 100.0));

        reset_editor_camera(&mut editor);

        assert_eq!(editor.camera_focus, Vec2::ZERO);
        assert_eq!(editor.camera_zoom, 1.0);
        assert_eq!(editor.camera_yaw, 0.0);
        assert_eq!(editor.camera_drag_cursor, None);
    }

    #[test]
    fn editor_camera_reset_has_same_frame_priority() {
        let mut keys = ButtonInput::default();
        let mut editor = MapEditorState::new(Vec::new());
        editor.camera_focus = Vec2::new(3.0, -2.0);
        editor.camera_zoom = 1.7;
        editor.camera_yaw = 1.4;
        editor.camera_drag_cursor = Some(Vec2::new(50.0, 100.0));
        keys.press(KeyCode::ShiftLeft);
        keys.press(KeyCode::KeyR);
        keys.press(KeyCode::ArrowRight);

        assert!(update_editor_camera_keyboard_controls(
            &mut editor,
            &keys,
            0.5,
            0.5,
        ));

        assert_eq!(editor.camera_focus, Vec2::ZERO);
        assert_eq!(editor.camera_zoom, 1.0);
        assert_eq!(editor.camera_yaw, 0.0);
        assert_eq!(editor.camera_drag_cursor, None);
    }

    #[test]
    fn snap_position_respects_off_and_grid_values() {
        let pos = Vec3::new(1.24, 0.49, -2.26);
        assert_eq!(snap_position(pos, 0.0), pos);
        assert_eq!(snap_position(pos, 0.5), Vec3::new(1.0, 0.49, -2.5));
        assert_eq!(snap_position(pos, 1.0), Vec3::new(1.0, 0.49, -2.0));
    }

    #[test]
    fn empty_overlay_path_is_per_arena() {
        assert_eq!(
            overlay_path(3),
            Path::new("assets/maps/overlays/arena_3.ron")
        );
        assert_eq!(MapOverlayDef::empty(3).arena_index, 3);
    }
}
