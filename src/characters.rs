use std::path::Path;

#[cfg(any(
    test,
    all(
        feature = "dev-hot-reload",
        not(feature = "shipping"),
        not(target_arch = "wasm32")
    )
))]
use std::fs;
#[cfg(any(
    test,
    all(
        feature = "dev-hot-reload",
        not(feature = "shipping"),
        not(target_arch = "wasm32")
    )
))]
use std::{path::PathBuf, time::SystemTime};

use bevy::prelude::*;
use serde::Deserialize;

use crate::constants::FIGHTER_COUNT;
use crate::techniques::TechniqueId;

pub const CHARACTER_MOVE_CATALOG_PATH: &str = "assets/characters/character_move_sets.ron";
const EMBEDDED_CHARACTER_MOVE_CATALOG: &str =
    include_str!("../assets/characters/character_move_sets.ron");

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
pub enum CharacterKind {
    Cat,
    Pig,
    Dog,
    Fox,
    Panda,
    Bee,
    Penguin,
    Chick,
}

pub const CHARACTER_KINDS: [CharacterKind; 8] = [
    CharacterKind::Cat,
    CharacterKind::Pig,
    CharacterKind::Dog,
    CharacterKind::Fox,
    CharacterKind::Panda,
    CharacterKind::Bee,
    CharacterKind::Penguin,
    CharacterKind::Chick,
];

pub const DEFAULT_FIGHTER_CHARACTERS: [CharacterKind; FIGHTER_COUNT] = [
    CharacterKind::Cat,
    CharacterKind::Pig,
    CharacterKind::Fox,
    CharacterKind::Panda,
];

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct FighterCharacter {
    pub kind: CharacterKind,
}

impl FighterCharacter {
    pub fn new(kind: CharacterKind) -> Self {
        Self { kind }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub enum CharacterMoveSlot {
    DashLight,
    DashHeavy,
    JumpLight,
    JumpHeavy,
    UltimateStartup,
    UltimateRush,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct CharacterProfileDef {
    pub kind: CharacterKind,
    pub label: String,
    pub scene: String,
    pub move_set: String,
    #[serde(default)]
    pub body: CharacterBodyDef,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(default)]
pub struct CharacterMeshBounds {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

impl Default for CharacterMeshBounds {
    fn default() -> Self {
        character_mesh_bounds(CharacterKind::Cat)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(default)]
pub struct CharacterBodyDef {
    pub ground_speed: f32,
    pub air_speed: f32,
    pub dash_impulse: f32,
    pub jump_impulse: f32,
    pub gravity: f32,
    pub fall_gravity: f32,
    pub stop_friction: f32,
    pub landing_stick: f32,
    pub dash_slide: f32,
    pub mesh_bounds: CharacterMeshBounds,
}

impl Default for CharacterBodyDef {
    fn default() -> Self {
        Self {
            ground_speed: 1.0,
            air_speed: 1.0,
            dash_impulse: 1.0,
            jump_impulse: 1.0,
            gravity: 1.0,
            fall_gravity: 1.0,
            stop_friction: 1.0,
            landing_stick: 1.0,
            dash_slide: 1.0,
            mesh_bounds: CharacterMeshBounds::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct CharacterMoveSlotDef {
    pub slot: CharacterMoveSlot,
    pub technique: TechniqueId,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct CharacterMoveSetDef {
    pub id: String,
    pub order: Vec<TechniqueId>,
    #[serde(default)]
    pub slots: Vec<CharacterMoveSlotDef>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct CharacterMoveCatalogFile {
    pub characters: Vec<CharacterProfileDef>,
    pub move_sets: Vec<CharacterMoveSetDef>,
}

#[derive(Resource, Clone, Debug)]
pub struct CharacterMoveCatalog {
    file: CharacterMoveCatalogFile,
    #[cfg(any(
        test,
        all(
            feature = "dev-hot-reload",
            not(feature = "shipping"),
            not(target_arch = "wasm32")
        )
    ))]
    path: PathBuf,
    #[cfg(any(
        test,
        all(
            feature = "dev-hot-reload",
            not(feature = "shipping"),
            not(target_arch = "wasm32")
        )
    ))]
    modified: Option<SystemTime>,
    last_error: Option<String>,
}

impl Default for CharacterMoveCatalog {
    fn default() -> Self {
        initial_character_move_catalog(Path::new(CHARACTER_MOVE_CATALOG_PATH))
    }
}

#[cfg(all(
    feature = "dev-hot-reload",
    not(feature = "shipping"),
    not(target_arch = "wasm32")
))]
fn initial_character_move_catalog(path: &Path) -> CharacterMoveCatalog {
    match load_character_move_catalog_file(path) {
        Ok((file, modified)) => CharacterMoveCatalog {
            file,
            path: path.to_path_buf(),
            modified,
            last_error: None,
        },
        Err(error) => CharacterMoveCatalog {
            file: default_character_move_catalog_file(),
            path: path.to_path_buf(),
            modified: None,
            last_error: Some(error),
        },
    }
}

#[cfg(not(all(
    feature = "dev-hot-reload",
    not(feature = "shipping"),
    not(target_arch = "wasm32")
)))]
fn initial_character_move_catalog(_path: &Path) -> CharacterMoveCatalog {
    CharacterMoveCatalog::from_embedded_gameplay()
        .expect("the embedded character move catalog must remain valid")
}

impl CharacterMoveCatalog {
    /// Constructs the immutable gameplay catalog used by online authority and
    /// prediction worlds. This never consults the working directory or the
    /// native file-watcher path.
    pub(crate) fn from_embedded_gameplay() -> Result<Self, String> {
        Ok(Self {
            file: parse_character_move_catalog(EMBEDDED_CHARACTER_MOVE_CATALOG)?,
            #[cfg(any(
                test,
                all(
                    feature = "dev-hot-reload",
                    not(feature = "shipping"),
                    not(target_arch = "wasm32")
                )
            ))]
            path: PathBuf::from(CHARACTER_MOVE_CATALOG_PATH),
            #[cfg(any(
                test,
                all(
                    feature = "dev-hot-reload",
                    not(feature = "shipping"),
                    not(target_arch = "wasm32")
                )
            ))]
            modified: None,
            last_error: None,
        })
    }

    #[allow(dead_code)]
    pub fn from_file(file: CharacterMoveCatalogFile) -> Self {
        let last_error = validate_catalog(&file).err();
        Self {
            file: if last_error.is_none() {
                file
            } else {
                default_character_move_catalog_file()
            },
            #[cfg(any(
                test,
                all(
                    feature = "dev-hot-reload",
                    not(feature = "shipping"),
                    not(target_arch = "wasm32")
                )
            ))]
            path: PathBuf::from(CHARACTER_MOVE_CATALOG_PATH),
            #[cfg(any(
                test,
                all(
                    feature = "dev-hot-reload",
                    not(feature = "shipping"),
                    not(target_arch = "wasm32")
                )
            ))]
            modified: None,
            last_error,
        }
    }

    pub fn profile(&self, kind: CharacterKind) -> Option<&CharacterProfileDef> {
        self.file
            .characters
            .iter()
            .find(|profile| profile.kind == kind)
    }

    #[allow(dead_code)]
    pub fn label(&self, kind: CharacterKind) -> &str {
        self.profile(kind)
            .map(|profile| profile.label.as_str())
            .unwrap_or_else(|| character_label(kind))
    }

    pub fn scene_path(&self, kind: CharacterKind) -> Option<&str> {
        self.profile(kind).map(|profile| profile.scene.as_str())
    }

    pub fn body(&self, kind: CharacterKind) -> CharacterBodyDef {
        self.profile(kind)
            .map(|profile| profile.body)
            .unwrap_or_default()
    }

    pub fn ordered_techniques(&self, kind: CharacterKind) -> &[TechniqueId] {
        self.move_set_for_character(kind)
            .map(|move_set| move_set.order.as_slice())
            .unwrap_or(&[])
    }

    pub fn allows_technique(&self, kind: CharacterKind, technique: TechniqueId) -> bool {
        technique.allowed_for_character(kind) && self.ordered_techniques(kind).contains(&technique)
    }

    pub fn slot_technique(
        &self,
        kind: CharacterKind,
        slot: CharacterMoveSlot,
    ) -> Option<TechniqueId> {
        self.move_set_for_character(kind)?
            .slots
            .iter()
            .find(|slot_def| slot_def.slot == slot)
            .map(|slot_def| slot_def.technique)
            .filter(|technique| technique.allowed_for_character(kind))
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn authored_file_for_test(&self) -> &CharacterMoveCatalogFile {
        &self.file
    }

    #[cfg(all(
        feature = "dev-hot-reload",
        not(feature = "shipping"),
        not(target_arch = "wasm32")
    ))]
    fn reload_if_changed(&mut self) -> bool {
        let Ok(metadata) = fs::metadata(&self.path) else {
            return false;
        };
        let Ok(modified) = metadata.modified() else {
            return false;
        };
        if self.modified.is_some_and(|previous| previous >= modified) {
            return false;
        }

        match load_character_move_catalog_file(&self.path) {
            Ok((file, modified)) => {
                self.file = file;
                self.modified = modified;
                self.last_error = None;
                true
            }
            Err(error) => {
                self.last_error = Some(error);
                false
            }
        }
    }

    fn move_set_for_character(&self, kind: CharacterKind) -> Option<&CharacterMoveSetDef> {
        let move_set_id = &self.profile(kind)?.move_set;
        self.file
            .move_sets
            .iter()
            .find(|move_set| &move_set.id == move_set_id)
    }
}

pub fn setup_character_move_catalog(mut commands: Commands) {
    let catalog = CharacterMoveCatalog::default();
    if let Some(error) = catalog.last_error() {
        warn!("Character move catalog started with defaults: {error}");
    }
    commands.insert_resource(catalog);
}

#[cfg(all(
    feature = "dev-hot-reload",
    not(feature = "shipping"),
    not(target_arch = "wasm32")
))]
pub fn reload_character_move_catalog(
    time: Res<Time>,
    simulation_drive: Res<crate::simulation::SimulationDriveMode>,
    mut next_check_at: Local<f32>,
    mut catalog: ResMut<CharacterMoveCatalog>,
) {
    if *simulation_drive == crate::simulation::SimulationDriveMode::ExternalProjection {
        return;
    }
    let now = time.elapsed_secs();
    if now < *next_check_at {
        return;
    }
    *next_check_at = now + 0.5;

    let previous_error = catalog.last_error().map(str::to_owned);
    if catalog.reload_if_changed() {
        info!(
            "Reloaded character move catalog from {}",
            catalog.path.display()
        );
    } else if catalog.last_error().map(str::to_owned) != previous_error
        && let Some(error) = catalog.last_error()
    {
        warn!("Keeping last valid character move catalog: {error}");
    }
}

pub fn character_for_fighter_id(id: usize) -> CharacterKind {
    DEFAULT_FIGHTER_CHARACTERS[id.min(DEFAULT_FIGHTER_CHARACTERS.len() - 1)]
}

#[cfg(any(
    test,
    all(
        feature = "dev-hot-reload",
        not(feature = "shipping"),
        not(target_arch = "wasm32")
    )
))]
pub fn next_character_kind(kind: CharacterKind) -> CharacterKind {
    CHARACTER_KINDS
        .iter()
        .position(|candidate| *candidate == kind)
        .map(|index| CHARACTER_KINDS[(index + 1) % CHARACTER_KINDS.len()])
        .unwrap_or(CharacterKind::Cat)
}

#[cfg(any(
    test,
    all(
        feature = "dev-hot-reload",
        not(feature = "shipping"),
        not(target_arch = "wasm32")
    )
))]
pub fn previous_character_kind(kind: CharacterKind) -> CharacterKind {
    CHARACTER_KINDS
        .iter()
        .position(|candidate| *candidate == kind)
        .map(|index| CHARACTER_KINDS[(index + CHARACTER_KINDS.len() - 1) % CHARACTER_KINDS.len()])
        .unwrap_or(CharacterKind::Cat)
}

pub fn character_label(kind: CharacterKind) -> &'static str {
    match kind {
        CharacterKind::Cat => "Cat",
        CharacterKind::Pig => "Pig",
        CharacterKind::Dog => "Dog",
        CharacterKind::Fox => "Fox",
        CharacterKind::Panda => "Panda",
        CharacterKind::Bee => "Bee",
        CharacterKind::Penguin => "Penguin",
        CharacterKind::Chick => "Chick",
    }
}

pub fn character_scene_model(
    asset_server: &AssetServer,
    catalog: &CharacterMoveCatalog,
    kind: CharacterKind,
) -> Option<Handle<Scene>> {
    let path = catalog.scene_path(kind)?;

    #[cfg(target_arch = "wasm32")]
    {
        return Some(asset_server.load(GltfAssetLabel::Scene(0).from_asset(path.to_owned())));
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    {
        let runtime_path = Path::new("assets").join(path);
        runtime_path
            .exists()
            .then(|| asset_server.load(GltfAssetLabel::Scene(0).from_asset(path.to_owned())))
    }

    #[cfg(not(any(feature = "native", target_arch = "wasm32")))]
    {
        let _ = (asset_server, path);
        None
    }
}

#[cfg(all(
    not(target_arch = "wasm32"),
    any(test, all(feature = "dev-hot-reload", not(feature = "shipping")))
))]
pub(crate) fn load_character_move_catalog_file(
    path: &Path,
) -> Result<(CharacterMoveCatalogFile, Option<SystemTime>), String> {
    if !path.exists() {
        return Ok((default_character_move_catalog_file(), None));
    }

    let contents = fs::read_to_string(path).map_err(|error| error.to_string())?;
    let file = parse_character_move_catalog(&contents)?;
    let modified = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .map_err(|error| error.to_string())?;
    Ok((file, Some(modified)))
}

#[cfg(all(test, target_arch = "wasm32"))]
pub(crate) fn load_character_move_catalog_file(
    _path: &Path,
) -> Result<(CharacterMoveCatalogFile, Option<SystemTime>), String> {
    Ok((
        parse_character_move_catalog(EMBEDDED_CHARACTER_MOVE_CATALOG)?,
        None,
    ))
}

fn parse_character_move_catalog(contents: &str) -> Result<CharacterMoveCatalogFile, String> {
    let file: CharacterMoveCatalogFile =
        ron::from_str(contents).map_err(|error| format!("RON parse failed: {error}"))?;
    validate_catalog(&file)?;
    Ok(file)
}

fn validate_catalog(file: &CharacterMoveCatalogFile) -> Result<(), String> {
    for (index, profile) in file.characters.iter().enumerate() {
        if file.characters[..index]
            .iter()
            .any(|prior| prior.kind == profile.kind)
        {
            return Err(format!(
                "duplicate character profile for {:?}",
                profile.kind
            ));
        }
        if profile.label.is_empty() || profile.scene.is_empty() || profile.move_set.is_empty() {
            return Err(format!(
                "character profile for {:?} has an empty required identifier",
                profile.kind
            ));
        }
        let body = profile.body;
        let scalars = [
            body.ground_speed,
            body.air_speed,
            body.dash_impulse,
            body.jump_impulse,
            body.gravity,
            body.fall_gravity,
            body.stop_friction,
            body.landing_stick,
            body.dash_slide,
        ];
        if scalars.into_iter().any(|value| !value.is_finite())
            || body
                .mesh_bounds
                .min
                .into_iter()
                .chain(body.mesh_bounds.max)
                .any(|value| !value.is_finite())
        {
            return Err(format!(
                "character profile for {:?} contains a non-finite body value",
                profile.kind
            ));
        }
        if body
            .mesh_bounds
            .min
            .into_iter()
            .zip(body.mesh_bounds.max)
            .any(|(minimum, maximum)| minimum > maximum)
        {
            return Err(format!(
                "character profile for {:?} has inverted mesh bounds",
                profile.kind
            ));
        }
    }
    for (index, move_set) in file.move_sets.iter().enumerate() {
        if move_set.id.is_empty() {
            return Err("move set ID must not be empty".to_owned());
        }
        if file.move_sets[..index]
            .iter()
            .any(|prior| prior.id == move_set.id)
        {
            return Err(format!("duplicate move set '{}'", move_set.id));
        }
        for (slot_index, slot) in move_set.slots.iter().enumerate() {
            if move_set.slots[..slot_index]
                .iter()
                .any(|prior| prior.slot == slot.slot)
            {
                return Err(format!(
                    "move set '{}' has duplicate {:?} slots",
                    move_set.id, slot.slot
                ));
            }
        }
    }

    for kind in CHARACTER_KINDS {
        let Some(profile) = file.characters.iter().find(|profile| profile.kind == kind) else {
            return Err(format!("missing character profile for {kind:?}"));
        };
        let Some(move_set) = file
            .move_sets
            .iter()
            .find(|move_set| move_set.id == profile.move_set)
        else {
            return Err(format!(
                "missing move set '{}' for {kind:?}",
                profile.move_set
            ));
        };
        if move_set.order.is_empty() {
            return Err(format!("move set '{}' has no ordered moves", move_set.id));
        }
    }
    Ok(())
}

pub fn character_body_profile(kind: CharacterKind) -> CharacterBodyDef {
    CharacterBodyDef {
        mesh_bounds: character_mesh_bounds(kind),
        ..default()
    }
}

pub fn character_mesh_bounds(kind: CharacterKind) -> CharacterMeshBounds {
    match kind {
        CharacterKind::Cat => CharacterMeshBounds {
            min: [-0.625, -0.3, -0.625],
            max: [0.625, 1.404, 0.702],
        },
        CharacterKind::Pig => CharacterMeshBounds {
            min: [-0.625, -0.3, -0.625],
            max: [0.625, 1.403, 0.835],
        },
        CharacterKind::Dog => CharacterMeshBounds {
            min: [-0.625, -0.3, -0.625],
            max: [0.635, 1.403, 0.876],
        },
        CharacterKind::Fox => CharacterMeshBounds {
            min: [-0.625, -0.305, -0.879],
            max: [0.625, 1.505, 0.935],
        },
        CharacterKind::Panda => CharacterMeshBounds {
            min: [-0.672, -0.3, -0.625],
            max: [0.672, 1.32, 0.815],
        },
        CharacterKind::Bee => CharacterMeshBounds {
            min: [-0.625, -0.3, -0.625],
            max: [0.625, 1.4, 0.8],
        },
        CharacterKind::Penguin => CharacterMeshBounds {
            min: [-0.625, -0.3, -0.625],
            max: [0.625, 1.34, 0.82],
        },
        CharacterKind::Chick => CharacterMeshBounds {
            min: [-0.625, -0.3, -0.625],
            max: [0.625, 1.413, 0.725],
        },
    }
}

fn default_character_move_catalog_file() -> CharacterMoveCatalogFile {
    CharacterMoveCatalogFile {
        characters: vec![
            CharacterProfileDef {
                kind: CharacterKind::Cat,
                label: "Cat".to_string(),
                scene: "characters/kenney_cube_pets/animal-cat.glb".to_string(),
                move_set: "cat_cube".to_string(),
                body: character_body_profile(CharacterKind::Cat),
            },
            CharacterProfileDef {
                kind: CharacterKind::Pig,
                label: "Pig".to_string(),
                scene: "characters/kenney_cube_pets/animal-pig.glb".to_string(),
                move_set: "pig_cube".to_string(),
                body: pig_body_profile(),
            },
            CharacterProfileDef {
                kind: CharacterKind::Dog,
                label: "Dog".to_string(),
                scene: "characters/kenney_cube_pets/animal-dog.glb".to_string(),
                move_set: "dog_cube".to_string(),
                body: character_body_profile(CharacterKind::Dog),
            },
            CharacterProfileDef {
                kind: CharacterKind::Fox,
                label: "Fox".to_string(),
                scene: "characters/kenney_cube_pets/animal-fox.glb".to_string(),
                move_set: "fox_cube".to_string(),
                body: character_body_profile(CharacterKind::Fox),
            },
            CharacterProfileDef {
                kind: CharacterKind::Panda,
                label: "Panda".to_string(),
                scene: "characters/kenney_cube_pets/animal-panda.glb".to_string(),
                move_set: "panda_cube".to_string(),
                body: character_body_profile(CharacterKind::Panda),
            },
            CharacterProfileDef {
                kind: CharacterKind::Bee,
                label: "Bee".to_string(),
                scene: "characters/kenney_cube_pets/animal-bee.glb".to_string(),
                move_set: "bee_cube".to_string(),
                body: bee_body_profile(),
            },
            CharacterProfileDef {
                kind: CharacterKind::Penguin,
                label: "Penguin".to_string(),
                scene: "characters/kenney_cube_pets/animal-penguin.glb".to_string(),
                move_set: "penguin_cube".to_string(),
                body: penguin_body_profile(),
            },
            CharacterProfileDef {
                kind: CharacterKind::Chick,
                label: "Chick".to_string(),
                scene: "characters/kenney_cube_pets/animal-chick.glb".to_string(),
                move_set: "chick_cube".to_string(),
                body: chick_body_profile(),
            },
        ],
        move_sets: vec![
            CharacterMoveSetDef {
                id: "cat_cube".to_string(),
                order: default_cat_technique_order().to_vec(),
                slots: default_cat_move_slots().to_vec(),
            },
            CharacterMoveSetDef {
                id: "pig_cube".to_string(),
                order: pig_technique_order().to_vec(),
                slots: character_move_slots(
                    TechniqueId::PigComboFinisher,
                    TechniqueId::PigHeavy,
                    TechniqueId::PigJumpAttack,
                    TechniqueId::PigJumpHeavy,
                    TechniqueId::PigUltimateStartup,
                    TechniqueId::PigUltimateRush,
                ),
            },
            CharacterMoveSetDef {
                id: "dog_cube".to_string(),
                order: dog_technique_order().to_vec(),
                slots: character_move_slots(
                    TechniqueId::DogComboFinisher,
                    TechniqueId::DogHeavy2,
                    TechniqueId::DogJumpAttack,
                    TechniqueId::DogJumpHeavy,
                    TechniqueId::DogUltimateStartup,
                    TechniqueId::DogUltimateRush,
                ),
            },
            CharacterMoveSetDef {
                id: "fox_cube".to_string(),
                order: fox_technique_order().to_vec(),
                slots: character_move_slots(
                    TechniqueId::FoxComboFinisher,
                    TechniqueId::FoxHeavy2,
                    TechniqueId::FoxJumpAttack,
                    TechniqueId::FoxJumpHeavy,
                    TechniqueId::FoxUltimateStartup,
                    TechniqueId::FoxUltimateRush,
                ),
            },
            CharacterMoveSetDef {
                id: "panda_cube".to_string(),
                order: panda_technique_order().to_vec(),
                slots: character_move_slots(
                    TechniqueId::PandaComboFinisher,
                    TechniqueId::PandaHeavy2,
                    TechniqueId::PandaJumpAttack,
                    TechniqueId::PandaJumpHeavy,
                    TechniqueId::PandaUltimateStartup,
                    TechniqueId::PandaUltimateRush,
                ),
            },
            CharacterMoveSetDef {
                id: "bee_cube".to_string(),
                order: bee_technique_order().to_vec(),
                slots: bee_move_slots().to_vec(),
            },
            CharacterMoveSetDef {
                id: "penguin_cube".to_string(),
                order: penguin_technique_order().to_vec(),
                slots: character_move_slots(
                    TechniqueId::PenguinDashAttack,
                    TechniqueId::PenguinDashHeavy,
                    TechniqueId::PenguinJumpAttack,
                    TechniqueId::PenguinJumpHeavy,
                    TechniqueId::PenguinUltimateStartup,
                    TechniqueId::PenguinUltimateRush,
                ),
            },
            CharacterMoveSetDef {
                id: "chick_cube".to_string(),
                order: chick_technique_order().to_vec(),
                slots: chick_move_slots().to_vec(),
            },
        ],
    }
}

pub fn pig_body_profile() -> CharacterBodyDef {
    CharacterBodyDef {
        ground_speed: 0.588,
        air_speed: 0.574,
        dash_impulse: 0.588,
        jump_impulse: 0.9,
        gravity: 1.04,
        fall_gravity: 1.13,
        stop_friction: 0.88,
        landing_stick: 1.22,
        dash_slide: 1.25,
        mesh_bounds: character_mesh_bounds(CharacterKind::Pig),
    }
}

pub fn bee_body_profile() -> CharacterBodyDef {
    CharacterBodyDef {
        ground_speed: 1.08,
        air_speed: 1.18,
        dash_impulse: 1.12,
        jump_impulse: 1.1,
        gravity: 0.96,
        fall_gravity: 1.02,
        stop_friction: 1.05,
        landing_stick: 0.92,
        dash_slide: 0.95,
        mesh_bounds: character_mesh_bounds(CharacterKind::Bee),
    }
}

pub fn penguin_body_profile() -> CharacterBodyDef {
    CharacterBodyDef {
        ground_speed: 0.95,
        air_speed: 0.88,
        dash_impulse: 1.08,
        jump_impulse: 0.96,
        gravity: 1.02,
        fall_gravity: 1.08,
        stop_friction: 0.72,
        landing_stick: 1.1,
        dash_slide: 1.35,
        mesh_bounds: character_mesh_bounds(CharacterKind::Penguin),
    }
}

pub fn chick_body_profile() -> CharacterBodyDef {
    CharacterBodyDef {
        ground_speed: 1.03,
        air_speed: 1.10,
        dash_impulse: 1.06,
        jump_impulse: 1.04,
        gravity: 0.98,
        fall_gravity: 1.04,
        mesh_bounds: character_mesh_bounds(CharacterKind::Chick),
        ..default()
    }
}

fn default_cat_technique_order() -> &'static [TechniqueId] {
    &[
        TechniqueId::CatLight2,
        TechniqueId::CatLight1,
        TechniqueId::CatHeavy2,
        TechniqueId::CatHeavy,
        TechniqueId::CatUltimateStartup,
        TechniqueId::CatUltimateRush,
        TechniqueId::Grab,
        TechniqueId::CatDashAttack,
        TechniqueId::CatJumpHeavy,
        TechniqueId::CatJumpAttack,
        TechniqueId::GuardCounter,
        TechniqueId::CatComboFinisher,
        TechniqueId::CatDashComboFinisher,
        TechniqueId::SpecialCast,
        TechniqueId::ItemPickup,
        TechniqueId::ItemSwing,
        TechniqueId::ItemThrow,
        TechniqueId::ItemDrop,
        TechniqueId::GuardStep,
        TechniqueId::QuickStand,
        TechniqueId::RecoveryRoll,
        TechniqueId::LandingRecovery,
    ]
}

fn default_cat_move_slots() -> &'static [CharacterMoveSlotDef] {
    &[
        CharacterMoveSlotDef {
            slot: CharacterMoveSlot::DashLight,
            technique: TechniqueId::CatDashComboFinisher,
        },
        CharacterMoveSlotDef {
            slot: CharacterMoveSlot::DashHeavy,
            technique: TechniqueId::CatHeavy2,
        },
        CharacterMoveSlotDef {
            slot: CharacterMoveSlot::JumpLight,
            technique: TechniqueId::CatJumpAttack,
        },
        CharacterMoveSlotDef {
            slot: CharacterMoveSlot::JumpHeavy,
            technique: TechniqueId::CatJumpHeavy,
        },
        CharacterMoveSlotDef {
            slot: CharacterMoveSlot::UltimateStartup,
            technique: TechniqueId::CatUltimateStartup,
        },
        CharacterMoveSlotDef {
            slot: CharacterMoveSlot::UltimateRush,
            technique: TechniqueId::CatUltimateRush,
        },
    ]
}

fn bee_move_slots() -> &'static [CharacterMoveSlotDef] {
    &[
        CharacterMoveSlotDef {
            slot: CharacterMoveSlot::DashLight,
            technique: TechniqueId::BeeLight1,
        },
        CharacterMoveSlotDef {
            slot: CharacterMoveSlot::DashHeavy,
            technique: TechniqueId::BeeHeavy2,
        },
        CharacterMoveSlotDef {
            slot: CharacterMoveSlot::JumpLight,
            technique: TechniqueId::BeeJumpAttack,
        },
        CharacterMoveSlotDef {
            slot: CharacterMoveSlot::JumpHeavy,
            technique: TechniqueId::BeeJumpHeavy,
        },
        CharacterMoveSlotDef {
            slot: CharacterMoveSlot::UltimateStartup,
            technique: TechniqueId::BeeUltimateStartup,
        },
    ]
}

fn dog_technique_order() -> &'static [TechniqueId] {
    &[
        TechniqueId::DogLight2,
        TechniqueId::DogLight1,
        TechniqueId::DogHeavy2,
        TechniqueId::DogHeavy,
        TechniqueId::DogUltimateStartup,
        TechniqueId::DogUltimateRush,
        TechniqueId::Grab,
        TechniqueId::DogDashAttack,
        TechniqueId::DogJumpHeavy,
        TechniqueId::DogJumpAttack,
        TechniqueId::GuardCounter,
        TechniqueId::DogComboFinisher,
        TechniqueId::SpecialCast,
        TechniqueId::ItemPickup,
        TechniqueId::ItemSwing,
        TechniqueId::ItemThrow,
        TechniqueId::ItemDrop,
        TechniqueId::GuardStep,
        TechniqueId::QuickStand,
        TechniqueId::RecoveryRoll,
        TechniqueId::LandingRecovery,
    ]
}

fn pig_technique_order() -> &'static [TechniqueId] {
    &[
        TechniqueId::PigLight2,
        TechniqueId::PigLight1,
        TechniqueId::PigHeavy,
        TechniqueId::PigUltimateStartup,
        TechniqueId::PigUltimateRush,
        TechniqueId::Grab,
        TechniqueId::PigDashAttack,
        TechniqueId::PigJumpHeavy,
        TechniqueId::PigJumpAttack,
        TechniqueId::GuardCounter,
        TechniqueId::PigComboFinisher,
        TechniqueId::SpecialCast,
        TechniqueId::ItemPickup,
        TechniqueId::ItemSwing,
        TechniqueId::ItemThrow,
        TechniqueId::ItemDrop,
        TechniqueId::GuardStep,
        TechniqueId::QuickStand,
        TechniqueId::RecoveryRoll,
        TechniqueId::LandingRecovery,
    ]
}

fn fox_technique_order() -> &'static [TechniqueId] {
    &[
        TechniqueId::FoxLight2,
        TechniqueId::FoxLight1,
        TechniqueId::FoxHeavy2,
        TechniqueId::FoxHeavy,
        TechniqueId::FoxUltimateStartup,
        TechniqueId::FoxUltimateRush,
        TechniqueId::Grab,
        TechniqueId::FoxDashAttack,
        TechniqueId::FoxJumpHeavy,
        TechniqueId::FoxJumpAttack,
        TechniqueId::GuardCounter,
        TechniqueId::FoxComboFinisher,
        TechniqueId::SpecialCast,
        TechniqueId::ItemPickup,
        TechniqueId::ItemSwing,
        TechniqueId::ItemThrow,
        TechniqueId::ItemDrop,
        TechniqueId::GuardStep,
        TechniqueId::QuickStand,
        TechniqueId::RecoveryRoll,
        TechniqueId::LandingRecovery,
    ]
}

fn panda_technique_order() -> &'static [TechniqueId] {
    &[
        TechniqueId::PandaLight2,
        TechniqueId::PandaLight1,
        TechniqueId::PandaHeavy2,
        TechniqueId::PandaHeavy,
        TechniqueId::PandaUltimateStartup,
        TechniqueId::PandaUltimateRush,
        TechniqueId::Grab,
        TechniqueId::PandaDashAttack,
        TechniqueId::PandaJumpHeavy,
        TechniqueId::PandaJumpAttack,
        TechniqueId::GuardCounter,
        TechniqueId::PandaComboFinisher,
        TechniqueId::SpecialCast,
        TechniqueId::ItemPickup,
        TechniqueId::ItemSwing,
        TechniqueId::ItemThrow,
        TechniqueId::ItemDrop,
        TechniqueId::GuardStep,
        TechniqueId::QuickStand,
        TechniqueId::RecoveryRoll,
        TechniqueId::LandingRecovery,
    ]
}

fn bee_technique_order() -> &'static [TechniqueId] {
    &[
        TechniqueId::BeeLight1,
        TechniqueId::BeeHeavy2,
        TechniqueId::BeeUltimateStartup,
        TechniqueId::Grab,
        TechniqueId::BeeDashAttack,
        TechniqueId::BeeJumpHeavy,
        TechniqueId::BeeJumpAttack,
        TechniqueId::GuardCounter,
        TechniqueId::SpecialCast,
        TechniqueId::ItemPickup,
        TechniqueId::ItemSwing,
        TechniqueId::ItemThrow,
        TechniqueId::ItemDrop,
        TechniqueId::GuardStep,
        TechniqueId::QuickStand,
        TechniqueId::RecoveryRoll,
        TechniqueId::LandingRecovery,
    ]
}

fn penguin_technique_order() -> &'static [TechniqueId] {
    &[
        TechniqueId::PenguinLight2,
        TechniqueId::PenguinLight1,
        TechniqueId::PenguinHeavy2,
        TechniqueId::PenguinHeavy,
        TechniqueId::PenguinUltimateStartup,
        TechniqueId::PenguinUltimateRush,
        TechniqueId::Grab,
        TechniqueId::PenguinDashAttack,
        TechniqueId::PenguinDashHeavy,
        TechniqueId::PenguinJumpHeavy,
        TechniqueId::PenguinJumpAttack,
        TechniqueId::GuardCounter,
        TechniqueId::PenguinComboFinisher,
        TechniqueId::SpecialCast,
        TechniqueId::ItemPickup,
        TechniqueId::ItemSwing,
        TechniqueId::ItemThrow,
        TechniqueId::ItemDrop,
        TechniqueId::GuardStep,
        TechniqueId::QuickStand,
        TechniqueId::RecoveryRoll,
        TechniqueId::LandingRecovery,
    ]
}

fn chick_technique_order() -> &'static [TechniqueId] {
    &[
        TechniqueId::ChickLight2,
        TechniqueId::ChickLight1,
        TechniqueId::ChickHeavy2,
        TechniqueId::ChickHeavy,
        TechniqueId::ChickUltimateStartup,
        TechniqueId::Grab,
        TechniqueId::ChickDashAttack,
        TechniqueId::ChickDashHeavy,
        TechniqueId::ChickJumpHeavy,
        TechniqueId::ChickJumpAttack,
        TechniqueId::GuardCounter,
        TechniqueId::ChickComboFinisher,
        TechniqueId::SpecialCast,
        TechniqueId::ItemPickup,
        TechniqueId::ItemSwing,
        TechniqueId::ItemThrow,
        TechniqueId::ItemDrop,
        TechniqueId::GuardStep,
        TechniqueId::QuickStand,
        TechniqueId::RecoveryRoll,
        TechniqueId::LandingRecovery,
    ]
}

fn chick_move_slots() -> &'static [CharacterMoveSlotDef] {
    &[
        CharacterMoveSlotDef {
            slot: CharacterMoveSlot::DashLight,
            technique: TechniqueId::ChickDashAttack,
        },
        CharacterMoveSlotDef {
            slot: CharacterMoveSlot::DashHeavy,
            technique: TechniqueId::ChickDashHeavy,
        },
        CharacterMoveSlotDef {
            slot: CharacterMoveSlot::JumpLight,
            technique: TechniqueId::ChickJumpAttack,
        },
        CharacterMoveSlotDef {
            slot: CharacterMoveSlot::JumpHeavy,
            technique: TechniqueId::ChickJumpHeavy,
        },
        CharacterMoveSlotDef {
            slot: CharacterMoveSlot::UltimateStartup,
            technique: TechniqueId::ChickUltimateStartup,
        },
    ]
}

fn character_move_slots(
    dash_light: TechniqueId,
    dash_heavy: TechniqueId,
    jump_light: TechniqueId,
    jump_heavy: TechniqueId,
    ultimate_startup: TechniqueId,
    ultimate_rush: TechniqueId,
) -> Vec<CharacterMoveSlotDef> {
    vec![
        CharacterMoveSlotDef {
            slot: CharacterMoveSlot::DashLight,
            technique: dash_light,
        },
        CharacterMoveSlotDef {
            slot: CharacterMoveSlot::DashHeavy,
            technique: dash_heavy,
        },
        CharacterMoveSlotDef {
            slot: CharacterMoveSlot::JumpLight,
            technique: jump_light,
        },
        CharacterMoveSlotDef {
            slot: CharacterMoveSlot::JumpHeavy,
            technique: jump_heavy,
        },
        CharacterMoveSlotDef {
            slot: CharacterMoveSlot::UltimateStartup,
            technique: ultimate_startup,
        },
        CharacterMoveSlotDef {
            slot: CharacterMoveSlot::UltimateRush,
            technique: ultimate_rush,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_gameplay_catalog_is_valid_and_has_no_reload_state() {
        let catalog = CharacterMoveCatalog::from_embedded_gameplay().unwrap();
        let parsed = parse_character_move_catalog(EMBEDDED_CHARACTER_MOVE_CATALOG).unwrap();

        assert_eq!(catalog.authored_file_for_test(), &parsed);
        assert_eq!(
            catalog.path.as_path(),
            Path::new(CHARACTER_MOVE_CATALOG_PATH)
        );
        assert_eq!(catalog.modified, None);
        assert_eq!(catalog.last_error(), None);
    }

    #[cfg(not(all(
        feature = "dev-hot-reload",
        not(feature = "shipping"),
        not(target_arch = "wasm32")
    )))]
    #[test]
    fn immutable_catalog_ignores_a_hostile_loose_file() {
        let path = std::env::temp_dir().join(format!(
            "afc-character-catalog-{}-{}.ron",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, "this loose catalog must never be parsed").unwrap();

        let catalog = initial_character_move_catalog(&path);
        let embedded = CharacterMoveCatalog::from_embedded_gameplay().unwrap();

        assert_eq!(
            catalog.authored_file_for_test(),
            embedded.authored_file_for_test()
        );
        assert_eq!(catalog.modified, None);
        assert_eq!(catalog.last_error(), None);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn embedded_gameplay_catalog_validation_rejects_incomplete_authorship() {
        let error = parse_character_move_catalog("(characters: [], move_sets: [])").unwrap_err();
        assert!(error.contains("missing character profile"));
    }

    #[test]
    fn character_catalog_validation_rejects_ambiguous_and_non_finite_authorship() {
        let mut duplicate = parse_character_move_catalog(EMBEDDED_CHARACTER_MOVE_CATALOG).unwrap();
        duplicate.characters[1].kind = duplicate.characters[0].kind;
        assert!(
            validate_catalog(&duplicate)
                .unwrap_err()
                .contains("duplicate character profile")
        );

        let mut non_finite = parse_character_move_catalog(EMBEDDED_CHARACTER_MOVE_CATALOG).unwrap();
        non_finite.characters[0].body.ground_speed = f32::NAN;
        assert!(
            validate_catalog(&non_finite)
                .unwrap_err()
                .contains("non-finite body value")
        );
    }

    #[test]
    fn committed_character_move_catalog_parses() {
        let path = Path::new(CHARACTER_MOVE_CATALOG_PATH);
        let (file, _) = load_character_move_catalog_file(path).unwrap();
        let catalog = CharacterMoveCatalog::from_file(file);

        for kind in CHARACTER_KINDS {
            assert!(catalog.profile(kind).is_some());
            assert!(!catalog.ordered_techniques(kind).is_empty());
            assert!(
                catalog
                    .slot_technique(kind, CharacterMoveSlot::DashLight)
                    .is_some()
            );
        }
        assert_eq!(
            catalog.slot_technique(CharacterKind::Cat, CharacterMoveSlot::DashLight),
            Some(TechniqueId::CatDashComboFinisher)
        );
        assert_eq!(
            catalog.slot_technique(CharacterKind::Dog, CharacterMoveSlot::DashLight),
            Some(TechniqueId::DogComboFinisher)
        );
        assert_eq!(
            catalog.slot_technique(CharacterKind::Pig, CharacterMoveSlot::DashLight),
            Some(TechniqueId::PigComboFinisher)
        );
        assert_eq!(
            catalog.slot_technique(CharacterKind::Pig, CharacterMoveSlot::DashHeavy),
            Some(TechniqueId::PigHeavy)
        );
        assert_eq!(
            catalog.slot_technique(CharacterKind::Fox, CharacterMoveSlot::DashHeavy),
            Some(TechniqueId::FoxHeavy2)
        );
        assert_eq!(
            catalog.slot_technique(CharacterKind::Panda, CharacterMoveSlot::DashHeavy),
            Some(TechniqueId::PandaHeavy2)
        );
        assert_eq!(
            catalog.slot_technique(CharacterKind::Bee, CharacterMoveSlot::DashLight),
            Some(TechniqueId::BeeLight1)
        );
        assert_eq!(
            catalog.slot_technique(CharacterKind::Bee, CharacterMoveSlot::DashHeavy),
            Some(TechniqueId::BeeHeavy2)
        );
        assert_eq!(
            catalog.slot_technique(CharacterKind::Penguin, CharacterMoveSlot::DashLight),
            Some(TechniqueId::PenguinDashAttack)
        );
        assert_eq!(
            catalog.slot_technique(CharacterKind::Penguin, CharacterMoveSlot::DashHeavy),
            Some(TechniqueId::PenguinDashHeavy)
        );
        assert_eq!(
            catalog.slot_technique(CharacterKind::Chick, CharacterMoveSlot::DashLight),
            Some(TechniqueId::ChickDashAttack)
        );
        assert_eq!(
            catalog.slot_technique(CharacterKind::Chick, CharacterMoveSlot::DashHeavy),
            Some(TechniqueId::ChickDashHeavy)
        );
        assert_eq!(
            catalog.slot_technique(CharacterKind::Chick, CharacterMoveSlot::UltimateRush),
            None
        );
        assert_eq!(
            catalog.scene_path(CharacterKind::Bee),
            Some("characters/kenney_cube_pets/animal-bee.glb")
        );
        assert_eq!(
            catalog.scene_path(CharacterKind::Penguin),
            Some("characters/kenney_cube_pets/animal-penguin.glb")
        );
        assert_eq!(
            catalog.scene_path(CharacterKind::Chick),
            Some("characters/kenney_cube_pets/animal-chick.glb")
        );
        assert_eq!(character_label(CharacterKind::Chick), "Chick");
    }

    #[test]
    fn missing_move_set_entries_do_not_fall_back_to_cat() {
        let catalog = CharacterMoveCatalog::from_file(CharacterMoveCatalogFile {
            characters: vec![
                CharacterProfileDef {
                    kind: CharacterKind::Cat,
                    label: "Cat".to_string(),
                    scene: "characters/kenney_cube_pets/animal-cat.glb".to_string(),
                    move_set: "cat_cube".to_string(),
                    body: CharacterBodyDef::default(),
                },
                CharacterProfileDef {
                    kind: CharacterKind::Pig,
                    label: "Pig".to_string(),
                    scene: "characters/kenney_cube_pets/animal-pig.glb".to_string(),
                    move_set: "cat_cube".to_string(),
                    body: pig_body_profile(),
                },
                CharacterProfileDef {
                    kind: CharacterKind::Dog,
                    label: "Dog".to_string(),
                    scene: "characters/kenney_cube_pets/animal-dog.glb".to_string(),
                    move_set: "dog_empty".to_string(),
                    body: CharacterBodyDef::default(),
                },
                CharacterProfileDef {
                    kind: CharacterKind::Fox,
                    label: "Fox".to_string(),
                    scene: "characters/kenney_cube_pets/animal-fox.glb".to_string(),
                    move_set: "cat_cube".to_string(),
                    body: CharacterBodyDef::default(),
                },
                CharacterProfileDef {
                    kind: CharacterKind::Panda,
                    label: "Panda".to_string(),
                    scene: "characters/kenney_cube_pets/animal-panda.glb".to_string(),
                    move_set: "cat_cube".to_string(),
                    body: CharacterBodyDef::default(),
                },
                CharacterProfileDef {
                    kind: CharacterKind::Bee,
                    label: "Bee".to_string(),
                    scene: "characters/kenney_cube_pets/animal-bee.glb".to_string(),
                    move_set: "cat_cube".to_string(),
                    body: bee_body_profile(),
                },
                CharacterProfileDef {
                    kind: CharacterKind::Penguin,
                    label: "Penguin".to_string(),
                    scene: "characters/kenney_cube_pets/animal-penguin.glb".to_string(),
                    move_set: "cat_cube".to_string(),
                    body: penguin_body_profile(),
                },
                CharacterProfileDef {
                    kind: CharacterKind::Chick,
                    label: "Chick".to_string(),
                    scene: "characters/kenney_cube_pets/animal-chick.glb".to_string(),
                    move_set: "cat_cube".to_string(),
                    body: chick_body_profile(),
                },
            ],
            move_sets: vec![
                CharacterMoveSetDef {
                    id: "cat_cube".to_string(),
                    order: default_cat_technique_order().to_vec(),
                    slots: default_cat_move_slots().to_vec(),
                },
                CharacterMoveSetDef {
                    id: "dog_empty".to_string(),
                    order: vec![TechniqueId::GuardStep],
                    slots: Vec::new(),
                },
            ],
        });

        assert_eq!(
            catalog.slot_technique(CharacterKind::Dog, CharacterMoveSlot::DashLight),
            None
        );
        assert_eq!(
            catalog.slot_technique(CharacterKind::Pig, CharacterMoveSlot::DashLight),
            None
        );
        assert!(!catalog.allows_technique(CharacterKind::Dog, TechniqueId::CatLight1));
        assert!(!catalog.allows_technique(CharacterKind::Pig, TechniqueId::CatLight1));
        assert!(!catalog.allows_technique(CharacterKind::Chick, TechniqueId::CatLight1));
        assert!(catalog.allows_technique(CharacterKind::Cat, TechniqueId::CatLight1));
    }

    #[test]
    fn chick_is_selectable_without_replacing_default_starting_fighters() {
        assert!(CHARACTER_KINDS.contains(&CharacterKind::Chick));
        assert_eq!(
            DEFAULT_FIGHTER_CHARACTERS,
            [
                CharacterKind::Cat,
                CharacterKind::Pig,
                CharacterKind::Fox,
                CharacterKind::Panda
            ]
        );
    }

    #[test]
    fn character_catalog_can_author_body_feel_per_character() {
        let catalog = CharacterMoveCatalog::default();
        let cat = catalog.body(CharacterKind::Cat);
        let pig = catalog.body(CharacterKind::Pig);
        let bee = catalog.body(CharacterKind::Bee);
        let penguin = catalog.body(CharacterKind::Penguin);
        let chick = catalog.body(CharacterKind::Chick);

        assert_eq!(cat, CharacterBodyDef::default());
        assert!(pig.ground_speed < cat.ground_speed);
        assert!(pig.jump_impulse < cat.jump_impulse);
        assert!(pig.fall_gravity > cat.fall_gravity);
        assert!(pig.landing_stick > cat.landing_stick);
        assert!(pig.dash_slide > cat.dash_slide);
        assert!(bee.ground_speed > cat.ground_speed);
        assert!(bee.air_speed > cat.air_speed);
        assert!(bee.jump_impulse > cat.jump_impulse);
        assert!(bee.gravity < cat.gravity);
        assert!(penguin.dash_impulse > cat.dash_impulse);
        assert!(penguin.stop_friction < cat.stop_friction);
        assert!(penguin.dash_slide > cat.dash_slide);
        assert!(chick.ground_speed > cat.ground_speed);
        assert!(chick.air_speed > cat.air_speed);
        assert!(chick.gravity < cat.gravity);
        assert!(chick.fall_gravity > cat.fall_gravity);
    }

    #[test]
    fn character_catalog_carries_cube_pet_mesh_bounds() {
        let catalog = CharacterMoveCatalog::default();
        let cat = catalog.body(CharacterKind::Cat).mesh_bounds;
        let fox = catalog.body(CharacterKind::Fox).mesh_bounds;
        let panda = catalog.body(CharacterKind::Panda).mesh_bounds;
        let bee = catalog.body(CharacterKind::Bee).mesh_bounds;
        let penguin = catalog.body(CharacterKind::Penguin).mesh_bounds;
        let chick = catalog.body(CharacterKind::Chick).mesh_bounds;

        assert!(fox.max[2] - fox.min[2] > cat.max[2] - cat.min[2]);
        assert!(panda.max[0] - panda.min[0] > cat.max[0] - cat.min[0]);
        assert!(bee.max[2] - bee.min[2] > cat.max[2] - cat.min[2]);
        assert!(penguin.max[2] - penguin.min[2] > cat.max[2] - cat.min[2]);
        assert_eq!(chick.min, [-0.625, -0.3, -0.625]);
        assert_eq!(chick.max, [0.625, 1.413, 0.725]);
    }
}
