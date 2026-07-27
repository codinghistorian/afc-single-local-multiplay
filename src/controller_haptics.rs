use bevy::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[cfg(all(
    not(target_os = "macos"),
    not(target_arch = "wasm32"),
    feature = "native"
))]
use std::time::Duration;

#[cfg(all(
    not(target_os = "macos"),
    not(target_arch = "wasm32"),
    feature = "native"
))]
use bevy::input::gamepad::{GamepadRumbleIntensity, GamepadRumbleRequest};

use crate::components::{Controller, LocalInputAssignment};
use crate::control_settings::ControlPreferences;

#[cfg(target_arch = "wasm32")]
use js_sys::{Function, Object, Promise, Reflect};
#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::{JsCast, JsValue};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::{JsFuture, spawn_local};

const MAX_HAPTIC_SEGMENTS: usize = 3;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VibrationLevel {
    Off,
    Low,
    #[default]
    Standard,
    High,
}

impl VibrationLevel {
    pub const fn next(self) -> Self {
        match self {
            Self::Off => Self::Low,
            Self::Low => Self::Standard,
            Self::Standard => Self::High,
            Self::High => Self::Off,
        }
    }

    pub const fn previous(self) -> Self {
        match self {
            Self::Off => Self::High,
            Self::Low => Self::Off,
            Self::Standard => Self::Low,
            Self::High => Self::Standard,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Off => "OFF",
            Self::Low => "LOW",
            Self::Standard => "STANDARD",
            Self::High => "HIGH",
        }
    }

    pub const fn magnitude_scale(self) -> f32 {
        match self {
            Self::Off => 0.0,
            Self::Low => 0.6,
            Self::Standard => 1.0,
            Self::High => 1.25,
        }
    }

    pub const fn enabled(self) -> bool {
        !matches!(self, Self::Off)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HapticAvailability {
    #[default]
    Unknown,
    #[cfg_attr(
        not(any(target_os = "macos", target_arch = "wasm32")),
        allow(dead_code)
    )]
    ApiReady,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Supported,
    Unsupported,
}

impl HapticAvailability {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unknown => "SYSTEM DEPENDENT",
            Self::ApiReady => "API READY — TEST",
            Self::Supported => "COMMAND ACCEPTED",
            Self::Unsupported => "NOT SUPPORTED",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HapticSegmentKind {
    Transient,
    Continuous,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HapticSegment {
    pub start_ms: u16,
    pub duration_ms: u16,
    pub strong: f32,
    pub weak: f32,
    pub kind: HapticSegmentKind,
}

impl HapticSegment {
    pub const fn transient(start_ms: u16, duration_ms: u16, strong: f32, weak: f32) -> Self {
        Self {
            start_ms,
            duration_ms,
            strong,
            weak,
            kind: HapticSegmentKind::Transient,
        }
    }

    pub const fn continuous(start_ms: u16, duration_ms: u16, strong: f32, weak: f32) -> Self {
        Self {
            start_ms,
            duration_ms,
            strong,
            weak,
            kind: HapticSegmentKind::Continuous,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HapticPattern {
    segments: [Option<HapticSegment>; MAX_HAPTIC_SEGMENTS],
    pub priority: u8,
}

impl HapticPattern {
    pub const fn new(
        first: HapticSegment,
        second: Option<HapticSegment>,
        third: Option<HapticSegment>,
        priority: u8,
    ) -> Self {
        Self {
            segments: [Some(first), second, third],
            priority,
        }
    }

    pub const fn simple(strength: f32, duration_secs: f32, priority: u8) -> Self {
        let duration_ms = (duration_secs * 1000.0) as u16;
        Self::new(
            HapticSegment::continuous(0, duration_ms, strength, strength * 0.65),
            None,
            None,
            priority,
        )
    }

    pub fn segments(self) -> impl Iterator<Item = HapticSegment> {
        self.segments.into_iter().flatten()
    }

    pub fn scaled(self, level: VibrationLevel) -> Self {
        let scale = level.magnitude_scale();
        let mut scaled = self;
        for segment in scaled.segments.iter_mut().flatten() {
            segment.strong = (segment.strong * scale).clamp(0.0, 1.0);
            segment.weak = (segment.weak * scale).clamp(0.0, 1.0);
        }
        scaled
    }

    pub fn duration_ms(self) -> u16 {
        self.segments()
            .map(|segment| segment.start_ms.saturating_add(segment.duration_ms))
            .max()
            .unwrap_or(0)
    }
}

pub const fn controller_test_pattern() -> HapticPattern {
    HapticPattern::new(
        HapticSegment::continuous(0, 180, 1.0, 0.08),
        Some(HapticSegment::continuous(300, 180, 0.08, 1.0)),
        Some(HapticSegment::continuous(600, 360, 0.85, 0.85)),
        100,
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CombatHapticRole {
    Attacker,
    Defender,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CombatHapticKind {
    Light,
    Guard,
    Heavy,
    Ultimate,
    Finisher,
    Secondary,
}

pub const fn combat_haptic_pattern(
    kind: CombatHapticKind,
    role: CombatHapticRole,
) -> HapticPattern {
    match (kind, role) {
        (CombatHapticKind::Light, CombatHapticRole::Attacker) => {
            HapticPattern::new(HapticSegment::transient(0, 28, 0.08, 0.42), None, None, 40)
        }
        (CombatHapticKind::Light, CombatHapticRole::Defender) => {
            HapticPattern::new(HapticSegment::continuous(0, 58, 0.44, 0.18), None, None, 40)
        }
        (CombatHapticKind::Guard, CombatHapticRole::Attacker) => {
            HapticPattern::new(HapticSegment::transient(0, 24, 0.05, 0.30), None, None, 55)
        }
        (CombatHapticKind::Guard, CombatHapticRole::Defender) => {
            HapticPattern::new(HapticSegment::transient(0, 38, 0.18, 0.48), None, None, 55)
        }
        (CombatHapticKind::Heavy, CombatHapticRole::Attacker) => HapticPattern::new(
            HapticSegment::transient(0, 32, 0.14, 0.62),
            Some(HapticSegment::continuous(40, 40, 0.18, 0.16)),
            None,
            70,
        ),
        (CombatHapticKind::Heavy, CombatHapticRole::Defender) => HapticPattern::new(
            HapticSegment::transient(0, 32, 0.55, 0.55),
            Some(HapticSegment::continuous(32, 78, 0.68, 0.12)),
            None,
            70,
        ),
        (CombatHapticKind::Ultimate, CombatHapticRole::Attacker) => HapticPattern::new(
            HapticSegment::transient(0, 35, 0.20, 0.75),
            Some(HapticSegment::continuous(55, 70, 0.42, 0.25)),
            None,
            90,
        ),
        (CombatHapticKind::Ultimate, CombatHapticRole::Defender) => HapticPattern::new(
            HapticSegment::transient(0, 35, 0.78, 0.70),
            Some(HapticSegment::continuous(35, 115, 0.88, 0.12)),
            None,
            90,
        ),
        (CombatHapticKind::Finisher, CombatHapticRole::Attacker) => HapticPattern::new(
            HapticSegment::transient(0, 35, 0.18, 0.65),
            Some(HapticSegment::continuous(55, 80, 0.40, 0.18)),
            None,
            100,
        ),
        (CombatHapticKind::Finisher, CombatHapticRole::Defender) => HapticPattern::new(
            HapticSegment::transient(0, 35, 0.82, 0.65),
            Some(HapticSegment::continuous(35, 180, 0.85, 0.10)),
            None,
            100,
        ),
        (CombatHapticKind::Secondary, CombatHapticRole::Defender) => {
            HapticPattern::new(HapticSegment::continuous(0, 70, 0.38, 0.12), None, None, 25)
        }
        (CombatHapticKind::Secondary, CombatHapticRole::Attacker) => {
            HapticPattern::new(HapticSegment::transient(0, 1, 0.0, 0.0), None, None, 0)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CombatHapticCue {
    pub attacker: Option<usize>,
    pub defender: usize,
    pub kind: CombatHapticKind,
}

impl CombatHapticCue {
    pub const fn impact(attacker: Option<usize>, defender: usize, kind: CombatHapticKind) -> Self {
        Self {
            attacker,
            defender,
            kind,
        }
    }

    pub const fn secondary(defender: usize) -> Self {
        Self::impact(None, defender, CombatHapticKind::Secondary)
    }
}

#[derive(Resource, Default)]
pub struct CombatHapticQueue {
    cues: Vec<CombatHapticCue>,
}

impl CombatHapticQueue {
    pub fn push(&mut self, cue: CombatHapticCue) {
        self.cues.push(cue);
    }

    fn drain(&mut self) -> impl Iterator<Item = CombatHapticCue> + '_ {
        self.cues.drain(..)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HapticPurpose {
    #[default]
    Gameplay,
    Join,
    Test,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ControllerHapticCommand {
    Play(HapticPattern),
    Stop,
}

#[derive(Message, Clone, Copy, Debug, PartialEq)]
pub struct ControllerHapticRequest {
    pub gamepad: Entity,
    pub purpose: HapticPurpose,
    pub command: ControllerHapticCommand,
}

impl ControllerHapticRequest {
    pub const fn play(gamepad: Entity, purpose: HapticPurpose, pattern: HapticPattern) -> Self {
        Self {
            gamepad,
            purpose,
            command: ControllerHapticCommand::Play(pattern),
        }
    }

    pub const fn stop(gamepad: Entity) -> Self {
        Self {
            gamepad,
            purpose: HapticPurpose::Gameplay,
            command: ControllerHapticCommand::Stop,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HapticPlaybackResult {
    Started,
    Completed,
    Preempted,
    Unsupported,
    Failed(String),
}

#[derive(Message, Clone, Debug, PartialEq, Eq)]
pub struct HapticPlaybackEvent {
    pub gamepad: Entity,
    pub purpose: HapticPurpose,
    pub result: HapticPlaybackResult,
}

pub struct ControllerHapticsPlugin;

#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ControllerHapticsSystems {
    Route,
    Playback,
}

impl Plugin for ControllerHapticsPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<ControllerHapticRequest>()
            .add_message::<HapticPlaybackEvent>()
            .init_resource::<CombatHapticQueue>()
            .init_resource::<CombatHapticMixer>()
            .configure_sets(
                PostUpdate,
                (
                    ControllerHapticsSystems::Route,
                    ControllerHapticsSystems::Playback,
                )
                    .chain(),
            )
            .add_systems(
                PostUpdate,
                route_combat_haptics.in_set(ControllerHapticsSystems::Route),
            );

        #[cfg(all(
            not(target_os = "macos"),
            not(target_arch = "wasm32"),
            feature = "native"
        ))]
        app.init_resource::<ScheduledHapticSegments>().add_systems(
            PostUpdate,
            play_bevy_haptics
                .in_set(ControllerHapticsSystems::Playback)
                .before(bevy::gilrs::RumbleSystems),
        );

        #[cfg(target_arch = "wasm32")]
        app.init_resource::<WebHapticSchedule>().add_systems(
            PostUpdate,
            (sync_web_haptic_devices, play_web_haptics)
                .chain()
                .in_set(ControllerHapticsSystems::Playback),
        );
    }
}

#[derive(Clone, Copy)]
struct MixedHapticState {
    last_kind: CombatHapticKind,
    last_started: f64,
    active_priority: u8,
    active_until: f64,
}

#[derive(Resource, Default)]
struct CombatHapticMixer {
    devices: HashMap<Entity, MixedHapticState>,
}

#[derive(Clone, Copy)]
struct RoutedCombatHaptic {
    kind: CombatHapticKind,
    role: CombatHapticRole,
    pattern: HapticPattern,
}

fn route_combat_haptics(
    time: Res<Time<Real>>,
    preferences: Res<ControlPreferences>,
    controllers: Query<&Controller>,
    connected_gamepads: Query<(), With<Gamepad>>,
    mut queue: ResMut<CombatHapticQueue>,
    mut mixer: ResMut<CombatHapticMixer>,
    mut requests: MessageWriter<ControllerHapticRequest>,
) {
    let cues: Vec<_> = queue.drain().collect();
    if !preferences.vibration.enabled() {
        for gamepad in mixer.devices.keys().copied() {
            requests.write(ControllerHapticRequest::stop(gamepad));
        }
        mixer.devices.clear();
        return;
    }

    let gamepad_for_slot = |slot: usize| {
        controllers.iter().find_map(|controller| {
            (controller.slot.index() == slot && controller.is_human())
                .then_some(controller.input)
                .and_then(|assignment| match assignment {
                    LocalInputAssignment::Gamepad(gamepad)
                        if connected_gamepads.get(gamepad).is_ok() =>
                    {
                        Some(gamepad)
                    }
                    _ => None,
                })
        })
    };

    let mut strongest_by_gamepad = HashMap::<Entity, RoutedCombatHaptic>::new();
    let mut route = |gamepad: Entity, kind: CombatHapticKind, role: CombatHapticRole| {
        if kind == CombatHapticKind::Secondary && role == CombatHapticRole::Attacker {
            return;
        }
        let pattern = combat_haptic_pattern(kind, role);
        let candidate = RoutedCombatHaptic {
            kind,
            role,
            pattern,
        };
        strongest_by_gamepad
            .entry(gamepad)
            .and_modify(|current| {
                if candidate.pattern.priority > current.pattern.priority
                    || (candidate.pattern.priority == current.pattern.priority
                        && candidate.role == CombatHapticRole::Defender)
                {
                    *current = candidate;
                }
            })
            .or_insert(candidate);
    };

    for cue in cues {
        if let Some(attacker) = cue.attacker
            && attacker != cue.defender
            && let Some(gamepad) = gamepad_for_slot(attacker)
        {
            route(gamepad, cue.kind, CombatHapticRole::Attacker);
        }
        if let Some(gamepad) = gamepad_for_slot(cue.defender) {
            route(gamepad, cue.kind, CombatHapticRole::Defender);
        }
    }

    let now = time.elapsed_secs_f64();
    for (gamepad, routed) in strongest_by_gamepad {
        if let Some(state) = mixer.devices.get(&gamepad) {
            let duplicate_inside_gate =
                state.last_kind == routed.kind && now - state.last_started < 0.025;
            let protected_finisher = state.active_until > now
                && state.active_priority >= 90
                && routed.pattern.priority < state.active_priority;
            if duplicate_inside_gate || protected_finisher {
                continue;
            }
        }
        let pattern = routed.pattern.scaled(preferences.vibration);
        requests.write(ControllerHapticRequest::play(
            gamepad,
            HapticPurpose::Gameplay,
            pattern,
        ));
        mixer.devices.insert(
            gamepad,
            MixedHapticState {
                last_kind: routed.kind,
                last_started: now,
                active_priority: pattern.priority,
                active_until: now + f64::from(pattern.duration_ms()) / 1000.0,
            },
        );
    }
}

pub fn queue_simple_haptic(
    requests: &mut MessageWriter<ControllerHapticRequest>,
    level: VibrationLevel,
    gamepad: Entity,
    strength: f32,
    duration_secs: f32,
    purpose: HapticPurpose,
) -> bool {
    queue_haptic_pattern(
        requests,
        level,
        gamepad,
        HapticPattern::simple(strength.clamp(0.0, 1.0), duration_secs.max(0.0), 20),
        purpose,
    )
}

pub fn queue_haptic_pattern(
    requests: &mut MessageWriter<ControllerHapticRequest>,
    level: VibrationLevel,
    gamepad: Entity,
    pattern: HapticPattern,
    purpose: HapticPurpose,
) -> bool {
    if !level.enabled() {
        return false;
    }
    requests.write(ControllerHapticRequest::play(
        gamepad,
        purpose,
        pattern.scaled(level),
    ));
    true
}

#[cfg(all(
    not(target_os = "macos"),
    not(target_arch = "wasm32"),
    feature = "native"
))]
#[derive(Clone, Copy)]
struct ScheduledHapticSegment {
    gamepad: Entity,
    starts_at: f64,
    segment: HapticSegment,
}

#[cfg(all(
    not(target_os = "macos"),
    not(target_arch = "wasm32"),
    feature = "native"
))]
#[derive(Resource, Default)]
struct ScheduledHapticSegments(Vec<ScheduledHapticSegment>);

#[cfg(all(
    not(target_os = "macos"),
    not(target_arch = "wasm32"),
    feature = "native"
))]
fn play_bevy_haptics(
    time: Res<Time<Real>>,
    mut haptic_requests: MessageReader<ControllerHapticRequest>,
    mut playback_events: MessageWriter<HapticPlaybackEvent>,
    mut rumble_requests: MessageWriter<GamepadRumbleRequest>,
    mut scheduled: ResMut<ScheduledHapticSegments>,
) {
    let now = time.elapsed_secs_f64();
    for request in haptic_requests.read() {
        match request.command {
            ControllerHapticCommand::Stop => {
                scheduled
                    .0
                    .retain(|segment| segment.gamepad != request.gamepad);
                rumble_requests.write(GamepadRumbleRequest::Stop {
                    gamepad: request.gamepad,
                });
            }
            ControllerHapticCommand::Play(pattern) => {
                scheduled
                    .0
                    .retain(|segment| segment.gamepad != request.gamepad);
                rumble_requests.write(GamepadRumbleRequest::Stop {
                    gamepad: request.gamepad,
                });
                scheduled
                    .0
                    .extend(pattern.segments().map(|segment| ScheduledHapticSegment {
                        gamepad: request.gamepad,
                        starts_at: now + f64::from(segment.start_ms) / 1000.0,
                        segment,
                    }));
                playback_events.write(HapticPlaybackEvent {
                    gamepad: request.gamepad,
                    purpose: request.purpose,
                    result: HapticPlaybackResult::Started,
                });
            }
        }
    }

    let mut waiting = Vec::with_capacity(scheduled.0.len());
    for scheduled_segment in scheduled.0.drain(..) {
        if scheduled_segment.starts_at > now {
            waiting.push(scheduled_segment);
            continue;
        }
        let segment = scheduled_segment.segment;
        rumble_requests.write(GamepadRumbleRequest::Add {
            gamepad: scheduled_segment.gamepad,
            duration: Duration::from_millis(u64::from(segment.duration_ms)),
            intensity: GamepadRumbleIntensity {
                strong_motor: segment.strong,
                weak_motor: segment.weak,
            },
        });
    }
    scheduled.0 = waiting;
}

#[cfg(target_arch = "wasm32")]
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
struct BrowserGamepadIndex(u32);

#[cfg(target_arch = "wasm32")]
#[derive(Clone)]
struct BrowserGamepadSnapshot {
    index: u32,
    id: String,
    haptics: HapticAvailability,
}

#[cfg(target_arch = "wasm32")]
fn browser_gamepads() -> Result<Vec<BrowserGamepadSnapshot>, String> {
    let window = web_sys::window().ok_or_else(|| "browser window is unavailable".to_string())?;
    let navigator =
        Reflect::get(window.as_ref(), &JsValue::from_str("navigator")).map_err(js_error_message)?;
    let get_gamepads = Reflect::get(&navigator, &JsValue::from_str("getGamepads"))
        .map_err(js_error_message)?
        .dyn_into::<Function>()
        .map_err(|_| "navigator.getGamepads is unavailable".to_string())?;
    let values = get_gamepads.call0(&navigator).map_err(js_error_message)?;
    let length = Reflect::get(&values, &JsValue::from_str("length"))
        .map_err(js_error_message)?
        .as_f64()
        .unwrap_or(0.0) as u32;
    let mut gamepads = Vec::new();
    for offset in 0..length {
        let gamepad = Reflect::get(&values, &JsValue::from_f64(f64::from(offset)))
            .map_err(js_error_message)?;
        if gamepad.is_null() || gamepad.is_undefined() {
            continue;
        }
        let connected = Reflect::get(&gamepad, &JsValue::from_str("connected"))
            .ok()
            .and_then(|value| value.as_bool())
            .unwrap_or(true);
        if !connected {
            continue;
        }
        let index = Reflect::get(&gamepad, &JsValue::from_str("index"))
            .ok()
            .and_then(|value| value.as_f64())
            .unwrap_or(f64::from(offset)) as u32;
        let id = Reflect::get(&gamepad, &JsValue::from_str("id"))
            .ok()
            .and_then(|value| value.as_string())
            .unwrap_or_else(|| "Gamepad".to_string());
        let haptics = gamepad_haptic_actuator(&gamepad)
            .map(|_| HapticAvailability::ApiReady)
            .unwrap_or(HapticAvailability::Unsupported);
        gamepads.push(BrowserGamepadSnapshot { index, id, haptics });
    }
    gamepads.sort_by_key(|gamepad| gamepad.index);
    Ok(gamepads)
}

#[cfg(target_arch = "wasm32")]
fn gamepad_haptic_actuator(gamepad: &JsValue) -> Option<JsValue> {
    let actuator = Reflect::get(gamepad, &JsValue::from_str("vibrationActuator")).ok()?;
    if actuator.is_null() || actuator.is_undefined() {
        return None;
    }
    Reflect::get(&actuator, &JsValue::from_str("playEffect"))
        .ok()?
        .dyn_into::<Function>()
        .ok()?;
    Some(actuator)
}

#[cfg(target_arch = "wasm32")]
fn browser_gamepad_value(index: u32) -> Result<JsValue, String> {
    let window = web_sys::window().ok_or_else(|| "browser window is unavailable".to_string())?;
    let navigator =
        Reflect::get(window.as_ref(), &JsValue::from_str("navigator")).map_err(js_error_message)?;
    let get_gamepads = Reflect::get(&navigator, &JsValue::from_str("getGamepads"))
        .map_err(js_error_message)?
        .dyn_into::<Function>()
        .map_err(|_| "navigator.getGamepads is unavailable".to_string())?;
    let values = get_gamepads.call0(&navigator).map_err(js_error_message)?;
    let gamepad =
        Reflect::get(&values, &JsValue::from_f64(f64::from(index))).map_err(js_error_message)?;
    if gamepad.is_null() || gamepad.is_undefined() {
        Err("browser gamepad disconnected".to_string())
    } else {
        Ok(gamepad)
    }
}

#[cfg(target_arch = "wasm32")]
fn sync_web_haptic_devices(
    mut commands: Commands,
    mut gamepads: Query<
        (
            Entity,
            &mut crate::control_settings::ControllerDeviceInfo,
            Option<&BrowserGamepadIndex>,
        ),
        With<Gamepad>,
    >,
) {
    let Ok(browser_gamepads) = browser_gamepads() else {
        return;
    };
    let mut claimed = Vec::new();
    let mut unassigned = Vec::new();
    for (entity, info, index) in &mut gamepads {
        if let Some(index) = index
            && browser_gamepads
                .iter()
                .any(|gamepad| gamepad.index == index.0)
        {
            claimed.push(index.0);
        } else {
            unassigned.push((entity, info.display_name.clone()));
        }
    }
    unassigned.sort_by_key(|(entity, _)| entity.to_bits());

    let mut assigned_entities = Vec::new();
    for (entity, display_name) in &unassigned {
        let normalized = display_name.to_ascii_lowercase();
        let candidate = browser_gamepads.iter().find(|gamepad| {
            !claimed.contains(&gamepad.index) && gamepad.id.to_ascii_lowercase() == normalized
        });
        if let Some(candidate) = candidate {
            claimed.push(candidate.index);
            assigned_entities.push(*entity);
            commands
                .entity(*entity)
                .insert(BrowserGamepadIndex(candidate.index));
        }
    }
    for (entity, _) in unassigned {
        if assigned_entities.contains(&entity) {
            continue;
        }
        if let Some(candidate) = browser_gamepads
            .iter()
            .find(|gamepad| !claimed.contains(&gamepad.index))
        {
            claimed.push(candidate.index);
            commands
                .entity(entity)
                .insert(BrowserGamepadIndex(candidate.index));
        }
    }

    for (_, mut info, index) in &mut gamepads {
        let Some(index) = index else {
            info.haptics = HapticAvailability::Unknown;
            continue;
        };
        let availability = browser_gamepads
            .iter()
            .find(|gamepad| gamepad.index == index.0)
            .map(|gamepad| gamepad.haptics)
            .unwrap_or(HapticAvailability::Unsupported);
        info.haptics = availability;
    }
}

#[cfg(target_arch = "wasm32")]
#[derive(Clone, Copy)]
struct ScheduledWebHapticSegment {
    gamepad: Entity,
    browser_index: u32,
    purpose: HapticPurpose,
    starts_at: f64,
    segment: HapticSegment,
    final_segment: bool,
}

#[cfg(target_arch = "wasm32")]
#[derive(Clone, Copy)]
struct ActiveWebHaptic {
    purpose: HapticPurpose,
    ends_at: f64,
}

#[cfg(target_arch = "wasm32")]
#[derive(Resource, Default)]
struct WebHapticSchedule {
    pending: Vec<ScheduledWebHapticSegment>,
    active: HashMap<Entity, ActiveWebHaptic>,
}

#[cfg(target_arch = "wasm32")]
#[derive(Clone)]
struct AsyncWebHapticResult {
    gamepad: Entity,
    purpose: HapticPurpose,
    result: HapticPlaybackResult,
}

#[cfg(target_arch = "wasm32")]
thread_local! {
    static WEB_HAPTIC_RESULTS: RefCell<Vec<AsyncWebHapticResult>> = const { RefCell::new(Vec::new()) };
}

#[cfg(target_arch = "wasm32")]
fn reset_web_haptics(index: u32) {
    let Ok(gamepad) = browser_gamepad_value(index) else {
        return;
    };
    let Some(actuator) = gamepad_haptic_actuator(&gamepad) else {
        return;
    };
    if let Ok(reset) = Reflect::get(&actuator, &JsValue::from_str("reset"))
        && let Ok(reset) = reset.dyn_into::<Function>()
    {
        let _ = reset.call0(&actuator);
    }
}

#[cfg(target_arch = "wasm32")]
fn play_web_haptic_segment(segment: ScheduledWebHapticSegment) -> Result<(), String> {
    let gamepad = browser_gamepad_value(segment.browser_index)?;
    let actuator = gamepad_haptic_actuator(&gamepad)
        .ok_or_else(|| "controller has no dual-rumble actuator".to_string())?;
    let play_effect = Reflect::get(&actuator, &JsValue::from_str("playEffect"))
        .map_err(js_error_message)?
        .dyn_into::<Function>()
        .map_err(|_| "vibrationActuator.playEffect is unavailable".to_string())?;
    let parameters = Object::new();
    Reflect::set(
        &parameters,
        &JsValue::from_str("startDelay"),
        &JsValue::from_f64(0.0),
    )
    .map_err(js_error_message)?;
    Reflect::set(
        &parameters,
        &JsValue::from_str("duration"),
        &JsValue::from_f64(f64::from(segment.segment.duration_ms)),
    )
    .map_err(js_error_message)?;
    Reflect::set(
        &parameters,
        &JsValue::from_str("strongMagnitude"),
        &JsValue::from_f64(f64::from(segment.segment.strong)),
    )
    .map_err(js_error_message)?;
    Reflect::set(
        &parameters,
        &JsValue::from_str("weakMagnitude"),
        &JsValue::from_f64(f64::from(segment.segment.weak)),
    )
    .map_err(js_error_message)?;
    let result = play_effect
        .call2(
            &actuator,
            &JsValue::from_str("dual-rumble"),
            parameters.as_ref(),
        )
        .map_err(js_error_message)?;
    let promise = Promise::resolve(&result);
    if segment.final_segment {
        spawn_local(async move {
            let result = match JsFuture::from(promise).await {
                Ok(value) if value.as_string().as_deref() == Some("preempted") => {
                    HapticPlaybackResult::Preempted
                }
                Ok(_) => HapticPlaybackResult::Completed,
                Err(error) => HapticPlaybackResult::Failed(js_error_message(error)),
            };
            WEB_HAPTIC_RESULTS.with_borrow_mut(|results| {
                results.push(AsyncWebHapticResult {
                    gamepad: segment.gamepad,
                    purpose: segment.purpose,
                    result,
                });
            });
        });
    }
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn play_web_haptics(
    time: Res<Time<Real>>,
    preferences: Res<ControlPreferences>,
    browser_indices: Query<&BrowserGamepadIndex>,
    mut requests: MessageReader<ControllerHapticRequest>,
    mut playback_events: MessageWriter<HapticPlaybackEvent>,
    mut schedule: ResMut<WebHapticSchedule>,
) {
    WEB_HAPTIC_RESULTS.with_borrow_mut(|results| {
        for result in results.drain(..) {
            schedule.active.remove(&result.gamepad);
            playback_events.write(HapticPlaybackEvent {
                gamepad: result.gamepad,
                purpose: result.purpose,
                result: result.result,
            });
        }
    });

    let now = time.elapsed_secs_f64();
    for request in requests.read() {
        let Ok(browser_index) = browser_indices.get(request.gamepad) else {
            playback_events.write(HapticPlaybackEvent {
                gamepad: request.gamepad,
                purpose: request.purpose,
                result: HapticPlaybackResult::Unsupported,
            });
            continue;
        };
        schedule
            .pending
            .retain(|segment| segment.gamepad != request.gamepad);
        if let Some(active) = schedule.active.remove(&request.gamepad) {
            reset_web_haptics(browser_index.0);
            playback_events.write(HapticPlaybackEvent {
                gamepad: request.gamepad,
                purpose: active.purpose,
                result: HapticPlaybackResult::Preempted,
            });
        }
        match request.command {
            ControllerHapticCommand::Stop => reset_web_haptics(browser_index.0),
            ControllerHapticCommand::Play(pattern) if preferences.vibration.enabled() => {
                let segments: Vec<_> = pattern.segments().collect();
                let last = segments.len().saturating_sub(1);
                schedule
                    .pending
                    .extend(segments.into_iter().enumerate().map(|(index, segment)| {
                        ScheduledWebHapticSegment {
                            gamepad: request.gamepad,
                            browser_index: browser_index.0,
                            purpose: request.purpose,
                            starts_at: now + f64::from(segment.start_ms) / 1000.0,
                            segment,
                            final_segment: index == last,
                        }
                    }));
                schedule.active.insert(
                    request.gamepad,
                    ActiveWebHaptic {
                        purpose: request.purpose,
                        ends_at: now + f64::from(pattern.duration_ms()) / 1000.0,
                    },
                );
                playback_events.write(HapticPlaybackEvent {
                    gamepad: request.gamepad,
                    purpose: request.purpose,
                    result: HapticPlaybackResult::Started,
                });
            }
            ControllerHapticCommand::Play(_) => reset_web_haptics(browser_index.0),
        }
    }

    let pending = std::mem::take(&mut schedule.pending);
    let mut waiting = Vec::with_capacity(pending.len());
    for segment in pending {
        if segment.starts_at > now {
            waiting.push(segment);
            continue;
        }
        if let Err(error) = play_web_haptic_segment(segment) {
            schedule.active.remove(&segment.gamepad);
            playback_events.write(HapticPlaybackEvent {
                gamepad: segment.gamepad,
                purpose: segment.purpose,
                result: if error.contains("no dual-rumble") {
                    HapticPlaybackResult::Unsupported
                } else {
                    HapticPlaybackResult::Failed(error)
                },
            });
        }
    }
    schedule.pending = waiting;
    schedule
        .active
        .retain(|_, active| active.ends_at + 0.25 > now);
}

#[cfg(target_arch = "wasm32")]
fn js_error_message(error: JsValue) -> String {
    error
        .as_string()
        .or_else(|| {
            Reflect::get(&error, &JsValue::from_str("message"))
                .ok()
                .and_then(|message| message.as_string())
        })
        .unwrap_or_else(|| format!("{error:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{ParticipantKind, PlayerSlotId};

    #[test]
    fn vibration_levels_cycle_and_scale_without_changing_timing() {
        assert_eq!(VibrationLevel::Off.next(), VibrationLevel::Low);
        assert_eq!(VibrationLevel::High.next(), VibrationLevel::Off);
        let standard = controller_test_pattern();
        let low = standard.scaled(VibrationLevel::Low);
        let high = standard.scaled(VibrationLevel::High);
        assert_eq!(low.duration_ms(), standard.duration_ms());
        assert_eq!(high.duration_ms(), standard.duration_ms());
        assert!((low.segments().next().unwrap().weak - 0.048).abs() < 0.001);
        assert!(high.segments().all(|segment| segment.strong <= 1.0));
    }

    #[test]
    fn controller_test_pattern_is_short_and_three_stage() {
        let pattern = controller_test_pattern();
        assert_eq!(pattern.segments().count(), 3);
        assert_eq!(pattern.duration_ms(), 960);
        let segments: Vec<_> = pattern.segments().collect();
        assert!(segments[0].strong > segments[0].weak);
        assert!(segments[1].weak > segments[1].strong);
        assert_eq!(segments[2].strong, segments[2].weak);
    }

    #[test]
    fn combat_palette_keeps_attacker_crisp_and_defender_deep() {
        let attacker = combat_haptic_pattern(CombatHapticKind::Light, CombatHapticRole::Attacker);
        let defender = combat_haptic_pattern(CombatHapticKind::Light, CombatHapticRole::Defender);
        let attacker = attacker.segments().next().unwrap();
        let defender = defender.segments().next().unwrap();
        assert!(attacker.weak > attacker.strong);
        assert!(defender.strong > defender.weak);
        assert!(defender.duration_ms > attacker.duration_ms);
    }

    #[test]
    fn finisher_has_priority_and_duration_headroom() {
        let heavy = combat_haptic_pattern(CombatHapticKind::Heavy, CombatHapticRole::Defender);
        let finisher =
            combat_haptic_pattern(CombatHapticKind::Finisher, CombatHapticRole::Defender);
        assert!(finisher.priority > heavy.priority);
        assert_eq!(finisher.duration_ms(), 215);
    }

    #[test]
    fn resolved_hit_routes_distinct_roles_to_owned_gamepads() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<ControllerHapticRequest>()
            .init_resource::<CombatHapticQueue>()
            .init_resource::<CombatHapticMixer>()
            .init_resource::<ControlPreferences>()
            .add_systems(Update, route_combat_haptics);
        let attacker_gamepad = app.world_mut().spawn(Gamepad::default()).id();
        let defender_gamepad = app.world_mut().spawn(Gamepad::default()).id();
        app.world_mut().spawn(Controller::new(
            PlayerSlotId::new(0).unwrap(),
            ParticipantKind::Human,
            LocalInputAssignment::Gamepad(attacker_gamepad),
        ));
        app.world_mut().spawn(Controller::new(
            PlayerSlotId::new(1).unwrap(),
            ParticipantKind::Human,
            LocalInputAssignment::Gamepad(defender_gamepad),
        ));
        app.world_mut()
            .resource_mut::<CombatHapticQueue>()
            .push(CombatHapticCue::impact(Some(0), 1, CombatHapticKind::Light));

        app.update();

        let requests: Vec<_> = app
            .world_mut()
            .resource_mut::<Messages<ControllerHapticRequest>>()
            .drain()
            .collect();
        assert_eq!(requests.len(), 2);
        let attacker = requests
            .iter()
            .find(|request| request.gamepad == attacker_gamepad)
            .unwrap();
        let defender = requests
            .iter()
            .find(|request| request.gamepad == defender_gamepad)
            .unwrap();
        let ControllerHapticCommand::Play(attacker) = attacker.command else {
            panic!("attacker should receive a pattern");
        };
        let ControllerHapticCommand::Play(defender) = defender.command else {
            panic!("defender should receive a pattern");
        };
        let attacker = attacker.segments().next().unwrap();
        let defender = defender.segments().next().unwrap();
        assert!(attacker.weak > attacker.strong);
        assert!(defender.strong > defender.weak);
    }

    #[test]
    fn neutral_impacts_ignore_keyboard_and_bot_slots() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<ControllerHapticRequest>()
            .init_resource::<CombatHapticQueue>()
            .init_resource::<CombatHapticMixer>()
            .init_resource::<ControlPreferences>()
            .add_systems(Update, route_combat_haptics);
        app.world_mut().spawn(Controller::new(
            PlayerSlotId::new(0).unwrap(),
            ParticipantKind::Human,
            LocalInputAssignment::Keyboard(0),
        ));
        app.world_mut().spawn(Controller::new(
            PlayerSlotId::new(1).unwrap(),
            ParticipantKind::Bot,
            LocalInputAssignment::Unassigned,
        ));
        app.world_mut()
            .resource_mut::<CombatHapticQueue>()
            .push(CombatHapticCue::impact(None, 0, CombatHapticKind::Heavy));

        app.update();

        assert!(
            app.world_mut()
                .resource_mut::<Messages<ControllerHapticRequest>>()
                .drain()
                .next()
                .is_none()
        );
    }
}
