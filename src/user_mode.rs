use bevy::asset::LoadState;
use bevy::camera::{RenderTarget, visibility::RenderLayers};
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use bevy::scene::SceneInstanceReady;
use bevy::time::{Real, Virtual};
use bevy::ui::UiTargetCamera;

use crate::arena::ARENA_PREVIEW_RENDER_LAYER;
use crate::arena_defs::{active_arena_index, arena_definitions, set_active_arena_index};
use crate::bot::start_bot_combat_ai;
use crate::camera::{ScreenLook, ScreenLookTransition, UiCamera, begin_screen_look_transition};
use crate::characters::{
    CharacterKind, CharacterMoveCatalog, character_label, character_scene_model,
};
use crate::combat::HitEffects;
use crate::combat_sfx::{CombatSfxCue, CombatSfxKind};
use crate::components::{
    BotBrain, ControlAction, Controller, Fighter, FighterInput, LocalInputAssignment,
    PlayerControlBindings, PlayerKeyBindings,
};
use crate::constants::FIGHTER_COUNT;
use crate::control_settings::{
    ControlPreferences, ControllerDeviceInfo, ControllerFamily, controller_info,
    request_controller_rumble, save_control_preferences,
};
use crate::controller_haptics::{
    ControllerHapticRequest, HapticPlaybackEvent, HapticPlaybackResult, HapticPurpose,
    combat_preview_pattern, controller_test_pattern, queue_haptic_pattern,
};
use crate::game_state::{
    GameplayPauseOwner, GameplayPauseOwners, LocalSetup, MatchAnnouncements, MatchPhase,
    MatchState, reconcile_fighter_control_from_setup,
};
use crate::tutorial::{TutorialTransition, TutorialTransitionAction, request_tutorial_transition};

const USER_MODE_MENU_MUSIC_PATH: &str = "music/bgm/cc0_menu_menu_music.ogg";
const USER_MODE_MAIN_MENU_BACKGROUND_FADE_SECS: f32 = 0.35;
const USER_MODE_SINGLE_PLAYER_BACKGROUND_PATH: &str =
    "backgrounds/menu/animal_fighter_single_player_background_1920x1080.png";
const USER_MODE_MULTIPLAYER_BACKGROUND_PATH: &str = "backgrounds/menu/afc_multiplayer_menu.png";
const USER_MODE_TUTORIAL_BACKGROUND_PATH: &str =
    "backgrounds/menu/animal_fighter_tutorial_background_1920x1080.png";
const USER_MODE_SETTINGS_BACKGROUND_PATH: &str =
    "backgrounds/menu/animal_fighter_settings_background_1920x1080.png";
const USER_MODE_BATTLE_MUSIC_PATHS: [&str; 10] = [
    "music/bgm/cc0_crown_hope.ogg",
    "music/bgm/cc0_causeway_pirate_tune.ogg",
    "music/bgm/cc0_sunstone_desert_loop.mp3",
    "music/bgm/cc0_crank_robotic_city.ogg",
    "music/bgm/cc0_vent_urgent.mp3",
    "music/bgm/cc0_bumper_carnival_rides.ogg",
    "music/bgm/cc0_feast_medieval_fair.ogg",
    "music/bgm/cc0_snare_rhythm_garden.ogg",
    "music/bgm/cc0_sky_snow_stage.ogg",
    "music/bgm/cc0_powder_pirate_indenture_loop.wav",
];
const USER_MODE_PREVIEW_TEXTURE_SIZE: u32 = 384;
const USER_MODE_PREVIEW_LAYER: usize = 20;
const USER_MODE_PREVIEW_ORIGIN: Vec3 = Vec3::new(92.0, 32.0, 92.0);
const USER_MODE_PREVIEW_SCALE: f32 = 1.248;
const USER_MODE_ARENA_PREVIEW_TEXTURE_WIDTH: u32 = 720;
const USER_MODE_ARENA_PREVIEW_TEXTURE_HEIGHT: u32 = 480;
const USER_MODE_ARENA_PREVIEW_CAMERA_DISTANCE_SCALE: f32 = 1.18;
const USER_MODE_PLAYER_FIGHTER_ID: usize = 0;
const USER_MODE_BOT_FIGHTER_ID: usize = 1;
const USER_MODE_STOCK_RULE_INDEX: usize = 2;
const USER_MODE_DEATH_SLOW_MOTION_SCALE: f32 = 0.22;
const USER_MODE_NOIR_FADE_SECS: f32 = 1.2;
const USER_MODE_RESULT_MENU_DELAY_SECS: f32 = 1.65;
const USER_MODE_FILTER_RESET_SECS: f32 = 0.45;
const USER_MODE_RESULT_SFX_PRIORITY: u8 = 120;
const USER_MODE_MENU_STICK_THRESHOLD: f32 = 0.55;
const USER_MODE_MENU_REPEAT_DELAY: f32 = 0.38;
const USER_MODE_MENU_REPEAT_INTERVAL: f32 = 0.12;
const USER_MODE_CHOICE_FONT_SIZE: f32 = 23.8;
const USER_MODE_SELECTABLE_CHARACTERS: [CharacterKind; 5] = [
    CharacterKind::Cat,
    CharacterKind::Pig,
    CharacterKind::Bee,
    CharacterKind::Penguin,
    CharacterKind::Chick,
];
const USER_MODE_DEFAULT_CHARACTERS: [CharacterKind; FIGHTER_COUNT] = [
    CharacterKind::Cat,
    CharacterKind::Pig,
    CharacterKind::Bee,
    CharacterKind::Penguin,
];
const USER_MODE_KEY_ROW_FONT_SIZE: f32 = 18.0;
const USER_MODE_KEY_ROW_HEIGHT: f32 = 34.0;
const USER_MODE_KEY_ROW_GAP: f32 = 3.0;
const USER_MODE_KEY_VISIBLE_ROWS: usize = 6;
const USER_MODE_KEY_ROW_PITCH: f32 = USER_MODE_KEY_ROW_HEIGHT + USER_MODE_KEY_ROW_GAP;
const USER_MODE_KEY_LIST_HEIGHT: f32 = USER_MODE_KEY_ROW_HEIGHT * USER_MODE_KEY_VISIBLE_ROWS as f32
    + USER_MODE_KEY_ROW_GAP * (USER_MODE_KEY_VISIBLE_ROWS - 1) as f32;
#[cfg(target_arch = "wasm32")]
const WEB_BATTLE_WARMUP_SECS: f32 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserModeScreen {
    Dev,
    Start,
    ModeSelect,
    PlayerCountSelect,
    DeviceJoin,
    ControlsHub,
    ControllerTest,
    KeySettings,
    CharacterSelect,
    ArenaSelect,
    ControlsBriefing,
    BattleResult,
    TutorialHub,
    TutorialLesson,
    TutorialPause,
    TutorialFinalResult,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ControlsHubChoice {
    #[default]
    ControllerSetup,
    ControllerTest,
    KeyboardControls,
}

impl ControlsHubChoice {
    fn previous(self) -> Self {
        match self {
            Self::ControllerSetup => Self::KeyboardControls,
            Self::ControllerTest => Self::ControllerSetup,
            Self::KeyboardControls => Self::ControllerTest,
        }
    }

    fn next(self) -> Self {
        match self {
            Self::ControllerSetup => Self::ControllerTest,
            Self::ControllerTest => Self::KeyboardControls,
            Self::KeyboardControls => Self::ControllerSetup,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ControllerSetupContext {
    #[default]
    Match,
    Settings,
    Tutorial,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ControllerSetupPhase {
    #[default]
    Normal,
    Reorder,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserPlayMode {
    SinglePlayer,
    TwoPlayers,
    ThreePlayers,
    FourPlayers,
}

impl UserPlayMode {
    const fn human_player_count(self) -> usize {
        match self {
            Self::SinglePlayer => 1,
            Self::TwoPlayers => 2,
            Self::ThreePlayers => 3,
            Self::FourPlayers => 4,
        }
    }

    const fn is_single_player(self) -> bool {
        matches!(self, Self::SinglePlayer)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UserModeMainMenuChoice {
    SinglePlayer,
    Multiplayer,
    Tutorial,
    Settings,
}

impl UserModeMainMenuChoice {
    fn previous(self) -> Self {
        match self {
            Self::SinglePlayer => Self::Settings,
            Self::Multiplayer => Self::SinglePlayer,
            Self::Tutorial => Self::Multiplayer,
            Self::Settings => Self::Tutorial,
        }
    }

    fn next(self) -> Self {
        match self {
            Self::SinglePlayer => Self::Multiplayer,
            Self::Multiplayer => Self::Tutorial,
            Self::Tutorial => Self::Settings,
            Self::Settings => Self::SinglePlayer,
        }
    }
}

const USER_MODE_MAIN_MENU_BACKGROUNDS: &[(UserModeMainMenuChoice, &str)] = &[
    (
        UserModeMainMenuChoice::SinglePlayer,
        USER_MODE_SINGLE_PLAYER_BACKGROUND_PATH,
    ),
    (
        UserModeMainMenuChoice::Multiplayer,
        USER_MODE_MULTIPLAYER_BACKGROUND_PATH,
    ),
    (
        UserModeMainMenuChoice::Tutorial,
        USER_MODE_TUTORIAL_BACKGROUND_PATH,
    ),
    (
        UserModeMainMenuChoice::Settings,
        USER_MODE_SETTINGS_BACKGROUND_PATH,
    ),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UserModePlayerCountChoice {
    TwoPlayers,
    ThreePlayers,
    FourPlayers,
}

impl UserModePlayerCountChoice {
    fn previous(self) -> Self {
        match self {
            Self::TwoPlayers => Self::FourPlayers,
            Self::ThreePlayers => Self::TwoPlayers,
            Self::FourPlayers => Self::ThreePlayers,
        }
    }

    fn next(self) -> Self {
        match self {
            Self::TwoPlayers => Self::ThreePlayers,
            Self::ThreePlayers => Self::FourPlayers,
            Self::FourPlayers => Self::TwoPlayers,
        }
    }

    const fn play_mode(self) -> UserPlayMode {
        match self {
            Self::TwoPlayers => UserPlayMode::TwoPlayers,
            Self::ThreePlayers => UserPlayMode::ThreePlayers,
            Self::FourPlayers => UserPlayMode::FourPlayers,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct KeyBindingCapture {
    player: usize,
    action: ControlAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KeyBindingApplyResult {
    capture: KeyBindingCapture,
    swapped: Option<KeyBindingCapture>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UserModeResultChoice {
    PlayAgain,
    ChooseCharacter,
}

impl UserModeResultChoice {
    fn toggle(self) -> Self {
        match self {
            Self::PlayAgain => Self::ChooseCharacter,
            Self::ChooseCharacter => Self::PlayAgain,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UserModeMatchStartFlow {
    ControlsBriefing,
    BattleStarted,
}

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UserModeUiAction {
    MainMenu(UserModeMainMenuChoice),
    PlayerCount(UserModePlayerCountChoice),
    ControlsHub(ControlsHubChoice),
    Previous,
    Next,
    PreviousColumn,
    NextColumn,
    Confirm,
    Back,
    ControllerSetupReady,
    ControllerSetupChangeOrder,
    ControllerSetupClear,
    ControllerSetupRemoveSeat(usize),
    ToggleVibration,
    ToggleHapticStyle,
    TestVibration,
    TestCombatHaptics,
    ResetKeys,
    ConfirmKeyReset,
    CancelKeyReset,
    KeyBinding(KeyBindingCapture),
    Result(UserModeResultChoice),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UserModeRoute {
    None,
    CharacterPlayerAdvanced,
    ArenaEntered,
    ArenaChanged,
    PrepareMatch,
    ConfirmBattle,
    Replay,
    ChooseCharacter,
    ControlsBack,
    ExitToDev,
}

#[derive(Resource, Clone, Debug)]
pub struct UserModeState {
    screen: UserModeScreen,
    play_mode: UserPlayMode,
    main_menu_choice: UserModeMainMenuChoice,
    player_count_choice: UserModePlayerCountChoice,
    player_characters: [CharacterKind; FIGHTER_COUNT],
    input_assignments: [LocalInputAssignment; FIGHTER_COUNT],
    character_ready: [bool; FIGHTER_COUNT],
    arena_index: usize,
    character_select_player: usize,
    key_settings_cursor: usize,
    key_capture: Option<KeyBindingCapture>,
    controls_hub_choice: ControlsHubChoice,
    controller_setup_context: ControllerSetupContext,
    controller_setup_phase: ControllerSetupPhase,
    controller_setup_snapshot: [LocalInputAssignment; FIGHTER_COUNT],
    controller_setup_action_cursor: usize,
    controller_setup_input_latched: bool,
    controller_setup_clear_confirmation: bool,
    controller_test_cursor: usize,
    controller_test_active: Option<Entity>,
    controller_test_back_hold: f32,
    key_reset_confirmation: bool,
    controls_briefing_seen: bool,
    battle_music_pending: bool,
    battle_bot_ai_pending: bool,
    battle_active: bool,
    result_elapsed: f32,
    result_menu_ready: bool,
    result_choice: UserModeResultChoice,
    result_winner: Option<usize>,
}

#[derive(Resource)]
pub struct UserModeGameplayScene {
    loaded: bool,
    warmup_remaining: f32,
}

#[derive(Resource, Clone, Debug)]
pub struct LocalControllerReconnect {
    missing_seats: [bool; FIGHTER_COUNT],
    resume_delay_frames: u8,
    paused_by_reconnect: bool,
}

impl Default for LocalControllerReconnect {
    fn default() -> Self {
        Self {
            missing_seats: [false; FIGHTER_COUNT],
            resume_delay_frames: 0,
            paused_by_reconnect: false,
        }
    }
}

impl LocalControllerReconnect {
    pub fn blocks_gameplay(&self) -> bool {
        self.paused_by_reconnect || self.resume_delay_frames > 0
    }

    fn any_missing(&self) -> bool {
        self.missing_seats.iter().any(|missing| *missing)
    }

    fn clear(&mut self) {
        self.missing_seats = [false; FIGHTER_COUNT];
        self.resume_delay_frames = 0;
        self.paused_by_reconnect = false;
    }
}

impl Default for UserModeGameplayScene {
    fn default() -> Self {
        Self {
            loaded: !cfg!(target_arch = "wasm32"),
            warmup_remaining: 0.0,
        }
    }
}

impl UserModeGameplayScene {
    pub fn ready_for_battle(&self) -> bool {
        self.loaded && self.warmup_remaining <= 0.0
    }
}

pub fn gameplay_scene_loaded(scene: Res<UserModeGameplayScene>) -> bool {
    scene.loaded
}

#[cfg(target_arch = "wasm32")]
pub fn should_spawn_web_gameplay_scene(
    user_mode: Res<UserModeState>,
    scene: Res<UserModeGameplayScene>,
    state: Res<MatchState>,
) -> bool {
    !scene.loaded
        && (user_mode.screen == UserModeScreen::ArenaSelect
            || user_mode.screen == UserModeScreen::ControlsBriefing
            || user_mode.battle_music_pending
            || user_mode.battle_active
            || state.reset_requested
            || state.phase == MatchPhase::Resetting
            || state.phase == MatchPhase::Fighting)
}

#[cfg(target_arch = "wasm32")]
pub fn mark_web_gameplay_scene_loaded(mut scene: ResMut<UserModeGameplayScene>) {
    scene.loaded = true;
    scene.warmup_remaining = WEB_BATTLE_WARMUP_SECS;
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn should_spawn_web_gameplay_scene() -> bool {
    false
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn mark_web_gameplay_scene_loaded() {}

#[cfg(target_arch = "wasm32")]
struct WebMatchConfig {
    play_mode: UserPlayMode,
    player_characters: [CharacterKind; FIGHTER_COUNT],
    arena_index: usize,
    bindings: Option<PlayerKeyBindings>,
}

#[cfg(target_arch = "wasm32")]
fn take_web_match_config() -> Option<WebMatchConfig> {
    use wasm_bindgen::JsValue;

    let global = js_sys::global();
    let key = JsValue::from_str("__ffcMatchConfig");
    let value = js_sys::Reflect::get(&global, &key).ok()?;
    if value.is_null() || value.is_undefined() {
        return None;
    }
    let _ = js_sys::Reflect::set(&global, &key, &JsValue::NULL);

    let play_mode = match js_string_prop(&value, "mode").as_deref() {
        Some("two") => UserPlayMode::TwoPlayers,
        Some("three") => UserPlayMode::ThreePlayers,
        Some("four") => UserPlayMode::FourPlayers,
        _ => UserPlayMode::SinglePlayer,
    };
    let mut player_characters = USER_MODE_DEFAULT_CHARACTERS;
    for (player, character) in player_characters.iter_mut().enumerate() {
        let property = format!("p{}Character", player + 1);
        if let Some(parsed) = parse_web_character(js_string_prop(&value, &property).as_deref()) {
            *character = parsed;
        }
    }
    if play_mode.is_single_player() {
        player_characters[1] = opposite_user_mode_character(player_characters[0]);
    }
    let arena_index = js_number_prop(&value, "arenaIndex")
        .map(|index| index as usize)
        .unwrap_or(0)
        .min(arena_definitions().len().saturating_sub(1));
    let bindings = js_sys::Reflect::get(&value, &JsValue::from_str("bindings"))
        .ok()
        .and_then(|bindings| parse_web_bindings(&bindings));

    Some(WebMatchConfig {
        play_mode,
        player_characters,
        arena_index,
        bindings,
    })
}

#[cfg(target_arch = "wasm32")]
fn web_battle_start_signal_requested() -> bool {
    use wasm_bindgen::JsValue;

    let global = js_sys::global();
    let key = JsValue::from_str("__ffcStartBattle");
    let value = js_sys::Reflect::get(&global, &key).ok();
    value
        .as_ref()
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

#[cfg(target_arch = "wasm32")]
fn clear_web_battle_start_signal() {
    use wasm_bindgen::JsValue;

    let global = js_sys::global();
    let key = JsValue::from_str("__ffcStartBattle");
    let _ = js_sys::Reflect::set(&global, &key, &JsValue::FALSE);
}

#[cfg(target_arch = "wasm32")]
fn js_string_prop(value: &wasm_bindgen::JsValue, prop: &str) -> Option<String> {
    use wasm_bindgen::JsValue;

    js_sys::Reflect::get(value, &JsValue::from_str(prop))
        .ok()
        .and_then(|value| value.as_string())
}

#[cfg(target_arch = "wasm32")]
fn js_number_prop(value: &wasm_bindgen::JsValue, prop: &str) -> Option<f64> {
    use wasm_bindgen::JsValue;

    js_sys::Reflect::get(value, &JsValue::from_str(prop))
        .ok()
        .and_then(|value| value.as_f64())
}

#[cfg(target_arch = "wasm32")]
fn parse_web_character(value: Option<&str>) -> Option<CharacterKind> {
    match value? {
        "cat" => Some(CharacterKind::Cat),
        "pig" => Some(CharacterKind::Pig),
        "bee" => Some(CharacterKind::Bee),
        "penguin" => Some(CharacterKind::Penguin),
        "chick" => Some(CharacterKind::Chick),
        _ => None,
    }
}

#[cfg(target_arch = "wasm32")]
fn parse_web_bindings(value: &wasm_bindgen::JsValue) -> Option<PlayerKeyBindings> {
    use wasm_bindgen::JsValue;

    let defaults = PlayerKeyBindings::default();
    let p1 = js_sys::Reflect::get(value, &JsValue::from_str("p1")).ok()?;
    let p2 = js_sys::Reflect::get(value, &JsValue::from_str("p2")).ok()?;
    let p3 = js_sys::Reflect::get(value, &JsValue::from_str("p3")).ok()?;
    let p4 = js_sys::Reflect::get(value, &JsValue::from_str("p4")).ok()?;
    let bindings = PlayerKeyBindings {
        p1: parse_web_player_bindings(&p1, defaults.p1),
        p2: parse_web_player_bindings(&p2, defaults.p2),
        p3: parse_web_player_bindings(&p3, defaults.p3),
        p4: parse_web_player_bindings(&p4, defaults.p4),
    };
    (!bindings.has_duplicate_keys()).then_some(bindings)
}

#[cfg(target_arch = "wasm32")]
fn parse_web_player_bindings(
    value: &wasm_bindgen::JsValue,
    fallback: crate::components::PlayerControlBindings,
) -> crate::components::PlayerControlBindings {
    crate::components::PlayerControlBindings {
        left: js_string_prop(value, "left")
            .and_then(|value| parse_web_key_code(&value))
            .unwrap_or(fallback.left),
        right: js_string_prop(value, "right")
            .and_then(|value| parse_web_key_code(&value))
            .unwrap_or(fallback.right),
        up: js_string_prop(value, "up")
            .and_then(|value| parse_web_key_code(&value))
            .unwrap_or(fallback.up),
        down: js_string_prop(value, "down")
            .and_then(|value| parse_web_key_code(&value))
            .unwrap_or(fallback.down),
        aim_grab: js_string_prop(value, "aimGrab")
            .and_then(|value| parse_web_key_code(&value))
            .unwrap_or(fallback.aim_grab),
        heavy: js_string_prop(value, "heavy")
            .and_then(|value| parse_web_key_code(&value))
            .unwrap_or(fallback.heavy),
        light: js_string_prop(value, "light")
            .and_then(|value| parse_web_key_code(&value))
            .unwrap_or(fallback.light),
        jump: js_string_prop(value, "jump")
            .and_then(|value| parse_web_key_code(&value))
            .unwrap_or(fallback.jump),
        special: js_string_prop(value, "special")
            .and_then(|value| parse_web_key_code(&value))
            .unwrap_or(fallback.special),
    }
}

#[cfg(target_arch = "wasm32")]
fn parse_web_key_code(value: &str) -> Option<KeyCode> {
    let key = match value {
        "ArrowLeft" => KeyCode::ArrowLeft,
        "ArrowRight" => KeyCode::ArrowRight,
        "ArrowUp" => KeyCode::ArrowUp,
        "ArrowDown" => KeyCode::ArrowDown,
        "Comma" => KeyCode::Comma,
        "Minus" => KeyCode::Minus,
        "Period" => KeyCode::Period,
        "Digit0" => KeyCode::Digit0,
        "Digit1" => KeyCode::Digit1,
        "Digit2" => KeyCode::Digit2,
        "Digit3" => KeyCode::Digit3,
        "Digit4" => KeyCode::Digit4,
        "Digit5" => KeyCode::Digit5,
        "Digit6" => KeyCode::Digit6,
        "Digit7" => KeyCode::Digit7,
        "Digit8" => KeyCode::Digit8,
        "Digit9" => KeyCode::Digit9,
        "KeyA" => KeyCode::KeyA,
        "KeyB" => KeyCode::KeyB,
        "KeyC" => KeyCode::KeyC,
        "KeyD" => KeyCode::KeyD,
        "KeyE" => KeyCode::KeyE,
        "KeyF" => KeyCode::KeyF,
        "KeyG" => KeyCode::KeyG,
        "KeyH" => KeyCode::KeyH,
        "KeyI" => KeyCode::KeyI,
        "KeyJ" => KeyCode::KeyJ,
        "KeyK" => KeyCode::KeyK,
        "KeyL" => KeyCode::KeyL,
        "KeyM" => KeyCode::KeyM,
        "KeyN" => KeyCode::KeyN,
        "KeyO" => KeyCode::KeyO,
        "KeyP" => KeyCode::KeyP,
        "KeyQ" => KeyCode::KeyQ,
        "KeyR" => KeyCode::KeyR,
        "KeyS" => KeyCode::KeyS,
        "KeyT" => KeyCode::KeyT,
        "KeyU" => KeyCode::KeyU,
        "KeyV" => KeyCode::KeyV,
        "KeyW" => KeyCode::KeyW,
        "KeyX" => KeyCode::KeyX,
        "KeyY" => KeyCode::KeyY,
        "KeyZ" => KeyCode::KeyZ,
        _ => return None,
    };
    (!crate::components::reserved_binding_key(key)).then_some(key)
}

#[cfg(target_arch = "wasm32")]
fn set_web_battle_status(status: &str) {
    use wasm_bindgen::JsValue;

    let global = js_sys::global();
    let _ = js_sys::Reflect::set(
        &global,
        &JsValue::from_str("__ffcBattleStatus"),
        &JsValue::from_str(status),
    );
}

impl Default for UserModeState {
    fn default() -> Self {
        Self {
            screen: default_user_mode_screen(),
            play_mode: UserPlayMode::SinglePlayer,
            main_menu_choice: UserModeMainMenuChoice::SinglePlayer,
            player_count_choice: UserModePlayerCountChoice::TwoPlayers,
            player_characters: USER_MODE_DEFAULT_CHARACTERS,
            input_assignments: [LocalInputAssignment::Unassigned; FIGHTER_COUNT],
            character_ready: [false; FIGHTER_COUNT],
            arena_index: 0,
            character_select_player: 0,
            key_settings_cursor: 0,
            key_capture: None,
            controls_hub_choice: ControlsHubChoice::ControllerSetup,
            controller_setup_context: ControllerSetupContext::Match,
            controller_setup_phase: ControllerSetupPhase::Normal,
            controller_setup_snapshot: [LocalInputAssignment::Unassigned; FIGHTER_COUNT],
            controller_setup_action_cursor: 2,
            controller_setup_input_latched: false,
            controller_setup_clear_confirmation: false,
            controller_test_cursor: 0,
            controller_test_active: None,
            controller_test_back_hold: 0.0,
            key_reset_confirmation: false,
            controls_briefing_seen: false,
            battle_music_pending: false,
            battle_bot_ai_pending: false,
            battle_active: false,
            result_elapsed: 0.0,
            result_menu_ready: false,
            result_choice: UserModeResultChoice::PlayAgain,
            result_winner: None,
        }
    }
}

fn default_user_mode_screen() -> UserModeScreen {
    #[cfg(target_arch = "wasm32")]
    {
        UserModeScreen::ModeSelect
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    {
        UserModeScreen::Dev
    }
}

impl UserModeState {
    pub fn active(&self) -> bool {
        self.screen != UserModeScreen::Dev
    }

    pub fn screen(&self) -> UserModeScreen {
        self.screen
    }

    pub fn selected_character(&self) -> CharacterKind {
        self.player_characters[self.character_select_player.min(FIGHTER_COUNT - 1)]
    }

    pub fn blocks_dev_input(&self) -> bool {
        self.active() || self.battle_music_pending || self.battle_active
    }

    pub fn hides_dev_controls(&self) -> bool {
        self.blocks_dev_input()
    }

    pub fn shows_gameplay_hud(&self) -> bool {
        !self.active()
            || matches!(
                self.screen,
                UserModeScreen::TutorialLesson
                    | UserModeScreen::TutorialPause
                    | UserModeScreen::TutorialFinalResult
            )
    }

    pub fn restricts_bot_special_inputs(&self) -> bool {
        !self.tutorial_screen_active()
            && (self.battle_active
                || self.battle_music_pending
                || self.screen == UserModeScreen::ControlsBriefing
                || self.screen == UserModeScreen::BattleResult)
    }

    pub fn single_player_camera_target_id(&self) -> Option<usize> {
        (self.play_mode == UserPlayMode::SinglePlayer
            && (self.battle_active
                || self.battle_music_pending
                || self.screen == UserModeScreen::BattleResult))
            .then_some(USER_MODE_PLAYER_FIGHTER_ID)
    }

    pub fn tutorial_screen_active(&self) -> bool {
        matches!(
            self.screen,
            UserModeScreen::TutorialHub
                | UserModeScreen::TutorialLesson
                | UserModeScreen::TutorialPause
                | UserModeScreen::TutorialFinalResult
        ) || (self.screen == UserModeScreen::DeviceJoin
            && self.controller_setup_context == ControllerSetupContext::Tutorial)
    }

    pub(crate) fn tutorial_player_assignment(&self) -> LocalInputAssignment {
        match self.input_assignments[0] {
            LocalInputAssignment::Unassigned => LocalInputAssignment::Keyboard(0),
            assignment => assignment,
        }
    }

    pub(crate) fn enter_tutorial_hub(&mut self) {
        self.screen = UserModeScreen::TutorialHub;
        self.play_mode = UserPlayMode::SinglePlayer;
        self.key_capture = None;
        self.clear_battle_state();
    }

    pub(crate) fn enter_tutorial_lesson(&mut self) {
        self.screen = UserModeScreen::TutorialLesson;
        self.play_mode = UserPlayMode::SinglePlayer;
        self.battle_music_pending = true;
        self.battle_bot_ai_pending = false;
        self.battle_active = true;
        self.clear_result_state();
    }

    pub(crate) fn enter_tutorial_pause(&mut self) {
        self.screen = UserModeScreen::TutorialPause;
    }

    pub(crate) fn resume_tutorial_lesson(&mut self) {
        self.screen = UserModeScreen::TutorialLesson;
    }

    pub(crate) fn enter_tutorial_final_result(&mut self) {
        self.screen = UserModeScreen::TutorialFinalResult;
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    pub fn blocks_practice_health_refill(&self) -> bool {
        self.battle_active || self.screen == UserModeScreen::BattleResult
    }

    fn enter_fresh_mode_select(&mut self) {
        self.screen = UserModeScreen::ModeSelect;
        self.play_mode = UserPlayMode::SinglePlayer;
        self.main_menu_choice = UserModeMainMenuChoice::SinglePlayer;
        self.player_count_choice = UserModePlayerCountChoice::TwoPlayers;
        self.player_characters = USER_MODE_DEFAULT_CHARACTERS;
        self.clear_input_assignments();
        self.character_ready = [false; FIGHTER_COUNT];
        self.arena_index = 0;
        self.character_select_player = 0;
        self.key_settings_cursor = 0;
        self.key_capture = None;
        self.controls_hub_choice = ControlsHubChoice::ControllerSetup;
        self.controller_setup_context = ControllerSetupContext::Match;
        self.controller_setup_phase = ControllerSetupPhase::Normal;
        self.controller_setup_snapshot = [LocalInputAssignment::Unassigned; FIGHTER_COUNT];
        self.controller_setup_action_cursor = 2;
        self.controller_setup_input_latched = false;
        self.controller_setup_clear_confirmation = false;
        self.controller_test_cursor = 0;
        self.controller_test_active = None;
        self.controller_test_back_hold = 0.0;
        self.key_reset_confirmation = false;
        self.controls_briefing_seen = false;
        self.clear_battle_state();
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    fn exit_to_dev(&mut self) {
        self.screen = UserModeScreen::Dev;
        self.clear_battle_state();
    }

    fn exit_to_battle(&mut self) {
        self.screen = UserModeScreen::Dev;
        self.battle_music_pending = true;
        self.battle_bot_ai_pending = self.play_mode == UserPlayMode::SinglePlayer;
        self.battle_active = true;
        self.clear_result_state();
    }

    fn enter_character_select(&mut self) {
        self.screen = UserModeScreen::CharacterSelect;
        self.character_select_player = 0;
        self.character_ready = [false; FIGHTER_COUNT];
        self.key_capture = None;
        self.clear_battle_state();
    }

    fn enter_device_join(&mut self) {
        self.screen = UserModeScreen::DeviceJoin;
        self.controller_setup_context = ControllerSetupContext::Match;
        self.controller_setup_phase = ControllerSetupPhase::Normal;
        self.controller_setup_snapshot = self.input_assignments;
        self.controller_setup_action_cursor = 2;
        self.controller_setup_input_latched = false;
        self.controller_setup_clear_confirmation = false;
        self.character_ready = [false; FIGHTER_COUNT];
        self.character_select_player = 0;
        self.key_capture = None;
        self.clear_battle_state();
    }

    pub(crate) fn enter_tutorial_device_join(&mut self) {
        self.screen = UserModeScreen::DeviceJoin;
        self.play_mode = UserPlayMode::SinglePlayer;
        self.controller_setup_context = ControllerSetupContext::Tutorial;
        self.controller_setup_phase = ControllerSetupPhase::Normal;
        self.controller_setup_snapshot = self.input_assignments;
        self.controller_setup_action_cursor = 2;
        self.controller_setup_input_latched = false;
        self.controller_setup_clear_confirmation = false;
        self.character_ready = [false; FIGHTER_COUNT];
        self.character_select_player = 0;
        self.key_capture = None;
        self.clear_battle_state();
    }

    fn enter_settings_device_join(&mut self) {
        self.screen = UserModeScreen::DeviceJoin;
        self.controller_setup_context = ControllerSetupContext::Settings;
        self.controller_setup_phase = ControllerSetupPhase::Normal;
        self.controller_setup_snapshot = self.input_assignments;
        self.controller_setup_action_cursor = 2;
        self.controller_setup_input_latched = false;
        self.controller_setup_clear_confirmation = false;
        self.key_capture = None;
        self.clear_battle_state();
    }

    fn enter_controls_hub(&mut self) {
        self.screen = UserModeScreen::ControlsHub;
        self.key_capture = None;
        self.key_reset_confirmation = false;
        self.controller_test_active = None;
        self.controller_test_back_hold = 0.0;
        self.clear_battle_state();
    }

    fn enter_controller_test(&mut self) {
        self.screen = UserModeScreen::ControllerTest;
        self.controller_test_cursor = 0;
        self.controller_test_active = None;
        self.controller_test_back_hold = 0.0;
        self.key_capture = None;
        self.clear_battle_state();
    }

    fn enter_player_count_select(&mut self) {
        self.screen = UserModeScreen::PlayerCountSelect;
        self.key_capture = None;
        self.clear_battle_state();
    }

    fn enter_arena_select(&mut self) {
        self.screen = UserModeScreen::ArenaSelect;
        self.key_capture = None;
        self.clear_battle_state();
    }

    pub(crate) fn enter_mode_select(&mut self) {
        self.screen = UserModeScreen::ModeSelect;
        self.key_capture = None;
        self.clear_battle_state();
    }

    fn return_to_character_select_player(&mut self, player: usize) {
        self.screen = UserModeScreen::CharacterSelect;
        self.character_select_player = player.min(self.play_mode.human_player_count() - 1);
        self.character_ready = [false; FIGHTER_COUNT];
        self.key_capture = None;
        self.clear_battle_state();
    }

    fn enter_key_settings(&mut self) {
        self.screen = UserModeScreen::KeySettings;
        self.key_capture = None;
        self.key_reset_confirmation = false;
        self.key_settings_cursor = 0;
        self.clear_battle_state();
    }

    fn enter_controls_briefing(&mut self) {
        self.screen = UserModeScreen::ControlsBriefing;
        self.key_capture = None;
        self.controls_briefing_seen = true;
        self.clear_battle_state();
    }

    fn enter_battle_result(&mut self, winner: Option<usize>) {
        self.screen = UserModeScreen::BattleResult;
        self.result_elapsed = 0.0;
        self.result_menu_ready = false;
        self.result_choice = UserModeResultChoice::PlayAgain;
        self.result_winner = winner;
    }

    fn tick_battle_result(&mut self, dt: f32) -> bool {
        if self.screen != UserModeScreen::BattleResult || self.result_menu_ready {
            return false;
        }
        self.result_elapsed += dt;
        if self.result_elapsed >= USER_MODE_RESULT_MENU_DELAY_SECS {
            self.result_menu_ready = true;
            return true;
        }
        false
    }

    fn toggle_result_choice(&mut self) {
        self.result_choice = self.result_choice.toggle();
    }

    fn clear_battle_state(&mut self) {
        self.battle_music_pending = false;
        self.battle_bot_ai_pending = false;
        self.battle_active = false;
        self.clear_result_state();
    }

    fn clear_result_state(&mut self) {
        self.result_elapsed = 0.0;
        self.result_menu_ready = false;
        self.result_choice = UserModeResultChoice::PlayAgain;
        self.result_winner = None;
    }

    fn select_previous(&mut self) {
        self.set_selected_character(previous_user_mode_character(self.selected_character()));
    }

    fn select_next(&mut self) {
        self.set_selected_character(next_user_mode_character(self.selected_character()));
    }

    fn set_selected_character(&mut self, character: CharacterKind) {
        let player = self.character_select_player.min(FIGHTER_COUNT - 1);
        self.player_characters[player] = character;
    }

    fn select_previous_arena(&mut self) {
        let arena_count = arena_definitions().len().max(1);
        self.arena_index = (self.arena_index + arena_count - 1) % arena_count;
    }

    fn select_next_arena(&mut self) {
        let arena_count = arena_definitions().len().max(1);
        self.arena_index = (self.arena_index + 1) % arena_count;
    }

    fn confirm_character_selection(&mut self) -> bool {
        let player = self
            .character_select_player
            .min(self.play_mode.human_player_count() - 1);
        self.character_ready[player] = true;
        if self.all_characters_ready() {
            return true;
        }
        self.character_select_player = (0..self.play_mode.human_player_count())
            .find(|candidate| !self.character_ready[*candidate])
            .unwrap_or(0);
        false
    }

    fn all_characters_ready(&self) -> bool {
        self.character_ready
            .iter()
            .take(self.play_mode.human_player_count())
            .all(|ready| *ready)
    }

    fn clear_input_assignments(&mut self) {
        self.input_assignments = [LocalInputAssignment::Unassigned; FIGHTER_COUNT];
    }

    fn controller_setup_target(&self) -> usize {
        match self.controller_setup_context {
            ControllerSetupContext::Match => self.play_mode.human_player_count(),
            ControllerSetupContext::Settings => FIGHTER_COUNT,
            ControllerSetupContext::Tutorial => 1,
        }
    }

    fn joined_player_count(&self) -> usize {
        self.input_assignments
            .iter()
            .take(self.controller_setup_target())
            .filter(|assignment| **assignment != LocalInputAssignment::Unassigned)
            .count()
    }

    fn assignment_is_joined(&self, assignment: LocalInputAssignment) -> bool {
        self.input_assignments.contains(&assignment)
    }

    fn join_assignment(&mut self, assignment: LocalInputAssignment) -> Option<usize> {
        if assignment == LocalInputAssignment::Unassigned || self.assignment_is_joined(assignment) {
            return None;
        }
        let target = self.controller_setup_target();
        let seat = self
            .input_assignments
            .iter()
            .take(target)
            .position(|current| *current == LocalInputAssignment::Unassigned)?;
        self.input_assignments[seat] = assignment;
        Some(seat)
    }

    fn leave_assignment(&mut self, assignment: LocalInputAssignment) -> Option<usize> {
        let target = self.controller_setup_target();
        let seat = self
            .input_assignments
            .iter()
            .take(target)
            .position(|current| *current == assignment)?;
        for index in seat..target.saturating_sub(1) {
            self.input_assignments[index] = self.input_assignments[index + 1];
        }
        self.input_assignments[target - 1] = LocalInputAssignment::Unassigned;
        Some(seat)
    }

    fn remove_assignment_at(&mut self, seat: usize) -> Option<LocalInputAssignment> {
        let target = self.controller_setup_target();
        if seat >= target || self.input_assignments[seat] == LocalInputAssignment::Unassigned {
            return None;
        }
        let removed = self.input_assignments[seat];
        for index in seat..target.saturating_sub(1) {
            self.input_assignments[index] = self.input_assignments[index + 1];
        }
        self.input_assignments[target - 1] = LocalInputAssignment::Unassigned;
        Some(removed)
    }

    fn begin_controller_reorder(&mut self) {
        if self.controller_setup_phase == ControllerSetupPhase::Reorder {
            return;
        }
        self.controller_setup_snapshot = self.input_assignments;
        self.clear_input_assignments();
        self.controller_setup_phase = ControllerSetupPhase::Reorder;
        self.controller_setup_input_latched = true;
        self.controller_setup_clear_confirmation = false;
    }

    fn finish_controller_reorder(&mut self) {
        self.controller_setup_phase = ControllerSetupPhase::Normal;
        self.controller_setup_snapshot = self.input_assignments;
        self.controller_setup_clear_confirmation = false;
    }

    fn cancel_controller_reorder(&mut self) {
        self.input_assignments = self.controller_setup_snapshot;
        self.controller_setup_phase = ControllerSetupPhase::Normal;
        self.controller_setup_input_latched = false;
        self.controller_setup_clear_confirmation = false;
    }

    fn arm_or_confirm_clear_assignments(&mut self) -> bool {
        if self.controller_setup_clear_confirmation {
            self.clear_input_assignments();
            self.controller_setup_clear_confirmation = false;
            self.controller_setup_snapshot = self.input_assignments;
            true
        } else {
            self.controller_setup_clear_confirmation = true;
            false
        }
    }

    fn move_controller_setup_action(&mut self, direction: isize) {
        self.controller_setup_action_cursor =
            (self.controller_setup_action_cursor as isize + direction).rem_euclid(3) as usize;
    }

    fn selected_controller_setup_action(&self) -> UserModeUiAction {
        match self.controller_setup_action_cursor {
            0 => UserModeUiAction::ControllerSetupChangeOrder,
            1 => UserModeUiAction::ControllerSetupClear,
            _ => UserModeUiAction::ControllerSetupReady,
        }
    }

    fn ensure_test_or_web_assignments(&mut self) {
        for player in 0..self.play_mode.human_player_count() {
            if self.input_assignments[player] == LocalInputAssignment::Unassigned {
                self.input_assignments[player] = LocalInputAssignment::Keyboard(player);
            }
        }
    }

    fn selected_key_target(&self) -> KeyBindingCapture {
        let action_index = self.key_settings_cursor % ControlAction::ALL.len();
        KeyBindingCapture {
            player: self.key_settings_cursor / ControlAction::ALL.len(),
            action: ControlAction::ALL[action_index],
        }
    }

    fn move_key_cursor(&mut self, direction: isize) {
        let total = ControlAction::ALL.len() * FIGHTER_COUNT;
        self.key_settings_cursor =
            (self.key_settings_cursor as isize + direction).rem_euclid(total as isize) as usize;
    }

    fn move_key_column(&mut self, direction: isize) {
        let action_count = ControlAction::ALL.len();
        let action_index = self.key_settings_cursor % action_count;
        let player = self.key_settings_cursor / action_count;
        let next_player =
            (player as isize + direction).clamp(0, FIGHTER_COUNT as isize - 1) as usize;
        self.key_settings_cursor = next_player * action_count + action_index;
    }

    fn begin_key_capture(&mut self) {
        self.key_capture = Some(self.selected_key_target());
    }

    fn cancel_key_capture(&mut self) {
        self.key_capture = None;
    }

    fn apply_key_capture(
        &mut self,
        bindings: &mut PlayerKeyBindings,
        key: KeyCode,
    ) -> Result<KeyBindingApplyResult, &'static str> {
        let Some(capture) = self.key_capture else {
            return Err("capture");
        };
        let swapped = bindings
            .try_set_key_swapping(capture.player, capture.action, key)?
            .map(|(player, action)| KeyBindingCapture { player, action });
        self.key_capture = None;
        Ok(KeyBindingApplyResult { capture, swapped })
    }
}

fn activate_main_menu_choice(user_mode: &mut UserModeState, choice: UserModeMainMenuChoice) {
    user_mode.main_menu_choice = choice;
    match choice {
        UserModeMainMenuChoice::SinglePlayer => {
            user_mode.play_mode = UserPlayMode::SinglePlayer;
            user_mode.enter_device_join();
        }
        UserModeMainMenuChoice::Multiplayer => user_mode.enter_player_count_select(),
        UserModeMainMenuChoice::Tutorial => user_mode.enter_tutorial_device_join(),
        UserModeMainMenuChoice::Settings => user_mode.enter_controls_hub(),
    }
}

fn activate_player_count_choice(user_mode: &mut UserModeState, choice: UserModePlayerCountChoice) {
    user_mode.player_count_choice = choice;
    user_mode.play_mode = choice.play_mode();
    user_mode.enter_device_join();
}

fn route_user_mode_action(
    user_mode: &mut UserModeState,
    action: UserModeUiAction,
) -> UserModeRoute {
    if let UserModeUiAction::Back = action {
        if user_mode.key_capture.is_some() {
            user_mode.cancel_key_capture();
            return UserModeRoute::None;
        }
        if user_mode.key_reset_confirmation {
            user_mode.key_reset_confirmation = false;
            return UserModeRoute::None;
        }
        if user_mode.screen == UserModeScreen::DeviceJoin {
            if user_mode.controller_setup_clear_confirmation {
                user_mode.controller_setup_clear_confirmation = false;
                return UserModeRoute::None;
            }
            if user_mode.controller_setup_phase == ControllerSetupPhase::Reorder {
                user_mode.cancel_controller_reorder();
                return UserModeRoute::None;
            }
        }
        return match user_mode.screen {
            UserModeScreen::ModeSelect => {
                #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
                {
                    UserModeRoute::ExitToDev
                }
                #[cfg(target_arch = "wasm32")]
                {
                    UserModeRoute::None
                }
            }
            UserModeScreen::PlayerCountSelect | UserModeScreen::ControlsHub => {
                user_mode.enter_mode_select();
                UserModeRoute::None
            }
            UserModeScreen::ControllerTest | UserModeScreen::KeySettings => {
                user_mode.enter_controls_hub();
                UserModeRoute::None
            }
            UserModeScreen::DeviceJoin => {
                match user_mode.controller_setup_context {
                    ControllerSetupContext::Settings => user_mode.enter_controls_hub(),
                    ControllerSetupContext::Tutorial => user_mode.enter_mode_select(),
                    ControllerSetupContext::Match if user_mode.play_mode.is_single_player() => {
                        user_mode.enter_mode_select();
                    }
                    ControllerSetupContext::Match => user_mode.enter_player_count_select(),
                }
                UserModeRoute::None
            }
            UserModeScreen::CharacterSelect if user_mode.character_select_player > 0 => {
                user_mode.character_select_player -= 1;
                UserModeRoute::None
            }
            UserModeScreen::CharacterSelect => {
                user_mode.enter_device_join();
                UserModeRoute::None
            }
            UserModeScreen::ArenaSelect => {
                user_mode.return_to_character_select_player(
                    user_mode.play_mode.human_player_count().saturating_sub(1),
                );
                UserModeRoute::None
            }
            UserModeScreen::ControlsBriefing => {
                user_mode.controls_briefing_seen = false;
                user_mode.enter_arena_select();
                UserModeRoute::ControlsBack
            }
            _ => UserModeRoute::None,
        };
    }

    match (user_mode.screen, action) {
        (UserModeScreen::ModeSelect, UserModeUiAction::MainMenu(choice)) => {
            activate_main_menu_choice(user_mode, choice);
            UserModeRoute::None
        }
        (UserModeScreen::ModeSelect, UserModeUiAction::Previous) => {
            user_mode.main_menu_choice = user_mode.main_menu_choice.previous();
            UserModeRoute::None
        }
        (UserModeScreen::ModeSelect, UserModeUiAction::Next) => {
            user_mode.main_menu_choice = user_mode.main_menu_choice.next();
            UserModeRoute::None
        }
        (UserModeScreen::ModeSelect, UserModeUiAction::Confirm) => {
            activate_main_menu_choice(user_mode, user_mode.main_menu_choice);
            UserModeRoute::None
        }
        (UserModeScreen::PlayerCountSelect, UserModeUiAction::PlayerCount(choice)) => {
            activate_player_count_choice(user_mode, choice);
            UserModeRoute::None
        }
        (UserModeScreen::PlayerCountSelect, UserModeUiAction::Previous) => {
            user_mode.player_count_choice = user_mode.player_count_choice.previous();
            UserModeRoute::None
        }
        (UserModeScreen::PlayerCountSelect, UserModeUiAction::Next) => {
            user_mode.player_count_choice = user_mode.player_count_choice.next();
            UserModeRoute::None
        }
        (UserModeScreen::PlayerCountSelect, UserModeUiAction::Confirm) => {
            activate_player_count_choice(user_mode, user_mode.player_count_choice);
            UserModeRoute::None
        }
        (UserModeScreen::ControlsHub, UserModeUiAction::ControlsHub(choice)) => {
            user_mode.controls_hub_choice = choice;
            match choice {
                ControlsHubChoice::ControllerSetup => user_mode.enter_settings_device_join(),
                ControlsHubChoice::ControllerTest => user_mode.enter_controller_test(),
                ControlsHubChoice::KeyboardControls => user_mode.enter_key_settings(),
            }
            UserModeRoute::None
        }
        (UserModeScreen::ControlsHub, UserModeUiAction::Previous) => {
            user_mode.controls_hub_choice = user_mode.controls_hub_choice.previous();
            UserModeRoute::None
        }
        (UserModeScreen::ControlsHub, UserModeUiAction::Next) => {
            user_mode.controls_hub_choice = user_mode.controls_hub_choice.next();
            UserModeRoute::None
        }
        (UserModeScreen::ControlsHub, UserModeUiAction::Confirm) => {
            match user_mode.controls_hub_choice {
                ControlsHubChoice::ControllerSetup => user_mode.enter_settings_device_join(),
                ControlsHubChoice::ControllerTest => user_mode.enter_controller_test(),
                ControlsHubChoice::KeyboardControls => user_mode.enter_key_settings(),
            }
            UserModeRoute::None
        }
        (UserModeScreen::CharacterSelect, UserModeUiAction::Previous) => {
            user_mode.select_previous();
            UserModeRoute::None
        }
        (UserModeScreen::CharacterSelect, UserModeUiAction::Next) => {
            user_mode.select_next();
            UserModeRoute::None
        }
        (UserModeScreen::CharacterSelect, UserModeUiAction::Confirm) => {
            if user_mode.confirm_character_selection() {
                user_mode.enter_arena_select();
                UserModeRoute::ArenaEntered
            } else {
                UserModeRoute::CharacterPlayerAdvanced
            }
        }
        (UserModeScreen::ArenaSelect, UserModeUiAction::Previous) => {
            user_mode.select_previous_arena();
            UserModeRoute::ArenaChanged
        }
        (UserModeScreen::ArenaSelect, UserModeUiAction::Next) => {
            user_mode.select_next_arena();
            UserModeRoute::ArenaChanged
        }
        (UserModeScreen::ArenaSelect, UserModeUiAction::Confirm) => UserModeRoute::PrepareMatch,
        (UserModeScreen::KeySettings, UserModeUiAction::Previous) => {
            user_mode.move_key_cursor(-1);
            UserModeRoute::None
        }
        (UserModeScreen::KeySettings, UserModeUiAction::Next) => {
            user_mode.move_key_cursor(1);
            UserModeRoute::None
        }
        (UserModeScreen::KeySettings, UserModeUiAction::PreviousColumn) => {
            user_mode.move_key_column(-1);
            UserModeRoute::None
        }
        (UserModeScreen::KeySettings, UserModeUiAction::NextColumn) => {
            user_mode.move_key_column(1);
            UserModeRoute::None
        }
        (UserModeScreen::KeySettings, UserModeUiAction::Confirm) => {
            user_mode.begin_key_capture();
            UserModeRoute::None
        }
        (UserModeScreen::KeySettings, UserModeUiAction::KeyBinding(capture)) => {
            user_mode.key_settings_cursor = capture.player * ControlAction::ALL.len()
                + key_settings_action_index(capture.action);
            user_mode.begin_key_capture();
            UserModeRoute::None
        }
        (UserModeScreen::ControlsBriefing, UserModeUiAction::Confirm) => {
            UserModeRoute::ConfirmBattle
        }
        (UserModeScreen::BattleResult, UserModeUiAction::Previous | UserModeUiAction::Next) => {
            user_mode.toggle_result_choice();
            UserModeRoute::None
        }
        (UserModeScreen::BattleResult, UserModeUiAction::Result(choice)) => {
            user_mode.result_choice = choice;
            match choice {
                UserModeResultChoice::PlayAgain => UserModeRoute::Replay,
                UserModeResultChoice::ChooseCharacter => {
                    user_mode.enter_character_select();
                    UserModeRoute::ChooseCharacter
                }
            }
        }
        (UserModeScreen::BattleResult, UserModeUiAction::Confirm) => {
            match user_mode.result_choice {
                UserModeResultChoice::PlayAgain => UserModeRoute::Replay,
                UserModeResultChoice::ChooseCharacter => {
                    user_mode.enter_character_select();
                    UserModeRoute::ChooseCharacter
                }
            }
        }
        _ => UserModeRoute::None,
    }
}

#[derive(Component)]
pub(crate) struct UserModeRoot;

#[derive(Component)]
pub(crate) struct UserModeStartPanel;

#[derive(Component)]
pub(crate) struct UserModeMainMenuPanel;

#[derive(Clone, Copy, Debug, PartialEq)]
struct MainMenuBackgroundFade {
    opacity: f32,
    start_opacity: f32,
    target_opacity: f32,
    elapsed: f32,
}

impl Default for MainMenuBackgroundFade {
    fn default() -> Self {
        Self {
            opacity: 0.0,
            start_opacity: 0.0,
            target_opacity: 0.0,
            elapsed: 0.0,
        }
    }
}

impl MainMenuBackgroundFade {
    fn advance(&mut self, target_opacity: f32, delta_seconds: f32) -> f32 {
        if self.target_opacity != target_opacity {
            self.start_opacity = self.opacity;
            self.target_opacity = target_opacity;
            self.elapsed = 0.0;
        }

        if self.opacity == self.target_opacity {
            return self.opacity;
        }

        self.elapsed =
            (self.elapsed + delta_seconds.max(0.0)).min(USER_MODE_MAIN_MENU_BACKGROUND_FADE_SECS);
        let amount = (self.elapsed / USER_MODE_MAIN_MENU_BACKGROUND_FADE_SECS).clamp(0.0, 1.0);
        let eased = amount * amount * (3.0 - 2.0 * amount);
        self.opacity = self.start_opacity + (self.target_opacity - self.start_opacity) * eased;

        if amount >= 1.0 {
            self.opacity = self.target_opacity;
        }
        self.opacity
    }
}

#[derive(Component)]
pub(crate) struct UserModeMainMenuBackground {
    choice: UserModeMainMenuChoice,
    asset_path: &'static str,
    fade: MainMenuBackgroundFade,
    load_failure_reported: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MainMenuBackgroundTarget {
    Black,
    Choice(UserModeMainMenuChoice),
    HoldForLoad,
    Failed(UserModeMainMenuChoice),
}

#[derive(Component)]
pub(crate) struct UserModePlayerCountPanel;

#[derive(Component)]
pub(crate) struct UserModeDeviceJoinPanel;

#[derive(Component)]
pub(crate) struct UserModeDeviceJoinText;

#[derive(Component)]
pub(crate) struct UserModeDeviceJoinTitleText;

#[derive(Component)]
pub(crate) struct UserModeDeviceJoinSeatCard {
    seat: usize,
}

#[derive(Component)]
pub(crate) struct UserModeDeviceJoinSeatText {
    seat: usize,
}

#[derive(Component)]
pub(crate) struct UserModeDeviceJoinReadyText;

#[derive(Component)]
pub(crate) struct UserModeDeviceJoinClearText;

#[derive(Component)]
pub(crate) struct UserModeControlsHubPanel;

#[derive(Component)]
pub(crate) struct UserModeControllerTestPanel;

#[derive(Component)]
pub(crate) struct UserModeControllerTestText;

#[derive(Component)]
pub(crate) struct UserModeVibrationButtonText;

#[derive(Component)]
pub(crate) struct UserModeHapticStyleButtonText;

#[derive(Component)]
pub(crate) struct UserModeCharacterSelectPanel;

#[derive(Component)]
pub(crate) struct UserModeArenaSelectPanel;

#[derive(Component)]
pub(crate) struct UserModeKeySettingsPanel;

#[derive(Component)]
pub(crate) struct UserModeKeySettingsPromptText;

#[derive(Component)]
pub(crate) struct UserModeKeySettingsScroll {
    player: usize,
}

#[derive(Component)]
pub(crate) struct UserModeKeySettingsRowText {
    player: usize,
    action: ControlAction,
}

#[derive(Component)]
pub(crate) struct UserModeKeyResetPanel;

#[derive(Component)]
pub(crate) struct UserModeControlsPanel;

#[derive(Component)]
pub(crate) struct UserModeControlsText;

#[derive(Component)]
pub(crate) struct UserModeResultPanel;

#[derive(Component)]
pub(crate) struct UserModeResultText;

#[derive(Component)]
pub(crate) struct UserModeChoiceText;

#[derive(Component)]
pub(crate) struct UserModeCharacterTitleText;

#[derive(Component)]
pub(crate) struct UserModeBackButton;

#[derive(Component)]
pub(crate) struct ControllerReconnectOverlay;

#[derive(Component)]
pub(crate) struct ControllerReconnectText;

#[derive(Component)]
pub(crate) struct UserModeCharacterPreview;

#[derive(Component)]
pub(crate) struct UserModeArenaPreviewPanel;

#[derive(Component)]
pub(crate) struct UserModeArenaPreviewCamera;

#[derive(Component)]
pub(crate) struct UserModePreviewRoot;

#[derive(Component)]
pub(crate) struct UserModePreviewCamera;

#[derive(Component)]
pub(crate) struct UserModePreviewScene {
    character: CharacterKind,
}

#[derive(Component)]
pub(crate) struct UserModeMusic;

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ArenaMusic {
    arena_index: usize,
}

fn user_mode_preview_render_layers() -> RenderLayers {
    RenderLayers::layer(USER_MODE_PREVIEW_LAYER)
}

fn apply_user_mode_preview_render_layers(
    scene_ready: On<SceneInstanceReady>,
    children: Query<&Children>,
    mut commands: Commands,
) {
    for descendant in children.iter_descendants(scene_ready.entity) {
        commands
            .entity(descendant)
            .insert(user_mode_preview_render_layers());
    }
}

fn user_mode_action_button(
    label: impl Into<String>,
    action: UserModeUiAction,
    width: Val,
    height: f32,
    font_size: f32,
) -> impl Bundle {
    (
        Button,
        action,
        Node {
            width,
            height: Val::Px(height),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            border: UiRect::all(Val::Px(2.0)),
            padding: UiRect::axes(Val::Px(16.0), Val::Px(6.0)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.055, 0.055, 0.065, 0.94)),
        BorderColor::all(Color::srgb(0.42, 0.4, 0.35)),
        children![(
            Text::new(label),
            TextFont {
                font_size,
                ..default()
            },
            TextColor(Color::srgb(0.95, 0.86, 0.68)),
            TextShadow::default(),
            TextLayout::new_with_justify(Justify::Center),
        )],
    )
}

fn user_mode_back_button() -> impl Bundle {
    (
        Button,
        UserModeBackButton,
        UserModeUiAction::Back,
        Node {
            display: Display::None,
            position_type: PositionType::Absolute,
            left: Val::Px(28.0),
            top: Val::Px(24.0),
            width: Val::Px(112.0),
            height: Val::Px(44.0),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            border: UiRect::all(Val::Px(2.0)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.055, 0.055, 0.065, 0.94)),
        BorderColor::all(Color::srgb(0.42, 0.4, 0.35)),
        children![(
            Text::new("BACK"),
            TextFont {
                font_size: 19.0,
                ..default()
            },
            TextColor(Color::srgb(0.95, 0.86, 0.68)),
        )],
    )
}

fn controller_setup_seat_card(seat: usize) -> impl Bundle {
    (
        UserModeDeviceJoinSeatCard { seat },
        Node {
            width: Val::Percent(23.0),
            min_width: Val::Px(210.0),
            max_width: Val::Px(270.0),
            min_height: Val::Px(190.0),
            flex_direction: FlexDirection::Column,
            justify_content: JustifyContent::SpaceBetween,
            align_items: AlignItems::Center,
            row_gap: Val::Px(12.0),
            border: UiRect::all(Val::Px(3.0)),
            padding: UiRect::all(Val::Px(14.0)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.055, 0.055, 0.065, 0.94)),
        BorderColor::all(Color::srgb(0.42, 0.4, 0.35)),
        children![
            (
                Text::new(format!("PLAYER {}", seat + 1)),
                TextFont {
                    font_size: 21.0,
                    ..default()
                },
                TextColor(Color::srgb(0.98, 0.86, 0.58)),
                TextShadow::default(),
            ),
            (
                UserModeDeviceJoinSeatText { seat },
                Text::new("WAITING\nPress confirm"),
                TextFont {
                    font_size: 17.0,
                    ..default()
                },
                TextColor(Color::srgb(0.86, 0.82, 0.72)),
                TextLayout::new_with_justify(Justify::Center),
            ),
            user_mode_action_button(
                "REMOVE",
                UserModeUiAction::ControllerSetupRemoveSeat(seat),
                Val::Percent(100.0),
                38.0,
                16.0,
            ),
        ],
    )
}

fn key_settings_column(player: usize) -> impl Bundle {
    (
        Node {
            flex_basis: Val::Percent(25.0),
            max_width: Val::Px(250.0),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Stretch,
            row_gap: Val::Px(8.0),
            border: UiRect::all(Val::Px(2.0)),
            padding: UiRect::all(Val::Px(8.0)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.055, 0.055, 0.065, 0.88)),
        BorderColor::all(Color::srgb(0.63, 0.61, 0.56)),
        children![
            (
                Text::new(format!("P{}", player + 1)),
                TextFont {
                    font_size: 21.0,
                    ..default()
                },
                TextColor(Color::srgb(0.95, 0.86, 0.68)),
                TextShadow::default(),
            ),
            (
                UserModeKeySettingsScroll { player },
                Node {
                    width: Val::Percent(100.0),
                    height: Val::Px(USER_MODE_KEY_LIST_HEIGHT),
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(USER_MODE_KEY_ROW_GAP),
                    overflow: Overflow::scroll_y(),
                    ..default()
                },
                ScrollPosition::default(),
                children![
                    key_settings_row(player, ControlAction::Left),
                    key_settings_row(player, ControlAction::Right),
                    key_settings_row(player, ControlAction::Up),
                    key_settings_row(player, ControlAction::Down),
                    key_settings_row(player, ControlAction::AimGrab),
                    key_settings_row(player, ControlAction::Heavy),
                    key_settings_row(player, ControlAction::Light),
                    key_settings_row(player, ControlAction::Jump),
                    key_settings_row(player, ControlAction::Special),
                ],
            ),
        ],
    )
}

fn key_settings_row(player: usize, action: ControlAction) -> impl Bundle {
    (
        Button,
        UserModeUiAction::KeyBinding(KeyBindingCapture { player, action }),
        Node {
            height: Val::Px(USER_MODE_KEY_ROW_HEIGHT),
            min_height: Val::Px(USER_MODE_KEY_ROW_HEIGHT),
            max_height: Val::Px(USER_MODE_KEY_ROW_HEIGHT),
            align_items: AlignItems::Center,
            border: UiRect::all(Val::Px(1.0)),
            padding: UiRect::axes(Val::Px(8.0), Val::Px(0.0)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.055, 0.055, 0.065, 0.94)),
        BorderColor::all(Color::srgb(0.3, 0.29, 0.26)),
        children![(
            UserModeKeySettingsRowText { player, action },
            Text::new(""),
            TextFont {
                font_size: USER_MODE_KEY_ROW_FONT_SIZE,
                ..default()
            },
            TextColor(Color::srgb(0.86, 0.82, 0.72)),
        )],
    )
}

pub fn setup_user_mode_ui(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    character_catalog: Res<CharacterMoveCatalog>,
    mut images: ResMut<Assets<Image>>,
    ui_cameras: Query<Entity, With<UiCamera>>,
) {
    let ui_camera = ui_cameras.iter().next();

    #[cfg(target_arch = "wasm32")]
    let preview_view_format = None;
    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    let preview_view_format = Some(TextureFormat::Rgba8UnormSrgb);

    let preview_image = Image::new_target_texture(
        USER_MODE_PREVIEW_TEXTURE_SIZE,
        USER_MODE_PREVIEW_TEXTURE_SIZE,
        TextureFormat::Rgba8Unorm,
        preview_view_format.clone(),
    );
    let preview_image = images.add(preview_image);
    let arena_preview_image = Image::new_target_texture(
        USER_MODE_ARENA_PREVIEW_TEXTURE_WIDTH,
        USER_MODE_ARENA_PREVIEW_TEXTURE_HEIGHT,
        TextureFormat::Rgba8Unorm,
        preview_view_format,
    );
    let arena_preview_image = images.add(arena_preview_image);
    let preview_origin = USER_MODE_PREVIEW_ORIGIN;
    let preview_scene =
        character_scene_model(&asset_server, &character_catalog, CharacterKind::Cat);

    commands
        .spawn((
            UserModePreviewRoot,
            Transform::from_translation(preview_origin),
            Visibility::Visible,
            user_mode_preview_render_layers(),
        ))
        .with_children(|parent| {
            if let Some(scene) = preview_scene {
                parent
                    .spawn((
                        UserModePreviewScene {
                            character: CharacterKind::Cat,
                        },
                        SceneRoot(scene),
                        Transform::from_xyz(0.0, 0.18, 0.0)
                            .with_scale(Vec3::splat(USER_MODE_PREVIEW_SCALE)),
                        user_mode_preview_render_layers(),
                    ))
                    .observe(apply_user_mode_preview_render_layers);
            }
        });

    commands.spawn((
        PointLight {
            intensity: 2800.0,
            range: 8.0,
            shadows_enabled: false,
            ..default()
        },
        Transform::from_translation(preview_origin + Vec3::new(-1.6, 2.4, 3.0)),
        user_mode_preview_render_layers(),
    ));
    commands.spawn((
        Camera3d::default(),
        Camera {
            order: -4,
            clear_color: Color::srgba(0.0, 0.0, 0.0, 0.0).into(),
            ..default()
        },
        RenderTarget::Image(preview_image.clone().into()),
        Transform::from_translation(preview_origin + Vec3::new(0.0, 1.0, 4.35))
            .looking_at(preview_origin + Vec3::new(0.0, 0.86, 0.0), Vec3::Y),
        UserModePreviewCamera,
        user_mode_preview_render_layers(),
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 18_000.0,
            shadows_enabled: false,
            ..default()
        },
        Transform::from_xyz(-8.0, 16.0, 10.0).looking_at(Vec3::ZERO, Vec3::Y),
        RenderLayers::layer(ARENA_PREVIEW_RENDER_LAYER),
    ));
    commands.spawn((
        PointLight {
            intensity: 900_000.0,
            range: 40.0,
            shadows_enabled: false,
            ..default()
        },
        Transform::from_xyz(0.0, 12.0, 7.0),
        RenderLayers::layer(ARENA_PREVIEW_RENDER_LAYER),
    ));
    commands.spawn((
        Camera3d::default(),
        Camera {
            order: -5,
            is_active: false,
            clear_color: Color::srgb(0.025, 0.03, 0.04).into(),
            ..default()
        },
        RenderTarget::Image(arena_preview_image.clone().into()),
        arena_preview_camera_transform(0),
        UserModeArenaPreviewCamera,
        RenderLayers::layer(ARENA_PREVIEW_RENDER_LAYER),
    ));

    let main_menu_backgrounds = commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
            Pickable::IGNORE,
        ))
        .with_children(|parent| {
            for &(choice, asset_path) in USER_MODE_MAIN_MENU_BACKGROUNDS {
                parent.spawn((
                    UserModeMainMenuBackground {
                        choice,
                        asset_path,
                        fade: MainMenuBackgroundFade::default(),
                        load_failure_reported: false,
                    },
                    Node {
                        display: Display::None,
                        position_type: PositionType::Absolute,
                        left: Val::Px(0.0),
                        top: Val::Px(0.0),
                        width: Val::Percent(100.0),
                        height: Val::Percent(100.0),
                        ..default()
                    },
                    ImageNode::new(asset_server.load(asset_path))
                        .with_color(Color::srgba(1.0, 1.0, 1.0, 0.0))
                        .with_mode(NodeImageMode::Stretch),
                    Pickable::IGNORE,
                ));
            }
        })
        .id();

    let mut user_mode_root = commands.spawn((
        UserModeRoot,
        Node {
            display: Display::None,
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: Val::Px(0.0),
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        },
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.0)),
        Pickable::IGNORE,
        children![
            (
                UserModeStartPanel,
                Node {
                    display: Display::None,
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    flex_direction: FlexDirection::Column,
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    row_gap: Val::Px(18.0),
                    ..default()
                },
                Pickable::IGNORE,
                children![
                    (
                        Text::new("Animal Fighter Club"),
                        TextFont {
                            font_size: 72.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.95, 0.86, 0.68)),
                        TextShadow::default(),
                    ),
                    (
                        Text::new("Click or press Enter to start"),
                        TextFont {
                            font_size: 28.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.82, 0.78, 0.68)),
                    ),
                ],
            ),
            (
                UserModeMainMenuPanel,
                Node {
                    display: Display::None,
                    position_type: PositionType::Absolute,
                    left: Val::Px(0.0),
                    top: Val::Px(0.0),
                    width: Val::Percent(45.0),
                    height: Val::Percent(100.0),
                    flex_direction: FlexDirection::Column,
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    row_gap: Val::Px(14.0),
                    ..default()
                },
                Pickable::IGNORE,
                children![
                    (
                        Text::new("ANIMAL FIGHTER CLUB"),
                        TextFont {
                            font_size: 46.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.95, 0.86, 0.68)),
                        TextShadow::default(),
                    ),
                    user_mode_action_button(
                        "SINGLE PLAYER",
                        UserModeUiAction::MainMenu(UserModeMainMenuChoice::SinglePlayer),
                        Val::Px(340.0),
                        58.0,
                        24.0,
                    ),
                    user_mode_action_button(
                        "MULTIPLAYER",
                        UserModeUiAction::MainMenu(UserModeMainMenuChoice::Multiplayer),
                        Val::Px(340.0),
                        58.0,
                        24.0,
                    ),
                    user_mode_action_button(
                        "TUTORIAL",
                        UserModeUiAction::MainMenu(UserModeMainMenuChoice::Tutorial),
                        Val::Px(340.0),
                        58.0,
                        24.0,
                    ),
                    user_mode_action_button(
                        "SETTINGS",
                        UserModeUiAction::MainMenu(UserModeMainMenuChoice::Settings),
                        Val::Px(340.0),
                        58.0,
                        24.0,
                    ),
                    (
                        Text::new("Up/Down or W/S choose  |  Enter confirm"),
                        TextFont {
                            font_size: 18.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.68, 0.66, 0.62)),
                    ),
                ],
            ),
            (
                UserModePlayerCountPanel,
                Node {
                    display: Display::None,
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    flex_direction: FlexDirection::Column,
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    row_gap: Val::Px(14.0),
                    ..default()
                },
                Pickable::IGNORE,
                children![
                    (
                        Text::new("MULTIPLAYER"),
                        TextFont {
                            font_size: 46.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.95, 0.86, 0.68)),
                        TextShadow::default(),
                    ),
                    user_mode_action_button(
                        "2 PLAYERS",
                        UserModeUiAction::PlayerCount(UserModePlayerCountChoice::TwoPlayers),
                        Val::Px(300.0),
                        58.0,
                        24.0,
                    ),
                    user_mode_action_button(
                        "3 PLAYERS",
                        UserModeUiAction::PlayerCount(UserModePlayerCountChoice::ThreePlayers),
                        Val::Px(300.0),
                        58.0,
                        24.0,
                    ),
                    user_mode_action_button(
                        "4 PLAYERS",
                        UserModeUiAction::PlayerCount(UserModePlayerCountChoice::FourPlayers),
                        Val::Px(300.0),
                        58.0,
                        24.0,
                    ),
                    (
                        Text::new("Up/Down or W/S choose  |  Enter confirm  |  Esc back"),
                        TextFont {
                            font_size: 18.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.68, 0.66, 0.62)),
                    ),
                ],
            ),
            (
                UserModeControlsHubPanel,
                Node {
                    display: Display::None,
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    flex_direction: FlexDirection::Column,
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    row_gap: Val::Px(16.0),
                    ..default()
                },
                Pickable::IGNORE,
                children![
                    (
                        Text::new("CONTROLS"),
                        TextFont {
                            font_size: 46.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.95, 0.86, 0.68)),
                        TextShadow::default(),
                    ),
                    (
                        Text::new("Connect, verify, and tune every local player's controls."),
                        TextFont {
                            font_size: 19.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.72, 0.7, 0.64)),
                    ),
                    user_mode_action_button(
                        "CONTROLLER SETUP",
                        UserModeUiAction::ControlsHub(ControlsHubChoice::ControllerSetup),
                        Val::Px(420.0),
                        68.0,
                        24.0,
                    ),
                    user_mode_action_button(
                        "CONTROLLER TEST",
                        UserModeUiAction::ControlsHub(ControlsHubChoice::ControllerTest),
                        Val::Px(420.0),
                        68.0,
                        24.0,
                    ),
                    user_mode_action_button(
                        "KEYBOARD CONTROLS",
                        UserModeUiAction::ControlsHub(ControlsHubChoice::KeyboardControls),
                        Val::Px(420.0),
                        68.0,
                        24.0,
                    ),
                    (
                        Text::new("Up/Down choose  |  Confirm open  |  Back return"),
                        TextFont {
                            font_size: 18.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.68, 0.66, 0.62)),
                    ),
                ],
            ),
            (
                UserModeDeviceJoinPanel,
                Node {
                    display: Display::None,
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    flex_direction: FlexDirection::Column,
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    row_gap: Val::Px(16.0),
                    padding: UiRect::axes(Val::Px(32.0), Val::Px(24.0)),
                    ..default()
                },
                Pickable::IGNORE,
                children![
                    (
                        UserModeDeviceJoinTitleText,
                        Text::new("CONTROLLER SETUP"),
                        TextFont {
                            font_size: 42.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.95, 0.86, 0.68)),
                        TextShadow::default(),
                    ),
                    (
                        UserModeDeviceJoinText,
                        Text::new("Connect a controller or use one of the keyboard layouts."),
                        TextFont {
                            font_size: 18.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.92, 0.88, 0.78)),
                        TextLayout::new_with_justify(Justify::Center),
                    ),
                    (
                        Node {
                            width: Val::Percent(96.0),
                            max_width: Val::Px(1160.0),
                            flex_direction: FlexDirection::Row,
                            flex_wrap: FlexWrap::Wrap,
                            justify_content: JustifyContent::Center,
                            align_items: AlignItems::Stretch,
                            column_gap: Val::Px(14.0),
                            row_gap: Val::Px(14.0),
                            ..default()
                        },
                        children![
                            controller_setup_seat_card(0),
                            controller_setup_seat_card(1),
                            controller_setup_seat_card(2),
                            controller_setup_seat_card(3),
                        ],
                    ),
                    (
                        Node {
                            flex_direction: FlexDirection::Row,
                            justify_content: JustifyContent::Center,
                            column_gap: Val::Px(12.0),
                            ..default()
                        },
                        children![
                            user_mode_action_button(
                                "CHANGE ORDER",
                                UserModeUiAction::ControllerSetupChangeOrder,
                                Val::Px(220.0),
                                48.0,
                                18.0,
                            ),
                            (
                                Button,
                                UserModeUiAction::ControllerSetupClear,
                                Node {
                                    width: Val::Px(190.0),
                                    height: Val::Px(48.0),
                                    justify_content: JustifyContent::Center,
                                    align_items: AlignItems::Center,
                                    border: UiRect::all(Val::Px(2.0)),
                                    ..default()
                                },
                                BackgroundColor(Color::srgba(0.055, 0.055, 0.065, 0.94)),
                                BorderColor::all(Color::srgb(0.42, 0.4, 0.35)),
                                children![(
                                    UserModeDeviceJoinClearText,
                                    Text::new("CLEAR"),
                                    TextFont {
                                        font_size: 18.0,
                                        ..default()
                                    },
                                    TextColor(Color::srgb(0.95, 0.86, 0.68)),
                                )],
                            ),
                            (
                                Button,
                                UserModeUiAction::ControllerSetupReady,
                                Node {
                                    width: Val::Px(220.0),
                                    height: Val::Px(48.0),
                                    justify_content: JustifyContent::Center,
                                    align_items: AlignItems::Center,
                                    border: UiRect::all(Val::Px(2.0)),
                                    ..default()
                                },
                                BackgroundColor(Color::srgba(0.055, 0.055, 0.065, 0.94)),
                                BorderColor::all(Color::srgb(0.42, 0.4, 0.35)),
                                children![(
                                    UserModeDeviceJoinReadyText,
                                    Text::new("READY"),
                                    TextFont {
                                        font_size: 18.0,
                                        ..default()
                                    },
                                    TextColor(Color::srgb(0.95, 0.86, 0.68)),
                                )],
                            ),
                        ],
                    ),
                ],
            ),
            (
                UserModeCharacterSelectPanel,
                Node {
                    display: Display::None,
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    flex_direction: FlexDirection::Column,
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    row_gap: Val::Px(14.0),
                    ..default()
                },
                Pickable::IGNORE,
                children![
                    (
                        UserModeCharacterTitleText,
                        Text::new("P1 SELECT A CHARACTER"),
                        TextFont {
                            font_size: 42.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.95, 0.86, 0.68)),
                        TextShadow::default(),
                    ),
                    (
                        UserModeCharacterPreview,
                        ImageNode::new(preview_image),
                        Node {
                            width: Val::Px(290.0),
                            height: Val::Px(290.0),
                            ..default()
                        },
                    ),
                    (
                        Node {
                            flex_direction: FlexDirection::Row,
                            align_items: AlignItems::Center,
                            column_gap: Val::Px(18.0),
                            ..default()
                        },
                        children![
                            user_mode_action_button(
                                "<",
                                UserModeUiAction::Previous,
                                Val::Px(64.0),
                                54.0,
                                34.0
                            ),
                            (
                                UserModeChoiceText,
                                Text::new(character_select_message(CharacterKind::Cat)),
                                TextFont {
                                    font_size: USER_MODE_CHOICE_FONT_SIZE,
                                    ..default()
                                },
                                TextColor(Color::srgb(0.96, 0.92, 0.82)),
                                TextShadow::default(),
                                TextLayout::new_with_justify(Justify::Center),
                                Node {
                                    min_width: Val::Px(240.0),
                                    ..default()
                                },
                            ),
                            user_mode_action_button(
                                ">",
                                UserModeUiAction::Next,
                                Val::Px(64.0),
                                54.0,
                                34.0
                            ),
                        ],
                    ),
                    user_mode_action_button(
                        "SELECT",
                        UserModeUiAction::Confirm,
                        Val::Px(240.0),
                        54.0,
                        22.0
                    ),
                    (
                        Text::new("Left/Right or Q/E choose  |  Enter confirm  |  Esc back"),
                        TextFont {
                            font_size: 18.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.68, 0.66, 0.62)),
                    ),
                ],
            ),
            (
                UserModeArenaSelectPanel,
                Node {
                    display: Display::None,
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    flex_direction: FlexDirection::Column,
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    row_gap: Val::Px(8.0),
                    ..default()
                },
                Pickable::IGNORE,
                children![
                    (
                        Text::new("SELECT ARENA"),
                        TextFont {
                            font_size: 36.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.95, 0.86, 0.68)),
                        TextShadow::default(),
                    ),
                    (
                        UserModeArenaPreviewPanel,
                        Node {
                            width: Val::Percent(58.0),
                            max_width: Val::Px(600.0),
                            aspect_ratio: Some(1.5),
                            border: UiRect::all(Val::Px(3.0)),
                            padding: UiRect::all(Val::Px(4.0)),
                            ..default()
                        },
                        BackgroundColor(Color::srgb(0.035, 0.04, 0.05)),
                        BorderColor::all(Color::srgb(0.78, 0.67, 0.4)),
                        children![(
                            ImageNode::new(arena_preview_image),
                            Node {
                                width: Val::Percent(100.0),
                                height: Val::Percent(100.0),
                                ..default()
                            },
                        )],
                    ),
                    (
                        Node {
                            flex_direction: FlexDirection::Row,
                            align_items: AlignItems::Center,
                            column_gap: Val::Px(18.0),
                            ..default()
                        },
                        children![
                            user_mode_action_button(
                                "<",
                                UserModeUiAction::Previous,
                                Val::Px(64.0),
                                54.0,
                                34.0
                            ),
                            (
                                UserModeChoiceText,
                                Text::new(arena_select_message(0)),
                                TextFont {
                                    font_size: USER_MODE_CHOICE_FONT_SIZE,
                                    ..default()
                                },
                                TextColor(Color::srgb(0.96, 0.92, 0.82)),
                                TextShadow::default(),
                                TextLayout::new_with_justify(Justify::Center),
                                Node {
                                    min_width: Val::Px(300.0),
                                    ..default()
                                },
                            ),
                            user_mode_action_button(
                                ">",
                                UserModeUiAction::Next,
                                Val::Px(64.0),
                                54.0,
                                34.0
                            ),
                        ],
                    ),
                    user_mode_action_button(
                        "START MATCH",
                        UserModeUiAction::Confirm,
                        Val::Px(260.0),
                        54.0,
                        22.0
                    ),
                    (
                        Text::new("Left/Right or Q/E choose  |  Enter confirm  |  Esc back"),
                        TextFont {
                            font_size: 18.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.68, 0.66, 0.62)),
                    ),
                ],
            ),
            (
                UserModeKeySettingsPanel,
                Node {
                    display: Display::None,
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    flex_direction: FlexDirection::Column,
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    row_gap: Val::Px(14.0),
                    padding: UiRect::axes(Val::Px(48.0), Val::Px(28.0)),
                    ..default()
                },
                Pickable::IGNORE,
                children![
                    (
                        Text::new("KEYBOARD CONTROLS"),
                        TextFont {
                            font_size: 38.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.95, 0.86, 0.68)),
                        TextShadow::default(),
                    ),
                    (
                        UserModeKeySettingsPromptText,
                        Text::new(key_settings_prompt_message(&UserModeState::default())),
                        TextFont {
                            font_size: 18.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.68, 0.66, 0.62)),
                    ),
                    (
                        Node {
                            width: Val::Percent(96.0),
                            max_width: Val::Px(1120.0),
                            flex_direction: FlexDirection::Row,
                            justify_content: JustifyContent::Center,
                            align_items: AlignItems::FlexStart,
                            column_gap: Val::Px(10.0),
                            ..default()
                        },
                        children![
                            key_settings_column(0),
                            key_settings_column(1),
                            key_settings_column(2),
                            key_settings_column(3)
                        ],
                    ),
                    user_mode_action_button(
                        "RESTORE DEFAULTS",
                        UserModeUiAction::ResetKeys,
                        Val::Px(240.0),
                        44.0,
                        17.0,
                    ),
                    (
                        UserModeKeyResetPanel,
                        Node {
                            display: Display::None,
                            position_type: PositionType::Absolute,
                            left: Val::Percent(25.0),
                            top: Val::Percent(31.0),
                            width: Val::Percent(50.0),
                            min_height: Val::Px(230.0),
                            flex_direction: FlexDirection::Column,
                            justify_content: JustifyContent::Center,
                            align_items: AlignItems::Center,
                            row_gap: Val::Px(18.0),
                            border: UiRect::all(Val::Px(3.0)),
                            padding: UiRect::all(Val::Px(24.0)),
                            ..default()
                        },
                        GlobalZIndex(20),
                        BackgroundColor(Color::srgba(0.025, 0.025, 0.035, 0.98)),
                        BorderColor::all(Color::srgb(0.95, 0.62, 0.28)),
                        children![
                            (
                                Text::new("RESTORE ALL KEYBOARD CONTROLS?"),
                                TextFont {
                                    font_size: 25.0,
                                    ..default()
                                },
                                TextColor(Color::srgb(1.0, 0.82, 0.58)),
                                TextShadow::default(),
                            ),
                            (
                                Text::new("This replaces every P1-P4 key and cannot be undone."),
                                TextFont {
                                    font_size: 18.0,
                                    ..default()
                                },
                                TextColor(Color::srgb(0.86, 0.82, 0.74)),
                            ),
                            (
                                Node {
                                    flex_direction: FlexDirection::Row,
                                    column_gap: Val::Px(14.0),
                                    ..default()
                                },
                                children![
                                    user_mode_action_button(
                                        "RESTORE",
                                        UserModeUiAction::ConfirmKeyReset,
                                        Val::Px(180.0),
                                        48.0,
                                        18.0,
                                    ),
                                    user_mode_action_button(
                                        "CANCEL",
                                        UserModeUiAction::CancelKeyReset,
                                        Val::Px(180.0),
                                        48.0,
                                        18.0,
                                    ),
                                ],
                            ),
                        ],
                    ),
                ],
            ),
            (
                UserModeControllerTestPanel,
                Node {
                    display: Display::None,
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    flex_direction: FlexDirection::Column,
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    row_gap: Val::Px(18.0),
                    padding: UiRect::axes(Val::Px(48.0), Val::Px(28.0)),
                    ..default()
                },
                Pickable::IGNORE,
                children![
                    (
                        Text::new("CONTROLLER TEST"),
                        TextFont {
                            font_size: 42.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.95, 0.86, 0.68)),
                        TextShadow::default(),
                    ),
                    (
                        UserModeControllerTestText,
                        Text::new("Connect a controller to inspect it."),
                        TextFont {
                            font_size: 19.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.9, 0.86, 0.76)),
                        TextLayout::new_with_justify(Justify::Center),
                    ),
                    (
                        Node {
                            flex_direction: FlexDirection::Row,
                            column_gap: Val::Px(12.0),
                            ..default()
                        },
                        children![
                            (
                                Button,
                                UserModeUiAction::ToggleVibration,
                                Node {
                                    width: Val::Px(230.0),
                                    height: Val::Px(48.0),
                                    justify_content: JustifyContent::Center,
                                    align_items: AlignItems::Center,
                                    border: UiRect::all(Val::Px(2.0)),
                                    ..default()
                                },
                                BackgroundColor(Color::srgba(0.055, 0.055, 0.065, 0.94)),
                                BorderColor::all(Color::srgb(0.42, 0.4, 0.35)),
                                children![(
                                    UserModeVibrationButtonText,
                                    Text::new("VIBRATION: STANDARD"),
                                    TextFont {
                                        font_size: 18.0,
                                        ..default()
                                    },
                                    TextColor(Color::srgb(0.95, 0.86, 0.68)),
                                )],
                            ),
                            user_mode_action_button(
                                "TEST VIBRATION",
                                UserModeUiAction::TestVibration,
                                Val::Px(230.0),
                                48.0,
                                18.0,
                            ),
                        ],
                    ),
                    (
                        Node {
                            flex_direction: FlexDirection::Row,
                            column_gap: Val::Px(12.0),
                            ..default()
                        },
                        children![
                            (
                                Button,
                                UserModeUiAction::ToggleHapticStyle,
                                Node {
                                    width: Val::Px(230.0),
                                    height: Val::Px(48.0),
                                    justify_content: JustifyContent::Center,
                                    align_items: AlignItems::Center,
                                    border: UiRect::all(Val::Px(2.0)),
                                    ..default()
                                },
                                BackgroundColor(Color::srgba(0.055, 0.055, 0.065, 0.94)),
                                BorderColor::all(Color::srgb(0.42, 0.4, 0.35)),
                                children![(
                                    UserModeHapticStyleButtonText,
                                    Text::new("STYLE: COMPETITIVE"),
                                    TextFont {
                                        font_size: 18.0,
                                        ..default()
                                    },
                                    TextColor(Color::srgb(0.95, 0.86, 0.68)),
                                )],
                            ),
                            user_mode_action_button(
                                "TEST COMBAT FEEL",
                                UserModeUiAction::TestCombatHaptics,
                                Val::Px(230.0),
                                48.0,
                                18.0,
                            ),
                        ],
                    ),
                    (
                        Text::new(
                            "Left/Right choose device  |  Confirm inspect  |  Back return\nHardware test checks both motors; combat preview demonstrates release → block → heavy → ultimate",
                        ),
                        TextFont {
                            font_size: 18.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.68, 0.66, 0.62)),
                    ),
                ],
            ),
            (
                UserModeControlsPanel,
                Node {
                    display: Display::None,
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    flex_direction: FlexDirection::Column,
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    row_gap: Val::Px(22.0),
                    padding: UiRect::axes(Val::Px(52.0), Val::Px(32.0)),
                    ..default()
                },
                Pickable::IGNORE,
                children![
                    (
                        Text::new("CONTROLS BRIEFING"),
                        TextFont {
                            font_size: 42.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.95, 0.86, 0.68)),
                        TextShadow::default(),
                    ),
                    (
                        UserModeControlsText,
                        Text::new(controls_briefing_message(
                            &UserModeState::default(),
                            &PlayerKeyBindings::default(),
                            true,
                        )),
                        TextFont {
                            font_size: 18.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.92, 0.88, 0.78)),
                        TextLayout::new_with_justify(Justify::Center),
                    ),
                    user_mode_action_button(
                        "FIGHT",
                        UserModeUiAction::Confirm,
                        Val::Px(240.0),
                        54.0,
                        22.0,
                    ),
                ],
            ),
            (
                UserModeResultPanel,
                Node {
                    display: Display::None,
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    flex_direction: FlexDirection::Column,
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    row_gap: Val::Px(24.0),
                    ..default()
                },
                Pickable::IGNORE,
                children![
                    (
                        UserModeResultText,
                        Text::new("RESULT"),
                        TextFont {
                            font_size: 58.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.95, 0.88, 0.72)),
                        TextShadow::default(),
                    ),
                    (
                        Node {
                            flex_direction: FlexDirection::Row,
                            column_gap: Val::Px(18.0),
                            ..default()
                        },
                        children![
                            user_mode_action_button(
                                "PLAY AGAIN",
                                UserModeUiAction::Result(UserModeResultChoice::PlayAgain),
                                Val::Px(240.0),
                                58.0,
                                23.0,
                            ),
                            user_mode_action_button(
                                "CHOOSE CHARACTER",
                                UserModeUiAction::Result(UserModeResultChoice::ChooseCharacter),
                                Val::Px(280.0),
                                58.0,
                                23.0,
                            ),
                        ],
                    ),
                    (
                        Text::new("Left/Right choose  |  Enter confirm"),
                        TextFont {
                            font_size: 20.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.68, 0.66, 0.62)),
                    ),
                ],
            ),
            user_mode_back_button(),
        ],
    ));
    user_mode_root.insert_children(0, &[main_menu_backgrounds]);
    if let Some(ui_camera) = ui_camera {
        user_mode_root.insert(UiTargetCamera(ui_camera));
    }

    let mut reconnect_overlay = commands.spawn((
        ControllerReconnectOverlay,
        Node {
            display: Display::None,
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            top: Val::Px(0.0),
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            row_gap: Val::Px(18.0),
            ..default()
        },
        GlobalZIndex(1000),
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.78)),
        Pickable::IGNORE,
        children![
            (
                Text::new("CONTROLLER DISCONNECTED"),
                TextFont {
                    font_size: 48.0,
                    ..default()
                },
                TextColor(Color::srgb(1.0, 0.78, 0.42)),
                TextShadow::default(),
            ),
            (
                ControllerReconnectText,
                Text::new("Press Confirm on the original or another unassigned controller"),
                TextFont {
                    font_size: 24.0,
                    ..default()
                },
                TextColor(Color::srgb(0.95, 0.92, 0.84)),
                TextLayout::new_with_justify(Justify::Center),
            ),
        ],
    ));
    if let Some(ui_camera) = ui_camera {
        reconnect_overlay.insert(UiTargetCamera(ui_camera));
    }
}

fn handle_device_join_input(
    user_mode: &mut UserModeState,
    keys: &ButtonInput<KeyCode>,
    bindings: &PlayerKeyBindings,
    gamepads: &Query<(Entity, &Gamepad)>,
    metadata: &Query<&ControllerDeviceInfo>,
    preferences: &ControlPreferences,
    rumble_requests: &mut MessageWriter<ControllerHapticRequest>,
) -> Option<String> {
    let mut messages = Vec::new();
    let mut departed = Vec::new();
    for (entity, gamepad) in gamepads {
        let assignment = LocalInputAssignment::Gamepad(entity);
        let family = controller_info(entity, metadata)
            .map(|info| info.family)
            .unwrap_or_default();
        if user_mode.assignment_is_joined(assignment) && gamepad_leave_requested(gamepad, family) {
            if let Some(seat) = user_mode.leave_assignment(assignment) {
                departed.push(entity);
                messages.push(format!("P{} controller left", seat + 1));
            }
        }
    }

    for (entity, gamepad) in gamepads {
        let assignment = LocalInputAssignment::Gamepad(entity);
        let family = controller_info(entity, metadata)
            .map(|info| info.family)
            .unwrap_or_default();
        if !departed.contains(&entity)
            && !user_mode.assignment_is_joined(assignment)
            && gamepad_join_requested(gamepad, family)
        {
            let seat = first_missing_controller_seat(user_mode, gamepads)
                .or_else(|| user_mode.join_assignment(assignment));
            if let Some(seat) = seat {
                user_mode.input_assignments[seat] = assignment;
                user_mode.controller_setup_input_latched = true;
                let _ = request_controller_rumble(rumble_requests, preferences, entity, 0.28, 0.12);
                messages.push(format!(
                    "P{} joined with {}",
                    seat + 1,
                    controller_info(entity, metadata)
                        .map(|info| info.family.display_name())
                        .unwrap_or("controller")
                ));
            }
        }
    }

    let mut departed_keyboards = Vec::new();
    for player in 0..FIGHTER_COUNT {
        let Some(player_bindings) = bindings.bindings_for_player(player) else {
            continue;
        };
        let assignment = LocalInputAssignment::Keyboard(player);
        if user_mode.assignment_is_joined(assignment) && keys.just_pressed(player_bindings.aim_grab)
        {
            if let Some(seat) = user_mode.leave_assignment(assignment) {
                departed_keyboards.push(player);
                messages.push(format!("P{} Keyboard {} left", seat + 1, player + 1));
            }
        }
    }
    for player in 0..FIGHTER_COUNT {
        let Some(player_bindings) = bindings.bindings_for_player(player) else {
            continue;
        };
        if !departed_keyboards.contains(&player)
            && keyboard_join_requested(keys, player_bindings)
            && !user_mode.assignment_is_joined(LocalInputAssignment::Keyboard(player))
        {
            if let Some(seat) = user_mode.join_assignment(LocalInputAssignment::Keyboard(player)) {
                user_mode.controller_setup_input_latched = true;
                messages.push(format!("P{} joined with Keyboard {}", seat + 1, player + 1));
            }
        }
    }

    (!messages.is_empty()).then(|| messages.join("  |  "))
}

fn first_missing_controller_seat(
    user_mode: &UserModeState,
    gamepads: &Query<(Entity, &Gamepad)>,
) -> Option<usize> {
    user_mode
        .input_assignments
        .iter()
        .take(user_mode.controller_setup_target())
        .position(|assignment| {
            matches!(
                assignment,
                LocalInputAssignment::Gamepad(entity) if gamepads.get(*entity).is_err()
            )
        })
}

fn gamepad_join_requested(gamepad: &Gamepad, family: ControllerFamily) -> bool {
    gamepad.just_pressed(family.confirm_button())
}

fn gamepad_leave_requested(gamepad: &Gamepad, family: ControllerFamily) -> bool {
    gamepad.just_pressed(family.back_button())
}

fn keyboard_join_requested(keys: &ButtonInput<KeyCode>, bindings: PlayerControlBindings) -> bool {
    keys.just_pressed(bindings.jump) || keys.just_pressed(bindings.aim_grab)
}

fn controller_setup_confirm_input_pressed(
    keys: &ButtonInput<KeyCode>,
    bindings: &PlayerKeyBindings,
    gamepads: &Query<(Entity, &Gamepad)>,
    metadata: &Query<&ControllerDeviceInfo>,
) -> bool {
    gamepads.iter().any(|(entity, gamepad)| {
        let family = controller_info(entity, metadata)
            .map(|info| info.family)
            .unwrap_or_default();
        gamepad.pressed(family.confirm_button())
    }) || (0..FIGHTER_COUNT).any(|player| {
        bindings
            .bindings_for_player(player)
            .map(|binding| keys.pressed(binding.jump) || keys.pressed(binding.aim_grab))
            .unwrap_or(false)
    })
}

fn controller_setup_assignment_connected(
    assignment: LocalInputAssignment,
    gamepads: &Query<(Entity, &Gamepad)>,
) -> bool {
    match assignment {
        LocalInputAssignment::Keyboard(_) => true,
        LocalInputAssignment::Gamepad(entity) => gamepads.get(entity).is_ok(),
        LocalInputAssignment::Unassigned => false,
    }
}

fn controller_setup_can_finish(
    user_mode: &UserModeState,
    gamepads: &Query<(Entity, &Gamepad)>,
) -> bool {
    let assignments = user_mode
        .input_assignments
        .iter()
        .take(user_mode.controller_setup_target());
    match user_mode.controller_setup_context {
        ControllerSetupContext::Match | ControllerSetupContext::Tutorial => assignments
            .clone()
            .all(|assignment| controller_setup_assignment_connected(*assignment, gamepads)),
        ControllerSetupContext::Settings => assignments
            .filter(|assignment| **assignment != LocalInputAssignment::Unassigned)
            .all(|assignment| controller_setup_assignment_connected(*assignment, gamepads)),
    }
}

fn connected_controller_entities(gamepads: &Query<(Entity, &Gamepad)>) -> Vec<Entity> {
    let mut entities = gamepads
        .iter()
        .map(|(entity, _)| entity)
        .collect::<Vec<_>>();
    entities.sort_by_key(|entity| entity.to_bits());
    entities
}

fn selected_controller_test_entity(
    user_mode: &UserModeState,
    gamepads: &Query<(Entity, &Gamepad)>,
) -> Option<Entity> {
    if let Some(active) = user_mode.controller_test_active
        && gamepads.get(active).is_ok()
    {
        return Some(active);
    }
    let entities = connected_controller_entities(gamepads);
    (!entities.is_empty()).then(|| entities[user_mode.controller_test_cursor % entities.len()])
}

fn move_controller_test_cursor(
    user_mode: &mut UserModeState,
    gamepads: &Query<(Entity, &Gamepad)>,
    direction: isize,
) {
    let count = gamepads.iter().count();
    if count == 0 {
        user_mode.controller_test_cursor = 0;
        return;
    }
    user_mode.controller_test_cursor =
        (user_mode.controller_test_cursor as isize + direction).rem_euclid(count as isize) as usize;
}

fn controller_test_message(
    user_mode: &UserModeState,
    gamepads: &Query<(Entity, &Gamepad)>,
    metadata: &Query<&ControllerDeviceInfo>,
) -> String {
    let entities = connected_controller_entities(gamepads);
    let Some(entity) = selected_controller_test_entity(user_mode, gamepads) else {
        return "NO CONTROLLERS DETECTED\n\nPair a controller in the operating system, then press its Confirm button.\nKeyboard and mouse remain available."
            .to_string();
    };
    let Ok((_, gamepad)) = gamepads.get(entity) else {
        return "The selected controller disconnected.\nChoose another connected controller."
            .to_string();
    };
    let info = controller_info(entity, metadata);
    let family = info.map(|info| info.family).unwrap_or_default();
    let name = info
        .map(|info| info.display_name.as_str())
        .unwrap_or("Gamepad");
    let assignment = user_mode
        .input_assignments
        .iter()
        .position(|assignment| *assignment == LocalInputAssignment::Gamepad(entity))
        .map(|seat| format!("Assigned to P{}", seat + 1))
        .unwrap_or_else(|| "Unassigned".to_string());
    let index = entities
        .iter()
        .position(|candidate| *candidate == entity)
        .unwrap_or(0);
    let haptic_connection_hint =
        if cfg!(target_os = "macos") && family == crate::control_settings::ControllerFamily::Xbox {
            "\nmacOS Xbox rumble: pair through Bluetooth; a USB cable may provide input only."
        } else {
            ""
        };

    if user_mode.controller_test_active.is_none() {
        return format!(
            "{} / {}  —  {} {}\n{}\n{}  |  HAPTICS {}{}\n\nPress {} to inspect every input.",
            index + 1,
            entities.len(),
            family.display_name(),
            name,
            assignment,
            if info.map(|info| info.connected).unwrap_or(true) {
                "CONNECTED"
            } else {
                "DISCONNECTED"
            },
            info.map(|info| info.haptics.label())
                .unwrap_or("SYSTEM DEPENDENT"),
            haptic_connection_hint,
            family.confirm_label(),
        );
    }

    let pressed = GamepadButton::all()
        .into_iter()
        .filter(|button| gamepad.pressed(*button))
        .map(|button| family.face_button_label(button))
        .collect::<Vec<_>>();
    let pressed = if pressed.is_empty() {
        "None".to_string()
    } else {
        pressed.join("  ")
    };
    let left = gamepad.left_stick();
    let right = gamepad.right_stick();
    let left_trigger = gamepad.get(GamepadButton::LeftTrigger2).unwrap_or(0.0);
    let right_trigger = gamepad.get(GamepadButton::RightTrigger2).unwrap_or(0.0);
    let movement_state = if left.length() >= 0.20 {
        "ACTIVE"
    } else {
        "inside deadzone"
    };
    format!(
        "{} — {}\n{}  |  HAPTICS {}{}\n\nPressed: {pressed}\nLeft stick  X {:+.2}  Y {:+.2}  — {movement_state}\nRight stick X {:+.2}  Y {:+.2}\n{} {:.2}   {} {:.2}\n\nGameplay face layout: {} Jump  |  {} Aim/Grab  |  {} Light  |  {} Heavy\nMovement deadzone: 0.20  |  D-Pad Left/Right: vibration  |  Up/Down: style\nStart: motor test  |  {}: combat preview  |  Hold {} for 0.75 seconds to finish.",
        family.display_name(),
        name,
        assignment,
        info.map(|info| info.haptics.label())
            .unwrap_or("SYSTEM DEPENDENT"),
        haptic_connection_hint,
        left.x,
        left.y,
        right.x,
        right.y,
        family.face_button_label(GamepadButton::LeftTrigger2),
        left_trigger,
        family.face_button_label(GamepadButton::RightTrigger2),
        right_trigger,
        family.face_button_label(GamepadButton::South),
        family.face_button_label(GamepadButton::East),
        family.face_button_label(GamepadButton::West),
        family.face_button_label(GamepadButton::North),
        family.face_button_label(GamepadButton::West),
        family.back_label(),
    )
}

fn handle_character_device_actions(
    user_mode: &mut UserModeState,
    keys: &ButtonInput<KeyCode>,
    bindings: &PlayerKeyBindings,
    gamepads: &Query<(Entity, &Gamepad)>,
    metadata: &Query<&ControllerDeviceInfo>,
    dt: f32,
    trackers: &mut MenuNavigationTrackers,
) -> Option<UserModeRoute> {
    let mut handled = false;
    for player in 0..user_mode.play_mode.human_player_count() {
        let assignment = user_mode.input_assignments[player];
        let Some(action) = assignment_user_mode_action(
            assignment,
            UserModeScreen::CharacterSelect,
            keys,
            bindings,
            gamepads,
            metadata,
            dt,
            &mut trackers.seats[player],
        ) else {
            continue;
        };
        handled = true;
        user_mode.character_select_player = player;

        if user_mode.character_ready[player] {
            if action == UserModeUiAction::Back {
                user_mode.character_ready[player] = false;
            }
            continue;
        }
        if action == UserModeUiAction::Back && player != 0 {
            continue;
        }

        let route = route_user_mode_action(user_mode, action);
        if route == UserModeRoute::ArenaEntered || route == UserModeRoute::ControlsBack {
            return Some(route);
        }
        if user_mode.screen != UserModeScreen::CharacterSelect {
            return Some(route);
        }
    }
    handled.then_some(UserModeRoute::None)
}

fn unassigned_gamepad_user_mode_action(
    screen: UserModeScreen,
    gamepads: &Query<(Entity, &Gamepad)>,
    metadata: &Query<&ControllerDeviceInfo>,
    dt: f32,
    tracker: &mut MenuDirectionRepeat,
) -> Option<UserModeUiAction> {
    gamepads.iter().find_map(|(entity, gamepad)| {
        let family = controller_info(entity, metadata)
            .map(|info| info.family)
            .unwrap_or_default();
        gamepad_user_mode_action(screen, gamepad, family, dt, tracker)
    })
}

#[derive(SystemParam)]
pub struct UserModeInputDevices<'w, 's> {
    keys: Res<'w, ButtonInput<KeyCode>>,
    buttons: Res<'w, ButtonInput<MouseButton>>,
    gamepads: Query<'w, 's, (Entity, &'static Gamepad)>,
    controller_metadata: Query<'w, 's, &'static ControllerDeviceInfo>,
    real_time: Res<'w, Time<Real>>,
    action_buttons:
        Query<'w, 's, (&'static Interaction, &'static UserModeUiAction), Changed<Interaction>>,
}

#[derive(SystemParam)]
pub struct UserModeInputContext<'w, 's> {
    asset_server: Res<'w, AssetServer>,
    user_mode: ResMut<'w, UserModeState>,
    key_bindings: ResMut<'w, PlayerKeyBindings>,
    control_preferences: ResMut<'w, ControlPreferences>,
    rumble_requests: MessageWriter<'w, ControllerHapticRequest>,
    setup: ResMut<'w, LocalSetup>,
    state: ResMut<'w, MatchState>,
    gameplay_scene: Res<'w, UserModeGameplayScene>,
    announcements: ResMut<'w, MatchAnnouncements>,
    music: Query<'w, 's, Entity, With<UserModeMusic>>,
    virtual_time: ResMut<'w, Time<Virtual>>,
    screen_look: ResMut<'w, ScreenLook>,
    screen_transition: ResMut<'w, ScreenLookTransition>,
    pause_owners: ResMut<'w, GameplayPauseOwners>,
    tutorial_transition: ResMut<'w, TutorialTransition>,
}

pub fn sync_main_menu_pointer_hover(
    mut user_mode: ResMut<UserModeState>,
    action_buttons: Query<(&Interaction, &UserModeUiAction), Changed<Interaction>>,
) {
    if user_mode.screen != UserModeScreen::ModeSelect {
        return;
    }

    if let Some(choice) = action_buttons.iter().find_map(|(interaction, action)| {
        if *interaction != Interaction::Hovered {
            return None;
        }
        match action {
            UserModeUiAction::MainMenu(choice) => Some(*choice),
            _ => None,
        }
    }) {
        user_mode.main_menu_choice = choice;
    }
}

pub fn handle_user_mode_input(
    devices: UserModeInputDevices,
    context: UserModeInputContext,
    mut menu_navigation: Local<MenuNavigationTrackers>,
    mut commands: Commands,
) {
    let UserModeInputDevices {
        keys,
        buttons,
        gamepads,
        controller_metadata,
        real_time,
        action_buttons,
    } = devices;
    let UserModeInputContext {
        asset_server,
        mut user_mode,
        mut key_bindings,
        mut control_preferences,
        mut rumble_requests,
        mut setup,
        mut state,
        gameplay_scene,
        mut announcements,
        music,
        mut virtual_time,
        mut screen_look,
        mut screen_transition,
        mut pause_owners,
        mut tutorial_transition,
    } = context;

    if tutorial_transition.active() {
        return;
    }

    menu_navigation.reset_for_screen(user_mode.screen);
    if user_mode.screen == UserModeScreen::Start {
        if keys.just_pressed(KeyCode::Enter)
            || keys.just_pressed(KeyCode::Space)
            || buttons.just_pressed(MouseButton::Left)
            || gamepads.iter().any(|(entity, gamepad)| {
                let family = controller_info(entity, &controller_metadata)
                    .map(|info| info.family)
                    .unwrap_or_default();
                gamepad.just_pressed(family.confirm_button())
            })
        {
            user_mode.enter_fresh_mode_select();
            start_user_mode_menu_music(&mut commands, &asset_server);
        }
        return;
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    {
        if !user_mode.blocks_dev_input() && user_mode_pressed(&keys) {
            stop_user_mode_music(&mut commands, &music);
            reset_user_mode_presentation(
                &mut virtual_time,
                &mut screen_look,
                &mut screen_transition,
            );
            start_user_mode_menu_music(&mut commands, &asset_server);
            user_mode.enter_fresh_mode_select();
            if state.phase != MatchPhase::Setup {
                state.return_to_setup();
            }
            announcements.show("", 0.0);
            return;
        }
    }

    if !user_mode.active() {
        return;
    }

    if matches!(
        user_mode.screen,
        UserModeScreen::TutorialHub
            | UserModeScreen::TutorialLesson
            | UserModeScreen::TutorialPause
            | UserModeScreen::TutorialFinalResult
    ) {
        return;
    }

    let pointer_action = action_buttons.iter().find_map(|(interaction, action)| {
        (*interaction == Interaction::Pressed).then_some(*action)
    });

    if user_mode.screen == UserModeScreen::DeviceJoin {
        let p1_requested_exit = match user_mode.input_assignments[0] {
            LocalInputAssignment::Gamepad(entity) => gamepads
                .get(entity)
                .map(|(_, gamepad)| gamepad.just_pressed(GamepadButton::Start))
                .unwrap_or(false),
            _ => false,
        };
        if keys.just_pressed(KeyCode::Escape)
            || p1_requested_exit
            || pointer_action == Some(UserModeUiAction::Back)
        {
            if user_mode.controller_setup_context == ControllerSetupContext::Tutorial {
                request_tutorial_transition(
                    &mut tutorial_transition,
                    &mut pause_owners,
                    TutorialTransitionAction::LeaveTutorial,
                );
            } else {
                route_user_mode_action(&mut user_mode, UserModeUiAction::Back);
            }
            return;
        }
        if let Some(message) = handle_device_join_input(
            &mut user_mode,
            &keys,
            &key_bindings,
            &gamepads,
            &controller_metadata,
            &control_preferences,
            &mut rumble_requests,
        ) {
            announcements.show(message, 0.9);
        }

        if user_mode.controller_setup_input_latched
            && !controller_setup_confirm_input_pressed(
                &keys,
                &key_bindings,
                &gamepads,
                &controller_metadata,
            )
        {
            user_mode.controller_setup_input_latched = false;
        }

        let owner_action = if user_mode.input_assignments[0] == LocalInputAssignment::Unassigned {
            unassigned_gamepad_user_mode_action(
                UserModeScreen::DeviceJoin,
                &gamepads,
                &controller_metadata,
                real_time.delta_secs(),
                &mut menu_navigation.unassigned,
            )
            .or_else(|| keyboard_user_mode_action(&user_mode, &keys))
        } else {
            assignment_user_mode_action(
                user_mode.input_assignments[0],
                UserModeScreen::DeviceJoin,
                &keys,
                &key_bindings,
                &gamepads,
                &controller_metadata,
                real_time.delta_secs(),
                &mut menu_navigation.seats[0],
            )
            .or_else(|| (keys.just_pressed(KeyCode::Enter)).then_some(UserModeUiAction::Confirm))
        };
        let mut action = pointer_action.or(owner_action);
        if action == Some(UserModeUiAction::Previous) {
            user_mode.move_controller_setup_action(-1);
            return;
        }
        if action == Some(UserModeUiAction::Next) {
            user_mode.move_controller_setup_action(1);
            return;
        }
        if action == Some(UserModeUiAction::Confirm) {
            action = Some(user_mode.selected_controller_setup_action());
        }
        let Some(action) = action else {
            return;
        };
        match action {
            UserModeUiAction::ControllerSetupRemoveSeat(seat) => {
                if user_mode.remove_assignment_at(seat).is_some() {
                    user_mode.controller_setup_snapshot = user_mode.input_assignments;
                    announcements.show(format!("P{} assignment removed", seat + 1), 0.8);
                }
            }
            UserModeUiAction::ControllerSetupChangeOrder => {
                user_mode.begin_controller_reorder();
                announcements.show("Confirm devices in the new P1-P4 order", 1.1);
            }
            UserModeUiAction::ControllerSetupClear => {
                if user_mode.arm_or_confirm_clear_assignments() {
                    announcements.show("All assignments cleared", 0.9);
                } else {
                    announcements.show("Choose CLEAR again to confirm", 1.0);
                }
            }
            UserModeUiAction::ControllerSetupReady => {
                if user_mode.controller_setup_input_latched {
                    announcements.show("Release Confirm, then choose Ready", 0.8);
                } else if !controller_setup_can_finish(&user_mode, &gamepads) {
                    announcements.show("Reconnect or remove every required controller", 1.1);
                } else if user_mode.controller_setup_phase == ControllerSetupPhase::Reorder {
                    user_mode.finish_controller_reorder();
                    announcements.show("Player order saved", 0.9);
                } else {
                    if let LocalInputAssignment::Gamepad(entity) = user_mode.input_assignments[0] {
                        let _ = request_controller_rumble(
                            &mut rumble_requests,
                            &control_preferences,
                            entity,
                            0.35,
                            0.16,
                        );
                    }
                    match user_mode.controller_setup_context {
                        ControllerSetupContext::Settings => {
                            user_mode.enter_controls_hub();
                            announcements.show("Controller setup saved for this session", 1.0);
                        }
                        ControllerSetupContext::Tutorial => {
                            request_tutorial_transition(
                                &mut tutorial_transition,
                                &mut pause_owners,
                                TutorialTransitionAction::EnterHub,
                            );
                        }
                        ControllerSetupContext::Match => {
                            user_mode.enter_character_select();
                            announcements.show("Choose your characters", 0.9);
                        }
                    }
                }
            }
            _ => {}
        }
        return;
    }

    #[cfg(target_arch = "wasm32")]
    if matches!(
        user_mode.screen,
        UserModeScreen::ModeSelect
            | UserModeScreen::PlayerCountSelect
            | UserModeScreen::CharacterSelect
            | UserModeScreen::ArenaSelect
    ) {
        if let Some(config) = take_web_match_config() {
            user_mode.play_mode = config.play_mode;
            user_mode.player_characters = config.player_characters;
            user_mode.arena_index = config.arena_index;
            if let Some(bindings) = config.bindings {
                *key_bindings = bindings;
            }
            stop_user_mode_music(&mut commands, &music);
            let flow = prepare_user_mode_match(&mut user_mode, &mut setup, &mut state);
            announce_user_mode_match_flow(flow, &setup, &mut announcements);
            return;
        }
    }

    if user_mode.screen == UserModeScreen::ControllerTest {
        if user_mode
            .controller_test_active
            .is_some_and(|entity| gamepads.get(entity).is_err())
        {
            user_mode.controller_test_active = None;
            user_mode.controller_test_back_hold = 0.0;
            announcements.show("Test controller disconnected", 0.9);
        }

        let toggle_vibration = pointer_action == Some(UserModeUiAction::ToggleVibration);
        let toggle_haptic_style = pointer_action == Some(UserModeUiAction::ToggleHapticStyle);
        let pointer_test = pointer_action == Some(UserModeUiAction::TestVibration);
        let pointer_combat_test = pointer_action == Some(UserModeUiAction::TestCombatHaptics);
        if toggle_vibration {
            control_preferences.vibration = control_preferences.vibration.next();
            if !control_preferences.vibration.enabled() {
                for (gamepad, _) in &gamepads {
                    rumble_requests.write(ControllerHapticRequest::stop(gamepad));
                }
            }
            match save_control_preferences(&key_bindings, &control_preferences) {
                Ok(()) => announcements.show(
                    format!(
                        "Controller vibration: {}",
                        control_preferences.vibration.label()
                    ),
                    0.9,
                ),
                Err(error) => {
                    warn!("Could not save control preferences: {error}");
                    announcements.show("Vibration changed for this session; save failed", 1.2);
                }
            }
            return;
        }
        if toggle_haptic_style {
            control_preferences.haptic_style = control_preferences.haptic_style.next();
            for (gamepad, _) in &gamepads {
                rumble_requests.write(ControllerHapticRequest::stop(gamepad));
            }
            match save_control_preferences(&key_bindings, &control_preferences) {
                Ok(()) => announcements.show(
                    format!("Haptic style: {}", control_preferences.haptic_style.label()),
                    0.9,
                ),
                Err(error) => {
                    warn!("Could not save control preferences: {error}");
                    announcements.show("Haptic style changed for this session; save failed", 1.2);
                }
            }
            return;
        }

        if let Some(active) = user_mode.controller_test_active {
            if keys.just_pressed(KeyCode::Escape) || pointer_action == Some(UserModeUiAction::Back)
            {
                user_mode.controller_test_active = None;
                user_mode.controller_test_back_hold = 0.0;
                return;
            }
            let Ok((_, gamepad)) = gamepads.get(active) else {
                return;
            };
            let family = controller_info(active, &controller_metadata)
                .map(|info| info.family)
                .unwrap_or_default();
            let vibration_direction = if gamepad.just_pressed(GamepadButton::DPadLeft) {
                -1
            } else if gamepad.just_pressed(GamepadButton::DPadRight) {
                1
            } else {
                0
            };
            if vibration_direction != 0 {
                control_preferences.vibration = if vibration_direction < 0 {
                    control_preferences.vibration.previous()
                } else {
                    control_preferences.vibration.next()
                };
                if !control_preferences.vibration.enabled() {
                    rumble_requests.write(ControllerHapticRequest::stop(active));
                }
                match save_control_preferences(&key_bindings, &control_preferences) {
                    Ok(()) => announcements.show(
                        format!("Vibration: {}", control_preferences.vibration.label()),
                        0.75,
                    ),
                    Err(error) => {
                        warn!("Could not save control preferences: {error}");
                        announcements.show("Vibration changed; save failed", 1.0);
                    }
                }
                return;
            }
            let style_direction = if gamepad.just_pressed(GamepadButton::DPadUp) {
                -1
            } else if gamepad.just_pressed(GamepadButton::DPadDown) {
                1
            } else {
                0
            };
            if style_direction != 0 {
                control_preferences.haptic_style = if style_direction < 0 {
                    control_preferences.haptic_style.previous()
                } else {
                    control_preferences.haptic_style.next()
                };
                rumble_requests.write(ControllerHapticRequest::stop(active));
                match save_control_preferences(&key_bindings, &control_preferences) {
                    Ok(()) => announcements.show(
                        format!("Haptic style: {}", control_preferences.haptic_style.label()),
                        0.75,
                    ),
                    Err(error) => {
                        warn!("Could not save control preferences: {error}");
                        announcements.show("Haptic style changed; save failed", 1.0);
                    }
                }
                return;
            }
            if gamepad.pressed(family.back_button()) {
                user_mode.controller_test_back_hold += real_time.delta_secs();
                if user_mode.controller_test_back_hold >= 0.75 {
                    user_mode.controller_test_active = None;
                    user_mode.controller_test_back_hold = 0.0;
                    announcements.show("Controller test complete", 0.7);
                    return;
                }
            } else {
                user_mode.controller_test_back_hold = 0.0;
            }
            if pointer_test || gamepad.just_pressed(GamepadButton::Start) {
                if queue_haptic_pattern(
                    &mut rumble_requests,
                    control_preferences.vibration,
                    active,
                    controller_test_pattern(),
                    HapticPurpose::Test,
                ) {
                    announcements.show("Checking controller haptics…", 0.8);
                } else {
                    announcements.show("Choose Low, Standard, or High to test", 0.9);
                }
            } else if pointer_combat_test || gamepad.just_pressed(GamepadButton::West) {
                if queue_haptic_pattern(
                    &mut rumble_requests,
                    control_preferences.vibration,
                    active,
                    combat_preview_pattern(control_preferences.haptic_style),
                    HapticPurpose::Preview,
                ) {
                    announcements.show(
                        format!(
                            "{} combat preview",
                            control_preferences.haptic_style.label()
                        ),
                        1.1,
                    );
                } else {
                    announcements.show("Choose Low, Standard, or High to preview", 0.9);
                }
            }
            return;
        }

        if let Some(pressed_entity) = gamepads.iter().find_map(|(entity, gamepad)| {
            let family = controller_info(entity, &controller_metadata)
                .map(|info| info.family)
                .unwrap_or_default();
            gamepad
                .just_pressed(family.confirm_button())
                .then_some(entity)
        }) {
            user_mode.controller_test_active = Some(pressed_entity);
            user_mode.controller_test_back_hold = 0.0;
            return;
        }

        let open_navigation = user_mode.input_assignments[0] == LocalInputAssignment::Unassigned;
        let device_action = if open_navigation {
            unassigned_gamepad_user_mode_action(
                UserModeScreen::ControllerTest,
                &gamepads,
                &controller_metadata,
                real_time.delta_secs(),
                &mut menu_navigation.unassigned,
            )
        } else {
            assignment_user_mode_action(
                user_mode.input_assignments[0],
                UserModeScreen::ControllerTest,
                &keys,
                &key_bindings,
                &gamepads,
                &controller_metadata,
                real_time.delta_secs(),
                &mut menu_navigation.seats[0],
            )
        };
        let keyboard_action = open_navigation
            .then(|| keyboard_user_mode_action(&user_mode, &keys))
            .flatten();
        match pointer_action.or(device_action).or(keyboard_action) {
            Some(UserModeUiAction::Previous) => {
                move_controller_test_cursor(&mut user_mode, &gamepads, -1)
            }
            Some(UserModeUiAction::Next) => {
                move_controller_test_cursor(&mut user_mode, &gamepads, 1)
            }
            Some(UserModeUiAction::Confirm) => {
                if let Some(entity) = selected_controller_test_entity(&user_mode, &gamepads) {
                    user_mode.controller_test_active = Some(entity);
                    user_mode.controller_test_back_hold = 0.0;
                } else {
                    announcements.show("No controller detected", 0.8);
                }
            }
            Some(UserModeUiAction::TestVibration) => {
                if let Some(entity) = selected_controller_test_entity(&user_mode, &gamepads) {
                    if queue_haptic_pattern(
                        &mut rumble_requests,
                        control_preferences.vibration,
                        entity,
                        controller_test_pattern(),
                        HapticPurpose::Test,
                    ) {
                        announcements.show("Checking controller haptics…", 0.8);
                    } else {
                        announcements.show("Choose Low, Standard, or High to test", 0.9);
                    }
                }
            }
            Some(UserModeUiAction::TestCombatHaptics) => {
                if let Some(entity) = selected_controller_test_entity(&user_mode, &gamepads) {
                    if queue_haptic_pattern(
                        &mut rumble_requests,
                        control_preferences.vibration,
                        entity,
                        combat_preview_pattern(control_preferences.haptic_style),
                        HapticPurpose::Preview,
                    ) {
                        announcements.show(
                            format!(
                                "{} combat preview",
                                control_preferences.haptic_style.label()
                            ),
                            1.1,
                        );
                    } else {
                        announcements.show("Choose Low, Standard, or High to preview", 0.9);
                    }
                }
            }
            Some(UserModeUiAction::Back) => {
                route_user_mode_action(&mut user_mode, UserModeUiAction::Back);
            }
            _ => {}
        }
        return;
    }

    if user_mode.key_capture.is_some() {
        if pointer_action == Some(UserModeUiAction::Back) || keys.just_pressed(KeyCode::Escape) {
            route_user_mode_action(&mut user_mode, UserModeUiAction::Back);
            return;
        }
        if let Some(key) = keys.get_just_pressed().next().copied() {
            match user_mode.apply_key_capture(&mut key_bindings, key) {
                Ok(result) => {
                    let message = if let Some(swapped) = result.swapped {
                        format!(
                            "P{} {}: {:?} (swapped P{} {})",
                            result.capture.player + 1,
                            result.capture.action.label(),
                            key,
                            swapped.player + 1,
                            swapped.action.label()
                        )
                    } else {
                        format!(
                            "P{} {}: {:?}",
                            result.capture.player + 1,
                            result.capture.action.label(),
                            key
                        )
                    };
                    match save_control_preferences(&key_bindings, &control_preferences) {
                        Ok(()) => announcements.show(message, 1.0),
                        Err(error) => {
                            warn!("Could not save control preferences: {error}");
                            announcements.show(format!("{message} — save failed"), 1.2);
                        }
                    }
                }
                Err("reserved") => announcements.show("Reserved key", 1.0),
                _ => announcements.show("Cannot bind key", 1.0),
            }
        }
        return;
    }

    if user_mode.screen == UserModeScreen::KeySettings {
        let p1_menu_pressed = match user_mode.input_assignments[0] {
            LocalInputAssignment::Gamepad(entity) => gamepads
                .get(entity)
                .map(|(_, gamepad)| gamepad.just_pressed(GamepadButton::Start))
                .unwrap_or(false),
            _ => false,
        };
        if user_mode.key_reset_confirmation {
            let controller_action = match user_mode.input_assignments[0] {
                LocalInputAssignment::Gamepad(entity) => {
                    gamepads.get(entity).ok().and_then(|(_, gamepad)| {
                        let family = controller_info(entity, &controller_metadata)
                            .map(|info| info.family)
                            .unwrap_or_default();
                        if gamepad.just_pressed(family.confirm_button()) {
                            Some(UserModeUiAction::ConfirmKeyReset)
                        } else if gamepad.just_pressed(family.back_button()) {
                            Some(UserModeUiAction::CancelKeyReset)
                        } else {
                            None
                        }
                    })
                }
                _ => None,
            };
            let action = pointer_action
                .or(controller_action)
                .or_else(|| {
                    keys.just_pressed(KeyCode::Enter)
                        .then_some(UserModeUiAction::ConfirmKeyReset)
                })
                .or_else(|| {
                    keys.just_pressed(KeyCode::Escape)
                        .then_some(UserModeUiAction::CancelKeyReset)
                });
            match action {
                Some(UserModeUiAction::ConfirmKeyReset) => {
                    *key_bindings = PlayerKeyBindings::default();
                    user_mode.key_reset_confirmation = false;
                    match save_control_preferences(&key_bindings, &control_preferences) {
                        Ok(()) => announcements.show("Keyboard controls restored", 1.0),
                        Err(error) => {
                            warn!("Could not save control preferences: {error}");
                            announcements
                                .show("Controls restored for this session; save failed", 1.2);
                        }
                    }
                }
                Some(UserModeUiAction::CancelKeyReset | UserModeUiAction::Back) => {
                    user_mode.key_reset_confirmation = false;
                }
                _ => {}
            }
            return;
        }
        if pointer_action == Some(UserModeUiAction::ResetKeys)
            || keys.just_pressed(KeyCode::KeyR)
            || p1_menu_pressed
        {
            user_mode.key_reset_confirmation = true;
            return;
        }
    }

    #[cfg(target_arch = "wasm32")]
    let web_start_requested =
        user_mode.screen == UserModeScreen::ControlsBriefing && web_battle_start_signal_requested();
    #[cfg(not(target_arch = "wasm32"))]
    let web_start_requested = false;

    if pointer_action.is_none() && user_mode.screen == UserModeScreen::CharacterSelect {
        if let Some(route) = handle_character_device_actions(
            &mut user_mode,
            &keys,
            &key_bindings,
            &gamepads,
            &controller_metadata,
            real_time.delta_secs(),
            &mut menu_navigation,
        ) {
            if route == UserModeRoute::ArenaEntered {
                set_active_arena_index(user_mode.arena_index);
                announcements.show("Choose arena", 0.9);
            } else if user_mode.screen == UserModeScreen::CharacterSelect {
                announcements.show(
                    format!(
                        "P{}: {}{}",
                        user_mode.character_select_player + 1,
                        character_label(user_mode.selected_character()),
                        if user_mode.character_ready[user_mode.character_select_player] {
                            " ready"
                        } else {
                            ""
                        }
                    ),
                    0.5,
                );
            }
            return;
        }
    }

    let before_device_join = matches!(
        user_mode.screen,
        UserModeScreen::ModeSelect | UserModeScreen::PlayerCountSelect
    ) || (matches!(
        user_mode.screen,
        UserModeScreen::ControlsHub | UserModeScreen::ControllerTest | UserModeScreen::KeySettings
    ) && user_mode.input_assignments[0]
        == LocalInputAssignment::Unassigned);
    let device_action = if before_device_join {
        unassigned_gamepad_user_mode_action(
            user_mode.screen,
            &gamepads,
            &controller_metadata,
            real_time.delta_secs(),
            &mut menu_navigation.unassigned,
        )
    } else {
        assignment_user_mode_action(
            user_mode.input_assignments[0],
            user_mode.screen,
            &keys,
            &key_bindings,
            &gamepads,
            &controller_metadata,
            real_time.delta_secs(),
            &mut menu_navigation.seats[0],
        )
    };
    let keyboard_action = before_device_join
        .then(|| keyboard_user_mode_action(&user_mode, &keys))
        .flatten()
        .or_else(|| single_player_enter_action(&user_mode, &keys));
    let action = pointer_action
        .or(device_action)
        .or(keyboard_action)
        .or_else(|| web_start_requested.then_some(UserModeUiAction::Confirm));
    let Some(action) = action else {
        return;
    };

    let enters_tutorial = user_mode.screen == UserModeScreen::ModeSelect
        && match action {
            UserModeUiAction::MainMenu(UserModeMainMenuChoice::Tutorial) => true,
            UserModeUiAction::Confirm => {
                user_mode.main_menu_choice == UserModeMainMenuChoice::Tutorial
            }
            _ => false,
        };
    if enters_tutorial {
        user_mode.main_menu_choice = UserModeMainMenuChoice::Tutorial;
        request_tutorial_transition(
            &mut tutorial_transition,
            &mut pause_owners,
            TutorialTransitionAction::EnterDeviceJoin,
        );
        return;
    }

    let route = route_user_mode_action(&mut user_mode, action);
    match route {
        UserModeRoute::None => {}
        UserModeRoute::CharacterPlayerAdvanced => announcements.show(
            format!(
                "P{} choose character",
                user_mode.character_select_player + 1
            ),
            0.9,
        ),
        UserModeRoute::ArenaEntered => {
            set_active_arena_index(user_mode.arena_index);
            announcements.show("Choose arena", 0.9);
        }
        UserModeRoute::ArenaChanged => set_active_arena_index(user_mode.arena_index),
        UserModeRoute::PrepareMatch => {
            stop_user_mode_music(&mut commands, &music);
            let flow = prepare_user_mode_match(&mut user_mode, &mut setup, &mut state);
            announce_user_mode_match_flow(flow, &setup, &mut announcements);
        }
        UserModeRoute::ConfirmBattle => {
            if gameplay_scene.ready_for_battle() {
                #[cfg(target_arch = "wasm32")]
                if web_start_requested {
                    clear_web_battle_start_signal();
                }
                confirm_user_mode_match_start(&mut user_mode, &mut state);
                announcements.show(
                    format!(
                        "Starting match as {}",
                        character_label(setup.player_character())
                    ),
                    0.9,
                );
            } else {
                announcements.show("Loading battle", 0.7);
            }
        }
        UserModeRoute::Replay => {
            reset_user_mode_presentation(
                &mut virtual_time,
                &mut screen_look,
                &mut screen_transition,
            );
            let flow = prepare_user_mode_match(&mut user_mode, &mut setup, &mut state);
            announce_user_mode_match_flow(flow, &setup, &mut announcements);
        }
        UserModeRoute::ChooseCharacter => {
            reset_user_mode_presentation(
                &mut virtual_time,
                &mut screen_look,
                &mut screen_transition,
            );
            state.return_to_setup();
            stop_user_mode_music(&mut commands, &music);
            start_user_mode_menu_music(&mut commands, &asset_server);
            announcements.show("", 0.0);
        }
        UserModeRoute::ControlsBack => {
            state.return_to_setup();
            stop_user_mode_music(&mut commands, &music);
            start_user_mode_menu_music(&mut commands, &asset_server);
            announcements.show("Choose arena", 0.8);
        }
        UserModeRoute::ExitToDev => {
            #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
            {
                stop_user_mode_music(&mut commands, &music);
                reset_user_mode_presentation(
                    &mut virtual_time,
                    &mut screen_look,
                    &mut screen_transition,
                );
                user_mode.exit_to_dev();
                announcements.show("Dev setup", 0.8);
            }
        }
    }
}

pub fn announce_haptic_test_results(
    mut events: MessageReader<HapticPlaybackEvent>,
    mut announcements: ResMut<MatchAnnouncements>,
) {
    for event in events.read() {
        if !matches!(event.purpose, HapticPurpose::Test | HapticPurpose::Preview) {
            continue;
        }
        match &event.result {
            HapticPlaybackResult::Started => {
                let message = match event.purpose {
                    HapticPurpose::Test => "Testing: left motor → right motor → both",
                    HapticPurpose::Preview => "Preview: release → block → heavy → ultimate",
                    _ => unreachable!(),
                };
                announcements.show(message, 1.0);
            }
            HapticPlaybackResult::Completed => {
                let message = if event.purpose == HapticPurpose::Preview {
                    "Combat haptic preview complete"
                } else if cfg!(target_os = "macos") {
                    "Haptic command completed; if silent, reconnect the Xbox controller via Bluetooth"
                } else {
                    "Vibration test complete"
                };
                announcements.show(message, 1.6);
            }
            HapticPlaybackResult::Preempted => {
                announcements.show("Haptic preview replaced by a newer cue", 0.75);
            }
            HapticPlaybackResult::Unsupported => {
                announcements.show("This controller or connection has no haptics", 1.25);
            }
            HapticPlaybackResult::Failed(error) => {
                warn!("Controller haptic playback failed: {error}");
                announcements.show("Controller haptics failed; reconnect and retry", 1.25);
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub fn sync_web_battle_status(
    time: Res<Time>,
    user_mode: Res<UserModeState>,
    state: Res<MatchState>,
    mut scene: ResMut<UserModeGameplayScene>,
) {
    if scene.loaded && scene.warmup_remaining > 0.0 {
        scene.warmup_remaining = (scene.warmup_remaining - time.delta_secs()).max(0.0);
    }

    let briefing = user_mode.screen == UserModeScreen::ControlsBriefing;
    let battle_requested = briefing
        || user_mode.battle_music_pending
        || user_mode.battle_active
        || state.reset_requested
        || state.phase == MatchPhase::Resetting
        || state.phase == MatchPhase::Fighting;

    let status = if scene.ready_for_battle()
        && user_mode.battle_active
        && state.phase == MatchPhase::Fighting
    {
        "ready"
    } else if briefing && scene.ready_for_battle() {
        "briefing_ready"
    } else if battle_requested {
        "loading"
    } else {
        "idle"
    };

    set_web_battle_status(status);
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn sync_web_battle_status() {}

pub fn sync_user_mode_battle_result(
    time: Res<Time<Real>>,
    mut virtual_time: ResMut<Time<Virtual>>,
    mut user_mode: ResMut<UserModeState>,
    mut feedback: ResMut<HitEffects>,
    state: Res<MatchState>,
    mut screen_look: ResMut<ScreenLook>,
    mut screen_transition: ResMut<ScreenLookTransition>,
) {
    if user_mode.battle_active
        && state.phase == MatchPhase::Results
        && user_mode.screen != UserModeScreen::BattleResult
        && !user_mode.tutorial_screen_active()
    {
        user_mode.enter_battle_result(user_mode_result_winner(&state));
        virtual_time.set_relative_speed(USER_MODE_DEATH_SLOW_MOTION_SCALE);
        begin_screen_look_transition(
            &mut screen_look,
            &mut screen_transition,
            ScreenLook::NoirCrime,
            USER_MODE_NOIR_FADE_SECS,
        );
    }

    if user_mode.tick_battle_result(time.delta_secs()) {
        virtual_time.set_relative_speed(1.0);
        if let Some(kind) = result_sfx_kind(&user_mode) {
            feedback.push_combat_sfx(CombatSfxCue::new(
                kind,
                Vec3::ZERO,
                USER_MODE_RESULT_SFX_PRIORITY,
            ));
        }
    }
}

pub fn sync_user_mode_battle_music(
    asset_server: Res<AssetServer>,
    mut user_mode: ResMut<UserModeState>,
    state: Res<MatchState>,
    arena_music: Query<(Entity, &ArenaMusic)>,
    mut commands: Commands,
) {
    if user_mode.battle_music_pending && state.phase == MatchPhase::Fighting {
        reconcile_arena_music(
            &mut commands,
            &asset_server,
            &arena_music,
            state.arena_index,
        );
        user_mode.battle_music_pending = false;
        user_mode.battle_active = true;
        return;
    }

    if user_mode.battle_active && state.phase != MatchPhase::Fighting {
        stop_arena_music(&mut commands, &arena_music);
        user_mode.battle_bot_ai_pending = false;
        user_mode.battle_active = false;
    }
}

pub fn sync_dev_mode_music(
    asset_server: Res<AssetServer>,
    user_mode: Res<UserModeState>,
    arena_music: Query<(Entity, &ArenaMusic)>,
    mut commands: Commands,
) {
    if !dev_mode_music_enabled(&user_mode) {
        return;
    }

    reconcile_arena_music(
        &mut commands,
        &asset_server,
        &arena_music,
        active_arena_index(),
    );
}

pub fn sync_user_mode_battle_bot(
    mut user_mode: ResMut<UserModeState>,
    state: Res<MatchState>,
    scene: Res<UserModeGameplayScene>,
    mut bots: Query<(&Fighter, &mut BotBrain)>,
) {
    if !user_mode.battle_bot_ai_pending
        || user_mode.play_mode != UserPlayMode::SinglePlayer
        || state.phase != MatchPhase::Fighting
        || !scene.ready_for_battle()
    {
        return;
    }

    for (fighter, mut brain) in &mut bots {
        if fighter.id == USER_MODE_BOT_FIGHTER_ID && state.fighter_can_participate(fighter.id) {
            start_bot_combat_ai(&mut brain);
            user_mode.battle_bot_ai_pending = false;
            return;
        }
    }
}

pub fn sync_user_mode_controllers(
    mut commands: Commands,
    user_mode: Res<UserModeState>,
    setup: Res<LocalSetup>,
    mut fighters: Query<(Entity, &Fighter, &mut Controller, Has<BotBrain>)>,
) {
    if !user_mode.battle_active && !user_mode.battle_music_pending {
        return;
    }

    for (entity, fighter, mut controller, has_bot_brain) in &mut fighters {
        reconcile_fighter_control_from_setup(
            &mut commands,
            entity,
            fighter,
            &setup,
            &mut controller,
            has_bot_brain,
        );
    }
}

fn disconnected_controller_seats(
    user_mode: &UserModeState,
    gamepads: &Query<(Entity, &Gamepad)>,
) -> [bool; FIGHTER_COUNT] {
    std::array::from_fn(|seat| {
        seat < user_mode.play_mode.human_player_count()
            && matches!(
                user_mode.input_assignments[seat],
                LocalInputAssignment::Gamepad(entity) if gamepads.get(entity).is_err()
            )
    })
}

pub fn handle_local_controller_reconnect(
    mut user_mode: ResMut<UserModeState>,
    mut setup: ResMut<LocalSetup>,
    state: Res<MatchState>,
    gamepads: Query<(Entity, &Gamepad)>,
    metadata: Query<&ControllerDeviceInfo>,
    mut reconnect: ResMut<LocalControllerReconnect>,
    mut pause_owners: ResMut<GameplayPauseOwners>,
    mut fighters: Query<(&Controller, &mut FighterInput)>,
) {
    if !user_mode.battle_active || state.phase != MatchPhase::Fighting {
        if !reconnect.blocks_gameplay() && !reconnect.any_missing() {
            return;
        }
        pause_owners.set(GameplayPauseOwner::ControllerReconnect, false);
        reconnect.clear();
        return;
    }

    let mut missing = disconnected_controller_seats(&user_mode, &gamepads);
    if missing.iter().any(|seat| *seat) {
        if reconnect.missing_seats != missing {
            reconnect.missing_seats = missing;
        }
        if reconnect.resume_delay_frames != 0 {
            reconnect.resume_delay_frames = 0;
        }
        if !reconnect.paused_by_reconnect {
            reconnect.paused_by_reconnect = true;
            pause_owners.set(GameplayPauseOwner::ControllerReconnect, true);
        }

        for (controller, mut input) in &mut fighters {
            if missing[controller.slot.index()] {
                *input = FighterInput::default();
            }
        }

        for (entity, gamepad) in &gamepads {
            let assignment = LocalInputAssignment::Gamepad(entity);
            let family = controller_info(entity, &metadata)
                .map(|info| info.family)
                .unwrap_or_default();
            if user_mode.assignment_is_joined(assignment)
                || !gamepad.just_pressed(family.confirm_button())
            {
                continue;
            }
            let Some(seat) = missing.iter().position(|seat| *seat) else {
                break;
            };
            user_mode.input_assignments[seat] = assignment;
            setup.slots[seat].input = assignment;
            missing[seat] = false;
        }

        if reconnect.missing_seats != missing {
            reconnect.missing_seats = missing;
        }
        if reconnect.any_missing() {
            return;
        }
        reconnect.resume_delay_frames = 1;
        return;
    }

    if reconnect.missing_seats != [false; FIGHTER_COUNT] {
        reconnect.missing_seats = [false; FIGHTER_COUNT];
    }
    if reconnect.resume_delay_frames > 0 {
        reconnect.resume_delay_frames -= 1;
        return;
    }
    if reconnect.paused_by_reconnect {
        reconnect.paused_by_reconnect = false;
        pause_owners.set(GameplayPauseOwner::ControllerReconnect, false);
    }
}

pub fn update_controller_reconnect_overlay(
    user_mode: Res<UserModeState>,
    reconnect: Res<LocalControllerReconnect>,
    gamepads: Query<(Entity, &Gamepad)>,
    metadata: Query<&ControllerDeviceInfo>,
    mut overlays: Query<&mut Node, With<ControllerReconnectOverlay>>,
    mut texts: Query<&mut Text, With<ControllerReconnectText>>,
) {
    let visible = reconnect.blocks_gameplay();
    for mut node in &mut overlays {
        node.display = if visible {
            Display::Flex
        } else {
            Display::None
        };
    }
    let missing = reconnect
        .missing_seats
        .iter()
        .enumerate()
        .filter_map(|(seat, missing)| missing.then_some(format!("P{}", seat + 1)))
        .collect::<Vec<_>>()
        .join(", ");
    let mut prompts = gamepads
        .iter()
        .filter(|(entity, _)| {
            !user_mode.assignment_is_joined(LocalInputAssignment::Gamepad(*entity))
        })
        .map(|(entity, _)| {
            let family = controller_info(entity, &metadata)
                .map(|info| info.family)
                .unwrap_or_default();
            format!("{} {}", family.display_name(), family.confirm_label())
        })
        .collect::<Vec<_>>();
    prompts.sort();
    prompts.dedup();
    let prompt = if prompts.is_empty() {
        "Confirm on the original or another unassigned controller".to_string()
    } else {
        format!("Press {} to reclaim a seat", prompts.join(" or "))
    };
    for mut text in &mut texts {
        **text = if reconnect.any_missing() {
            format!("{missing} disconnected\n{prompt}")
        } else {
            "Controllers restored — resuming...".to_string()
        };
    }
}

pub fn sync_user_mode_preview_scene(
    mut commands: Commands,
    user_mode: Res<UserModeState>,
    asset_server: Res<AssetServer>,
    character_catalog: Res<CharacterMoveCatalog>,
    roots: Query<Entity, With<UserModePreviewRoot>>,
    previews: Query<(Entity, &UserModePreviewScene)>,
) {
    if user_mode.screen() != UserModeScreen::CharacterSelect {
        return;
    }
    let selected = user_mode.selected_character();
    if previews
        .iter()
        .any(|(_, preview)| preview.character == selected)
    {
        return;
    }
    let Some(scene) = character_scene_model(&asset_server, &character_catalog, selected) else {
        return;
    };
    for (entity, _) in &previews {
        commands.entity(entity).despawn();
    }
    for root in &roots {
        commands.entity(root).with_children(|parent| {
            parent
                .spawn((
                    UserModePreviewScene {
                        character: selected,
                    },
                    SceneRoot(scene.clone()),
                    Transform::from_xyz(0.0, 0.18, 0.0)
                        .with_scale(Vec3::splat(USER_MODE_PREVIEW_SCALE)),
                    user_mode_preview_render_layers(),
                ))
                .observe(apply_user_mode_preview_render_layers);
        });
    }
}

pub fn rotate_user_mode_preview(
    time: Res<Time>,
    user_mode: Res<UserModeState>,
    mut previews: Query<&mut Transform, With<UserModePreviewRoot>>,
    mut cameras: Query<&mut Camera, With<UserModePreviewCamera>>,
) {
    let preview_active = user_mode.screen() == UserModeScreen::CharacterSelect;
    for mut camera in &mut cameras {
        if camera.is_active != preview_active {
            camera.is_active = preview_active;
        }
    }
    if !preview_active {
        return;
    }
    let yaw = time.elapsed_secs() * 0.9;
    for mut transform in &mut previews {
        transform.rotation = Quat::from_rotation_y(yaw);
    }
}

pub fn update_user_mode_selection_previews(
    user_mode: Res<UserModeState>,
    mut character_previews: Query<
        &mut Node,
        (
            With<UserModeCharacterPreview>,
            Without<UserModeArenaPreviewPanel>,
        ),
    >,
    mut arena_preview_panels: Query<
        &mut Node,
        (
            With<UserModeArenaPreviewPanel>,
            Without<UserModeCharacterPreview>,
        ),
    >,
    mut arena_preview_cameras: Query<
        (&mut Camera, &mut Transform),
        With<UserModeArenaPreviewCamera>,
    >,
) {
    let character_visible = user_mode.screen() == UserModeScreen::CharacterSelect;
    let arena_visible = user_mode.screen() == UserModeScreen::ArenaSelect;

    for mut node in &mut character_previews {
        node.display = if character_visible {
            Display::Flex
        } else {
            Display::None
        };
    }
    for mut node in &mut arena_preview_panels {
        node.display = if arena_visible {
            Display::Flex
        } else {
            Display::None
        };
    }

    for (mut camera, mut transform) in &mut arena_preview_cameras {
        if camera.is_active != arena_visible {
            camera.is_active = arena_visible;
        }
        if arena_visible {
            *transform = arena_preview_camera_transform(user_mode.arena_index);
        }
    }
}

pub fn sync_user_mode_ui_camera(
    mut commands: Commands,
    roots: Query<Entity, (With<UserModeRoot>, Without<UiTargetCamera>)>,
    ui_cameras: Query<Entity, With<UiCamera>>,
) {
    let Some(ui_camera) = ui_cameras.iter().next() else {
        return;
    };
    for root in &roots {
        commands.entity(root).insert(UiTargetCamera(ui_camera));
    }
}

fn main_menu_background_path(choice: UserModeMainMenuChoice) -> Option<&'static str> {
    USER_MODE_MAIN_MENU_BACKGROUNDS
        .iter()
        .find_map(|&(mapped_choice, path)| (mapped_choice == choice).then_some(path))
}

fn desired_main_menu_background(user_mode: &UserModeState) -> Option<UserModeMainMenuChoice> {
    (user_mode.screen() == UserModeScreen::ModeSelect).then_some(user_mode.main_menu_choice)
}

pub fn update_user_mode_main_menu_backgrounds(
    user_mode: Res<UserModeState>,
    real_time: Res<Time<Real>>,
    asset_server: Res<AssetServer>,
    mut backgrounds: ParamSet<(
        Query<(&UserModeMainMenuBackground, &ImageNode)>,
        Query<(&mut UserModeMainMenuBackground, &mut Node, &mut ImageNode)>,
    )>,
) {
    let desired_choice = desired_main_menu_background(&user_mode);
    let target = {
        let background_query = backgrounds.p0();
        match desired_choice {
            None => MainMenuBackgroundTarget::Black,
            Some(choice) if main_menu_background_path(choice).is_none() => {
                MainMenuBackgroundTarget::Black
            }
            Some(choice) => match background_query
                .iter()
                .find(|(background, _)| background.choice == choice)
            {
                None => MainMenuBackgroundTarget::Black,
                Some((_, image)) if asset_server.is_loaded_with_dependencies(image.image.id()) => {
                    MainMenuBackgroundTarget::Choice(choice)
                }
                Some((_, image))
                    if matches!(
                        asset_server.load_state(image.image.id()),
                        LoadState::Failed(_)
                    ) =>
                {
                    MainMenuBackgroundTarget::Failed(choice)
                }
                Some(_) => MainMenuBackgroundTarget::HoldForLoad,
            },
        }
    };

    if target == MainMenuBackgroundTarget::HoldForLoad {
        return;
    }

    for (mut background, mut node, mut image) in &mut backgrounds.p1() {
        if target == MainMenuBackgroundTarget::Failed(background.choice)
            && !background.load_failure_reported
        {
            warn!(
                "Could not load main-menu background asset {}",
                background.asset_path
            );
            background.load_failure_reported = true;
        }

        let target_opacity = if target == MainMenuBackgroundTarget::Choice(background.choice) {
            1.0
        } else {
            0.0
        };
        let opacity = background
            .fade
            .advance(target_opacity, real_time.delta_secs());
        image.color = Color::srgba(1.0, 1.0, 1.0, opacity);
        node.display = if opacity > 0.0 {
            Display::Flex
        } else {
            Display::None
        };
    }
}

pub fn update_user_mode_ui(
    user_mode: Res<UserModeState>,
    bindings: Res<PlayerKeyBindings>,
    gamepads: Query<Entity, With<Gamepad>>,
    controller_metadata: Query<&ControllerDeviceInfo>,
    mut roots: Query<(&mut Node, &mut BackgroundColor), With<UserModeRoot>>,
    mut back_buttons: Query<&mut Node, (With<UserModeBackButton>, Without<UserModeRoot>)>,
    mut panels: Query<
        (
            &mut Node,
            Option<&UserModeStartPanel>,
            Option<&UserModeMainMenuPanel>,
            Option<&UserModePlayerCountPanel>,
            Option<&UserModeControlsHubPanel>,
            Option<&UserModeDeviceJoinPanel>,
            Option<&UserModeControllerTestPanel>,
            Option<&UserModeCharacterSelectPanel>,
            Option<&UserModeArenaSelectPanel>,
            Option<&UserModeKeySettingsPanel>,
            Option<&UserModeResultPanel>,
        ),
        (
            Without<UserModeRoot>,
            Without<UserModeBackButton>,
            Without<UserModeDeviceJoinSeatCard>,
            Or<(
                With<UserModeStartPanel>,
                With<UserModeMainMenuPanel>,
                With<UserModePlayerCountPanel>,
                With<UserModeControlsHubPanel>,
                With<UserModeDeviceJoinPanel>,
                With<UserModeControllerTestPanel>,
                With<UserModeCharacterSelectPanel>,
                With<UserModeArenaSelectPanel>,
                With<UserModeKeySettingsPanel>,
                With<UserModeResultPanel>,
            )>,
        ),
    >,
    mut texts: Query<
        (
            &mut Text,
            Option<&UserModeChoiceText>,
            Option<&UserModeCharacterTitleText>,
            Option<&UserModeKeySettingsPromptText>,
            Option<&UserModeKeySettingsRowText>,
            Option<&UserModeResultText>,
            Option<&mut TextColor>,
        ),
        (
            Without<UserModeControlsText>,
            Without<UserModeDeviceJoinText>,
            Without<UserModeDeviceJoinTitleText>,
            Without<UserModeDeviceJoinReadyText>,
            Without<UserModeDeviceJoinClearText>,
            Without<UserModeDeviceJoinSeatText>,
            Or<(
                With<UserModeChoiceText>,
                With<UserModeCharacterTitleText>,
                With<UserModeKeySettingsPromptText>,
                With<UserModeKeySettingsRowText>,
                With<UserModeResultText>,
            )>,
        ),
    >,
    mut join_texts: Query<&mut Text, With<UserModeDeviceJoinText>>,
    mut join_title_texts: Query<
        &mut Text,
        (
            With<UserModeDeviceJoinTitleText>,
            Without<UserModeDeviceJoinText>,
            Without<UserModeDeviceJoinReadyText>,
            Without<UserModeDeviceJoinClearText>,
            Without<UserModeDeviceJoinSeatText>,
        ),
    >,
    mut join_ready_texts: Query<
        &mut Text,
        (
            With<UserModeDeviceJoinReadyText>,
            Without<UserModeDeviceJoinTitleText>,
            Without<UserModeDeviceJoinText>,
            Without<UserModeDeviceJoinClearText>,
            Without<UserModeDeviceJoinSeatText>,
        ),
    >,
    mut join_clear_texts: Query<
        &mut Text,
        (
            With<UserModeDeviceJoinClearText>,
            Without<UserModeDeviceJoinReadyText>,
            Without<UserModeDeviceJoinText>,
            Without<UserModeDeviceJoinTitleText>,
            Without<UserModeDeviceJoinSeatText>,
        ),
    >,
    mut seat_cards: Query<
        (
            &UserModeDeviceJoinSeatCard,
            &mut Node,
            &mut BackgroundColor,
            &mut BorderColor,
        ),
        (Without<UserModeRoot>, Without<UserModeBackButton>),
    >,
    mut seat_texts: Query<
        (&UserModeDeviceJoinSeatText, &mut Text),
        (
            Without<UserModeDeviceJoinText>,
            Without<UserModeDeviceJoinTitleText>,
            Without<UserModeDeviceJoinReadyText>,
            Without<UserModeDeviceJoinClearText>,
        ),
    >,
    mut key_settings_scrolls: Query<(&UserModeKeySettingsScroll, &mut ScrollPosition)>,
) {
    for (mut node, mut background) in &mut roots {
        node.display = if user_mode.active() {
            Display::Flex
        } else {
            Display::None
        };
        let alpha = user_mode_background_alpha(&user_mode);
        *background = BackgroundColor(Color::srgba(0.0, 0.0, 0.0, alpha));
    }

    let key_settings_visible = user_mode.screen() == UserModeScreen::KeySettings;
    let result_visible =
        user_mode.screen() == UserModeScreen::BattleResult && user_mode.result_menu_ready;
    let back_visible = matches!(
        user_mode.screen(),
        UserModeScreen::PlayerCountSelect
            | UserModeScreen::DeviceJoin
            | UserModeScreen::ControlsHub
            | UserModeScreen::ControllerTest
            | UserModeScreen::CharacterSelect
            | UserModeScreen::ArenaSelect
            | UserModeScreen::KeySettings
            | UserModeScreen::ControlsBriefing
    );
    for mut node in &mut back_buttons {
        node.display = if back_visible {
            Display::Flex
        } else {
            Display::None
        };
    }
    for (mut node, start, main, player_count, hub, join, test, character, arena, keys, result) in
        &mut panels
    {
        let visible = (start.is_some() && user_mode.screen() == UserModeScreen::Start)
            || (main.is_some() && user_mode.screen() == UserModeScreen::ModeSelect)
            || (player_count.is_some() && user_mode.screen() == UserModeScreen::PlayerCountSelect)
            || (hub.is_some() && user_mode.screen() == UserModeScreen::ControlsHub)
            || (join.is_some() && user_mode.screen() == UserModeScreen::DeviceJoin)
            || (test.is_some() && user_mode.screen() == UserModeScreen::ControllerTest)
            || (character.is_some() && user_mode.screen() == UserModeScreen::CharacterSelect)
            || (arena.is_some() && user_mode.screen() == UserModeScreen::ArenaSelect)
            || (keys.is_some() && key_settings_visible)
            || (result.is_some() && result_visible);
        node.display = if visible {
            Display::Flex
        } else {
            Display::None
        };
    }
    for mut text in &mut join_texts {
        **text = device_join_message(&user_mode, &bindings, &gamepads, &controller_metadata);
    }
    for mut text in &mut join_title_texts {
        **text = match (
            user_mode.controller_setup_context,
            user_mode.controller_setup_phase,
        ) {
            (_, ControllerSetupPhase::Reorder) => "CHANGE PLAYER ORDER",
            (ControllerSetupContext::Settings, _) => "CONTROLLER SETUP",
            (ControllerSetupContext::Match, _) => "READY YOUR FIGHTERS",
            (ControllerSetupContext::Tutorial, _) => "READY TO TRAIN",
        }
        .to_string();
    }
    for mut text in &mut join_ready_texts {
        **text = match (
            user_mode.controller_setup_context,
            user_mode.controller_setup_phase,
        ) {
            (_, ControllerSetupPhase::Reorder) => "SAVE ORDER",
            (ControllerSetupContext::Settings, _) => "DONE",
            (ControllerSetupContext::Match, _) => "READY",
            (ControllerSetupContext::Tutorial, _) => "READY TO TRAIN",
        }
        .to_string();
    }
    for mut text in &mut join_clear_texts {
        **text = if user_mode.controller_setup_clear_confirmation {
            "CONFIRM CLEAR"
        } else {
            "CLEAR"
        }
        .to_string();
    }
    let target = user_mode.controller_setup_target();
    for (card, mut node, mut background, mut border) in &mut seat_cards {
        let visible = user_mode.screen() == UserModeScreen::DeviceJoin && card.seat < target;
        node.display = if visible {
            Display::Flex
        } else {
            Display::None
        };
        let (background_color, border_color) =
            controller_setup_seat_colors(user_mode.input_assignments[card.seat], &gamepads);
        *background = BackgroundColor(background_color);
        *border = BorderColor::all(border_color);
    }
    for (seat, mut text) in &mut seat_texts {
        **text = controller_setup_seat_message(
            user_mode.input_assignments[seat.seat],
            &gamepads,
            &controller_metadata,
        );
    }

    let selected_key_target = user_mode.selected_key_target();
    for (mut text, choice, character_title, prompt, row, result, color) in &mut texts {
        if choice.is_some() {
            **text = user_mode_choice_message(&user_mode);
        } else if character_title.is_some() {
            **text = character_select_title_message(&user_mode);
        } else if prompt.is_some() {
            **text = key_settings_prompt_message(&user_mode);
        } else if let Some(row) = row {
            let key = bindings
                .key_for(row.player, row.action)
                .expect("valid player binding");
            let selected = key_settings_visible
                && row.player == selected_key_target.player
                && row.action == selected_key_target.action;
            **text = key_settings_row_message(row.action, key, selected);
            if let Some(mut color) = color {
                *color = key_settings_row_color(selected);
            }
        } else if result.is_some() {
            **text = result_title_message(&user_mode);
        }
    }
    for (scroll, mut scroll_position) in &mut key_settings_scrolls {
        if key_settings_visible {
            scroll_position.x = 0.0;
            scroll_position.y = if scroll.player == selected_key_target.player {
                let action_index = key_settings_action_index(selected_key_target.action);
                key_settings_scroll_offset(action_index)
            } else {
                0.0
            };
        } else {
            scroll_position.0 = Vec2::ZERO;
        }
    }
}

fn user_mode_action_selected(user_mode: &UserModeState, action: UserModeUiAction) -> bool {
    match action {
        UserModeUiAction::MainMenu(choice) => {
            user_mode.screen == UserModeScreen::ModeSelect && user_mode.main_menu_choice == choice
        }
        UserModeUiAction::PlayerCount(choice) => {
            user_mode.screen == UserModeScreen::PlayerCountSelect
                && user_mode.player_count_choice == choice
        }
        UserModeUiAction::ControlsHub(choice) => {
            user_mode.screen == UserModeScreen::ControlsHub
                && user_mode.controls_hub_choice == choice
        }
        UserModeUiAction::ControllerSetupChangeOrder
        | UserModeUiAction::ControllerSetupClear
        | UserModeUiAction::ControllerSetupReady => {
            user_mode.screen == UserModeScreen::DeviceJoin
                && user_mode.selected_controller_setup_action() == action
        }
        UserModeUiAction::KeyBinding(capture) => {
            user_mode.screen == UserModeScreen::KeySettings
                && user_mode.selected_key_target() == capture
        }
        UserModeUiAction::Result(choice) => {
            user_mode.screen == UserModeScreen::BattleResult && user_mode.result_choice == choice
        }
        _ => false,
    }
}

pub fn update_user_mode_button_styles(
    user_mode: Res<UserModeState>,
    mut buttons: Query<
        (
            &Interaction,
            &UserModeUiAction,
            &mut BackgroundColor,
            &mut BorderColor,
        ),
        (With<Button>, Without<UserModeRoot>),
    >,
) {
    for (interaction, action, mut background, mut border) in &mut buttons {
        let selected = user_mode_action_selected(&user_mode, *action);
        let (background_color, border_color) = match interaction {
            Interaction::Pressed => (Color::srgb(0.42, 0.31, 0.13), Color::srgb(1.0, 0.86, 0.48)),
            Interaction::Hovered => (Color::srgb(0.19, 0.16, 0.1), Color::srgb(0.94, 0.78, 0.42)),
            Interaction::None if selected => (
                Color::srgb(0.13, 0.105, 0.06),
                Color::srgb(0.86, 0.69, 0.35),
            ),
            Interaction::None => (
                Color::srgba(0.055, 0.055, 0.065, 0.94),
                Color::srgb(0.42, 0.4, 0.35),
            ),
        };
        *background = BackgroundColor(background_color);
        *border = BorderColor::all(border_color);
    }
}

pub fn update_user_mode_controls_ui(
    user_mode: Res<UserModeState>,
    bindings: Res<PlayerKeyBindings>,
    scene: Res<UserModeGameplayScene>,
    metadata: Query<&ControllerDeviceInfo>,
    mut panels: Query<&mut Node, With<UserModeControlsPanel>>,
    mut texts: Query<&mut Text, With<UserModeControlsText>>,
) {
    let visible = user_mode.screen() == UserModeScreen::ControlsBriefing;
    for mut node in &mut panels {
        node.display = if visible {
            Display::Flex
        } else {
            Display::None
        };
    }
    for mut text in &mut texts {
        **text = controls_briefing_message_for_family(
            &user_mode,
            &bindings,
            scene.ready_for_battle(),
            |entity| {
                controller_info(entity, &metadata)
                    .map(|info| info.family)
                    .unwrap_or_default()
            },
        );
    }
}

pub fn update_control_settings_ui(
    user_mode: Res<UserModeState>,
    preferences: Res<ControlPreferences>,
    gamepads: Query<(Entity, &Gamepad)>,
    metadata: Query<&ControllerDeviceInfo>,
    mut test_texts: Query<
        &mut Text,
        (
            With<UserModeControllerTestText>,
            Without<UserModeVibrationButtonText>,
        ),
    >,
    mut vibration_texts: Query<
        &mut Text,
        (
            With<UserModeVibrationButtonText>,
            Without<UserModeControllerTestText>,
        ),
    >,
    mut haptic_style_texts: Query<
        &mut Text,
        (
            With<UserModeHapticStyleButtonText>,
            Without<UserModeControllerTestText>,
            Without<UserModeVibrationButtonText>,
        ),
    >,
    mut reset_panels: Query<&mut Node, With<UserModeKeyResetPanel>>,
) {
    if user_mode.screen() == UserModeScreen::ControllerTest {
        for mut text in &mut test_texts {
            **text = controller_test_message(&user_mode, &gamepads, &metadata);
        }
    }
    for mut text in &mut vibration_texts {
        **text = format!("VIBRATION: {}", preferences.vibration.label());
    }
    for mut text in &mut haptic_style_texts {
        **text = format!("STYLE: {}", preferences.haptic_style.label());
    }
    for mut node in &mut reset_panels {
        node.display = if user_mode.screen() == UserModeScreen::KeySettings
            && user_mode.key_reset_confirmation
        {
            Display::Flex
        } else {
            Display::None
        };
    }
}

fn user_mode_background_alpha(user_mode: &UserModeState) -> f32 {
    match user_mode.screen() {
        UserModeScreen::Start
        | UserModeScreen::ModeSelect
        | UserModeScreen::PlayerCountSelect
        | UserModeScreen::DeviceJoin
        | UserModeScreen::ControlsHub
        | UserModeScreen::ControllerTest
        | UserModeScreen::KeySettings
        | UserModeScreen::CharacterSelect
        | UserModeScreen::ArenaSelect
        | UserModeScreen::ControlsBriefing
        | UserModeScreen::TutorialHub => 1.0,
        UserModeScreen::BattleResult if user_mode.result_menu_ready => 0.58,
        UserModeScreen::BattleResult => 0.0,
        UserModeScreen::TutorialPause | UserModeScreen::TutorialFinalResult => 0.42,
        UserModeScreen::TutorialLesson => 0.0,
        UserModeScreen::Dev => 0.0,
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct MenuDirectionRepeat {
    direction: IVec2,
    held_seconds: f32,
    next_repeat: f32,
}

#[derive(Default)]
pub(crate) struct MenuNavigationTrackers {
    screen: Option<UserModeScreen>,
    seats: [MenuDirectionRepeat; FIGHTER_COUNT],
    unassigned: MenuDirectionRepeat,
}

impl MenuNavigationTrackers {
    fn reset_for_screen(&mut self, screen: UserModeScreen) {
        if self.screen == Some(screen) {
            return;
        }
        self.screen = Some(screen);
        self.seats = [MenuDirectionRepeat::default(); FIGHTER_COUNT];
        self.unassigned = MenuDirectionRepeat::default();
    }
}

fn gamepad_menu_direction(gamepad: &Gamepad) -> IVec2 {
    let dpad = gamepad.dpad();
    let axis = if dpad.length_squared() > 0.0 {
        dpad
    } else {
        gamepad.left_stick()
    };
    if axis.x.abs() >= axis.y.abs() && axis.x.abs() >= USER_MODE_MENU_STICK_THRESHOLD {
        IVec2::new(axis.x.signum() as i32, 0)
    } else if axis.y.abs() >= USER_MODE_MENU_STICK_THRESHOLD {
        IVec2::new(0, axis.y.signum() as i32)
    } else {
        IVec2::ZERO
    }
}

fn repeated_menu_direction(
    direction: IVec2,
    dt: f32,
    tracker: &mut MenuDirectionRepeat,
) -> Option<IVec2> {
    if direction == IVec2::ZERO {
        *tracker = MenuDirectionRepeat::default();
        return None;
    }
    if tracker.direction != direction {
        tracker.direction = direction;
        tracker.held_seconds = 0.0;
        tracker.next_repeat = USER_MODE_MENU_REPEAT_DELAY;
        return Some(direction);
    }

    tracker.held_seconds += dt;
    if tracker.held_seconds < tracker.next_repeat {
        return None;
    }
    tracker.next_repeat += USER_MODE_MENU_REPEAT_INTERVAL;
    Some(direction)
}

fn direction_to_user_mode_action(
    screen: UserModeScreen,
    direction: IVec2,
) -> Option<UserModeUiAction> {
    match screen {
        UserModeScreen::ModeSelect
        | UserModeScreen::PlayerCountSelect
        | UserModeScreen::ControlsHub => {
            if direction.y > 0 {
                Some(UserModeUiAction::Previous)
            } else if direction.y < 0 {
                Some(UserModeUiAction::Next)
            } else {
                None
            }
        }
        UserModeScreen::DeviceJoin
        | UserModeScreen::ControllerTest
        | UserModeScreen::CharacterSelect
        | UserModeScreen::ArenaSelect
        | UserModeScreen::BattleResult => {
            if direction.x < 0 {
                Some(UserModeUiAction::Previous)
            } else if direction.x > 0 {
                Some(UserModeUiAction::Next)
            } else {
                None
            }
        }
        UserModeScreen::KeySettings => {
            if direction.x < 0 {
                Some(UserModeUiAction::PreviousColumn)
            } else if direction.x > 0 {
                Some(UserModeUiAction::NextColumn)
            } else if direction.y > 0 {
                Some(UserModeUiAction::Previous)
            } else if direction.y < 0 {
                Some(UserModeUiAction::Next)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn gamepad_user_mode_action(
    screen: UserModeScreen,
    gamepad: &Gamepad,
    family: ControllerFamily,
    dt: f32,
    tracker: &mut MenuDirectionRepeat,
) -> Option<UserModeUiAction> {
    if gamepad.just_pressed(family.back_button()) {
        return Some(UserModeUiAction::Back);
    }
    if gamepad.just_pressed(family.confirm_button()) {
        return Some(UserModeUiAction::Confirm);
    }
    repeated_menu_direction(gamepad_menu_direction(gamepad), dt, tracker)
        .and_then(|direction| direction_to_user_mode_action(screen, direction))
}

fn keyboard_assignment_user_mode_action(
    screen: UserModeScreen,
    keys: &ButtonInput<KeyCode>,
    bindings: PlayerControlBindings,
) -> Option<UserModeUiAction> {
    if keys.just_pressed(bindings.aim_grab) {
        return Some(UserModeUiAction::Back);
    }
    if keys.just_pressed(bindings.jump) {
        return Some(UserModeUiAction::Confirm);
    }
    let direction = if keys.just_pressed(bindings.left) {
        IVec2::NEG_X
    } else if keys.just_pressed(bindings.right) {
        IVec2::X
    } else if keys.just_pressed(bindings.up) {
        IVec2::Y
    } else if keys.just_pressed(bindings.down) {
        IVec2::NEG_Y
    } else {
        IVec2::ZERO
    };
    direction_to_user_mode_action(screen, direction)
}

fn assignment_user_mode_action(
    assignment: LocalInputAssignment,
    screen: UserModeScreen,
    keys: &ButtonInput<KeyCode>,
    bindings: &PlayerKeyBindings,
    gamepads: &Query<(Entity, &Gamepad)>,
    metadata: &Query<&ControllerDeviceInfo>,
    dt: f32,
    tracker: &mut MenuDirectionRepeat,
) -> Option<UserModeUiAction> {
    match assignment {
        LocalInputAssignment::Keyboard(player) => bindings
            .bindings_for_player(player)
            .and_then(|bindings| keyboard_assignment_user_mode_action(screen, keys, bindings)),
        LocalInputAssignment::Gamepad(entity) => {
            gamepads.get(entity).ok().and_then(|(_, gamepad)| {
                let family = controller_info(entity, metadata)
                    .map(|info| info.family)
                    .unwrap_or_default();
                gamepad_user_mode_action(screen, gamepad, family, dt, tracker)
            })
        }
        LocalInputAssignment::Unassigned => None,
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn user_mode_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    keys.just_pressed(KeyCode::KeyU)
        && (keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight))
}

fn vertical_previous_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    keys.just_pressed(KeyCode::ArrowUp) || keys.just_pressed(KeyCode::KeyW)
}

fn vertical_next_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    keys.just_pressed(KeyCode::ArrowDown) || keys.just_pressed(KeyCode::KeyS)
}

fn keyboard_user_mode_action(
    user_mode: &UserModeState,
    keys: &ButtonInput<KeyCode>,
) -> Option<UserModeUiAction> {
    if keys.just_pressed(KeyCode::Escape) {
        return Some(UserModeUiAction::Back);
    }

    match user_mode.screen {
        UserModeScreen::ModeSelect
        | UserModeScreen::PlayerCountSelect
        | UserModeScreen::ControlsHub => {
            if vertical_previous_pressed(keys) {
                Some(UserModeUiAction::Previous)
            } else if vertical_next_pressed(keys) {
                Some(UserModeUiAction::Next)
            } else if keys.just_pressed(KeyCode::Enter) {
                Some(UserModeUiAction::Confirm)
            } else {
                None
            }
        }
        UserModeScreen::DeviceJoin
        | UserModeScreen::ControllerTest
        | UserModeScreen::CharacterSelect
        | UserModeScreen::ArenaSelect => {
            if select_previous_pressed(keys) {
                Some(UserModeUiAction::Previous)
            } else if select_next_pressed(keys) {
                Some(UserModeUiAction::Next)
            } else if keys.just_pressed(KeyCode::Enter) {
                Some(UserModeUiAction::Confirm)
            } else {
                None
            }
        }
        UserModeScreen::KeySettings => {
            if keys.just_pressed(KeyCode::ArrowUp) {
                Some(UserModeUiAction::Previous)
            } else if keys.just_pressed(KeyCode::ArrowDown) {
                Some(UserModeUiAction::Next)
            } else if keys.just_pressed(KeyCode::ArrowLeft) {
                Some(UserModeUiAction::PreviousColumn)
            } else if keys.just_pressed(KeyCode::ArrowRight) {
                Some(UserModeUiAction::NextColumn)
            } else if keys.just_pressed(KeyCode::Enter) {
                Some(UserModeUiAction::Confirm)
            } else {
                None
            }
        }
        UserModeScreen::ControlsBriefing => (keys.just_pressed(KeyCode::Enter)
            || keys.just_pressed(KeyCode::Space))
        .then_some(UserModeUiAction::Confirm),
        UserModeScreen::BattleResult if user_mode.result_menu_ready => {
            if select_previous_pressed(keys) || select_next_pressed(keys) {
                Some(UserModeUiAction::Next)
            } else if keys.just_pressed(KeyCode::Enter) {
                Some(UserModeUiAction::Confirm)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn single_player_enter_action(
    user_mode: &UserModeState,
    keys: &ButtonInput<KeyCode>,
) -> Option<UserModeUiAction> {
    (user_mode.play_mode == UserPlayMode::SinglePlayer
        && matches!(
            user_mode.screen,
            UserModeScreen::CharacterSelect
                | UserModeScreen::ArenaSelect
                | UserModeScreen::ControlsBriefing
        )
        && keys.just_pressed(KeyCode::Enter))
    .then_some(UserModeUiAction::Confirm)
}

fn select_previous_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    keys.just_pressed(KeyCode::ArrowLeft) || keys.just_pressed(KeyCode::KeyQ)
}

fn select_next_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    keys.just_pressed(KeyCode::ArrowRight) || keys.just_pressed(KeyCode::KeyE)
}

fn user_mode_character_index(character: CharacterKind) -> usize {
    USER_MODE_SELECTABLE_CHARACTERS
        .iter()
        .position(|candidate| *candidate == character)
        .unwrap_or(0)
}

fn next_user_mode_character(character: CharacterKind) -> CharacterKind {
    let index = user_mode_character_index(character);
    USER_MODE_SELECTABLE_CHARACTERS[(index + 1) % USER_MODE_SELECTABLE_CHARACTERS.len()]
}

fn previous_user_mode_character(character: CharacterKind) -> CharacterKind {
    let index = user_mode_character_index(character);
    USER_MODE_SELECTABLE_CHARACTERS[(index + USER_MODE_SELECTABLE_CHARACTERS.len() - 1)
        % USER_MODE_SELECTABLE_CHARACTERS.len()]
}

fn character_select_message(selected: CharacterKind) -> String {
    let index = user_mode_character_index(selected);
    format!(
        "{}\n{} / {}",
        character_label(selected).to_ascii_uppercase(),
        index + 1,
        USER_MODE_SELECTABLE_CHARACTERS.len()
    )
}

fn arena_select_message(selected_index: usize) -> String {
    let arenas = arena_definitions();
    let selected_index = selected_index.min(arenas.len().saturating_sub(1));
    format!(
        "{}\n{} / {}",
        arenas[selected_index].name.to_ascii_uppercase(),
        selected_index + 1,
        arenas.len()
    )
}

fn arena_preview_camera_transform(selected_index: usize) -> Transform {
    let arenas = arena_definitions();
    let selected_index = selected_index.min(arenas.len().saturating_sub(1));
    Transform::from_translation(
        arenas[selected_index].camera_offset * USER_MODE_ARENA_PREVIEW_CAMERA_DISTANCE_SCALE,
    )
    .looking_at(Vec3::Y * 0.6, Vec3::Y)
}

fn user_mode_choice_message(user_mode: &UserModeState) -> String {
    match user_mode.screen {
        UserModeScreen::CharacterSelect => (0..user_mode.play_mode.human_player_count())
            .map(|player| {
                format!(
                    "P{} {}{}",
                    player + 1,
                    character_label(user_mode.player_characters[player]).to_ascii_uppercase(),
                    if user_mode.character_ready[player] {
                        " [READY]"
                    } else if player == user_mode.character_select_player {
                        " <"
                    } else {
                        ""
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("   "),
        UserModeScreen::ArenaSelect => arena_select_message(user_mode.arena_index),
        _ => String::new(),
    }
}

fn character_select_title_message(user_mode: &UserModeState) -> String {
    if user_mode.play_mode.is_single_player() {
        "SELECT A CHARACTER".to_string()
    } else {
        format!(
            "P{} SELECTING — EVERY PLAYER CONFIRMS",
            user_mode.character_select_player + 1
        )
    }
}

fn controller_setup_seat_message(
    assignment: LocalInputAssignment,
    gamepads: &Query<Entity, With<Gamepad>>,
    metadata: &Query<&ControllerDeviceInfo>,
) -> String {
    match assignment {
        LocalInputAssignment::Keyboard(player) => {
            format!("KEYBOARD {}\nCONNECTED\nCustom layout", player + 1)
        }
        LocalInputAssignment::Gamepad(entity) => {
            let info = controller_info(entity, metadata);
            let family = info
                .map(|info| info.family.display_name())
                .unwrap_or("Gamepad");
            let name = info
                .map(|info| info.display_name.as_str())
                .unwrap_or("Controller");
            let connection = if gamepads.get(entity).is_ok() {
                "CONNECTED"
            } else {
                "MISSING — reconnect or replace"
            };
            format!("{family}\n{name}\n{connection}")
        }
        LocalInputAssignment::Unassigned => "WAITING\nPress Confirm to join".to_string(),
    }
}

fn controller_setup_seat_colors(
    assignment: LocalInputAssignment,
    gamepads: &Query<Entity, With<Gamepad>>,
) -> (Color, Color) {
    match assignment {
        LocalInputAssignment::Unassigned => (
            Color::srgba(0.045, 0.045, 0.055, 0.94),
            Color::srgb(0.34, 0.33, 0.31),
        ),
        LocalInputAssignment::Gamepad(entity) if gamepads.get(entity).is_err() => (
            Color::srgba(0.16, 0.055, 0.035, 0.95),
            Color::srgb(1.0, 0.48, 0.27),
        ),
        _ => (
            Color::srgba(0.045, 0.115, 0.09, 0.95),
            Color::srgb(0.38, 0.9, 0.62),
        ),
    }
}

fn device_join_message(
    user_mode: &UserModeState,
    bindings: &PlayerKeyBindings,
    gamepads: &Query<Entity, With<Gamepad>>,
    metadata: &Query<&ControllerDeviceInfo>,
) -> String {
    let target = user_mode.controller_setup_target();
    let mut controller_prompts = gamepads
        .iter()
        .map(|entity| {
            let family = controller_info(entity, metadata)
                .map(|info| info.family)
                .unwrap_or_default();
            format!(
                "{}: {} join / {} leave",
                family.display_name(),
                family.confirm_label(),
                family.back_label()
            )
        })
        .collect::<Vec<_>>();
    controller_prompts.sort();
    controller_prompts.dedup();
    if controller_prompts.is_empty() {
        controller_prompts.push("Controller: Confirm join / Back leave".to_string());
    }
    let keyboard_shortcuts = (0..FIGHTER_COUNT)
        .map(|player| {
            format!(
                "K{}: {} or {}",
                player + 1,
                control_key_label(bindings, player, ControlAction::Jump),
                control_key_label(bindings, player, ControlAction::AimGrab)
            )
        })
        .collect::<Vec<_>>()
        .join("  |  ");
    let status = match (
        user_mode.controller_setup_context,
        user_mode.controller_setup_phase,
    ) {
        (_, ControllerSetupPhase::Reorder) => {
            format!(
                "Choose devices in order — {} selected",
                user_mode.joined_player_count()
            )
        }
        (ControllerSetupContext::Settings, _) => format!(
            "{} session assignment{}",
            user_mode.joined_player_count(),
            if user_mode.joined_player_count() == 1 {
                ""
            } else {
                "s"
            }
        ),
        (ControllerSetupContext::Match, _) => {
            format!(
                "{} / {target} required players ready",
                user_mode.joined_player_count()
            )
        }
        (ControllerSetupContext::Tutorial, _) => {
            format!(
                "{} / 1 trainee ready  |  progress is saved separately",
                user_mode.joined_player_count()
            )
        }
    };
    format!(
        "{}\nKeyboard: press that layout's Jump or Aim/Grab\n{keyboard_shortcuts}\n{status}  |  P1: Left/Right actions, Menu/Esc back",
        controller_prompts.join("  |  "),
    )
}

fn key_settings_prompt_message(user_mode: &UserModeState) -> String {
    if let Some(capture) = user_mode.key_capture {
        return format!(
            "Press the new key for P{} {}  |  Esc cancel",
            capture.player + 1,
            capture.action.label()
        );
    }
    "Up/Down row  |  Left/Right player  |  Confirm change  |  Menu/R restore defaults  |  Back"
        .to_string()
}

fn key_settings_row_message(action: ControlAction, key: KeyCode, selected: bool) -> String {
    let marker = if selected { ">" } else { " " };
    format!("{marker} {:<5}  {key:?}", action.label())
}

fn key_settings_row_color(selected: bool) -> TextColor {
    if selected {
        TextColor(Color::srgb(0.98, 0.9, 0.62))
    } else {
        TextColor(Color::srgb(0.86, 0.82, 0.72))
    }
}

fn key_settings_action_index(action: ControlAction) -> usize {
    ControlAction::ALL
        .iter()
        .position(|candidate| *candidate == action)
        .expect("control action should be in ControlAction::ALL")
}

fn key_settings_scroll_offset(action_index: usize) -> f32 {
    action_index.saturating_sub(USER_MODE_KEY_VISIBLE_ROWS - 1) as f32 * USER_MODE_KEY_ROW_PITCH
}

fn controls_briefing_message(
    user_mode: &UserModeState,
    bindings: &PlayerKeyBindings,
    ready_for_battle: bool,
) -> String {
    controls_briefing_message_for_family(user_mode, bindings, ready_for_battle, |_| {
        ControllerFamily::Xbox
    })
}

fn controls_briefing_message_for_family(
    user_mode: &UserModeState,
    bindings: &PlayerKeyBindings,
    ready_for_battle: bool,
    family_for: impl Fn(Entity) -> ControllerFamily + Copy,
) -> String {
    let p1_assignment = effective_assignment(user_mode, 0);
    let status = if ready_for_battle {
        match p1_assignment {
            LocalInputAssignment::Gamepad(entity) => {
                return controls_briefing_message_with_status(
                    user_mode,
                    bindings,
                    &format!(
                        "P1 press {} or click to fight",
                        family_for(entity).confirm_label()
                    ),
                    family_for,
                );
            }
            LocalInputAssignment::Keyboard(player) => {
                return controls_briefing_message_with_status(
                    user_mode,
                    bindings,
                    &format!(
                        "P1 press {} or click to fight",
                        control_key_label(bindings, player, ControlAction::Jump)
                    ),
                    family_for,
                );
            }
            LocalInputAssignment::Unassigned => unreachable!(),
        }
    } else {
        "Loading battle..."
    };
    controls_briefing_message_with_status(user_mode, bindings, status, family_for)
}

fn controls_briefing_message_with_status(
    user_mode: &UserModeState,
    bindings: &PlayerKeyBindings,
    status: &str,
    family_for: impl Fn(Entity) -> ControllerFamily + Copy,
) -> String {
    let arena = arena_definitions()[user_mode
        .arena_index
        .min(arena_definitions().len().saturating_sub(1))]
    .name;

    if user_mode.play_mode.is_single_player() {
        if matches!(
            effective_assignment(user_mode, 0),
            LocalInputAssignment::Gamepad(_)
        ) {
            return format!(
                "Defeat the bot.\nArena: {arena}\n\n{}\n\n{status}",
                controls_player_message(0, user_mode, bindings, family_for)
            );
        }
        return format!(
            "Defeat the bot.\nArena: {}\n\n{}\n\nDash: double-tap movement\nGuard: {} + {}\n\n{}",
            arena,
            controls_player_message(0, user_mode, bindings, family_for),
            control_key_label(bindings, 0, ControlAction::Heavy),
            control_key_label(bindings, 0, ControlAction::Light),
            status,
        );
    }

    let player_count = user_mode.play_mode.human_player_count();
    let player_controls = (0..player_count)
        .map(|player| controls_player_compact_message(player, user_mode, bindings, family_for))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{player_count} local players.\nArena: {arena}\n\n{player_controls}\n\nKeyboard: double-tap dash, Heavy + Light guard\nController prompts match each detected device family.\n\n{status}"
    )
}

fn effective_assignment(user_mode: &UserModeState, player: usize) -> LocalInputAssignment {
    match user_mode.input_assignments[player] {
        LocalInputAssignment::Unassigned => LocalInputAssignment::Keyboard(player),
        assignment => assignment,
    }
}

fn controls_player_message(
    player: usize,
    user_mode: &UserModeState,
    bindings: &PlayerKeyBindings,
    family_for: impl Fn(Entity) -> ControllerFamily + Copy,
) -> String {
    if let LocalInputAssignment::Gamepad(entity) = effective_assignment(user_mode, player) {
        let family = family_for(entity);
        return format!(
            "P{} — {} Controller\nMove: Left stick / D-pad\n{} Jump  |  {} Light  |  {} Heavy  |  {} Aim/Grab\n{} Dash  |  {} Guard  |  {} Ultimate  |  {} Special\n{}+{} Trap  |  {}+{} Hazard  |  {}+{} Shockwave",
            player + 1,
            family.display_name(),
            family.face_button_label(GamepadButton::South),
            family.face_button_label(GamepadButton::West),
            family.face_button_label(GamepadButton::North),
            family.face_button_label(GamepadButton::East),
            family.face_button_label(GamepadButton::RightTrigger2),
            family.face_button_label(GamepadButton::LeftTrigger),
            family.face_button_label(GamepadButton::LeftTrigger2),
            family.face_button_label(GamepadButton::RightTrigger),
            family.face_button_label(GamepadButton::RightTrigger),
            family.face_button_label(GamepadButton::LeftTrigger),
            family.face_button_label(GamepadButton::RightTrigger),
            family.face_button_label(GamepadButton::North),
            family.face_button_label(GamepadButton::RightTrigger),
            family.face_button_label(GamepadButton::East),
        );
    }
    format!(
        "P{}\nMove: {}/{}/{}/{}\nAim: {}\nHeavy / Throw: {}\nLight / Pickup / Item: {}\nJump: {}\nSpecial: {}  |  +Light Trap  +Aim Shockwave  +Heavy Drift Field",
        player + 1,
        control_key_label(bindings, player, ControlAction::Left),
        control_key_label(bindings, player, ControlAction::Right),
        control_key_label(bindings, player, ControlAction::Up),
        control_key_label(bindings, player, ControlAction::Down),
        control_key_label(bindings, player, ControlAction::AimGrab),
        control_key_label(bindings, player, ControlAction::Heavy),
        control_key_label(bindings, player, ControlAction::Light),
        control_key_label(bindings, player, ControlAction::Jump),
        control_key_label(bindings, player, ControlAction::Special),
    )
}

fn controls_player_compact_message(
    player: usize,
    user_mode: &UserModeState,
    bindings: &PlayerKeyBindings,
    family_for: impl Fn(Entity) -> ControllerFamily + Copy,
) -> String {
    if let LocalInputAssignment::Gamepad(entity) = effective_assignment(user_mode, player) {
        let family = family_for(entity);
        return format!(
            "P{}  {}: Stick/D-pad move | {} jump | {} light | {} heavy | {} aim | {} dash | {} guard | {} ult | {} special",
            player + 1,
            family.display_name(),
            family.face_button_label(GamepadButton::South),
            family.face_button_label(GamepadButton::West),
            family.face_button_label(GamepadButton::North),
            family.face_button_label(GamepadButton::East),
            family.face_button_label(GamepadButton::RightTrigger2),
            family.face_button_label(GamepadButton::LeftTrigger),
            family.face_button_label(GamepadButton::LeftTrigger2),
            family.face_button_label(GamepadButton::RightTrigger),
        );
    }
    format!(
        "P{}  Move {}/{}/{}/{}  |  Aim {}  |  Heavy {}  |  Light {}  |  Jump {}  |  Special {}",
        player + 1,
        control_key_label(bindings, player, ControlAction::Left),
        control_key_label(bindings, player, ControlAction::Right),
        control_key_label(bindings, player, ControlAction::Up),
        control_key_label(bindings, player, ControlAction::Down),
        control_key_label(bindings, player, ControlAction::AimGrab),
        control_key_label(bindings, player, ControlAction::Heavy),
        control_key_label(bindings, player, ControlAction::Light),
        control_key_label(bindings, player, ControlAction::Jump),
        control_key_label(bindings, player, ControlAction::Special),
    )
}

fn control_key_label(bindings: &PlayerKeyBindings, player: usize, action: ControlAction) -> String {
    bindings
        .key_for(player, action)
        .map(key_code_label)
        .unwrap_or_else(|| "?".to_string())
}

fn key_code_label(key: KeyCode) -> String {
    let raw = format!("{key:?}");
    if let Some(label) = raw.strip_prefix("Key") {
        return label.to_string();
    }
    if let Some(label) = raw.strip_prefix("Digit") {
        return label.to_string();
    }
    if let Some(label) = raw.strip_prefix("Arrow") {
        return format!("{label} Arrow");
    }
    raw
}

fn result_title_message(user_mode: &UserModeState) -> String {
    if user_mode.play_mode.is_single_player() {
        return match user_mode.result_winner {
            Some(USER_MODE_PLAYER_FIGHTER_ID) => "YOU WIN".to_string(),
            Some(_) => "YOU LOSE".to_string(),
            None => "DRAW".to_string(),
        };
    }

    match user_mode.result_winner {
        Some(winner) if winner < user_mode.play_mode.human_player_count() => {
            format!("P{} WINS", winner + 1)
        }
        _ => "DRAW".to_string(),
    }
}

fn result_sfx_kind(user_mode: &UserModeState) -> Option<CombatSfxKind> {
    if user_mode.play_mode.is_single_player() {
        return match user_mode.result_winner {
            Some(USER_MODE_PLAYER_FIGHTER_ID) => Some(CombatSfxKind::ResultWin),
            Some(_) => Some(CombatSfxKind::ResultLose),
            None => None,
        };
    }

    user_mode.result_winner.map(|_| CombatSfxKind::ResultWin)
}

pub(crate) fn start_user_mode_menu_music(commands: &mut Commands, asset_server: &AssetServer) {
    commands.spawn((
        UserModeMusic,
        AudioPlayer::new(asset_server.load(USER_MODE_MENU_MUSIC_PATH)),
        PlaybackSettings::LOOP,
    ));
}

fn user_mode_battle_music_path(arena_index: usize) -> &'static str {
    USER_MODE_BATTLE_MUSIC_PATHS[normalized_arena_music_index(arena_index)]
}

fn normalized_arena_music_index(arena_index: usize) -> usize {
    (arena_index < USER_MODE_BATTLE_MUSIC_PATHS.len())
        .then_some(arena_index)
        .unwrap_or(0)
}

fn start_arena_music(commands: &mut Commands, asset_server: &AssetServer, arena_index: usize) {
    let arena_index = normalized_arena_music_index(arena_index);
    commands.spawn((
        UserModeMusic,
        ArenaMusic { arena_index },
        AudioPlayer::new(asset_server.load(user_mode_battle_music_path(arena_index))),
        PlaybackSettings::LOOP,
    ));
}

fn dev_mode_music_enabled(user_mode: &UserModeState) -> bool {
    !user_mode.blocks_dev_input()
}

fn arena_music_should_stay(
    music_arena_index: usize,
    desired_arena_index: usize,
    desired_track_already_kept: bool,
) -> bool {
    !desired_track_already_kept && music_arena_index == desired_arena_index
}

fn reconcile_arena_music(
    commands: &mut Commands,
    asset_server: &AssetServer,
    music: &Query<(Entity, &ArenaMusic)>,
    arena_index: usize,
) {
    let arena_index = normalized_arena_music_index(arena_index);
    let mut desired_track_kept = false;

    for (entity, current_music) in music {
        if arena_music_should_stay(current_music.arena_index, arena_index, desired_track_kept) {
            desired_track_kept = true;
        } else {
            commands.entity(entity).despawn();
        }
    }

    if !desired_track_kept {
        start_arena_music(commands, asset_server, arena_index);
    }
}

pub(crate) fn stop_user_mode_music(
    commands: &mut Commands,
    music: &Query<Entity, With<UserModeMusic>>,
) {
    for entity in music {
        commands.entity(entity).despawn();
    }
}

fn stop_arena_music(commands: &mut Commands, music: &Query<(Entity, &ArenaMusic)>) {
    for (entity, _) in music {
        commands.entity(entity).despawn();
    }
}

fn announce_user_mode_match_flow(
    flow: UserModeMatchStartFlow,
    setup: &LocalSetup,
    announcements: &mut MatchAnnouncements,
) {
    match flow {
        UserModeMatchStartFlow::ControlsBriefing => announcements.show("Review controls", 0.9),
        UserModeMatchStartFlow::BattleStarted => announcements.show(
            format!(
                "Starting match as {}",
                character_label(setup.player_character())
            ),
            0.9,
        ),
    }
}

fn prepare_user_mode_match(
    user_mode: &mut UserModeState,
    setup: &mut LocalSetup,
    state: &mut MatchState,
) -> UserModeMatchStartFlow {
    let player_character = user_mode.player_characters[0];
    user_mode.ensure_test_or_web_assignments();

    setup.set_rule(USER_MODE_STOCK_RULE_INDEX);
    setup.arena_index = user_mode
        .arena_index
        .min(arena_definitions().len().saturating_sub(1));
    match user_mode.play_mode {
        UserPlayMode::SinglePlayer => setup.configure_single_player_duel(
            player_character,
            opposite_user_mode_character(player_character),
        ),
        UserPlayMode::TwoPlayers => setup.configure_two_player_duel(
            user_mode.player_characters[0],
            user_mode.player_characters[1],
        ),
        UserPlayMode::ThreePlayers | UserPlayMode::FourPlayers => setup.configure_local_players(
            user_mode.player_characters,
            user_mode.play_mode.human_player_count(),
        ),
    }
    for player in 0..user_mode.play_mode.human_player_count() {
        setup.slots[player].input = user_mode.input_assignments[player];
    }
    state.rule_index = setup.rule_index;
    state.rules = setup.active_rule();
    state.arena_index = setup.arena_index;
    state.apply_local_setup(setup);
    state.replay_seed = setup.replay_seed;
    state.reset_requested = false;
    set_active_arena_index(state.arena_index);
    if user_mode.controls_briefing_seen {
        confirm_user_mode_match_start(user_mode, state);
        UserModeMatchStartFlow::BattleStarted
    } else {
        user_mode.enter_controls_briefing();
        UserModeMatchStartFlow::ControlsBriefing
    }
}

fn confirm_user_mode_match_start(user_mode: &mut UserModeState, state: &mut MatchState) {
    state.request_rematch();
    user_mode.exit_to_battle();
}

fn reset_user_mode_presentation(
    virtual_time: &mut Time<Virtual>,
    screen_look: &mut ScreenLook,
    screen_transition: &mut ScreenLookTransition,
) {
    virtual_time.set_relative_speed(1.0);
    begin_screen_look_transition(
        screen_look,
        screen_transition,
        ScreenLook::Default,
        USER_MODE_FILTER_RESET_SECS,
    );
}

fn user_mode_result_winner(state: &MatchState) -> Option<usize> {
    let mut winner = None;
    let mut winning_stock = i32::MIN;
    let mut tied = false;

    for fighter_id in 0..FIGHTER_COUNT {
        let Some(stock) = state.stock_for(fighter_id) else {
            continue;
        };
        if stock > winning_stock {
            winner = Some(fighter_id);
            winning_stock = stock;
            tied = false;
        } else if stock == winning_stock {
            tied = true;
        }
    }

    (!tied).then_some(winner).flatten()
}

fn opposite_user_mode_character(character: CharacterKind) -> CharacterKind {
    match character {
        CharacterKind::Cat => CharacterKind::Pig,
        CharacterKind::Pig => CharacterKind::Cat,
        CharacterKind::Bee => CharacterKind::Pig,
        CharacterKind::Penguin => CharacterKind::Pig,
        CharacterKind::Chick => CharacterKind::Pig,
        _ => CharacterKind::Pig,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{LocalInputAssignment, ParticipantKind, PlayerSlotId};

    #[derive(Resource, Default)]
    struct ReconnectGameplayTicks(u32);

    fn count_reconnect_gameplay_ticks(mut ticks: ResMut<ReconnectGameplayTicks>) {
        ticks.0 += 1;
    }

    fn reconnect_test_app(
        play_mode: UserPlayMode,
        assignments: [LocalInputAssignment; FIGHTER_COUNT],
    ) -> App {
        let mut user_mode = UserModeState::default();
        user_mode.play_mode = play_mode;
        user_mode.input_assignments = assignments;
        user_mode.battle_active = true;
        let mut state = MatchState::default();
        state.phase = MatchPhase::Fighting;

        let mut app = App::new();
        app.insert_resource(user_mode)
            .insert_resource(LocalSetup::default())
            .insert_resource(state)
            .init_resource::<LocalControllerReconnect>()
            .init_resource::<GameplayPauseOwners>()
            .init_resource::<Time<Virtual>>()
            .init_resource::<ReconnectGameplayTicks>()
            .add_systems(
                Update,
                (
                    handle_local_controller_reconnect,
                    crate::game_state::sync_virtual_time_pause,
                    count_reconnect_gameplay_ticks
                        .run_if(crate::game_state::match_accepts_gameplay),
                )
                    .chain(),
            );
        app
    }

    fn pressed_a_gamepad() -> Gamepad {
        let mut gamepad = Gamepad::default();
        gamepad.digital_mut().press(GamepadButton::South);
        gamepad
    }

    #[test]
    fn shift_u_enters_user_mode() {
        let mut keys = ButtonInput::<KeyCode>::default();
        keys.press(KeyCode::ShiftLeft);
        keys.press(KeyCode::KeyU);
        assert!(user_mode_pressed(&keys));
    }

    #[test]
    fn fresh_user_mode_entry_opens_mode_select() {
        let mut user_mode = UserModeState::default();
        user_mode.enter_fresh_mode_select();
        assert_eq!(user_mode.screen(), UserModeScreen::ModeSelect);
        assert_eq!(user_mode.play_mode, UserPlayMode::SinglePlayer);
        assert_eq!(
            user_mode.main_menu_choice,
            UserModeMainMenuChoice::SinglePlayer
        );
        assert_eq!(
            user_mode.player_count_choice,
            UserModePlayerCountChoice::TwoPlayers
        );
        assert!(!user_mode.controls_briefing_seen);
    }

    #[test]
    fn main_menu_cycles_and_wraps_four_vertical_choices() {
        let choices = [
            UserModeMainMenuChoice::SinglePlayer,
            UserModeMainMenuChoice::Multiplayer,
            UserModeMainMenuChoice::Tutorial,
            UserModeMainMenuChoice::Settings,
        ];
        let mut choice = UserModeMainMenuChoice::SinglePlayer;
        for expected in choices.into_iter().skip(1) {
            choice = choice.next();
            assert_eq!(choice, expected);
        }
        assert_eq!(choice.next(), UserModeMainMenuChoice::SinglePlayer);
        assert_eq!(
            UserModeMainMenuChoice::SinglePlayer.previous(),
            UserModeMainMenuChoice::Settings
        );
    }

    #[test]
    fn main_menu_background_catalog_maps_every_choice_to_existing_art() {
        let expected = [
            (
                UserModeMainMenuChoice::SinglePlayer,
                USER_MODE_SINGLE_PLAYER_BACKGROUND_PATH,
            ),
            (
                UserModeMainMenuChoice::Multiplayer,
                USER_MODE_MULTIPLAYER_BACKGROUND_PATH,
            ),
            (
                UserModeMainMenuChoice::Tutorial,
                USER_MODE_TUTORIAL_BACKGROUND_PATH,
            ),
            (
                UserModeMainMenuChoice::Settings,
                USER_MODE_SETTINGS_BACKGROUND_PATH,
            ),
        ];

        for (choice, path) in expected {
            assert_eq!(main_menu_background_path(choice), Some(path));
            assert!(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("assets")
                    .join(path)
                    .is_file(),
                "missing main-menu background: {path}"
            );
        }
    }

    #[test]
    fn main_menu_background_fade_eases_and_reverses_without_a_jump() {
        let mut fade = MainMenuBackgroundFade::default();

        assert_eq!(fade.advance(1.0, 0.0), 0.0);
        assert!(
            (fade.advance(1.0, USER_MODE_MAIN_MENU_BACKGROUND_FADE_SECS * 0.5) - 0.5).abs() < 0.001
        );
        let before_reverse = fade.opacity;
        assert_eq!(fade.advance(0.0, 0.0), before_reverse);
        assert!(fade.advance(0.0, USER_MODE_MAIN_MENU_BACKGROUND_FADE_SECS * 0.5) < before_reverse);
        assert_eq!(
            fade.advance(0.0, USER_MODE_MAIN_MENU_BACKGROUND_FADE_SECS),
            0.0
        );
    }

    #[test]
    fn hovering_a_main_menu_button_moves_selection_without_activating_it() {
        let mut user_mode = UserModeState::default();
        user_mode.enter_fresh_mode_select();
        let mut app = App::new();
        app.insert_resource(user_mode)
            .add_systems(Update, sync_main_menu_pointer_hover);
        app.world_mut().spawn((
            Interaction::Hovered,
            UserModeUiAction::MainMenu(UserModeMainMenuChoice::Tutorial),
        ));

        app.update();

        let user_mode = app.world().resource::<UserModeState>();
        assert_eq!(user_mode.main_menu_choice, UserModeMainMenuChoice::Tutorial);
        assert_eq!(user_mode.screen(), UserModeScreen::ModeSelect);
    }

    #[test]
    fn tutorial_routes_through_one_player_device_setup_and_back_to_main_menu() {
        let mut user_mode = UserModeState::default();
        user_mode.enter_fresh_mode_select();

        activate_main_menu_choice(&mut user_mode, UserModeMainMenuChoice::Tutorial);

        assert_eq!(user_mode.screen(), UserModeScreen::DeviceJoin);
        assert_eq!(
            user_mode.controller_setup_context,
            ControllerSetupContext::Tutorial
        );
        assert_eq!(user_mode.controller_setup_target(), 1);
        assert!(user_mode.tutorial_screen_active());

        route_user_mode_action(&mut user_mode, UserModeUiAction::Back);
        assert_eq!(user_mode.screen(), UserModeScreen::ModeSelect);
    }

    #[test]
    fn tutorial_lesson_keeps_normal_bot_specials_available() {
        let mut user_mode = UserModeState::default();
        user_mode.enter_tutorial_lesson();

        assert!(user_mode.battle_active);
        assert_eq!(user_mode.single_player_camera_target_id(), Some(0));
        assert!(!user_mode.restricts_bot_special_inputs());
    }

    #[test]
    fn tutorial_match_screens_keep_the_gameplay_hud_visible() {
        let mut user_mode = UserModeState::default();
        user_mode.enter_fresh_mode_select();
        assert!(!user_mode.shows_gameplay_hud());

        user_mode.enter_tutorial_hub();
        assert!(!user_mode.shows_gameplay_hud());

        user_mode.enter_tutorial_lesson();
        assert!(user_mode.shows_gameplay_hud());

        user_mode.enter_tutorial_pause();
        assert!(user_mode.shows_gameplay_hud());

        user_mode.enter_tutorial_final_result();
        assert!(user_mode.shows_gameplay_hud());
    }

    #[test]
    fn player_count_cycles_wraps_and_maps_to_unchanged_play_modes() {
        let mut choice = UserModePlayerCountChoice::TwoPlayers;
        choice = choice.next();
        assert_eq!(choice, UserModePlayerCountChoice::ThreePlayers);
        choice = choice.next();
        assert_eq!(choice, UserModePlayerCountChoice::FourPlayers);
        assert_eq!(choice.next(), UserModePlayerCountChoice::TwoPlayers);
        assert_eq!(
            UserModePlayerCountChoice::TwoPlayers.previous(),
            UserModePlayerCountChoice::FourPlayers
        );
        assert_eq!(
            UserModePlayerCountChoice::FourPlayers.play_mode(),
            UserPlayMode::FourPlayers
        );
    }

    #[test]
    fn main_menu_and_player_count_actions_route_to_staged_screens() {
        let mut single = UserModeState::default();
        single.enter_mode_select();
        route_user_mode_action(
            &mut single,
            UserModeUiAction::MainMenu(UserModeMainMenuChoice::SinglePlayer),
        );
        assert_eq!(single.screen(), UserModeScreen::DeviceJoin);
        assert_eq!(single.play_mode, UserPlayMode::SinglePlayer);

        let mut multiplayer = UserModeState::default();
        multiplayer.enter_mode_select();
        route_user_mode_action(
            &mut multiplayer,
            UserModeUiAction::MainMenu(UserModeMainMenuChoice::Multiplayer),
        );
        assert_eq!(multiplayer.screen(), UserModeScreen::PlayerCountSelect);
        assert_eq!(
            multiplayer.player_count_choice,
            UserModePlayerCountChoice::TwoPlayers
        );

        for (choice, play_mode) in [
            (
                UserModePlayerCountChoice::TwoPlayers,
                UserPlayMode::TwoPlayers,
            ),
            (
                UserModePlayerCountChoice::ThreePlayers,
                UserPlayMode::ThreePlayers,
            ),
            (
                UserModePlayerCountChoice::FourPlayers,
                UserPlayMode::FourPlayers,
            ),
        ] {
            let mut state = multiplayer.clone();
            route_user_mode_action(&mut state, UserModeUiAction::PlayerCount(choice));
            assert_eq!(state.screen(), UserModeScreen::DeviceJoin);
            assert_eq!(state.play_mode, play_mode);
        }

        let mut settings = UserModeState::default();
        settings.enter_mode_select();
        route_user_mode_action(
            &mut settings,
            UserModeUiAction::MainMenu(UserModeMainMenuChoice::Settings),
        );
        assert_eq!(settings.screen(), UserModeScreen::ControlsHub);
    }

    #[test]
    fn single_and_multiplayer_character_actions_confirm_each_player_in_order() {
        for mode in [
            UserPlayMode::SinglePlayer,
            UserPlayMode::TwoPlayers,
            UserPlayMode::ThreePlayers,
            UserPlayMode::FourPlayers,
        ] {
            let mut user_mode = UserModeState::default();
            user_mode.play_mode = mode;
            user_mode.enter_character_select();
            let player_count = mode.human_player_count();

            for player in 0..player_count {
                assert_eq!(user_mode.character_select_player, player);
                let route = route_user_mode_action(&mut user_mode, UserModeUiAction::Confirm);
                if player + 1 < player_count {
                    assert_eq!(route, UserModeRoute::CharacterPlayerAdvanced);
                } else {
                    assert_eq!(route, UserModeRoute::ArenaEntered);
                }
            }
            assert_eq!(user_mode.screen(), UserModeScreen::ArenaSelect);
        }
    }

    #[test]
    fn hierarchical_back_visits_each_parent_and_previous_player() {
        let mut user_mode = UserModeState::default();
        user_mode.enter_player_count_select();
        route_user_mode_action(&mut user_mode, UserModeUiAction::Back);
        assert_eq!(user_mode.screen(), UserModeScreen::ModeSelect);

        user_mode.enter_key_settings();
        user_mode.begin_key_capture();
        route_user_mode_action(&mut user_mode, UserModeUiAction::Back);
        assert_eq!(user_mode.screen(), UserModeScreen::KeySettings);
        assert_eq!(user_mode.key_capture, None);
        route_user_mode_action(&mut user_mode, UserModeUiAction::Back);
        assert_eq!(user_mode.screen(), UserModeScreen::ControlsHub);
        route_user_mode_action(&mut user_mode, UserModeUiAction::Back);
        assert_eq!(user_mode.screen(), UserModeScreen::ModeSelect);

        user_mode.play_mode = UserPlayMode::FourPlayers;
        user_mode.return_to_character_select_player(3);
        for previous_player in (0..3).rev() {
            route_user_mode_action(&mut user_mode, UserModeUiAction::Back);
            assert_eq!(user_mode.character_select_player, previous_player);
        }
        route_user_mode_action(&mut user_mode, UserModeUiAction::Back);
        assert_eq!(user_mode.screen(), UserModeScreen::DeviceJoin);
        route_user_mode_action(&mut user_mode, UserModeUiAction::Back);
        assert_eq!(user_mode.screen(), UserModeScreen::PlayerCountSelect);

        user_mode.play_mode = UserPlayMode::SinglePlayer;
        user_mode.enter_character_select();
        route_user_mode_action(&mut user_mode, UserModeUiAction::Back);
        assert_eq!(user_mode.screen(), UserModeScreen::DeviceJoin);
        route_user_mode_action(&mut user_mode, UserModeUiAction::Back);
        assert_eq!(user_mode.screen(), UserModeScreen::ModeSelect);

        user_mode.play_mode = UserPlayMode::ThreePlayers;
        user_mode.enter_arena_select();
        route_user_mode_action(&mut user_mode, UserModeUiAction::Back);
        assert_eq!(user_mode.screen(), UserModeScreen::CharacterSelect);
        assert_eq!(user_mode.character_select_player, 2);
    }

    #[test]
    fn controls_back_preserves_arena_and_requests_clean_menu_music_restore() {
        let mut user_mode = UserModeState::default();
        user_mode.arena_index = 7;
        user_mode.enter_controls_briefing();

        let route = route_user_mode_action(&mut user_mode, UserModeUiAction::Back);

        assert_eq!(route, UserModeRoute::ControlsBack);
        assert_eq!(user_mode.screen(), UserModeScreen::ArenaSelect);
        assert_eq!(user_mode.arena_index, 7);
        assert!(!user_mode.controls_briefing_seen);
    }

    #[test]
    fn keyboard_vertical_menu_actions_support_arrows_and_w_s() {
        let mut user_mode = UserModeState::default();
        user_mode.enter_mode_select();
        for (key, expected) in [
            (KeyCode::ArrowUp, UserModeUiAction::Previous),
            (KeyCode::KeyW, UserModeUiAction::Previous),
            (KeyCode::ArrowDown, UserModeUiAction::Next),
            (KeyCode::KeyS, UserModeUiAction::Next),
        ] {
            let mut keys = ButtonInput::default();
            keys.press(key);
            assert_eq!(keyboard_user_mode_action(&user_mode, &keys), Some(expected));
        }
    }

    #[test]
    fn mixed_devices_join_in_order_without_duplicate_ownership() {
        let mut user_mode = UserModeState::default();
        user_mode.play_mode = UserPlayMode::FourPlayers;
        user_mode.enter_device_join();
        let first_gamepad = Entity::from_raw_u32(11).expect("valid entity");
        let second_gamepad = Entity::from_raw_u32(12).expect("valid entity");

        assert_eq!(
            user_mode.join_assignment(LocalInputAssignment::Gamepad(first_gamepad)),
            Some(0)
        );
        assert_eq!(
            user_mode.join_assignment(LocalInputAssignment::Keyboard(2)),
            Some(1)
        );
        assert_eq!(
            user_mode.join_assignment(LocalInputAssignment::Gamepad(second_gamepad)),
            Some(2)
        );
        assert_eq!(
            user_mode.join_assignment(LocalInputAssignment::Keyboard(0)),
            Some(3)
        );
        assert_eq!(
            user_mode.join_assignment(LocalInputAssignment::Gamepad(first_gamepad)),
            None
        );
        assert_eq!(user_mode.joined_player_count(), FIGHTER_COUNT);
        assert_eq!(
            user_mode.input_assignments,
            [
                LocalInputAssignment::Gamepad(first_gamepad),
                LocalInputAssignment::Keyboard(2),
                LocalInputAssignment::Gamepad(second_gamepad),
                LocalInputAssignment::Keyboard(0),
            ]
        );
    }

    #[test]
    fn leaving_device_compacts_join_order_and_frees_the_last_seat() {
        let mut user_mode = UserModeState::default();
        user_mode.play_mode = UserPlayMode::ThreePlayers;
        user_mode.enter_device_join();
        let gamepad = Entity::from_raw_u32(21).expect("valid entity");
        for assignment in [
            LocalInputAssignment::Keyboard(0),
            LocalInputAssignment::Gamepad(gamepad),
            LocalInputAssignment::Keyboard(3),
        ] {
            user_mode.join_assignment(assignment).unwrap();
        }

        assert_eq!(
            user_mode.leave_assignment(LocalInputAssignment::Gamepad(gamepad)),
            Some(1)
        );
        assert_eq!(
            user_mode.input_assignments,
            [
                LocalInputAssignment::Keyboard(0),
                LocalInputAssignment::Keyboard(3),
                LocalInputAssignment::Unassigned,
                LocalInputAssignment::Unassigned,
            ]
        );
    }

    #[test]
    fn every_keyboard_layout_can_join_with_jump_or_aim_grab() {
        let bindings = PlayerKeyBindings::default();
        for player in 0..FIGHTER_COUNT {
            let player_bindings = bindings.bindings_for_player(player).unwrap();
            for key in [player_bindings.jump, player_bindings.aim_grab] {
                let mut keys = ButtonInput::default();
                keys.press(key);
                assert!(keyboard_join_requested(&keys, player_bindings));
            }
        }
    }

    #[test]
    fn xbox_a_joins_and_b_leaves() {
        let mut join = Gamepad::default();
        join.digital_mut().press(GamepadButton::South);
        assert!(gamepad_join_requested(&join, ControllerFamily::Xbox));
        assert!(!gamepad_leave_requested(&join, ControllerFamily::Xbox));

        let mut leave = Gamepad::default();
        leave.digital_mut().press(GamepadButton::East);
        assert!(gamepad_leave_requested(&leave, ControllerFamily::Xbox));
        assert!(!gamepad_join_requested(&leave, ControllerFamily::Xbox));
    }

    #[test]
    fn held_gamepad_menu_direction_repeats_after_delay() {
        let mut tracker = MenuDirectionRepeat::default();
        assert_eq!(
            repeated_menu_direction(IVec2::X, 0.0, &mut tracker),
            Some(IVec2::X)
        );
        assert_eq!(
            repeated_menu_direction(IVec2::X, USER_MODE_MENU_REPEAT_DELAY - 0.01, &mut tracker),
            None
        );
        assert_eq!(
            repeated_menu_direction(IVec2::X, 0.02, &mut tracker),
            Some(IVec2::X)
        );
        assert_eq!(
            repeated_menu_direction(IVec2::ZERO, 0.0, &mut tracker),
            None
        );
        assert_eq!(tracker, MenuDirectionRepeat::default());
    }

    #[test]
    fn gamepad_menu_stick_uses_threshold_and_dpad_priority() {
        let mut gamepad = Gamepad::default();
        gamepad.analog_mut().set(
            GamepadAxis::LeftStickX,
            USER_MODE_MENU_STICK_THRESHOLD - 0.01,
        );
        assert_eq!(gamepad_menu_direction(&gamepad), IVec2::ZERO);

        gamepad
            .analog_mut()
            .set(GamepadAxis::LeftStickX, USER_MODE_MENU_STICK_THRESHOLD);
        assert_eq!(gamepad_menu_direction(&gamepad), IVec2::X);

        gamepad.analog_mut().set(GamepadButton::DPadUp, 1.0);
        assert_eq!(gamepad_menu_direction(&gamepad), IVec2::Y);
    }

    #[test]
    fn shared_menu_keyboard_action_comes_from_p1_assignment() {
        let bindings = PlayerKeyBindings::default();
        let p1 = bindings.bindings_for_player(0).unwrap();
        let p2 = bindings.bindings_for_player(1).unwrap();
        let mut keys = ButtonInput::default();
        keys.press(p2.jump);

        assert_eq!(
            keyboard_assignment_user_mode_action(UserModeScreen::ArenaSelect, &keys, p1),
            None
        );
        assert_eq!(
            keyboard_assignment_user_mode_action(UserModeScreen::ArenaSelect, &keys, p2),
            Some(UserModeUiAction::Confirm)
        );
    }

    #[test]
    fn enter_confirms_single_player_character_arena_and_fight() {
        let mut user_mode = UserModeState::default();
        user_mode.play_mode = UserPlayMode::SinglePlayer;
        let mut keys = ButtonInput::default();
        keys.press(KeyCode::Enter);

        user_mode.enter_character_select();
        let character_action = single_player_enter_action(&user_mode, &keys);
        assert_eq!(character_action, Some(UserModeUiAction::Confirm));
        assert_eq!(
            route_user_mode_action(&mut user_mode, character_action.unwrap()),
            UserModeRoute::ArenaEntered
        );
        assert_eq!(user_mode.screen(), UserModeScreen::ArenaSelect);

        let arena_action = single_player_enter_action(&user_mode, &keys);
        assert_eq!(arena_action, Some(UserModeUiAction::Confirm));
        assert_eq!(
            route_user_mode_action(&mut user_mode, arena_action.unwrap()),
            UserModeRoute::PrepareMatch
        );

        user_mode.enter_controls_briefing();
        let fight_action = single_player_enter_action(&user_mode, &keys);
        assert_eq!(fight_action, Some(UserModeUiAction::Confirm));
        assert_eq!(
            route_user_mode_action(&mut user_mode, fight_action.unwrap()),
            UserModeRoute::ConfirmBattle
        );

        user_mode.play_mode = UserPlayMode::FourPlayers;
        assert_eq!(
            single_player_enter_action(&user_mode, &keys),
            None,
            "multiplayer confirmation must remain scoped to each assigned seat"
        );
    }

    #[test]
    fn one_disconnect_neutralizes_and_gates_until_a_delayed_resume() {
        let disconnected = Entity::from_raw_u32(51).expect("valid entity");
        let mut app = reconnect_test_app(
            UserPlayMode::SinglePlayer,
            [
                LocalInputAssignment::Gamepad(disconnected),
                LocalInputAssignment::Unassigned,
                LocalInputAssignment::Unassigned,
                LocalInputAssignment::Unassigned,
            ],
        );
        let fighter = app
            .world_mut()
            .spawn((
                Controller::new(
                    PlayerSlotId::new(0).unwrap(),
                    ParticipantKind::Human,
                    LocalInputAssignment::Gamepad(disconnected),
                ),
                FighterInput {
                    jump: true,
                    light: true,
                    ..default()
                },
            ))
            .id();

        app.update();

        assert!(
            app.world()
                .resource::<LocalControllerReconnect>()
                .any_missing()
        );
        assert!(
            app.world()
                .resource::<LocalControllerReconnect>()
                .blocks_gameplay()
        );
        assert!(app.world().resource::<Time<Virtual>>().is_paused());
        let input = app.world().get::<FighterInput>(fighter).unwrap();
        assert_eq!(input.movement, Vec2::ZERO);
        assert!(!input.jump);
        assert!(!input.light);
        assert!(!input.heavy);
        assert!(!input.dash);
        assert_eq!(app.world().resource::<ReconnectGameplayTicks>().0, 0);

        let replacement = app.world_mut().spawn(pressed_a_gamepad()).id();
        app.update();
        assert_eq!(
            app.world().resource::<UserModeState>().input_assignments[0],
            LocalInputAssignment::Gamepad(replacement)
        );
        assert!(
            app.world()
                .resource::<LocalControllerReconnect>()
                .blocks_gameplay()
        );

        app.update();
        assert!(
            app.world()
                .resource::<LocalControllerReconnect>()
                .blocks_gameplay()
        );
        assert_eq!(app.world().resource::<ReconnectGameplayTicks>().0, 0);

        app.update();
        assert!(
            !app.world()
                .resource::<LocalControllerReconnect>()
                .blocks_gameplay()
        );
        assert!(!app.world().resource::<Time<Virtual>>().is_paused());
        assert_eq!(app.world().resource::<ReconnectGameplayTicks>().0, 1);
    }

    #[test]
    fn multiple_missing_seats_require_distinct_replacement_controllers() {
        let first_missing = Entity::from_raw_u32(61).expect("valid entity");
        let second_missing = Entity::from_raw_u32(62).expect("valid entity");
        let mut app = reconnect_test_app(
            UserPlayMode::TwoPlayers,
            [
                LocalInputAssignment::Gamepad(first_missing),
                LocalInputAssignment::Gamepad(second_missing),
                LocalInputAssignment::Unassigned,
                LocalInputAssignment::Unassigned,
            ],
        );

        app.update();
        assert_eq!(
            app.world()
                .resource::<LocalControllerReconnect>()
                .missing_seats,
            [true, true, false, false]
        );

        let first_replacement = app.world_mut().spawn(pressed_a_gamepad()).id();
        app.update();
        assert_eq!(
            app.world().resource::<UserModeState>().input_assignments[0],
            LocalInputAssignment::Gamepad(first_replacement)
        );
        assert_eq!(
            app.world()
                .resource::<LocalControllerReconnect>()
                .missing_seats,
            [false, true, false, false]
        );

        let second_replacement = app.world_mut().spawn(pressed_a_gamepad()).id();
        app.update();
        let user_mode = app.world().resource::<UserModeState>();
        assert_eq!(
            user_mode.input_assignments[0],
            LocalInputAssignment::Gamepad(first_replacement)
        );
        assert_eq!(
            user_mode.input_assignments[1],
            LocalInputAssignment::Gamepad(second_replacement)
        );
        assert_eq!(
            app.world().resource::<LocalSetup>().slots[1].input,
            LocalInputAssignment::Gamepad(second_replacement)
        );
        assert!(
            app.world()
                .resource::<LocalControllerReconnect>()
                .blocks_gameplay()
        );
    }

    #[test]
    fn four_player_character_select_confirms_each_player_in_order() {
        let mut user_mode = UserModeState::default();
        user_mode.play_mode = UserPlayMode::FourPlayers;
        user_mode.enter_character_select();

        for player in 0..FIGHTER_COUNT - 1 {
            assert_eq!(user_mode.character_select_player, player);
            assert!(!user_mode.confirm_character_selection());
        }
        assert_eq!(user_mode.character_select_player, FIGHTER_COUNT - 1);
        assert!(user_mode.confirm_character_selection());
    }

    #[test]
    fn selection_cycles_cat_pig_bee_penguin_and_chick() {
        let mut user_mode = UserModeState::default();
        assert_eq!(user_mode.selected_character(), CharacterKind::Cat);

        user_mode.select_next();
        assert_eq!(user_mode.selected_character(), CharacterKind::Pig);

        user_mode.select_next();
        assert_eq!(user_mode.selected_character(), CharacterKind::Bee);

        user_mode.select_next();
        assert_eq!(user_mode.selected_character(), CharacterKind::Penguin);

        user_mode.select_next();
        assert_eq!(user_mode.selected_character(), CharacterKind::Chick);

        user_mode.select_next();
        assert_eq!(user_mode.selected_character(), CharacterKind::Cat);

        user_mode.select_previous();
        assert_eq!(user_mode.selected_character(), CharacterKind::Chick);

        user_mode.select_previous();
        assert_eq!(user_mode.selected_character(), CharacterKind::Penguin);
    }

    #[test]
    fn character_selector_copy_only_exposes_focused_choice_and_counter() {
        let message = character_select_message(CharacterKind::Bee);

        assert_eq!(message, "BEE\n3 / 5");
        assert!(!message.contains("CAT"));
        assert!(!message.contains("PIG"));
        assert!(!message.contains("PENGUIN"));
        assert!(!message.contains("CHICK"));
    }

    #[test]
    fn arena_selection_cycles_through_available_maps() {
        let mut user_mode = UserModeState::default();
        user_mode.enter_arena_select();

        user_mode.select_previous_arena();
        assert_eq!(user_mode.arena_index, arena_definitions().len() - 1);

        user_mode.select_next_arena();
        assert_eq!(user_mode.arena_index, 0);

        user_mode.select_next_arena();
        assert_eq!(user_mode.arena_index, 1);
    }

    #[test]
    fn arena_selector_copy_only_exposes_focused_choice_and_counter() {
        let message = arena_select_message(5);

        assert_eq!(message, "BUMPER ALLEY\n6 / 10");
        assert!(!message.contains("CROWN RING"));
        assert!(!message.contains("POWDER KEG COURT"));
    }

    #[test]
    fn arena_preview_camera_frames_each_selected_arena() {
        for (index, arena) in arena_definitions().iter().enumerate() {
            assert_eq!(
                arena_preview_camera_transform(index).translation,
                arena.camera_offset * USER_MODE_ARENA_PREVIEW_CAMERA_DISTANCE_SCALE
            );
        }
        assert_eq!(
            arena_preview_camera_transform(arena_definitions().len()).translation,
            arena_preview_camera_transform(arena_definitions().len() - 1).translation
        );
    }

    #[test]
    fn user_mode_preview_uses_dedicated_non_world_render_layer() {
        let layers = user_mode_preview_render_layers();

        assert_eq!(
            layers.iter().collect::<Vec<_>>(),
            vec![USER_MODE_PREVIEW_LAYER]
        );
        assert_ne!(USER_MODE_PREVIEW_LAYER, 0);
        assert!(!layers.intersects(&RenderLayers::default()));
    }

    #[test]
    fn user_mode_blocks_dev_input_for_screens_and_battle() {
        let mut user_mode = UserModeState::default();
        assert!(!user_mode.blocks_dev_input());
        assert!(!user_mode.hides_dev_controls());

        user_mode.enter_mode_select();
        assert!(user_mode.blocks_dev_input());
        assert!(user_mode.hides_dev_controls());

        user_mode.enter_player_count_select();
        assert!(user_mode.blocks_dev_input());

        user_mode.enter_arena_select();
        assert!(user_mode.blocks_dev_input());

        user_mode.enter_key_settings();
        assert!(user_mode.blocks_dev_input());

        user_mode.enter_battle_result(Some(USER_MODE_PLAYER_FIGHTER_ID));
        assert!(user_mode.blocks_dev_input());

        let mut setup = LocalSetup::default();
        let mut state = MatchState::default();
        user_mode.screen = UserModeScreen::CharacterSelect;
        prepare_user_mode_match(&mut user_mode, &mut setup, &mut state);

        assert_eq!(user_mode.screen(), UserModeScreen::ControlsBriefing);
        assert!(user_mode.blocks_dev_input());
        assert!(user_mode.hides_dev_controls());

        confirm_user_mode_match_start(&mut user_mode, &mut state);
        assert_eq!(user_mode.screen(), UserModeScreen::Dev);
        assert!(state.reset_requested);
    }

    #[test]
    fn single_player_camera_target_only_applies_to_user_mode_battle() {
        let mut user_mode = UserModeState::default();
        assert_eq!(user_mode.single_player_camera_target_id(), None);

        user_mode.play_mode = UserPlayMode::SinglePlayer;
        user_mode.battle_active = true;
        assert_eq!(
            user_mode.single_player_camera_target_id(),
            Some(USER_MODE_PLAYER_FIGHTER_ID)
        );

        user_mode.play_mode = UserPlayMode::TwoPlayers;
        assert_eq!(user_mode.single_player_camera_target_id(), None);

        user_mode.play_mode = UserPlayMode::SinglePlayer;
        user_mode.battle_active = false;
        user_mode.battle_music_pending = true;
        assert_eq!(
            user_mode.single_player_camera_target_id(),
            Some(USER_MODE_PLAYER_FIGHTER_ID)
        );

        user_mode.battle_music_pending = false;
        user_mode.enter_battle_result(Some(USER_MODE_PLAYER_FIGHTER_ID));
        assert_eq!(
            user_mode.single_player_camera_target_id(),
            Some(USER_MODE_PLAYER_FIGHTER_ID)
        );
    }

    #[test]
    fn user_mode_prepare_applies_selected_character_without_starting_match() {
        let mut user_mode = UserModeState::default();
        let mut setup = LocalSetup::default();
        let mut state = MatchState::default();
        user_mode.player_characters[0] = CharacterKind::Pig;
        user_mode.arena_index = 5;
        user_mode.screen = UserModeScreen::CharacterSelect;

        let flow = prepare_user_mode_match(&mut user_mode, &mut setup, &mut state);

        assert_eq!(flow, UserModeMatchStartFlow::ControlsBriefing);
        assert_eq!(setup.player_character(), CharacterKind::Pig);
        assert_eq!(setup.arena_index, 5);
        assert_eq!(state.arena_index, 5);
        assert_eq!(
            setup.slots[USER_MODE_BOT_FIGHTER_ID].character,
            CharacterKind::Cat
        );
        assert_eq!(
            setup.slots[USER_MODE_BOT_FIGHTER_ID].participant,
            ParticipantKind::Bot
        );
        assert_eq!(setup.active_bot_count(), 1);
        assert_eq!(state.active_fighter_count, 2);
        assert!(state.rules.uses_stocks());
        assert!(!state.reset_requested);
        assert_eq!(user_mode.screen(), UserModeScreen::ControlsBriefing);
        assert!(user_mode.controls_briefing_seen);
        assert!(!user_mode.battle_music_pending);
        assert!(!user_mode.battle_bot_ai_pending);
        assert!(!user_mode.battle_active);
        assert!(user_mode.restricts_bot_special_inputs());
        assert!(user_mode.hides_dev_controls());
    }

    #[test]
    fn user_mode_prepare_skips_controls_after_first_briefing() {
        let mut user_mode = UserModeState::default();
        let mut setup = LocalSetup::default();
        let mut state = MatchState::default();
        user_mode.screen = UserModeScreen::CharacterSelect;

        let first_flow = prepare_user_mode_match(&mut user_mode, &mut setup, &mut state);
        assert_eq!(first_flow, UserModeMatchStartFlow::ControlsBriefing);
        confirm_user_mode_match_start(&mut user_mode, &mut state);

        user_mode.enter_battle_result(Some(USER_MODE_PLAYER_FIGHTER_ID));
        user_mode.enter_mode_select();
        user_mode.enter_character_select();
        user_mode.player_characters[0] = CharacterKind::Pig;

        let replay_flow = prepare_user_mode_match(&mut user_mode, &mut setup, &mut state);

        assert_eq!(replay_flow, UserModeMatchStartFlow::BattleStarted);
        assert_eq!(user_mode.screen(), UserModeScreen::Dev);
        assert!(state.reset_requested);
        assert!(user_mode.battle_active);
        assert_eq!(setup.player_character(), CharacterKind::Pig);
    }

    #[test]
    fn user_mode_confirm_controls_starts_prepared_match() {
        let mut user_mode = UserModeState::default();
        let mut setup = LocalSetup::default();
        let mut state = MatchState::default();
        user_mode.player_characters[0] = CharacterKind::Pig;
        user_mode.screen = UserModeScreen::CharacterSelect;
        prepare_user_mode_match(&mut user_mode, &mut setup, &mut state);

        confirm_user_mode_match_start(&mut user_mode, &mut state);

        assert!(state.reset_requested);
        assert_eq!(user_mode.screen(), UserModeScreen::Dev);
        assert!(user_mode.battle_music_pending);
        assert!(user_mode.battle_bot_ai_pending);
        assert!(user_mode.restricts_bot_special_inputs());
        assert!(user_mode.hides_dev_controls());
    }

    #[test]
    fn user_mode_start_pairs_cat_with_pig_bot() {
        let mut user_mode = UserModeState::default();
        let mut setup = LocalSetup::default();
        let mut state = MatchState::default();
        user_mode.player_characters[0] = CharacterKind::Cat;
        user_mode.screen = UserModeScreen::CharacterSelect;

        prepare_user_mode_match(&mut user_mode, &mut setup, &mut state);

        assert_eq!(
            setup.slots[USER_MODE_PLAYER_FIGHTER_ID].character,
            CharacterKind::Cat
        );
        assert_eq!(
            setup.slots[USER_MODE_BOT_FIGHTER_ID].character,
            CharacterKind::Pig
        );
        assert_eq!(
            setup.slots[USER_MODE_PLAYER_FIGHTER_ID].input,
            LocalInputAssignment::Keyboard(0)
        );
        assert_eq!(state.active_fighter_count, 2);
        assert!(state.rules.uses_stocks());
    }

    #[test]
    fn user_mode_start_pairs_bee_with_pig_bot() {
        let mut user_mode = UserModeState::default();
        let mut setup = LocalSetup::default();
        let mut state = MatchState::default();
        user_mode.player_characters[0] = CharacterKind::Bee;
        user_mode.screen = UserModeScreen::CharacterSelect;

        prepare_user_mode_match(&mut user_mode, &mut setup, &mut state);

        assert_eq!(
            setup.slots[USER_MODE_PLAYER_FIGHTER_ID].character,
            CharacterKind::Bee
        );
        assert_eq!(
            setup.slots[USER_MODE_BOT_FIGHTER_ID].character,
            CharacterKind::Pig
        );
        assert_eq!(
            setup.slots[USER_MODE_BOT_FIGHTER_ID].participant,
            ParticipantKind::Bot
        );
        assert_eq!(state.active_fighter_count, 2);
    }

    #[test]
    fn four_player_mode_activates_four_human_keyboard_slots() {
        let mut user_mode = UserModeState::default();
        let mut setup = LocalSetup::default();
        let mut state = MatchState::default();
        user_mode.play_mode = UserPlayMode::FourPlayers;
        user_mode.player_characters = [
            CharacterKind::Cat,
            CharacterKind::Pig,
            CharacterKind::Bee,
            CharacterKind::Chick,
        ];
        user_mode.screen = UserModeScreen::CharacterSelect;

        prepare_user_mode_match(&mut user_mode, &mut setup, &mut state);

        assert_eq!(setup.active_slots(), [true; FIGHTER_COUNT]);
        assert_eq!(setup.active_bot_count(), 0);
        assert_eq!(state.active_fighter_count, FIGHTER_COUNT);
        for fighter_id in 0..FIGHTER_COUNT {
            assert_eq!(setup.slots[fighter_id].participant, ParticipantKind::Human);
            assert_eq!(
                setup.slots[fighter_id].input,
                LocalInputAssignment::Keyboard(fighter_id)
            );
            assert_eq!(
                setup.slots[fighter_id].character,
                user_mode.player_characters[fighter_id]
            );
        }
    }

    #[test]
    fn user_mode_prepare_preserves_mixed_device_assignments() {
        let mut user_mode = UserModeState::default();
        let mut setup = LocalSetup::default();
        let mut state = MatchState::default();
        let gamepad = Entity::from_raw_u32(31).expect("valid entity");
        user_mode.play_mode = UserPlayMode::TwoPlayers;
        user_mode.input_assignments = [
            LocalInputAssignment::Gamepad(gamepad),
            LocalInputAssignment::Keyboard(3),
            LocalInputAssignment::Unassigned,
            LocalInputAssignment::Unassigned,
        ];
        user_mode.screen = UserModeScreen::CharacterSelect;

        prepare_user_mode_match(&mut user_mode, &mut setup, &mut state);

        assert_eq!(setup.slots[0].input, LocalInputAssignment::Gamepad(gamepad));
        assert_eq!(setup.slots[1].input, LocalInputAssignment::Keyboard(3));
        assert_eq!(setup.slots[2].input, LocalInputAssignment::Unassigned);
        assert_eq!(setup.slots[3].input, LocalInputAssignment::Unassigned);
    }

    #[test]
    fn four_player_default_bindings_are_complete_and_unique() {
        let bindings = PlayerKeyBindings::default();

        for player in 0..FIGHTER_COUNT {
            assert!(
                bindings
                    .bindings_for_assignment(LocalInputAssignment::Keyboard(player))
                    .is_some()
            );
            for action in ControlAction::ALL {
                assert!(bindings.key_for(player, action).is_some());
            }
        }
        assert_eq!(
            bindings.all_keys().len(),
            FIGHTER_COUNT * ControlAction::ALL.len()
        );
        assert!(!bindings.has_duplicate_keys());
    }

    #[test]
    fn controls_briefing_copy_uses_current_single_player_bindings() {
        let user_mode = UserModeState::default();
        let bindings = PlayerKeyBindings::default();

        let message = controls_briefing_message(&user_mode, &bindings, true);

        assert!(message.contains("Defeat the bot"));
        assert!(message.contains("Arena: Crown Ring"));
        assert!(message.contains("Move: Left Arrow/Right Arrow/Up Arrow/Down Arrow"));
        assert!(message.contains("Aim: Z"));
        assert!(message.contains("Heavy / Throw: X"));
        assert!(message.contains("Light / Pickup / Item: C"));
        assert!(message.contains("Jump: V"));
        assert!(message.contains("Guard: X + C"));
        assert!(message.contains("P1 press V or click to fight"));
    }

    #[test]
    fn controls_briefing_shows_xbox_assignment_and_full_layout() {
        let mut user_mode = UserModeState::default();
        user_mode.input_assignments[0] =
            LocalInputAssignment::Gamepad(Entity::from_raw_u32(41).expect("valid entity"));
        let bindings = PlayerKeyBindings::default();

        let message = controls_briefing_message(&user_mode, &bindings, true);

        assert!(message.contains("Xbox Controller"));
        assert!(message.contains("A Jump"));
        assert!(message.contains("X Light"));
        assert!(message.contains("Y Heavy"));
        assert!(message.contains("B Aim/Grab"));
        assert!(message.contains("RT Dash"));
        assert!(message.contains("LB Guard"));
        assert!(message.contains("LT Ultimate"));
        assert!(message.contains("RB Special"));
        assert!(message.contains("RB+LB Trap"));
        assert!(message.contains("P1 press A or click to fight"));
    }

    #[test]
    fn controls_briefing_copy_shows_two_player_columns_and_loading_state() {
        let mut user_mode = UserModeState::default();
        user_mode.play_mode = UserPlayMode::TwoPlayers;
        let bindings = PlayerKeyBindings::default();

        let message = controls_briefing_message(&user_mode, &bindings, false);

        assert!(message.contains("2 local players"));
        assert!(message.contains("Arena: Crown Ring"));
        assert!(message.contains("P1  Move Left Arrow/Right Arrow/Up Arrow/Down Arrow"));
        assert!(message.contains("P2  Move A/D/W/S"));
        assert!(message.contains("Loading battle"));
    }

    #[test]
    fn controls_briefing_lists_all_four_players() {
        let mut user_mode = UserModeState::default();
        user_mode.play_mode = UserPlayMode::FourPlayers;
        let bindings = PlayerKeyBindings::default();

        let message = controls_briefing_message(&user_mode, &bindings, true);

        assert!(message.contains("4 local players"));
        assert!(message.contains("P1  Move"));
        assert!(message.contains("P2  Move"));
        assert!(message.contains("P3  Move F/H/R/G"));
        assert!(message.contains("P4  Move J/L/O/K"));
    }

    #[test]
    fn battle_result_menu_waits_for_death_cinematic() {
        let mut user_mode = UserModeState::default();
        user_mode.enter_battle_result(Some(USER_MODE_PLAYER_FIGHTER_ID));

        assert_eq!(user_mode.screen(), UserModeScreen::BattleResult);
        assert!(!user_mode.result_menu_ready);
        assert!(!user_mode.tick_battle_result(USER_MODE_RESULT_MENU_DELAY_SECS - 0.1));
        assert!(!user_mode.result_menu_ready);
        assert!(user_mode.tick_battle_result(0.1));
        assert!(user_mode.result_menu_ready);
        assert_eq!(result_title_message(&user_mode), "YOU WIN");
    }

    #[test]
    fn result_choice_toggles_between_replay_and_character_select() {
        let mut user_mode = UserModeState::default();
        user_mode.enter_battle_result(Some(USER_MODE_BOT_FIGHTER_ID));

        assert_eq!(user_mode.result_choice, UserModeResultChoice::PlayAgain);
        user_mode.toggle_result_choice();
        assert_eq!(
            user_mode.result_choice,
            UserModeResultChoice::ChooseCharacter
        );
        assert_eq!(result_title_message(&user_mode), "YOU LOSE");
    }

    #[test]
    fn pointer_actions_share_routing_for_rows_arrows_confirm_back_keys_and_results() {
        let mut user_mode = UserModeState::default();
        user_mode.enter_mode_select();

        route_user_mode_action(
            &mut user_mode,
            UserModeUiAction::MainMenu(UserModeMainMenuChoice::Multiplayer),
        );
        assert_eq!(user_mode.screen(), UserModeScreen::PlayerCountSelect);
        route_user_mode_action(
            &mut user_mode,
            UserModeUiAction::PlayerCount(UserModePlayerCountChoice::TwoPlayers),
        );
        assert_eq!(user_mode.screen(), UserModeScreen::DeviceJoin);
        user_mode
            .join_assignment(LocalInputAssignment::Keyboard(0))
            .unwrap();
        user_mode
            .join_assignment(LocalInputAssignment::Keyboard(1))
            .unwrap();
        user_mode.enter_character_select();
        assert_eq!(user_mode.screen(), UserModeScreen::CharacterSelect);

        let first_character = user_mode.selected_character();
        route_user_mode_action(&mut user_mode, UserModeUiAction::Next);
        assert_ne!(user_mode.selected_character(), first_character);
        assert_eq!(
            route_user_mode_action(&mut user_mode, UserModeUiAction::Confirm),
            UserModeRoute::CharacterPlayerAdvanced
        );
        assert_eq!(
            route_user_mode_action(&mut user_mode, UserModeUiAction::Confirm),
            UserModeRoute::ArenaEntered
        );
        assert_eq!(
            route_user_mode_action(&mut user_mode, UserModeUiAction::Previous),
            UserModeRoute::ArenaChanged
        );

        user_mode.enter_key_settings();
        let capture = KeyBindingCapture {
            player: 2,
            action: ControlAction::Heavy,
        };
        route_user_mode_action(&mut user_mode, UserModeUiAction::KeyBinding(capture));
        assert_eq!(user_mode.key_capture, Some(capture));
        route_user_mode_action(&mut user_mode, UserModeUiAction::Back);
        assert_eq!(user_mode.key_capture, None);

        user_mode.player_count_choice = UserModePlayerCountChoice::TwoPlayers;
        user_mode.play_mode = UserPlayMode::TwoPlayers;
        user_mode.enter_battle_result(Some(0));
        user_mode.result_menu_ready = true;
        assert_eq!(
            route_user_mode_action(
                &mut user_mode,
                UserModeUiAction::Result(UserModeResultChoice::ChooseCharacter),
            ),
            UserModeRoute::ChooseCharacter
        );
        assert_eq!(user_mode.screen(), UserModeScreen::CharacterSelect);
        assert_eq!(user_mode.character_select_player, 0);
        assert_eq!(user_mode.play_mode, UserPlayMode::TwoPlayers);
        assert_eq!(
            user_mode.player_count_choice,
            UserModePlayerCountChoice::TwoPlayers
        );
    }

    #[test]
    fn button_selection_state_tracks_keyboard_focus_for_each_clickable_row_group() {
        let mut user_mode = UserModeState::default();
        user_mode.enter_mode_select();
        assert!(user_mode_action_selected(
            &user_mode,
            UserModeUiAction::MainMenu(UserModeMainMenuChoice::SinglePlayer)
        ));

        user_mode.enter_player_count_select();
        assert!(user_mode_action_selected(
            &user_mode,
            UserModeUiAction::PlayerCount(UserModePlayerCountChoice::TwoPlayers)
        ));

        user_mode.enter_key_settings();
        assert!(user_mode_action_selected(
            &user_mode,
            UserModeUiAction::KeyBinding(KeyBindingCapture {
                player: 0,
                action: ControlAction::Left,
            })
        ));

        user_mode.enter_battle_result(Some(0));
        assert!(user_mode_action_selected(
            &user_mode,
            UserModeUiAction::Result(UserModeResultChoice::PlayAgain)
        ));
    }

    #[test]
    fn result_sfx_matches_user_mode_outcome_after_result_delay() {
        let mut user_mode = UserModeState::default();

        user_mode.play_mode = UserPlayMode::SinglePlayer;
        user_mode.enter_battle_result(Some(USER_MODE_PLAYER_FIGHTER_ID));
        assert_eq!(result_sfx_kind(&user_mode), Some(CombatSfxKind::ResultWin));

        user_mode.enter_battle_result(Some(USER_MODE_BOT_FIGHTER_ID));
        assert_eq!(result_sfx_kind(&user_mode), Some(CombatSfxKind::ResultLose));

        user_mode.play_mode = UserPlayMode::TwoPlayers;
        user_mode.enter_battle_result(Some(USER_MODE_BOT_FIGHTER_ID));
        assert_eq!(result_sfx_kind(&user_mode), Some(CombatSfxKind::ResultWin));

        user_mode.enter_battle_result(None);
        assert_eq!(result_sfx_kind(&user_mode), None);
    }

    #[test]
    fn result_winner_uses_remaining_user_mode_stocks() {
        let mut state = MatchState::default();
        state.select_rule(USER_MODE_STOCK_RULE_INDEX);
        state.reset_for_new_match();
        state.stocks[USER_MODE_PLAYER_FIGHTER_ID] = 0;
        state.stocks[USER_MODE_BOT_FIGHTER_ID] = 1;

        assert_eq!(
            user_mode_result_winner(&state),
            Some(USER_MODE_BOT_FIGHTER_ID)
        );
    }

    #[test]
    fn four_player_result_reports_the_last_stock_holder() {
        let mut state = MatchState::default();
        state.select_rule(USER_MODE_STOCK_RULE_INDEX);
        state.set_active_slots([true; FIGHTER_COUNT]);
        state.reset_for_new_match();
        state.stocks = [0, 0, 0, 1];
        let mut user_mode = UserModeState::default();
        user_mode.play_mode = UserPlayMode::FourPlayers;
        user_mode.enter_battle_result(user_mode_result_winner(&state));

        assert_eq!(user_mode.result_winner, Some(3));
        assert_eq!(result_title_message(&user_mode), "P4 WINS");
        assert_eq!(result_sfx_kind(&user_mode), Some(CombatSfxKind::ResultWin));
    }

    #[test]
    fn user_mode_opposite_character_keeps_single_player_duels_1v1() {
        assert_eq!(
            opposite_user_mode_character(CharacterKind::Cat),
            CharacterKind::Pig
        );
        assert_eq!(
            opposite_user_mode_character(CharacterKind::Pig),
            CharacterKind::Cat
        );
        assert_eq!(
            opposite_user_mode_character(CharacterKind::Bee),
            CharacterKind::Pig
        );
        assert_eq!(
            opposite_user_mode_character(CharacterKind::Penguin),
            CharacterKind::Pig
        );
        assert_eq!(
            opposite_user_mode_character(CharacterKind::Chick),
            CharacterKind::Pig
        );
    }

    #[test]
    fn key_settings_scroll_offsets_follow_selected_row() {
        assert_eq!(key_settings_scroll_offset(0), 0.0);
        assert_eq!(
            key_settings_scroll_offset(USER_MODE_KEY_VISIBLE_ROWS - 1),
            0.0
        );
        assert_eq!(
            key_settings_scroll_offset(USER_MODE_KEY_VISIBLE_ROWS),
            USER_MODE_KEY_ROW_PITCH
        );
    }

    #[test]
    fn key_settings_enter_begins_capture_for_selected_binding() {
        let mut user_mode = UserModeState::default();
        user_mode.enter_key_settings();
        user_mode.move_key_cursor(5);

        let selected = user_mode.selected_key_target();
        user_mode.begin_key_capture();

        assert_eq!(user_mode.key_capture, Some(selected));
    }

    #[test]
    fn key_settings_left_right_moves_between_player_columns() {
        let mut user_mode = UserModeState::default();
        user_mode.enter_key_settings();
        user_mode.move_key_cursor(5);
        assert_eq!(user_mode.selected_key_target().player, 0);
        assert_eq!(user_mode.selected_key_target().action, ControlAction::Heavy);

        user_mode.move_key_column(1);
        assert_eq!(user_mode.selected_key_target().player, 1);
        assert_eq!(user_mode.selected_key_target().action, ControlAction::Heavy);

        user_mode.move_key_column(2);
        assert_eq!(user_mode.selected_key_target().player, 3);
        assert_eq!(user_mode.selected_key_target().action, ControlAction::Heavy);

        user_mode.move_key_column(-3);
        assert_eq!(user_mode.selected_key_target().player, 0);
        assert_eq!(user_mode.selected_key_target().action, ControlAction::Heavy);
    }

    #[test]
    fn key_settings_capture_applies_next_pressed_key() {
        let mut user_mode = UserModeState::default();
        let mut bindings = PlayerKeyBindings::default();
        user_mode.enter_key_settings();
        user_mode.move_key_cursor(5);
        user_mode.begin_key_capture();

        let result = user_mode
            .apply_key_capture(&mut bindings, KeyCode::KeyQ)
            .unwrap();

        assert_eq!(
            bindings.key_for(result.capture.player, result.capture.action),
            Some(KeyCode::KeyQ)
        );
        assert_eq!(result.swapped, None);
        assert_eq!(user_mode.key_capture, None);
    }

    #[test]
    fn key_settings_capture_swaps_key_used_by_other_player() {
        let mut user_mode = UserModeState::default();
        let mut bindings = PlayerKeyBindings::default();
        user_mode.enter_key_settings();
        user_mode.begin_key_capture();
        let capture = user_mode.key_capture.unwrap();

        let result = user_mode
            .apply_key_capture(&mut bindings, KeyCode::KeyA)
            .unwrap();

        assert_eq!(result.capture, capture);
        assert_eq!(
            result.swapped,
            Some(KeyBindingCapture {
                player: 1,
                action: ControlAction::Left
            })
        );
        assert_eq!(
            bindings.key_for(0, ControlAction::Left),
            Some(KeyCode::KeyA)
        );
        assert_eq!(
            bindings.key_for(1, ControlAction::Left),
            Some(KeyCode::ArrowLeft)
        );
        assert_eq!(user_mode.key_capture, None);
    }

    #[test]
    fn key_settings_capture_swaps_key_used_by_same_player() {
        let mut user_mode = UserModeState::default();
        let mut bindings = PlayerKeyBindings::default();
        user_mode.enter_key_settings();
        user_mode.move_key_cursor(5);
        user_mode.begin_key_capture();

        let result = user_mode
            .apply_key_capture(&mut bindings, KeyCode::KeyC)
            .unwrap();

        assert_eq!(
            result.capture,
            KeyBindingCapture {
                player: 0,
                action: ControlAction::Heavy
            }
        );
        assert_eq!(
            result.swapped,
            Some(KeyBindingCapture {
                player: 0,
                action: ControlAction::Light
            })
        );
        assert_eq!(
            bindings.key_for(0, ControlAction::Heavy),
            Some(KeyCode::KeyC)
        );
        assert_eq!(
            bindings.key_for(0, ControlAction::Light),
            Some(KeyCode::KeyX)
        );
        assert_eq!(user_mode.key_capture, None);
    }

    #[test]
    fn key_settings_capture_same_key_succeeds_without_swap() {
        let mut user_mode = UserModeState::default();
        let mut bindings = PlayerKeyBindings::default();
        user_mode.enter_key_settings();
        user_mode.begin_key_capture();

        let result = user_mode
            .apply_key_capture(&mut bindings, KeyCode::ArrowLeft)
            .unwrap();

        assert_eq!(
            result.capture,
            KeyBindingCapture {
                player: 0,
                action: ControlAction::Left
            }
        );
        assert_eq!(result.swapped, None);
        assert_eq!(
            bindings.key_for(0, ControlAction::Left),
            Some(KeyCode::ArrowLeft)
        );
        assert_eq!(user_mode.key_capture, None);
    }

    #[test]
    fn key_settings_reserved_capture_keeps_waiting_for_key() {
        let mut user_mode = UserModeState::default();
        let mut bindings = PlayerKeyBindings::default();
        user_mode.enter_key_settings();
        user_mode.begin_key_capture();
        let capture = user_mode.key_capture.unwrap();

        assert_eq!(
            user_mode.apply_key_capture(&mut bindings, KeyCode::Enter),
            Err("reserved")
        );

        assert_eq!(user_mode.key_capture, Some(capture));
        assert_eq!(
            bindings.key_for(capture.player, capture.action),
            Some(KeyCode::ArrowLeft)
        );
    }

    #[test]
    fn user_mode_music_has_a_cc0_asset_for_every_arena() {
        assert_eq!(
            USER_MODE_BATTLE_MUSIC_PATHS.len(),
            arena_definitions().len()
        );
        assert_eq!(
            user_mode_battle_music_path(USER_MODE_BATTLE_MUSIC_PATHS.len()),
            USER_MODE_BATTLE_MUSIC_PATHS[0]
        );

        for path in std::iter::once(USER_MODE_MENU_MUSIC_PATH)
            .chain(USER_MODE_BATTLE_MUSIC_PATHS.iter().copied())
        {
            assert!(path.starts_with("music/bgm/cc0_"));
            assert!(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("assets")
                    .join(path)
                    .is_file(),
                "missing user-mode music asset: {path}"
            );
        }
    }

    #[test]
    fn arena_music_reconciliation_keeps_only_one_matching_track() {
        let desired_arena_index = 2;
        let mut desired_track_kept = false;
        let keep = [1, 2, 2, 3].map(|music_arena_index| {
            let should_stay =
                arena_music_should_stay(music_arena_index, desired_arena_index, desired_track_kept);
            desired_track_kept |= should_stay;
            should_stay
        });

        assert_eq!(keep, [false, true, false, false]);
        assert!(desired_track_kept);
        assert_eq!(
            normalized_arena_music_index(USER_MODE_BATTLE_MUSIC_PATHS.len()),
            0
        );
    }

    #[test]
    fn dev_mode_music_yields_to_user_mode_screens_and_battles() {
        let mut user_mode = UserModeState::default();
        assert!(dev_mode_music_enabled(&user_mode));

        user_mode.enter_mode_select();
        assert!(!dev_mode_music_enabled(&user_mode));

        user_mode.screen = UserModeScreen::Dev;
        user_mode.battle_active = true;
        assert!(!dev_mode_music_enabled(&user_mode));

        user_mode.clear_battle_state();
        assert!(dev_mode_music_enabled(&user_mode));
    }

    #[test]
    fn controls_hub_cycles_and_opens_each_focused_subpage() {
        let mut user_mode = UserModeState::default();
        user_mode.enter_controls_hub();
        assert_eq!(
            user_mode.controls_hub_choice,
            ControlsHubChoice::ControllerSetup
        );

        route_user_mode_action(&mut user_mode, UserModeUiAction::Next);
        assert_eq!(
            user_mode.controls_hub_choice,
            ControlsHubChoice::ControllerTest
        );
        route_user_mode_action(&mut user_mode, UserModeUiAction::Confirm);
        assert_eq!(user_mode.screen(), UserModeScreen::ControllerTest);

        route_user_mode_action(&mut user_mode, UserModeUiAction::Back);
        user_mode.controls_hub_choice = ControlsHubChoice::KeyboardControls;
        route_user_mode_action(&mut user_mode, UserModeUiAction::Confirm);
        assert_eq!(user_mode.screen(), UserModeScreen::KeySettings);
    }

    #[test]
    fn settings_roster_prefills_match_setup_without_auto_clearing() {
        let controller = Entity::from_raw_u32(71).expect("valid entity");
        let mut user_mode = UserModeState::default();
        user_mode.input_assignments = [
            LocalInputAssignment::Gamepad(controller),
            LocalInputAssignment::Keyboard(2),
            LocalInputAssignment::Keyboard(0),
            LocalInputAssignment::Unassigned,
        ];
        user_mode.enter_settings_device_join();
        user_mode.enter_controls_hub();
        user_mode.play_mode = UserPlayMode::TwoPlayers;
        user_mode.enter_device_join();

        assert_eq!(user_mode.controller_setup_target(), 2);
        assert_eq!(
            user_mode.input_assignments[0],
            LocalInputAssignment::Gamepad(controller)
        );
        assert_eq!(
            user_mode.input_assignments[1],
            LocalInputAssignment::Keyboard(2)
        );
        assert_eq!(
            user_mode.input_assignments[2],
            LocalInputAssignment::Keyboard(0)
        );
    }

    #[test]
    fn controller_reorder_cancel_restores_snapshot_and_save_commits_order() {
        let original = [
            LocalInputAssignment::Keyboard(0),
            LocalInputAssignment::Keyboard(1),
            LocalInputAssignment::Unassigned,
            LocalInputAssignment::Unassigned,
        ];
        let mut user_mode = UserModeState::default();
        user_mode.input_assignments = original;
        user_mode.enter_settings_device_join();

        user_mode.begin_controller_reorder();
        assert_eq!(
            user_mode.input_assignments,
            [LocalInputAssignment::Unassigned; FIGHTER_COUNT]
        );
        user_mode.join_assignment(LocalInputAssignment::Keyboard(3));
        user_mode.cancel_controller_reorder();
        assert_eq!(user_mode.input_assignments, original);

        user_mode.begin_controller_reorder();
        user_mode.join_assignment(LocalInputAssignment::Keyboard(1));
        user_mode.join_assignment(LocalInputAssignment::Keyboard(0));
        user_mode.finish_controller_reorder();
        assert_eq!(
            user_mode.input_assignments,
            [
                LocalInputAssignment::Keyboard(1),
                LocalInputAssignment::Keyboard(0),
                LocalInputAssignment::Unassigned,
                LocalInputAssignment::Unassigned,
            ]
        );
    }

    #[test]
    fn clearing_controller_setup_requires_two_confirmations() {
        let mut user_mode = UserModeState::default();
        user_mode.input_assignments[0] = LocalInputAssignment::Keyboard(0);
        user_mode.enter_settings_device_join();

        assert!(!user_mode.arm_or_confirm_clear_assignments());
        assert_eq!(
            user_mode.input_assignments[0],
            LocalInputAssignment::Keyboard(0)
        );
        assert!(user_mode.arm_or_confirm_clear_assignments());
        assert_eq!(
            user_mode.input_assignments,
            [LocalInputAssignment::Unassigned; FIGHTER_COUNT]
        );
    }

    #[test]
    fn nintendo_menu_uses_a_to_confirm_and_b_to_go_back() {
        let mut confirm = Gamepad::default();
        confirm.digital_mut().press(GamepadButton::East);
        let mut tracker = MenuDirectionRepeat::default();
        assert_eq!(
            gamepad_user_mode_action(
                UserModeScreen::ControlsHub,
                &confirm,
                ControllerFamily::Nintendo,
                0.0,
                &mut tracker,
            ),
            Some(UserModeUiAction::Confirm)
        );

        let mut back = Gamepad::default();
        back.digital_mut().press(GamepadButton::South);
        assert_eq!(
            gamepad_user_mode_action(
                UserModeScreen::ControlsHub,
                &back,
                ControllerFamily::Nintendo,
                0.0,
                &mut tracker,
            ),
            Some(UserModeUiAction::Back)
        );
    }

    #[test]
    fn controller_test_copy_uses_live_family_buttons_and_deadzone() {
        use bevy::ecs::system::SystemState;

        let mut world = World::new();
        let mut gamepad = Gamepad::default();
        gamepad.digital_mut().press(GamepadButton::South);
        let entity = world
            .spawn((
                gamepad,
                ControllerDeviceInfo {
                    display_name: "Switch Pro Controller".to_string(),
                    family: ControllerFamily::Nintendo,
                    vendor_id: Some(0x057e),
                    product_id: None,
                    connected: true,
                    haptics: crate::controller_haptics::HapticAvailability::Supported,
                },
            ))
            .id();
        let mut user_mode = UserModeState::default();
        user_mode.input_assignments[0] = LocalInputAssignment::Gamepad(entity);
        user_mode.enter_controller_test();
        user_mode.controller_test_active = Some(entity);

        let mut system_state: SystemState<(
            Query<(Entity, &Gamepad)>,
            Query<&ControllerDeviceInfo>,
        )> = SystemState::new(&mut world);
        let (gamepads, metadata) = system_state.get(&world);
        let message = controller_test_message(&user_mode, &gamepads, &metadata);

        assert!(message.contains("Nintendo"));
        assert!(message.contains("Pressed: B"));
        assert!(message.contains("B Jump"));
        assert!(message.contains("Movement deadzone: 0.20"));
    }

    #[test]
    fn match_ready_requires_every_required_assignment_to_be_connected() {
        use bevy::ecs::system::SystemState;

        let mut world = World::new();
        let controller = world.spawn(Gamepad::default()).id();
        let mut user_mode = UserModeState::default();
        user_mode.play_mode = UserPlayMode::TwoPlayers;
        user_mode.input_assignments = [
            LocalInputAssignment::Gamepad(controller),
            LocalInputAssignment::Keyboard(0),
            LocalInputAssignment::Unassigned,
            LocalInputAssignment::Unassigned,
        ];
        user_mode.enter_device_join();

        let mut system_state: SystemState<Query<(Entity, &Gamepad)>> = SystemState::new(&mut world);
        {
            let gamepads = system_state.get(&world);
            assert!(controller_setup_can_finish(&user_mode, &gamepads));
        }
        world.entity_mut(controller).remove::<Gamepad>();
        {
            let gamepads = system_state.get(&world);
            assert!(!controller_setup_can_finish(&user_mode, &gamepads));
        }
    }

    #[test]
    fn nintendo_briefing_uses_physical_gameplay_button_labels() {
        let controller = Entity::from_raw_u32(72).expect("valid entity");
        let mut user_mode = UserModeState::default();
        user_mode.input_assignments[0] = LocalInputAssignment::Gamepad(controller);
        let message = controls_briefing_message_for_family(
            &user_mode,
            &PlayerKeyBindings::default(),
            true,
            |_| ControllerFamily::Nintendo,
        );

        assert!(message.contains("Nintendo Controller"));
        assert!(message.contains("B Jump"));
        assert!(message.contains("Y Light"));
        assert!(message.contains("A Aim/Grab"));
        assert!(message.contains("press A"));
    }

    #[test]
    fn back_cancels_key_reset_confirmation_before_leaving_keyboard_settings() {
        let mut user_mode = UserModeState::default();
        user_mode.enter_key_settings();
        user_mode.key_reset_confirmation = true;

        route_user_mode_action(&mut user_mode, UserModeUiAction::Back);

        assert_eq!(user_mode.screen(), UserModeScreen::KeySettings);
        assert!(!user_mode.key_reset_confirmation);
    }

    #[test]
    fn user_mode_music_assets_are_decodable_by_the_enabled_bevy_audio_formats() {
        use bevy::audio::{AudioSource, Decodable};

        for path in std::iter::once(USER_MODE_MENU_MUSIC_PATH)
            .chain(USER_MODE_BATTLE_MUSIC_PATHS.iter().copied())
        {
            let bytes = std::fs::read(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("assets")
                    .join(path),
            )
            .unwrap_or_else(|error| panic!("failed to read user-mode music asset {path}: {error}"));
            let source = AudioSource {
                bytes: bytes.into(),
            };
            assert!(
                source.decoder().next().is_some(),
                "user-mode music asset has no decodable samples: {path}"
            );
        }
    }
}
