use bevy::ecs::system::NonSendMarker;
use bevy::input::{InputSystems, gamepad::GamepadInput};
use bevy::prelude::*;
use objc2::rc::Retained;
use objc2_game_controller::{GCController, GCDevice, GCExtendedGamepad};

use crate::control_settings::ControllerDeviceInfo;

const DIGITAL_PRESS_THRESHOLD: f32 = 0.5;

/// Uses Apple's GameController framework on macOS.
///
/// Gilrs currently recognizes some Xbox Series USB devices but exposes no
/// buttons or axes for them. Feeding Apple's normalized controller profile into
/// Bevy keeps the rest of the game's device-neutral input path unchanged.
pub struct MacOsGamepadPlugin;

impl Plugin for MacOsGamepadPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PreUpdate, sync_macos_gamepads.after(InputSystems));
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

fn sync_macos_gamepads(
    _main_thread: NonSendMarker,
    mut commands: Commands,
    existing: Query<(Entity, &MacOsControllerId, Has<Gamepad>)>,
    mut gamepads: Query<&mut Gamepad, With<MacOsControllerId>>,
    mut metadata: Query<&mut ControllerDeviceInfo, With<MacOsControllerId>>,
) {
    let controllers = capture_macos_controllers();

    for captured in &controllers {
        let snapshot = &captured.snapshot;
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
            }
        } else {
            let display_name = controller_display_name(&captured.controller);
            let mut gamepad = Gamepad::default();
            apply_snapshot(&mut gamepad, snapshot);
            info!("Apple GameController connected: {display_name}");
            commands.spawn((
                snapshot.id,
                Name::new(display_name.clone()),
                gamepad,
                ControllerDeviceInfo::connected(display_name, None, None),
            ));
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
