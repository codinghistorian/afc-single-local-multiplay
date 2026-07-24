use bevy::camera::{RenderTarget, visibility::RenderLayers};
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use bevy::scene::SceneInstanceReady;
use bevy::time::Real;
use bevy::ui::UiTargetCamera;

use crate::arena::ARENA_PREVIEW_RENDER_LAYER;
use crate::arena_defs::{ActiveArena, arena_definitions};
use crate::bot::start_bot_combat_ai;
use crate::camera::{ScreenLook, ScreenLookTransition, UiCamera, begin_screen_look_transition};
use crate::characters::{
    CharacterKind, CharacterMoveCatalog, character_label, character_scene_model,
};
use crate::combat::HitEffects;
use crate::combat_sfx::{CombatSfxCue, CombatSfxKind};
use crate::components::{BotBrain, ControlAction, Controller, Fighter, PlayerKeyBindings};
use crate::constants::FIGHTER_COUNT;
use crate::game_state::{
    LocalSetup, MatchAnnouncements, MatchPhase, MatchState, reconcile_fighter_control_from_setup,
};
use crate::match_presentation::{
    MatchPresentationPolicy, MatchPresentationTransient, PresentationMusicTrack,
    PresentationResultSfx, PresentedResultSfxHistory,
};
use crate::native_online::NativeOnlineRuntime;
use crate::native_online_app::{NativeOnlineApplication, OverlayUnavailableSurface};
use crate::online_client::{EmbeddedOnlineClientPhase, EmbeddedOnlineClientStatus};
use crate::online_failure::{OnlineFailureCode, OnlineRecoveryAction};
use crate::release_identity::current_release_identity;
use crate::steam_platform::{
    MAX_STEAM_INPUT_CONTROLLERS, SteamInputSnapshot, SteamMenuAction, SteamMenuInputMask,
};

const USER_MODE_MENU_MUSIC_PATH: &str = "music/bgm/cc0_menu_menu_music.ogg";
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

/// Client-only visual time scaling.
///
/// This intentionally does not modify Bevy's global `Time<Virtual>` resource:
/// the fixed simulation clock is accumulated from virtual time, so changing it
/// would slow the authority/network timeline. Presentation systems that need
/// the result-screen effect opt into this scale explicitly.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct PresentationTimeScale(f32);

impl Default for PresentationTimeScale {
    fn default() -> Self {
        Self(1.0)
    }
}

impl PresentationTimeScale {
    pub fn set(&mut self, scale: f32) {
        self.0 = if scale.is_finite() && scale >= 0.0 {
            scale
        } else {
            1.0
        };
    }

    pub fn reset(&mut self) {
        self.0 = 1.0;
    }

    #[cfg(test)]
    pub const fn value(self) -> f32 {
        self.0
    }

    pub fn scale_delta(self, delta_seconds: f32) -> f32 {
        delta_seconds * self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserModeScreen {
    Dev,
    Start,
    ModeSelect,
    Online,
    PlayerCountSelect,
    KeySettings,
    CharacterSelect,
    ArenaSelect,
    ControlsBriefing,
    BattleResult,
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
    LocalMultiplayer,
    Online,
    Settings,
}

impl UserModeMainMenuChoice {
    fn previous(self) -> Self {
        match self {
            Self::SinglePlayer => Self::Settings,
            Self::LocalMultiplayer => Self::SinglePlayer,
            Self::Online => Self::LocalMultiplayer,
            Self::Settings => Self::Online,
        }
    }

    fn next(self) -> Self {
        match self {
            Self::SinglePlayer => Self::LocalMultiplayer,
            Self::LocalMultiplayer => Self::Online,
            Self::Online => Self::Settings,
            Self::Settings => Self::SinglePlayer,
        }
    }
}

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
    Previous,
    Next,
    PreviousColumn,
    NextColumn,
    Confirm,
    Back,
    ControllerBack,
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
    ReturnToStart,
    #[cfg(not(target_arch = "wasm32"))]
    ExitToDev,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UserModeControllerIntent {
    None,
    Dispatch(UserModeUiAction),
    OpenBindings(usize),
}

#[derive(Resource, Clone, Debug)]
pub struct UserModeState {
    screen: UserModeScreen,
    play_mode: UserPlayMode,
    main_menu_choice: UserModeMainMenuChoice,
    player_count_choice: UserModePlayerCountChoice,
    player_characters: [CharacterKind; FIGHTER_COUNT],
    arena_index: usize,
    character_select_player: usize,
    key_settings_cursor: usize,
    key_capture: Option<KeyBindingCapture>,
    controls_briefing_seen: bool,
    battle_music_pending: bool,
    battle_bot_ai_pending: bool,
    battle_active: bool,
    result_elapsed: f32,
    result_menu_ready: bool,
    result_choice: UserModeResultChoice,
    result_winner: Option<usize>,
    controller_menu_held: [SteamMenuInputMask; MAX_STEAM_INPUT_CONTROLLERS],
    controller_menu_screen: Option<UserModeScreen>,
    pending_controller_action: Option<UserModeUiAction>,
    /// Monotonic local request identity. Rendered `MatchState` may still show
    /// the prior projected result when a rematch is requested.
    match_request_revision: u64,
}

#[derive(Resource)]
pub struct UserModeGameplayScene {
    loaded: bool,
    warmup_remaining: f32,
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

#[cfg(not(any(feature = "native", target_arch = "wasm32")))]
pub fn should_spawn_web_gameplay_scene() -> bool {
    false
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn mark_web_gameplay_scene_loaded() {}

#[cfg(not(any(feature = "native", target_arch = "wasm32")))]
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
            arena_index: 0,
            character_select_player: 0,
            key_settings_cursor: 0,
            key_capture: None,
            controls_briefing_seen: false,
            battle_music_pending: false,
            battle_bot_ai_pending: false,
            battle_active: false,
            result_elapsed: 0.0,
            result_menu_ready: false,
            result_choice: UserModeResultChoice::PlayAgain,
            result_winner: None,
            controller_menu_held: [SteamMenuInputMask::NONE; MAX_STEAM_INPUT_CONTROLLERS],
            controller_menu_screen: None,
            pending_controller_action: None,
            match_request_revision: 0,
        }
    }
}

fn default_user_mode_screen() -> UserModeScreen {
    #[cfg(target_arch = "wasm32")]
    {
        UserModeScreen::ModeSelect
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        native_user_mode_initial_screen(cfg!(debug_assertions))
    }
}

#[cfg(not(target_arch = "wasm32"))]
const fn native_user_mode_initial_screen(debug_build: bool) -> UserModeScreen {
    if debug_build {
        UserModeScreen::Dev
    } else {
        UserModeScreen::Start
    }
}

impl UserModeState {
    pub fn active(&self) -> bool {
        self.screen != UserModeScreen::Dev
    }

    pub fn screen(&self) -> UserModeScreen {
        self.screen
    }

    /// Places an automated graphical performance fixture in the same direct
    /// dev sandbox used by local gameplay, without disabling gameplay HUD,
    /// camera, arena audio, VFX, or simulation. This only removes menu-flow
    /// churn; the performance plugin calls it solely when a scenario is named.
    pub(crate) fn force_performance_dev_mode(&mut self) {
        self.screen = UserModeScreen::Dev;
        self.key_capture = None;
        self.clear_battle_state();
    }

    pub fn online_active(&self) -> bool {
        self.screen == UserModeScreen::Online
    }

    pub(crate) fn enter_online(&mut self) {
        self.screen = UserModeScreen::Online;
        self.key_capture = None;
        self.clear_battle_state();
    }

    pub(crate) fn leave_online(&mut self) {
        if self.screen == UserModeScreen::Online {
            self.enter_mode_select();
        }
    }

    /// True while the player-facing match flow needs a canonical online/local
    /// authority session. Dev sandbox play deliberately remains on the direct
    /// local schedule.
    pub fn network_match_requested(&self) -> bool {
        self.battle_music_pending
            || self.battle_active
            || self.screen == UserModeScreen::BattleResult
    }

    pub const fn match_request_revision(&self) -> u64 {
        self.match_request_revision
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

    pub fn restricts_bot_special_inputs(&self) -> bool {
        self.battle_active
            || self.battle_music_pending
            || self.screen == UserModeScreen::ControlsBriefing
            || self.screen == UserModeScreen::BattleResult
    }

    pub fn single_player_camera_target_id(&self) -> Option<usize> {
        (self.play_mode == UserPlayMode::SinglePlayer
            && (self.battle_active
                || self.battle_music_pending
                || self.screen == UserModeScreen::BattleResult))
            .then_some(USER_MODE_PLAYER_FIGHTER_ID)
    }

    #[cfg(any(test, all(feature = "native", not(target_arch = "wasm32"))))]
    pub fn blocks_practice_health_refill(&self) -> bool {
        self.battle_active || self.screen == UserModeScreen::BattleResult
    }

    fn enter_fresh_mode_select(&mut self) {
        self.screen = UserModeScreen::ModeSelect;
        self.play_mode = UserPlayMode::SinglePlayer;
        self.main_menu_choice = UserModeMainMenuChoice::SinglePlayer;
        self.player_count_choice = UserModePlayerCountChoice::TwoPlayers;
        self.player_characters = USER_MODE_DEFAULT_CHARACTERS;
        self.arena_index = 0;
        self.character_select_player = 0;
        self.key_settings_cursor = 0;
        self.key_capture = None;
        self.controls_briefing_seen = false;
        self.clear_battle_state();
    }

    fn return_to_start(&mut self) {
        self.screen = UserModeScreen::Start;
        self.key_capture = None;
        self.clear_battle_state();
    }

    fn take_pending_controller_action(&mut self) -> Option<UserModeUiAction> {
        self.pending_controller_action.take()
    }

    fn controller_menu_intent(
        &mut self,
        steam_input: SteamInputSnapshot,
    ) -> UserModeControllerIntent {
        let screen_changed = self.controller_menu_screen != Some(self.screen);
        if screen_changed {
            self.controller_menu_screen = Some(self.screen);
        }

        let mut pressed = SteamMenuInputMask::NONE;
        let mut binding_ordinal = None;
        for local_ordinal in 0..MAX_STEAM_INPUT_CONTROLLERS {
            let controller = steam_input.controllers[local_ordinal];
            let held = controller.menu_held;
            if screen_changed {
                self.controller_menu_held[local_ordinal] = held;
                continue;
            }
            let just_pressed = held.without(self.controller_menu_held[local_ordinal]);
            self.controller_menu_held[local_ordinal] = held;
            pressed = pressed.union(just_pressed);
            if binding_ordinal.is_none()
                && controller.connected()
                && just_pressed.contains(SteamMenuAction::OpenBindings)
            {
                binding_ordinal = Some(local_ordinal);
            }
        }

        if screen_changed {
            return UserModeControllerIntent::None;
        }
        if let Some(local_ordinal) = binding_ordinal {
            return UserModeControllerIntent::OpenBindings(local_ordinal);
        }
        controller_user_mode_action(self, pressed)
            .map(UserModeControllerIntent::Dispatch)
            .unwrap_or(UserModeControllerIntent::None)
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

    fn enter_mode_select(&mut self) {
        self.screen = UserModeScreen::ModeSelect;
        self.key_capture = None;
        self.clear_battle_state();
    }

    fn return_to_character_select_player(&mut self, player: usize) {
        self.screen = UserModeScreen::CharacterSelect;
        self.character_select_player = player.min(self.play_mode.human_player_count() - 1);
        self.key_capture = None;
        self.clear_battle_state();
    }

    fn enter_key_settings(&mut self) {
        self.screen = UserModeScreen::KeySettings;
        self.key_capture = None;
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

    /// Presents a failed embedded authority as a terminal match outcome while
    /// retaining the current request revision. Existing result actions remain
    /// the only recovery path: Play Again creates a fresh revision and Choose
    /// Character explicitly cancels this request before a later new match.
    pub(crate) fn present_embedded_authority_failure(&mut self) {
        self.enter_battle_result(None);
        self.battle_music_pending = false;
        self.battle_bot_ai_pending = false;
        self.battle_active = false;
        self.result_elapsed = USER_MODE_RESULT_MENU_DELAY_SECS;
        self.result_menu_ready = true;
    }

    #[cfg(test)]
    pub(crate) fn request_embedded_match_for_test(&mut self) {
        self.match_request_revision = self
            .match_request_revision
            .checked_add(1)
            .expect("test match request revision exhausted");
        self.controls_briefing_seen = true;
        self.exit_to_battle();
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
        if self.character_select_player + 1 < self.play_mode.human_player_count() {
            self.character_select_player += 1;
            return false;
        }
        true
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
            user_mode.enter_character_select();
        }
        UserModeMainMenuChoice::LocalMultiplayer => user_mode.enter_player_count_select(),
        UserModeMainMenuChoice::Online => user_mode.enter_online(),
        UserModeMainMenuChoice::Settings => user_mode.enter_key_settings(),
    }
}

fn activate_player_count_choice(user_mode: &mut UserModeState, choice: UserModePlayerCountChoice) {
    user_mode.player_count_choice = choice;
    user_mode.play_mode = choice.play_mode();
    user_mode.enter_character_select();
}

fn controller_user_mode_action(
    user_mode: &UserModeState,
    pressed: SteamMenuInputMask,
) -> Option<UserModeUiAction> {
    if pressed.contains(SteamMenuAction::Back) {
        return match user_mode.screen {
            UserModeScreen::ModeSelect => Some(UserModeUiAction::ControllerBack),
            UserModeScreen::Dev | UserModeScreen::Start | UserModeScreen::Online => None,
            _ => Some(UserModeUiAction::Back),
        };
    }
    if user_mode.key_capture.is_some() {
        return None;
    }

    let up = pressed.contains(SteamMenuAction::Up);
    let down = pressed.contains(SteamMenuAction::Down);
    let left = pressed.contains(SteamMenuAction::Left);
    let right = pressed.contains(SteamMenuAction::Right);
    let accept = pressed.contains(SteamMenuAction::Accept);

    match user_mode.screen {
        UserModeScreen::Start => accept.then_some(UserModeUiAction::Confirm),
        UserModeScreen::ModeSelect | UserModeScreen::PlayerCountSelect => {
            let previous = up || left;
            let next = down || right;
            if previous ^ next {
                Some(if previous {
                    UserModeUiAction::Previous
                } else {
                    UserModeUiAction::Next
                })
            } else {
                accept.then_some(UserModeUiAction::Confirm)
            }
        }
        UserModeScreen::CharacterSelect | UserModeScreen::ArenaSelect => {
            let previous = left || up;
            let next = right || down;
            if previous ^ next {
                Some(if previous {
                    UserModeUiAction::Previous
                } else {
                    UserModeUiAction::Next
                })
            } else {
                accept.then_some(UserModeUiAction::Confirm)
            }
        }
        UserModeScreen::KeySettings => {
            if up ^ down {
                Some(if up {
                    UserModeUiAction::Previous
                } else {
                    UserModeUiAction::Next
                })
            } else if left ^ right {
                Some(if left {
                    UserModeUiAction::PreviousColumn
                } else {
                    UserModeUiAction::NextColumn
                })
            } else {
                // Steam controller remapping is owned by the Steam binding
                // panel; accepting here would start a keyboard-only capture.
                None
            }
        }
        UserModeScreen::ControlsBriefing => accept.then_some(UserModeUiAction::Confirm),
        UserModeScreen::BattleResult if user_mode.result_menu_ready => {
            if left ^ right || up ^ down {
                Some(UserModeUiAction::Next)
            } else {
                accept.then_some(UserModeUiAction::Confirm)
            }
        }
        UserModeScreen::Dev | UserModeScreen::Online | UserModeScreen::BattleResult => None,
    }
}

fn route_user_mode_action(
    user_mode: &mut UserModeState,
    mut action: UserModeUiAction,
) -> UserModeRoute {
    if action == UserModeUiAction::ControllerBack {
        if user_mode.screen == UserModeScreen::ModeSelect {
            user_mode.return_to_start();
            return UserModeRoute::ReturnToStart;
        }
        action = UserModeUiAction::Back;
    }
    if let UserModeUiAction::Back = action {
        if user_mode.key_capture.is_some() {
            user_mode.cancel_key_capture();
            return UserModeRoute::None;
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
                #[cfg(not(any(feature = "native", target_arch = "wasm32")))]
                {
                    UserModeRoute::ExitToDev
                }
            }
            UserModeScreen::PlayerCountSelect | UserModeScreen::KeySettings => {
                user_mode.enter_mode_select();
                UserModeRoute::None
            }
            UserModeScreen::CharacterSelect if user_mode.character_select_player > 0 => {
                user_mode.character_select_player -= 1;
                UserModeRoute::None
            }
            UserModeScreen::CharacterSelect => {
                if user_mode.play_mode.is_single_player() {
                    user_mode.enter_mode_select();
                } else {
                    user_mode.enter_player_count_select();
                }
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
pub(crate) struct UserModeReleaseIdentityText;

#[derive(Component)]
pub(crate) struct UserModeMainMenuPanel;

#[derive(Component)]
pub(crate) struct UserModePlayerCountPanel;

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

fn user_mode_release_identity_text() -> String {
    current_release_identity().short_ui_label()
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
    #[cfg(not(any(feature = "native", target_arch = "wasm32")))]
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
                        Text::new("Press A / Enter or click to start"),
                        TextFont {
                            font_size: 28.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.82, 0.78, 0.68)),
                    ),
                    (
                        UserModeReleaseIdentityText,
                        Text::new(user_mode_release_identity_text()),
                        TextFont {
                            font_size: 14.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.5, 0.49, 0.46)),
                    ),
                ],
            ),
            (
                UserModeMainMenuPanel,
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
                        "LOCAL MULTIPLAYER",
                        UserModeUiAction::MainMenu(UserModeMainMenuChoice::LocalMultiplayer),
                        Val::Px(340.0),
                        58.0,
                        24.0,
                    ),
                    user_mode_action_button(
                        "ONLINE",
                        UserModeUiAction::MainMenu(UserModeMainMenuChoice::Online),
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
                        Text::new("D-pad or W/S choose  |  A / Enter confirm  |  B back"),
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
                        Text::new("KEY SETTINGS"),
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
                        Text::new("PC KEYBOARD GAME"),
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
    if let Some(ui_camera) = ui_camera {
        user_mode_root.insert(UiTargetCamera(ui_camera));
    }
}

/// Samples Steam's current menu-action values into an edge-latched user-mode
/// action. The online menu maintains an independent latch because its focus
/// model and screen transitions are separate from the local shell.
pub fn sample_user_mode_steam_input(
    time: Res<Time<Real>>,
    mut runtime: NonSendMut<NativeOnlineRuntime>,
    mut online_application: NonSendMut<NativeOnlineApplication>,
    mut user_mode: ResMut<UserModeState>,
) {
    user_mode.pending_controller_action = None;
    match user_mode.controller_menu_intent(runtime.steam_input_snapshot()) {
        UserModeControllerIntent::Dispatch(action) => {
            user_mode.pending_controller_action = Some(action);
        }
        UserModeControllerIntent::OpenBindings(local_ordinal) if !user_mode.online_active() => {
            if let Ok(status) = runtime.show_steam_input_binding_panel(local_ordinal) {
                let now_ms = time.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
                online_application.observe_overlay_request(
                    OverlayUnavailableSurface::ControllerBindings,
                    status,
                    now_ms,
                );
            }
        }
        // The online application owns this call while its panel is visible;
        // both latches still observe every frame to prevent transition bleed.
        UserModeControllerIntent::OpenBindings(_) | UserModeControllerIntent::None => {}
    }
}

pub fn handle_user_mode_input(
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    action_buttons: Query<(&Interaction, &UserModeUiAction), Changed<Interaction>>,
    asset_server: Res<AssetServer>,
    mut user_mode: ResMut<UserModeState>,
    mut key_bindings: ResMut<PlayerKeyBindings>,
    mut setup: ResMut<LocalSetup>,
    mut state: ResMut<MatchState>,
    mut active_arena: ResMut<ActiveArena>,
    gameplay_scene: Res<UserModeGameplayScene>,
    mut announcements: ResMut<MatchAnnouncements>,
    music: Query<Entity, With<UserModeMusic>>,
    mut presentation_time_scale: ResMut<PresentationTimeScale>,
    mut screen_look: ResMut<ScreenLook>,
    mut screen_transition: ResMut<ScreenLookTransition>,
    mut commands: Commands,
) {
    let controller_action = user_mode.take_pending_controller_action();
    if user_mode.screen == UserModeScreen::Start {
        if keys.just_pressed(KeyCode::Enter)
            || keys.just_pressed(KeyCode::Space)
            || buttons.just_pressed(MouseButton::Left)
            || controller_action == Some(UserModeUiAction::Confirm)
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
                &mut presentation_time_scale,
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
            active_arena.select(state.arena_index);
            announce_user_mode_match_flow(flow, &setup, &mut announcements);
            return;
        }
    }

    let pointer_action = action_buttons.iter().find_map(|(interaction, action)| {
        (*interaction == Interaction::Pressed).then_some(*action)
    });

    if user_mode.key_capture.is_some() {
        if pointer_action == Some(UserModeUiAction::Back)
            || keys.just_pressed(KeyCode::Escape)
            || controller_action == Some(UserModeUiAction::Back)
        {
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
                    announcements.show(message, 1.0);
                }
                Err("reserved") => announcements.show("Reserved key", 1.0),
                _ => announcements.show("Cannot bind key", 1.0),
            }
        }
        return;
    }

    if user_mode.screen == UserModeScreen::KeySettings && keys.just_pressed(KeyCode::KeyR) {
        *key_bindings = PlayerKeyBindings::default();
        announcements.show("Controls reset", 1.0);
    }

    #[cfg(target_arch = "wasm32")]
    let web_start_requested =
        user_mode.screen == UserModeScreen::ControlsBriefing && web_battle_start_signal_requested();
    #[cfg(not(target_arch = "wasm32"))]
    let web_start_requested = false;

    let action = pointer_action
        .or_else(|| keyboard_user_mode_action(&user_mode, &keys))
        .or(controller_action)
        .or_else(|| web_start_requested.then_some(UserModeUiAction::Confirm));
    let Some(action) = action else {
        return;
    };

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
            active_arena.select(user_mode.arena_index);
            announcements.show("Choose arena", 0.9);
        }
        UserModeRoute::ArenaChanged => active_arena.select(user_mode.arena_index),
        UserModeRoute::PrepareMatch => {
            stop_user_mode_music(&mut commands, &music);
            let flow = prepare_user_mode_match(&mut user_mode, &mut setup, &mut state);
            active_arena.select(state.arena_index);
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
                &mut presentation_time_scale,
                &mut screen_look,
                &mut screen_transition,
            );
            let flow = prepare_user_mode_match(&mut user_mode, &mut setup, &mut state);
            active_arena.select(state.arena_index);
            announce_user_mode_match_flow(flow, &setup, &mut announcements);
        }
        UserModeRoute::ChooseCharacter => {
            reset_user_mode_presentation(
                &mut presentation_time_scale,
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
        UserModeRoute::ReturnToStart => {
            stop_user_mode_music(&mut commands, &music);
            reset_user_mode_presentation(
                &mut presentation_time_scale,
                &mut screen_look,
                &mut screen_transition,
            );
            announcements.show("", 0.0);
        }
        #[cfg(not(target_arch = "wasm32"))]
        UserModeRoute::ExitToDev => {
            #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
            {
                stop_user_mode_music(&mut commands, &music);
                reset_user_mode_presentation(
                    &mut presentation_time_scale,
                    &mut screen_look,
                    &mut screen_transition,
                );
                user_mode.exit_to_dev();
                announcements.show("Dev setup", 0.8);
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

#[cfg(not(any(feature = "native", target_arch = "wasm32")))]
pub fn sync_web_battle_status() {}

pub fn sync_user_mode_battle_result(
    time: Res<Time<Real>>,
    mut presentation_time_scale: ResMut<PresentationTimeScale>,
    mut user_mode: ResMut<UserModeState>,
    mut feedback: ResMut<HitEffects>,
    state: Res<MatchState>,
    mut screen_look: ResMut<ScreenLook>,
    mut screen_transition: ResMut<ScreenLookTransition>,
    simulation_drive: Res<crate::simulation::SimulationDriveMode>,
) {
    if *simulation_drive == crate::simulation::SimulationDriveMode::ExternalProjection {
        return;
    }
    if user_mode.battle_active
        && state.phase == MatchPhase::Results
        && user_mode.screen != UserModeScreen::BattleResult
    {
        user_mode.enter_battle_result(user_mode_result_winner(&state));
        presentation_time_scale.set(USER_MODE_DEATH_SLOW_MOTION_SCALE);
        begin_screen_look_transition(
            &mut screen_look,
            &mut screen_transition,
            ScreenLook::NoirCrime,
            USER_MODE_NOIR_FADE_SECS,
        );
    }

    if user_mode.tick_battle_result(time.delta_secs()) {
        presentation_time_scale.reset();
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
    simulation_drive: Res<crate::simulation::SimulationDriveMode>,
) {
    if *simulation_drive == crate::simulation::SimulationDriveMode::ExternalProjection {
        return;
    }
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
    active_arena: Res<ActiveArena>,
    arena_music: Query<(Entity, &ArenaMusic)>,
    mut commands: Commands,
    simulation_drive: Res<crate::simulation::SimulationDriveMode>,
) {
    if *simulation_drive == crate::simulation::SimulationDriveMode::ExternalProjection {
        return;
    }
    if !dev_mode_music_enabled(&user_mode) {
        return;
    }

    reconcile_arena_music(
        &mut commands,
        &asset_server,
        &arena_music,
        active_arena.index(),
    );
}

pub fn sync_user_mode_battle_bot(
    mut user_mode: ResMut<UserModeState>,
    state: Res<MatchState>,
    scene: Res<UserModeGameplayScene>,
    mut bots: Query<(&Fighter, &mut BotBrain)>,
    simulation_drive: Res<crate::simulation::SimulationDriveMode>,
) {
    if *simulation_drive == crate::simulation::SimulationDriveMode::ExternalProjection {
        return;
    }
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
    simulation_drive: Res<crate::simulation::SimulationDriveMode>,
) {
    if *simulation_drive == crate::simulation::SimulationDriveMode::ExternalProjection {
        return;
    }
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

pub fn update_user_mode_ui(
    user_mode: Res<UserModeState>,
    embedded_online: Res<EmbeddedOnlineClientStatus>,
    bindings: Res<PlayerKeyBindings>,
    mut roots: Query<(&mut Node, &mut BackgroundColor), With<UserModeRoot>>,
    mut back_buttons: Query<&mut Node, (With<UserModeBackButton>, Without<UserModeRoot>)>,
    mut panels: Query<
        (
            &mut Node,
            Option<&UserModeStartPanel>,
            Option<&UserModeMainMenuPanel>,
            Option<&UserModePlayerCountPanel>,
            Option<&UserModeCharacterSelectPanel>,
            Option<&UserModeArenaSelectPanel>,
            Option<&UserModeKeySettingsPanel>,
            Option<&UserModeResultPanel>,
        ),
        (
            Without<UserModeRoot>,
            Without<UserModeBackButton>,
            Or<(
                With<UserModeStartPanel>,
                With<UserModeMainMenuPanel>,
                With<UserModePlayerCountPanel>,
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
            Or<(
                With<UserModeChoiceText>,
                With<UserModeCharacterTitleText>,
                With<UserModeKeySettingsPromptText>,
                With<UserModeKeySettingsRowText>,
                With<UserModeResultText>,
            )>,
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
    for (mut node, start, main, player_count, character, arena, keys, result) in &mut panels {
        let visible = (start.is_some() && user_mode.screen() == UserModeScreen::Start)
            || (main.is_some() && user_mode.screen() == UserModeScreen::ModeSelect)
            || (player_count.is_some() && user_mode.screen() == UserModeScreen::PlayerCountSelect)
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
            **text = embedded_authority_failure_title(&embedded_online)
                .unwrap_or_else(|| result_title_message(&user_mode));
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

fn embedded_authority_failure_title(status: &EmbeddedOnlineClientStatus) -> Option<String> {
    if status.phase != EmbeddedOnlineClientPhase::Failed {
        return None;
    }
    let failure = status.failure?;
    debug_assert_eq!(
        failure.recovery,
        OnlineRecoveryAction::Retry,
        "embedded authority failure UI currently exposes Play Again"
    );
    let (title, reason) = match failure.code {
        OnlineFailureCode::InternalFailure => (
            "MATCH COULD NOT START",
            "The local match authority could not start safely.",
        ),
        OnlineFailureCode::AuthorityLost => (
            "MATCH ENDED",
            "The local match authority stopped; gameplay did not continue locally.",
        ),
        OnlineFailureCode::SynchronizationFailed => {
            ("MATCH ENDED", "The match could not synchronize safely.")
        }
        _ => (
            "MATCH ENDED",
            "The match authority ended the session safely.",
        ),
    };
    Some(format!("{title}\n{reason}\nPLAY AGAIN or choose character"))
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
        **text = controls_briefing_message(&user_mode, &bindings, scene.ready_for_battle());
    }
}

fn user_mode_background_alpha(user_mode: &UserModeState) -> f32 {
    match user_mode.screen() {
        UserModeScreen::Start
        | UserModeScreen::ModeSelect
        | UserModeScreen::PlayerCountSelect
        | UserModeScreen::KeySettings
        | UserModeScreen::CharacterSelect
        | UserModeScreen::ArenaSelect
        | UserModeScreen::ControlsBriefing => 1.0,
        UserModeScreen::BattleResult if user_mode.result_menu_ready => 0.58,
        UserModeScreen::BattleResult => 0.0,
        UserModeScreen::Dev | UserModeScreen::Online => 0.0,
    }
}

#[cfg(any(test, all(feature = "native", not(target_arch = "wasm32"))))]
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
        UserModeScreen::ModeSelect | UserModeScreen::PlayerCountSelect => {
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
        UserModeScreen::CharacterSelect | UserModeScreen::ArenaSelect => {
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
        UserModeScreen::CharacterSelect => character_select_message(user_mode.selected_character()),
        UserModeScreen::ArenaSelect => arena_select_message(user_mode.arena_index),
        _ => String::new(),
    }
}

fn character_select_title_message(user_mode: &UserModeState) -> String {
    format!(
        "P{} SELECT A CHARACTER",
        user_mode.character_select_player + 1
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
    "Up/Down row  |  Left/Right player  |  Enter change key  |  R reset  |  Esc back".to_string()
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
    let status = if ready_for_battle {
        "Press Enter or click to fight"
    } else {
        "Loading battle..."
    };
    let arena = arena_definitions()[user_mode
        .arena_index
        .min(arena_definitions().len().saturating_sub(1))]
    .name;

    if user_mode.play_mode.is_single_player() {
        return format!(
            "Defeat the bot.\nArena: {}\n\n{}\n\nDash: double-tap movement\nGuard: {} + {}\n\n{}",
            arena,
            controls_player_message(0, bindings),
            control_key_label(bindings, 0, ControlAction::Heavy),
            control_key_label(bindings, 0, ControlAction::Light),
            status,
        );
    }

    let player_count = user_mode.play_mode.human_player_count();
    let player_controls = (0..player_count)
        .map(|player| controls_player_compact_message(player, bindings))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{player_count} players share this keyboard.\nArena: {arena}\n\n{player_controls}\n\nDash: double-tap movement  |  Guard: Heavy + Light\n\n{status}"
    )
}

fn controls_player_message(player: usize, bindings: &PlayerKeyBindings) -> String {
    format!(
        "P{}\nMove: {}/{}/{}/{}\nHold Aim / Tap Grab: {}\nHeavy / Throw: {}\nLight / Pickup / Item: {}\nJump: {}",
        player + 1,
        control_key_label(bindings, player, ControlAction::Left),
        control_key_label(bindings, player, ControlAction::Right),
        control_key_label(bindings, player, ControlAction::Up),
        control_key_label(bindings, player, ControlAction::Down),
        control_key_label(bindings, player, ControlAction::AimGrab),
        control_key_label(bindings, player, ControlAction::Heavy),
        control_key_label(bindings, player, ControlAction::Light),
        control_key_label(bindings, player, ControlAction::Jump),
    )
}

fn controls_player_compact_message(player: usize, bindings: &PlayerKeyBindings) -> String {
    format!(
        "P{}  Move {}/{}/{}/{}  |  Hold Aim / Tap Grab {}  |  Heavy {}  |  Light {}  |  Jump {}",
        player + 1,
        control_key_label(bindings, player, ControlAction::Left),
        control_key_label(bindings, player, ControlAction::Right),
        control_key_label(bindings, player, ControlAction::Up),
        control_key_label(bindings, player, ControlAction::Down),
        control_key_label(bindings, player, ControlAction::AimGrab),
        control_key_label(bindings, player, ControlAction::Heavy),
        control_key_label(bindings, player, ControlAction::Light),
        control_key_label(bindings, player, ControlAction::Jump),
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

fn start_user_mode_menu_music(commands: &mut Commands, asset_server: &AssetServer) {
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
        MatchPresentationTransient,
        AudioPlayer::new(asset_server.load(user_mode_battle_music_path(arena_index))),
        PlaybackSettings::LOOP,
    ));
}

pub fn sync_online_match_presentation_audio(
    policy: Res<MatchPresentationPolicy>,
    simulation_drive: Res<crate::simulation::SimulationDriveMode>,
    asset_server: Res<AssetServer>,
    menu_music: Query<Entity, (With<UserModeMusic>, Without<ArenaMusic>)>,
    arena_music: Query<(Entity, &ArenaMusic)>,
    mut feedback: ResMut<HitEffects>,
    mut result_history: ResMut<PresentedResultSfxHistory>,
    mut commands: Commands,
) {
    if *simulation_drive != crate::simulation::SimulationDriveMode::ExternalProjection {
        return;
    }

    match policy.music {
        PresentationMusicTrack::None => {
            for entity in &menu_music {
                commands.entity(entity).despawn();
            }
            stop_arena_music(&mut commands, &arena_music);
        }
        PresentationMusicTrack::Menu => {
            stop_arena_music(&mut commands, &arena_music);
            let mut kept = false;
            for entity in &menu_music {
                if kept {
                    commands.entity(entity).despawn();
                } else {
                    kept = true;
                }
            }
            if !kept {
                start_user_mode_menu_music(&mut commands, &asset_server);
            }
        }
        PresentationMusicTrack::Arena(arena_index) => {
            for entity in &menu_music {
                commands.entity(entity).despawn();
            }
            reconcile_arena_music(&mut commands, &asset_server, &arena_music, arena_index);
        }
    }

    if let Some((key, result)) = policy.result_sfx
        && result_history.mark_if_new(key)
    {
        let kind = match result {
            PresentationResultSfx::Victory => CombatSfxKind::ResultWin,
            PresentationResultSfx::Defeat => CombatSfxKind::ResultLose,
        };
        feedback.push_combat_sfx(CombatSfxCue::new(
            kind,
            Vec3::ZERO,
            USER_MODE_RESULT_SFX_PRIORITY,
        ));
    }
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

fn stop_user_mode_music(commands: &mut Commands, music: &Query<Entity, With<UserModeMusic>>) {
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
    state.rule_index = setup.rule_index;
    state.rules = setup.active_rule();
    state.arena_index = setup.arena_index;
    state.apply_local_setup(setup);
    state.replay_seed = setup.replay_seed;
    state.reset_requested = false;
    if user_mode.controls_briefing_seen {
        confirm_user_mode_match_start(user_mode, state);
        UserModeMatchStartFlow::BattleStarted
    } else {
        user_mode.enter_controls_briefing();
        UserModeMatchStartFlow::ControlsBriefing
    }
}

fn confirm_user_mode_match_start(user_mode: &mut UserModeState, state: &mut MatchState) {
    user_mode.match_request_revision = user_mode
        .match_request_revision
        .checked_add(1)
        .expect("local match request revision exhausted");
    state.request_rematch();
    user_mode.exit_to_battle();
}

fn reset_user_mode_presentation(
    presentation_time_scale: &mut PresentationTimeScale,
    screen_look: &mut ScreenLook,
    screen_transition: &mut ScreenLookTransition,
) {
    presentation_time_scale.reset();
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
    use crate::components::{LocalInputAssignment, ParticipantKind};
    use crate::steam_platform::{SteamInputControllerId, SteamInputControllerSnapshot};

    fn steam_menu_snapshot(
        local_ordinal: usize,
        actions: &[SteamMenuAction],
    ) -> SteamInputSnapshot {
        let mut menu_held = SteamMenuInputMask::NONE;
        for action in actions {
            menu_held.insert(*action);
        }
        let mut snapshot = SteamInputSnapshot::default();
        snapshot.controllers[local_ordinal] = SteamInputControllerSnapshot {
            controller_id: SteamInputControllerId::new(local_ordinal as u64 + 1),
            menu_held,
            ..default()
        };
        snapshot
    }

    #[test]
    fn presentation_scale_is_local_clamped_and_resettable() {
        let mut scale = PresentationTimeScale::default();
        assert_eq!(scale.scale_delta(0.5), 0.5);

        scale.set(0.22);
        assert!((scale.scale_delta(0.5) - 0.11).abs() < f32::EPSILON);
        scale.set(f32::NAN);
        assert_eq!(scale.value(), 1.0);
        scale.set(-1.0);
        assert_eq!(scale.value(), 1.0);

        scale.set(0.0);
        assert_eq!(scale.scale_delta(1.0), 0.0);
        scale.reset();
        assert_eq!(scale.value(), 1.0);
    }

    #[test]
    fn title_release_identity_is_constructed_from_the_compiled_identity() {
        let identity = current_release_identity();
        let label = user_mode_release_identity_text();
        assert_eq!(label, identity.short_ui_label());
        assert!(label.starts_with(&format!("v{} • ", identity.product_version)));
        assert!(label.ends_with(&identity.compatibility_build_id[..12]));
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

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn native_release_cold_boot_uses_player_facing_start_screen() {
        assert_eq!(
            native_user_mode_initial_screen(false),
            UserModeScreen::Start
        );
        assert_eq!(native_user_mode_initial_screen(true), UserModeScreen::Dev);
    }

    #[test]
    fn performance_mode_hides_menu_flow_without_disabling_dev_gameplay() {
        let mut user_mode = UserModeState::default();
        user_mode.enter_fresh_mode_select();
        user_mode.battle_active = true;
        user_mode.force_performance_dev_mode();
        assert_eq!(user_mode.screen(), UserModeScreen::Dev);
        assert!(!user_mode.active());
        assert!(!user_mode.battle_active);
        assert!(!user_mode.battle_music_pending);
    }

    #[test]
    fn embedded_authority_failure_replay_is_an_explicit_new_match_revision() {
        let mut user_mode = UserModeState::default();
        let mut setup = LocalSetup::default();
        let mut state = MatchState::default();
        user_mode.controls_briefing_seen = true;
        confirm_user_mode_match_start(&mut user_mode, &mut state);
        assert_eq!(user_mode.match_request_revision(), 1);

        user_mode.present_embedded_authority_failure();
        assert_eq!(user_mode.screen(), UserModeScreen::BattleResult);
        assert!(user_mode.result_menu_ready);
        assert!(user_mode.network_match_requested());
        assert_eq!(
            route_user_mode_action(&mut user_mode, UserModeUiAction::Confirm),
            UserModeRoute::Replay
        );
        assert_eq!(
            user_mode.match_request_revision(),
            1,
            "selecting replay alone cannot mutate the failed request"
        );

        assert_eq!(
            prepare_user_mode_match(&mut user_mode, &mut setup, &mut state),
            UserModeMatchStartFlow::BattleStarted
        );
        assert_eq!(user_mode.match_request_revision(), 2);
        assert!(user_mode.network_match_requested());
    }

    #[test]
    fn steam_controller_cold_boot_reaches_online_with_edge_latched_focus() {
        let mut user_mode = UserModeState::default();
        user_mode.screen = UserModeScreen::Start;

        assert_eq!(
            user_mode.controller_menu_intent(SteamInputSnapshot::default()),
            UserModeControllerIntent::None
        );
        let accept = steam_menu_snapshot(0, &[SteamMenuAction::Accept]);
        assert_eq!(
            user_mode.controller_menu_intent(accept),
            UserModeControllerIntent::Dispatch(UserModeUiAction::Confirm)
        );
        assert_eq!(
            user_mode.controller_menu_intent(accept),
            UserModeControllerIntent::None,
            "a held face button must not repeat"
        );

        user_mode.enter_fresh_mode_select();
        assert_eq!(
            user_mode.controller_menu_intent(accept),
            UserModeControllerIntent::None,
            "the Start accept must be primed across the screen transition"
        );
        assert_eq!(
            user_mode.controller_menu_intent(SteamInputSnapshot::default()),
            UserModeControllerIntent::None
        );

        let down = steam_menu_snapshot(0, &[SteamMenuAction::Down]);
        assert_eq!(
            user_mode.controller_menu_intent(down),
            UserModeControllerIntent::Dispatch(UserModeUiAction::Next)
        );
        route_user_mode_action(&mut user_mode, UserModeUiAction::Next);
        assert_eq!(
            user_mode.main_menu_choice,
            UserModeMainMenuChoice::LocalMultiplayer
        );
        assert_eq!(
            user_mode.controller_menu_intent(down),
            UserModeControllerIntent::None
        );
        user_mode.controller_menu_intent(SteamInputSnapshot::default());
        assert_eq!(
            user_mode.controller_menu_intent(down),
            UserModeControllerIntent::Dispatch(UserModeUiAction::Next)
        );
        route_user_mode_action(&mut user_mode, UserModeUiAction::Next);
        assert_eq!(user_mode.main_menu_choice, UserModeMainMenuChoice::Online);

        user_mode.controller_menu_intent(SteamInputSnapshot::default());
        assert_eq!(
            user_mode.controller_menu_intent(accept),
            UserModeControllerIntent::Dispatch(UserModeUiAction::Confirm)
        );
        route_user_mode_action(&mut user_mode, UserModeUiAction::Confirm);
        assert_eq!(user_mode.screen(), UserModeScreen::Online);
    }

    #[test]
    fn steam_controller_back_returns_to_start_without_transition_cascade() {
        let mut user_mode = UserModeState::default();
        user_mode.enter_online();
        user_mode.controller_menu_intent(SteamInputSnapshot::default());

        let back = steam_menu_snapshot(0, &[SteamMenuAction::Back]);
        user_mode.leave_online();
        assert_eq!(user_mode.screen(), UserModeScreen::ModeSelect);
        assert_eq!(
            user_mode.controller_menu_intent(back),
            UserModeControllerIntent::None,
            "the Back that left Online must not also leave Mode Select"
        );
        user_mode.controller_menu_intent(SteamInputSnapshot::default());
        assert_eq!(
            user_mode.controller_menu_intent(back),
            UserModeControllerIntent::Dispatch(UserModeUiAction::ControllerBack)
        );
        assert_eq!(
            route_user_mode_action(&mut user_mode, UserModeUiAction::ControllerBack),
            UserModeRoute::ReturnToStart
        );
        assert_eq!(user_mode.screen(), UserModeScreen::Start);
    }

    #[test]
    fn steam_controller_binding_intent_preserves_requesting_ordinal() {
        let mut user_mode = UserModeState::default();
        user_mode.enter_mode_select();
        user_mode.controller_menu_intent(SteamInputSnapshot::default());

        let bindings = steam_menu_snapshot(3, &[SteamMenuAction::OpenBindings]);
        assert_eq!(
            user_mode.controller_menu_intent(bindings),
            UserModeControllerIntent::OpenBindings(3)
        );
        assert_eq!(
            user_mode.controller_menu_intent(bindings),
            UserModeControllerIntent::None
        );
    }

    #[test]
    fn main_menu_cycles_and_wraps_four_vertical_choices() {
        let choices = [
            UserModeMainMenuChoice::SinglePlayer,
            UserModeMainMenuChoice::LocalMultiplayer,
            UserModeMainMenuChoice::Online,
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
        assert_eq!(single.screen(), UserModeScreen::CharacterSelect);
        assert_eq!(single.play_mode, UserPlayMode::SinglePlayer);

        let mut multiplayer = UserModeState::default();
        multiplayer.enter_mode_select();
        route_user_mode_action(
            &mut multiplayer,
            UserModeUiAction::MainMenu(UserModeMainMenuChoice::LocalMultiplayer),
        );
        assert_eq!(multiplayer.screen(), UserModeScreen::PlayerCountSelect);
        assert_eq!(
            multiplayer.player_count_choice,
            UserModePlayerCountChoice::TwoPlayers
        );

        let mut online = UserModeState::default();
        online.enter_mode_select();
        route_user_mode_action(
            &mut online,
            UserModeUiAction::MainMenu(UserModeMainMenuChoice::Online),
        );
        assert_eq!(online.screen(), UserModeScreen::Online);

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
            assert_eq!(state.screen(), UserModeScreen::CharacterSelect);
            assert_eq!(state.play_mode, play_mode);
        }

        let mut settings = UserModeState::default();
        settings.enter_mode_select();
        route_user_mode_action(
            &mut settings,
            UserModeUiAction::MainMenu(UserModeMainMenuChoice::Settings),
        );
        assert_eq!(settings.screen(), UserModeScreen::KeySettings);
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
        assert_eq!(user_mode.screen(), UserModeScreen::ModeSelect);

        user_mode.play_mode = UserPlayMode::FourPlayers;
        user_mode.return_to_character_select_player(3);
        for previous_player in (0..3).rev() {
            route_user_mode_action(&mut user_mode, UserModeUiAction::Back);
            assert_eq!(user_mode.character_select_player, previous_player);
        }
        route_user_mode_action(&mut user_mode, UserModeUiAction::Back);
        assert_eq!(user_mode.screen(), UserModeScreen::PlayerCountSelect);

        user_mode.play_mode = UserPlayMode::SinglePlayer;
        user_mode.enter_character_select();
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
        assert_eq!(bindings.all_keys().len(), FIGHTER_COUNT * 8);
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
        assert!(message.contains("Hold Aim / Tap Grab: Z"));
        assert!(message.contains("Heavy / Throw: X"));
        assert!(message.contains("Light / Pickup / Item: C"));
        assert!(message.contains("Jump: V"));
        assert!(message.contains("Guard: X + C"));
        assert!(message.contains("Press Enter or click to fight"));
    }

    #[test]
    fn controls_briefing_copy_shows_two_player_columns_and_loading_state() {
        let mut user_mode = UserModeState::default();
        user_mode.play_mode = UserPlayMode::TwoPlayers;
        let bindings = PlayerKeyBindings::default();

        let message = controls_briefing_message(&user_mode, &bindings, false);

        assert!(message.contains("2 players share this keyboard"));
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

        assert!(message.contains("4 players share this keyboard"));
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
            UserModeUiAction::MainMenu(UserModeMainMenuChoice::LocalMultiplayer),
        );
        assert_eq!(user_mode.screen(), UserModeScreen::PlayerCountSelect);
        route_user_mode_action(
            &mut user_mode,
            UserModeUiAction::PlayerCount(UserModePlayerCountChoice::TwoPlayers),
        );
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
            .apply_key_capture(&mut bindings, KeyCode::KeyE)
            .unwrap();

        assert_eq!(
            bindings.key_for(result.capture.player, result.capture.action),
            Some(KeyCode::KeyE)
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
