use std::time::Duration;

use bevy::input::gamepad::{
    GamepadConnection, GamepadConnectionEvent, GamepadRumbleIntensity, GamepadRumbleRequest,
};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{PlayerKeyBindings, reserved_binding_key};

const CONTROL_PREFERENCES_VERSION: u32 = 1;
#[cfg(target_arch = "wasm32")]
const CONTROL_PREFERENCES_STORAGE_KEY: &str = "animal-fighter-club.controls.v1";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ControllerFamily {
    Xbox,
    PlayStation,
    Nintendo,
    #[default]
    Generic,
}

impl ControllerFamily {
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Xbox => "Xbox",
            Self::PlayStation => "PlayStation",
            Self::Nintendo => "Nintendo",
            Self::Generic => "Gamepad",
        }
    }

    pub const fn confirm_button(self) -> GamepadButton {
        match self {
            Self::Nintendo => GamepadButton::East,
            Self::Xbox | Self::PlayStation | Self::Generic => GamepadButton::South,
        }
    }

    pub const fn back_button(self) -> GamepadButton {
        match self {
            Self::Nintendo => GamepadButton::South,
            Self::Xbox | Self::PlayStation | Self::Generic => GamepadButton::East,
        }
    }

    pub const fn confirm_label(self) -> &'static str {
        match self {
            Self::Xbox | Self::Nintendo => "A",
            Self::PlayStation => "Cross",
            Self::Generic => "Bottom",
        }
    }

    pub const fn back_label(self) -> &'static str {
        match self {
            Self::Xbox | Self::Nintendo => "B",
            Self::PlayStation => "Circle",
            Self::Generic => "Right",
        }
    }

    pub const fn face_button_label(self, button: GamepadButton) -> &'static str {
        match (self, button) {
            (Self::Xbox, GamepadButton::South) => "A",
            (Self::Xbox, GamepadButton::East) => "B",
            (Self::Xbox, GamepadButton::West) => "X",
            (Self::Xbox, GamepadButton::North) => "Y",
            (Self::PlayStation, GamepadButton::South) => "Cross",
            (Self::PlayStation, GamepadButton::East) => "Circle",
            (Self::PlayStation, GamepadButton::West) => "Square",
            (Self::PlayStation, GamepadButton::North) => "Triangle",
            (Self::Nintendo, GamepadButton::South) => "B",
            (Self::Nintendo, GamepadButton::East) => "A",
            (Self::Nintendo, GamepadButton::West) => "Y",
            (Self::Nintendo, GamepadButton::North) => "X",
            (Self::Generic, GamepadButton::South) => "Bottom",
            (Self::Generic, GamepadButton::East) => "Right",
            (Self::Generic, GamepadButton::West) => "Left",
            (Self::Generic, GamepadButton::North) => "Top",
            (Self::PlayStation, GamepadButton::LeftTrigger) => "L1",
            (Self::PlayStation, GamepadButton::LeftTrigger2) => "L2",
            (Self::PlayStation, GamepadButton::RightTrigger) => "R1",
            (Self::PlayStation, GamepadButton::RightTrigger2) => "R2",
            (Self::Nintendo, GamepadButton::LeftTrigger) => "L",
            (Self::Nintendo, GamepadButton::LeftTrigger2) => "ZL",
            (Self::Nintendo, GamepadButton::RightTrigger) => "R",
            (Self::Nintendo, GamepadButton::RightTrigger2) => "ZR",
            (_, GamepadButton::LeftTrigger) => "LB",
            (_, GamepadButton::LeftTrigger2) => "LT",
            (_, GamepadButton::RightTrigger) => "RB",
            (_, GamepadButton::RightTrigger2) => "RT",
            (_, GamepadButton::Select) => "Select",
            (_, GamepadButton::Start) => "Menu",
            (_, GamepadButton::Mode) => "Home",
            (_, GamepadButton::LeftThumb) => "L3",
            (_, GamepadButton::RightThumb) => "R3",
            (_, GamepadButton::DPadUp) => "D-Up",
            (_, GamepadButton::DPadDown) => "D-Down",
            (_, GamepadButton::DPadLeft) => "D-Left",
            (_, GamepadButton::DPadRight) => "D-Right",
            _ => "Other",
        }
    }
}

pub fn classify_controller_family(name: &str, vendor_id: Option<u16>) -> ControllerFamily {
    match vendor_id {
        Some(0x045e) => return ControllerFamily::Xbox,
        Some(0x054c) => return ControllerFamily::PlayStation,
        Some(0x057e) => return ControllerFamily::Nintendo,
        _ => {}
    }

    let normalized = name.to_ascii_lowercase();
    if normalized.contains("xbox")
        || normalized.contains("xinput")
        || normalized.contains("microsoft controller")
    {
        ControllerFamily::Xbox
    } else if normalized.contains("playstation")
        || normalized.contains("dualshock")
        || normalized.contains("dualsense")
        || normalized.contains("sony interactive")
    {
        ControllerFamily::PlayStation
    } else if normalized.contains("nintendo")
        || normalized.contains("joy-con")
        || normalized.contains("switch pro")
    {
        ControllerFamily::Nintendo
    } else {
        ControllerFamily::Generic
    }
}

#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct ControllerDeviceInfo {
    pub display_name: String,
    pub family: ControllerFamily,
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
    pub connected: bool,
}

impl ControllerDeviceInfo {
    pub(crate) fn connected(
        display_name: String,
        vendor_id: Option<u16>,
        product_id: Option<u16>,
    ) -> Self {
        Self {
            family: classify_controller_family(&display_name, vendor_id),
            display_name,
            vendor_id,
            product_id,
            connected: true,
        }
    }

    fn disconnected() -> Self {
        Self {
            display_name: "Disconnected controller".to_string(),
            family: ControllerFamily::Generic,
            vendor_id: None,
            product_id: None,
            connected: false,
        }
    }
}

pub fn sync_controller_device_info(
    mut commands: Commands,
    mut connection_events: MessageReader<GamepadConnectionEvent>,
    mut cached: Query<&mut ControllerDeviceInfo>,
    uncached_gamepads: Query<(Entity, &Gamepad, Option<&Name>), Without<ControllerDeviceInfo>>,
) {
    for event in connection_events.read() {
        match &event.connection {
            GamepadConnection::Connected {
                name,
                vendor_id,
                product_id,
            } => {
                let info = ControllerDeviceInfo::connected(name.clone(), *vendor_id, *product_id);
                if let Ok(mut current) = cached.get_mut(event.gamepad) {
                    *current = info;
                } else if let Ok(mut entity) = commands.get_entity(event.gamepad) {
                    entity.insert(info);
                }
            }
            GamepadConnection::Disconnected => {
                if let Ok(mut current) = cached.get_mut(event.gamepad) {
                    current.connected = false;
                } else if let Ok(mut entity) = commands.get_entity(event.gamepad) {
                    entity.insert(ControllerDeviceInfo::disconnected());
                }
            }
        }
    }

    for (entity, gamepad, name) in &uncached_gamepads {
        let display_name = name
            .map(|name| name.as_str().to_string())
            .unwrap_or_else(|| "Gamepad".to_string());
        let info = ControllerDeviceInfo::connected(
            display_name,
            gamepad.vendor_id(),
            gamepad.product_id(),
        );
        commands.entity(entity).insert(info);
    }
}

pub fn controller_info<'a>(
    entity: Entity,
    metadata: &'a Query<&ControllerDeviceInfo>,
) -> Option<&'a ControllerDeviceInfo> {
    metadata.get(entity).ok()
}

#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct ControlPreferences {
    pub vibration_enabled: bool,
}

impl Default for ControlPreferences {
    fn default() -> Self {
        Self {
            vibration_enabled: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct StoredControlPreferences {
    version: u32,
    vibration_enabled: bool,
    key_bindings: PlayerKeyBindings,
}

fn encode_control_preferences(
    bindings: &PlayerKeyBindings,
    preferences: &ControlPreferences,
) -> Result<String, String> {
    let stored = StoredControlPreferences {
        version: CONTROL_PREFERENCES_VERSION,
        vibration_enabled: preferences.vibration_enabled,
        key_bindings: bindings.clone(),
    };
    ron::ser::to_string_pretty(&stored, ron::ser::PrettyConfig::new())
        .map_err(|error| format!("could not encode controls: {error}"))
}

fn decode_control_preferences(
    contents: &str,
) -> Result<(PlayerKeyBindings, ControlPreferences), String> {
    let stored: StoredControlPreferences =
        ron::from_str(contents).map_err(|error| format!("could not parse controls: {error}"))?;
    if stored.version != CONTROL_PREFERENCES_VERSION {
        return Err(format!("unsupported controls version {}", stored.version));
    }
    if stored.key_bindings.has_duplicate_keys() {
        return Err("saved controls contain duplicate keys".to_string());
    }
    if stored
        .key_bindings
        .all_keys()
        .into_iter()
        .any(reserved_binding_key)
    {
        return Err("saved controls contain reserved keys".to_string());
    }
    Ok((
        stored.key_bindings,
        ControlPreferences {
            vibration_enabled: stored.vibration_enabled,
        },
    ))
}

pub fn load_control_preferences(
    mut bindings: ResMut<PlayerKeyBindings>,
    mut preferences: ResMut<ControlPreferences>,
) {
    let Some(contents) = load_control_preferences_contents() else {
        return;
    };
    match decode_control_preferences(&contents) {
        Ok((saved_bindings, saved_preferences)) => {
            *bindings = saved_bindings;
            *preferences = saved_preferences;
        }
        Err(error) => warn!("Ignoring saved control preferences: {error}"),
    }
}

pub fn save_control_preferences(
    bindings: &PlayerKeyBindings,
    preferences: &ControlPreferences,
) -> Result<(), String> {
    let contents = encode_control_preferences(bindings, preferences)?;
    save_control_preferences_contents(&contents)
}

pub fn request_controller_rumble(
    requests: &mut MessageWriter<GamepadRumbleRequest>,
    preferences: &ControlPreferences,
    gamepad: Entity,
    strength: f32,
    duration_secs: f32,
) -> bool {
    if !preferences.vibration_enabled {
        return false;
    }
    let strength = strength.clamp(0.0, 1.0);
    requests.write(GamepadRumbleRequest::Add {
        gamepad,
        duration: Duration::from_secs_f32(duration_secs.max(0.0)),
        intensity: GamepadRumbleIntensity {
            strong_motor: strength,
            weak_motor: strength * 0.65,
        },
    });
    true
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn control_preferences_path() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;

    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);

    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Library").join("Application Support"));

    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".config"))
        });

    base.map(|base| {
        base.join("Animal Fighter Club")
            .join("control-settings.ron")
    })
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn load_control_preferences_contents() -> Option<String> {
    let path = control_preferences_path()?;
    match std::fs::read_to_string(&path) {
        Ok(contents) => Some(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            warn!("Could not load {}: {error}", path.display());
            None
        }
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn save_control_preferences_contents(contents: &str) -> Result<(), String> {
    use std::io::Write;

    let path = control_preferences_path()
        .ok_or_else(|| "platform control-settings directory is unavailable".to_string())?;
    let parent = path
        .parent()
        .ok_or_else(|| "control-settings path has no parent".to_string())?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    let temporary_path = path.with_extension(format!("ron.tmp-{}", std::process::id()));
    let mut temporary = std::fs::File::create(&temporary_path)
        .map_err(|error| format!("could not create {}: {error}", temporary_path.display()))?;
    temporary
        .write_all(contents.as_bytes())
        .and_then(|()| temporary.sync_all())
        .map_err(|error| format!("could not write {}: {error}", temporary_path.display()))?;
    std::fs::rename(&temporary_path, &path)
        .or_else(|rename_error| {
            #[cfg(target_os = "windows")]
            {
                std::fs::write(&path, contents).map_err(|_| rename_error)
            }
            #[cfg(not(target_os = "windows"))]
            {
                Err(rename_error)
            }
        })
        .map_err(|error| format!("could not replace {}: {error}", path.display()))
}

#[cfg(target_arch = "wasm32")]
fn browser_storage() -> Result<web_sys::Storage, String> {
    web_sys::window()
        .ok_or_else(|| "browser window is unavailable".to_string())?
        .local_storage()
        .map_err(|error| format!("localStorage access failed: {error:?}"))?
        .ok_or_else(|| "localStorage is unavailable".to_string())
}

#[cfg(target_arch = "wasm32")]
fn load_control_preferences_contents() -> Option<String> {
    match browser_storage().and_then(|storage| {
        storage
            .get_item(CONTROL_PREFERENCES_STORAGE_KEY)
            .map_err(|error| format!("localStorage read failed: {error:?}"))
    }) {
        Ok(contents) => contents,
        Err(error) => {
            warn!("Could not load control preferences: {error}");
            None
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn save_control_preferences_contents(contents: &str) -> Result<(), String> {
    browser_storage()?
        .set_item(CONTROL_PREFERENCES_STORAGE_KEY, contents)
        .map_err(|error| format!("localStorage write failed: {error:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_family_prefers_vendor_id_and_uses_name_fallbacks() {
        assert_eq!(
            classify_controller_family("Generic pad", Some(0x045e)),
            ControllerFamily::Xbox
        );
        assert_eq!(
            classify_controller_family("Controller (Xbox One)", None),
            ControllerFamily::Xbox
        );
        assert_eq!(
            classify_controller_family("DualSense Wireless Controller", None),
            ControllerFamily::PlayStation
        );
        assert_eq!(
            classify_controller_family("Nintendo Switch Pro Controller", None),
            ControllerFamily::Nintendo
        );
        assert_eq!(
            classify_controller_family("USB Game Controller", None),
            ControllerFamily::Generic
        );
    }

    #[test]
    fn nintendo_uses_family_conventional_confirm_and_back() {
        assert_eq!(
            ControllerFamily::Nintendo.confirm_button(),
            GamepadButton::East
        );
        assert_eq!(
            ControllerFamily::Nintendo.back_button(),
            GamepadButton::South
        );
        assert_eq!(
            ControllerFamily::Xbox.confirm_button(),
            GamepadButton::South
        );
        assert_eq!(
            ControllerFamily::PlayStation.back_button(),
            GamepadButton::East
        );
    }

    #[test]
    fn control_preferences_roundtrip() {
        let bindings = PlayerKeyBindings::default();
        let preferences = ControlPreferences {
            vibration_enabled: false,
        };
        let encoded = encode_control_preferences(&bindings, &preferences).unwrap();
        let decoded = decode_control_preferences(&encoded).unwrap();
        assert_eq!(decoded, (bindings, preferences));
    }

    #[test]
    fn control_preferences_reject_unknown_version() {
        let encoded = encode_control_preferences(
            &PlayerKeyBindings::default(),
            &ControlPreferences::default(),
        )
        .unwrap()
        .replacen("version: 1", "version: 99", 1);
        assert!(decode_control_preferences(&encoded).is_err());
    }

    #[test]
    fn control_preferences_reject_duplicate_or_reserved_keys() {
        let mut duplicate = PlayerKeyBindings::default();
        duplicate.p2.left = duplicate.p1.left;
        let duplicate =
            encode_control_preferences(&duplicate, &ControlPreferences::default()).unwrap();
        assert!(decode_control_preferences(&duplicate).is_err());

        let mut reserved = PlayerKeyBindings::default();
        reserved.p1.left = KeyCode::Escape;
        let reserved =
            encode_control_preferences(&reserved, &ControlPreferences::default()).unwrap();
        assert!(decode_control_preferences(&reserved).is_err());
    }

    #[test]
    fn disabled_vibration_does_not_emit_a_request() {
        let preferences = ControlPreferences {
            vibration_enabled: false,
        };
        assert!(!preferences.vibration_enabled);
    }
}
