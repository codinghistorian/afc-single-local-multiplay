use std::collections::HashMap;

use bevy::ecs::system::NonSendMarker;
use bevy::input::{InputSystems, gamepad::GamepadInput};
use bevy::prelude::*;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, msg_send};
use objc2_core_haptics::{
    CHHapticDynamicParameter, CHHapticEngine, CHHapticEvent, CHHapticEventParameter,
    CHHapticEventParameterIDHapticIntensity, CHHapticEventParameterIDHapticSharpness,
    CHHapticEventTypeHapticContinuous, CHHapticEventTypeHapticTransient, CHHapticPattern,
    CHHapticPatternPlayer, CHHapticTimeImmediate,
};
use objc2_foundation::NSArray;
use objc2_game_controller::{
    GCController, GCDevice, GCDeviceHaptics, GCExtendedGamepad, GCHapticsLocality,
    GCHapticsLocalityDefault, GCHapticsLocalityHandles, GCHapticsLocalityLeftHandle,
    GCHapticsLocalityRightHandle,
};

use crate::control_settings::ControlPreferences;
use crate::control_settings::ControllerDeviceInfo;
use crate::controller_haptics::{
    ControllerHapticCommand, ControllerHapticRequest, ControllerHapticsSystems, HapticAvailability,
    HapticPattern, HapticPlaybackEvent, HapticPlaybackResult, HapticPurpose, HapticSegmentKind,
};

const DIGITAL_PRESS_THRESHOLD: f32 = 0.5;

/// Uses Apple's GameController framework on macOS.
///
/// Gilrs currently recognizes some Xbox Series USB devices but exposes no
/// buttons or axes for them. Feeding Apple's normalized controller profile into
/// Bevy keeps the rest of the game's device-neutral input path unchanged.
pub struct MacOsGamepadPlugin;

impl Plugin for MacOsGamepadPlugin {
    fn build(&self, app: &mut App) {
        app.insert_non_send_resource(MacOsHapticState::default())
            .add_systems(PreUpdate, sync_macos_gamepads.after(InputSystems))
            .add_systems(
                PostUpdate,
                play_macos_haptics.in_set(ControllerHapticsSystems::Playback),
            );
    }
}

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct MacOsControllerId(usize);

#[derive(Clone, Debug, PartialEq)]
struct MacOsControllerSnapshot {
    id: MacOsControllerId,
    buttons: [(GamepadButton, f32); 17],
    axes: [(GamepadAxis, f32); 4],
}

struct CapturedMacOsController {
    controller: Retained<GCController>,
    snapshot: MacOsControllerSnapshot,
}

#[derive(Default)]
struct MacOsHapticState {
    devices: HashMap<MacOsControllerId, MacOsHapticDevice>,
}

struct MacOsHapticDevice {
    haptics: Retained<GCDeviceHaptics>,
    layout: MacOsHapticLayout,
    engines: Option<MacOsHapticEngines>,
    active: Vec<ActiveMacOsHaptic>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MacOsHapticLayout {
    DualHandles,
    Handles,
    Default,
}

impl MacOsHapticLayout {
    const fn label(self) -> &'static str {
        match self {
            Self::DualHandles => "left + right handles",
            Self::Handles => "combined handles",
            Self::Default => "default actuator",
        }
    }
}

enum MacOsHapticEngines {
    Combined(Retained<CHHapticEngine>),
    DualHandles {
        strong: Retained<CHHapticEngine>,
        weak: Retained<CHHapticEngine>,
    },
}

struct ActiveMacOsHaptic {
    #[allow(dead_code)]
    players: Vec<Retained<ProtocolObject<dyn CHHapticPatternPlayer>>>,
    gamepad: Entity,
    purpose: HapticPurpose,
    ends_at: f64,
}

fn sync_macos_gamepads(
    _main_thread: NonSendMarker,
    mut commands: Commands,
    existing: Query<(Entity, &MacOsControllerId, Has<Gamepad>)>,
    mut gamepads: Query<&mut Gamepad, With<MacOsControllerId>>,
    mut metadata: Query<&mut ControllerDeviceInfo, With<MacOsControllerId>>,
    preferences: Res<ControlPreferences>,
    mut haptic_state: NonSendMut<MacOsHapticState>,
) {
    let controllers = capture_macos_controllers();

    for captured in &controllers {
        let snapshot = &captured.snapshot;
        // SAFETY: Apple GameController objects are accessed on the main thread
        // and retained for the duration of this system.
        let device_haptics = unsafe { captured.controller.haptics() };
        let haptic_layout = device_haptics.as_deref().map(macos_haptic_layout);
        let availability = if device_haptics.is_some() {
            // A non-null GCDeviceHaptics only means that macOS exposes its
            // haptics API. In particular, a wired Xbox controller can expose
            // this object while its motors remain silent. Playback is the
            // meaningful test, so avoid claiming support before then.
            HapticAvailability::ApiReady
        } else {
            HapticAvailability::Unsupported
        };
        if let Some(device_haptics) = device_haptics {
            let layout = haptic_layout.expect("haptic layout exists with device haptics");
            let device =
                haptic_state
                    .devices
                    .entry(snapshot.id)
                    .or_insert_with(|| MacOsHapticDevice {
                        haptics: device_haptics.clone(),
                        layout,
                        engines: None,
                        active: Vec::new(),
                    });
            device.haptics = device_haptics;
            if preferences.vibration.enabled() && device.engines.is_none() {
                if let Err(error) = device.ensure_engines() {
                    debug!("Could not prewarm controller haptics: {error}");
                }
            }
        }
        if let Some((entity, _, connected)) = existing.iter().find(|(_, id, _)| **id == snapshot.id)
        {
            if connected {
                if let Ok(mut gamepad) = gamepads.get_mut(entity) {
                    apply_snapshot(&mut gamepad, snapshot);
                }
            } else {
                let mut gamepad = Gamepad::default();
                apply_snapshot(&mut gamepad, snapshot);
                commands.entity(entity).insert(gamepad);
            }
            if let Ok(mut info) = metadata.get_mut(entity) {
                info.connected = true;
                if info.haptics != HapticAvailability::Supported
                    || availability == HapticAvailability::Unsupported
                {
                    info.haptics = availability;
                }
            }
        } else {
            let display_name = controller_display_name(&captured.controller);
            let mut gamepad = Gamepad::default();
            apply_snapshot(&mut gamepad, snapshot);
            info!(
                "Apple GameController connected: {display_name}; haptics: {}; route: {}",
                availability.label(),
                haptic_layout
                    .map(MacOsHapticLayout::label)
                    .unwrap_or("none"),
            );
            let mut info = ControllerDeviceInfo::connected(display_name.clone(), None, None);
            info.haptics = availability;
            commands.spawn((snapshot.id, Name::new(display_name.clone()), gamepad, info));
        }
    }

    for (entity, id, connected) in &existing {
        let still_connected = controllers
            .iter()
            .any(|controller| controller.snapshot.id == *id);
        if connected && !still_connected {
            info!("Apple GameController disconnected: {entity}");
            commands.entity(entity).remove::<Gamepad>();
            if let Ok(mut info) = metadata.get_mut(entity) {
                info.connected = false;
            }
            if let Some(mut device) = haptic_state.devices.remove(id) {
                let _ = device.stop();
            }
        }
    }
}

fn macos_haptic_layout(haptics: &GCDeviceHaptics) -> MacOsHapticLayout {
    // SAFETY: `haptics` is retained by its controller/device state while the
    // immutable locality set is inspected. The framework owns all referenced
    // extern string constants for the process lifetime.
    let (localities, left_handle, right_handle, handles) = unsafe {
        (
            haptics.supportedLocalities(),
            GCHapticsLocalityLeftHandle,
            GCHapticsLocalityRightHandle,
            GCHapticsLocalityHandles,
        )
    };
    if localities.containsObject(left_handle) && localities.containsObject(right_handle) {
        MacOsHapticLayout::DualHandles
    } else if localities.containsObject(handles) {
        MacOsHapticLayout::Handles
    } else {
        // Apple guarantees the default locality whenever GCDeviceHaptics
        // exists. It is also the safest fallback for unusual controllers.
        MacOsHapticLayout::Default
    }
}

fn create_macos_haptic_engine(
    haptics: &GCDeviceHaptics,
    locality: &'static GCHapticsLocality,
    locality_label: &str,
) -> Result<Retained<CHHapticEngine>, String> {
    // `objc2-game-controller` 0.3.2 incorrectly gates this generated method
    // away from macOS, although it is present in the macOS SDK.
    // SAFETY: The selector and retained return type match
    // `-[GCDeviceHaptics createEngineWithLocality:]`.
    let engine: Option<Retained<CHHapticEngine>> =
        unsafe { msg_send![haptics, createEngineWithLocality: locality] };
    let engine =
        engine.ok_or_else(|| format!("controller did not create its {locality_label} engine"))?;
    // Haptic-only mode avoids unnecessary audio-engine setup. Explicitly
    // unmuting here also prevents inherited engine state from becoming a
    // silent success.
    // SAFETY: The engine is retained for every configuration and start call.
    unsafe {
        engine.setPlaysHapticsOnly(true);
        engine.setIsMutedForHaptics(false);
        engine.setAutoShutdownEnabled(false);
        engine
            .startAndReturnError()
            .map_err(|error| format!("could not start {locality_label} engine: {error:?}"))?;
    }
    Ok(engine)
}

impl MacOsHapticEngines {
    fn start(&self) -> Result<(), String> {
        let start = |engine: &CHHapticEngine, label: &str| {
            // Calling start on an already-running engine is supported and
            // recovers cleanly after a system reset or interruption.
            unsafe {
                engine
                    .startAndReturnError()
                    .map_err(|error| format!("could not restart {label} engine: {error:?}"))
            }
        };
        match self {
            Self::Combined(engine) => start(engine, "combined"),
            Self::DualHandles { strong, weak } => {
                start(strong, "left-handle")?;
                start(weak, "right-handle")
            }
        }
    }
}

impl MacOsHapticDevice {
    fn ensure_engines(&mut self) -> Result<&MacOsHapticEngines, String> {
        if self.engines.is_none() {
            let engines = match self.layout {
                MacOsHapticLayout::DualHandles => {
                    // SAFETY: GameController owns these extern locality
                    // constants for the process lifetime.
                    let (left_handle, right_handle) =
                        unsafe { (GCHapticsLocalityLeftHandle, GCHapticsLocalityRightHandle) };
                    MacOsHapticEngines::DualHandles {
                        // Xbox-compatible dual-motor routing: the left handle
                        // receives the strong/low-frequency channel and the
                        // right handle receives the weak/high-frequency channel.
                        strong: create_macos_haptic_engine(
                            &self.haptics,
                            left_handle,
                            "left-handle",
                        )?,
                        weak: create_macos_haptic_engine(
                            &self.haptics,
                            right_handle,
                            "right-handle",
                        )?,
                    }
                }
                MacOsHapticLayout::Handles => MacOsHapticEngines::Combined(
                    // SAFETY: GameController owns this extern locality
                    // constant for the process lifetime.
                    create_macos_haptic_engine(
                        &self.haptics,
                        unsafe { GCHapticsLocalityHandles },
                        "handles",
                    )?,
                ),
                MacOsHapticLayout::Default => MacOsHapticEngines::Combined(
                    // SAFETY: GameController owns this extern locality
                    // constant for the process lifetime.
                    create_macos_haptic_engine(
                        &self.haptics,
                        unsafe { GCHapticsLocalityDefault },
                        "default",
                    )?,
                ),
            };
            self.engines = Some(engines);
        } else {
            self.engines
                .as_ref()
                .expect("haptic engines exist")
                .start()?;
        }
        Ok(self.engines.as_ref().expect("haptic engines initialized"))
    }

    fn stop(&mut self) -> Vec<ActiveMacOsHaptic> {
        let active: Vec<_> = self.active.drain(..).collect();
        for active in &active {
            for player in &active.players {
                // SAFETY: Each player is retained until cancellation.
                let _ = unsafe { player.cancelAndReturnError() };
            }
        }
        active
    }

    fn play_once(
        &mut self,
        pattern: HapticPattern,
    ) -> Result<Vec<Retained<ProtocolObject<dyn CHHapticPatternPlayer>>>, String> {
        let engines = self.ensure_engines()?;
        let mut players = match engines {
            MacOsHapticEngines::Combined(engine) => {
                vec![create_core_haptic_player(
                    engine,
                    pattern,
                    MacOsHapticChannel::Combined,
                )?]
            }
            MacOsHapticEngines::DualHandles { strong, weak } => vec![
                create_core_haptic_player(strong, pattern, MacOsHapticChannel::Strong)?,
                create_core_haptic_player(weak, pattern, MacOsHapticChannel::Weak)?,
            ],
        };
        players.retain(Option::is_some);
        let players: Vec<_> = players.into_iter().flatten().collect();
        if players.is_empty() {
            return Err("haptic pattern contains no audible motor intensity".to_string());
        }
        for (index, player) in players.iter().enumerate() {
            // SAFETY: The player and its engine are retained throughout
            // playback, and zero requests immediate playback.
            if let Err(error) = unsafe { player.startAtTime_error(CHHapticTimeImmediate) } {
                for started in &players[..index] {
                    let _ = unsafe { started.cancelAndReturnError() };
                }
                return Err(format!("could not start haptic player: {error:?}"));
            }
        }
        Ok(players)
    }

    fn play(
        &mut self,
        gamepad: Entity,
        purpose: HapticPurpose,
        pattern: HapticPattern,
        now: f64,
    ) -> Result<(), String> {
        let _ = self.stop();
        let players = match self.play_once(pattern) {
            Ok(players) => players,
            Err(first_error) => {
                // Rebuild once after an interruption. If a controller
                // advertised explicit handles but rejected them, fall back to
                // Apple's guaranteed default locality.
                self.engines = None;
                self.layout = MacOsHapticLayout::Default;
                self.play_once(pattern)
                    .map_err(|retry_error| format!("{first_error}; retry: {retry_error}"))?
            }
        };
        self.active.push(ActiveMacOsHaptic {
            players,
            gamepad,
            purpose,
            ends_at: now + f64::from(pattern.duration_ms()) / 1000.0,
        });
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum MacOsHapticChannel {
    Combined,
    Strong,
    Weak,
}

fn create_core_haptic_player(
    engine: &CHHapticEngine,
    pattern: HapticPattern,
    channel: MacOsHapticChannel,
) -> Result<Option<Retained<ProtocolObject<dyn CHHapticPatternPlayer>>>, String> {
    let Some(pattern) = create_core_haptic_pattern(pattern, channel)? else {
        return Ok(None);
    };
    // SAFETY: The pattern and engine remain retained across player creation.
    unsafe {
        engine
            .createPlayerWithPattern_error(&pattern)
            .map(Some)
            .map_err(|error| format!("could not create haptic player: {error:?}"))
    }
}

fn create_core_haptic_pattern(
    pattern: HapticPattern,
    channel: MacOsHapticChannel,
) -> Result<Option<Retained<CHHapticPattern>>, String> {
    let mut events = Vec::new();
    for segment in pattern.segments() {
        let (intensity, sharpness) = match channel {
            MacOsHapticChannel::Combined => {
                let intensity = segment.strong.max(segment.weak).clamp(0.0, 1.0);
                let sharpness = if segment.strong + segment.weak <= f32::EPSILON {
                    0.0
                } else {
                    (segment.weak / (segment.strong + segment.weak)).clamp(0.0, 1.0)
                };
                (intensity, sharpness)
            }
            MacOsHapticChannel::Strong => (segment.strong.clamp(0.0, 1.0), 0.12),
            MacOsHapticChannel::Weak => (segment.weak.clamp(0.0, 1.0), 0.88),
        };
        if intensity <= f32::EPSILON {
            continue;
        }
        // SAFETY: All parameter IDs and event types are framework constants,
        // and the created objects are retained by their arrays and pattern.
        let event = unsafe {
            let parameters = NSArray::from_retained_slice(&[
                CHHapticEventParameter::initWithParameterID_value(
                    CHHapticEventParameter::alloc(),
                    CHHapticEventParameterIDHapticIntensity,
                    intensity,
                ),
                CHHapticEventParameter::initWithParameterID_value(
                    CHHapticEventParameter::alloc(),
                    CHHapticEventParameterIDHapticSharpness,
                    sharpness,
                ),
            ]);
            match segment.kind {
                HapticSegmentKind::Transient => {
                    CHHapticEvent::initWithEventType_parameters_relativeTime(
                        CHHapticEvent::alloc(),
                        CHHapticEventTypeHapticTransient,
                        &parameters,
                        f64::from(segment.start_ms) / 1000.0,
                    )
                }
                HapticSegmentKind::Continuous => {
                    CHHapticEvent::initWithEventType_parameters_relativeTime_duration(
                        CHHapticEvent::alloc(),
                        CHHapticEventTypeHapticContinuous,
                        &parameters,
                        f64::from(segment.start_ms) / 1000.0,
                        f64::from(segment.duration_ms) / 1000.0,
                    )
                }
            }
        };
        events.push(event);
    }
    if events.is_empty() {
        return Ok(None);
    }
    let events = NSArray::from_retained_slice(&events);
    let dynamic_parameters = NSArray::<CHHapticDynamicParameter>::new();
    // SAFETY: Arrays contain the exact framework types required by the
    // initializer and live through initialization.
    unsafe {
        CHHapticPattern::initWithEvents_parameters_error(
            CHHapticPattern::alloc(),
            &events,
            &dynamic_parameters,
        )
        .map(Some)
        .map_err(|error| format!("could not build haptic pattern: {error:?}"))
    }
}

fn play_macos_haptics(
    _main_thread: NonSendMarker,
    time: Res<Time<Real>>,
    preferences: Res<ControlPreferences>,
    mut requests: MessageReader<ControllerHapticRequest>,
    mut playback_events: MessageWriter<HapticPlaybackEvent>,
    controller_ids: Query<&MacOsControllerId>,
    mut metadata: Query<&mut ControllerDeviceInfo>,
    mut haptic_state: NonSendMut<MacOsHapticState>,
) {
    let now = time.elapsed_secs_f64();
    for device in haptic_state.devices.values_mut() {
        let mut still_active = Vec::with_capacity(device.active.len());
        for active in device.active.drain(..) {
            if active.ends_at <= now {
                playback_events.write(HapticPlaybackEvent {
                    gamepad: active.gamepad,
                    purpose: active.purpose,
                    result: HapticPlaybackResult::Completed,
                });
            } else {
                still_active.push(active);
            }
        }
        device.active = still_active;
    }

    for request in requests.read() {
        let Ok(id) = controller_ids.get(request.gamepad) else {
            playback_events.write(HapticPlaybackEvent {
                gamepad: request.gamepad,
                purpose: request.purpose,
                result: HapticPlaybackResult::Unsupported,
            });
            continue;
        };
        let Some(device) = haptic_state.devices.get_mut(id) else {
            playback_events.write(HapticPlaybackEvent {
                gamepad: request.gamepad,
                purpose: request.purpose,
                result: HapticPlaybackResult::Unsupported,
            });
            continue;
        };
        match request.command {
            ControllerHapticCommand::Stop => {
                for active in device.stop() {
                    playback_events.write(HapticPlaybackEvent {
                        gamepad: active.gamepad,
                        purpose: active.purpose,
                        result: HapticPlaybackResult::Preempted,
                    });
                }
            }
            ControllerHapticCommand::Play(pattern) if preferences.vibration.enabled() => {
                for active in device.stop() {
                    playback_events.write(HapticPlaybackEvent {
                        gamepad: active.gamepad,
                        purpose: active.purpose,
                        result: HapticPlaybackResult::Preempted,
                    });
                }
                match device.play(request.gamepad, request.purpose, pattern, now) {
                    Ok(()) => {
                        if let Ok(mut info) = metadata.get_mut(request.gamepad) {
                            // This confirms that Core Haptics accepted the
                            // engine, pattern, and player calls. It cannot
                            // prove that a silent wired controller moved its
                            // physical motors.
                            info.haptics = HapticAvailability::Supported;
                        }
                        playback_events.write(HapticPlaybackEvent {
                            gamepad: request.gamepad,
                            purpose: request.purpose,
                            result: HapticPlaybackResult::Started,
                        });
                    }
                    Err(error) => {
                        if let Ok(mut info) = metadata.get_mut(request.gamepad) {
                            info.haptics = HapticAvailability::ApiReady;
                        }
                        playback_events.write(HapticPlaybackEvent {
                            gamepad: request.gamepad,
                            purpose: request.purpose,
                            result: HapticPlaybackResult::Failed(error),
                        });
                    }
                }
            }
            ControllerHapticCommand::Play(_) => {
                let _ = device.stop();
            }
        }
    }
}

fn controller_display_name(controller: &GCController) -> String {
    // SAFETY: The controller is retained by the capture for this entire call.
    unsafe {
        let name = controller
            .vendorName()
            .map(|name| name.to_string())
            .unwrap_or_else(|| "Game Controller".to_string());
        let product_category = controller.productCategory().to_string();
        if product_category.is_empty() || name.contains(&product_category) {
            name
        } else {
            format!("{name} ({product_category})")
        }
    }
}

fn apply_snapshot(gamepad: &mut Gamepad, snapshot: &MacOsControllerSnapshot) {
    for (button, value) in snapshot.buttons {
        let value = value.clamp(0.0, 1.0);
        gamepad
            .analog_mut()
            .set(GamepadInput::Button(button), value);
        if value >= DIGITAL_PRESS_THRESHOLD {
            gamepad.digital_mut().press(button);
        } else {
            gamepad.digital_mut().release(button);
        }
    }
    for (axis, value) in snapshot.axes {
        gamepad
            .analog_mut()
            .set(GamepadInput::Axis(axis), value.clamp(-1.0, 1.0));
    }
}

fn capture_macos_controllers() -> Vec<CapturedMacOsController> {
    // SAFETY: `NonSendMarker` forces the calling system onto Bevy's main
    // thread. Every Objective-C value is retained until its state is copied
    // into Rust-owned numbers.
    unsafe {
        GCController::controllers()
            .to_vec()
            .into_iter()
            .filter_map(|controller| {
                let profile = controller.extendedGamepad()?;
                let id = MacOsControllerId(objc2::rc::Retained::as_ptr(&controller) as usize);
                Some(CapturedMacOsController {
                    controller,
                    snapshot: snapshot_extended_gamepad(id, &profile),
                })
            })
            .collect()
    }
}

unsafe fn snapshot_extended_gamepad(
    id: MacOsControllerId,
    profile: &GCExtendedGamepad,
) -> MacOsControllerSnapshot {
    let dpad = unsafe { profile.dpad() };
    let left_stick = unsafe { profile.leftThumbstick() };
    let right_stick = unsafe { profile.rightThumbstick() };
    let button_value = |button: RetainedButton| unsafe { button.value() };
    let optional_button_value =
        |button: Option<RetainedButton>| button.map(button_value).unwrap_or(0.0);

    MacOsControllerSnapshot {
        id,
        buttons: [
            (
                GamepadButton::South,
                button_value(unsafe { profile.buttonA() }),
            ),
            (
                GamepadButton::East,
                button_value(unsafe { profile.buttonB() }),
            ),
            (
                GamepadButton::West,
                button_value(unsafe { profile.buttonX() }),
            ),
            (
                GamepadButton::North,
                button_value(unsafe { profile.buttonY() }),
            ),
            (
                GamepadButton::LeftTrigger,
                button_value(unsafe { profile.leftShoulder() }),
            ),
            (
                GamepadButton::LeftTrigger2,
                button_value(unsafe { profile.leftTrigger() }),
            ),
            (
                GamepadButton::RightTrigger,
                button_value(unsafe { profile.rightShoulder() }),
            ),
            (
                GamepadButton::RightTrigger2,
                button_value(unsafe { profile.rightTrigger() }),
            ),
            (
                GamepadButton::Select,
                optional_button_value(unsafe { profile.buttonOptions() }),
            ),
            (
                GamepadButton::Start,
                button_value(unsafe { profile.buttonMenu() }),
            ),
            (
                GamepadButton::Mode,
                optional_button_value(unsafe { profile.buttonHome() }),
            ),
            (
                GamepadButton::LeftThumb,
                optional_button_value(unsafe { profile.leftThumbstickButton() }),
            ),
            (
                GamepadButton::RightThumb,
                optional_button_value(unsafe { profile.rightThumbstickButton() }),
            ),
            (GamepadButton::DPadUp, button_value(unsafe { dpad.up() })),
            (
                GamepadButton::DPadDown,
                button_value(unsafe { dpad.down() }),
            ),
            (
                GamepadButton::DPadLeft,
                button_value(unsafe { dpad.left() }),
            ),
            (
                GamepadButton::DPadRight,
                button_value(unsafe { dpad.right() }),
            ),
        ],
        axes: [
            (GamepadAxis::LeftStickX, unsafe {
                left_stick.xAxis().value()
            }),
            (GamepadAxis::LeftStickY, unsafe {
                left_stick.yAxis().value()
            }),
            (GamepadAxis::RightStickX, unsafe {
                right_stick.xAxis().value()
            }),
            (GamepadAxis::RightStickY, unsafe {
                right_stick.yAxis().value()
            }),
        ],
    }
}

type RetainedButton = objc2::rc::Retained<objc2_game_controller::GCControllerButtonInput>;

#[cfg(test)]
mod tests {
    use super::*;

    fn test_snapshot(south: f32) -> MacOsControllerSnapshot {
        const BUTTONS: [GamepadButton; 17] = [
            GamepadButton::South,
            GamepadButton::East,
            GamepadButton::West,
            GamepadButton::North,
            GamepadButton::LeftTrigger,
            GamepadButton::LeftTrigger2,
            GamepadButton::RightTrigger,
            GamepadButton::RightTrigger2,
            GamepadButton::Select,
            GamepadButton::Start,
            GamepadButton::Mode,
            GamepadButton::LeftThumb,
            GamepadButton::RightThumb,
            GamepadButton::DPadUp,
            GamepadButton::DPadDown,
            GamepadButton::DPadLeft,
            GamepadButton::DPadRight,
        ];
        MacOsControllerSnapshot {
            id: MacOsControllerId(1),
            buttons: std::array::from_fn(|index| {
                (BUTTONS[index], if index == 0 { south } else { 0.0 })
            }),
            axes: [
                (GamepadAxis::LeftStickX, 0.75),
                (GamepadAxis::LeftStickY, -0.4),
                (GamepadAxis::RightStickX, 0.0),
                (GamepadAxis::RightStickY, 0.0),
            ],
        }
    }

    #[test]
    fn snapshot_application_preserves_edges_and_analog_values() {
        let mut gamepad = Gamepad::default();
        let pressed = test_snapshot(1.0);

        apply_snapshot(&mut gamepad, &pressed);
        assert!(gamepad.just_pressed(GamepadButton::South));
        assert!(gamepad.pressed(GamepadButton::South));
        assert_eq!(gamepad.left_stick(), Vec2::new(0.75, -0.4));

        gamepad.digital_mut().clear();
        apply_snapshot(&mut gamepad, &pressed);
        assert!(!gamepad.just_pressed(GamepadButton::South));

        apply_snapshot(&mut gamepad, &test_snapshot(0.0));
        assert!(gamepad.just_released(GamepadButton::South));
    }
}
