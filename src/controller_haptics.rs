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

use crate::components::{
    Controller, Fighter, FighterAction, FighterActionState, LocalInputAssignment,
};
use crate::control_settings::ControlPreferences;

#[cfg(target_arch = "wasm32")]
use js_sys::{Function, Object, Promise, Reflect};
#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::{JsCast, JsValue};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::{JsFuture, spawn_local};

const MAX_HAPTIC_SEGMENTS: usize = 8;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VibrationLevel {
    Off,
    Low,
    #[default]
    Standard,
    High,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HapticStyle {
    Minimal,
    #[default]
    Competitive,
    Cinematic,
}

impl VibrationLevel {
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
        let mut segments = [None; MAX_HAPTIC_SEGMENTS];
        segments[0] = Some(first);
        segments[1] = second;
        segments[2] = third;
        Self { segments, priority }
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

    pub fn scaled_duration(self, scale: f32) -> Self {
        let mut scaled = self;
        for segment in scaled.segments.iter_mut().flatten() {
            segment.start_ms = (f32::from(segment.start_ms) * scale)
                .round()
                .clamp(0.0, f32::from(u16::MAX)) as u16;
            segment.duration_ms = (f32::from(segment.duration_ms) * scale)
                .round()
                .clamp(1.0, f32::from(u16::MAX)) as u16;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CombatHapticRole {
    Attacker,
    Defender,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HapticMoveClass {
    Light,
    Heavy,
    Special,
    Guard,
    Counter,
    Ultimate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HapticActionPhase {
    Startup,
    Release,
    Charge,
    Lock,
    Aftermath,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HapticImpactWeight {
    Light,
    Heavy,
    Ultimate,
    Terminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HapticContactOutcome {
    Clean,
    Guarded,
    Counter,
    Grab,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HapticSecondaryKind {
    Bounce,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CombatHapticCue {
    Action {
        slot: usize,
        class: HapticMoveClass,
        phase: HapticActionPhase,
    },
    Contact {
        attacker: Option<usize>,
        defender: usize,
        outcome: HapticContactOutcome,
        weight: HapticImpactWeight,
    },
    Secondary {
        slot: usize,
        kind: HapticSecondaryKind,
    },
}

impl CombatHapticCue {
    pub const fn action(slot: usize, class: HapticMoveClass, phase: HapticActionPhase) -> Self {
        Self::Action { slot, class, phase }
    }

    pub const fn contact(
        attacker: Option<usize>,
        defender: usize,
        outcome: HapticContactOutcome,
        weight: HapticImpactWeight,
    ) -> Self {
        Self::Contact {
            attacker,
            defender,
            outcome,
            weight,
        }
    }

    pub const fn secondary(slot: usize) -> Self {
        Self::Secondary {
            slot,
            kind: HapticSecondaryKind::Bounce,
        }
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

#[derive(Resource, Default)]
pub struct FighterHapticTracker {
    actions: HashMap<Entity, FighterAction>,
}

pub const fn haptic_move_class_for_action(action: FighterAction) -> Option<HapticMoveClass> {
    match action {
        FighterAction::LightAttack1
        | FighterAction::LightAttack2
        | FighterAction::DashAttack
        | FighterAction::JumpAttack => Some(HapticMoveClass::Light),
        FighterAction::ComboFinisher
        | FighterAction::HeavyAttack
        | FighterAction::HeavyAttack2
        | FighterAction::JumpHeavyAttack
        | FighterAction::GrabStartup
        | FighterAction::Throwing
        | FighterAction::ItemSwing
        | FighterAction::ItemThrow => Some(HapticMoveClass::Heavy),
        FighterAction::SpecialCast => Some(HapticMoveClass::Special),
        FighterAction::Guarding => Some(HapticMoveClass::Guard),
        FighterAction::GuardCounter => Some(HapticMoveClass::Counter),
        FighterAction::UltimateStartup | FighterAction::UltimateRush => {
            Some(HapticMoveClass::Ultimate)
        }
        _ => None,
    }
}

fn action_transition_phase(action: FighterAction) -> HapticActionPhase {
    match action {
        FighterAction::UltimateRush => HapticActionPhase::Lock,
        FighterAction::Throwing | FighterAction::ItemThrow | FighterAction::SpecialCast => {
            HapticActionPhase::Release
        }
        _ => HapticActionPhase::Startup,
    }
}

pub fn queue_fighter_action_haptics(
    fighters: Query<(Entity, &Fighter, &Controller, &FighterActionState)>,
    mut tracker: ResMut<FighterHapticTracker>,
    mut haptics: ResMut<CombatHapticQueue>,
) {
    for (entity, fighter, controller, action) in &fighters {
        if !controller.is_human() || !matches!(controller.input, LocalInputAssignment::Gamepad(_)) {
            continue;
        }
        let previous = tracker.actions.insert(entity, action.action);
        if previous.is_none() || previous == Some(action.action) {
            continue;
        }
        if let Some(class) = haptic_move_class_for_action(action.action) {
            haptics.push(CombatHapticCue::action(
                fighter.id,
                class,
                action_transition_phase(action.action),
            ));
        }
    }
    tracker.actions.retain(|entity, _| {
        fighters.get(*entity).is_ok_and(|(_, _, controller, _)| {
            controller.is_human() && matches!(controller.input, LocalInputAssignment::Gamepad(_))
        })
    });
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HapticPurpose {
    #[default]
    Gameplay,
    Join,
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
            .init_resource::<FighterHapticTracker>()
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum RoutedHapticKind {
    Action(HapticMoveClass, HapticActionPhase),
    Contact(HapticContactOutcome, HapticImpactWeight, CombatHapticRole),
    Secondary(HapticSecondaryKind),
}

impl RoutedHapticKind {
    const fn category_rank(self) -> u8 {
        match self {
            Self::Action(_, _) => 1,
            Self::Secondary(_) => 2,
            Self::Contact(_, _, _) => 3,
        }
    }
}

#[derive(Default)]
struct MixedHapticState {
    last_started: HashMap<RoutedHapticKind, f64>,
    active_kind: Option<RoutedHapticKind>,
    active_priority: u8,
    active_until: f64,
}

#[derive(Resource, Default)]
struct CombatHapticMixer {
    devices: HashMap<Entity, MixedHapticState>,
}

#[derive(Clone, Copy)]
struct RoutedCombatHaptic {
    kind: RoutedHapticKind,
    role: CombatHapticRole,
    pattern: HapticPattern,
    retrigger_ms: u16,
}

fn action_haptic_pattern(class: HapticMoveClass, phase: HapticActionPhase) -> HapticPattern {
    match (class, phase) {
        (HapticMoveClass::Light, HapticActionPhase::Startup) => {
            HapticPattern::new(HapticSegment::transient(0, 18, 0.03, 0.16), None, None, 10)
        }
        (HapticMoveClass::Light, HapticActionPhase::Release) => {
            HapticPattern::new(HapticSegment::transient(0, 20, 0.04, 0.22), None, None, 20)
        }
        (HapticMoveClass::Heavy, HapticActionPhase::Startup) => {
            HapticPattern::new(HapticSegment::transient(0, 22, 0.06, 0.18), None, None, 12)
        }
        (HapticMoveClass::Heavy, HapticActionPhase::Release) => HapticPattern::new(
            HapticSegment::transient(0, 22, 0.10, 0.30),
            Some(HapticSegment::continuous(36, 34, 0.16, 0.08)),
            None,
            25,
        ),
        (HapticMoveClass::Special, HapticActionPhase::Startup) => {
            HapticPattern::new(HapticSegment::transient(0, 22, 0.04, 0.20), None, None, 13)
        }
        (HapticMoveClass::Special, HapticActionPhase::Release) => HapticPattern::new(
            HapticSegment::transient(0, 24, 0.07, 0.28),
            Some(HapticSegment::continuous(42, 30, 0.12, 0.07)),
            None,
            27,
        ),
        (HapticMoveClass::Guard, HapticActionPhase::Startup) => {
            HapticPattern::new(HapticSegment::transient(0, 24, 0.05, 0.20), None, None, 18)
        }
        (HapticMoveClass::Counter, HapticActionPhase::Startup) => HapticPattern::new(
            HapticSegment::transient(0, 26, 0.12, 0.36),
            Some(HapticSegment::continuous(48, 32, 0.20, 0.10)),
            None,
            48,
        ),
        (HapticMoveClass::Ultimate, HapticActionPhase::Startup) => HapticPattern::new(
            HapticSegment::transient(0, 40, 0.10, 0.38),
            Some(HapticSegment::continuous(55, 65, 0.28, 0.12)),
            None,
            82,
        ),
        (HapticMoveClass::Ultimate, HapticActionPhase::Charge) => HapticPattern::new(
            HapticSegment::transient(0, 24, 0.12, 0.40),
            Some(HapticSegment::continuous(38, 36, 0.25, 0.10)),
            None,
            86,
        ),
        (HapticMoveClass::Ultimate, HapticActionPhase::Lock) => HapticPattern::new(
            HapticSegment::transient(0, 34, 0.25, 0.55),
            Some(HapticSegment::continuous(46, 44, 0.34, 0.12)),
            None,
            90,
        ),
        (_, HapticActionPhase::Charge) => {
            HapticPattern::new(HapticSegment::transient(0, 26, 0.10, 0.24), None, None, 22)
        }
        (_, HapticActionPhase::Aftermath) => {
            HapticPattern::new(HapticSegment::continuous(0, 54, 0.16, 0.06), None, None, 16)
        }
        (_, HapticActionPhase::Lock) => {
            HapticPattern::new(HapticSegment::transient(0, 28, 0.12, 0.30), None, None, 35)
        }
        (HapticMoveClass::Guard, HapticActionPhase::Release)
        | (HapticMoveClass::Counter, HapticActionPhase::Release) => {
            HapticPattern::new(HapticSegment::transient(0, 24, 0.08, 0.26), None, None, 28)
        }
        (HapticMoveClass::Ultimate, HapticActionPhase::Release) => HapticPattern::new(
            HapticSegment::transient(0, 30, 0.16, 0.55),
            Some(HapticSegment::continuous(45, 50, 0.42, 0.32)),
            Some(HapticSegment::continuous(105, 95, 0.68, 0.12)),
            96,
        ),
    }
}

fn clean_contact_pattern(weight: HapticImpactWeight, role: CombatHapticRole) -> HapticPattern {
    match (weight, role) {
        (HapticImpactWeight::Light, CombatHapticRole::Attacker) => {
            HapticPattern::new(HapticSegment::transient(0, 32, 0.08, 0.48), None, None, 55)
        }
        (HapticImpactWeight::Light, CombatHapticRole::Defender) => {
            HapticPattern::new(HapticSegment::continuous(0, 60, 0.50, 0.16), None, None, 60)
        }
        (HapticImpactWeight::Heavy, CombatHapticRole::Attacker) => HapticPattern::new(
            HapticSegment::transient(0, 36, 0.18, 0.65),
            Some(HapticSegment::continuous(48, 42, 0.22, 0.12)),
            None,
            72,
        ),
        (HapticImpactWeight::Heavy, CombatHapticRole::Defender) => HapticPattern::new(
            HapticSegment::transient(0, 34, 0.62, 0.48),
            Some(HapticSegment::continuous(34, 88, 0.72, 0.10)),
            None,
            78,
        ),
        (HapticImpactWeight::Ultimate, CombatHapticRole::Attacker) => HapticPattern::new(
            HapticSegment::transient(0, 28, 0.12, 0.58),
            Some(HapticSegment::continuous(42, 48, 0.34, 0.14)),
            None,
            92,
        ),
        (HapticImpactWeight::Ultimate, CombatHapticRole::Defender) => HapticPattern::new(
            HapticSegment::transient(0, 38, 0.78, 0.62),
            Some(HapticSegment::continuous(38, 122, 0.90, 0.14)),
            Some(HapticSegment::continuous(175, 80, 0.42, 0.08)),
            96,
        ),
        (HapticImpactWeight::Terminal, CombatHapticRole::Attacker) => HapticPattern::new(
            HapticSegment::transient(0, 35, 0.20, 0.70),
            Some(HapticSegment::continuous(50, 90, 0.45, 0.18)),
            None,
            100,
        ),
        (HapticImpactWeight::Terminal, CombatHapticRole::Defender) => HapticPattern::new(
            HapticSegment::transient(0, 40, 0.90, 0.60),
            Some(HapticSegment::continuous(40, 180, 0.88, 0.08)),
            None,
            100,
        ),
    }
}

fn contact_haptic_pattern(
    outcome: HapticContactOutcome,
    weight: HapticImpactWeight,
    role: CombatHapticRole,
) -> HapticPattern {
    if weight == HapticImpactWeight::Terminal {
        return clean_contact_pattern(weight, role);
    }
    match (outcome, role) {
        (HapticContactOutcome::Clean, _) => clean_contact_pattern(weight, role),
        (HapticContactOutcome::Guarded, CombatHapticRole::Attacker) => {
            let heavy = matches!(
                weight,
                HapticImpactWeight::Heavy | HapticImpactWeight::Ultimate
            );
            HapticPattern::new(
                HapticSegment::transient(
                    0,
                    if heavy { 28 } else { 24 },
                    if heavy { 0.08 } else { 0.04 },
                    if heavy { 0.34 } else { 0.26 },
                ),
                None,
                None,
                if heavy { 62 } else { 58 },
            )
        }
        (HapticContactOutcome::Guarded, CombatHapticRole::Defender) => {
            let heavy = matches!(
                weight,
                HapticImpactWeight::Heavy | HapticImpactWeight::Ultimate
            );
            HapticPattern::new(
                HapticSegment::transient(
                    0,
                    if heavy { 40 } else { 34 },
                    if heavy { 0.28 } else { 0.20 },
                    if heavy { 0.66 } else { 0.58 },
                ),
                Some(HapticSegment::continuous(
                    if heavy { 42 } else { 38 },
                    if heavy { 44 } else { 32 },
                    if heavy { 0.24 } else { 0.16 },
                    0.10,
                )),
                None,
                if heavy { 68 } else { 64 },
            )
        }
        (HapticContactOutcome::Counter, CombatHapticRole::Attacker) => HapticPattern::new(
            HapticSegment::transient(0, 28, 0.12, 0.58),
            Some(HapticSegment::transient(52, 22, 0.08, 0.38)),
            None,
            80,
        ),
        (HapticContactOutcome::Counter, CombatHapticRole::Defender) => HapticPattern::new(
            HapticSegment::transient(0, 36, 0.70, 0.35),
            Some(HapticSegment::continuous(52, 70, 0.55, 0.10)),
            None,
            84,
        ),
        (HapticContactOutcome::Grab, CombatHapticRole::Attacker) => {
            if weight == HapticImpactWeight::Ultimate {
                HapticPattern::new(
                    HapticSegment::transient(0, 34, 0.25, 0.55),
                    Some(HapticSegment::continuous(46, 44, 0.34, 0.12)),
                    None,
                    90,
                )
            } else {
                HapticPattern::new(HapticSegment::transient(0, 34, 0.14, 0.48), None, None, 68)
            }
        }
        (HapticContactOutcome::Grab, CombatHapticRole::Defender) => {
            if weight == HapticImpactWeight::Ultimate {
                HapticPattern::new(
                    HapticSegment::transient(0, 42, 0.62, 0.44),
                    Some(HapticSegment::continuous(42, 86, 0.68, 0.12)),
                    None,
                    92,
                )
            } else {
                HapticPattern::new(
                    HapticSegment::transient(0, 46, 0.48, 0.22),
                    Some(HapticSegment::continuous(52, 42, 0.30, 0.08)),
                    None,
                    72,
                )
            }
        }
    }
}

fn routed_haptic_pattern(
    kind: RoutedHapticKind,
    style: HapticStyle,
) -> Option<(HapticPattern, u16)> {
    let pattern = match kind {
        RoutedHapticKind::Action(class, phase) => {
            if style == HapticStyle::Minimal {
                return None;
            }
            if phase == HapticActionPhase::Aftermath && style != HapticStyle::Cinematic {
                return None;
            }
            action_haptic_pattern(class, phase)
        }
        RoutedHapticKind::Contact(outcome, weight, role) => {
            contact_haptic_pattern(outcome, weight, role)
        }
        RoutedHapticKind::Secondary(_) => {
            if style == HapticStyle::Minimal {
                return None;
            }
            HapticPattern::new(HapticSegment::continuous(0, 62, 0.34, 0.10), None, None, 30)
        }
    };
    let pattern = if style == HapticStyle::Cinematic {
        pattern.scaled_duration(1.18)
    } else {
        pattern
    };
    let retrigger_ms = match kind {
        RoutedHapticKind::Action(HapticMoveClass::Ultimate, _) => 70,
        RoutedHapticKind::Action(_, _) => 35,
        RoutedHapticKind::Contact(_, HapticImpactWeight::Light, _) => 45,
        RoutedHapticKind::Contact(_, HapticImpactWeight::Heavy, _) => 60,
        RoutedHapticKind::Contact(_, HapticImpactWeight::Ultimate, _) => 70,
        RoutedHapticKind::Contact(_, HapticImpactWeight::Terminal, _) => 200,
        RoutedHapticKind::Secondary(_) => 80,
    };
    Some((pattern, retrigger_ms))
}

fn route_combat_haptics(
    time: Res<Time<Real>>,
    preferences: Res<ControlPreferences>,
    state: Option<Res<crate::game_state::MatchState>>,
    reconnect: Option<Res<crate::user_mode::LocalControllerReconnect>>,
    controllers: Query<&Controller>,
    connected_gamepads: Query<(), With<Gamepad>>,
    mut queue: ResMut<CombatHapticQueue>,
    mut mixer: ResMut<CombatHapticMixer>,
    mut requests: MessageWriter<ControllerHapticRequest>,
) {
    let cues: Vec<_> = queue.drain().collect();
    let gameplay_blocked = state.as_ref().is_some_and(|state| !state.is_fighting())
        || reconnect
            .as_ref()
            .is_some_and(|reconnect| reconnect.blocks_gameplay());
    if !preferences.vibration.enabled() || gameplay_blocked {
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

    mixer.devices.retain(|gamepad, _| {
        let assigned = controllers.iter().any(|controller| {
            controller.is_human()
                && matches!(
                    controller.input,
                    LocalInputAssignment::Gamepad(assigned_gamepad)
                        if assigned_gamepad == *gamepad
                            && connected_gamepads.get(*gamepad).is_ok()
                )
        });
        if !assigned {
            requests.write(ControllerHapticRequest::stop(*gamepad));
        }
        assigned
    });

    let mut strongest_by_gamepad = HashMap::<Entity, RoutedCombatHaptic>::new();
    let mut route = |gamepad: Entity, kind: RoutedHapticKind, role: CombatHapticRole| {
        let Some((pattern, retrigger_ms)) = routed_haptic_pattern(kind, preferences.haptic_style)
        else {
            return;
        };
        let candidate = RoutedCombatHaptic {
            kind,
            role,
            pattern,
            retrigger_ms,
        };
        strongest_by_gamepad
            .entry(gamepad)
            .and_modify(|current| {
                let candidate_rank = candidate.kind.category_rank();
                let current_rank = current.kind.category_rank();
                let defender_contact_wins = matches!(candidate.kind, RoutedHapticKind::Contact(..))
                    && matches!(current.kind, RoutedHapticKind::Contact(..))
                    && candidate.role == CombatHapticRole::Defender
                    && current.role == CombatHapticRole::Attacker;
                if candidate_rank > current_rank
                    || defender_contact_wins
                    || (candidate_rank == current_rank
                        && candidate.pattern.priority > current.pattern.priority)
                    || (candidate_rank == current_rank
                        && candidate.pattern.priority == current.pattern.priority
                        && candidate.role == CombatHapticRole::Defender)
                {
                    *current = candidate;
                }
            })
            .or_insert(candidate);
    };

    for cue in cues {
        match cue {
            CombatHapticCue::Action { slot, class, phase } => {
                if let Some(gamepad) = gamepad_for_slot(slot) {
                    route(
                        gamepad,
                        RoutedHapticKind::Action(class, phase),
                        CombatHapticRole::Attacker,
                    );
                }
            }
            CombatHapticCue::Contact {
                attacker,
                defender,
                outcome,
                weight,
            } => {
                if let Some(attacker) = attacker
                    && attacker != defender
                    && let Some(gamepad) = gamepad_for_slot(attacker)
                {
                    route(
                        gamepad,
                        RoutedHapticKind::Contact(outcome, weight, CombatHapticRole::Attacker),
                        CombatHapticRole::Attacker,
                    );
                }
                if let Some(gamepad) = gamepad_for_slot(defender) {
                    route(
                        gamepad,
                        RoutedHapticKind::Contact(outcome, weight, CombatHapticRole::Defender),
                        CombatHapticRole::Defender,
                    );
                }
            }
            CombatHapticCue::Secondary { slot, kind } => {
                if let Some(gamepad) = gamepad_for_slot(slot) {
                    route(
                        gamepad,
                        RoutedHapticKind::Secondary(kind),
                        CombatHapticRole::Defender,
                    );
                }
            }
        }
    }

    let now = time.elapsed_secs_f64();
    for (gamepad, routed) in strongest_by_gamepad {
        if let Some(state) = mixer.devices.get(&gamepad) {
            let duplicate_inside_gate =
                state
                    .last_started
                    .get(&routed.kind)
                    .is_some_and(|last_started| {
                        now - last_started < f64::from(routed.retrigger_ms) / 1000.0
                    });
            let active_rank = state
                .active_kind
                .map(RoutedHapticKind::category_rank)
                .unwrap_or(0);
            let routed_rank = routed.kind.category_rank();
            let protected_stronger_cue = state.active_until > now
                && (active_rank > routed_rank
                    || (active_rank == routed_rank
                        && routed.pattern.priority < state.active_priority));
            if duplicate_inside_gate || protected_stronger_cue {
                continue;
            }
        }
        let pattern = routed.pattern.scaled(preferences.vibration);
        requests.write(ControllerHapticRequest::play(
            gamepad,
            HapticPurpose::Gameplay,
            pattern,
        ));
        let device = mixer.devices.entry(gamepad).or_default();
        device.last_started.insert(routed.kind, now);
        device.active_kind = Some(routed.kind);
        device.active_priority = pattern.priority;
        device.active_until = now + f64::from(pattern.duration_ms()) / 1000.0;
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
    fn vibration_levels_scale_without_changing_timing() {
        let standard = HapticPattern::simple(0.8, 0.24, 50);
        let low = standard.scaled(VibrationLevel::Low);
        let high = standard.scaled(VibrationLevel::High);
        assert_eq!(low.duration_ms(), standard.duration_ms());
        assert_eq!(high.duration_ms(), standard.duration_ms());
        assert!((low.segments().next().unwrap().strong - 0.48).abs() < 0.001);
        assert!(high.segments().all(|segment| segment.strong <= 1.0));
    }

    #[test]
    fn combat_palette_keeps_attacker_crisp_and_defender_deep() {
        let attacker = clean_contact_pattern(HapticImpactWeight::Light, CombatHapticRole::Attacker);
        let defender = clean_contact_pattern(HapticImpactWeight::Light, CombatHapticRole::Defender);
        let attacker = attacker.segments().next().unwrap();
        let defender = defender.segments().next().unwrap();
        assert!(attacker.weak > attacker.strong);
        assert!(defender.strong > defender.weak);
        assert!(defender.duration_ms > attacker.duration_ms);
    }

    #[test]
    fn finisher_has_priority_and_duration_headroom() {
        let heavy = clean_contact_pattern(HapticImpactWeight::Heavy, CombatHapticRole::Defender);
        let finisher =
            clean_contact_pattern(HapticImpactWeight::Terminal, CombatHapticRole::Defender);
        assert!(finisher.priority > heavy.priority);
        assert_eq!(finisher.duration_ms(), 220);
    }

    #[test]
    fn haptic_styles_filter_density_without_weakening_contact() {
        let action = RoutedHapticKind::Action(HapticMoveClass::Light, HapticActionPhase::Startup);
        let contact = RoutedHapticKind::Contact(
            HapticContactOutcome::Clean,
            HapticImpactWeight::Light,
            CombatHapticRole::Attacker,
        );
        assert!(routed_haptic_pattern(action, HapticStyle::Minimal).is_none());
        assert!(routed_haptic_pattern(action, HapticStyle::Competitive).is_some());
        assert_eq!(
            routed_haptic_pattern(contact, HapticStyle::Minimal)
                .unwrap()
                .0,
            routed_haptic_pattern(contact, HapticStyle::Competitive)
                .unwrap()
                .0
        );
        assert!(
            routed_haptic_pattern(contact, HapticStyle::Cinematic)
                .unwrap()
                .0
                .duration_ms()
                > routed_haptic_pattern(contact, HapticStyle::Competitive)
                    .unwrap()
                    .0
                    .duration_ms()
        );
    }

    #[test]
    fn guarded_contact_is_dry_for_attacker_and_bright_for_defender() {
        let attacker = contact_haptic_pattern(
            HapticContactOutcome::Guarded,
            HapticImpactWeight::Light,
            CombatHapticRole::Attacker,
        );
        let defender = contact_haptic_pattern(
            HapticContactOutcome::Guarded,
            HapticImpactWeight::Light,
            CombatHapticRole::Defender,
        );
        let attacker = attacker.segments().next().unwrap();
        let defender = defender.segments().next().unwrap();
        assert!(attacker.weak > attacker.strong);
        assert!(defender.weak > attacker.weak);
        assert!(defender.strong > attacker.strong);
    }

    #[test]
    fn ultimate_grab_has_a_longer_victim_capture_than_normal_grab() {
        let normal = contact_haptic_pattern(
            HapticContactOutcome::Grab,
            HapticImpactWeight::Heavy,
            CombatHapticRole::Defender,
        );
        let ultimate = contact_haptic_pattern(
            HapticContactOutcome::Grab,
            HapticImpactWeight::Ultimate,
            CombatHapticRole::Defender,
        );
        assert!(ultimate.priority > normal.priority);
        assert!(ultimate.duration_ms() > normal.duration_ms());
    }

    #[test]
    fn fighter_actions_map_to_shared_haptic_language() {
        assert_eq!(
            haptic_move_class_for_action(FighterAction::LightAttack1),
            Some(HapticMoveClass::Light)
        );
        assert_eq!(
            haptic_move_class_for_action(FighterAction::ComboFinisher),
            Some(HapticMoveClass::Heavy)
        );
        assert_eq!(
            haptic_move_class_for_action(FighterAction::Guarding),
            Some(HapticMoveClass::Guard)
        );
        assert_eq!(
            haptic_move_class_for_action(FighterAction::GuardCounter),
            Some(HapticMoveClass::Counter)
        );
        assert_eq!(
            haptic_move_class_for_action(FighterAction::UltimateRush),
            Some(HapticMoveClass::Ultimate)
        );
        assert_eq!(haptic_move_class_for_action(FighterAction::Idle), None);
    }

    #[test]
    fn guarded_contact_beats_even_stronger_same_frame_release_feedback() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<ControllerHapticRequest>()
            .init_resource::<CombatHapticQueue>()
            .init_resource::<CombatHapticMixer>()
            .init_resource::<ControlPreferences>()
            .add_systems(Update, route_combat_haptics);
        let gamepad = app.world_mut().spawn(Gamepad::default()).id();
        app.world_mut().spawn(Controller::new(
            PlayerSlotId::new(0).unwrap(),
            ParticipantKind::Human,
            LocalInputAssignment::Gamepad(gamepad),
        ));
        let mut queue = app.world_mut().resource_mut::<CombatHapticQueue>();
        queue.push(CombatHapticCue::action(
            0,
            HapticMoveClass::Ultimate,
            HapticActionPhase::Release,
        ));
        queue.push(CombatHapticCue::contact(
            Some(0),
            1,
            HapticContactOutcome::Guarded,
            HapticImpactWeight::Light,
        ));

        app.update();

        let requests: Vec<_> = app
            .world_mut()
            .resource_mut::<Messages<ControllerHapticRequest>>()
            .drain()
            .collect();
        assert_eq!(requests.len(), 1);
        let ControllerHapticCommand::Play(pattern) = requests[0].command else {
            panic!("contact should play");
        };
        assert_eq!(pattern.priority, 58);
    }

    #[test]
    fn active_contact_cannot_be_preempted_by_action_feedback() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<ControllerHapticRequest>()
            .init_resource::<CombatHapticQueue>()
            .init_resource::<CombatHapticMixer>()
            .init_resource::<ControlPreferences>()
            .add_systems(Update, route_combat_haptics);
        let gamepad = app.world_mut().spawn(Gamepad::default()).id();
        app.world_mut().spawn(Controller::new(
            PlayerSlotId::new(0).unwrap(),
            ParticipantKind::Human,
            LocalInputAssignment::Gamepad(gamepad),
        ));
        app.world_mut()
            .resource_mut::<CombatHapticQueue>()
            .push(CombatHapticCue::contact(
                Some(1),
                0,
                HapticContactOutcome::Clean,
                HapticImpactWeight::Heavy,
            ));
        app.update();
        app.world_mut()
            .resource_mut::<Messages<ControllerHapticRequest>>()
            .clear();
        app.world_mut()
            .resource_mut::<CombatHapticQueue>()
            .push(CombatHapticCue::action(
                0,
                HapticMoveClass::Light,
                HapticActionPhase::Startup,
            ));

        app.update();

        assert!(
            app.world_mut()
                .resource_mut::<Messages<ControllerHapticRequest>>()
                .drain()
                .next()
                .is_none()
        );
    }

    #[test]
    fn contact_preempts_an_active_action_regardless_of_numeric_priority() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<ControllerHapticRequest>()
            .init_resource::<CombatHapticQueue>()
            .init_resource::<CombatHapticMixer>()
            .init_resource::<ControlPreferences>()
            .add_systems(Update, route_combat_haptics);
        let gamepad = app.world_mut().spawn(Gamepad::default()).id();
        app.world_mut().spawn(Controller::new(
            PlayerSlotId::new(0).unwrap(),
            ParticipantKind::Human,
            LocalInputAssignment::Gamepad(gamepad),
        ));
        app.world_mut()
            .resource_mut::<CombatHapticQueue>()
            .push(CombatHapticCue::action(
                0,
                HapticMoveClass::Ultimate,
                HapticActionPhase::Release,
            ));
        app.update();
        app.world_mut()
            .resource_mut::<Messages<ControllerHapticRequest>>()
            .clear();
        app.world_mut()
            .resource_mut::<CombatHapticQueue>()
            .push(CombatHapticCue::contact(
                Some(0),
                1,
                HapticContactOutcome::Guarded,
                HapticImpactWeight::Light,
            ));

        app.update();

        let request = app
            .world_mut()
            .resource_mut::<Messages<ControllerHapticRequest>>()
            .drain()
            .next()
            .expect("guard contact should replace the release");
        let ControllerHapticCommand::Play(pattern) = request.command else {
            panic!("guard contact should play");
        };
        assert_eq!(pattern.priority, 58);
    }

    #[test]
    fn duplicate_multi_hit_contact_is_rate_limited() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<ControllerHapticRequest>()
            .init_resource::<CombatHapticQueue>()
            .init_resource::<CombatHapticMixer>()
            .init_resource::<ControlPreferences>()
            .add_systems(Update, route_combat_haptics);
        let gamepad = app.world_mut().spawn(Gamepad::default()).id();
        app.world_mut().spawn(Controller::new(
            PlayerSlotId::new(0).unwrap(),
            ParticipantKind::Human,
            LocalInputAssignment::Gamepad(gamepad),
        ));
        let cue = CombatHapticCue::contact(
            Some(1),
            0,
            HapticContactOutcome::Clean,
            HapticImpactWeight::Light,
        );
        app.world_mut()
            .resource_mut::<CombatHapticQueue>()
            .push(cue);
        app.update();
        app.world_mut()
            .resource_mut::<Messages<ControllerHapticRequest>>()
            .clear();
        app.world_mut()
            .resource_mut::<CombatHapticQueue>()
            .push(cue);

        app.update();

        assert!(
            app.world_mut()
                .resource_mut::<Messages<ControllerHapticRequest>>()
                .drain()
                .next()
                .is_none()
        );
    }

    #[test]
    fn setup_phase_gates_queued_gameplay_haptics() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<ControllerHapticRequest>()
            .init_resource::<CombatHapticQueue>()
            .init_resource::<CombatHapticMixer>()
            .init_resource::<ControlPreferences>()
            .init_resource::<crate::game_state::MatchState>()
            .add_systems(Update, route_combat_haptics);
        let gamepad = app.world_mut().spawn(Gamepad::default()).id();
        app.world_mut().spawn(Controller::new(
            PlayerSlotId::new(0).unwrap(),
            ParticipantKind::Human,
            LocalInputAssignment::Gamepad(gamepad),
        ));
        app.world_mut()
            .resource_mut::<CombatHapticQueue>()
            .push(CombatHapticCue::contact(
                Some(1),
                0,
                HapticContactOutcome::Clean,
                HapticImpactWeight::Heavy,
            ));

        app.update();

        assert!(
            app.world_mut()
                .resource_mut::<Messages<ControllerHapticRequest>>()
                .drain()
                .next()
                .is_none()
        );
    }

    #[test]
    fn disconnected_active_gamepad_is_stopped_and_removed_from_mixer() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_message::<ControllerHapticRequest>()
            .init_resource::<CombatHapticQueue>()
            .init_resource::<CombatHapticMixer>()
            .init_resource::<ControlPreferences>()
            .add_systems(Update, route_combat_haptics);
        let gamepad = app.world_mut().spawn(Gamepad::default()).id();
        app.world_mut().spawn(Controller::new(
            PlayerSlotId::new(0).unwrap(),
            ParticipantKind::Human,
            LocalInputAssignment::Gamepad(gamepad),
        ));
        app.world_mut()
            .resource_mut::<CombatHapticQueue>()
            .push(CombatHapticCue::contact(
                Some(1),
                0,
                HapticContactOutcome::Clean,
                HapticImpactWeight::Heavy,
            ));
        app.update();
        app.world_mut()
            .resource_mut::<Messages<ControllerHapticRequest>>()
            .clear();
        app.world_mut().entity_mut(gamepad).remove::<Gamepad>();

        app.update();

        let request = app
            .world_mut()
            .resource_mut::<Messages<ControllerHapticRequest>>()
            .drain()
            .next()
            .expect("disconnect should stop active haptics");
        assert_eq!(request.command, ControllerHapticCommand::Stop);
        assert!(
            app.world()
                .resource::<CombatHapticMixer>()
                .devices
                .is_empty()
        );
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
            .push(CombatHapticCue::contact(
                Some(0),
                1,
                HapticContactOutcome::Clean,
                HapticImpactWeight::Light,
            ));

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
            .push(CombatHapticCue::contact(
                None,
                0,
                HapticContactOutcome::Clean,
                HapticImpactWeight::Heavy,
            ));

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
