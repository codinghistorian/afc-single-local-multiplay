#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use bevy::input::mouse::{MouseScrollUnit, MouseWheel};
use bevy::prelude::*;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use bevy::render::view::{ColorGrading, ColorGradingGlobal, ColorGradingSection};
use bevy::time::Real;
use serde::{Deserialize, Serialize};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use std::fs;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use std::path::{Path, PathBuf};

use crate::arena_defs::{ArenaDefinition, active_arena_definition};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::arena_defs::{TRAINING_GROUND_ARENA_INDEX, set_active_arena_index};
use crate::combat::HitEffects;
use crate::components::{Fighter, FighterAction, FighterActionState};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::constants::ARENA_RADIUS;
use crate::constants::CAMERA_FOLLOW_RATE;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::game_state::MatchAnnouncements;
use crate::game_state::MatchState;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::map_editor::MapEditorState;
use crate::user_mode::UserModeState;

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const GAMEPLAY_CAMERA_PAN_SPEED: f32 = 8.0;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const GAMEPLAY_CAMERA_ROTATE_SPEED: f32 = 1.6;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const GAMEPLAY_CAMERA_HEIGHT_SPEED: f32 = 8.0;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const GAMEPLAY_CAMERA_SCROLL_LINE_ZOOM: f32 = 0.12;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const GAMEPLAY_CAMERA_SCROLL_PIXEL_ZOOM: f32 = 0.004;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const GAMEPLAY_CAMERA_MIN_ZOOM: f32 = 0.55;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const GAMEPLAY_CAMERA_MAX_ZOOM: f32 = 2.2;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const GAMEPLAY_CAMERA_MIN_HEIGHT_OFFSET: f32 = -6.0;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const GAMEPLAY_CAMERA_MAX_HEIGHT_OFFSET: f32 = 8.0;
const GAMEPLAY_CAMERA_SHAKE_DECAY_PER_SEC: f32 = 1.8;
const GAMEPLAY_CAMERA_SHAKE_FREQUENCY: f32 = 72.0;
const GAMEPLAY_CAMERA_SHAKE_SECONDARY_SCALE: f32 = 1.37;
const GAMEPLAY_CAMERA_SHAKE_TRANSLATION_SCALE: f32 = 0.34;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const SINGLE_PLAYER_CAMERA_PRESET_PATH: &str = "assets/camera/single_player_camera.ron";
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const DEV_PLAYER_CAMERA_TARGET_ID: usize = 0;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const DEV_SCREENSHOT_PATH: &str = "/tmp/afc-training-ground.png";
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const TRAINING_GROUND_CAPTURE_ENV: &str = "AFC_TRAINING_GROUND_CAPTURE";
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const ARENA_CAPTURE_INDEX_ENV: &str = "AFC_ARENA_CAPTURE_INDEX";
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
const ARENA_CAPTURE_PATH_ENV: &str = "AFC_ARENA_CAPTURE_PATH";

#[derive(Component)]
pub struct ArenaCamera;

#[derive(Component)]
pub struct UiCamera;

#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenLook {
    Default,
    NoirCrime,
    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    Comedy,
    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    Family,
    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    Romance,
}

impl Default for ScreenLook {
    fn default() -> Self {
        Self::Default
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
impl ScreenLook {
    fn next(self) -> Self {
        match self {
            Self::Default => Self::NoirCrime,
            Self::NoirCrime => Self::Comedy,
            Self::Comedy => Self::Family,
            Self::Family => Self::Romance,
            Self::Romance => Self::Default,
        }
    }

    fn filters_enabled(self) -> bool {
        self != Self::Default
    }

    fn announcement(self) -> &'static str {
        match self {
            Self::Default => "Camera filter: Default",
            Self::NoirCrime => "Camera filter: Noir Crime",
            Self::Comedy => "Camera filter: Comedy",
            Self::Family => "Camera filter: Family",
            Self::Romance => "Camera filter: Romance",
        }
    }
}

#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct GameplayCameraControl {
    pub focus_offset: Vec2,
    pub yaw: f32,
    pub zoom: f32,
    pub height_offset: f32,
}

impl Default for GameplayCameraControl {
    fn default() -> Self {
        Self {
            focus_offset: Vec2::ZERO,
            yaw: 0.0,
            zoom: 1.0,
            height_offset: 0.0,
        }
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
impl GameplayCameraControl {
    fn reset(&mut self) {
        *self = Self::default();
    }
}

#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct SinglePlayerCameraPreset {
    control: GameplayCameraControl,
    follow_player: bool,
}

impl Default for SinglePlayerCameraPreset {
    fn default() -> Self {
        Self {
            control: GameplayCameraControl::default(),
            follow_player: true,
        }
    }
}

impl SinglePlayerCameraPreset {
    #[cfg(any(test, all(feature = "native", not(target_arch = "wasm32"))))]
    fn new(control: GameplayCameraControl, follow_player: bool) -> Self {
        Self {
            control,
            follow_player,
        }
    }

    fn control(&self) -> GameplayCameraControl {
        self.control
    }

    fn follow_player(&self) -> bool {
        self.follow_player
    }
}

#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SinglePlayerCameraMode {
    follow_player: bool,
}

impl Default for SinglePlayerCameraMode {
    fn default() -> Self {
        Self {
            follow_player: true,
        }
    }
}

impl SinglePlayerCameraMode {
    fn new(follow_player: bool) -> Self {
        Self { follow_player }
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    fn toggle_follow_player(&mut self) {
        self.follow_player = !self.follow_player;
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    fn announcement(self) -> &'static str {
        if self.follow_player {
            "Single-player follow camera: On"
        } else {
            "Single-player follow camera: Off"
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
struct SinglePlayerCameraPresetDef {
    focus_offset: [f32; 2],
    yaw: f32,
    zoom: f32,
    height_offset: f32,
    #[serde(default = "default_single_player_follow_player")]
    follow_player: bool,
}

fn default_single_player_follow_player() -> bool {
    true
}

impl Default for SinglePlayerCameraPresetDef {
    fn default() -> Self {
        Self::from(SinglePlayerCameraPreset::default())
    }
}

impl From<SinglePlayerCameraPreset> for SinglePlayerCameraPresetDef {
    fn from(preset: SinglePlayerCameraPreset) -> Self {
        Self {
            focus_offset: [preset.control.focus_offset.x, preset.control.focus_offset.y],
            yaw: preset.control.yaw,
            zoom: preset.control.zoom,
            height_offset: preset.control.height_offset,
            follow_player: preset.follow_player,
        }
    }
}

impl From<SinglePlayerCameraPresetDef> for SinglePlayerCameraPreset {
    fn from(def: SinglePlayerCameraPresetDef) -> Self {
        Self {
            control: GameplayCameraControl {
                focus_offset: Vec2::new(def.focus_offset[0], def.focus_offset[1]),
                yaw: def.yaw,
                zoom: def.zoom,
                height_offset: def.height_offset,
            },
            follow_player: def.follow_player,
        }
    }
}

#[derive(Resource, Clone, Copy, Debug)]
pub struct CameraActionEffects {
    pub enabled: bool,
    last_offset: Vec3,
}

impl Default for CameraActionEffects {
    fn default() -> Self {
        Self {
            enabled: true,
            last_offset: Vec3::ZERO,
        }
    }
}

#[derive(Resource, Clone, Copy, Debug)]
pub struct ScreenLookTransition {
    from: ScreenLook,
    to: ScreenLook,
    elapsed: f32,
    duration: f32,
    active: bool,
}

impl Default for ScreenLookTransition {
    fn default() -> Self {
        Self {
            from: ScreenLook::Default,
            to: ScreenLook::Default,
            elapsed: 0.0,
            duration: 0.0,
            active: false,
        }
    }
}

impl ScreenLookTransition {
    fn clear(&mut self) {
        self.active = false;
        self.elapsed = 0.0;
        self.duration = 0.0;
        self.from = self.to;
    }
}

pub fn setup_camera(mut commands: Commands) {
    let single_player_preset = load_single_player_camera_preset();
    commands.insert_resource(SinglePlayerCameraMode::new(
        single_player_preset.follow_player(),
    ));
    commands.insert_resource(single_player_preset);
    commands.insert_resource(GameplayCameraControl::default());
    let screen_look = ScreenLook::default();
    commands.insert_resource(screen_look);
    commands.insert_resource(ScreenLookTransition::default());
    let arena = active_arena_definition();
    commands.spawn((
        Camera3d::default(),
        Projection::Perspective(PerspectiveProjection::default()),
        ColorGrading::default(),
        arena_camera_base_transform(arena),
        ArenaCamera,
    ));
    commands.spawn((
        Camera2d,
        Camera {
            order: 100,
            clear_color: ClearColorConfig::None,
            ..default()
        },
        UiCamera,
        Name::new("Default UI camera"),
    ));
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn configure_training_ground_capture() {
    if let Some(index) = std::env::var(ARENA_CAPTURE_INDEX_ENV)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
    {
        set_active_arena_index(index);
    } else if std::env::var_os(TRAINING_GROUND_CAPTURE_ENV).is_some() {
        set_active_arena_index(TRAINING_GROUND_ARENA_INDEX);
    }
}

fn arena_camera_base_transform(arena: &ArenaDefinition) -> Transform {
    Transform::from_translation(arena.camera_offset).looking_at(Vec3::ZERO, Vec3::Y)
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn update_gameplay_camera_controls(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut scroll_events: MessageReader<MouseWheel>,
    editor: Option<Res<MapEditorState>>,
    user_mode: Res<UserModeState>,
    mut control: ResMut<GameplayCameraControl>,
) {
    let scroll_zoom = scroll_events
        .read()
        .map(mouse_wheel_zoom_delta)
        .sum::<f32>();

    if editor.as_ref().is_some_and(|state| state.active()) {
        return;
    }

    update_gameplay_camera_control(
        &mut control,
        &keys,
        scroll_zoom,
        time.delta_secs(),
        user_mode.blocks_dev_input(),
    );
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn capture_screenshot_hotkey(mut commands: Commands, keys: Res<ButtonInput<KeyCode>>) {
    if !keys.just_pressed(KeyCode::F12) {
        return;
    }

    let path =
        std::env::var("AFC_SCREENSHOT_PATH").unwrap_or_else(|_| DEV_SCREENSHOT_PATH.to_string());
    commands
        .spawn(Screenshot::primary_window())
        .observe(save_to_disk(path));
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn capture_training_ground_when_requested(
    mut commands: Commands,
    mut frames: Local<u32>,
    mut captured: Local<bool>,
    mut ui_cameras: Query<&mut Camera, With<UiCamera>>,
    mut arena_cameras: Query<&mut Transform, With<ArenaCamera>>,
    mut fighters: Query<&mut Visibility, With<Fighter>>,
    mut ui_nodes: Query<&mut Visibility, (With<Node>, Without<Fighter>)>,
) {
    let path = if std::env::var_os(ARENA_CAPTURE_INDEX_ENV).is_some() {
        std::env::var_os(ARENA_CAPTURE_PATH_ENV)
            .unwrap_or_else(|| "/tmp/afc-arena-capture.png".into())
    } else if let Some(path) = std::env::var_os(TRAINING_GROUND_CAPTURE_ENV) {
        path
    } else {
        return;
    };

    for mut camera in &mut ui_cameras {
        camera.is_active = false;
    }
    for mut transform in &mut arena_cameras {
        *transform = arena_camera_base_transform(active_arena_definition());
    }
    for mut visibility in &mut fighters {
        *visibility = Visibility::Hidden;
    }
    for mut visibility in &mut ui_nodes {
        *visibility = Visibility::Hidden;
    }

    *frames += 1;
    if *captured || *frames < 90 {
        return;
    }
    *captured = true;
    let path = if path.is_empty() {
        DEV_SCREENSHOT_PATH.into()
    } else {
        path
    };
    commands
        .spawn(Screenshot::primary_window())
        .observe(save_to_disk(path));
}

fn load_single_player_camera_preset() -> SinglePlayerCameraPreset {
    #[cfg(target_arch = "wasm32")]
    {
        single_player_camera_preset_from_contents(Some(include_str!(
            "../assets/camera/single_player_camera.ron"
        )))
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    {
        let contents = fs::read_to_string(single_player_camera_preset_path()).ok();
        single_player_camera_preset_from_contents(contents.as_deref())
    }
}

fn single_player_camera_preset_from_contents(contents: Option<&str>) -> SinglePlayerCameraPreset {
    contents
        .and_then(|contents| ron::from_str::<SinglePlayerCameraPresetDef>(contents).ok())
        .map(SinglePlayerCameraPreset::from)
        .unwrap_or_default()
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn load_single_player_camera_preset_hotkey(
    keys: Res<ButtonInput<KeyCode>>,
    editor: Option<Res<MapEditorState>>,
    user_mode: Res<UserModeState>,
    preset: Res<SinglePlayerCameraPreset>,
    mut control: ResMut<GameplayCameraControl>,
    mut mode: ResMut<SinglePlayerCameraMode>,
    mut announcements: ResMut<MatchAnnouncements>,
) {
    if editor.as_ref().is_some_and(|state| state.active()) || user_mode.blocks_dev_input() {
        return;
    }

    if !single_player_camera_load_pressed(&keys) {
        return;
    }

    apply_single_player_camera_preset(&mut control, &mut mode, &preset);
    announcements.show("Single-player camera loaded", 1.0);
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn toggle_single_player_camera_follow_hotkey(
    keys: Res<ButtonInput<KeyCode>>,
    editor: Option<Res<MapEditorState>>,
    user_mode: Res<UserModeState>,
    mut mode: ResMut<SinglePlayerCameraMode>,
    mut announcements: ResMut<MatchAnnouncements>,
) {
    if editor.as_ref().is_some_and(|state| state.active()) || user_mode.blocks_dev_input() {
        return;
    }

    if !single_player_camera_follow_toggle_pressed(&keys) {
        return;
    }

    mode.toggle_follow_player();
    announcements.show(mode.announcement(), 1.0);
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn save_single_player_camera_preset_hotkey(
    keys: Res<ButtonInput<KeyCode>>,
    editor: Option<Res<MapEditorState>>,
    user_mode: Res<UserModeState>,
    control: Res<GameplayCameraControl>,
    mode: Res<SinglePlayerCameraMode>,
    mut preset: ResMut<SinglePlayerCameraPreset>,
    mut announcements: ResMut<MatchAnnouncements>,
) {
    if editor.as_ref().is_some_and(|state| state.active()) || user_mode.blocks_dev_input() {
        return;
    }

    if !single_player_camera_save_pressed(&keys) {
        return;
    }

    *preset = SinglePlayerCameraPreset::new(*control, mode.follow_player);
    match save_single_player_camera_preset_to_disk(*preset) {
        Ok(()) => announcements.show("Single-player camera saved", 1.0),
        Err(error) => announcements.show(format!("Camera save failed: {error}"), 1.2),
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn save_single_player_camera_preset_to_disk(
    preset: SinglePlayerCameraPreset,
) -> Result<(), String> {
    let path = single_player_camera_preset_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }

    let contents = single_player_camera_preset_contents(preset)?;
    fs::write(path, contents).map_err(|error| error.to_string())
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn single_player_camera_preset_contents(
    preset: SinglePlayerCameraPreset,
) -> Result<String, String> {
    let pretty = ron::ser::PrettyConfig::new();
    ron::ser::to_string_pretty(&SinglePlayerCameraPresetDef::from(preset), pretty)
        .map_err(|error| error.to_string())
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn apply_single_player_camera_preset(
    control: &mut GameplayCameraControl,
    mode: &mut SinglePlayerCameraMode,
    preset: &SinglePlayerCameraPreset,
) {
    *control = preset.control();
    *mode = SinglePlayerCameraMode::new(preset.follow_player());
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn single_player_camera_preset_path() -> PathBuf {
    Path::new(SINGLE_PLAYER_CAMERA_PRESET_PATH).to_path_buf()
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn apply_screen_look_to_cameras(
    screen_look: ScreenLook,
    cameras: &mut Query<&mut ColorGrading, With<ArenaCamera>>,
) {
    for mut grading in cameras {
        *grading = screen_look_color_grading(screen_look);
    }
}

pub fn begin_screen_look_transition(
    screen_look: &mut ScreenLook,
    transition: &mut ScreenLookTransition,
    target: ScreenLook,
    duration: f32,
) {
    if *screen_look == target && !transition.active {
        return;
    }
    transition.from = *screen_look;
    transition.to = target;
    transition.elapsed = 0.0;
    transition.duration = duration.max(0.001);
    transition.active = true;
    *screen_look = target;
}

pub fn update_screen_look_transition(
    time: Res<Time<Real>>,
    mut transition: ResMut<ScreenLookTransition>,
    mut cameras: Query<&mut ColorGrading, With<ArenaCamera>>,
) {
    if !transition.active {
        return;
    }

    transition.elapsed = (transition.elapsed + time.delta_secs()).min(transition.duration);
    let amount = screen_look_transition_amount(transition.elapsed, transition.duration);
    let grading = lerp_color_grading(
        screen_look_color_grading(transition.from),
        screen_look_color_grading(transition.to),
        amount,
    );
    for mut camera in &mut cameras {
        *camera = grading.clone();
    }

    if transition.elapsed >= transition.duration {
        for mut camera in &mut cameras {
            *camera = screen_look_color_grading(transition.to);
        }
        transition.clear();
    }
}

fn screen_look_color_grading(screen_look: ScreenLook) -> ColorGrading {
    match screen_look {
        ScreenLook::Default => ColorGrading::default(),
        ScreenLook::NoirCrime => ColorGrading {
            global: ColorGradingGlobal {
                exposure: -0.35,
                temperature: -0.22,
                tint: -0.08,
                post_saturation: 0.18,
                ..default()
            },
            shadows: color_grading_section(0.25, 1.35, 0.95, 0.82, -0.02),
            midtones: color_grading_section(0.32, 1.25, 1.0, 0.92, -0.01),
            highlights: color_grading_section(0.45, 1.15, 1.0, 1.05, 0.0),
        },
        #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
        ScreenLook::Comedy => ColorGrading {
            global: ColorGradingGlobal {
                exposure: 0.15,
                temperature: 0.18,
                tint: 0.02,
                post_saturation: 1.35,
                ..default()
            },
            shadows: color_grading_section(1.08, 0.88, 1.0, 1.02, 0.025),
            midtones: color_grading_section(1.18, 0.86, 1.0, 1.08, 0.02),
            highlights: color_grading_section(1.1, 0.84, 1.0, 1.12, 0.0),
        },
        #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
        ScreenLook::Family => ColorGrading {
            global: ColorGradingGlobal {
                exposure: 0.1,
                temperature: 0.3,
                tint: 0.05,
                post_saturation: 1.22,
                ..default()
            },
            shadows: color_grading_section(1.0, 0.92, 1.02, 1.0, 0.015),
            midtones: color_grading_section(1.12, 0.9, 1.0, 1.06, 0.015),
            highlights: color_grading_section(1.08, 0.88, 0.98, 1.16, 0.0),
        },
        #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
        ScreenLook::Romance => ColorGrading {
            global: ColorGradingGlobal {
                exposure: 0.06,
                temperature: 0.25,
                tint: 0.18,
                post_saturation: 1.1,
                ..default()
            },
            shadows: color_grading_section(0.95, 0.86, 1.03, 0.98, 0.025),
            midtones: color_grading_section(1.05, 0.84, 1.0, 1.04, 0.025),
            highlights: color_grading_section(1.0, 0.8, 0.98, 1.14, 0.0),
        },
    }
}

fn screen_look_transition_amount(elapsed: f32, duration: f32) -> f32 {
    let linear = (elapsed / duration.max(0.001)).clamp(0.0, 1.0);
    linear * linear * (3.0 - 2.0 * linear)
}

fn lerp_color_grading(from: ColorGrading, to: ColorGrading, amount: f32) -> ColorGrading {
    ColorGrading {
        global: ColorGradingGlobal {
            exposure: lerp_f32(from.global.exposure, to.global.exposure, amount),
            temperature: lerp_f32(from.global.temperature, to.global.temperature, amount),
            tint: lerp_f32(from.global.tint, to.global.tint, amount),
            post_saturation: lerp_f32(
                from.global.post_saturation,
                to.global.post_saturation,
                amount,
            ),
            ..default()
        },
        shadows: lerp_color_grading_section(from.shadows, to.shadows, amount),
        midtones: lerp_color_grading_section(from.midtones, to.midtones, amount),
        highlights: lerp_color_grading_section(from.highlights, to.highlights, amount),
    }
}

fn lerp_color_grading_section(
    from: ColorGradingSection,
    to: ColorGradingSection,
    amount: f32,
) -> ColorGradingSection {
    ColorGradingSection {
        saturation: lerp_f32(from.saturation, to.saturation, amount),
        contrast: lerp_f32(from.contrast, to.contrast, amount),
        gamma: lerp_f32(from.gamma, to.gamma, amount),
        gain: lerp_f32(from.gain, to.gain, amount),
        lift: lerp_f32(from.lift, to.lift, amount),
    }
}

fn lerp_f32(from: f32, to: f32, amount: f32) -> f32 {
    from + (to - from) * amount.clamp(0.0, 1.0)
}

#[cfg(test)]
mod screen_look_transition_tests {
    use super::*;

    #[test]
    fn screen_look_transition_amount_eases_to_one() {
        assert_eq!(screen_look_transition_amount(0.0, 1.0), 0.0);
        assert_eq!(screen_look_transition_amount(1.0, 1.0), 1.0);
        let halfway = screen_look_transition_amount(0.5, 1.0);
        assert!(halfway > 0.0 && halfway < 1.0);
    }

    #[test]
    fn color_grading_lerp_moves_default_toward_noir() {
        let default_grade = screen_look_color_grading(ScreenLook::Default);
        let noir_grade = screen_look_color_grading(ScreenLook::NoirCrime);
        let halfway = lerp_color_grading(default_grade.clone(), noir_grade.clone(), 0.5);

        assert!(halfway.global.post_saturation < default_grade.global.post_saturation);
        assert!(halfway.global.post_saturation > noir_grade.global.post_saturation);
        assert!(halfway.global.exposure < default_grade.global.exposure);
    }
}

fn color_grading_section(
    saturation: f32,
    contrast: f32,
    gamma: f32,
    gain: f32,
    lift: f32,
) -> ColorGradingSection {
    ColorGradingSection {
        saturation,
        contrast,
        gamma,
        gain,
        lift,
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn toggle_camera_action_effects(
    keys: Res<ButtonInput<KeyCode>>,
    editor: Option<Res<MapEditorState>>,
    user_mode: Res<UserModeState>,
    mut effects: ResMut<HitEffects>,
    mut camera_action_effects: ResMut<CameraActionEffects>,
    mut screen_look: ResMut<ScreenLook>,
    mut transition: ResMut<ScreenLookTransition>,
    mut cameras: Query<&mut ColorGrading, With<ArenaCamera>>,
    mut announcements: ResMut<MatchAnnouncements>,
) {
    if editor.as_ref().is_some_and(|state| state.active()) || user_mode.blocks_dev_input() {
        return;
    }

    if !(keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight)) {
        return;
    }

    if !keys.just_pressed(KeyCode::KeyC) {
        return;
    }

    let next_look = screen_look.next();
    *screen_look = next_look;
    transition.clear();
    camera_action_effects.enabled = next_look.filters_enabled();
    if !camera_action_effects.enabled {
        effects.shake = 0.0;
    }
    apply_screen_look_to_cameras(next_look, &mut cameras);
    announcements.show(next_look.announcement(), 1.0);
}

pub fn follow_camera(
    time: Res<Time>,
    mut effects: ResMut<HitEffects>,
    mut camera_action_effects: ResMut<CameraActionEffects>,
    state: Res<MatchState>,
    control: Res<GameplayCameraControl>,
    single_player_preset: Res<SinglePlayerCameraPreset>,
    single_player_mode: Res<SinglePlayerCameraMode>,
    user_mode: Res<UserModeState>,
    mut cameras: Query<&mut Transform, With<ArenaCamera>>,
    fighters: Query<(&Fighter, &Transform, &FighterActionState), Without<ArenaCamera>>,
) {
    let samples = fighters
        .iter()
        .filter_map(|(fighter, transform, action)| {
            camera_sample_for_fighter(&state, fighter, transform, action)
        })
        .collect::<Vec<_>>();
    let user_single_player_target_id = user_mode.single_player_camera_target_id();
    let user_follow_target_id =
        gameplay_camera_user_follow_target_id(user_single_player_target_id, &single_player_mode);
    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    let follow_target_id = user_follow_target_id.or_else(|| {
        gameplay_camera_native_dev_follow_target_id(user_mode.active(), &single_player_mode)
    });
    #[cfg(not(all(feature = "native", not(target_arch = "wasm32"))))]
    let follow_target_id = user_follow_target_id;
    let follow_target_present = gameplay_camera_target_present(&samples, follow_target_id);
    let center = gameplay_camera_center_for_samples(&samples, follow_target_id);
    let farthest = gameplay_camera_farthest_for_mode(&samples, center, follow_target_present);
    let mut camera_control = gameplay_camera_control_for_mode(
        &control,
        &single_player_preset,
        user_single_player_target_id.is_some(),
    );
    camera_control =
        gameplay_camera_control_for_follow_target(camera_control, follow_target_present);

    let focus = gameplay_camera_focus(center, &camera_control);
    let target = gameplay_camera_target(center, farthest, &camera_control);
    let dt = time.delta_secs();

    effects.shake = decayed_camera_shake(effects.shake, dt);
    let shake = if camera_action_effects.enabled {
        camera_action_shake_offset(effects.shake, time.elapsed_secs())
    } else {
        effects.shake = 0.0;
        Vec3::ZERO
    };

    let follow_alpha = gameplay_camera_follow_alpha(dt);
    for mut camera in &mut cameras {
        let base_translation = camera.translation - camera_action_effects.last_offset;
        camera.translation = base_translation.lerp(target, follow_alpha) + shake;
        camera.look_at(focus + Vec3::Y * 0.6, Vec3::Y);
    }
    camera_action_effects.last_offset = shake;
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CameraFighterSample {
    id: usize,
    position: Vec3,
}

fn camera_sample_for_fighter(
    state: &MatchState,
    fighter: &Fighter,
    transform: &Transform,
    action: &FighterActionState,
) -> Option<CameraFighterSample> {
    if !state.fighter_active(fighter.id)
        || matches!(
            action.action,
            FighterAction::RingOut | FighterAction::Respawning
        )
    {
        return None;
    }

    Some(CameraFighterSample {
        id: fighter.id,
        position: transform.translation,
    })
}

fn gameplay_camera_center_for_samples(
    samples: &[CameraFighterSample],
    target_id: Option<usize>,
) -> Vec3 {
    if let Some(target_id) = target_id {
        if let Some(sample) = samples.iter().find(|sample| sample.id == target_id) {
            return flat_camera_position(sample.position);
        }
    }

    if samples.is_empty() {
        return Vec3::ZERO;
    }

    let mut center = Vec3::ZERO;
    for sample in samples {
        center += sample.position;
    }
    center /= samples.len() as f32;
    flat_camera_position(center)
}

fn gameplay_camera_farthest_from_center(samples: &[CameraFighterSample], center: Vec3) -> f32 {
    samples
        .iter()
        .map(|sample| (sample.position - center).length())
        .fold(0.0, f32::max)
}

fn gameplay_camera_farthest_for_mode(
    samples: &[CameraFighterSample],
    center: Vec3,
    follow_target_present: bool,
) -> f32 {
    if follow_target_present {
        0.0
    } else {
        gameplay_camera_farthest_from_center(samples, center)
    }
}

fn flat_camera_position(position: Vec3) -> Vec3 {
    Vec3::new(position.x, 0.0, position.z)
}

fn gameplay_camera_target_present(
    samples: &[CameraFighterSample],
    target_id: Option<usize>,
) -> bool {
    target_id.is_some_and(|target_id| samples.iter().any(|sample| sample.id == target_id))
}

fn gameplay_camera_user_follow_target_id(
    user_single_player_target_id: Option<usize>,
    mode: &SinglePlayerCameraMode,
) -> Option<usize> {
    if !mode.follow_player {
        return None;
    }

    user_single_player_target_id
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn gameplay_camera_native_dev_follow_target_id(
    user_mode_active: bool,
    mode: &SinglePlayerCameraMode,
) -> Option<usize> {
    if mode.follow_player && !user_mode_active {
        Some(DEV_PLAYER_CAMERA_TARGET_ID)
    } else {
        None
    }
}

fn gameplay_camera_control_for_follow_target(
    mut control: GameplayCameraControl,
    follow_target_present: bool,
) -> GameplayCameraControl {
    if follow_target_present {
        control.focus_offset = Vec2::ZERO;
    }
    control
}

fn gameplay_camera_control_for_mode(
    control: &GameplayCameraControl,
    single_player_preset: &SinglePlayerCameraPreset,
    single_player_camera_active: bool,
) -> GameplayCameraControl {
    if single_player_camera_active {
        single_player_preset.control()
    } else {
        *control
    }
}

fn decayed_camera_shake(shake: f32, dt: f32) -> f32 {
    (shake - dt * GAMEPLAY_CAMERA_SHAKE_DECAY_PER_SEC).max(0.0)
}

fn camera_action_shake_offset(shake: f32, elapsed: f32) -> Vec3 {
    if shake <= 0.0 {
        return Vec3::ZERO;
    }

    let t = elapsed * GAMEPLAY_CAMERA_SHAKE_FREQUENCY;
    Vec3::new(
        t.sin() * shake,
        0.0,
        (t * GAMEPLAY_CAMERA_SHAKE_SECONDARY_SCALE).cos() * shake,
    ) * GAMEPLAY_CAMERA_SHAKE_TRANSLATION_SCALE
}

fn gameplay_camera_follow_alpha(dt: f32) -> f32 {
    1.0 - (-CAMERA_FOLLOW_RATE * dt).exp()
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn gameplay_camera_pan_direction(keys: &ButtonInput<KeyCode>) -> Vec2 {
    if !shift_pressed(keys) || command_or_control_pressed(keys) {
        return Vec2::ZERO;
    }

    let mut direction = Vec2::ZERO;
    if keys.pressed(KeyCode::ArrowUp) {
        direction.y -= 1.0;
    }
    if keys.pressed(KeyCode::ArrowDown) {
        direction.y += 1.0;
    }
    direction
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn gameplay_camera_height_direction(keys: &ButtonInput<KeyCode>) -> f32 {
    if !shift_pressed(keys) || !command_or_control_pressed(keys) {
        return 0.0;
    }

    let mut direction = 0.0;
    if keys.pressed(KeyCode::ArrowUp) {
        direction += 1.0;
    }
    if keys.pressed(KeyCode::ArrowDown) {
        direction -= 1.0;
    }
    direction
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn gameplay_camera_rotation_direction(keys: &ButtonInput<KeyCode>) -> f32 {
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
fn command_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    keys.pressed(KeyCode::SuperLeft) || keys.pressed(KeyCode::SuperRight)
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn command_or_control_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    command_pressed(keys)
        || keys.pressed(KeyCode::ControlLeft)
        || keys.pressed(KeyCode::ControlRight)
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn camera_reset_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    shift_pressed(keys) && keys.just_pressed(KeyCode::KeyR)
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn single_player_camera_load_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    shift_pressed(keys) && command_or_control_pressed(keys) && keys.just_pressed(KeyCode::KeyL)
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn single_player_camera_save_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    shift_pressed(keys) && command_or_control_pressed(keys) && keys.just_pressed(KeyCode::KeyS)
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn single_player_camera_follow_toggle_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    shift_pressed(keys) && command_or_control_pressed(keys) && keys.just_pressed(KeyCode::KeyF)
}

pub fn camera_relative_direction(direction: Vec2, yaw: f32) -> Vec2 {
    let (sin, cos) = (-yaw).sin_cos();
    Vec2::new(
        direction.x * cos - direction.y * sin,
        direction.x * sin + direction.y * cos,
    )
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn update_gameplay_camera_control(
    control: &mut GameplayCameraControl,
    keys: &ButtonInput<KeyCode>,
    scroll_zoom: f32,
    dt: f32,
    dev_input_blocked: bool,
) {
    if dev_input_blocked {
        return;
    }

    if camera_reset_pressed(keys) {
        control.reset();
        return;
    }

    apply_gameplay_camera_pan(control, gameplay_camera_pan_direction(keys), dt);
    apply_gameplay_camera_rotation(control, gameplay_camera_rotation_direction(keys), dt);
    apply_gameplay_camera_height(control, gameplay_camera_height_direction(keys), dt);
    apply_gameplay_camera_scroll_zoom(control, gameplay_camera_scroll_zoom(keys, scroll_zoom));
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn apply_gameplay_camera_pan(control: &mut GameplayCameraControl, direction: Vec2, dt: f32) {
    if direction == Vec2::ZERO {
        return;
    }

    let relative_direction = camera_relative_direction(direction.normalize_or_zero(), control.yaw);
    control.focus_offset += relative_direction * GAMEPLAY_CAMERA_PAN_SPEED * dt;
    control.focus_offset = clamp_camera_focus_offset(control.focus_offset);
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn apply_gameplay_camera_rotation(control: &mut GameplayCameraControl, direction: f32, dt: f32) {
    if direction.abs() <= f32::EPSILON {
        return;
    }

    control.yaw = (control.yaw + direction * GAMEPLAY_CAMERA_ROTATE_SPEED * dt)
        .rem_euclid(std::f32::consts::TAU);
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn apply_gameplay_camera_height(control: &mut GameplayCameraControl, direction: f32, dt: f32) {
    if direction.abs() <= f32::EPSILON {
        return;
    }

    control.height_offset = (control.height_offset + direction * GAMEPLAY_CAMERA_HEIGHT_SPEED * dt)
        .clamp(
            GAMEPLAY_CAMERA_MIN_HEIGHT_OFFSET,
            GAMEPLAY_CAMERA_MAX_HEIGHT_OFFSET,
        );
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn gameplay_camera_scroll_zoom(keys: &ButtonInput<KeyCode>, scroll_zoom: f32) -> f32 {
    if shift_pressed(keys) {
        scroll_zoom
    } else {
        0.0
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn apply_gameplay_camera_scroll_zoom(control: &mut GameplayCameraControl, scroll_zoom: f32) {
    if scroll_zoom.abs() <= f32::EPSILON {
        return;
    }

    control.zoom =
        (control.zoom + scroll_zoom).clamp(GAMEPLAY_CAMERA_MIN_ZOOM, GAMEPLAY_CAMERA_MAX_ZOOM);
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn mouse_wheel_zoom_delta(event: &MouseWheel) -> f32 {
    let scale = match event.unit {
        MouseScrollUnit::Line => GAMEPLAY_CAMERA_SCROLL_LINE_ZOOM,
        MouseScrollUnit::Pixel => GAMEPLAY_CAMERA_SCROLL_PIXEL_ZOOM,
    };
    event.y * scale
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn clamp_camera_focus_offset(offset: Vec2) -> Vec2 {
    if offset.length() <= ARENA_RADIUS {
        return offset;
    }
    offset.normalize_or_zero() * ARENA_RADIUS
}

fn gameplay_camera_focus(center: Vec3, control: &GameplayCameraControl) -> Vec3 {
    center + Vec3::new(control.focus_offset.x, 0.0, control.focus_offset.y)
}

fn gameplay_camera_target(center: Vec3, farthest: f32, control: &GameplayCameraControl) -> Vec3 {
    let zoom = (farthest - 4.0).max(0.0) * 0.33;
    let mut offset =
        (active_arena_definition().camera_offset + Vec3::new(0.0, zoom * 0.6, zoom)) * control.zoom;
    offset.y += control.height_offset;
    gameplay_camera_focus(center, control) + Quat::from_rotation_y(control.yaw) * offset
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena_defs::{TRAINING_GROUND_ARENA_INDEX, arena_definition};

    fn assert_vec2_close(actual: Vec2, expected: Vec2, tolerance: f32) {
        assert!(
            actual.distance(expected) <= tolerance,
            "expected {actual:?} to be within {tolerance} of {expected:?}"
        );
    }

    fn assert_vec3_close(actual: Vec3, expected: Vec3, tolerance: f32) {
        assert!(
            actual.distance(expected) <= tolerance,
            "expected {actual:?} to be within {tolerance} of {expected:?}"
        );
    }

    #[test]
    fn training_ground_uses_the_standard_gameplay_camera_pitch() {
        let arena = arena_definition(TRAINING_GROUND_ARENA_INDEX);
        let transform = arena_camera_base_transform(arena);
        assert_vec3_close(transform.translation, arena.camera_offset, 0.001);
        assert!(
            arena
                .camera_offset
                .normalize()
                .distance(crate::constants::CAMERA_BASE_OFFSET.normalize())
                < 0.002
        );
    }

    #[test]
    fn single_player_camera_preset_roundtrips_ron() {
        let control = GameplayCameraControl {
            focus_offset: Vec2::new(1.5, -2.25),
            yaw: 0.75,
            zoom: 1.35,
            height_offset: 2.0,
        };
        let preset = SinglePlayerCameraPreset::new(control, false);
        let contents = ron::ser::to_string(&SinglePlayerCameraPresetDef::from(preset)).unwrap();

        let loaded = single_player_camera_preset_from_contents(Some(&contents));

        assert_eq!(loaded.control(), control);
        assert!(!loaded.follow_player());
    }

    #[test]
    fn old_single_player_camera_presets_default_to_follow_player() {
        let contents = "(
            focus_offset: (1.5, -2.25),
            yaw: 0.75,
            zoom: 1.35,
            height_offset: 2.0,
        )";

        let preset = single_player_camera_preset_from_contents(Some(contents));

        assert!(preset.follow_player());
    }

    #[test]
    fn single_player_camera_preset_falls_back_to_default_for_missing_or_invalid_data() {
        assert_eq!(
            single_player_camera_preset_from_contents(None),
            SinglePlayerCameraPreset::default()
        );
        assert_eq!(
            single_player_camera_preset_from_contents(Some("not valid ron")),
            SinglePlayerCameraPreset::default()
        );
    }

    #[test]
    fn single_player_camera_save_hotkey_requires_shift_and_command_or_control() {
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::SuperLeft);
        keys.press(KeyCode::KeyS);
        assert!(!single_player_camera_save_pressed(&keys));

        keys.press(KeyCode::ShiftLeft);
        assert!(single_player_camera_save_pressed(&keys));

        let mut control_keys = ButtonInput::<KeyCode>::default();
        control_keys.press(KeyCode::ShiftLeft);
        control_keys.press(KeyCode::ControlLeft);
        control_keys.press(KeyCode::KeyS);
        assert!(single_player_camera_save_pressed(&control_keys));
    }

    #[test]
    fn single_player_camera_load_hotkey_requires_shift_and_command_or_control() {
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::SuperLeft);
        keys.press(KeyCode::KeyL);
        assert!(!single_player_camera_load_pressed(&keys));

        keys.press(KeyCode::ShiftLeft);
        assert!(single_player_camera_load_pressed(&keys));

        let mut control_keys = ButtonInput::<KeyCode>::default();
        control_keys.press(KeyCode::ShiftLeft);
        control_keys.press(KeyCode::ControlLeft);
        control_keys.press(KeyCode::KeyL);
        assert!(single_player_camera_load_pressed(&control_keys));
    }

    #[test]
    fn single_player_camera_follow_toggle_requires_shift_and_command_or_control() {
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::SuperLeft);
        keys.press(KeyCode::KeyF);
        assert!(!single_player_camera_follow_toggle_pressed(&keys));

        keys.press(KeyCode::ShiftLeft);
        assert!(single_player_camera_follow_toggle_pressed(&keys));

        let mut control_keys = ButtonInput::<KeyCode>::default();
        control_keys.press(KeyCode::ShiftLeft);
        control_keys.press(KeyCode::ControlLeft);
        control_keys.press(KeyCode::KeyF);
        assert!(single_player_camera_follow_toggle_pressed(&control_keys));
    }

    #[test]
    fn single_player_camera_preset_replaces_live_control_when_loaded() {
        let mut control = GameplayCameraControl {
            focus_offset: Vec2::new(3.0, -1.0),
            yaw: 0.4,
            zoom: 1.4,
            height_offset: -2.0,
        };
        let mut mode = SinglePlayerCameraMode::new(true);
        let preset_control = GameplayCameraControl {
            focus_offset: Vec2::new(-2.0, 1.25),
            yaw: 1.1,
            zoom: 0.8,
            height_offset: 3.5,
        };
        let preset = SinglePlayerCameraPreset::new(preset_control, false);

        apply_single_player_camera_preset(&mut control, &mut mode, &preset);

        assert_eq!(control, preset_control);
        assert!(!mode.follow_player);
    }

    #[test]
    fn single_player_camera_save_contents_include_follow_mode() {
        let preset = SinglePlayerCameraPreset::new(
            GameplayCameraControl {
                focus_offset: Vec2::new(0.5, -0.25),
                yaw: 0.3,
                zoom: 1.2,
                height_offset: -1.0,
            },
            false,
        );

        let contents = single_player_camera_preset_contents(preset).unwrap();
        let loaded = single_player_camera_preset_from_contents(Some(&contents));

        assert_eq!(loaded, preset);
    }

    #[test]
    fn gameplay_camera_shift_arrows_choose_pan_and_rotation() {
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::ArrowUp);
        assert_eq!(gameplay_camera_pan_direction(&keys), Vec2::ZERO);
        assert_eq!(gameplay_camera_height_direction(&keys), 0.0);
        assert_eq!(gameplay_camera_rotation_direction(&keys), 0.0);

        keys.press(KeyCode::ShiftLeft);
        assert_eq!(gameplay_camera_pan_direction(&keys), Vec2::NEG_Y);
        assert_eq!(gameplay_camera_height_direction(&keys), 0.0);

        keys.press(KeyCode::ArrowLeft);
        assert_eq!(gameplay_camera_rotation_direction(&keys), -1.0);
    }

    #[test]
    fn gameplay_camera_shift_command_vertical_arrows_change_height_not_pan() {
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::ShiftLeft);
        keys.press(KeyCode::SuperLeft);
        keys.press(KeyCode::ArrowUp);

        assert_eq!(gameplay_camera_pan_direction(&keys), Vec2::ZERO);
        assert_eq!(gameplay_camera_height_direction(&keys), 1.0);

        keys.release(KeyCode::ArrowUp);
        keys.press(KeyCode::ArrowDown);

        assert_eq!(gameplay_camera_pan_direction(&keys), Vec2::ZERO);
        assert_eq!(gameplay_camera_height_direction(&keys), -1.0);

        let mut control_keys = ButtonInput::<KeyCode>::default();
        control_keys.press(KeyCode::ShiftLeft);
        control_keys.press(KeyCode::ControlLeft);
        control_keys.press(KeyCode::ArrowUp);

        assert_eq!(gameplay_camera_pan_direction(&control_keys), Vec2::ZERO);
        assert_eq!(gameplay_camera_height_direction(&control_keys), 1.0);
    }

    #[test]
    fn gameplay_camera_pan_is_relative_to_current_yaw() {
        let mut control = GameplayCameraControl {
            focus_offset: Vec2::ZERO,
            yaw: std::f32::consts::FRAC_PI_2,
            zoom: 1.0,
            height_offset: 0.0,
        };

        apply_gameplay_camera_pan(&mut control, Vec2::NEG_Y, 0.5);

        assert!(control.focus_offset.x < 0.0);
        assert!(control.focus_offset.y.abs() < 0.001);
    }

    #[test]
    fn gameplay_camera_focus_offset_clamps_to_arena() {
        let mut control = GameplayCameraControl::default();
        apply_gameplay_camera_pan(&mut control, Vec2::X, 999.0);

        assert!(control.focus_offset.length() <= ARENA_RADIUS);
    }

    #[test]
    fn gameplay_camera_target_preserves_follow_center_with_yaw() {
        let control = GameplayCameraControl {
            focus_offset: Vec2::new(2.0, -1.0),
            yaw: std::f32::consts::FRAC_PI_2,
            zoom: 1.0,
            height_offset: 0.0,
        };
        let center = Vec3::new(1.0, 0.0, 3.0);
        let focus = gameplay_camera_focus(center, &control);
        let target = gameplay_camera_target(center, 4.0, &control);
        let expected_target =
            focus + Quat::from_rotation_y(control.yaw) * active_arena_definition().camera_offset;

        assert_vec3_close(focus, Vec3::new(3.0, 0.0, 2.0), 0.001);
        assert_vec3_close(target, expected_target, 0.001);
    }

    #[test]
    fn gameplay_camera_center_uses_single_player_target_when_available() {
        let samples = [
            CameraFighterSample {
                id: 0,
                position: Vec3::new(10.0, 3.0, -2.0),
            },
            CameraFighterSample {
                id: 1,
                position: Vec3::new(-2.0, 0.0, 6.0),
            },
        ];

        assert_vec3_close(
            gameplay_camera_center_for_samples(&samples, Some(0)),
            Vec3::new(10.0, 0.0, -2.0),
            0.001,
        );
        assert_vec3_close(
            gameplay_camera_center_for_samples(&samples, None),
            Vec3::new(4.0, 0.0, 2.0),
            0.001,
        );
    }

    #[test]
    fn gameplay_camera_center_falls_back_to_average_when_target_missing() {
        let samples = [
            CameraFighterSample {
                id: 0,
                position: Vec3::new(8.0, 0.0, 0.0),
            },
            CameraFighterSample {
                id: 1,
                position: Vec3::new(-2.0, 0.0, 0.0),
            },
        ];

        assert_vec3_close(
            gameplay_camera_center_for_samples(&samples, Some(99)),
            Vec3::new(3.0, 0.0, 0.0),
            0.001,
        );
    }

    #[test]
    fn gameplay_camera_farthest_uses_selected_center_for_zoom_pressure() {
        let samples = [
            CameraFighterSample {
                id: 0,
                position: Vec3::new(10.0, 0.0, 0.0),
            },
            CameraFighterSample {
                id: 1,
                position: Vec3::new(-2.0, 0.0, 0.0),
            },
        ];

        assert_eq!(
            gameplay_camera_farthest_from_center(&samples, Vec3::new(10.0, 0.0, 0.0)),
            12.0
        );
    }

    #[test]
    fn gameplay_camera_follow_mode_suppresses_opponent_zoom_pressure() {
        let samples = [
            CameraFighterSample {
                id: 0,
                position: Vec3::new(10.0, 0.0, 0.0),
            },
            CameraFighterSample {
                id: 1,
                position: Vec3::new(-2.0, 0.0, 0.0),
            },
        ];

        assert!(gameplay_camera_target_present(&samples, Some(0)));
        assert!(!gameplay_camera_target_present(&samples, Some(99)));

        assert_eq!(
            gameplay_camera_farthest_for_mode(&samples, Vec3::new(10.0, 0.0, 0.0), true),
            0.0
        );
        assert_eq!(
            gameplay_camera_farthest_for_mode(&samples, Vec3::new(10.0, 0.0, 0.0), false),
            12.0
        );
    }

    #[test]
    fn gameplay_camera_control_uses_saved_preset_only_for_single_player_mode() {
        let live_control = GameplayCameraControl {
            focus_offset: Vec2::new(1.0, 0.0),
            yaw: 0.2,
            zoom: 1.1,
            height_offset: 0.5,
        };
        let saved_control = GameplayCameraControl {
            focus_offset: Vec2::new(-2.0, 1.5),
            yaw: 1.2,
            zoom: 1.6,
            height_offset: 3.0,
        };
        let preset = SinglePlayerCameraPreset::new(saved_control, false);

        assert_eq!(
            gameplay_camera_control_for_mode(&live_control, &preset, true),
            saved_control
        );
        assert_eq!(
            gameplay_camera_control_for_mode(&live_control, &preset, false),
            live_control
        );
    }

    #[test]
    fn gameplay_camera_user_follow_target_uses_user_target_only() {
        assert_eq!(
            gameplay_camera_user_follow_target_id(Some(0), &SinglePlayerCameraMode::new(true)),
            Some(0)
        );
        assert_eq!(
            gameplay_camera_user_follow_target_id(Some(0), &SinglePlayerCameraMode::new(false)),
            None
        );
        assert_eq!(
            gameplay_camera_user_follow_target_id(None, &SinglePlayerCameraMode::new(true)),
            None
        );
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    #[test]
    fn gameplay_camera_native_dev_follow_target_falls_back_outside_user_mode() {
        assert_eq!(
            gameplay_camera_native_dev_follow_target_id(false, &SinglePlayerCameraMode::new(true)),
            Some(DEV_PLAYER_CAMERA_TARGET_ID)
        );
        assert_eq!(
            gameplay_camera_native_dev_follow_target_id(true, &SinglePlayerCameraMode::new(true)),
            None
        );
        assert_eq!(
            gameplay_camera_native_dev_follow_target_id(false, &SinglePlayerCameraMode::new(false)),
            None
        );
    }

    #[test]
    fn gameplay_camera_follow_target_ignores_saved_pan_offset() {
        let control = GameplayCameraControl {
            focus_offset: Vec2::new(2.0, -3.0),
            yaw: 1.0,
            zoom: 1.2,
            height_offset: 0.5,
        };

        let follow_control = gameplay_camera_control_for_follow_target(control, true);
        assert_eq!(follow_control.focus_offset, Vec2::ZERO);
        assert_eq!(follow_control.yaw, control.yaw);
        assert_eq!(follow_control.zoom, control.zoom);
        assert_eq!(follow_control.height_offset, control.height_offset);
        assert_eq!(
            gameplay_camera_control_for_follow_target(control, false),
            control
        );
    }

    #[test]
    fn gameplay_camera_rotation_wraps() {
        let mut control = GameplayCameraControl {
            focus_offset: Vec2::ZERO,
            yaw: 0.1,
            zoom: 1.0,
            height_offset: 0.0,
        };

        apply_gameplay_camera_rotation(&mut control, -1.0, 1.0);

        assert!(control.yaw > std::f32::consts::PI);
    }

    #[test]
    fn gameplay_camera_height_offset_clamps() {
        let mut control = GameplayCameraControl::default();

        apply_gameplay_camera_height(&mut control, 1.0, 99.0);
        assert_eq!(control.height_offset, GAMEPLAY_CAMERA_MAX_HEIGHT_OFFSET);

        apply_gameplay_camera_height(&mut control, -1.0, 99.0);
        assert_eq!(control.height_offset, GAMEPLAY_CAMERA_MIN_HEIGHT_OFFSET);
    }

    #[test]
    fn camera_relative_pan_direction_matches_editor_navigation() {
        let forward = camera_relative_direction(Vec2::NEG_Y, std::f32::consts::FRAC_PI_2);
        let right = camera_relative_direction(Vec2::X, std::f32::consts::FRAC_PI_2);

        assert_vec2_close(forward, Vec2::NEG_X, 0.001);
        assert_vec2_close(right, Vec2::NEG_Y, 0.001);
    }

    #[test]
    fn gameplay_camera_scroll_zoom_requires_shift_and_clamps() {
        let mut keys = ButtonInput::<KeyCode>::default();
        assert_eq!(gameplay_camera_scroll_zoom(&keys, 0.25), 0.0);

        keys.press(KeyCode::ShiftLeft);
        assert_eq!(gameplay_camera_scroll_zoom(&keys, 0.25), 0.25);

        let mut control = GameplayCameraControl::default();
        apply_gameplay_camera_scroll_zoom(&mut control, 99.0);
        assert_eq!(control.zoom, GAMEPLAY_CAMERA_MAX_ZOOM);
        apply_gameplay_camera_scroll_zoom(&mut control, -99.0);
        assert_eq!(control.zoom, GAMEPLAY_CAMERA_MIN_ZOOM);
    }

    #[test]
    fn gameplay_camera_target_distance_scales_with_zoom() {
        let center = Vec3::ZERO;
        let mut control = GameplayCameraControl::default();
        let normal_distance = gameplay_camera_target(center, 4.0, &control).length();

        control.zoom = 1.6;
        let zoomed_distance = gameplay_camera_target(center, 4.0, &control).length();

        assert!(zoomed_distance > normal_distance);
    }

    #[test]
    fn gameplay_camera_target_height_offset_changes_vertical_position() {
        let center = Vec3::ZERO;
        let mut control = GameplayCameraControl::default();
        let normal_target = gameplay_camera_target(center, 4.0, &control);

        control.height_offset = 3.0;
        let raised_target = gameplay_camera_target(center, 4.0, &control);

        assert_vec3_close(raised_target, normal_target + Vec3::Y * 3.0, 0.001);
    }

    #[test]
    fn camera_action_shake_is_not_follow_lerp_scaled() {
        let shake = camera_action_shake_offset(1.0, 0.0);
        let follow_alpha = gameplay_camera_follow_alpha(1.0 / 60.0);

        assert!(shake.length() > 0.3);
        assert!(follow_alpha < 0.1);
    }

    #[test]
    fn camera_action_shake_decays_to_zero() {
        assert_eq!(decayed_camera_shake(0.1, 1.0), 0.0);
        assert!(decayed_camera_shake(0.5, 0.1) < 0.5);
    }

    #[test]
    fn camera_action_effects_are_enabled_by_default() {
        assert!(CameraActionEffects::default().enabled);
    }

    #[test]
    fn gameplay_camera_reset_restores_default_view_state() {
        let mut control = GameplayCameraControl {
            focus_offset: Vec2::new(4.0, -3.0),
            yaw: 1.2,
            zoom: 1.8,
            height_offset: 4.0,
        };

        control.reset();

        assert_eq!(control, GameplayCameraControl::default());
    }

    #[test]
    fn gameplay_camera_reset_has_same_frame_priority() {
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::ShiftLeft);
        keys.press(KeyCode::KeyR);
        keys.press(KeyCode::ArrowRight);
        let mut control = GameplayCameraControl {
            focus_offset: Vec2::new(4.0, -3.0),
            yaw: 1.2,
            zoom: 1.8,
            height_offset: 4.0,
        };

        update_gameplay_camera_control(&mut control, &keys, 0.5, 0.5, false);

        assert_eq!(control, GameplayCameraControl::default());
    }

    #[test]
    fn gameplay_camera_control_ignores_dev_input_when_blocked() {
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::ShiftLeft);
        keys.press(KeyCode::KeyR);
        keys.press(KeyCode::ArrowRight);
        let mut control = GameplayCameraControl {
            focus_offset: Vec2::new(4.0, -3.0),
            yaw: 1.2,
            zoom: 1.8,
            height_offset: 4.0,
        };
        let unchanged = control;

        update_gameplay_camera_control(&mut control, &keys, 0.5, 0.5, true);

        assert_eq!(control, unchanged);
    }

    #[test]
    fn mouse_wheel_zoom_delta_matches_editor_scale() {
        let line = MouseWheel {
            unit: MouseScrollUnit::Line,
            x: 0.0,
            y: 1.0,
            window: Entity::PLACEHOLDER,
        };
        let pixel = MouseWheel {
            unit: MouseScrollUnit::Pixel,
            x: 0.0,
            y: 10.0,
            window: Entity::PLACEHOLDER,
        };

        assert_eq!(
            mouse_wheel_zoom_delta(&line),
            GAMEPLAY_CAMERA_SCROLL_LINE_ZOOM
        );
        assert_eq!(
            mouse_wheel_zoom_delta(&pixel),
            GAMEPLAY_CAMERA_SCROLL_PIXEL_ZOOM * 10.0
        );
    }
}
