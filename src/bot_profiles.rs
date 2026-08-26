use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use std::fs;

use bevy::prelude::*;
use serde::Deserialize;

pub const BOT_PROFILE_CATALOG_PATH: &str = "assets/bots/bot_profiles.ron";
pub const BOT_DECISION_HZ: u32 = 20;
pub const BOT_OPPONENT_HISTORY_TICKS: u32 = 160;
const EMBEDDED_BOT_PROFILE_CATALOG: &str = include_str!("../assets/bots/bot_profiles.ron");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
pub enum BotProfileId {
    Standard,
    Tutorial,
}

impl BotProfileId {
    const fn index(self) -> usize {
        match self {
            Self::Standard => 0,
            Self::Tutorial => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotUtilityWeights {
    pub survive: f32,
    pub regain_stamina: f32,
    pub reposition: f32,
    pub approach: f32,
    pub pressure: f32,
    pub punish: f32,
    pub disengage: f32,
    pub collect_item: f32,
    pub use_item: f32,
    pub objective: f32,
}

impl Default for BotUtilityWeights {
    fn default() -> Self {
        Self {
            survive: 1.35,
            regain_stamina: 0.95,
            reposition: 0.90,
            approach: 1.00,
            pressure: 1.05,
            punish: 1.25,
            disengage: 0.95,
            collect_item: 0.70,
            use_item: 0.85,
            objective: 1.00,
        }
    }
}

impl BotUtilityWeights {
    fn named_values(self) -> [(&'static str, f32); 10] {
        [
            ("survive", self.survive),
            ("regain_stamina", self.regain_stamina),
            ("reposition", self.reposition),
            ("approach", self.approach),
            ("pressure", self.pressure),
            ("punish", self.punish),
            ("disengage", self.disengage),
            ("collect_item", self.collect_item),
            ("use_item", self.use_item),
            ("objective", self.objective),
        ]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotProfile {
    pub tactical_planning_enabled: bool,
    pub forecast_horizon_ticks: u32,
    pub forecast_risk_weight: f32,
    pub tactic_learning_rate: f32,
    pub tactic_bias_cap: f32,
    pub near_optimal_margin: f32,
    pub reaction_ticks_min: u32,
    pub reaction_ticks_max: u32,
    pub perception_error_m: f32,
    pub commitment_ticks_min: u32,
    pub commitment_ticks_max: u32,
    pub target_switch_margin: f32,
    pub danger_health_ratio: f32,
    pub low_stamina_ratio: f32,
    pub attack_stamina_reserve_ratio: f32,
    pub edge_safety_margin_m: f32,
    pub hazard_safety_margin_m: f32,
    pub intentional_mistake_rate: f32,
    pub utility_jitter: f32,
    pub adaptation_cap: f32,
    pub style_modifier_cap: f32,
    pub equipment_modifier_cap: f32,
    pub weights: BotUtilityWeights,
}

impl Default for BotProfile {
    fn default() -> Self {
        standard_bot_profile()
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotProfileEntry {
    pub id: BotProfileId,
    pub profile: BotProfile,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotProfileCatalogFile {
    pub profiles: Vec<BotProfileEntry>,
}

#[derive(Resource, Clone, Debug)]
pub struct BotProfileCatalog {
    profiles: [BotProfile; 2],
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    path: PathBuf,
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    modified: Option<SystemTime>,
    last_error: Option<String>,
}

impl Default for BotProfileCatalog {
    fn default() -> Self {
        initial_bot_profile_catalog(Path::new(BOT_PROFILE_CATALOG_PATH))
    }
}

#[cfg(all(
    feature = "dev-hot-reload",
    not(feature = "shipping"),
    not(target_arch = "wasm32")
))]
fn initial_bot_profile_catalog(path: &Path) -> BotProfileCatalog {
    match load_bot_profile_catalog_file(path).and_then(|(file, modified)| {
        BotProfileCatalog::from_loaded_file(file, path.to_path_buf(), modified)
    }) {
        Ok(catalog) => catalog,
        Err(error) => BotProfileCatalog::compiled_fallback(path.to_path_buf(), error),
    }
}

#[cfg(not(all(
    feature = "dev-hot-reload",
    not(feature = "shipping"),
    not(target_arch = "wasm32")
)))]
fn initial_bot_profile_catalog(_path: &Path) -> BotProfileCatalog {
    BotProfileCatalog::from_embedded_gameplay()
        .expect("the embedded bot profile catalog must remain valid")
}

impl BotProfileCatalog {
    /// Constructs the immutable profile catalog used by online authority and
    /// deterministic fixtures. This never consults the process working
    /// directory or native hot-reload state.
    pub(crate) fn from_embedded_gameplay() -> Result<Self, String> {
        let file: BotProfileCatalogFile = ron::from_str(EMBEDDED_BOT_PROFILE_CATALOG)
            .map_err(|error| format!("RON parse failed: {error}"))?;
        validate_bot_profile_catalog(&file)?;
        Self::from_loaded_file(file, PathBuf::from(BOT_PROFILE_CATALOG_PATH), None)
    }

    pub fn profile(&self, id: BotProfileId) -> BotProfile {
        self.profiles[id.index()]
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    #[allow(dead_code)]
    pub(crate) fn from_file(file: BotProfileCatalogFile) -> Result<Self, String> {
        Self::from_loaded_file(file, PathBuf::from(BOT_PROFILE_CATALOG_PATH), None)
    }

    fn from_loaded_file(
        file: BotProfileCatalogFile,
        path: PathBuf,
        modified: Option<SystemTime>,
    ) -> Result<Self, String> {
        Ok(Self {
            profiles: normalize_profiles(&file)?,
            path,
            modified,
            last_error: None,
        })
    }

    fn compiled_fallback(path: PathBuf, error: String) -> Self {
        let profiles = normalize_profiles(&default_bot_profile_catalog_file())
            .expect("compiled bot profiles must be valid");
        Self {
            profiles,
            path,
            modified: None,
            last_error: Some(error),
        }
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    fn reload_if_changed(&mut self) -> bool {
        let metadata = match fs::metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) => {
                self.last_error = Some(format!(
                    "failed to inspect {}: {error}",
                    self.path.display()
                ));
                return false;
            }
        };
        let modified = match metadata.modified() {
            Ok(modified) => modified,
            Err(error) => {
                self.last_error = Some(format!(
                    "failed to read modification time for {}: {error}",
                    self.path.display()
                ));
                return false;
            }
        };
        if self.last_error.is_none() && self.modified.is_some_and(|previous| previous >= modified) {
            return false;
        }

        match load_bot_profile_catalog_file(&self.path).and_then(|(file, loaded_modified)| {
            normalize_profiles(&file).map(|profiles| (profiles, loaded_modified))
        }) {
            Ok((profiles, loaded_modified)) => {
                self.profiles = profiles;
                self.modified = loaded_modified;
                self.last_error = None;
                true
            }
            Err(error) => {
                self.last_error = Some(error);
                false
            }
        }
    }
}

pub fn setup_bot_profile_catalog(mut commands: Commands) {
    let catalog = BotProfileCatalog::default();
    if let Some(error) = catalog.last_error() {
        warn!("Bot profile catalog started with compiled defaults: {error}");
    }
    commands.insert_resource(catalog);
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub fn reload_bot_profile_catalog(
    time: Res<Time>,
    mut next_check_at: Local<f32>,
    mut catalog: ResMut<BotProfileCatalog>,
) {
    let now = time.elapsed_secs();
    if now < *next_check_at {
        return;
    }
    *next_check_at = now + 0.5;

    let previous_error = catalog.last_error().map(str::to_owned);
    if catalog.reload_if_changed() {
        info!(
            "Reloaded bot profile catalog from {}",
            catalog.path.display()
        );
    } else if catalog.last_error().map(str::to_owned) != previous_error
        && let Some(error) = catalog.last_error()
    {
        warn!("Keeping last valid bot profile catalog: {error}");
    }
}

pub(crate) fn load_bot_profile_catalog_file(
    path: &Path,
) -> Result<(BotProfileCatalogFile, Option<SystemTime>), String> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = path;
        let contents = include_str!("../assets/bots/bot_profiles.ron");
        let file: BotProfileCatalogFile =
            ron::from_str(contents).map_err(|error| format!("RON parse failed: {error}"))?;
        validate_bot_profile_catalog(&file)?;
        return Ok((file, None));
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    {
        let contents = fs::read_to_string(path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        let file: BotProfileCatalogFile =
            ron::from_str(&contents).map_err(|error| format!("RON parse failed: {error}"))?;
        validate_bot_profile_catalog(&file)?;
        let modified = fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .map_err(|error| {
                format!(
                    "failed to read modification time for {}: {error}",
                    path.display()
                )
            })?;
        Ok((file, Some(modified)))
    }

    #[cfg(all(not(feature = "native"), not(target_arch = "wasm32")))]
    {
        let _ = path;
        let file: BotProfileCatalogFile = ron::from_str(EMBEDDED_BOT_PROFILE_CATALOG)
            .map_err(|error| format!("RON parse failed: {error}"))?;
        validate_bot_profile_catalog(&file)?;
        Ok((file, None))
    }
}

fn normalize_profiles(file: &BotProfileCatalogFile) -> Result<[BotProfile; 2], String> {
    validate_bot_profile_catalog(file)?;
    let mut profiles = [standard_bot_profile(), tutorial_bot_profile()];
    for entry in &file.profiles {
        profiles[entry.id.index()] = entry.profile;
    }
    Ok(profiles)
}

fn validate_bot_profile_catalog(file: &BotProfileCatalogFile) -> Result<(), String> {
    let mut seen = [false; 2];
    for entry in &file.profiles {
        let index = entry.id.index();
        if seen[index] {
            return Err(format!("duplicate bot profile for {:?}", entry.id));
        }
        seen[index] = true;
        validate_bot_profile(entry.id, &entry.profile)?;
    }

    for id in [BotProfileId::Standard, BotProfileId::Tutorial] {
        if !seen[id.index()] {
            return Err(format!("missing bot profile for {id:?}"));
        }
    }
    Ok(())
}

fn validate_bot_profile(id: BotProfileId, profile: &BotProfile) -> Result<(), String> {
    if !(1..=40).contains(&profile.forecast_horizon_ticks) {
        return Err(format!("{id:?}.forecast_horizon_ticks must be in 1..=40"));
    }
    if profile.reaction_ticks_min < 1
        || profile.reaction_ticks_min > profile.reaction_ticks_max
        || profile.reaction_ticks_max > 20
    {
        return Err(format!(
            "{id:?}.reaction_ticks must satisfy 1 <= min <= max <= 20"
        ));
    }
    if profile.commitment_ticks_min < 1
        || profile.commitment_ticks_min > profile.commitment_ticks_max
        || profile.commitment_ticks_max > 40
    {
        return Err(format!(
            "{id:?}.commitment_ticks must satisfy 1 <= min <= max <= 40"
        ));
    }

    for (name, value) in [
        ("target_switch_margin", profile.target_switch_margin),
        ("danger_health_ratio", profile.danger_health_ratio),
        ("low_stamina_ratio", profile.low_stamina_ratio),
        (
            "attack_stamina_reserve_ratio",
            profile.attack_stamina_reserve_ratio,
        ),
        ("intentional_mistake_rate", profile.intentional_mistake_rate),
        ("utility_jitter", profile.utility_jitter),
        ("adaptation_cap", profile.adaptation_cap),
        ("style_modifier_cap", profile.style_modifier_cap),
        ("equipment_modifier_cap", profile.equipment_modifier_cap),
        ("forecast_risk_weight", profile.forecast_risk_weight),
        ("tactic_learning_rate", profile.tactic_learning_rate),
        ("tactic_bias_cap", profile.tactic_bias_cap),
        ("near_optimal_margin", profile.near_optimal_margin),
    ] {
        validate_float_range(id, name, value, 0.0, 1.0)?;
    }

    for (name, value) in [
        ("perception_error_m", profile.perception_error_m),
        ("edge_safety_margin_m", profile.edge_safety_margin_m),
        ("hazard_safety_margin_m", profile.hazard_safety_margin_m),
    ] {
        validate_float_range(id, name, value, 0.0, 10.0)?;
    }

    let mut any_positive_weight = false;
    for (name, value) in profile.weights.named_values() {
        validate_float_range(id, name, value, 0.0, 4.0)?;
        any_positive_weight |= value > 0.0;
    }
    if !any_positive_weight {
        return Err(format!("{id:?}.weights must contain a positive value"));
    }

    Ok(())
}

fn validate_float_range(
    id: BotProfileId,
    name: &str,
    value: f32,
    min: f32,
    max: f32,
) -> Result<(), String> {
    if !value.is_finite() || !(min..=max).contains(&value) {
        return Err(format!(
            "{id:?}.{name} must be finite and in {min}..={max}, got {value}"
        ));
    }
    Ok(())
}

fn default_bot_profile_catalog_file() -> BotProfileCatalogFile {
    BotProfileCatalogFile {
        profiles: vec![
            BotProfileEntry {
                id: BotProfileId::Standard,
                profile: standard_bot_profile(),
            },
            BotProfileEntry {
                id: BotProfileId::Tutorial,
                profile: tutorial_bot_profile(),
            },
        ],
    }
}

const fn standard_bot_profile() -> BotProfile {
    BotProfile {
        tactical_planning_enabled: true,
        forecast_horizon_ticks: 20,
        forecast_risk_weight: 0.35,
        tactic_learning_rate: 0.12,
        tactic_bias_cap: 0.75,
        near_optimal_margin: 0.25,
        reaction_ticks_min: 3,
        reaction_ticks_max: 5,
        perception_error_m: 0.18,
        commitment_ticks_min: 2,
        commitment_ticks_max: 10,
        target_switch_margin: 0.20,
        danger_health_ratio: 0.28,
        low_stamina_ratio: 0.30,
        attack_stamina_reserve_ratio: 0.15,
        edge_safety_margin_m: 1.25,
        hazard_safety_margin_m: 1.75,
        intentional_mistake_rate: 0.08,
        utility_jitter: 0.06,
        adaptation_cap: 0.20,
        style_modifier_cap: 0.10,
        equipment_modifier_cap: 0.10,
        weights: BotUtilityWeights {
            survive: 1.35,
            regain_stamina: 0.95,
            reposition: 0.90,
            approach: 1.00,
            pressure: 1.05,
            punish: 1.25,
            disengage: 0.95,
            collect_item: 0.70,
            use_item: 0.85,
            objective: 1.00,
        },
    }
}

const fn tutorial_bot_profile() -> BotProfile {
    BotProfile {
        tactical_planning_enabled: false,
        forecast_horizon_ticks: 20,
        forecast_risk_weight: 0.35,
        tactic_learning_rate: 0.12,
        tactic_bias_cap: 0.75,
        near_optimal_margin: 0.25,
        reaction_ticks_min: 5,
        reaction_ticks_max: 7,
        perception_error_m: 0.35,
        commitment_ticks_min: 3,
        commitment_ticks_max: 10,
        target_switch_margin: 0.30,
        danger_health_ratio: 0.35,
        low_stamina_ratio: 0.35,
        attack_stamina_reserve_ratio: 0.25,
        edge_safety_margin_m: 1.50,
        hazard_safety_margin_m: 2.00,
        intentional_mistake_rate: 0.18,
        utility_jitter: 0.10,
        adaptation_cap: 0.05,
        style_modifier_cap: 0.05,
        equipment_modifier_cap: 0.05,
        weights: BotUtilityWeights {
            survive: 1.45,
            regain_stamina: 1.10,
            reposition: 1.00,
            approach: 0.75,
            pressure: 0.55,
            punish: 0.65,
            disengage: 1.15,
            collect_item: 0.35,
            use_item: 0.50,
            objective: 0.85,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    use std::sync::atomic::{AtomicU64, Ordering};

    fn valid_file() -> BotProfileCatalogFile {
        default_bot_profile_catalog_file()
    }

    fn profile_mut(file: &mut BotProfileCatalogFile, id: BotProfileId) -> &mut BotProfile {
        &mut file
            .profiles
            .iter_mut()
            .find(|entry| entry.id == id)
            .expect("fixture profile must exist")
            .profile
    }

    fn assert_profile_invalid(edit: impl FnOnce(&mut BotProfile)) {
        let mut file = valid_file();
        edit(profile_mut(&mut file, BotProfileId::Standard));
        assert!(validate_bot_profile_catalog(&file).is_err());
    }

    #[test]
    fn committed_catalog_matches_compiled_profiles() {
        let (file, _) = load_bot_profile_catalog_file(Path::new(BOT_PROFILE_CATALOG_PATH)).unwrap();
        let catalog = BotProfileCatalog::from_file(file).unwrap();

        assert_eq!(
            catalog.profile(BotProfileId::Standard),
            standard_bot_profile()
        );
        assert_eq!(
            catalog.profile(BotProfileId::Tutorial),
            tutorial_bot_profile()
        );
    }

    #[test]
    fn profile_lookup_returns_exact_authored_profile() {
        let mut file = valid_file();
        profile_mut(&mut file, BotProfileId::Standard).target_switch_margin = 0.23;
        profile_mut(&mut file, BotProfileId::Tutorial).target_switch_margin = 0.31;
        let expected_standard = *profile_mut(&mut file, BotProfileId::Standard);
        let expected_tutorial = *profile_mut(&mut file, BotProfileId::Tutorial);

        let catalog = BotProfileCatalog::from_file(file).unwrap();

        assert_eq!(catalog.profile(BotProfileId::Standard), expected_standard);
        assert_eq!(catalog.profile(BotProfileId::Tutorial), expected_tutorial);
    }

    #[test]
    fn ron_schema_is_strict() {
        let authored = include_str!("../assets/bots/bot_profiles.ron");

        let unknown_field = authored.replacen("profiles: [", "unexpected: 1,\n    profiles: [", 1);
        assert_ne!(unknown_field, authored);
        assert!(ron::from_str::<BotProfileCatalogFile>(&unknown_field).is_err());

        let missing_field = authored.replacen("reaction_ticks_min: 3,", "", 1);
        assert_ne!(missing_field, authored);
        assert!(ron::from_str::<BotProfileCatalogFile>(&missing_field).is_err());

        let unknown_id = authored.replacen("id: Standard", "id: Impossible", 1);
        assert_ne!(unknown_id, authored);
        assert!(ron::from_str::<BotProfileCatalogFile>(&unknown_id).is_err());

        assert!(ron::from_str::<BotProfileCatalogFile>("()").is_err());
    }

    #[test]
    fn catalog_requires_one_of_each_profile() {
        let mut missing = valid_file();
        missing
            .profiles
            .retain(|entry| entry.id != BotProfileId::Tutorial);
        let missing_error = validate_bot_profile_catalog(&missing).unwrap_err();
        assert!(missing_error.contains("missing bot profile"));

        let mut duplicate = valid_file();
        let duplicate_standard = duplicate
            .profiles
            .iter()
            .find(|entry| entry.id == BotProfileId::Standard)
            .unwrap()
            .clone();
        duplicate.profiles.push(duplicate_standard);
        let duplicate_error = validate_bot_profile_catalog(&duplicate).unwrap_err();
        assert!(duplicate_error.contains("duplicate bot profile"));
    }

    #[test]
    fn semantic_validation_rejects_ticks_ranges_and_nonfinite_values() {
        assert_profile_invalid(|profile| profile.reaction_ticks_min = 0);
        assert_profile_invalid(|profile| profile.forecast_horizon_ticks = 0);
        assert_profile_invalid(|profile| profile.forecast_horizon_ticks = 41);
        assert_profile_invalid(|profile| profile.tactic_bias_cap = 1.01);
        assert_profile_invalid(|profile| profile.reaction_ticks_max = 21);
        assert_profile_invalid(|profile| {
            profile.reaction_ticks_min = 6;
            profile.reaction_ticks_max = 5;
        });
        assert_profile_invalid(|profile| profile.commitment_ticks_min = 0);
        assert_profile_invalid(|profile| profile.commitment_ticks_max = 41);
        assert_profile_invalid(|profile| {
            profile.commitment_ticks_min = 11;
            profile.commitment_ticks_max = 10;
        });
        assert_profile_invalid(|profile| profile.perception_error_m = f32::NAN);
        assert_profile_invalid(|profile| profile.target_switch_margin = 1.01);
        assert_profile_invalid(|profile| profile.edge_safety_margin_m = 10.01);
        assert_profile_invalid(|profile| profile.weights.survive = -0.01);
        assert_profile_invalid(|profile| profile.weights.survive = 4.01);
        assert_profile_invalid(|profile| {
            profile.weights = BotUtilityWeights {
                survive: 0.0,
                regain_stamina: 0.0,
                reposition: 0.0,
                approach: 0.0,
                pressure: 0.0,
                punish: 0.0,
                disengage: 0.0,
                collect_item: 0.0,
                use_item: 0.0,
                objective: 0.0,
            };
        });
    }

    #[test]
    fn compiled_fallback_retains_error_and_exact_defaults() {
        let path = PathBuf::from("missing-bot-profiles.ron");
        let catalog = BotProfileCatalog::compiled_fallback(path.clone(), "missing".to_string());

        assert_eq!(catalog.path, path);
        assert_eq!(catalog.modified, None);
        assert_eq!(catalog.last_error(), Some("missing"));
        assert_eq!(
            catalog.profile(BotProfileId::Standard),
            standard_bot_profile()
        );
        assert_eq!(
            catalog.profile(BotProfileId::Tutorial),
            tutorial_bot_profile()
        );
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    static NEXT_TEMP_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    struct TestDirectory(PathBuf);

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    impl TestDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEMP_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "afc-bot-profile-test-{}-{sequence}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn file(&self) -> PathBuf {
            self.0.join("bot_profiles.ron")
        }
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    #[test]
    fn failed_reload_retains_last_valid_catalog_and_valid_recovery_swaps_atomically() {
        let directory = TestDirectory::new();
        let path = directory.file();
        let authored = include_str!("../assets/bots/bot_profiles.ron");
        std::fs::write(&path, authored).unwrap();
        let (file, modified) = load_bot_profile_catalog_file(&path).unwrap();
        let mut catalog =
            BotProfileCatalog::from_loaded_file(file, path.clone(), modified).unwrap();
        let before = catalog.profiles;

        std::fs::remove_file(&path).unwrap();
        assert!(!catalog.reload_if_changed());
        assert_eq!(catalog.profiles, before);
        assert!(catalog.last_error().is_some());

        std::fs::write(&path, "not valid RON").unwrap();
        assert!(!catalog.reload_if_changed());
        assert_eq!(catalog.profiles, before);
        assert!(catalog.last_error().is_some());

        let recovered = authored.replacen(
            "target_switch_margin: 0.20",
            "target_switch_margin: 0.25",
            1,
        );
        assert_ne!(recovered, authored);
        std::fs::write(&path, recovered).unwrap();
        assert!(catalog.reload_if_changed());
        assert_eq!(
            catalog.profile(BotProfileId::Standard).target_switch_margin,
            0.25
        );
        assert_eq!(
            catalog.profile(BotProfileId::Tutorial),
            tutorial_bot_profile()
        );
        assert!(catalog.last_error().is_none());
    }
}
