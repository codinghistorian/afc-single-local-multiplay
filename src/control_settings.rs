use bevy::input::gamepad::{GamepadConnection, GamepadConnectionEvent};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{PlayerKeyBindings, reserved_binding_key};
use crate::controller_haptics::{
    ControllerHapticRequest, HapticAvailability, HapticPurpose, HapticStyle, VibrationLevel,
    queue_simple_haptic,
};

const CONTROL_PREFERENCES_VERSION: u32 = 5;
pub(crate) const SOUND_VOLUME_STEP_PERCENT: u8 = 10;
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
    pub haptics: HapticAvailability,
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
            haptics: HapticAvailability::Unknown,
        }
    }

    fn disconnected() -> Self {
        Self {
            display_name: "Disconnected controller".to_string(),
            family: ControllerFamily::Generic,
            vendor_id: None,
            product_id: None,
            connected: false,
            haptics: HapticAvailability::Unknown,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AudioChannel {
    Music,
    SoundEffects,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AudioChannelPreference {
    enabled: bool,
    volume_percent: u8,
}

impl Default for AudioChannelPreference {
    fn default() -> Self {
        Self {
            enabled: true,
            volume_percent: 100,
        }
    }
}

impl AudioChannelPreference {
    pub(crate) const fn new(enabled: bool, volume_percent: u8) -> Self {
        Self {
            enabled,
            volume_percent,
        }
    }

    pub(crate) const fn enabled(self) -> bool {
        self.enabled
    }

    pub(crate) const fn volume_percent(self) -> u8 {
        self.volume_percent
    }

    pub(crate) fn toggle(&mut self) {
        self.enabled = !self.enabled;
    }

    pub(crate) fn step(&mut self, direction: i8) {
        let delta = i16::from(direction.signum()) * i16::from(SOUND_VOLUME_STEP_PERCENT);
        self.volume_percent = (i16::from(self.volume_percent) + delta).clamp(0, 100) as u8;
    }

    pub(crate) fn gain(self) -> f32 {
        let normalized = f32::from(self.volume_percent) / 100.0;
        normalized * normalized
    }

    pub(crate) fn effective_gain(self) -> f32 {
        if self.enabled { self.gain() } else { 0.0 }
    }

    const fn is_valid(self) -> bool {
        self.volume_percent <= 100
    }
}

#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct ControlPreferences {
    pub vibration: VibrationLevel,
    pub haptic_style: HapticStyle,
    pub(crate) music: AudioChannelPreference,
    pub(crate) sound_effects: AudioChannelPreference,
}

impl Default for ControlPreferences {
    fn default() -> Self {
        Self {
            vibration: VibrationLevel::Standard,
            haptic_style: HapticStyle::Competitive,
            music: AudioChannelPreference::default(),
            sound_effects: AudioChannelPreference::default(),
        }
    }
}

impl ControlPreferences {
    pub(crate) const fn audio(&self, channel: AudioChannel) -> AudioChannelPreference {
        match channel {
            AudioChannel::Music => self.music,
            AudioChannel::SoundEffects => self.sound_effects,
        }
    }

    pub(crate) fn audio_mut(&mut self, channel: AudioChannel) -> &mut AudioChannelPreference {
        match channel {
            AudioChannel::Music => &mut self.music,
            AudioChannel::SoundEffects => &mut self.sound_effects,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct StoredControlPreferences {
    version: u32,
    vibration: VibrationLevel,
    haptic_style: HapticStyle,
    music_enabled: bool,
    music_volume_percent: u8,
    sound_effects_enabled: bool,
    sound_effects_volume_percent: u8,
    key_bindings: PlayerKeyBindings,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct VersionFourStoredControlPreferences {
    version: u32,
    vibration: VibrationLevel,
    haptic_style: HapticStyle,
    key_bindings: PlayerKeyBindings,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct VersionTwoStoredControlPreferences {
    version: u32,
    vibration: VibrationLevel,
    key_bindings: LegacyPlayerKeyBindings,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct VersionThreeStoredControlPreferences {
    version: u32,
    vibration: VibrationLevel,
    haptic_style: HapticStyle,
    key_bindings: LegacyPlayerKeyBindings,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct LegacyStoredControlPreferences {
    version: u32,
    vibration_enabled: bool,
    key_bindings: LegacyPlayerKeyBindings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct LegacyPlayerControlBindings {
    left: KeyCode,
    right: KeyCode,
    up: KeyCode,
    down: KeyCode,
    aim_grab: KeyCode,
    heavy: KeyCode,
    light: KeyCode,
    jump: KeyCode,
}

impl LegacyPlayerControlBindings {
    fn migrate(
        self,
        defaults: crate::components::PlayerControlBindings,
    ) -> crate::components::PlayerControlBindings {
        crate::components::PlayerControlBindings {
            left: self.left,
            right: self.right,
            up: self.up,
            down: self.down,
            aim_grab: self.aim_grab,
            heavy: self.heavy,
            light: self.light,
            jump: self.jump,
            special: defaults.special,
        }
    }
}

impl From<crate::components::PlayerControlBindings> for LegacyPlayerControlBindings {
    fn from(bindings: crate::components::PlayerControlBindings) -> Self {
        Self {
            left: bindings.left,
            right: bindings.right,
            up: bindings.up,
            down: bindings.down,
            aim_grab: bindings.aim_grab,
            heavy: bindings.heavy,
            light: bindings.light,
            jump: bindings.jump,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct LegacyPlayerKeyBindings {
    p1: LegacyPlayerControlBindings,
    p2: LegacyPlayerControlBindings,
    p3: LegacyPlayerControlBindings,
    p4: LegacyPlayerControlBindings,
}

impl LegacyPlayerKeyBindings {
    #[cfg(test)]
    fn from_current(bindings: &PlayerKeyBindings) -> Self {
        Self {
            p1: bindings.p1.into(),
            p2: bindings.p2.into(),
            p3: bindings.p3.into(),
            p4: bindings.p4.into(),
        }
    }

    fn migrate(self) -> PlayerKeyBindings {
        let defaults = PlayerKeyBindings::default();
        PlayerKeyBindings {
            p1: self.p1.migrate(defaults.p1),
            p2: self.p2.migrate(defaults.p2),
            p3: self.p3.migrate(defaults.p3),
            p4: self.p4.migrate(defaults.p4),
        }
    }
}

#[derive(Deserialize)]
struct StoredControlPreferencesHeader {
    version: u32,
}

fn encode_control_preferences(
    bindings: &PlayerKeyBindings,
    preferences: &ControlPreferences,
) -> Result<String, String> {
    let stored = StoredControlPreferences {
        version: CONTROL_PREFERENCES_VERSION,
        vibration: preferences.vibration,
        haptic_style: preferences.haptic_style,
        music_enabled: preferences.music.enabled(),
        music_volume_percent: preferences.music.volume_percent(),
        sound_effects_enabled: preferences.sound_effects.enabled(),
        sound_effects_volume_percent: preferences.sound_effects.volume_percent(),
        key_bindings: bindings.clone(),
    };
    ron::ser::to_string_pretty(&stored, ron::ser::PrettyConfig::new())
        .map_err(|error| format!("could not encode controls: {error}"))
}

fn decode_control_preferences(
    contents: &str,
) -> Result<(PlayerKeyBindings, ControlPreferences), String> {
    let header: StoredControlPreferencesHeader =
        ron::from_str(contents).map_err(|error| format!("could not parse controls: {error}"))?;
    let (key_bindings, vibration, haptic_style, music, sound_effects) = match header.version {
        1 => {
            let stored: LegacyStoredControlPreferences = ron::from_str(contents)
                .map_err(|error| format!("could not parse legacy controls: {error}"))?;
            (
                stored.key_bindings.migrate(),
                if stored.vibration_enabled {
                    VibrationLevel::Standard
                } else {
                    VibrationLevel::Off
                },
                HapticStyle::Competitive,
                AudioChannelPreference::default(),
                AudioChannelPreference::default(),
            )
        }
        2 => {
            let stored: VersionTwoStoredControlPreferences = ron::from_str(contents)
                .map_err(|error| format!("could not parse version 2 controls: {error}"))?;
            (
                stored.key_bindings.migrate(),
                stored.vibration,
                HapticStyle::Competitive,
                AudioChannelPreference::default(),
                AudioChannelPreference::default(),
            )
        }
        3 => {
            let stored: VersionThreeStoredControlPreferences = ron::from_str(contents)
                .map_err(|error| format!("could not parse version 3 controls: {error}"))?;
            (
                stored.key_bindings.migrate(),
                stored.vibration,
                stored.haptic_style,
                AudioChannelPreference::default(),
                AudioChannelPreference::default(),
            )
        }
        4 => {
            let stored: VersionFourStoredControlPreferences = ron::from_str(contents)
                .map_err(|error| format!("could not parse version 4 controls: {error}"))?;
            (
                stored.key_bindings,
                stored.vibration,
                stored.haptic_style,
                AudioChannelPreference::default(),
                AudioChannelPreference::default(),
            )
        }
        CONTROL_PREFERENCES_VERSION => {
            let stored: StoredControlPreferences = ron::from_str(contents)
                .map_err(|error| format!("could not parse controls: {error}"))?;
            (
                stored.key_bindings,
                stored.vibration,
                stored.haptic_style,
                AudioChannelPreference::new(stored.music_enabled, stored.music_volume_percent),
                AudioChannelPreference::new(
                    stored.sound_effects_enabled,
                    stored.sound_effects_volume_percent,
                ),
            )
        }
        version => return Err(format!("unsupported controls version {version}")),
    };
    if !music.is_valid() || !sound_effects.is_valid() {
        return Err("saved sound volume must be between 0 and 100 percent".to_string());
    }
    if key_bindings.has_duplicate_keys() {
        return Err("saved controls contain duplicate keys".to_string());
    }
    if key_bindings
        .active_keys()
        .into_iter()
        .any(reserved_binding_key)
    {
        return Err("saved controls contain reserved keys".to_string());
    }
    Ok((
        key_bindings,
        ControlPreferences {
            vibration,
            haptic_style,
            music,
            sound_effects,
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
    requests: &mut MessageWriter<ControllerHapticRequest>,
    preferences: &ControlPreferences,
    gamepad: Entity,
    strength: f32,
    duration_secs: f32,
) -> bool {
    queue_simple_haptic(
        requests,
        preferences.vibration,
        gamepad,
        strength,
        duration_secs,
        HapticPurpose::Join,
    )
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
            vibration: VibrationLevel::Off,
            haptic_style: HapticStyle::Cinematic,
            music: AudioChannelPreference::new(false, 40),
            sound_effects: AudioChannelPreference::new(true, 70),
        };
        let encoded = encode_control_preferences(&bindings, &preferences).unwrap();
        let decoded = decode_control_preferences(&encoded).unwrap();
        assert_eq!(decoded, (bindings, preferences));
    }

    #[test]
    fn control_preferences_preserve_inactive_special_bindings_and_allow_active_overlap() {
        let mut bindings = PlayerKeyBindings::default();
        bindings.p1.left = bindings.p1.special;
        let encoded =
            encode_control_preferences(&bindings, &ControlPreferences::default()).unwrap();

        let (decoded, _) = decode_control_preferences(&encoded).unwrap();

        assert_eq!(decoded.p1.left, KeyCode::KeyE);
        assert_eq!(decoded.p1.special, KeyCode::KeyE);
        assert_eq!(decoded, bindings);
    }

    #[test]
    fn control_preferences_reject_unknown_version() {
        let encoded = encode_control_preferences(
            &PlayerKeyBindings::default(),
            &ControlPreferences::default(),
        )
        .unwrap()
        .replacen("version: 5", "version: 99", 1);
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
            vibration: VibrationLevel::Off,
            haptic_style: HapticStyle::Competitive,
            ..default()
        };
        assert!(!preferences.vibration.enabled());
    }

    #[test]
    fn sound_preferences_default_to_enabled_at_full_volume() {
        let preferences = ControlPreferences::default();

        for channel in [AudioChannel::Music, AudioChannel::SoundEffects] {
            let preference = preferences.audio(channel);
            assert!(preference.enabled());
            assert_eq!(preference.volume_percent(), 100);
            assert_eq!(preference.effective_gain(), 1.0);
        }
    }

    #[test]
    fn sound_volume_steps_by_ten_percent_and_clamps() {
        let mut preference = AudioChannelPreference::new(true, 100);
        preference.step(1);
        assert_eq!(preference.volume_percent(), 100);
        preference.step(-1);
        assert_eq!(preference.volume_percent(), 90);

        for _ in 0..12 {
            preference.step(-1);
        }
        assert_eq!(preference.volume_percent(), 0);
        preference.step(-1);
        assert_eq!(preference.volume_percent(), 0);
        preference.step(1);
        assert_eq!(preference.volume_percent(), 10);
    }

    #[test]
    fn sound_channel_toggles_remain_independent() {
        let mut preferences = ControlPreferences::default();

        preferences.audio_mut(AudioChannel::Music).toggle();

        assert!(!preferences.audio(AudioChannel::Music).enabled());
        assert!(preferences.audio(AudioChannel::SoundEffects).enabled());
        assert_eq!(preferences.audio(AudioChannel::Music).volume_percent(), 100);
    }

    #[test]
    fn sound_volume_uses_a_squared_perceptual_gain_curve() {
        assert_eq!(AudioChannelPreference::new(true, 0).effective_gain(), 0.0);
        assert_eq!(AudioChannelPreference::new(true, 50).effective_gain(), 0.25);
        assert_eq!(AudioChannelPreference::new(true, 100).effective_gain(), 1.0);
        assert_eq!(
            AudioChannelPreference::new(false, 100).effective_gain(),
            0.0
        );
    }

    #[test]
    fn version_four_controls_migrate_to_default_sound_preferences() {
        let encoded = ron::ser::to_string(&VersionFourStoredControlPreferences {
            version: 4,
            vibration: VibrationLevel::High,
            haptic_style: HapticStyle::Cinematic,
            key_bindings: PlayerKeyBindings::default(),
        })
        .unwrap();

        let (_, preferences) = decode_control_preferences(&encoded).unwrap();

        assert_eq!(preferences.vibration, VibrationLevel::High);
        assert_eq!(preferences.haptic_style, HapticStyle::Cinematic);
        assert_eq!(preferences.music, AudioChannelPreference::default());
        assert_eq!(preferences.sound_effects, AudioChannelPreference::default());
    }

    #[test]
    fn version_five_rejects_sound_percentages_over_one_hundred() {
        let encoded = encode_control_preferences(
            &PlayerKeyBindings::default(),
            &ControlPreferences::default(),
        )
        .unwrap()
        .replacen("music_volume_percent: 100", "music_volume_percent: 101", 1);

        assert!(decode_control_preferences(&encoded).is_err());

        let encoded = encode_control_preferences(
            &PlayerKeyBindings::default(),
            &ControlPreferences::default(),
        )
        .unwrap()
        .replacen(
            "sound_effects_volume_percent: 100",
            "sound_effects_volume_percent: 255",
            1,
        );
        assert!(decode_control_preferences(&encoded).is_err());
    }

    #[test]
    fn legacy_vibration_boolean_migrates_to_level() {
        let bindings = PlayerKeyBindings::default();
        let enabled = ron::ser::to_string(&LegacyStoredControlPreferences {
            version: 1,
            vibration_enabled: true,
            key_bindings: LegacyPlayerKeyBindings::from_current(&bindings),
        })
        .unwrap();
        let disabled = enabled.replacen("true", "false", 1);
        assert_eq!(
            decode_control_preferences(&enabled).unwrap().1.vibration,
            VibrationLevel::Standard
        );
        assert_eq!(
            decode_control_preferences(&disabled).unwrap().1.vibration,
            VibrationLevel::Off
        );
        assert_eq!(
            decode_control_preferences(&enabled).unwrap().1.haptic_style,
            HapticStyle::Competitive
        );
    }

    #[test]
    fn version_two_controls_migrate_to_competitive_haptics() {
        let encoded = ron::ser::to_string(&VersionTwoStoredControlPreferences {
            version: 2,
            vibration: VibrationLevel::High,
            key_bindings: LegacyPlayerKeyBindings::from_current(&PlayerKeyBindings::default()),
        })
        .unwrap();
        let preferences = decode_control_preferences(&encoded).unwrap().1;
        assert_eq!(preferences.vibration, VibrationLevel::High);
        assert_eq!(preferences.haptic_style, HapticStyle::Competitive);
    }

    #[test]
    fn versions_one_through_three_fill_only_new_special_defaults() {
        let mut current = PlayerKeyBindings::default();
        current.p1.left = KeyCode::KeyQ;
        current.p2.jump = KeyCode::BracketLeft;
        let legacy = LegacyPlayerKeyBindings::from_current(&current);
        let encoded_versions = [
            ron::ser::to_string(&LegacyStoredControlPreferences {
                version: 1,
                vibration_enabled: true,
                key_bindings: legacy.clone(),
            })
            .unwrap(),
            ron::ser::to_string(&VersionTwoStoredControlPreferences {
                version: 2,
                vibration: VibrationLevel::Low,
                key_bindings: legacy.clone(),
            })
            .unwrap(),
            ron::ser::to_string(&VersionThreeStoredControlPreferences {
                version: 3,
                vibration: VibrationLevel::Low,
                haptic_style: HapticStyle::Cinematic,
                key_bindings: legacy,
            })
            .unwrap(),
        ];

        for encoded in &encoded_versions {
            let (migrated, preferences) = decode_control_preferences(encoded).unwrap();
            assert_eq!(migrated.p1.left, KeyCode::KeyQ);
            assert_eq!(migrated.p2.jump, KeyCode::BracketLeft);
            assert_eq!(migrated.p1.special, KeyCode::KeyE);
            assert_eq!(migrated.p2.special, KeyCode::KeyP);
            assert_eq!(migrated.p3.special, KeyCode::Period);
            assert_eq!(migrated.p4.special, KeyCode::Minus);
            assert_eq!(preferences.music, AudioChannelPreference::default());
            assert_eq!(preferences.sound_effects, AudioChannelPreference::default());
        }

        let (_, preferences) = decode_control_preferences(&encoded_versions[2]).unwrap();
        assert_eq!(preferences.vibration, VibrationLevel::Low);
        assert_eq!(preferences.haptic_style, HapticStyle::Cinematic);
    }
}
