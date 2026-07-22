use bevy::camera::{RenderTarget, visibility::RenderLayers};
use bevy::prelude::*;
use bevy::render::render_resource::TextureFormat;
use bevy::scene::SceneInstanceReady;
use bevy::time::{Real, Virtual};
use bevy::ui::UiTargetCamera;

use crate::arena_defs::{arena_definitions, set_active_arena_index};
use crate::bot::start_bot_combat_ai;
use crate::camera::{ScreenLook, ScreenLookTransition, UiCamera, begin_screen_look_transition};
use crate::characters::{
    CharacterKind, CharacterMoveCatalog, character_label, character_scene_model,
};
use crate::combat::HitEffects;
use crate::combat_sfx::{CombatSfxCue, CombatSfxKind};
use crate::components::{BotBrain, ControlAction, Controller, Fighter, PlayerKeyBindings};
use crate::game_state::{
    LocalSetup, MatchAnnouncements, MatchPhase, MatchState, reconcile_fighter_control_from_setup,
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UserModeMenuChoice {
    SinglePlayer,
    TwoPlayers,
    KeySettings,
}

impl UserModeMenuChoice {
    fn previous(self) -> Self {
        match self {
            Self::SinglePlayer => Self::KeySettings,
            Self::TwoPlayers => Self::SinglePlayer,
            Self::KeySettings => Self::TwoPlayers,
        }
    }

    fn next(self) -> Self {
        match self {
            Self::SinglePlayer => Self::TwoPlayers,
            Self::TwoPlayers => Self::KeySettings,
            Self::KeySettings => Self::SinglePlayer,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CharacterSelectPlayer {
    PlayerOne,
    PlayerTwo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KeyBindingCapture {
    player: usize,
    action: ControlAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct KeyBindingApplyResult {
    capture: KeyBindingCapture,
    swapped: Option<KeyBindingCapture>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UserModeResultChoice {
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

#[derive(Resource, Clone, Debug)]
pub struct UserModeState {
    screen: UserModeScreen,
    play_mode: UserPlayMode,
    menu_choice: UserModeMenuChoice,
    p1_character: CharacterKind,
    p2_character: CharacterKind,
    arena_index: usize,
    character_select_player: CharacterSelectPlayer,
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
        && (user_mode.screen == UserModeScreen::ControlsBriefing
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
    p1_character: CharacterKind,
    p2_character: CharacterKind,
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
        _ => UserPlayMode::SinglePlayer,
    };
    let p1_character = parse_web_character(js_string_prop(&value, "p1Character").as_deref())
        .unwrap_or(CharacterKind::Cat);
    let p2_character = parse_web_character(js_string_prop(&value, "p2Character").as_deref())
        .unwrap_or_else(|| opposite_user_mode_character(p1_character));
    let arena_index = js_number_prop(&value, "arenaIndex")
        .map(|index| index as usize)
        .unwrap_or(0)
        .min(arena_definitions().len().saturating_sub(1));
    let bindings = js_sys::Reflect::get(&value, &JsValue::from_str("bindings"))
        .ok()
        .and_then(|bindings| parse_web_bindings(&bindings));

    Some(WebMatchConfig {
        play_mode,
        p1_character,
        p2_character,
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

    let p1 = js_sys::Reflect::get(value, &JsValue::from_str("p1")).ok()?;
    let p2 = js_sys::Reflect::get(value, &JsValue::from_str("p2")).ok()?;
    let bindings = PlayerKeyBindings {
        p1: parse_web_player_bindings(
            &p1,
            crate::components::PlayerControlBindings::player_one_default(),
        ),
        p2: parse_web_player_bindings(
            &p2,
            crate::components::PlayerControlBindings::player_two_default(),
        ),
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
            menu_choice: UserModeMenuChoice::SinglePlayer,
            p1_character: CharacterKind::Cat,
            p2_character: CharacterKind::Pig,
            arena_index: 0,
            character_select_player: CharacterSelectPlayer::PlayerOne,
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
        match self.character_select_player {
            CharacterSelectPlayer::PlayerOne => self.p1_character,
            CharacterSelectPlayer::PlayerTwo => self.p2_character,
        }
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

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    pub fn blocks_practice_health_refill(&self) -> bool {
        self.battle_active || self.screen == UserModeScreen::BattleResult
    }

    fn enter_fresh_mode_select(&mut self) {
        self.screen = UserModeScreen::ModeSelect;
        self.play_mode = UserPlayMode::SinglePlayer;
        self.menu_choice = UserModeMenuChoice::SinglePlayer;
        self.p1_character = CharacterKind::Cat;
        self.p2_character = CharacterKind::Pig;
        self.arena_index = 0;
        self.character_select_player = CharacterSelectPlayer::PlayerOne;
        self.key_settings_cursor = 0;
        self.key_capture = None;
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
        self.character_select_player = CharacterSelectPlayer::PlayerOne;
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
        match self.character_select_player {
            CharacterSelectPlayer::PlayerOne => self.p1_character = character,
            CharacterSelectPlayer::PlayerTwo => self.p2_character = character,
        }
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
        if self.play_mode == UserPlayMode::TwoPlayers
            && self.character_select_player == CharacterSelectPlayer::PlayerOne
        {
            self.character_select_player = CharacterSelectPlayer::PlayerTwo;
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
        let total = ControlAction::ALL.len() * 2;
        self.key_settings_cursor =
            (self.key_settings_cursor as isize + direction).rem_euclid(total as isize) as usize;
    }

    fn move_key_column(&mut self, direction: isize) {
        let action_count = ControlAction::ALL.len();
        let action_index = self.key_settings_cursor % action_count;
        let player = self.key_settings_cursor / action_count;
        let next_player = (player as isize + direction).clamp(0, 1) as usize;
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

#[derive(Component)]
pub(crate) struct UserModeRoot;

#[derive(Component)]
pub(crate) struct UserModeStartPanel;

#[derive(Component)]
pub(crate) struct UserModeSelectPanel;

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
pub(crate) struct UserModeResultChoiceText;

#[derive(Component)]
pub(crate) struct UserModeChoiceText;

#[derive(Component)]
pub(crate) struct UserModeSelectTitleText;

#[derive(Component)]
pub(crate) struct UserModeSelectHintText;

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

#[derive(Component)]
pub(crate) struct UserModeBattleMusic;

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

fn key_settings_column(player: usize) -> impl Bundle {
    (
        Node {
            flex_basis: Val::Percent(50.0),
            max_width: Val::Px(420.0),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Stretch,
            row_gap: Val::Px(8.0),
            border: UiRect::all(Val::Px(2.0)),
            padding: UiRect::all(Val::Px(12.0)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.055, 0.055, 0.065, 0.88)),
        BorderColor::all(Color::srgb(0.63, 0.61, 0.56)),
        children![
            (
                Text::new(format!("P{}", player + 1)),
                TextFont {
                    font_size: 24.0,
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
        Node {
            height: Val::Px(USER_MODE_KEY_ROW_HEIGHT),
            min_height: Val::Px(USER_MODE_KEY_ROW_HEIGHT),
            max_height: Val::Px(USER_MODE_KEY_ROW_HEIGHT),
            align_items: AlignItems::Center,
            padding: UiRect::axes(Val::Px(8.0), Val::Px(0.0)),
            ..default()
        },
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
        preview_view_format,
    );
    let preview_image = images.add(preview_image);
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
                UserModeSelectPanel,
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
                        UserModeSelectTitleText,
                        Text::new("SELECT A CHARACTER"),
                        TextFont {
                            font_size: 46.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.95, 0.86, 0.68)),
                        TextShadow::default(),
                    ),
                    (
                        ImageNode::new(preview_image),
                        Node {
                            width: Val::Px(310.0),
                            height: Val::Px(310.0),
                            ..default()
                        },
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
                    ),
                    (
                        UserModeSelectHintText,
                        Text::new("Left/Right or Q/E choose  |  Enter start"),
                        TextFont {
                            font_size: 20.0,
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
                            width: Val::Percent(88.0),
                            max_width: Val::Px(900.0),
                            flex_direction: FlexDirection::Row,
                            justify_content: JustifyContent::Center,
                            align_items: AlignItems::FlexStart,
                            column_gap: Val::Px(20.0),
                            ..default()
                        },
                        children![key_settings_column(0), key_settings_column(1)],
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
                            font_size: 22.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.92, 0.88, 0.78)),
                        TextLayout::new_with_justify(Justify::Center),
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
                        UserModeResultChoiceText,
                        Text::new(result_choice_message(UserModeResultChoice::PlayAgain)),
                        TextFont {
                            font_size: 30.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.96, 0.92, 0.82)),
                        TextShadow::default(),
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
        ],
    ));
    if let Some(ui_camera) = ui_camera {
        user_mode_root.insert(UiTargetCamera(ui_camera));
    }
}

pub fn handle_user_mode_input(
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    asset_server: Res<AssetServer>,
    mut user_mode: ResMut<UserModeState>,
    mut key_bindings: ResMut<PlayerKeyBindings>,
    mut setup: ResMut<LocalSetup>,
    mut state: ResMut<MatchState>,
    gameplay_scene: Res<UserModeGameplayScene>,
    mut announcements: ResMut<MatchAnnouncements>,
    music: Query<Entity, With<UserModeMusic>>,
    mut virtual_time: ResMut<Time<Virtual>>,
    mut screen_look: ResMut<ScreenLook>,
    mut screen_transition: ResMut<ScreenLookTransition>,
    mut commands: Commands,
) {
    if user_mode.screen == UserModeScreen::Start {
        if keys.just_pressed(KeyCode::Enter)
            || keys.just_pressed(KeyCode::Space)
            || buttons.just_pressed(MouseButton::Left)
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

    #[cfg(target_arch = "wasm32")]
    if matches!(
        user_mode.screen,
        UserModeScreen::ModeSelect | UserModeScreen::CharacterSelect | UserModeScreen::ArenaSelect
    ) {
        if let Some(config) = take_web_match_config() {
            user_mode.play_mode = config.play_mode;
            user_mode.p1_character = config.p1_character;
            user_mode.p2_character = config.p2_character;
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

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    {
        if keys.just_pressed(KeyCode::Escape) && user_mode.screen != UserModeScreen::KeySettings {
            stop_user_mode_music(&mut commands, &music);
            reset_user_mode_presentation(
                &mut virtual_time,
                &mut screen_look,
                &mut screen_transition,
            );
            user_mode.exit_to_dev();
            announcements.show("Dev setup", 0.8);
            return;
        }
    }

    if user_mode.screen == UserModeScreen::ModeSelect {
        if select_previous_pressed(&keys) {
            user_mode.menu_choice = user_mode.menu_choice.previous();
        }
        if select_next_pressed(&keys) {
            user_mode.menu_choice = user_mode.menu_choice.next();
        }
        if keys.just_pressed(KeyCode::Enter) {
            match user_mode.menu_choice {
                UserModeMenuChoice::SinglePlayer => {
                    user_mode.play_mode = UserPlayMode::SinglePlayer;
                    user_mode.enter_character_select();
                }
                UserModeMenuChoice::TwoPlayers => {
                    user_mode.play_mode = UserPlayMode::TwoPlayers;
                    user_mode.enter_character_select();
                }
                UserModeMenuChoice::KeySettings => user_mode.enter_key_settings(),
            }
        }
        return;
    }

    if user_mode.screen == UserModeScreen::KeySettings {
        if user_mode.key_capture.is_some() {
            if keys.just_pressed(KeyCode::Escape) {
                user_mode.cancel_key_capture();
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
        if keys.just_pressed(KeyCode::Escape) {
            user_mode.enter_mode_select();
            return;
        }
        if keys.just_pressed(KeyCode::KeyR) {
            *key_bindings = PlayerKeyBindings::default();
            announcements.show("Controls reset", 1.0);
        }
        if keys.just_pressed(KeyCode::ArrowUp) {
            user_mode.move_key_cursor(-1);
        }
        if keys.just_pressed(KeyCode::ArrowDown) {
            user_mode.move_key_cursor(1);
        }
        if keys.just_pressed(KeyCode::ArrowLeft) {
            user_mode.move_key_column(-1);
        }
        if keys.just_pressed(KeyCode::ArrowRight) {
            user_mode.move_key_column(1);
        }
        if keys.just_pressed(KeyCode::Enter) {
            user_mode.begin_key_capture();
        }
        return;
    }

    if user_mode.screen == UserModeScreen::ControlsBriefing {
        let local_start_requested = keys.just_pressed(KeyCode::Enter)
            || keys.just_pressed(KeyCode::Space)
            || buttons.just_pressed(MouseButton::Left);
        #[cfg(target_arch = "wasm32")]
        let web_start_requested = web_battle_start_signal_requested();
        #[cfg(not(target_arch = "wasm32"))]
        let web_start_requested = false;

        let start_requested = local_start_requested || web_start_requested;

        if start_requested {
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
        return;
    }

    if user_mode.screen == UserModeScreen::BattleResult {
        if !user_mode.result_menu_ready {
            return;
        }
        if select_previous_pressed(&keys) || select_next_pressed(&keys) {
            user_mode.toggle_result_choice();
        }
        if keys.just_pressed(KeyCode::Enter) {
            reset_user_mode_presentation(
                &mut virtual_time,
                &mut screen_look,
                &mut screen_transition,
            );
            match user_mode.result_choice {
                UserModeResultChoice::PlayAgain => {
                    let flow = prepare_user_mode_match(&mut user_mode, &mut setup, &mut state);
                    announce_user_mode_match_flow(flow, &setup, &mut announcements);
                }
                UserModeResultChoice::ChooseCharacter => {
                    state.return_to_setup();
                    user_mode.enter_mode_select();
                    stop_user_mode_music(&mut commands, &music);
                    start_user_mode_menu_music(&mut commands, &asset_server);
                    announcements.show("", 0.0);
                }
            }
        }
        return;
    }

    if user_mode.screen == UserModeScreen::CharacterSelect {
        if select_previous_pressed(&keys) {
            user_mode.select_previous();
        }
        if select_next_pressed(&keys) {
            user_mode.select_next();
        }
        if keys.just_pressed(KeyCode::Enter) {
            if !user_mode.confirm_character_selection() {
                announcements.show("P2 choose character", 0.9);
                return;
            }
            user_mode.enter_arena_select();
            announcements.show("Choose arena", 0.9);
        }
        return;
    }

    if user_mode.screen == UserModeScreen::ArenaSelect {
        if select_previous_pressed(&keys) {
            user_mode.select_previous_arena();
        }
        if select_next_pressed(&keys) {
            user_mode.select_next_arena();
        }
        if keys.just_pressed(KeyCode::Enter) {
            stop_user_mode_music(&mut commands, &music);
            let flow = prepare_user_mode_match(&mut user_mode, &mut setup, &mut state);
            announce_user_mode_match_flow(flow, &setup, &mut announcements);
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
    battle_music: Query<Entity, With<UserModeBattleMusic>>,
    mut commands: Commands,
) {
    if user_mode.battle_music_pending && state.phase == MatchPhase::Fighting {
        if battle_music.is_empty() {
            start_user_mode_battle_music(&mut commands, &asset_server, state.arena_index);
        }
        user_mode.battle_music_pending = false;
        user_mode.battle_active = true;
        return;
    }

    if user_mode.battle_active && state.phase != MatchPhase::Fighting {
        stop_user_mode_battle_music(&mut commands, &battle_music);
        user_mode.battle_bot_ai_pending = false;
        user_mode.battle_active = false;
    }
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
    bindings: Res<PlayerKeyBindings>,
    mut roots: Query<(&mut Node, &mut BackgroundColor), With<UserModeRoot>>,
    mut start_panels: Query<
        &mut Node,
        (
            With<UserModeStartPanel>,
            Without<UserModeRoot>,
            Without<UserModeSelectPanel>,
            Without<UserModeKeySettingsPanel>,
            Without<UserModeControlsPanel>,
            Without<UserModeResultPanel>,
        ),
    >,
    mut select_panels: Query<
        &mut Node,
        (
            With<UserModeSelectPanel>,
            Without<UserModeRoot>,
            Without<UserModeStartPanel>,
            Without<UserModeKeySettingsPanel>,
            Without<UserModeControlsPanel>,
        ),
    >,
    mut key_settings_panels: Query<
        &mut Node,
        (
            With<UserModeKeySettingsPanel>,
            Without<UserModeRoot>,
            Without<UserModeStartPanel>,
            Without<UserModeSelectPanel>,
            Without<UserModeControlsPanel>,
            Without<UserModeResultPanel>,
        ),
    >,
    mut result_panels: Query<
        &mut Node,
        (
            With<UserModeResultPanel>,
            Without<UserModeRoot>,
            Without<UserModeStartPanel>,
            Without<UserModeSelectPanel>,
            Without<UserModeKeySettingsPanel>,
            Without<UserModeControlsPanel>,
        ),
    >,
    mut choices: Query<
        &mut Text,
        (
            With<UserModeChoiceText>,
            Without<UserModeSelectTitleText>,
            Without<UserModeSelectHintText>,
            Without<UserModeKeySettingsPromptText>,
            Without<UserModeKeySettingsRowText>,
            Without<UserModeControlsText>,
            Without<UserModeResultText>,
            Without<UserModeResultChoiceText>,
        ),
    >,
    mut select_titles: Query<
        &mut Text,
        (
            With<UserModeSelectTitleText>,
            Without<UserModeChoiceText>,
            Without<UserModeSelectHintText>,
            Without<UserModeKeySettingsPromptText>,
            Without<UserModeKeySettingsRowText>,
            Without<UserModeControlsText>,
            Without<UserModeResultText>,
            Without<UserModeResultChoiceText>,
        ),
    >,
    mut select_hints: Query<
        &mut Text,
        (
            With<UserModeSelectHintText>,
            Without<UserModeChoiceText>,
            Without<UserModeSelectTitleText>,
            Without<UserModeKeySettingsPromptText>,
            Without<UserModeKeySettingsRowText>,
            Without<UserModeControlsText>,
            Without<UserModeResultText>,
            Without<UserModeResultChoiceText>,
        ),
    >,
    mut key_settings_prompts: Query<
        &mut Text,
        (
            With<UserModeKeySettingsPromptText>,
            Without<UserModeChoiceText>,
            Without<UserModeSelectTitleText>,
            Without<UserModeSelectHintText>,
            Without<UserModeKeySettingsRowText>,
            Without<UserModeControlsText>,
            Without<UserModeResultText>,
            Without<UserModeResultChoiceText>,
        ),
    >,
    mut key_settings_rows: Query<
        (&UserModeKeySettingsRowText, &mut Text, &mut TextColor),
        (
            Without<UserModeChoiceText>,
            Without<UserModeSelectTitleText>,
            Without<UserModeSelectHintText>,
            Without<UserModeKeySettingsPromptText>,
            Without<UserModeControlsText>,
            Without<UserModeResultText>,
            Without<UserModeResultChoiceText>,
        ),
    >,
    mut key_settings_scrolls: Query<(&UserModeKeySettingsScroll, &mut ScrollPosition)>,
    mut result_titles: Query<
        &mut Text,
        (
            With<UserModeResultText>,
            Without<UserModeChoiceText>,
            Without<UserModeSelectTitleText>,
            Without<UserModeSelectHintText>,
            Without<UserModeKeySettingsPromptText>,
            Without<UserModeKeySettingsRowText>,
            Without<UserModeControlsText>,
            Without<UserModeResultChoiceText>,
        ),
    >,
    mut result_choices: Query<
        &mut Text,
        (
            With<UserModeResultChoiceText>,
            Without<UserModeChoiceText>,
            Without<UserModeSelectTitleText>,
            Without<UserModeSelectHintText>,
            Without<UserModeKeySettingsPromptText>,
            Without<UserModeKeySettingsRowText>,
            Without<UserModeControlsText>,
            Without<UserModeResultText>,
        ),
    >,
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

    let start_visible = user_mode.screen() == UserModeScreen::Start;
    let select_visible = matches!(
        user_mode.screen(),
        UserModeScreen::ModeSelect | UserModeScreen::CharacterSelect | UserModeScreen::ArenaSelect
    );
    let key_settings_visible = user_mode.screen() == UserModeScreen::KeySettings;
    let result_visible =
        user_mode.screen() == UserModeScreen::BattleResult && user_mode.result_menu_ready;
    for mut node in &mut start_panels {
        node.display = if start_visible {
            Display::Flex
        } else {
            Display::None
        };
    }
    for mut node in &mut select_panels {
        node.display = if select_visible {
            Display::Flex
        } else {
            Display::None
        };
    }
    for mut node in &mut key_settings_panels {
        node.display = if key_settings_visible {
            Display::Flex
        } else {
            Display::None
        };
    }
    for mut node in &mut result_panels {
        node.display = if result_visible {
            Display::Flex
        } else {
            Display::None
        };
    }
    for mut text in &mut choices {
        **text = user_mode_choice_message(&user_mode);
    }
    for mut text in &mut select_titles {
        **text = user_mode_select_title_message(&user_mode);
    }
    for mut text in &mut select_hints {
        **text = user_mode_select_hint_message(&user_mode);
    }
    for mut text in &mut key_settings_prompts {
        **text = key_settings_prompt_message(&user_mode);
    }
    let selected_key_target = user_mode.selected_key_target();
    for (row, mut text, mut color) in &mut key_settings_rows {
        let key = bindings
            .key_for(row.player, row.action)
            .expect("valid player binding");
        let selected = key_settings_visible
            && row.player == selected_key_target.player
            && row.action == selected_key_target.action;
        **text = key_settings_row_message(row.action, key, selected);
        *color = key_settings_row_color(selected);
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
    for mut text in &mut result_titles {
        **text = result_title_message(&user_mode);
    }
    for mut text in &mut result_choices {
        **text = result_choice_message(user_mode.result_choice);
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
        | UserModeScreen::KeySettings
        | UserModeScreen::CharacterSelect
        | UserModeScreen::ArenaSelect
        | UserModeScreen::ControlsBriefing => 1.0,
        UserModeScreen::BattleResult if user_mode.result_menu_ready => 0.58,
        UserModeScreen::BattleResult => 0.0,
        UserModeScreen::Dev => 0.0,
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn user_mode_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    keys.just_pressed(KeyCode::KeyU)
        && (keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight))
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
    USER_MODE_SELECTABLE_CHARACTERS
        .iter()
        .map(|character| {
            let label = character_label(*character).to_ascii_uppercase();
            if selected == *character {
                format!("> {label} <")
            } else {
                format!("  {label}  ")
            }
        })
        .collect::<Vec<_>>()
        .join("        ")
}

fn mode_select_message(choice: UserModeMenuChoice) -> String {
    let single = if choice == UserModeMenuChoice::SinglePlayer {
        "> SINGLE PLAYER <"
    } else {
        "  Single Player  "
    };
    let two = if choice == UserModeMenuChoice::TwoPlayers {
        "> TWO PLAYERS <"
    } else {
        "  Two Players  "
    };
    let keys = if choice == UserModeMenuChoice::KeySettings {
        "> KEY SETTINGS <"
    } else {
        "  Key Settings  "
    };
    format!("{single}        {two}        {keys}")
}

fn arena_select_message(selected_index: usize) -> String {
    let arenas = arena_definitions();
    let selected_index = selected_index.min(arenas.len().saturating_sub(1));
    arenas
        .iter()
        .enumerate()
        .map(|(index, arena)| {
            let label = arena.name.to_ascii_uppercase();
            if index == selected_index {
                format!("> {label} <")
            } else {
                format!("  {label}  ")
            }
        })
        .collect::<Vec<_>>()
        .chunks(4)
        .map(|row| row.join("   "))
        .collect::<Vec<_>>()
        .join("\n")
}

fn user_mode_select_title_message(user_mode: &UserModeState) -> String {
    match user_mode.screen {
        UserModeScreen::ModeSelect => "ANIMAL FIGHTER CLUB".to_string(),
        UserModeScreen::CharacterSelect => "SELECT A CHARACTER".to_string(),
        UserModeScreen::ArenaSelect => "SELECT AN ARENA".to_string(),
        _ => "ANIMAL FIGHTER CLUB".to_string(),
    }
}

fn user_mode_select_hint_message(user_mode: &UserModeState) -> String {
    match user_mode.screen {
        UserModeScreen::ModeSelect => "Left/Right or Q/E choose  |  Enter confirm".to_string(),
        UserModeScreen::CharacterSelect => "Left/Right or Q/E choose  |  Enter confirm".to_string(),
        UserModeScreen::ArenaSelect => "Left/Right or Q/E choose map  |  Enter start".to_string(),
        _ => "Enter confirm".to_string(),
    }
}

fn user_mode_choice_message(user_mode: &UserModeState) -> String {
    match user_mode.screen {
        UserModeScreen::ModeSelect => mode_select_message(user_mode.menu_choice),
        UserModeScreen::CharacterSelect => {
            let player = match user_mode.character_select_player {
                CharacterSelectPlayer::PlayerOne => "P1",
                CharacterSelectPlayer::PlayerTwo => "P2",
            };
            format!(
                "{player} CHOOSE\n{}",
                character_select_message(user_mode.selected_character())
            )
        }
        UserModeScreen::ArenaSelect => arena_select_message(user_mode.arena_index),
        _ => character_select_message(user_mode.selected_character()),
    }
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

    match user_mode.play_mode {
        UserPlayMode::SinglePlayer => {
            format!(
                "Defeat the bot.\nArena: {}\n\n{}\n\nDash: double-tap movement\nGuard: {} + {}\n\n{}",
                arena,
                controls_player_message(0, bindings),
                control_key_label(bindings, 0, ControlAction::Heavy),
                control_key_label(bindings, 0, ControlAction::Light),
                status,
            )
        }
        UserPlayMode::TwoPlayers => {
            format!(
                "P1 and P2 share this keyboard.\nArena: {}\n\n{}\n\n{}\n\nDash: double-tap movement\nGuard: Heavy + Light\n\n{}",
                arena,
                controls_player_message(0, bindings),
                controls_player_message(1, bindings),
                status,
            )
        }
    }
}

fn controls_player_message(player: usize, bindings: &PlayerKeyBindings) -> String {
    format!(
        "P{}\nMove: {}/{}/{}/{}\nAim: {}\nHeavy / Throw: {}\nLight / Pickup / Item: {}\nJump: {}",
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
    match user_mode.result_winner {
        Some(USER_MODE_PLAYER_FIGHTER_ID) if user_mode.play_mode == UserPlayMode::TwoPlayers => {
            "P1 WINS".to_string()
        }
        Some(USER_MODE_BOT_FIGHTER_ID) if user_mode.play_mode == UserPlayMode::TwoPlayers => {
            "P2 WINS".to_string()
        }
        Some(USER_MODE_PLAYER_FIGHTER_ID) => "YOU WIN".to_string(),
        Some(USER_MODE_BOT_FIGHTER_ID) => "YOU LOSE".to_string(),
        _ => "DRAW".to_string(),
    }
}

fn result_sfx_kind(user_mode: &UserModeState) -> Option<CombatSfxKind> {
    match user_mode.result_winner {
        Some(USER_MODE_PLAYER_FIGHTER_ID) => Some(CombatSfxKind::ResultWin),
        Some(USER_MODE_BOT_FIGHTER_ID) if user_mode.play_mode == UserPlayMode::TwoPlayers => {
            Some(CombatSfxKind::ResultWin)
        }
        Some(USER_MODE_BOT_FIGHTER_ID) => Some(CombatSfxKind::ResultLose),
        _ => None,
    }
}

fn result_choice_message(choice: UserModeResultChoice) -> String {
    let play_again = if choice == UserModeResultChoice::PlayAgain {
        "> PLAY AGAIN <"
    } else {
        "  Play Again  "
    };
    let choose_character = if choice == UserModeResultChoice::ChooseCharacter {
        "> CHOOSE CHARACTER <"
    } else {
        "  Choose Character  "
    };
    format!("{play_again}        {choose_character}")
}

fn start_user_mode_menu_music(commands: &mut Commands, asset_server: &AssetServer) {
    commands.spawn((
        UserModeMusic,
        AudioPlayer::new(asset_server.load(USER_MODE_MENU_MUSIC_PATH)),
        PlaybackSettings::LOOP,
    ));
}

fn user_mode_battle_music_path(arena_index: usize) -> &'static str {
    USER_MODE_BATTLE_MUSIC_PATHS
        .get(arena_index)
        .copied()
        .unwrap_or(USER_MODE_BATTLE_MUSIC_PATHS[0])
}

fn start_user_mode_battle_music(
    commands: &mut Commands,
    asset_server: &AssetServer,
    arena_index: usize,
) {
    commands.spawn((
        UserModeMusic,
        UserModeBattleMusic,
        AudioPlayer::new(asset_server.load(user_mode_battle_music_path(arena_index))),
        PlaybackSettings::LOOP,
    ));
}

fn stop_user_mode_music(commands: &mut Commands, music: &Query<Entity, With<UserModeMusic>>) {
    for entity in music {
        commands.entity(entity).despawn();
    }
}

fn stop_user_mode_battle_music(
    commands: &mut Commands,
    music: &Query<Entity, With<UserModeBattleMusic>>,
) {
    for entity in music {
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
    let player_character = user_mode.p1_character;
    let opponent_character = if user_mode.play_mode == UserPlayMode::TwoPlayers {
        user_mode.p2_character
    } else {
        opposite_user_mode_character(player_character)
    };

    setup.set_rule(USER_MODE_STOCK_RULE_INDEX);
    setup.arena_index = user_mode
        .arena_index
        .min(arena_definitions().len().saturating_sub(1));
    if user_mode.play_mode == UserPlayMode::SinglePlayer {
        setup.configure_single_player_duel(player_character, opponent_character);
    } else {
        setup.configure_two_player_duel(player_character, opponent_character);
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
    let player_stock = state.stock_for(USER_MODE_PLAYER_FIGHTER_ID).unwrap_or(0);
    let bot_stock = state.stock_for(USER_MODE_BOT_FIGHTER_ID).unwrap_or(0);
    match player_stock.cmp(&bot_stock) {
        std::cmp::Ordering::Greater => Some(USER_MODE_PLAYER_FIGHTER_ID),
        std::cmp::Ordering::Less => Some(USER_MODE_BOT_FIGHTER_ID),
        std::cmp::Ordering::Equal => None,
    }
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
        assert_eq!(user_mode.menu_choice, UserModeMenuChoice::SinglePlayer);
        assert!(!user_mode.controls_briefing_seen);
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
    fn arena_select_message_lists_maps_in_readable_rows() {
        let message = arena_select_message(5);

        assert!(message.contains("> BUMPER ALLEY <"));
        assert!(message.contains("CROWN RING"));
        assert!(message.contains("POWDER KEG COURT"));
        assert_eq!(message.lines().count(), 3);
        assert!(message.lines().all(|line| line.chars().count() <= 80));
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
        user_mode.p1_character = CharacterKind::Pig;
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
        user_mode.p1_character = CharacterKind::Pig;

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
        user_mode.p1_character = CharacterKind::Pig;
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
        user_mode.p1_character = CharacterKind::Cat;
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
        user_mode.p1_character = CharacterKind::Bee;
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
        assert!(message.contains("Press Enter or click to fight"));
    }

    #[test]
    fn controls_briefing_copy_shows_two_player_columns_and_loading_state() {
        let mut user_mode = UserModeState::default();
        user_mode.play_mode = UserPlayMode::TwoPlayers;
        let bindings = PlayerKeyBindings::default();

        let message = controls_briefing_message(&user_mode, &bindings, false);

        assert!(message.contains("P1 and P2 share this keyboard"));
        assert!(message.contains("Arena: Crown Ring"));
        assert!(message.contains("P1\nMove: Left Arrow/Right Arrow/Up Arrow/Down Arrow"));
        assert!(message.contains("P2\nMove: A/D/W/S"));
        assert!(message.contains("Loading battle"));
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

        user_mode.move_key_column(-1);
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
            .apply_key_capture(&mut bindings, KeyCode::KeyB)
            .unwrap();

        assert_eq!(
            bindings.key_for(result.capture.player, result.capture.action),
            Some(KeyCode::KeyB)
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
