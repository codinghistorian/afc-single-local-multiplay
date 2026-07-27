use std::collections::BTreeSet;

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy::time::Real;
use bevy::ui::UiTargetCamera;
use serde::{Deserialize, Serialize};

use crate::arena_defs::{active_arena_definition, set_active_arena_index};
use crate::bee_skills::ActiveBeeSkill;
use crate::bot::{BotDifficulty, start_bot_combat_ai};
use crate::camera::{CameraActionEffects, ScreenLook, ScreenLookTransition, UiCamera};
use crate::characters::CharacterKind;
#[cfg(test)]
use crate::characters::CharacterMoveCatalog;
use crate::chick_skills::ActiveChickSkill;
use crate::combat::HitEffects;
use crate::components::{
    BotBehaviorMode, BotBrain, ControlAction, Controller, DrunkStatus, Fighter, FighterAction,
    FighterActionState, FighterGrabState, FighterInput, FighterInventory, FighterMotor,
    FighterSpecialState, FighterStats, Hitbox, LocalInputAssignment, PlayerKeyBindings,
};
use crate::constants::{ARENA_TOP_Y, FIGHTER_COUNT, MAX_HEALTH, STOCK_LIVES};
use crate::control_settings::{ControllerDeviceInfo, ControllerFamily, controller_info};
use crate::effects::{EffectKind, VisualEffect};
use crate::equipment::FighterEquipment;
use crate::game_state::{
    GameplayPauseOwner, GameplayPauseOwners, Hitstop, LocalSetup, MatchAnnouncements, MatchPhase,
    MatchState, MatchTelemetry,
};
use crate::items::{ArenaItem, ItemAssets, ItemKind, ItemState, item_scale};
use crate::penguin_skills::{ActivePenguinSkill, ActivePenguinSurface};
use crate::specials::{ActiveSpecial, SpecialKind};
use crate::techniques::TechniqueId;
use crate::user_mode::{
    UserModeMusic, UserModeScreen, UserModeState, start_user_mode_menu_music, stop_user_mode_music,
};

const TUTORIAL_PROGRESS_VERSION: u32 = 1;
#[cfg(target_arch = "wasm32")]
const TUTORIAL_PROGRESS_STORAGE_KEY: &str = "animal-fighter-club.tutorial.v1";

pub const TUTORIAL_PLAYER_ID: usize = 0;
pub const TUTORIAL_DUMMY_ID: usize = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TutorialChapterId {
    Basics,
    Combat,
    DefenseRecovery,
    Items,
    SharedSpecials,
    HudWinning,
    CatLab,
    PigLab,
    BeeLab,
    PenguinLab,
    ChickLab,
    FinalExam,
}

impl TutorialChapterId {
    pub const ALL: [Self; 12] = [
        Self::Basics,
        Self::Combat,
        Self::DefenseRecovery,
        Self::Items,
        Self::SharedSpecials,
        Self::HudWinning,
        Self::CatLab,
        Self::PigLab,
        Self::BeeLab,
        Self::PenguinLab,
        Self::ChickLab,
        Self::FinalExam,
    ];

    pub const fn stable_id(self) -> &'static str {
        match self {
            Self::Basics => "basics",
            Self::Combat => "combat",
            Self::DefenseRecovery => "defense_recovery",
            Self::Items => "items",
            Self::SharedSpecials => "shared_specials",
            Self::HudWinning => "hud_winning",
            Self::CatLab => "cat_lab",
            Self::PigLab => "pig_lab",
            Self::BeeLab => "bee_lab",
            Self::PenguinLab => "penguin_lab",
            Self::ChickLab => "chick_lab",
            Self::FinalExam => "final_exam",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TutorialChapterStatus {
    New,
    Visited,
    Complete,
}

impl TutorialChapterStatus {
    pub const fn label(self) -> &'static str {
        match self {
            Self::New => "NEW",
            Self::Visited => "VISITED",
            Self::Complete => "COMPLETE",
        }
    }
}

#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct TutorialProgress {
    visited: BTreeSet<TutorialChapterId>,
    completed: BTreeSet<TutorialChapterId>,
}

impl TutorialProgress {
    pub fn status(&self, chapter: TutorialChapterId) -> TutorialChapterStatus {
        if self.is_complete(chapter) {
            TutorialChapterStatus::Complete
        } else if self.is_visited(chapter) {
            TutorialChapterStatus::Visited
        } else {
            TutorialChapterStatus::New
        }
    }

    pub fn is_visited(&self, chapter: TutorialChapterId) -> bool {
        self.visited.contains(&chapter)
    }

    pub fn is_complete(&self, chapter: TutorialChapterId) -> bool {
        self.completed.contains(&chapter)
    }

    pub fn mark_visited(&mut self, chapter: TutorialChapterId) {
        self.visited.insert(chapter);
    }

    pub fn mark_complete(&mut self, chapter: TutorialChapterId) {
        self.visited.insert(chapter);
        self.completed.insert(chapter);
    }

    pub fn reset(&mut self) {
        self.visited.clear();
        self.completed.clear();
    }

    pub fn visited_ids(&self) -> impl Iterator<Item = TutorialChapterId> + '_ {
        self.visited.iter().copied()
    }

    pub fn completed_ids(&self) -> impl Iterator<Item = TutorialChapterId> + '_ {
        self.completed.iter().copied()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct StoredTutorialProgress {
    version: u32,
    visited: BTreeSet<TutorialChapterId>,
    completed: BTreeSet<TutorialChapterId>,
}

fn encode_tutorial_progress(progress: &TutorialProgress) -> Result<String, String> {
    let stored = StoredTutorialProgress {
        version: TUTORIAL_PROGRESS_VERSION,
        visited: progress.visited_ids().collect(),
        completed: progress.completed_ids().collect(),
    };
    ron::ser::to_string_pretty(&stored, ron::ser::PrettyConfig::new())
        .map_err(|error| format!("could not encode tutorial progress: {error}"))
}

fn decode_tutorial_progress(contents: &str) -> Result<TutorialProgress, String> {
    let stored: StoredTutorialProgress = ron::from_str(contents)
        .map_err(|error| format!("could not parse tutorial progress: {error}"))?;
    if stored.version != TUTORIAL_PROGRESS_VERSION {
        return Err(format!(
            "unsupported tutorial progress version {}",
            stored.version
        ));
    }
    if !stored.completed.is_subset(&stored.visited) {
        return Err("completed tutorial chapters must also be visited".to_string());
    }
    Ok(TutorialProgress {
        visited: stored.visited,
        completed: stored.completed,
    })
}

pub fn load_tutorial_progress(mut progress: ResMut<TutorialProgress>) {
    let Some(contents) = load_tutorial_progress_contents() else {
        return;
    };
    match decode_tutorial_progress(&contents) {
        Ok(saved) => *progress = saved,
        Err(error) => warn!("Ignoring saved tutorial progress: {error}"),
    }
}

pub fn save_tutorial_progress(progress: &TutorialProgress) -> Result<(), String> {
    let contents = encode_tutorial_progress(progress)?;
    save_tutorial_progress_contents(&contents)
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn tutorial_progress_path() -> Option<std::path::PathBuf> {
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
            .join("tutorial-progress.ron")
    })
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn load_tutorial_progress_contents() -> Option<String> {
    let path = tutorial_progress_path()?;
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
fn save_tutorial_progress_contents(contents: &str) -> Result<(), String> {
    use std::io::Write;

    let path = tutorial_progress_path()
        .ok_or_else(|| "platform tutorial-progress directory is unavailable".to_string())?;
    let parent = path
        .parent()
        .ok_or_else(|| "tutorial-progress path has no parent".to_string())?;
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
fn tutorial_browser_storage() -> Result<web_sys::Storage, String> {
    web_sys::window()
        .ok_or_else(|| "browser window is unavailable".to_string())?
        .local_storage()
        .map_err(|error| format!("localStorage access failed: {error:?}"))?
        .ok_or_else(|| "localStorage is unavailable".to_string())
}

#[cfg(target_arch = "wasm32")]
fn load_tutorial_progress_contents() -> Option<String> {
    match tutorial_browser_storage().and_then(|storage| {
        storage
            .get_item(TUTORIAL_PROGRESS_STORAGE_KEY)
            .map_err(|error| format!("localStorage read failed: {error:?}"))
    }) {
        Ok(contents) => contents,
        Err(error) => {
            warn!("Could not load tutorial progress: {error}");
            None
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn save_tutorial_progress_contents(contents: &str) -> Result<(), String> {
    tutorial_browser_storage()?
        .set_item(TUTORIAL_PROGRESS_STORAGE_KEY, contents)
        .map_err(|error| format!("localStorage write failed: {error:?}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TutorialDirection {
    Left,
    Right,
    Forward,
    Back,
    Any,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TutorialObjective {
    Knowledge,
    Movement {
        direction: TutorialDirection,
        distance: f32,
    },
    Input(ControlAction),
    Action(FighterAction),
    ActivateTechnique {
        technique: TechniqueId,
        confirmed_hit: bool,
    },
    ConfirmedHits {
        count: u8,
    },
    Guarding {
        count: u8,
    },
    GrabEscape,
    Recovery(TechniqueId),
    ItemUse {
        kind: ItemKind,
        uses: u8,
    },
    ItemThrow(ItemKind),
    SpecialSpawn(SpecialKind),
    RingOutOpponent,
    LoseLife,
    MatchResult {
        win: bool,
    },
}

impl TutorialObjective {
    pub fn target_count(self) -> u32 {
        match self {
            Self::ConfirmedHits { count }
            | Self::Guarding { count }
            | Self::ItemUse { uses: count, .. } => count as u32,
            _ => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScriptedDummyMode {
    Passive,
    Positioning,
    Guarding,
    TimedLightAttacks,
    TimedHeavyAttacks,
    Grabs,
    KnockdownSetup,
    NormalBot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TutorialPromptAction {
    Move,
    Aim,
    Light,
    Heavy,
    Jump,
    Special,
    Dash,
    Guard,
    Ultimate,
    Menu,
    Confirm,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TutorialStep {
    pub title: &'static str,
    pub instruction: &'static str,
    pub objective: TutorialObjective,
    pub controls: &'static [TutorialPromptAction],
    pub hint: &'static str,
    pub strong_hint: &'static str,
    pub dummy: ScriptedDummyMode,
}

const fn lesson_step(
    title: &'static str,
    instruction: &'static str,
    objective: TutorialObjective,
    controls: &'static [TutorialPromptAction],
    hint: &'static str,
    strong_hint: &'static str,
    dummy: ScriptedDummyMode,
) -> TutorialStep {
    TutorialStep {
        title,
        instruction,
        objective,
        controls,
        hint,
        strong_hint,
        dummy,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TutorialChapter {
    pub id: TutorialChapterId,
    pub number: usize,
    pub title: &'static str,
    pub summary: &'static str,
    pub player_character: CharacterKind,
    pub steps: &'static [TutorialStep],
    pub final_exam: bool,
}

const BASICS_STEPS: &[TutorialStep] = &[
    lesson_step(
        "Your control layout",
        "Prompts always follow the keyboard layout or controller assigned to Player 1. Menu pauses any lesson.",
        TutorialObjective::Knowledge,
        &[TutorialPromptAction::Menu],
        "Confirm when you have found each movement and action control.",
        "The compact prompt at the bottom updates whenever P1's device changes.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Move left",
        "Move across the ring to the left. Movement is relative to the gameplay camera.",
        TutorialObjective::Movement {
            direction: TutorialDirection::Left,
            distance: 2.2,
        },
        &[TutorialPromptAction::Move],
        "Hold left until the progress bar fills.",
        "Use P1's Left binding or push the stick/D-pad left.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Move right",
        "Return across the ring to the right.",
        TutorialObjective::Movement {
            direction: TutorialDirection::Right,
            distance: 2.2,
        },
        &[TutorialPromptAction::Move],
        "Hold right until the progress bar fills.",
        "Use P1's Right binding or push the stick/D-pad right.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Camera-relative depth",
        "Move forward and back. The same inputs remain screen-relative as the camera turns.",
        TutorialObjective::Movement {
            direction: TutorialDirection::Forward,
            distance: 2.0,
        },
        &[TutorialPromptAction::Move],
        "Move toward the top of the screen.",
        "Use P1's Up binding; the game converts it through the current camera yaw.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Move back",
        "Move toward the bottom of the screen to verify all four camera-relative directions.",
        TutorialObjective::Movement {
            direction: TutorialDirection::Back,
            distance: 2.0,
        },
        &[TutorialPromptAction::Move],
        "Move toward the bottom of the screen.",
        "Use P1's Down binding; movement remains relative to the gameplay camera.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Jump",
        "Jump clears low attacks and begins your aerial routes.",
        TutorialObjective::Action(FighterAction::Jumping),
        &[TutorialPromptAction::Jump],
        "Press Jump once.",
        "Wait until Cat is standing, then press the displayed Jump control.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Dash",
        "Dash for a burst of movement. Keyboard dashes use a directional double-tap.",
        TutorialObjective::Action(FighterAction::Dashing),
        &[TutorialPromptAction::Dash],
        "Dash in any safe direction.",
        "Keyboard: tap one movement direction twice quickly. Controller: flick the stick or use Dash.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Aim and face",
        "Hold Aim while moving to turn without committing to an attack.",
        TutorialObjective::Input(ControlAction::AimGrab),
        &[TutorialPromptAction::Aim, TutorialPromptAction::Move],
        "Hold Aim and nudge a movement direction.",
        "Keep the displayed Aim control held for a moment.",
        ScriptedDummyMode::Positioning,
    ),
];

const COMBAT_STEPS: &[TutorialStep] = &[
    lesson_step(
        "Confirm your hits",
        "Land two separate attacks. Only real contact advances this objective.",
        TutorialObjective::ConfirmedHits { count: 2 },
        &[TutorialPromptAction::Light],
        "Stand close enough for each Light attack to connect.",
        "Wait for Cat to recover between presses so two distinct hit confirms are observed.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Light chain",
        "Land Cat's three-hit grounded Light chain on Pig.",
        TutorialObjective::ActivateTechnique {
            technique: TechniqueId::CatComboFinisher,
            confirmed_hit: true,
        },
        &[TutorialPromptAction::Light],
        "Press Light again as each hit connects.",
        "Stand close and rhythmically press Light three times; do not mash before the branch window.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Heavy route",
        "Land the Heavy follow-up launcher.",
        TutorialObjective::ActivateTechnique {
            technique: TechniqueId::CatHeavy2,
            confirmed_hit: true,
        },
        &[TutorialPromptAction::Heavy],
        "Press Heavy, then Heavy again.",
        "Stay close to Pig and press the second Heavy during the first move's follow-up window.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Dash attack",
        "Dash toward Pig and finish with Light.",
        TutorialObjective::ActivateTechnique {
            technique: TechniqueId::CatDashComboFinisher,
            confirmed_hit: true,
        },
        &[TutorialPromptAction::Dash, TutorialPromptAction::Light],
        "Begin the attack while the dash is still active.",
        "Dash directly toward Pig, then press Light before Cat stops sliding.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Aerial Light",
        "Jump and connect Cat's diving Light.",
        TutorialObjective::ActivateTechnique {
            technique: TechniqueId::CatJumpAttack,
            confirmed_hit: true,
        },
        &[TutorialPromptAction::Jump, TutorialPromptAction::Light],
        "Press Light after leaving the floor.",
        "Jump toward Pig and press Light near the top of the jump.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Aerial Heavy",
        "Jump and connect the falling fish Heavy.",
        TutorialObjective::ActivateTechnique {
            technique: TechniqueId::CatJumpHeavy,
            confirmed_hit: true,
        },
        &[TutorialPromptAction::Jump, TutorialPromptAction::Heavy],
        "Press Heavy after leaving the floor.",
        "Jump toward Pig and press Heavy with enough horizontal room for the fish arc.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Grab and throw",
        "Grab Pig, choose a direction, and throw.",
        TutorialObjective::Action(FighterAction::Throwing),
        &[TutorialPromptAction::Aim, TutorialPromptAction::Move],
        "Tap Aim near Pig, then choose a throw direction.",
        "Walk into grab range, tap Aim/Grab, then press a movement direction.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "MP",
        "Heavy techniques, shared specials, and ultimates consume MP. White Wine and Barrel restore it.",
        TutorialObjective::Knowledge,
        &[],
        "Watch the blue MP meter beneath HP.",
        "Confirm to refill MP and continue.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Cat ultimate",
        "With at least half MP, start Cat's ultimate and catch Pig.",
        TutorialObjective::ActivateTechnique {
            technique: TechniqueId::CatUltimateRush,
            confirmed_hit: true,
        },
        &[TutorialPromptAction::Ultimate],
        "Use the Ultimate input while facing Pig.",
        "Stand a short distance away. Keyboard Ultimate is Aim + Light + Heavy; keep the target in front.",
        ScriptedDummyMode::Passive,
    ),
];

const DEFENSE_STEPS: &[TutorialStep] = &[
    lesson_step(
        "Pressure a guard",
        "Complete Cat's Heavy route while Pig guards to see how defense changes contact.",
        TutorialObjective::ActivateTechnique {
            technique: TechniqueId::CatHeavy2,
            confirmed_hit: false,
        },
        &[TutorialPromptAction::Heavy],
        "Press Heavy again during the follow-up window.",
        "The technique must complete even when Pig's guard prevents an ordinary hit confirm.",
        ScriptedDummyMode::Guarding,
    ),
    lesson_step(
        "Guard",
        "Block one scripted Light attack while facing Pig.",
        TutorialObjective::Guarding { count: 1 },
        &[TutorialPromptAction::Guard],
        "Hold Guard before Pig swings.",
        "Face Pig and keep Guard held through the hit. Keyboard Guard is Light + Heavy.",
        ScriptedDummyMode::TimedLightAttacks,
    ),
    lesson_step(
        "Perfect guard counter",
        "Start Guard just before impact, then answer with Light.",
        TutorialObjective::Recovery(TechniqueId::GuardCounter),
        &[TutorialPromptAction::Guard, TutorialPromptAction::Light],
        "Release and re-press Guard close to the hit, then press Light.",
        "Watch Pig's wind-up. Guard at the last moment; press Light while the counter window is glowing.",
        ScriptedDummyMode::TimedLightAttacks,
    ),
    lesson_step(
        "Guard step",
        "While guarding, dash sideways to evade and reposition.",
        TutorialObjective::Recovery(TechniqueId::GuardStep),
        &[TutorialPromptAction::Guard, TutorialPromptAction::Dash],
        "Hold Guard, choose a direction, then Dash.",
        "Keep Guard held as you double-tap a direction or press the controller Dash control.",
        ScriptedDummyMode::TimedHeavyAttacks,
    ),
    lesson_step(
        "Brace and escape",
        "Brace with Guard and move away from Pig to break the grab before the throw.",
        TutorialObjective::GrabEscape,
        &[TutorialPromptAction::Guard, TutorialPromptAction::Move],
        "Let the grab connect, then hold Guard while moving directly away.",
        "Stand close. Once grabbed, keep Guard held and push away from Pig until Cat escapes.",
        ScriptedDummyMode::Grabs,
    ),
    lesson_step(
        "Quick stand",
        "After knockdown, press Jump as the recovery window opens.",
        TutorialObjective::Recovery(TechniqueId::QuickStand),
        &[TutorialPromptAction::Jump],
        "Wait for the knockdown, then press Jump.",
        "Do not press early. Use Jump once Cat is flat on the floor.",
        ScriptedDummyMode::KnockdownSetup,
    ),
    lesson_step(
        "Recovery roll",
        "After knockdown, choose a direction and Dash to roll.",
        TutorialObjective::Recovery(TechniqueId::RecoveryRoll),
        &[TutorialPromptAction::Move, TutorialPromptAction::Dash],
        "Wait for the knockdown, then Dash with a direction.",
        "As Cat lies down, hold away from Pig and use Dash.",
        ScriptedDummyMode::KnockdownSetup,
    ),
];

const ITEM_STEPS: &[TutorialStep] = &[
    lesson_step(
        "Pickup and use",
        "Light picks up nearby items. Light again uses recovery and boost items; Heavy throws them.",
        TutorialObjective::Knowledge,
        &[TutorialPromptAction::Light, TutorialPromptAction::Heavy],
        "The exercise item appears beside Cat at every step.",
        "Confirm to begin the eight-item tour.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Apple",
        "Take damage, then pick up and eat the Apple to restore HP.",
        TutorialObjective::ItemUse {
            kind: ItemKind::Apple,
            uses: 1,
        },
        &[TutorialPromptAction::Light],
        "Light to pick up; Light again to eat.",
        "Stand beside the Apple and press Light twice, allowing the pickup animation to finish.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "White Wine",
        "Spend MP, then drink White Wine to restore it.",
        TutorialObjective::ItemUse {
            kind: ItemKind::WineWhite,
            uses: 1,
        },
        &[TutorialPromptAction::Light],
        "Light to pick up; Light again to drink.",
        "The step starts with missing MP so the blue meter visibly rises.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Turkey",
        "Turkey carries three healing portions. Use all three.",
        TutorialObjective::ItemUse {
            kind: ItemKind::Turkey,
            uses: 3,
        },
        &[TutorialPromptAction::Light],
        "Pick it up once, then use Light after each item animation.",
        "Wait until Cat returns to idle between portions; the HUD shows remaining uses.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Barrel recovery",
        "Use all three Barrel drinks to restore MP.",
        TutorialObjective::ItemUse {
            kind: ItemKind::Barrel,
            uses: 3,
        },
        &[TutorialPromptAction::Light],
        "Use Light for each drink.",
        "Wait for each use animation; the Barrel remains held until its final portion.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Barrel spray",
        "Throw a fresh Barrel with Heavy to create its damaging spray.",
        TutorialObjective::ItemThrow(ItemKind::Barrel),
        &[TutorialPromptAction::Light, TutorialPromptAction::Heavy],
        "Pick up with Light, then throw toward Pig with Heavy.",
        "Face Pig before pressing Heavy; the impact starts the four-second spray.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Coffee",
        "Drink Coffee for a temporary movement-speed boost.",
        TutorialObjective::ItemUse {
            kind: ItemKind::CupCoffee,
            uses: 1,
        },
        &[TutorialPromptAction::Light],
        "Pick up and use with Light.",
        "The speed status appears on the HUD after use.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Mushroom",
        "Eat the Mushroom to become larger and stronger, but easier to hit and damage.",
        TutorialObjective::ItemUse {
            kind: ItemKind::Mushroom,
            uses: 1,
        },
        &[TutorialPromptAction::Light],
        "Pick up and use with Light.",
        "Watch Cat's size and the temporary giant status indicator.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Mystery Crate",
        "The Mystery Crate is heavy utility: throw it to break it and reveal another item.",
        TutorialObjective::ItemThrow(ItemKind::Crate),
        &[TutorialPromptAction::Light, TutorialPromptAction::Heavy],
        "Pick up with Light and throw with Heavy.",
        "Aim the Heavy throw at Pig or the floor; Crates cannot be consumed.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Steamer",
        "Throw the Steamer to arm its timed blast. Leave the warning circle before it explodes.",
        TutorialObjective::ItemThrow(ItemKind::Steamer),
        &[TutorialPromptAction::Light, TutorialPromptAction::Heavy],
        "Pick up, throw, then move clear.",
        "Heavy throws the Steamer even though Light can also release it; retreat after the fuse starts.",
        ScriptedDummyMode::Passive,
    ),
];

const SPECIAL_STEPS: &[TutorialStep] = &[
    lesson_step(
        "Pulse Dart",
        "Spawn the fast straight Pulse Dart. Shared specials consume MP and enter cooldown.",
        TutorialObjective::SpecialSpawn(SpecialKind::Projectile),
        &[TutorialPromptAction::Special],
        "Use Special without a modifier.",
        "Release Light, Aim, and Heavy before pressing Special.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Trip Plate",
        "Place a Trip Plate trap in front of Cat.",
        TutorialObjective::SpecialSpawn(SpecialKind::Trap),
        &[TutorialPromptAction::Special, TutorialPromptAction::Light],
        "Hold Light as Special is pressed.",
        "The step refills MP and clears cooldown; press the displayed Special + Light chord together.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Snap Wave",
        "Release the close expanding Snap Wave.",
        TutorialObjective::SpecialSpawn(SpecialKind::Shockwave),
        &[TutorialPromptAction::Special, TutorialPromptAction::Aim],
        "Hold Aim as Special is pressed.",
        "Press Special while the displayed Aim control is already held.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Drift Field",
        "Create the lingering Drift Field hazard.",
        TutorialObjective::SpecialSpawn(SpecialKind::Hazard),
        &[TutorialPromptAction::Special, TutorialPromptAction::Heavy],
        "Hold Heavy as Special is pressed.",
        "Press Special while Heavy is held; each objective starts with full MP and no cooldown.",
        ScriptedDummyMode::Passive,
    ),
];

const HUD_STEPS: &[TutorialStep] = &[
    lesson_step(
        "HP and MP",
        "HP is your colored survival meter. MP is the blue resource used by advanced actions.",
        TutorialObjective::Knowledge,
        &[],
        "Both meters sit on each fighter plate.",
        "Confirm after locating Cat's HP and MP.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Lives",
        "Life Ring-Out starts each fighter with three lives. Losing the last life eliminates them.",
        TutorialObjective::Knowledge,
        &[],
        "The life count is shown beside the fighter portrait.",
        "Confirm after locating both stock counters.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Status and cooldowns",
        "The HUD shows held items, SPEED/GIANT timers, and SP/EQ cooldowns.",
        TutorialObjective::Knowledge,
        &[],
        "Compact icons appear only while relevant.",
        "Confirm to continue to danger states.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "EDGE danger",
        "EDGE warns that knockback can carry you beyond supported ground. Move back toward center.",
        TutorialObjective::Movement {
            direction: TutorialDirection::Any,
            distance: 1.5,
        },
        &[TutorialPromptAction::Move],
        "Move safely toward the ring center.",
        "Use the camera and floor outline to keep supported ground beneath Cat.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "KO at zero HP",
        "Reaching zero HP costs a life even without crossing the boundary.",
        TutorialObjective::Knowledge,
        &[],
        "Damage and ring-outs both feed the same stock victory rule.",
        "Confirm to practice boundary ring-outs.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Boundary ring-out",
        "Knock Pig beyond Crown Ring's boundary to take one life.",
        TutorialObjective::RingOutOpponent,
        &[TutorialPromptAction::Heavy, TutorialPromptAction::Light],
        "Build damage, then use Heavy attacks near the edge.",
        "Push Pig toward the nearest edge with the Light chain, then launch with Heavy.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Lose a life",
        "Cross Crown Ring's boundary once. Practice lessons restart this step instead of consuming the rest of the match.",
        TutorialObjective::LoseLife,
        &[TutorialPromptAction::Move],
        "Walk outward from the near edge until Cat rings out.",
        "The next objective resets both fighters and restores practice stocks.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Respawning",
        "After a non-final life is lost, the fighter returns with restored HP and brief invulnerability.",
        TutorialObjective::Knowledge,
        &[],
        "The stock count remains reduced after the respawn.",
        "Confirm once Pig has returned.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Elimination",
        "A fighter with no lives cannot participate. The match ends when one fighter remains.",
        TutorialObjective::Knowledge,
        &[],
        "Eliminated fighter plates remain as the match record.",
        "Confirm to finish the winning-rules lesson.",
        ScriptedDummyMode::Passive,
    ),
    lesson_step(
        "Last fighter standing",
        "Life Ring-Out victory belongs to the final fighter with stock remaining.",
        TutorialObjective::Knowledge,
        &[],
        "The Final Exam uses this exact three-life ruleset.",
        "Confirm to complete HUD & Winning.",
        ScriptedDummyMode::Passive,
    ),
];

macro_rules! lab_step {
    ($title:literal, $instruction:literal, $technique:expr, $confirmed:expr, [$($control:expr),* $(,)?]) => {
        lesson_step(
            $title,
            $instruction,
            TutorialObjective::ActivateTechnique {
                technique: $technique,
                confirmed_hit: $confirmed,
            },
            &[$($control),*],
            "Use the displayed recipe on the passive Pig.",
            "Reset the step for clean spacing, then perform the recipe after both fighters return to idle.",
            ScriptedDummyMode::Passive,
        )
    };
}

const CAT_LAB_STEPS: &[TutorialStep] = &[
    lab_step!(
        "Paw-Paw-Flare",
        "Complete Cat's grounded Light chain.",
        TechniqueId::CatComboFinisher,
        true,
        [TutorialPromptAction::Light]
    ),
    lab_step!(
        "Rising Claw",
        "Complete Cat's two-hit Heavy route.",
        TechniqueId::CatHeavy2,
        true,
        [TutorialPromptAction::Heavy]
    ),
    lab_step!(
        "Pounce Flare",
        "Dash and branch to Cat's Light finisher.",
        TechniqueId::CatDashComboFinisher,
        true,
        [TutorialPromptAction::Dash, TutorialPromptAction::Light]
    ),
    lab_step!(
        "Diving Paw",
        "Land Cat's aerial Light.",
        TechniqueId::CatJumpAttack,
        true,
        [TutorialPromptAction::Jump, TutorialPromptAction::Light]
    ),
    lab_step!(
        "Falling Fish",
        "Land Cat's aerial Heavy.",
        TechniqueId::CatJumpHeavy,
        true,
        [TutorialPromptAction::Jump, TutorialPromptAction::Heavy]
    ),
    lesson_step(
        "Pulse Dart",
        "Demonstrate Cat's shared neutral special.",
        TutorialObjective::SpecialSpawn(SpecialKind::Projectile),
        &[TutorialPromptAction::Special],
        "Press Special alone.",
        "Release every modifier before pressing Special.",
        ScriptedDummyMode::Passive,
    ),
    lab_step!(
        "Royal Rush",
        "Catch Pig with Cat's ultimate rush.",
        TechniqueId::CatUltimateRush,
        true,
        [TutorialPromptAction::Ultimate]
    ),
];

const PIG_LAB_STEPS: &[TutorialStep] = &[
    lab_step!(
        "Ham Slam",
        "Complete Pig's grounded Light chain.",
        TechniqueId::PigComboFinisher,
        true,
        [TutorialPromptAction::Light]
    ),
    lab_step!(
        "Half-Circle Ham",
        "Charge and land Pig's grounded Heavy.",
        TechniqueId::PigHeavy,
        true,
        [TutorialPromptAction::Heavy]
    ),
    lab_step!(
        "Belly Rush",
        "Dash into Pig's attack.",
        TechniqueId::PigDashAttack,
        true,
        [TutorialPromptAction::Dash, TutorialPromptAction::Light]
    ),
    lab_step!(
        "Air Pounce",
        "Land Pig's aerial Light.",
        TechniqueId::PigJumpAttack,
        true,
        [TutorialPromptAction::Jump, TutorialPromptAction::Light]
    ),
    lab_step!(
        "Air Meat Slam",
        "Land Pig's plunging aerial Heavy.",
        TechniqueId::PigJumpHeavy,
        true,
        [TutorialPromptAction::Jump, TutorialPromptAction::Heavy]
    ),
    lesson_step(
        "Trip Plate",
        "Demonstrate Pig's shared trap special.",
        TutorialObjective::SpecialSpawn(SpecialKind::Trap),
        &[TutorialPromptAction::Special, TutorialPromptAction::Light],
        "Press Special + Light.",
        "Hold Light before pressing Special.",
        ScriptedDummyMode::Passive,
    ),
    lab_step!(
        "Unblockable Ham Rush",
        "Catch the dummy with Pig's ultimate.",
        TechniqueId::PigUltimateRush,
        true,
        [TutorialPromptAction::Ultimate]
    ),
];

const BEE_LAB_STEPS: &[TutorialStep] = &[
    lab_step!(
        "Worker Shot",
        "Fire Bee's grounded Light worker shot.",
        TechniqueId::BeeLight1,
        true,
        [TutorialPromptAction::Light]
    ),
    lab_step!(
        "Homing Burst",
        "Fire Bee's Heavy route.",
        TechniqueId::BeeHeavy2,
        true,
        [TutorialPromptAction::Heavy]
    ),
    lab_step!(
        "Air Dash Shot",
        "Dash and use Bee's action route.",
        TechniqueId::BeeDashAttack,
        false,
        [TutorialPromptAction::Dash, TutorialPromptAction::Light]
    ),
    lab_step!(
        "Dive Worker",
        "Land Bee's aerial Light.",
        TechniqueId::BeeJumpAttack,
        true,
        [TutorialPromptAction::Jump, TutorialPromptAction::Light]
    ),
    lab_step!(
        "Honey Drop",
        "Land Bee's aerial Heavy.",
        TechniqueId::BeeJumpHeavy,
        true,
        [TutorialPromptAction::Jump, TutorialPromptAction::Heavy]
    ),
    lesson_step(
        "Snap Wave",
        "Demonstrate Bee's shared close special.",
        TutorialObjective::SpecialSpawn(SpecialKind::Shockwave),
        &[TutorialPromptAction::Special, TutorialPromptAction::Aim],
        "Press Special + Aim.",
        "Hold Aim before pressing Special.",
        ScriptedDummyMode::Passive,
    ),
    lab_step!(
        "Royal Swarm",
        "Summon Bee's ultimate area swarm.",
        TechniqueId::BeeUltimateStartup,
        false,
        [TutorialPromptAction::Ultimate]
    ),
];

const PENGUIN_LAB_STEPS: &[TutorialStep] = &[
    lab_step!(
        "Snow Peck Chain",
        "Complete Penguin's grounded Light chain.",
        TechniqueId::PenguinComboFinisher,
        true,
        [TutorialPromptAction::Light]
    ),
    lab_step!(
        "Ice Launcher",
        "Complete Penguin's Heavy route.",
        TechniqueId::PenguinHeavy2,
        true,
        [TutorialPromptAction::Heavy]
    ),
    lab_step!(
        "Snowflake Slide",
        "Dash and use Penguin's Light action.",
        TechniqueId::PenguinDashAttack,
        false,
        [TutorialPromptAction::Dash, TutorialPromptAction::Light]
    ),
    lab_step!(
        "Slope Crash",
        "Dash and use Penguin's Heavy action.",
        TechniqueId::PenguinDashHeavy,
        true,
        [TutorialPromptAction::Dash, TutorialPromptAction::Heavy]
    ),
    lab_step!(
        "Fish Torpedo",
        "Land Penguin's aerial Light.",
        TechniqueId::PenguinJumpAttack,
        true,
        [TutorialPromptAction::Jump, TutorialPromptAction::Light]
    ),
    lab_step!(
        "Snowman Drop",
        "Land Penguin's aerial Heavy.",
        TechniqueId::PenguinJumpHeavy,
        true,
        [TutorialPromptAction::Jump, TutorialPromptAction::Heavy]
    ),
    lesson_step(
        "Drift Field",
        "Demonstrate Penguin's shared lingering special.",
        TutorialObjective::SpecialSpawn(SpecialKind::Hazard),
        &[TutorialPromptAction::Special, TutorialPromptAction::Heavy],
        "Press Special + Heavy.",
        "Hold Heavy before pressing Special.",
        ScriptedDummyMode::Passive,
    ),
    lab_step!(
        "Grand Ice Field",
        "Start Penguin's ultimate slide route.",
        TechniqueId::PenguinUltimateRush,
        false,
        [TutorialPromptAction::Ultimate]
    ),
];

const CHICK_LAB_STEPS: &[TutorialStep] = &[
    lab_step!(
        "Egg Peck Chain",
        "Complete Chick's grounded Light chain.",
        TechniqueId::ChickComboFinisher,
        true,
        [TutorialPromptAction::Light]
    ),
    lab_step!(
        "Orbit Egg Heavy",
        "Complete Chick's Heavy route.",
        TechniqueId::ChickHeavy2,
        false,
        [TutorialPromptAction::Heavy]
    ),
    lab_step!(
        "Scoot Shot",
        "Dash and use Chick's Light action.",
        TechniqueId::ChickDashAttack,
        false,
        [TutorialPromptAction::Dash, TutorialPromptAction::Light]
    ),
    lab_step!(
        "Long Scoot",
        "Dash and use Chick's Heavy action.",
        TechniqueId::ChickDashHeavy,
        false,
        [TutorialPromptAction::Dash, TutorialPromptAction::Heavy]
    ),
    lab_step!(
        "Egg Dive",
        "Land Chick's aerial Light.",
        TechniqueId::ChickJumpAttack,
        true,
        [TutorialPromptAction::Jump, TutorialPromptAction::Light]
    ),
    lab_step!(
        "Fresh Egg Drop",
        "Use Chick's aerial Heavy.",
        TechniqueId::ChickJumpHeavy,
        false,
        [TutorialPromptAction::Jump, TutorialPromptAction::Heavy]
    ),
    lesson_step(
        "Pulse Dart",
        "Demonstrate Chick's shared neutral special.",
        TutorialObjective::SpecialSpawn(SpecialKind::Projectile),
        &[TutorialPromptAction::Special],
        "Press Special alone.",
        "Release every modifier before pressing Special.",
        ScriptedDummyMode::Passive,
    ),
    lab_step!(
        "Sixteen-Egg Burst",
        "Start Chick's ultimate egg burst.",
        TechniqueId::ChickUltimateStartup,
        false,
        [TutorialPromptAction::Ultimate]
    ),
];

const FINAL_EXAM_STEPS: &[TutorialStep] = &[lesson_step(
    "Final Exam",
    "Defeat the tutorial-difficulty Pig in a normal three-life Crown Ring match.",
    TutorialObjective::MatchResult { win: true },
    &[
        TutorialPromptAction::Move,
        TutorialPromptAction::Light,
        TutorialPromptAction::Heavy,
        TutorialPromptAction::Special,
    ],
    "Use movement, defense, items, shared specials, and Cat's full move set.",
    "Control center stage, preserve MP for recovery pressure, and launch Pig near the boundary.",
    ScriptedDummyMode::NormalBot,
)];

pub const TUTORIAL_CHAPTERS: [TutorialChapter; 12] = [
    TutorialChapter {
        id: TutorialChapterId::Basics,
        number: 1,
        title: "Basics",
        summary: "Movement, camera-relative control, jump, dash, aim, and your active device layout.",
        player_character: CharacterKind::Cat,
        steps: BASICS_STEPS,
        final_exam: false,
    },
    TutorialChapter {
        id: TutorialChapterId::Combat,
        number: 2,
        title: "Combat",
        summary: "Light and Heavy routes, aerials, grabs, MP, and Cat's ultimate.",
        player_character: CharacterKind::Cat,
        steps: COMBAT_STEPS,
        final_exam: false,
    },
    TutorialChapter {
        id: TutorialChapterId::DefenseRecovery,
        number: 3,
        title: "Defense & Recovery",
        summary: "Guard, perfect counters, guard steps, grab escape, quick stand, and recovery roll.",
        player_character: CharacterKind::Cat,
        steps: DEFENSE_STEPS,
        final_exam: false,
    },
    TutorialChapter {
        id: TutorialChapterId::Items,
        number: 4,
        title: "Items",
        summary: "Pickup, use, and throw every current arena item.",
        player_character: CharacterKind::Cat,
        steps: ITEM_STEPS,
        final_exam: false,
    },
    TutorialChapter {
        id: TutorialChapterId::SharedSpecials,
        number: 5,
        title: "Shared Specials",
        summary: "Pulse Dart, Trip Plate, Snap Wave, and Drift Field.",
        player_character: CharacterKind::Cat,
        steps: SPECIAL_STEPS,
        final_exam: false,
    },
    TutorialChapter {
        id: TutorialChapterId::HudWinning,
        number: 6,
        title: "HUD & Winning",
        summary: "Read HP, MP, stocks, status, EDGE danger, respawns, elimination, and victory.",
        player_character: CharacterKind::Cat,
        steps: HUD_STEPS,
        final_exam: false,
    },
    TutorialChapter {
        id: TutorialChapterId::CatLab,
        number: 7,
        title: "Cat Lab",
        summary: "Verify Cat's grounded, dash, aerial, special, and ultimate routes.",
        player_character: CharacterKind::Cat,
        steps: CAT_LAB_STEPS,
        final_exam: false,
    },
    TutorialChapter {
        id: TutorialChapterId::PigLab,
        number: 8,
        title: "Pig Lab",
        summary: "Verify Pig's grounded, dash, aerial, special, and ultimate routes.",
        player_character: CharacterKind::Pig,
        steps: PIG_LAB_STEPS,
        final_exam: false,
    },
    TutorialChapter {
        id: TutorialChapterId::BeeLab,
        number: 9,
        title: "Bee Lab",
        summary: "Verify Bee's grounded, dash, aerial, special, and ultimate routes.",
        player_character: CharacterKind::Bee,
        steps: BEE_LAB_STEPS,
        final_exam: false,
    },
    TutorialChapter {
        id: TutorialChapterId::PenguinLab,
        number: 10,
        title: "Penguin Lab",
        summary: "Verify Penguin's grounded, dash, aerial, special, and ultimate routes.",
        player_character: CharacterKind::Penguin,
        steps: PENGUIN_LAB_STEPS,
        final_exam: false,
    },
    TutorialChapter {
        id: TutorialChapterId::ChickLab,
        number: 11,
        title: "Chick Lab",
        summary: "Verify Chick's grounded, dash, aerial, special, and ultimate routes.",
        player_character: CharacterKind::Chick,
        steps: CHICK_LAB_STEPS,
        final_exam: false,
    },
    TutorialChapter {
        id: TutorialChapterId::FinalExam,
        number: 12,
        title: "Final Exam",
        summary: "Cat versus a forgiving normal-rules Pig: three lives, Crown Ring items, last fighter standing.",
        player_character: CharacterKind::Cat,
        steps: FINAL_EXAM_STEPS,
        final_exam: true,
    },
];

pub fn tutorial_chapter(id: TutorialChapterId) -> &'static TutorialChapter {
    &TUTORIAL_CHAPTERS[chapter_index(id)]
}

pub fn chapter_index(id: TutorialChapterId) -> usize {
    TutorialChapterId::ALL
        .iter()
        .position(|candidate| *candidate == id)
        .expect("every tutorial chapter ID belongs to the catalog")
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TutorialPhase {
    #[default]
    Inactive,
    WaitingForMatch,
    Prompt,
    Playing,
    Success,
    PauseMenu,
    ChapterComplete,
    FinalResult,
}

const TUTORIAL_FADE_OUT_SECONDS: f32 = 0.18;
const TUTORIAL_FADE_HOLD_SECONDS: f32 = 0.06;
const TUTORIAL_FADE_IN_SECONDS: f32 = 0.26;
const TUTORIAL_SETTLE_STABLE_SECONDS: f32 = 0.18;
const TUTORIAL_AIM_HOLD_SECONDS: f32 = 0.35;
const TUTORIAL_SUCCESS_REVEAL_SECONDS: f32 = 0.14;
const TUTORIAL_SUCCESS_MIN_SECONDS: f32 = 0.25;
const TUTORIAL_SUCCESS_AUTO_SECONDS: f32 = 1.1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TutorialCompletionState {
    #[default]
    Observing,
    Settling,
    Succeeded,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum TutorialTransitionStage {
    #[default]
    Idle,
    FadingOut,
    Covered,
    FadingIn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TutorialTransitionAction {
    EnterDeviceJoin,
    EnterHub,
    StartChapter(TutorialChapterId),
    BeginPlaying,
    AdvanceStep { skipped: bool },
    OpenPause,
    ResumePause,
    RestartStep,
    ExitToHub,
    LeaveTutorial,
    ShowFinalResult { won: bool },
    FinalSkipToHub,
}

#[derive(Resource, Clone, Debug)]
pub struct TutorialTransition {
    stage: TutorialTransitionStage,
    elapsed: f32,
    pending_action: Option<TutorialTransitionAction>,
    wait_for_lesson_ready: bool,
}

impl Default for TutorialTransition {
    fn default() -> Self {
        Self {
            stage: TutorialTransitionStage::Idle,
            elapsed: 0.0,
            pending_action: None,
            wait_for_lesson_ready: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TutorialTransitionAdvance {
    action: Option<TutorialTransitionAction>,
    finished: bool,
}

impl TutorialTransition {
    pub(crate) fn active(&self) -> bool {
        self.stage != TutorialTransitionStage::Idle
    }

    fn request(&mut self, action: TutorialTransitionAction) -> bool {
        if self.active() {
            return false;
        }
        self.stage = TutorialTransitionStage::FadingOut;
        self.elapsed = 0.0;
        self.pending_action = Some(action);
        self.wait_for_lesson_ready = matches!(action, TutorialTransitionAction::StartChapter(_));
        true
    }

    fn advance(&mut self, delta_seconds: f32, reveal_ready: bool) -> TutorialTransitionAdvance {
        let delta_seconds = delta_seconds.max(0.0);
        match self.stage {
            TutorialTransitionStage::Idle => TutorialTransitionAdvance::default(),
            TutorialTransitionStage::FadingOut => {
                self.elapsed = (self.elapsed + delta_seconds).min(TUTORIAL_FADE_OUT_SECONDS);
                if self.elapsed < TUTORIAL_FADE_OUT_SECONDS {
                    return TutorialTransitionAdvance::default();
                }
                self.stage = TutorialTransitionStage::Covered;
                self.elapsed = 0.0;
                TutorialTransitionAdvance {
                    action: self.pending_action.take(),
                    finished: false,
                }
            }
            TutorialTransitionStage::Covered => {
                if self.wait_for_lesson_ready && !reveal_ready {
                    return TutorialTransitionAdvance::default();
                }
                self.elapsed = (self.elapsed + delta_seconds).min(TUTORIAL_FADE_HOLD_SECONDS);
                if self.elapsed >= TUTORIAL_FADE_HOLD_SECONDS {
                    self.stage = TutorialTransitionStage::FadingIn;
                    self.elapsed = 0.0;
                    self.wait_for_lesson_ready = false;
                }
                TutorialTransitionAdvance::default()
            }
            TutorialTransitionStage::FadingIn => {
                self.elapsed = (self.elapsed + delta_seconds).min(TUTORIAL_FADE_IN_SECONDS);
                if self.elapsed < TUTORIAL_FADE_IN_SECONDS {
                    return TutorialTransitionAdvance::default();
                }
                *self = Self::default();
                TutorialTransitionAdvance {
                    action: None,
                    finished: true,
                }
            }
        }
    }

    fn alpha(&self) -> f32 {
        match self.stage {
            TutorialTransitionStage::Idle => 0.0,
            TutorialTransitionStage::FadingOut => {
                tutorial_fade_ease(self.elapsed / TUTORIAL_FADE_OUT_SECONDS)
            }
            TutorialTransitionStage::Covered => 1.0,
            TutorialTransitionStage::FadingIn => {
                1.0 - tutorial_fade_ease(self.elapsed / TUTORIAL_FADE_IN_SECONDS)
            }
        }
    }
}

fn tutorial_fade_ease(amount: f32) -> f32 {
    let amount = amount.clamp(0.0, 1.0);
    amount * amount * (3.0 - 2.0 * amount)
}

pub(crate) fn request_tutorial_transition(
    transition: &mut TutorialTransition,
    pause_owners: &mut GameplayPauseOwners,
    action: TutorialTransitionAction,
) -> bool {
    let started = transition.request(action);
    if started {
        pause_owners.set(GameplayPauseOwner::TutorialTransition, true);
    }
    started
}

pub fn tutorial_transition_active(transition: Res<TutorialTransition>) -> bool {
    transition.active()
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TutorialObjectiveBaseline {
    pub player_position: Vec3,
    pub last_player_position: Vec3,
    pub player_damage: f32,
    pub ring_outs: u32,
    pub player_stock: i32,
    pub dummy_stock: i32,
    pub item_durability: i32,
    pub lesson_item_entity: Option<Entity>,
    pub item_count: usize,
}

#[derive(Resource, Clone, Debug)]
pub struct TutorialSession {
    pub chapter: Option<TutorialChapterId>,
    pub step_index: usize,
    pub attempts: u32,
    pub skipped_steps: BTreeSet<usize>,
    pub phase: TutorialPhase,
    pub paused_from: TutorialPhase,
    pub hub_cursor: usize,
    pub pause_cursor: usize,
    pub final_cursor: usize,
    pub reset_confirmation: bool,
    pub reset_requested: bool,
    pub cleanup_requested: bool,
    pub objective_progress: u32,
    pub objective_baseline: TutorialObjectiveBaseline,
    pub objective_latched: bool,
    pub item_was_held: bool,
    pub completion_state: TutorialCompletionState,
    pub completion_stable_elapsed: f32,
    pub completion_hold_elapsed: f32,
    pub completion_saw_airborne: bool,
    pub completion_world_effect_seen: bool,
    pub success_elapsed: f32,
    pub final_exam_won: Option<bool>,
    pub dummy_script_elapsed: f32,
    return_setup: Option<LocalSetup>,
}

impl Default for TutorialSession {
    fn default() -> Self {
        Self {
            chapter: None,
            step_index: 0,
            attempts: 0,
            skipped_steps: BTreeSet::new(),
            phase: TutorialPhase::Inactive,
            paused_from: TutorialPhase::Inactive,
            hub_cursor: 0,
            pause_cursor: 0,
            final_cursor: 0,
            reset_confirmation: false,
            reset_requested: false,
            cleanup_requested: false,
            objective_progress: 0,
            objective_baseline: TutorialObjectiveBaseline::default(),
            objective_latched: false,
            item_was_held: false,
            completion_state: TutorialCompletionState::Observing,
            completion_stable_elapsed: 0.0,
            completion_hold_elapsed: 0.0,
            completion_saw_airborne: false,
            completion_world_effect_seen: false,
            success_elapsed: 0.0,
            final_exam_won: None,
            dummy_script_elapsed: 0.0,
            return_setup: None,
        }
    }
}

impl TutorialSession {
    pub fn start(&mut self, chapter: TutorialChapterId) {
        self.chapter = Some(chapter);
        self.step_index = 0;
        self.attempts = 0;
        self.skipped_steps.clear();
        self.phase = TutorialPhase::WaitingForMatch;
        self.paused_from = TutorialPhase::Inactive;
        self.pause_cursor = 0;
        self.final_cursor = 0;
        self.reset_confirmation = false;
        self.reset_requested = true;
        self.cleanup_requested = false;
        self.objective_progress = 0;
        self.objective_baseline = TutorialObjectiveBaseline::default();
        self.objective_latched = false;
        self.item_was_held = false;
        self.reset_completion();
        self.final_exam_won = None;
        self.dummy_script_elapsed = 0.0;
    }

    pub fn current_chapter(&self) -> Option<&'static TutorialChapter> {
        self.chapter.map(tutorial_chapter)
    }

    pub fn current_step(&self) -> Option<&'static TutorialStep> {
        self.current_chapter()
            .and_then(|chapter| chapter.steps.get(self.step_index))
    }

    pub fn active(&self) -> bool {
        self.chapter.is_some() && self.phase != TutorialPhase::Inactive
    }

    pub fn stronger_hint_active(&self) -> bool {
        self.attempts >= 3
    }

    pub fn chapter_can_complete(&self) -> bool {
        self.skipped_steps.is_empty()
    }

    pub fn begin_prompt(&mut self) {
        self.phase = TutorialPhase::Prompt;
        self.reset_requested = true;
        self.objective_progress = 0;
        self.objective_latched = false;
        self.item_was_held = false;
        self.reset_completion();
        self.dummy_script_elapsed = 0.0;
    }

    pub fn resume_step(&mut self) {
        self.phase = TutorialPhase::Playing;
        self.pause_cursor = 0;
        self.dummy_script_elapsed = 0.0;
    }

    pub fn pause(&mut self) {
        if matches!(
            self.phase,
            TutorialPhase::Prompt | TutorialPhase::Playing | TutorialPhase::Success
        ) {
            self.paused_from = self.phase;
            self.phase = TutorialPhase::PauseMenu;
            self.pause_cursor = 0;
        }
    }

    pub fn resume_from_pause(&mut self) {
        if self.phase == TutorialPhase::PauseMenu {
            self.phase = match self.paused_from {
                TutorialPhase::Prompt => TutorialPhase::Prompt,
                TutorialPhase::Success => TutorialPhase::Success,
                _ => TutorialPhase::Playing,
            };
            self.paused_from = TutorialPhase::Inactive;
        }
    }

    pub fn reset_completion(&mut self) {
        self.completion_state = TutorialCompletionState::Observing;
        self.completion_stable_elapsed = 0.0;
        self.completion_hold_elapsed = 0.0;
        self.completion_saw_airborne = false;
        self.completion_world_effect_seen = false;
        self.success_elapsed = 0.0;
    }

    pub fn restart_step(&mut self) {
        self.attempts = self.attempts.saturating_add(1);
        self.begin_prompt();
    }

    pub fn finish_step(&mut self, skipped: bool) -> bool {
        if skipped {
            self.skipped_steps.insert(self.step_index);
        }
        let Some(chapter) = self.current_chapter() else {
            return false;
        };
        if self.step_index + 1 >= chapter.steps.len() {
            self.phase = TutorialPhase::ChapterComplete;
            return true;
        }
        self.step_index += 1;
        self.attempts = 0;
        self.begin_prompt();
        false
    }

    pub fn request_cleanup(&mut self) {
        self.cleanup_requested = true;
    }

    pub fn clear_for_hub(&mut self) {
        let hub_cursor = self.hub_cursor;
        *self = Self {
            hub_cursor,
            ..default()
        };
    }
}

pub fn tutorial_grid_move(cursor: usize, direction: IVec2) -> usize {
    let row = cursor / 2;
    let column = cursor % 2;
    let next_column = if direction.x < 0 {
        column.saturating_sub(1)
    } else if direction.x > 0 {
        (column + 1).min(1)
    } else {
        column
    };
    let next_row = if direction.y < 0 {
        row.saturating_sub(1)
    } else if direction.y > 0 {
        (row + 1).min(5)
    } else {
        row
    };
    (next_row * 2 + next_column).min(TUTORIAL_CHAPTERS.len() - 1)
}

pub fn tutorial_control_prompt(
    step: &TutorialStep,
    assignment: LocalInputAssignment,
    bindings: &PlayerKeyBindings,
    controller_metadata: &Query<&ControllerDeviceInfo>,
) -> String {
    step.controls
        .iter()
        .map(|action| tutorial_action_label(*action, assignment, bindings, controller_metadata))
        .collect::<Vec<_>>()
        .join(" + ")
}

fn tutorial_action_label(
    action: TutorialPromptAction,
    assignment: LocalInputAssignment,
    bindings: &PlayerKeyBindings,
    controller_metadata: &Query<&ControllerDeviceInfo>,
) -> String {
    match assignment {
        LocalInputAssignment::Gamepad(entity) => {
            let family = controller_info(entity, controller_metadata)
                .map(|info| info.family)
                .unwrap_or_default();
            controller_tutorial_action_label(action, family)
        }
        LocalInputAssignment::Keyboard(player) => {
            keyboard_tutorial_action_label(action, player, bindings)
        }
        LocalInputAssignment::Unassigned => keyboard_tutorial_action_label(action, 0, bindings),
    }
}

fn controller_tutorial_action_label(
    action: TutorialPromptAction,
    family: ControllerFamily,
) -> String {
    match action {
        TutorialPromptAction::Move => "Left stick / D-pad".to_string(),
        TutorialPromptAction::Aim => family.face_button_label(GamepadButton::East).to_string(),
        TutorialPromptAction::Light => family.face_button_label(GamepadButton::West).to_string(),
        TutorialPromptAction::Heavy => family.face_button_label(GamepadButton::North).to_string(),
        TutorialPromptAction::Jump => family.face_button_label(GamepadButton::South).to_string(),
        TutorialPromptAction::Special => family
            .face_button_label(GamepadButton::RightTrigger)
            .to_string(),
        TutorialPromptAction::Dash => family
            .face_button_label(GamepadButton::RightTrigger2)
            .to_string(),
        TutorialPromptAction::Guard => family
            .face_button_label(GamepadButton::LeftTrigger)
            .to_string(),
        TutorialPromptAction::Ultimate => family
            .face_button_label(GamepadButton::LeftTrigger2)
            .to_string(),
        TutorialPromptAction::Menu => family.face_button_label(GamepadButton::Start).to_string(),
        TutorialPromptAction::Confirm => family
            .face_button_label(family.confirm_button())
            .to_string(),
    }
}

fn keyboard_tutorial_action_label(
    action: TutorialPromptAction,
    player: usize,
    bindings: &PlayerKeyBindings,
) -> String {
    let key = |action| {
        bindings
            .key_for(player, action)
            .map(tutorial_key_label)
            .unwrap_or_else(|| "?".to_string())
    };
    match action {
        TutorialPromptAction::Move => format!(
            "{}/{}/{}/{}",
            key(ControlAction::Left),
            key(ControlAction::Right),
            key(ControlAction::Up),
            key(ControlAction::Down)
        ),
        TutorialPromptAction::Aim => key(ControlAction::AimGrab),
        TutorialPromptAction::Light => key(ControlAction::Light),
        TutorialPromptAction::Heavy => key(ControlAction::Heavy),
        TutorialPromptAction::Jump => key(ControlAction::Jump),
        TutorialPromptAction::Special => key(ControlAction::Special),
        TutorialPromptAction::Dash => "double-tap Move".to_string(),
        TutorialPromptAction::Guard => format!(
            "{} + {}",
            key(ControlAction::Light),
            key(ControlAction::Heavy)
        ),
        TutorialPromptAction::Ultimate => format!(
            "{} + {} + {}",
            key(ControlAction::AimGrab),
            key(ControlAction::Light),
            key(ControlAction::Heavy)
        ),
        TutorialPromptAction::Menu => "Esc".to_string(),
        TutorialPromptAction::Confirm => "Enter".to_string(),
    }
}

fn tutorial_key_label(key: KeyCode) -> String {
    let raw = format!("{key:?}");
    if let Some(label) = raw.strip_prefix("Key") {
        label.to_string()
    } else if let Some(label) = raw.strip_prefix("Digit") {
        label.to_string()
    } else if let Some(label) = raw.strip_prefix("Arrow") {
        format!("{label} Arrow")
    } else {
        raw
    }
}

pub fn tutorial_objective_progress_text(session: &TutorialSession) -> String {
    let Some(step) = session.current_step() else {
        return String::new();
    };
    let target = step.objective.target_count();
    match step.objective {
        TutorialObjective::Movement { distance, .. } => format!(
            "{:.1} / {:.1} m",
            session.objective_progress as f32 / 10.0,
            distance
        ),
        _ if target > 1 => format!("{} / {target}", session.objective_progress.min(target)),
        _ => String::new(),
    }
}

fn tutorial_settlement_status(session: &TutorialSession) -> Option<&'static str> {
    if session.completion_state != TutorialCompletionState::Settling {
        return None;
    }
    Some(match session.current_step()?.objective {
        TutorialObjective::Movement { .. } => "RELEASE MOVEMENT TO FINISH",
        TutorialObjective::Input(_) => "RELEASE AIM TO FINISH",
        TutorialObjective::Action(FighterAction::Jumping) => "FINISH THE JUMP AND LAND",
        TutorialObjective::Action(FighterAction::Dashing) => "LET THE DASH FINISH",
        TutorialObjective::ActivateTechnique { .. } if session.completion_saw_airborne => {
            "FINISH THE MOVE AND LAND"
        }
        TutorialObjective::Guarding { .. } => "RELEASE GUARD AFTER THE BLOCK",
        TutorialObjective::GrabEscape => "REGAIN CONTROL TO FINISH",
        TutorialObjective::ItemThrow(ItemKind::Barrel) => "WAIT FOR THE SPRAY",
        TutorialObjective::ItemThrow(ItemKind::Crate) => "WAIT FOR THE REVEAL",
        TutorialObjective::ItemThrow(ItemKind::Steamer) => "MOVE CLEAR - WAIT FOR THE BLAST",
        TutorialObjective::SpecialSpawn(_) => "LET THE SPECIAL ACTIVATE",
        TutorialObjective::RingOutOpponent | TutorialObjective::LoseLife => "WAIT FOR THE RESPAWN",
        _ => "LET THE ACTION FINISH",
    })
}

pub fn configure_tutorial_match(
    chapter_id: TutorialChapterId,
    assignment: LocalInputAssignment,
    setup: &mut LocalSetup,
    state: &mut MatchState,
    user_mode: &mut UserModeState,
    session: &mut TutorialSession,
    pause_owners: &mut GameplayPauseOwners,
) {
    let chapter = tutorial_chapter(chapter_id);
    if session.return_setup.is_none() {
        session.return_setup = Some(setup.clone());
    }
    setup.set_rule(2);
    setup.arena_index = 0;
    setup.configure_single_player_duel(chapter.player_character, CharacterKind::Pig);
    setup.slots[TUTORIAL_PLAYER_ID].input = assignment;
    state.rule_index = setup.rule_index;
    state.rules = setup.active_rule();
    state.arena_index = 0;
    state.apply_local_setup(setup);
    state.replay_seed = setup.replay_seed;
    set_active_arena_index(0);
    session.start(chapter_id);
    pause_owners.set(GameplayPauseOwner::TutorialPrompt, true);
    pause_owners.set(GameplayPauseOwner::TutorialMenu, false);
    pause_owners.set(GameplayPauseOwner::TutorialSuccess, false);
    user_mode.enter_tutorial_lesson();
    state.request_rematch();
}

fn current_item_objective(session: &TutorialSession) -> Option<ItemKind> {
    match session.current_step()?.objective {
        TutorialObjective::ItemUse { kind, .. } | TutorialObjective::ItemThrow(kind) => Some(kind),
        _ => None,
    }
}

fn practice_positions(objective: TutorialObjective) -> [Vec3; 2] {
    match objective {
        TutorialObjective::RingOutOpponent => [
            Vec3::new(2.6, ARENA_TOP_Y, 2.8),
            Vec3::new(5.0, ARENA_TOP_Y, 2.8),
        ],
        TutorialObjective::LoseLife => [
            Vec3::new(5.0, ARENA_TOP_Y, 2.8),
            Vec3::new(0.0, ARENA_TOP_Y, 0.0),
        ],
        TutorialObjective::Movement { .. } => [
            Vec3::new(-2.2, ARENA_TOP_Y, 0.0),
            Vec3::new(3.6, ARENA_TOP_Y, 2.8),
        ],
        _ => [
            Vec3::new(-1.35, ARENA_TOP_Y, 0.0),
            Vec3::new(1.35, ARENA_TOP_Y, 0.0),
        ],
    }
}

#[allow(clippy::type_complexity)]
pub fn reset_tutorial_step(
    mut commands: Commands,
    mut session: ResMut<TutorialSession>,
    mut state: ResMut<MatchState>,
    telemetry: Res<MatchTelemetry>,
    item_assets: Option<Res<ItemAssets>>,
    mut pause_owners: ResMut<GameplayPauseOwners>,
    mut hitstop: ResMut<Hitstop>,
    mut feedback: ResMut<HitEffects>,
    mut camera_effects: ResMut<CameraActionEffects>,
    transient_entities: Query<
        Entity,
        Or<(
            With<Hitbox>,
            With<ActiveSpecial>,
            With<ActiveBeeSkill>,
            With<ActiveChickSkill>,
            With<ActivePenguinSkill>,
            With<ActivePenguinSurface>,
            With<VisualEffect>,
        )>,
    >,
    mut fighters: Query<(
        &Fighter,
        &mut FighterStats,
        &mut FighterMotor,
        &mut FighterInput,
        &mut FighterActionState,
        &mut FighterInventory,
        &mut FighterGrabState,
        &mut FighterSpecialState,
        &mut DrunkStatus,
        &mut FighterEquipment,
        &mut Transform,
        &mut Visibility,
    )>,
    mut bots: Query<(Entity, &Fighter, &mut BotBrain)>,
    mut items: Query<
        (
            Entity,
            &mut ArenaItem,
            &mut Transform,
            &mut Visibility,
            &mut MeshMaterial3d<StandardMaterial>,
            &mut SceneRoot,
        ),
        Without<Fighter>,
    >,
) {
    if !session.active() || !session.reset_requested || state.phase != MatchPhase::Fighting {
        return;
    }
    let Some(chapter) = session.current_chapter() else {
        return;
    };
    let Some(step) = session.current_step() else {
        return;
    };
    let Some(item_assets) = item_assets.as_ref() else {
        return;
    };

    for entity in &transient_entities {
        commands.entity(entity).despawn();
    }
    hitstop.remaining = 0.0;
    *feedback = HitEffects::default();
    *camera_effects = CameraActionEffects::default();

    let arena = active_arena_definition();
    let positions = if chapter.final_exam {
        [arena.spawn_points[0], arena.spawn_points[1]]
    } else {
        practice_positions(step.objective)
    };
    let mut player_position = positions[0];
    for (
        fighter,
        mut stats,
        mut motor,
        mut input,
        mut action,
        mut inventory,
        mut grab,
        mut special,
        mut drunk,
        mut equipment,
        mut transform,
        mut visibility,
    ) in &mut fighters
    {
        *stats = FighterStats::default();
        if fighter.id == TUTORIAL_PLAYER_ID {
            match step.objective {
                TutorialObjective::ItemUse {
                    kind: ItemKind::Apple | ItemKind::Turkey,
                    ..
                } => stats.health = MAX_HEALTH * 0.35,
                TutorialObjective::ItemUse {
                    kind: ItemKind::WineWhite | ItemKind::Barrel,
                    ..
                } => stats.stamina = 0.0,
                _ => {}
            }
        }
        *motor = FighterMotor::default();
        motor.facing = if fighter.id == TUTORIAL_PLAYER_ID {
            Vec3::X
        } else {
            Vec3::NEG_X
        };
        *input = FighterInput::default();
        *action = FighterActionState::default();
        *inventory = FighterInventory::default();
        *grab = FighterGrabState::default();
        *special = FighterSpecialState::default();
        *drunk = DrunkStatus::default();
        equipment.cooldown = 0.0;
        if let Some(position) = positions.get(fighter.id) {
            transform.translation = *position;
            fighter_position_floor_fix(&mut transform);
            if fighter.id == TUTORIAL_PLAYER_ID {
                player_position = transform.translation;
            }
        }
        *visibility = if fighter.id <= TUTORIAL_DUMMY_ID {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
    }

    for (entity, fighter, mut brain) in &mut bots {
        if fighter.id != TUTORIAL_DUMMY_ID {
            continue;
        }
        if chapter.final_exam {
            start_bot_combat_ai(&mut brain);
            commands.entity(entity).insert(BotDifficulty::Tutorial);
        } else {
            brain.behavior = BotBehaviorMode::TrainingDummy;
            brain.decision_timer = 0.0;
            brain.movement_plan_timer = 0.0;
            brain.dash_timer = 0.0;
            brain.attack_timer = 0.0;
            commands.entity(entity).remove::<BotDifficulty>();
        }
    }

    let exercise_kind = current_item_objective(&session);
    let exercise_position = Vec3::new(-0.25, ARENA_TOP_Y + 0.55, 0.0);
    let mut baseline_item_durability = 0;
    let mut lesson_item_entity = None;
    let mut item_count = 0;
    for (index, (entity, mut item, mut transform, mut visibility, mut material, mut scene_root)) in
        (&mut items).into_iter().enumerate()
    {
        item_count += 1;
        let target = if chapter.final_exam {
            arena
                .item_anchors
                .get(index)
                .map(|anchor| (anchor.kind, anchor.position, anchor.phase))
        } else if index == 0 {
            exercise_kind.map(|kind| (kind, exercise_position, 0.0))
        } else {
            None
        };
        if let Some((kind, position, phase)) = target {
            item.retarget_for_anchor(kind, position, phase);
            transform.translation = position;
            transform.scale = item_scale(kind);
            material.0 = item_assets.material_for(kind, false);
            scene_root.0 = item_assets.scene_for(kind);
            *visibility = Visibility::Visible;
            if !chapter.final_exam {
                baseline_item_durability = item.durability;
                lesson_item_entity = Some(entity);
            }
        } else {
            item.deactivate_for_match();
            *visibility = Visibility::Hidden;
        }
    }

    if !chapter.final_exam {
        state.stocks = [STOCK_LIVES; FIGHTER_COUNT];
        state.phase = MatchPhase::Fighting;
        state.phase_timer = 0.0;
    }
    session.objective_baseline = TutorialObjectiveBaseline {
        player_position,
        last_player_position: player_position,
        player_damage: telemetry.damage_by_fighter[TUTORIAL_PLAYER_ID],
        ring_outs: telemetry.ring_outs,
        player_stock: state.stocks[TUTORIAL_PLAYER_ID],
        dummy_stock: state.stocks[TUTORIAL_DUMMY_ID],
        item_durability: baseline_item_durability,
        lesson_item_entity,
        item_count,
    };
    session.objective_progress = 0;
    session.objective_latched = false;
    session.item_was_held = false;
    session.reset_completion();
    session.dummy_script_elapsed = 0.0;
    session.reset_requested = false;
    session.phase = TutorialPhase::Prompt;
    pause_owners.set(GameplayPauseOwner::TutorialPrompt, true);
    pause_owners.set(GameplayPauseOwner::TutorialMenu, false);
    pause_owners.set(GameplayPauseOwner::TutorialSuccess, false);
}

fn fighter_position_floor_fix(transform: &mut Transform) {
    transform.translation.y = ARENA_TOP_Y;
}

pub fn script_tutorial_dummy(
    time: Res<Time>,
    state: Res<MatchState>,
    mut session: ResMut<TutorialSession>,
    mut dummy: Query<(
        &Fighter,
        &Controller,
        &FighterActionState,
        &mut FighterInput,
    )>,
) {
    if session.phase != TutorialPhase::Playing || !state.is_fighting() {
        return;
    }
    let Some(step) = session.current_step() else {
        return;
    };
    if step.dummy == ScriptedDummyMode::NormalBot {
        return;
    }
    session.dummy_script_elapsed += time.delta_secs();
    let elapsed = session.dummy_script_elapsed;
    for (fighter, controller, action, mut input) in &mut dummy {
        if fighter.id != TUTORIAL_DUMMY_ID || !controller.is_bot() {
            continue;
        }
        *input = FighterInput::default();
        match step.dummy {
            ScriptedDummyMode::Passive | ScriptedDummyMode::Positioning => {}
            ScriptedDummyMode::Guarding => input.guard = true,
            ScriptedDummyMode::TimedLightAttacks => {
                input.light = scripted_input_pulse(elapsed, 2.2)
                    || matches!(
                        action.action,
                        FighterAction::LightAttack1 | FighterAction::LightAttack2
                    );
            }
            ScriptedDummyMode::TimedHeavyAttacks | ScriptedDummyMode::KnockdownSetup => {
                input.heavy = scripted_input_pulse(elapsed, 2.8);
            }
            ScriptedDummyMode::Grabs => {
                input.grab = scripted_input_pulse(elapsed, 2.5);
            }
            ScriptedDummyMode::NormalBot => {}
        }
    }
}

fn scripted_input_pulse(elapsed: f32, period: f32) -> bool {
    elapsed.rem_euclid(period) < 0.12
}

#[allow(clippy::too_many_arguments)]
pub fn observe_tutorial_objective(
    time: Res<Time>,
    mut session: ResMut<TutorialSession>,
    state: Res<MatchState>,
    telemetry: Res<MatchTelemetry>,
    mut pause_owners: ResMut<GameplayPauseOwners>,
    mut transition: ResMut<TutorialTransition>,
    fighters: Query<(
        &Fighter,
        &FighterInput,
        &FighterActionState,
        &FighterStats,
        &FighterMotor,
        &FighterGrabState,
        &Transform,
        &FighterInventory,
    )>,
    items: Query<(Entity, &ArenaItem)>,
    specials: Query<&ActiveSpecial>,
    effects: Query<&VisualEffect>,
) {
    if session.phase != TutorialPhase::Playing || transition.active() {
        return;
    }
    let Some(chapter) = session.current_chapter() else {
        return;
    };
    let Some(step) = session.current_step() else {
        return;
    };

    if chapter.final_exam && state.phase == MatchPhase::Results {
        let won = tutorial_final_exam_won(&state);
        request_tutorial_transition(
            &mut transition,
            &mut pause_owners,
            TutorialTransitionAction::ShowFinalResult { won },
        );
        return;
    }

    let dummy_state =
        fighters
            .iter()
            .find_map(|(fighter, _, action, _, motor, _, transform, _)| {
                (fighter.id == TUTORIAL_DUMMY_ID).then_some((action, motor, transform.translation))
            });
    let dummy_position = dummy_state.map(|(_, _, position)| position);
    let Some((input, action, _stats, motor, grab, transform, inventory)) =
        fighters.iter().find_map(
            |(fighter, input, action, stats, motor, grab, transform, inventory)| {
                (fighter.id == TUTORIAL_PLAYER_ID)
                    .then_some((input, action, stats, motor, grab, transform, inventory))
            },
        )
    else {
        return;
    };

    let player_lost_stock =
        state.stocks[TUTORIAL_PLAYER_ID] < session.objective_baseline.player_stock;
    let dummy_lost_stock = state.stocks[TUTORIAL_DUMMY_ID] < session.objective_baseline.dummy_stock;
    let unexpected_stock_change = (player_lost_stock
        && step.objective != TutorialObjective::LoseLife)
        || (dummy_lost_stock && step.objective != TutorialObjective::RingOutOpponent);
    if !chapter.final_exam && unexpected_stock_change {
        request_tutorial_transition(
            &mut transition,
            &mut pause_owners,
            TutorialTransitionAction::RestartStep,
        );
        return;
    }

    let evidence_detected = match step.objective {
        TutorialObjective::Knowledge => false,
        TutorialObjective::Movement {
            direction,
            distance,
        } => {
            let displacement = (transform.translation
                - session.objective_baseline.last_player_position)
                .xz()
                .length();
            session.objective_baseline.last_player_position = transform.translation;
            if tutorial_direction_pressed(direction, input.movement) {
                session.objective_progress = session
                    .objective_progress
                    .saturating_add((displacement * 10.0).round() as u32);
            }
            session.objective_progress >= (distance * 10.0).round() as u32
        }
        TutorialObjective::Input(control) => {
            let active = tutorial_control_is_active(control, input);
            if control == ControlAction::AimGrab {
                if active && input.movement.length_squared() > 0.12 {
                    session.completion_hold_elapsed += time.delta_secs();
                } else {
                    session.completion_hold_elapsed = 0.0;
                }
                session.completion_hold_elapsed >= TUTORIAL_AIM_HOLD_SECONDS
            } else {
                active
            }
        }
        TutorialObjective::Action(expected) => action.action == expected,
        TutorialObjective::ActivateTechnique {
            technique,
            confirmed_hit,
        } => action.technique_id == Some(technique) && (!confirmed_hit || action.confirmed_hit),
        TutorialObjective::ConfirmedHits { count } => {
            if latch_event(action.confirmed_hit, &mut session.objective_latched) {
                session.objective_progress = session.objective_progress.saturating_add(1);
            }
            session.objective_progress >= count as u32
        }
        TutorialObjective::Guarding { count } => {
            if latch_event(
                motor.guard_counter_window_timer > 0.0,
                &mut session.objective_latched,
            ) {
                session.objective_progress = session.objective_progress.saturating_add(1);
            }
            session.objective_progress >= count as u32
        }
        TutorialObjective::GrabEscape => {
            let grabbed = action.action == FighterAction::Grabbed || grab.held_by.is_some();
            if grabbed {
                session.objective_latched = true;
                false
            } else if session.objective_latched && input.guard {
                dummy_position.is_some_and(|dummy_position| {
                    let away = Vec2::new(
                        transform.translation.x - dummy_position.x,
                        transform.translation.z - dummy_position.z,
                    )
                    .normalize_or_zero();
                    input.movement.normalize_or_zero().dot(away) > 0.25
                })
            } else {
                false
            }
        }
        TutorialObjective::Recovery(technique) => action.technique_id == Some(technique),
        TutorialObjective::ItemUse { kind, uses } => {
            let relevant = items.iter().find(|(_, item)| item.kind == kind);
            if let Some((entity, item)) = relevant {
                session.item_was_held |= inventory.held == Some(entity);
                let consumed = session
                    .objective_baseline
                    .item_durability
                    .saturating_sub(item.durability)
                    .max(0) as u32;
                session.objective_progress = consumed;
            }
            session.item_was_held && session.objective_progress >= uses as u32
        }
        TutorialObjective::ItemThrow(kind) => items.iter().any(|(entity, item)| {
            session.item_was_held |= inventory.held == Some(entity) && item.kind == kind;
            item.kind == kind
                && session.item_was_held
                && matches!(
                    item.state,
                    ItemState::Thrown { .. } | ItemState::Armed { .. } | ItemState::Spraying { .. }
                )
        }),
        TutorialObjective::SpecialSpawn(kind) => specials
            .iter()
            .any(|special| special.owner_id == TUTORIAL_PLAYER_ID && special.kind == kind),
        TutorialObjective::RingOutOpponent => {
            telemetry.ring_outs > session.objective_baseline.ring_outs || dummy_lost_stock
        }
        TutorialObjective::LoseLife => player_lost_stock,
        TutorialObjective::MatchResult { win } => {
            state.phase == MatchPhase::Results
                && ((state.stocks[TUTORIAL_PLAYER_ID] > state.stocks[TUTORIAL_DUMMY_ID]) == win)
        }
    };

    if session.completion_state == TutorialCompletionState::Observing && evidence_detected {
        session.completion_state = TutorialCompletionState::Settling;
        session.completion_stable_elapsed = 0.0;
        session.completion_saw_airborne = !motor.grounded;
        session.completion_world_effect_seen = false;
    }

    if session.completion_state != TutorialCompletionState::Settling {
        return;
    }

    session.completion_saw_airborne |= !motor.grounded;
    match step.objective {
        TutorialObjective::ItemThrow(ItemKind::Barrel) => {
            session.completion_world_effect_seen |= effects
                .iter()
                .any(|effect| effect.kind == EffectKind::AlcoholSpray);
        }
        TutorialObjective::ItemThrow(ItemKind::Crate) => {
            let lesson_item_finished = session
                .objective_baseline
                .lesson_item_entity
                .and_then(|entity| items.get(entity).ok())
                .is_some_and(|(_, item)| matches!(item.state, ItemState::Respawning));
            let reward_revealed = items.iter().count() > session.objective_baseline.item_count
                && items.iter().any(|(entity, item)| {
                    Some(entity) != session.objective_baseline.lesson_item_entity
                        && !matches!(item.state, ItemState::Respawning)
                });
            session.completion_world_effect_seen |= lesson_item_finished && reward_revealed;
        }
        TutorialObjective::ItemThrow(ItemKind::Steamer) => {
            session.completion_world_effect_seen |= effects
                .iter()
                .any(|effect| effect.kind == EffectKind::PopBombBlast);
        }
        TutorialObjective::ItemThrow(_) => {
            session.completion_world_effect_seen = true;
        }
        TutorialObjective::SpecialSpawn(kind) => {
            session.completion_world_effect_seen |= specials.iter().any(|special| {
                tutorial_special_activation_matches(
                    special.owner_id,
                    special.kind,
                    special.active_feedback_sent,
                    kind,
                )
            });
        }
        _ => {}
    }

    let player_recovered = tutorial_fighter_has_recovered(action, motor);
    let settled = match step.objective {
        TutorialObjective::Knowledge | TutorialObjective::MatchResult { .. } => false,
        TutorialObjective::Movement { .. } => {
            input.movement.length_squared() <= 0.04 && player_recovered
        }
        TutorialObjective::Input(control) => {
            !tutorial_control_is_active(control, input) && player_recovered
        }
        TutorialObjective::Action(FighterAction::Jumping) => {
            session.completion_saw_airborne && player_recovered
        }
        TutorialObjective::Action(_) => player_recovered,
        TutorialObjective::ActivateTechnique { .. }
        | TutorialObjective::ConfirmedHits { .. }
        | TutorialObjective::Recovery(_)
        | TutorialObjective::ItemUse { .. } => player_recovered,
        TutorialObjective::Guarding { .. } => {
            !input.guard && action.action != FighterAction::Guarding && player_recovered
        }
        TutorialObjective::GrabEscape => {
            grab.held_by.is_none() && action.action != FighterAction::Grabbed && player_recovered
        }
        TutorialObjective::ItemThrow(_) | TutorialObjective::SpecialSpawn(_) => {
            player_recovered && session.completion_world_effect_seen
        }
        TutorialObjective::RingOutOpponent => dummy_state
            .is_some_and(|(action, motor, _)| tutorial_fighter_has_recovered(action, motor)),
        TutorialObjective::LoseLife => player_recovered,
    };

    if settled {
        session.completion_stable_elapsed += time.delta_secs();
    } else {
        session.completion_stable_elapsed = 0.0;
    }
    if session.completion_stable_elapsed >= TUTORIAL_SETTLE_STABLE_SECONDS {
        session.completion_state = TutorialCompletionState::Succeeded;
        session.success_elapsed = 0.0;
        session.phase = TutorialPhase::Success;
        pause_owners.set(GameplayPauseOwner::TutorialSuccess, true);
    }
}

fn tutorial_fighter_has_recovered(action: &FighterActionState, motor: &FighterMotor) -> bool {
    motor.grounded
        && motor.velocity.y.abs() <= 0.05
        && motor.dash_slide_timer <= 0.0
        && matches!(action.action, FighterAction::Idle | FighterAction::Moving)
}

fn tutorial_special_activation_matches(
    owner_id: usize,
    actual_kind: SpecialKind,
    active_feedback_sent: bool,
    expected_kind: SpecialKind,
) -> bool {
    owner_id == TUTORIAL_PLAYER_ID && actual_kind == expected_kind && active_feedback_sent
}

fn tutorial_final_exam_won(state: &MatchState) -> bool {
    state.stocks[TUTORIAL_PLAYER_ID] > 0 && state.stocks[TUTORIAL_DUMMY_ID] <= 0
}

fn tutorial_direction_pressed(direction: TutorialDirection, movement: Vec2) -> bool {
    match direction {
        TutorialDirection::Left => movement.x < -0.35,
        TutorialDirection::Right => movement.x > 0.35,
        TutorialDirection::Forward => movement.y > 0.35,
        TutorialDirection::Back => movement.y < -0.35,
        TutorialDirection::Any => movement.length_squared() > 0.12,
    }
}

fn tutorial_control_is_active(control: ControlAction, input: &FighterInput) -> bool {
    match control {
        ControlAction::Left => input.movement.x < -0.35,
        ControlAction::Right => input.movement.x > 0.35,
        ControlAction::Up => input.movement.y > 0.35,
        ControlAction::Down => input.movement.y < -0.35,
        ControlAction::AimGrab => input.aim,
        ControlAction::Heavy => input.heavy || input.heavy_held,
        ControlAction::Light => input.light || input.light_held,
        ControlAction::Jump => input.jump,
        ControlAction::Special => input.special,
    }
}

fn latch_event(active: bool, latched: &mut bool) -> bool {
    if active && !*latched {
        *latched = true;
        true
    } else if !active {
        *latched = false;
        false
    } else {
        false
    }
}

#[derive(Component)]
pub struct TutorialUiRoot;

#[derive(Component)]
pub(crate) struct TutorialHubPanel;

#[derive(Component)]
pub(crate) struct TutorialPromptPanel;

#[derive(Component)]
pub(crate) struct TutorialObjectivePanel;

#[derive(Component)]
pub(crate) struct TutorialSuccessPanel;

#[derive(Component)]
pub(crate) struct TutorialPausePanel;

#[derive(Component)]
pub(crate) struct TutorialCompletePanel;

#[derive(Component)]
pub(crate) struct TutorialFinalPanel;

#[derive(Component)]
pub(crate) struct TutorialResetPanel;

#[derive(Component)]
pub(crate) struct TutorialFadeOverlay;

#[derive(Component)]
pub(crate) struct TutorialHubSummaryText;

#[derive(Component)]
pub(crate) struct TutorialChapterButtonText {
    chapter: TutorialChapterId,
}

#[derive(Component)]
pub(crate) struct TutorialPromptTitleText;

#[derive(Component)]
pub(crate) struct TutorialPromptInstructionText;

#[derive(Component)]
pub(crate) struct TutorialPromptControlText;

#[derive(Component)]
pub(crate) struct TutorialPromptHintText;

#[derive(Component)]
pub(crate) struct TutorialObjectiveTitleText;

#[derive(Component)]
pub(crate) struct TutorialObjectiveProgressText;

#[derive(Component)]
pub(crate) struct TutorialObjectiveControlText;

#[derive(Component)]
pub(crate) struct TutorialObjectiveHintText;

#[derive(Component)]
pub(crate) struct TutorialSuccessDetailText;

#[derive(Component)]
pub(crate) struct TutorialSuccessControlText;

#[derive(Component)]
pub(crate) struct TutorialCompleteText;

#[derive(Component)]
pub(crate) struct TutorialFinalText;

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub enum TutorialUiAction {
    Chapter(TutorialChapterId),
    ResetProgress,
    ConfirmReset,
    CancelReset,
    Back,
    Continue,
    Resume,
    RestartStep,
    SkipStep,
    ExitToHub,
    FinalRetry,
    FinalHub,
    FinalSkip,
}

fn tutorial_button(
    label: impl Into<String>,
    action: TutorialUiAction,
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
            padding: UiRect::axes(Val::Px(14.0), Val::Px(6.0)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.045, 0.05, 0.065, 0.96)),
        BorderColor::all(Color::srgb(0.39, 0.43, 0.48)),
        children![(
            Text::new(label),
            TextFont {
                font_size,
                ..default()
            },
            TextColor(Color::srgb(0.94, 0.88, 0.72)),
            TextLayout::new_with_justify(Justify::Center),
            TextShadow::default(),
        )],
    )
}

fn tutorial_modal_node(display: Display, width: f32) -> Node {
    Node {
        display,
        width: Val::Px(width),
        max_width: Val::Percent(92.0),
        flex_direction: FlexDirection::Column,
        justify_content: JustifyContent::Center,
        align_items: AlignItems::Center,
        row_gap: Val::Px(15.0),
        border: UiRect::all(Val::Px(3.0)),
        padding: UiRect::all(Val::Px(26.0)),
        ..default()
    }
}

pub fn setup_tutorial_ui(mut commands: Commands, ui_cameras: Query<Entity, With<UiCamera>>) {
    let mut root = commands.spawn((
        TutorialUiRoot,
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
        GlobalZIndex(120),
        Pickable::IGNORE,
    ));
    if let Some(camera) = ui_cameras.iter().next() {
        root.insert(UiTargetCamera(camera));
    }
    root.with_children(|root| {
        root.spawn((
            TutorialHubPanel,
            Node {
                display: Display::None,
                width: Val::Percent(94.0),
                height: Val::Percent(96.0),
                max_width: Val::Px(1120.0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                row_gap: Val::Px(10.0),
                padding: UiRect::axes(Val::Px(24.0), Val::Px(14.0)),
                ..default()
            },
            Pickable::IGNORE,
        ))
        .with_children(|hub| {
            hub.spawn((
                Text::new("TUTORIAL"),
                TextFont {
                    font_size: 44.0,
                    ..default()
                },
                TextColor(Color::srgb(0.98, 0.86, 0.58)),
                TextShadow::default(),
            ));
            hub.spawn((
                TutorialHubSummaryText,
                Text::new(""),
                TextFont {
                    font_size: 17.0,
                    ..default()
                },
                TextColor(Color::srgb(0.74, 0.78, 0.82)),
                TextLayout::new_with_justify(Justify::Center),
            ));
            hub.spawn((
                Node {
                    width: Val::Percent(100.0),
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(8.0),
                    ..default()
                },
                Pickable::IGNORE,
            ))
            .with_children(|grid| {
                for row in 0..6 {
                    grid.spawn((
                        Node {
                            width: Val::Percent(100.0),
                            height: Val::Px(66.0),
                            flex_direction: FlexDirection::Row,
                            column_gap: Val::Px(10.0),
                            ..default()
                        },
                        Pickable::IGNORE,
                    ))
                    .with_children(|row_node| {
                        for column in 0..2 {
                            let chapter =
                                &TUTORIAL_CHAPTERS[(row * 2 + column).min(11)];
                            row_node
                                .spawn(tutorial_button(
                                    "",
                                    TutorialUiAction::Chapter(chapter.id),
                                    Val::Percent(50.0),
                                    66.0,
                                    18.0,
                                ))
                                .with_child((
                                    TutorialChapterButtonText {
                                        chapter: chapter.id,
                                    },
                                    Text::new(""),
                                    TextFont {
                                        font_size: 18.0,
                                        ..default()
                                    },
                                    TextColor(Color::srgb(0.94, 0.88, 0.72)),
                                    TextLayout::new_with_justify(Justify::Center),
                                ));
                        }
                    });
                }
            });
            hub.spawn((
                Node {
                    flex_direction: FlexDirection::Row,
                    column_gap: Val::Px(12.0),
                    ..default()
                },
                Pickable::IGNORE,
                children![
                    tutorial_button(
                        "RESET PROGRESS",
                        TutorialUiAction::ResetProgress,
                        Val::Px(210.0),
                        44.0,
                        16.0,
                    ),
                    tutorial_button(
                        "BACK",
                        TutorialUiAction::Back,
                        Val::Px(150.0),
                        44.0,
                        16.0,
                    ),
                ],
            ));
        });

        root.spawn((
            TutorialPromptPanel,
            tutorial_modal_node(Display::None, 700.0),
            BackgroundColor(Color::srgba(0.025, 0.03, 0.045, 0.97)),
            BorderColor::all(Color::srgb(0.84, 0.66, 0.3)),
            children![
                (
                    TutorialPromptTitleText,
                    Text::new(""),
                    TextFont {
                        font_size: 32.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.98, 0.86, 0.58)),
                    TextLayout::new_with_justify(Justify::Center),
                ),
                (
                    TutorialPromptInstructionText,
                    Text::new(""),
                    TextFont {
                        font_size: 21.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.94, 0.93, 0.9)),
                    TextLayout::new_with_justify(Justify::Center),
                ),
                (
                    TutorialPromptControlText,
                    Text::new(""),
                    TextFont {
                        font_size: 20.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.52, 0.86, 0.98)),
                    TextLayout::new_with_justify(Justify::Center),
                ),
                (
                    TutorialPromptHintText,
                    Text::new(""),
                    TextFont {
                        font_size: 17.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.72, 0.76, 0.8)),
                    TextLayout::new_with_justify(Justify::Center),
                ),
                tutorial_button(
                    "CONTINUE",
                    TutorialUiAction::Continue,
                    Val::Px(230.0),
                    48.0,
                    18.0,
                ),
            ],
        ));

        root.spawn((
            TutorialObjectivePanel,
            Node {
                display: Display::None,
                position_type: PositionType::Absolute,
                top: Val::Px(18.0),
                width: Val::Px(680.0),
                max_width: Val::Percent(90.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(4.0),
                border: UiRect::all(Val::Px(2.0)),
                padding: UiRect::axes(Val::Px(20.0), Val::Px(10.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.025, 0.03, 0.045, 0.86)),
            BorderColor::all(Color::srgb(0.34, 0.62, 0.75)),
            Pickable::IGNORE,
            children![
                (
                    TutorialObjectiveTitleText,
                    Text::new(""),
                    TextFont {
                        font_size: 20.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.98, 0.86, 0.58)),
                    TextLayout::new_with_justify(Justify::Center),
                ),
                (
                    TutorialObjectiveProgressText,
                    Text::new(""),
                    TextFont {
                        font_size: 17.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.92, 0.94, 0.96)),
                    TextLayout::new_with_justify(Justify::Center),
                ),
                (
                    TutorialObjectiveControlText,
                    Text::new(""),
                    TextFont {
                        font_size: 16.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.52, 0.86, 0.98)),
                    TextLayout::new_with_justify(Justify::Center),
                ),
                (
                    TutorialObjectiveHintText,
                    Text::new(""),
                    TextFont {
                        font_size: 14.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.72, 0.76, 0.8)),
                    TextLayout::new_with_justify(Justify::Center),
                ),
            ],
        ));

        root.spawn((
            TutorialSuccessPanel,
            tutorial_modal_node(Display::None, 500.0),
            UiTransform::from_scale(Vec2::splat(0.94)),
            BackgroundColor(Color::srgba(0.02, 0.075, 0.045, 0.98)),
            BorderColor::all(Color::srgb(0.35, 0.92, 0.48)),
        ))
        .with_children(|success| {
            success
                .spawn((
                    Node {
                        width: Val::Px(76.0),
                        height: Val::Px(62.0),
                        position_type: PositionType::Relative,
                        ..default()
                    },
                    Pickable::IGNORE,
                ))
                .with_children(|flag| {
                    flag.spawn((
                        Node {
                            position_type: PositionType::Absolute,
                            left: Val::Px(12.0),
                            top: Val::Px(3.0),
                            width: Val::Px(5.0),
                            height: Val::Px(56.0),
                            ..default()
                        },
                        BackgroundColor(Color::srgb(0.76, 0.84, 0.72)),
                        Pickable::IGNORE,
                    ));
                    flag.spawn((
                        Node {
                            position_type: PositionType::Absolute,
                            left: Val::Px(17.0),
                            top: Val::Px(6.0),
                            width: Val::Px(47.0),
                            height: Val::Px(28.0),
                            border: UiRect::all(Val::Px(2.0)),
                            ..default()
                        },
                        BackgroundColor(Color::srgb(0.12, 0.72, 0.28)),
                        BorderColor::all(Color::srgb(0.55, 1.0, 0.63)),
                        Pickable::IGNORE,
                    ));
                    flag.spawn((
                        Node {
                            position_type: PositionType::Absolute,
                            left: Val::Px(5.0),
                            top: Val::Px(56.0),
                            width: Val::Px(20.0),
                            height: Val::Px(4.0),
                            ..default()
                        },
                        BackgroundColor(Color::srgb(0.76, 0.84, 0.72)),
                        Pickable::IGNORE,
                    ));
                });
            success.spawn((
                Text::new("GOOD JOB!"),
                TextFont {
                    font_size: 36.0,
                    ..default()
                },
                TextColor(Color::srgb(0.68, 1.0, 0.72)),
                TextLayout::new_with_justify(Justify::Center),
                TextShadow::default(),
            ));
            success.spawn((
                TutorialSuccessDetailText,
                Text::new(""),
                TextFont {
                    font_size: 22.0,
                    ..default()
                },
                TextColor(Color::srgb(0.92, 0.98, 0.9)),
                TextLayout::new_with_justify(Justify::Center),
            ));
            success.spawn((
                TutorialSuccessControlText,
                Text::new(""),
                TextFont {
                    font_size: 16.0,
                    ..default()
                },
                TextColor(Color::srgb(0.64, 0.9, 0.72)),
                TextLayout::new_with_justify(Justify::Center),
            ));
            success.spawn(tutorial_button(
                "NEXT",
                TutorialUiAction::Continue,
                Val::Px(190.0),
                44.0,
                17.0,
            ));
        });

        root.spawn((
            TutorialPausePanel,
            tutorial_modal_node(Display::None, 520.0),
            BackgroundColor(Color::srgba(0.025, 0.03, 0.045, 0.98)),
            BorderColor::all(Color::srgb(0.84, 0.66, 0.3)),
            children![
                (
                    Text::new("TUTORIAL PAUSED"),
                    TextFont {
                        font_size: 34.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.98, 0.86, 0.58)),
                ),
                tutorial_button(
                    "RESUME",
                    TutorialUiAction::Resume,
                    Val::Px(320.0),
                    48.0,
                    18.0,
                ),
                tutorial_button(
                    "RESTART STEP",
                    TutorialUiAction::RestartStep,
                    Val::Px(320.0),
                    48.0,
                    18.0,
                ),
                tutorial_button(
                    "SKIP STEP",
                    TutorialUiAction::SkipStep,
                    Val::Px(320.0),
                    48.0,
                    18.0,
                ),
                tutorial_button(
                    "EXIT TO HUB",
                    TutorialUiAction::ExitToHub,
                    Val::Px(320.0),
                    48.0,
                    18.0,
                ),
            ],
        ));

        root.spawn((
            TutorialCompletePanel,
            tutorial_modal_node(Display::None, 620.0),
            BackgroundColor(Color::srgba(0.025, 0.03, 0.045, 0.98)),
            BorderColor::all(Color::srgb(0.46, 0.84, 0.52)),
            children![
                (
                    TutorialCompleteText,
                    Text::new(""),
                    TextFont {
                        font_size: 28.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.9, 0.96, 0.82)),
                    TextLayout::new_with_justify(Justify::Center),
                ),
                tutorial_button(
                    "RETURN TO HUB",
                    TutorialUiAction::ExitToHub,
                    Val::Px(270.0),
                    48.0,
                    18.0,
                ),
            ],
        ));

        root.spawn((
            TutorialFinalPanel,
            tutorial_modal_node(Display::None, 620.0),
            BackgroundColor(Color::srgba(0.025, 0.03, 0.045, 0.98)),
            BorderColor::all(Color::srgb(0.84, 0.66, 0.3)),
            children![
                (
                    TutorialFinalText,
                    Text::new(""),
                    TextFont {
                        font_size: 30.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.98, 0.86, 0.58)),
                    TextLayout::new_with_justify(Justify::Center),
                ),
                tutorial_button(
                    "RETRY",
                    TutorialUiAction::FinalRetry,
                    Val::Px(300.0),
                    48.0,
                    18.0,
                ),
                tutorial_button(
                    "RETURN TO HUB",
                    TutorialUiAction::FinalHub,
                    Val::Px(300.0),
                    48.0,
                    18.0,
                ),
                tutorial_button(
                    "SKIP",
                    TutorialUiAction::FinalSkip,
                    Val::Px(300.0),
                    48.0,
                    18.0,
                ),
            ],
        ));

        root.spawn((
            TutorialResetPanel,
            tutorial_modal_node(Display::None, 560.0),
            GlobalZIndex(140),
            BackgroundColor(Color::srgba(0.025, 0.03, 0.045, 0.99)),
            BorderColor::all(Color::srgb(0.88, 0.42, 0.32)),
            children![
                (
                    Text::new(
                        "RESET TUTORIAL PROGRESS?\n\nVisited and Complete marks will be cleared. Controls and device settings are preserved.",
                    ),
                    TextFont {
                        font_size: 20.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.94, 0.9, 0.84)),
                    TextLayout::new_with_justify(Justify::Center),
                ),
                tutorial_button(
                    "CONFIRM RESET",
                    TutorialUiAction::ConfirmReset,
                    Val::Px(270.0),
                    48.0,
                    18.0,
                ),
                tutorial_button(
                    "CANCEL",
                    TutorialUiAction::CancelReset,
                    Val::Px(270.0),
                    48.0,
                    18.0,
                ),
            ],
        ));

        root.spawn((
            TutorialFadeOverlay,
            Node {
                display: Display::None,
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
            GlobalZIndex(1000),
            BackgroundColor(Color::srgba(0.006, 0.006, 0.012, 0.0)),
            Pickable {
                should_block_lower: true,
                is_hoverable: false,
            },
        ));
    });
}

pub fn sync_tutorial_ui_camera(
    mut commands: Commands,
    roots: Query<Entity, (With<TutorialUiRoot>, Without<UiTargetCamera>)>,
    ui_cameras: Query<Entity, With<UiCamera>>,
) {
    let Some(camera) = ui_cameras.iter().next() else {
        return;
    };
    for root in &roots {
        commands.entity(root).insert(UiTargetCamera(camera));
    }
}

#[allow(clippy::type_complexity)]
pub fn update_tutorial_ui(
    user_mode: Res<UserModeState>,
    session: Res<TutorialSession>,
    transition: Res<TutorialTransition>,
    progress: Res<TutorialProgress>,
    bindings: Res<PlayerKeyBindings>,
    controller_metadata: Query<&ControllerDeviceInfo>,
    mut roots: Query<
        &mut Node,
        (
            With<TutorialUiRoot>,
            Without<Button>,
            Without<TutorialFadeOverlay>,
        ),
    >,
    mut panels: Query<
        (
            &mut Node,
            Option<&TutorialHubPanel>,
            Option<&TutorialPromptPanel>,
            Option<&TutorialObjectivePanel>,
            Option<&TutorialPausePanel>,
            Option<&TutorialCompletePanel>,
            Option<&TutorialFinalPanel>,
            Option<&TutorialResetPanel>,
        ),
        (
            Without<TutorialUiRoot>,
            Without<Button>,
            Without<TutorialFadeOverlay>,
            Without<TutorialSuccessPanel>,
            Or<(
                With<TutorialHubPanel>,
                With<TutorialPromptPanel>,
                With<TutorialObjectivePanel>,
                With<TutorialPausePanel>,
                With<TutorialCompletePanel>,
                With<TutorialFinalPanel>,
                With<TutorialResetPanel>,
            )>,
        ),
    >,
    mut success_panels: Query<
        (&mut Node, &mut UiTransform),
        (
            With<TutorialSuccessPanel>,
            Without<TutorialUiRoot>,
            Without<Button>,
            Without<TutorialFadeOverlay>,
        ),
    >,
    mut texts: Query<
        (
            &mut Text,
            Option<&TutorialHubSummaryText>,
            Option<&TutorialChapterButtonText>,
            Option<&TutorialPromptTitleText>,
            Option<&TutorialPromptInstructionText>,
            Option<&TutorialPromptControlText>,
            Option<&TutorialPromptHintText>,
            Option<&TutorialObjectiveTitleText>,
            Option<&TutorialObjectiveProgressText>,
            Option<&TutorialObjectiveControlText>,
            Option<&TutorialObjectiveHintText>,
            Option<&TutorialSuccessDetailText>,
            Option<&TutorialSuccessControlText>,
            Option<&TutorialCompleteText>,
            Option<&TutorialFinalText>,
        ),
        Or<(
            With<TutorialHubSummaryText>,
            With<TutorialChapterButtonText>,
            With<TutorialPromptTitleText>,
            With<TutorialPromptInstructionText>,
            With<TutorialPromptControlText>,
            With<TutorialPromptHintText>,
            With<TutorialObjectiveTitleText>,
            With<TutorialObjectiveProgressText>,
            With<TutorialObjectiveControlText>,
            With<TutorialObjectiveHintText>,
            With<TutorialSuccessDetailText>,
            With<TutorialSuccessControlText>,
            With<TutorialCompleteText>,
            With<TutorialFinalText>,
        )>,
    >,
    mut final_buttons: Query<
        (&TutorialUiAction, &mut Node),
        (
            With<Button>,
            Without<TutorialUiRoot>,
            Without<TutorialHubPanel>,
            Without<TutorialPromptPanel>,
            Without<TutorialObjectivePanel>,
            Without<TutorialPausePanel>,
            Without<TutorialCompletePanel>,
            Without<TutorialFinalPanel>,
            Without<TutorialResetPanel>,
        ),
    >,
    mut fade_overlays: Query<
        (&mut Node, &mut BackgroundColor),
        (
            With<TutorialFadeOverlay>,
            Without<TutorialUiRoot>,
            Without<Button>,
        ),
    >,
) {
    let tutorial_visible = user_mode.tutorial_screen_active() || transition.active();
    for mut node in &mut roots {
        node.display = if tutorial_visible {
            Display::Flex
        } else {
            Display::None
        };
    }

    let fade_alpha = transition.alpha();
    for (mut node, mut background) in &mut fade_overlays {
        node.display = if transition.active() {
            Display::Flex
        } else {
            Display::None
        };
        *background = BackgroundColor(Color::srgba(0.006, 0.006, 0.012, fade_alpha));
    }

    for (mut node, hub, prompt, objective, pause, complete, final_result, reset) in &mut panels {
        let visible = (hub.is_some() && user_mode.screen() == UserModeScreen::TutorialHub)
            || (prompt.is_some()
                && user_mode.screen() == UserModeScreen::TutorialLesson
                && session.phase == TutorialPhase::Prompt)
            || (objective.is_some()
                && user_mode.screen() == UserModeScreen::TutorialLesson
                && session.phase == TutorialPhase::Playing)
            || (pause.is_some()
                && user_mode.screen() == UserModeScreen::TutorialPause
                && session.phase == TutorialPhase::PauseMenu)
            || (complete.is_some() && session.phase == TutorialPhase::ChapterComplete)
            || (final_result.is_some()
                && user_mode.screen() == UserModeScreen::TutorialFinalResult
                && session.phase == TutorialPhase::FinalResult)
            || (reset.is_some()
                && user_mode.screen() == UserModeScreen::TutorialHub
                && session.reset_confirmation);
        node.display = if visible {
            Display::Flex
        } else {
            Display::None
        };
    }

    let success_visible = user_mode.screen() == UserModeScreen::TutorialLesson
        && session.phase == TutorialPhase::Success;
    let success_reveal =
        tutorial_fade_ease(session.success_elapsed / TUTORIAL_SUCCESS_REVEAL_SECONDS);
    for (mut node, mut transform) in &mut success_panels {
        node.display = if success_visible {
            Display::Flex
        } else {
            Display::None
        };
        transform.scale = Vec2::splat(0.94 + success_reveal * 0.06);
    }

    let selected_chapter = TUTORIAL_CHAPTERS[session.hub_cursor.min(11)];
    let step = session.current_step();
    let assignment = user_mode.tutorial_player_assignment();
    let controls = step
        .map(|step| tutorial_control_prompt(step, assignment, &bindings, &controller_metadata))
        .unwrap_or_default();
    for (
        mut text,
        hub_summary,
        chapter_button,
        prompt_title,
        prompt_instruction,
        prompt_control,
        prompt_hint,
        objective_title,
        objective_progress,
        objective_control,
        objective_hint,
        success_detail,
        success_control,
        complete,
        final_result,
    ) in &mut texts
    {
        if hub_summary.is_some() {
            **text = format!(
                "{}\n\nAll 12 chapters are unlocked and replayable. Move in two columns; confirm to train. R resets tutorial marks only.",
                selected_chapter.summary
            );
        } else if let Some(chapter_button) = chapter_button {
            let chapter = tutorial_chapter(chapter_button.chapter);
            **text = format!(
                "{:02}. {}  [{}]",
                chapter.number,
                chapter.title,
                progress.status(chapter.id).label()
            );
        } else if prompt_title.is_some() {
            **text = session
                .current_chapter()
                .zip(step)
                .map(|(chapter, step)| {
                    format!(
                        "{}  |  STEP {} / {}\n{}",
                        chapter.title,
                        session.step_index + 1,
                        chapter.steps.len(),
                        step.title
                    )
                })
                .unwrap_or_default();
        } else if prompt_instruction.is_some() {
            **text = step
                .map(|step| step.instruction)
                .unwrap_or_default()
                .to_string();
        } else if prompt_control.is_some() {
            **text = if controls.is_empty() {
                "Confirm when ready".to_string()
            } else {
                format!("CONTROL  {controls}")
            };
        } else if prompt_hint.is_some() {
            **text = step
                .map(|step| {
                    if session.stronger_hint_active() {
                        format!("STRONGER HINT  {}", step.strong_hint)
                    } else {
                        format!("HINT  {}", step.hint)
                    }
                })
                .unwrap_or_default();
        } else if objective_title.is_some() {
            **text = step
                .map(|step| format!("OBJECTIVE  {}", step.title))
                .unwrap_or_default();
        } else if objective_progress.is_some() {
            let progress_text = tutorial_objective_progress_text(&session);
            **text = step
                .map(|step| {
                    if let Some(status) = tutorial_settlement_status(&session) {
                        if progress_text.is_empty() {
                            format!("{}  |  {status}", step.instruction)
                        } else {
                            format!("{}  |  {progress_text}  |  {status}", step.instruction)
                        }
                    } else if progress_text.is_empty() {
                        step.instruction.to_string()
                    } else {
                        format!("{}  |  {progress_text}", step.instruction)
                    }
                })
                .unwrap_or_default();
        } else if objective_control.is_some() {
            **text = if controls.is_empty() {
                String::new()
            } else {
                format!("CONTROL  {controls}")
            };
        } else if objective_hint.is_some() {
            **text = step
                .map(|step| {
                    if session.stronger_hint_active() {
                        step.strong_hint
                    } else {
                        step.hint
                    }
                })
                .unwrap_or_default()
                .to_string();
        } else if success_detail.is_some() {
            **text = step
                .map(|step| format!("{} COMPLETE", step.title.to_uppercase()))
                .unwrap_or_default();
        } else if success_control.is_some() {
            let confirm = tutorial_action_label(
                TutorialPromptAction::Confirm,
                assignment,
                &bindings,
                &controller_metadata,
            );
            **text = format!("{confirm}  NEXT  |  AUTO-CONTINUE");
        } else if complete.is_some() {
            **text = session
                .current_chapter()
                .map(|chapter| {
                    if session.chapter_can_complete() {
                        format!("{} COMPLETE\nProgress saved.", chapter.title)
                    } else {
                        format!(
                            "{} FINISHED\nSkipped steps allow progression but do not award a completion mark.",
                            chapter.title
                        )
                    }
                })
                .unwrap_or_default();
        } else if final_result.is_some() {
            **text = if session.final_exam_won == Some(true) {
                "FINAL EXAM COMPLETE\nYou won the standard three-life match.".to_string()
            } else {
                "FINAL EXAM: RETRY?\nChoose Retry, Return to Hub, or Skip.".to_string()
            };
        }
    }

    let final_won = session.final_exam_won == Some(true);
    for (action, mut node) in &mut final_buttons {
        if matches!(
            action,
            TutorialUiAction::FinalRetry | TutorialUiAction::FinalHub | TutorialUiAction::FinalSkip
        ) {
            node.display = if final_won && !matches!(action, TutorialUiAction::FinalHub) {
                Display::None
            } else {
                Display::Flex
            };
        }
    }
}

fn tutorial_action_selected(session: &TutorialSession, action: TutorialUiAction) -> bool {
    match action {
        TutorialUiAction::Chapter(chapter) => {
            session.hub_cursor == chapter_index(chapter) && !session.reset_confirmation
        }
        TutorialUiAction::Continue => matches!(
            session.phase,
            TutorialPhase::Prompt | TutorialPhase::Success
        ),
        TutorialUiAction::Resume => {
            session.phase == TutorialPhase::PauseMenu && session.pause_cursor == 0
        }
        TutorialUiAction::RestartStep => {
            session.phase == TutorialPhase::PauseMenu && session.pause_cursor == 1
        }
        TutorialUiAction::SkipStep => {
            session.phase == TutorialPhase::PauseMenu && session.pause_cursor == 2
        }
        TutorialUiAction::ExitToHub => {
            (session.phase == TutorialPhase::PauseMenu && session.pause_cursor == 3)
                || session.phase == TutorialPhase::ChapterComplete
        }
        TutorialUiAction::FinalRetry => {
            session.phase == TutorialPhase::FinalResult
                && session.final_exam_won != Some(true)
                && session.final_cursor == 0
        }
        TutorialUiAction::FinalHub => {
            session.phase == TutorialPhase::FinalResult
                && (session.final_exam_won == Some(true) || session.final_cursor == 1)
        }
        TutorialUiAction::FinalSkip => {
            session.phase == TutorialPhase::FinalResult
                && session.final_exam_won != Some(true)
                && session.final_cursor == 2
        }
        TutorialUiAction::ConfirmReset => session.reset_confirmation,
        _ => false,
    }
}

pub fn update_tutorial_button_styles(
    session: Res<TutorialSession>,
    mut buttons: Query<
        (
            &Interaction,
            &TutorialUiAction,
            &mut BackgroundColor,
            &mut BorderColor,
        ),
        With<Button>,
    >,
) {
    for (interaction, action, mut background, mut border) in &mut buttons {
        let selected = tutorial_action_selected(&session, *action);
        let (background_color, border_color) = match interaction {
            Interaction::Pressed => (Color::srgb(0.38, 0.28, 0.1), Color::srgb(1.0, 0.86, 0.48)),
            Interaction::Hovered => (Color::srgb(0.16, 0.18, 0.19), Color::srgb(0.54, 0.84, 0.96)),
            Interaction::None if selected => {
                (Color::srgb(0.12, 0.15, 0.17), Color::srgb(0.38, 0.8, 0.96))
            }
            Interaction::None => (
                Color::srgba(0.045, 0.05, 0.065, 0.96),
                Color::srgb(0.39, 0.43, 0.48),
            ),
        };
        *background = BackgroundColor(background_color);
        *border = BorderColor::all(border_color);
    }
}

#[derive(Default)]
pub struct TutorialNavigationLatch {
    direction: IVec2,
}

#[derive(Clone, Copy, Debug, Default)]
struct TutorialMenuInput {
    direction: IVec2,
    confirm: bool,
    back: bool,
    menu: bool,
}

#[derive(SystemParam)]
pub struct TutorialInputDevices<'w, 's> {
    keys: Res<'w, ButtonInput<KeyCode>>,
    gamepads: Query<'w, 's, (Entity, &'static Gamepad)>,
    controller_metadata: Query<'w, 's, &'static ControllerDeviceInfo>,
    bindings: Res<'w, PlayerKeyBindings>,
    action_buttons:
        Query<'w, 's, (&'static Interaction, &'static TutorialUiAction), Changed<Interaction>>,
}

#[derive(SystemParam)]
pub struct TutorialInputContext<'w, 's> {
    commands: Commands<'w, 's>,
    user_mode: ResMut<'w, UserModeState>,
    session: ResMut<'w, TutorialSession>,
    progress: ResMut<'w, TutorialProgress>,
    setup: ResMut<'w, LocalSetup>,
    state: ResMut<'w, MatchState>,
    pause_owners: ResMut<'w, GameplayPauseOwners>,
    announcements: ResMut<'w, MatchAnnouncements>,
    music: Query<'w, 's, Entity, With<UserModeMusic>>,
}

fn tutorial_menu_input(
    user_mode: &UserModeState,
    devices: &TutorialInputDevices,
    latch: &mut TutorialNavigationLatch,
) -> TutorialMenuInput {
    let assignment = user_mode.tutorial_player_assignment();
    let mut input = TutorialMenuInput {
        confirm: devices.keys.just_pressed(KeyCode::Enter)
            || devices.keys.just_pressed(KeyCode::Space),
        back: devices.keys.just_pressed(KeyCode::Escape),
        menu: devices.keys.just_pressed(KeyCode::Escape),
        ..default()
    };

    let keyboard_player = match assignment {
        LocalInputAssignment::Keyboard(player) => Some(player),
        LocalInputAssignment::Unassigned => Some(0),
        LocalInputAssignment::Gamepad(_) => None,
    };
    if let Some(player) = keyboard_player
        && let Some(bindings) = devices.bindings.bindings_for_player(player)
    {
        input.confirm |= devices.keys.just_pressed(bindings.jump);
        input.back |= devices.keys.just_pressed(bindings.aim_grab);
        input.direction = if devices.keys.just_pressed(bindings.left)
            || devices.keys.just_pressed(KeyCode::ArrowLeft)
        {
            IVec2::NEG_X
        } else if devices.keys.just_pressed(bindings.right)
            || devices.keys.just_pressed(KeyCode::ArrowRight)
        {
            IVec2::X
        } else if devices.keys.just_pressed(bindings.up)
            || devices.keys.just_pressed(KeyCode::ArrowUp)
        {
            IVec2::NEG_Y
        } else if devices.keys.just_pressed(bindings.down)
            || devices.keys.just_pressed(KeyCode::ArrowDown)
        {
            IVec2::Y
        } else {
            IVec2::ZERO
        };
    }

    if let LocalInputAssignment::Gamepad(entity) = assignment
        && let Ok((_, gamepad)) = devices.gamepads.get(entity)
    {
        let family = controller_info(entity, &devices.controller_metadata)
            .map(|info| info.family)
            .unwrap_or_default();
        input.confirm |= gamepad.just_pressed(family.confirm_button());
        input.back |= gamepad.just_pressed(family.back_button());
        input.menu |= gamepad.just_pressed(GamepadButton::Start);
        let axis = if gamepad.dpad().length_squared() > 0.0 {
            gamepad.dpad()
        } else {
            gamepad.left_stick()
        };
        let direction = if axis.x.abs() >= axis.y.abs() && axis.x.abs() >= 0.55 {
            IVec2::new(axis.x.signum() as i32, 0)
        } else if axis.y.abs() >= 0.55 {
            IVec2::new(0, -axis.y.signum() as i32)
        } else {
            IVec2::ZERO
        };
        if direction == IVec2::ZERO {
            latch.direction = IVec2::ZERO;
        } else if latch.direction != direction {
            input.direction = direction;
            latch.direction = direction;
        }
    } else {
        latch.direction = IVec2::ZERO;
    }

    input
}

fn record_tutorial_step_completion(
    session: &mut TutorialSession,
    progress: &mut TutorialProgress,
    skipped: bool,
) -> bool {
    let Some(chapter_id) = session.chapter else {
        return false;
    };
    let chapter_finished = session.finish_step(skipped);
    if chapter_finished && session.chapter_can_complete() {
        progress.mark_complete(chapter_id);
        if let Err(error) = save_tutorial_progress(progress) {
            warn!("Could not save tutorial completion: {error}");
        }
    }
    chapter_finished
}

fn start_selected_tutorial_chapter(chapter: TutorialChapterId, context: &mut TutorialInputContext) {
    debug!("Starting tutorial chapter {}", chapter.stable_id());
    context.progress.mark_visited(chapter);
    if let Err(error) = save_tutorial_progress(&context.progress) {
        warn!("Could not save tutorial visit: {error}");
    }
    stop_user_mode_music(&mut context.commands, &context.music);
    let assignment = context.user_mode.tutorial_player_assignment();
    configure_tutorial_match(
        chapter,
        assignment,
        &mut context.setup,
        &mut context.state,
        &mut context.user_mode,
        &mut context.session,
        &mut context.pause_owners,
    );
    context.announcements.show(
        format!(
            "Chapter {:02}: {}",
            chapter_index(chapter) + 1,
            tutorial_chapter(chapter).title
        ),
        1.0,
    );
}

fn commit_tutorial_transition(
    action: TutorialTransitionAction,
    context: &mut TutorialInputContext,
) {
    match action {
        TutorialTransitionAction::EnterDeviceJoin => {
            context.user_mode.enter_tutorial_device_join();
        }
        TutorialTransitionAction::EnterHub => {
            context.user_mode.enter_tutorial_hub();
            context
                .announcements
                .show("Choose any tutorial chapter", 0.9);
        }
        TutorialTransitionAction::StartChapter(chapter) => {
            start_selected_tutorial_chapter(chapter, context);
        }
        TutorialTransitionAction::BeginPlaying => {
            context.session.resume_step();
            context
                .pause_owners
                .set(GameplayPauseOwner::TutorialPrompt, false);
            context
                .pause_owners
                .set(GameplayPauseOwner::TutorialSuccess, false);
        }
        TutorialTransitionAction::AdvanceStep { skipped } => {
            context
                .pause_owners
                .set(GameplayPauseOwner::TutorialSuccess, false);
            let finished = record_tutorial_step_completion(
                &mut context.session,
                &mut context.progress,
                skipped,
            );
            context.user_mode.resume_tutorial_lesson();
            context
                .pause_owners
                .set(GameplayPauseOwner::TutorialPrompt, true);
            context
                .pause_owners
                .set(GameplayPauseOwner::TutorialMenu, finished);
        }
        TutorialTransitionAction::OpenPause => {
            context.session.pause();
            context.user_mode.enter_tutorial_pause();
            context
                .pause_owners
                .set(GameplayPauseOwner::TutorialMenu, true);
        }
        TutorialTransitionAction::ResumePause => {
            context.session.resume_from_pause();
            context.user_mode.resume_tutorial_lesson();
            context
                .pause_owners
                .set(GameplayPauseOwner::TutorialMenu, false);
            context.pause_owners.set(
                GameplayPauseOwner::TutorialPrompt,
                context.session.phase == TutorialPhase::Prompt,
            );
            context.pause_owners.set(
                GameplayPauseOwner::TutorialSuccess,
                context.session.phase == TutorialPhase::Success,
            );
        }
        TutorialTransitionAction::RestartStep => {
            if context.state.phase == MatchPhase::Results {
                context.state.reset_for_new_match();
            }
            context.session.restart_step();
            context.user_mode.resume_tutorial_lesson();
            context
                .pause_owners
                .set(GameplayPauseOwner::TutorialMenu, false);
            context
                .pause_owners
                .set(GameplayPauseOwner::TutorialSuccess, false);
            context
                .pause_owners
                .set(GameplayPauseOwner::TutorialPrompt, true);
        }
        TutorialTransitionAction::ExitToHub => {
            context.session.request_cleanup();
        }
        TutorialTransitionAction::LeaveTutorial => {
            context.session.clear_for_hub();
            context.pause_owners.clear_tutorial_overlays();
            context.user_mode.enter_mode_select();
        }
        TutorialTransitionAction::ShowFinalResult { won } => {
            context.session.final_exam_won = Some(won);
            context.session.phase = TutorialPhase::FinalResult;
            context
                .pause_owners
                .set(GameplayPauseOwner::TutorialPrompt, false);
            context
                .pause_owners
                .set(GameplayPauseOwner::TutorialSuccess, false);
            context
                .pause_owners
                .set(GameplayPauseOwner::TutorialMenu, true);
            context.user_mode.enter_tutorial_final_result();
            if won && context.session.chapter_can_complete() {
                context.progress.mark_complete(TutorialChapterId::FinalExam);
                if let Err(error) = save_tutorial_progress(&context.progress) {
                    warn!("Could not save tutorial completion: {error}");
                }
            }
        }
        TutorialTransitionAction::FinalSkipToHub => {
            record_tutorial_step_completion(&mut context.session, &mut context.progress, true);
            context.session.request_cleanup();
        }
    }
}

pub fn advance_tutorial_success(
    time: Res<Time<Real>>,
    mut session: ResMut<TutorialSession>,
    mut pause_owners: ResMut<GameplayPauseOwners>,
    mut transition: ResMut<TutorialTransition>,
) {
    if session.phase != TutorialPhase::Success
        || transition.active()
        || pause_owners.contains(GameplayPauseOwner::ControllerReconnect)
        || pause_owners.contains(GameplayPauseOwner::TutorialMenu)
    {
        return;
    }
    session.success_elapsed += time.delta_secs();
    if session.success_elapsed >= TUTORIAL_SUCCESS_AUTO_SECONDS {
        request_tutorial_transition(
            &mut transition,
            &mut pause_owners,
            TutorialTransitionAction::AdvanceStep { skipped: false },
        );
    }
}

fn tutorial_success_ready_for_confirm(session: &TutorialSession) -> bool {
    session.phase == TutorialPhase::Success
        && session.success_elapsed >= TUTORIAL_SUCCESS_MIN_SECONDS
}

pub fn advance_tutorial_transition(
    time: Res<Time<Real>>,
    mut transition: ResMut<TutorialTransition>,
    mut context: TutorialInputContext,
) {
    let reveal_ready = context.session.phase == TutorialPhase::Prompt;
    let advance = transition.advance(time.delta_secs(), reveal_ready);
    if let Some(action) = advance.action {
        commit_tutorial_transition(action, &mut context);
    }
    if advance.finished {
        context
            .pause_owners
            .set(GameplayPauseOwner::TutorialTransition, false);
    }
}

pub fn handle_tutorial_input(
    devices: TutorialInputDevices,
    mut context: TutorialInputContext,
    mut transition: ResMut<TutorialTransition>,
    mut navigation_latch: Local<TutorialNavigationLatch>,
) {
    if transition.active() {
        navigation_latch.direction = IVec2::ZERO;
        return;
    }
    if !context.user_mode.tutorial_screen_active()
        || context.user_mode.screen() == UserModeScreen::DeviceJoin
    {
        navigation_latch.direction = IVec2::ZERO;
        return;
    }

    let pointer_action = devices
        .action_buttons
        .iter()
        .find_map(|(interaction, action)| {
            (*interaction == Interaction::Pressed).then_some(*action)
        });
    let menu = tutorial_menu_input(&context.user_mode, &devices, &mut navigation_latch);

    match context.user_mode.screen() {
        UserModeScreen::TutorialHub => {
            if context.session.reset_confirmation {
                let action = pointer_action.or_else(|| {
                    if menu.confirm {
                        Some(TutorialUiAction::ConfirmReset)
                    } else if menu.back {
                        Some(TutorialUiAction::CancelReset)
                    } else {
                        None
                    }
                });
                match action {
                    Some(TutorialUiAction::ConfirmReset) => {
                        context.progress.reset();
                        context.session.reset_confirmation = false;
                        if let Err(error) = save_tutorial_progress(&context.progress) {
                            warn!("Could not save reset tutorial progress: {error}");
                        }
                        context
                            .announcements
                            .show("Tutorial progress reset; controls preserved", 1.1);
                    }
                    Some(TutorialUiAction::CancelReset) => {
                        context.session.reset_confirmation = false;
                    }
                    _ => {}
                }
                return;
            }

            if menu.direction != IVec2::ZERO {
                context.session.hub_cursor =
                    tutorial_grid_move(context.session.hub_cursor, menu.direction);
            }
            let action = pointer_action.or_else(|| {
                if menu.confirm {
                    Some(TutorialUiAction::Chapter(
                        TutorialChapterId::ALL[context.session.hub_cursor.min(11)],
                    ))
                } else if menu.back {
                    Some(TutorialUiAction::Back)
                } else if devices.keys.just_pressed(KeyCode::KeyR) {
                    Some(TutorialUiAction::ResetProgress)
                } else {
                    None
                }
            });
            match action {
                Some(TutorialUiAction::Chapter(chapter)) => {
                    context.session.hub_cursor = chapter_index(chapter);
                    request_tutorial_transition(
                        &mut transition,
                        &mut context.pause_owners,
                        TutorialTransitionAction::StartChapter(chapter),
                    );
                }
                Some(TutorialUiAction::ResetProgress) => {
                    context.session.reset_confirmation = true;
                }
                Some(TutorialUiAction::Back) => {
                    request_tutorial_transition(
                        &mut transition,
                        &mut context.pause_owners,
                        TutorialTransitionAction::LeaveTutorial,
                    );
                }
                _ => {}
            }
        }
        UserModeScreen::TutorialLesson => match context.session.phase {
            TutorialPhase::WaitingForMatch => {
                if menu.menu {
                    request_tutorial_transition(
                        &mut transition,
                        &mut context.pause_owners,
                        TutorialTransitionAction::ExitToHub,
                    );
                }
            }
            TutorialPhase::Prompt => {
                if menu.menu {
                    request_tutorial_transition(
                        &mut transition,
                        &mut context.pause_owners,
                        TutorialTransitionAction::OpenPause,
                    );
                    return;
                }
                if pointer_action == Some(TutorialUiAction::Continue) || menu.confirm {
                    if context
                        .session
                        .current_step()
                        .is_some_and(|step| step.objective == TutorialObjective::Knowledge)
                    {
                        request_tutorial_transition(
                            &mut transition,
                            &mut context.pause_owners,
                            TutorialTransitionAction::AdvanceStep { skipped: false },
                        );
                    } else {
                        request_tutorial_transition(
                            &mut transition,
                            &mut context.pause_owners,
                            TutorialTransitionAction::BeginPlaying,
                        );
                    }
                }
            }
            TutorialPhase::Playing => {
                if menu.menu {
                    request_tutorial_transition(
                        &mut transition,
                        &mut context.pause_owners,
                        TutorialTransitionAction::OpenPause,
                    );
                }
            }
            TutorialPhase::Success => {
                if menu.menu {
                    request_tutorial_transition(
                        &mut transition,
                        &mut context.pause_owners,
                        TutorialTransitionAction::OpenPause,
                    );
                    return;
                }
                if tutorial_success_ready_for_confirm(&context.session)
                    && (pointer_action == Some(TutorialUiAction::Continue) || menu.confirm)
                {
                    request_tutorial_transition(
                        &mut transition,
                        &mut context.pause_owners,
                        TutorialTransitionAction::AdvanceStep { skipped: false },
                    );
                }
            }
            TutorialPhase::ChapterComplete => {
                if pointer_action == Some(TutorialUiAction::ExitToHub) || menu.confirm || menu.back
                {
                    request_tutorial_transition(
                        &mut transition,
                        &mut context.pause_owners,
                        TutorialTransitionAction::ExitToHub,
                    );
                }
            }
            _ => {}
        },
        UserModeScreen::TutorialPause => {
            if menu.direction.y != 0 {
                context.session.pause_cursor = (context.session.pause_cursor as isize
                    + menu.direction.y.signum() as isize)
                    .rem_euclid(4) as usize;
            }
            let action = pointer_action.or_else(|| {
                if menu.back || menu.menu {
                    Some(TutorialUiAction::Resume)
                } else if menu.confirm {
                    Some(match context.session.pause_cursor {
                        0 => TutorialUiAction::Resume,
                        1 => TutorialUiAction::RestartStep,
                        2 => TutorialUiAction::SkipStep,
                        _ => TutorialUiAction::ExitToHub,
                    })
                } else {
                    None
                }
            });
            if let Some(action) = action {
                let transition_action = match action {
                    TutorialUiAction::Resume => Some(TutorialTransitionAction::ResumePause),
                    TutorialUiAction::RestartStep => Some(TutorialTransitionAction::RestartStep),
                    TutorialUiAction::SkipStep => Some(TutorialTransitionAction::AdvanceStep {
                        skipped: context.session.paused_from != TutorialPhase::Success,
                    }),
                    TutorialUiAction::ExitToHub => Some(TutorialTransitionAction::ExitToHub),
                    _ => None,
                };
                if let Some(transition_action) = transition_action {
                    request_tutorial_transition(
                        &mut transition,
                        &mut context.pause_owners,
                        transition_action,
                    );
                }
            }
        }
        UserModeScreen::TutorialFinalResult => {
            let won = context.session.final_exam_won == Some(true);
            if !won && menu.direction.y != 0 {
                context.session.final_cursor = (context.session.final_cursor as isize
                    + menu.direction.y.signum() as isize)
                    .rem_euclid(3) as usize;
            }
            let action = pointer_action.or_else(|| {
                if menu.back {
                    Some(TutorialUiAction::FinalHub)
                } else if menu.confirm {
                    Some(if won {
                        TutorialUiAction::FinalHub
                    } else {
                        match context.session.final_cursor {
                            0 => TutorialUiAction::FinalRetry,
                            1 => TutorialUiAction::FinalHub,
                            _ => TutorialUiAction::FinalSkip,
                        }
                    })
                } else {
                    None
                }
            });
            match action {
                Some(TutorialUiAction::FinalRetry) if !won => {
                    request_tutorial_transition(
                        &mut transition,
                        &mut context.pause_owners,
                        TutorialTransitionAction::StartChapter(TutorialChapterId::FinalExam),
                    );
                }
                Some(TutorialUiAction::FinalSkip) if !won => {
                    request_tutorial_transition(
                        &mut transition,
                        &mut context.pause_owners,
                        TutorialTransitionAction::FinalSkipToHub,
                    );
                }
                Some(TutorialUiAction::FinalHub) => {
                    request_tutorial_transition(
                        &mut transition,
                        &mut context.pause_owners,
                        TutorialTransitionAction::ExitToHub,
                    );
                }
                _ => {}
            }
        }
        _ => {}
    }
}

#[derive(SystemParam)]
pub struct TutorialCleanupResources<'w> {
    asset_server: Res<'w, AssetServer>,
    session: ResMut<'w, TutorialSession>,
    user_mode: ResMut<'w, UserModeState>,
    setup: ResMut<'w, LocalSetup>,
    state: ResMut<'w, MatchState>,
    pause_owners: ResMut<'w, GameplayPauseOwners>,
    virtual_time: ResMut<'w, Time<Virtual>>,
    screen_look: ResMut<'w, ScreenLook>,
    screen_transition: ResMut<'w, ScreenLookTransition>,
    feedback: ResMut<'w, HitEffects>,
    camera_effects: ResMut<'w, CameraActionEffects>,
    hitstop: ResMut<'w, Hitstop>,
    announcements: ResMut<'w, MatchAnnouncements>,
}

pub fn cleanup_tutorial_session(
    mut commands: Commands,
    resources: TutorialCleanupResources,
    transients: Query<
        Entity,
        Or<(
            With<Hitbox>,
            With<ActiveSpecial>,
            With<ActiveBeeSkill>,
            With<ActiveChickSkill>,
            With<ActivePenguinSkill>,
            With<ActivePenguinSurface>,
            With<VisualEffect>,
        )>,
    >,
    mut tutorial_bots: Query<(Entity, &mut BotBrain), With<BotDifficulty>>,
    music: Query<Entity, With<UserModeMusic>>,
) {
    let TutorialCleanupResources {
        asset_server,
        mut session,
        mut user_mode,
        mut setup,
        mut state,
        mut pause_owners,
        mut virtual_time,
        mut screen_look,
        mut screen_transition,
        mut feedback,
        mut camera_effects,
        mut hitstop,
        mut announcements,
    } = resources;
    if !session.cleanup_requested {
        return;
    }

    for entity in &transients {
        commands.entity(entity).despawn();
    }
    for (entity, mut brain) in &mut tutorial_bots {
        brain.behavior = BotBehaviorMode::TrainingDummy;
        commands.entity(entity).remove::<BotDifficulty>();
    }
    stop_user_mode_music(&mut commands, &music);
    start_user_mode_menu_music(&mut commands, &asset_server);

    restore_tutorial_setup(
        &mut session,
        &mut setup,
        user_mode.tutorial_player_assignment(),
        &mut state,
    );
    pause_owners.clear_tutorial_overlays();
    virtual_time.set_relative_speed(1.0);
    *screen_look = ScreenLook::Default;
    *screen_transition = ScreenLookTransition::default();
    *feedback = HitEffects::default();
    *camera_effects = CameraActionEffects::default();
    hitstop.remaining = 0.0;
    announcements.show("Choose any tutorial chapter", 0.9);
    user_mode.enter_tutorial_hub();
    session.clear_for_hub();
}

fn restore_tutorial_setup(
    session: &mut TutorialSession,
    setup: &mut LocalSetup,
    player_assignment: LocalInputAssignment,
    state: &mut MatchState,
) {
    if let Some(mut return_setup) = session.return_setup.take() {
        return_setup.slots[TUTORIAL_PLAYER_ID].input = player_assignment;
        *setup = return_setup;
    }
    state.return_to_setup();
    state.rule_index = setup.rule_index;
    state.rules = setup.active_rule();
    state.arena_index = setup.arena_index;
    state.apply_local_setup(setup);
    state.replay_seed = setup.replay_seed;
    set_active_arena_index(setup.arena_index);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn advance_virtual_time(app: &mut App, seconds: f32) {
        app.world_mut()
            .resource_mut::<Time<()>>()
            .advance_by(Duration::from_secs_f32(seconds));
    }

    fn advance_real_time(app: &mut App, seconds: f32) {
        app.world_mut()
            .resource_mut::<Time<Real>>()
            .advance_by(Duration::from_secs_f32(seconds));
    }

    fn item_objective_test_app(
        step_index: usize,
        kind: ItemKind,
        item_state: impl FnOnce(Entity) -> ItemState,
    ) -> (App, Entity) {
        let mut app = App::new();
        let mut session = TutorialSession::default();
        session.start(TutorialChapterId::Items);
        session.step_index = step_index;
        session.phase = TutorialPhase::Playing;
        session.reset_requested = false;
        session.item_was_held = true;
        let mut state = MatchState::default();
        state.phase = MatchPhase::Fighting;
        state.stocks = [STOCK_LIVES; FIGHTER_COUNT];
        app.insert_resource(session)
            .insert_resource(state)
            .insert_resource(MatchTelemetry::default())
            .insert_resource(GameplayPauseOwners::default())
            .insert_resource(TutorialTransition::default())
            .insert_resource(Time::<()>::default())
            .add_systems(Update, observe_tutorial_objective);
        let player = app
            .world_mut()
            .spawn((
                Fighter {
                    id: TUTORIAL_PLAYER_ID,
                    name: "Cat",
                    color: Color::WHITE,
                    spawn: Vec3::ZERO,
                },
                FighterInput::default(),
                FighterActionState::default(),
                FighterStats::default(),
                FighterMotor::default(),
                FighterGrabState::default(),
                Transform::default(),
                FighterInventory::default(),
            ))
            .id();
        let mut item = ArenaItem::new(kind, Vec3::ZERO, 0.0);
        item.state = item_state(player);
        let item_entity = app.world_mut().spawn(item).id();
        {
            let mut session = app.world_mut().resource_mut::<TutorialSession>();
            session.objective_baseline.lesson_item_entity = Some(item_entity);
            session.objective_baseline.item_count = 1;
        }
        (app, item_entity)
    }

    #[test]
    fn catalog_has_twelve_unique_open_chapters() {
        assert_eq!(TUTORIAL_CHAPTERS.len(), 12);
        let ids = TUTORIAL_CHAPTERS
            .iter()
            .map(|chapter| chapter.id)
            .collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), 12);
        assert_eq!(ids, TutorialChapterId::ALL.into_iter().collect());
        assert!(
            TUTORIAL_CHAPTERS
                .iter()
                .all(|chapter| !chapter.steps.is_empty())
        );
        let stable_ids = TutorialChapterId::ALL
            .into_iter()
            .map(TutorialChapterId::stable_id)
            .collect::<BTreeSet<_>>();
        assert_eq!(stable_ids.len(), 12);
    }

    #[test]
    fn catalog_covers_five_selectable_fighters() {
        let fighters = TUTORIAL_CHAPTERS
            .iter()
            .map(|chapter| chapter.player_character)
            .collect::<Vec<_>>();
        for expected in [
            CharacterKind::Cat,
            CharacterKind::Pig,
            CharacterKind::Bee,
            CharacterKind::Penguin,
            CharacterKind::Chick,
        ] {
            assert!(fighters.contains(&expected));
        }
        for excluded in [CharacterKind::Dog, CharacterKind::Fox, CharacterKind::Panda] {
            assert!(!fighters.contains(&excluded));
        }
    }

    #[test]
    fn curriculum_covers_all_items_and_shared_specials() {
        let mut items = BTreeSet::new();
        let mut specials = BTreeSet::new();
        for chapter in TUTORIAL_CHAPTERS {
            for step in chapter.steps {
                match step.objective {
                    TutorialObjective::ItemUse { kind, .. }
                    | TutorialObjective::ItemThrow(kind) => {
                        items.insert(format!("{kind:?}"));
                    }
                    TutorialObjective::SpecialSpawn(kind) => {
                        specials.insert(format!("{kind:?}"));
                    }
                    _ => {}
                }
            }
        }
        assert_eq!(items.len(), 8);
        assert_eq!(specials.len(), 4);
    }

    #[test]
    fn lab_objectives_only_reference_valid_character_techniques() {
        let catalog = CharacterMoveCatalog::default();
        for chapter in TUTORIAL_CHAPTERS {
            for step in chapter.steps {
                let TutorialObjective::ActivateTechnique { technique, .. } = step.objective else {
                    continue;
                };
                assert!(
                    catalog.allows_technique(chapter.player_character, technique),
                    "{} cannot use {:?}",
                    chapter.title,
                    technique
                );
            }
        }
    }

    #[test]
    fn progress_roundtrips_and_reset_preserves_no_other_preferences() {
        let controls = PlayerKeyBindings::default();
        let controls_before = controls.clone();
        let mut progress = TutorialProgress::default();
        progress.mark_visited(TutorialChapterId::Basics);
        progress.mark_complete(TutorialChapterId::Combat);
        let encoded = encode_tutorial_progress(&progress).unwrap();
        assert_eq!(decode_tutorial_progress(&encoded).unwrap(), progress);

        progress.reset();
        assert_eq!(progress, TutorialProgress::default());
        assert_eq!(controls, controls_before);
    }

    #[test]
    fn corrupt_and_unsupported_progress_is_ignored_by_decoder() {
        assert!(decode_tutorial_progress("not ron").is_err());
        let encoded = encode_tutorial_progress(&TutorialProgress::default())
            .unwrap()
            .replacen("version: 1", "version: 99", 1);
        assert!(decode_tutorial_progress(&encoded).is_err());
    }

    #[test]
    fn completed_without_visited_is_rejected() {
        let stored = StoredTutorialProgress {
            version: 1,
            visited: BTreeSet::new(),
            completed: [TutorialChapterId::Basics].into_iter().collect(),
        };
        let encoded = ron::ser::to_string(&stored).unwrap();
        assert!(decode_tutorial_progress(&encoded).is_err());
    }

    #[test]
    fn grid_navigation_is_two_columns_and_six_rows() {
        assert_eq!(tutorial_grid_move(0, IVec2::X), 1);
        assert_eq!(tutorial_grid_move(1, IVec2::X), 1);
        assert_eq!(tutorial_grid_move(1, IVec2::Y), 3);
        assert_eq!(tutorial_grid_move(10, IVec2::Y), 10);
        assert_eq!(tutorial_grid_move(11, IVec2::NEG_Y), 9);
    }

    #[test]
    fn skipping_allows_progression_but_prevents_completion() {
        let mut session = TutorialSession::default();
        session.start(TutorialChapterId::FinalExam);
        assert!(session.finish_step(true));
        assert_eq!(session.phase, TutorialPhase::ChapterComplete);
        assert!(!session.chapter_can_complete());
    }

    #[test]
    fn restart_tracks_attempts_and_enables_stronger_hint() {
        let mut session = TutorialSession::default();
        session.start(TutorialChapterId::Basics);
        for _ in 0..3 {
            session.restart_step();
        }
        assert!(session.stronger_hint_active());
        assert_eq!(session.phase, TutorialPhase::Prompt);
        assert!(session.reset_requested);
    }

    #[test]
    fn pause_resume_restores_prompt_playing_or_success_phase() {
        let mut session = TutorialSession::default();
        session.start(TutorialChapterId::Basics);
        session.phase = TutorialPhase::Prompt;
        session.pause();
        assert_eq!(session.phase, TutorialPhase::PauseMenu);
        session.resume_from_pause();
        assert_eq!(session.phase, TutorialPhase::Prompt);

        session.resume_step();
        session.pause();
        session.resume_from_pause();
        assert_eq!(session.phase, TutorialPhase::Playing);

        session.phase = TutorialPhase::Success;
        session.pause();
        session.resume_from_pause();
        assert_eq!(session.phase, TutorialPhase::Success);
    }

    #[test]
    fn tutorial_pause_owners_do_not_clear_controller_reconnect() {
        let mut owners = GameplayPauseOwners::default();
        owners.set(GameplayPauseOwner::ControllerReconnect, true);
        owners.set(GameplayPauseOwner::TutorialPrompt, true);
        owners.set(GameplayPauseOwner::TutorialMenu, true);
        owners.set(GameplayPauseOwner::TutorialSuccess, true);
        owners.set(GameplayPauseOwner::TutorialTransition, true);

        owners.clear_tutorial_overlays();

        assert!(owners.contains(GameplayPauseOwner::ControllerReconnect));
        assert!(!owners.contains(GameplayPauseOwner::TutorialPrompt));
        assert!(!owners.contains(GameplayPauseOwner::TutorialMenu));
        assert!(!owners.contains(GameplayPauseOwner::TutorialSuccess));
        assert!(owners.contains(GameplayPauseOwner::TutorialTransition));
        owners.set(GameplayPauseOwner::TutorialTransition, false);
        assert!(owners.blocks_gameplay());
    }

    #[test]
    fn tutorial_fade_changes_state_only_while_fully_covered() {
        let mut transition = TutorialTransition::default();
        assert!(transition.request(TutorialTransitionAction::BeginPlaying));
        assert_eq!(transition.alpha(), 0.0);

        let halfway = transition.advance(TUTORIAL_FADE_OUT_SECONDS * 0.5, true);
        assert_eq!(halfway.action, None);
        assert!((transition.alpha() - 0.5).abs() < 0.001);

        let covered = transition.advance(TUTORIAL_FADE_OUT_SECONDS * 0.5, true);
        assert_eq!(covered.action, Some(TutorialTransitionAction::BeginPlaying));
        assert_eq!(transition.stage, TutorialTransitionStage::Covered);
        assert_eq!(transition.alpha(), 1.0);

        transition.advance(TUTORIAL_FADE_HOLD_SECONDS, true);
        assert_eq!(transition.stage, TutorialTransitionStage::FadingIn);
        transition.advance(TUTORIAL_FADE_IN_SECONDS * 0.5, true);
        assert!((transition.alpha() - 0.5).abs() < 0.001);

        let finished = transition.advance(TUTORIAL_FADE_IN_SECONDS * 0.5, true);
        assert!(finished.finished);
        assert!(!transition.active());
        assert_eq!(transition.alpha(), 0.0);
    }

    #[test]
    fn tutorial_fade_ignores_repeat_requests_and_waits_for_lesson_setup() {
        let mut transition = TutorialTransition::default();
        assert!(transition.request(TutorialTransitionAction::StartChapter(
            TutorialChapterId::Basics
        )));
        assert!(!transition.request(TutorialTransitionAction::LeaveTutorial));

        let covered = transition.advance(TUTORIAL_FADE_OUT_SECONDS, false);
        assert_eq!(
            covered.action,
            Some(TutorialTransitionAction::StartChapter(
                TutorialChapterId::Basics
            ))
        );
        transition.advance(TUTORIAL_FADE_HOLD_SECONDS * 2.0, false);
        assert_eq!(transition.stage, TutorialTransitionStage::Covered);
        assert_eq!(transition.alpha(), 1.0);

        transition.advance(TUTORIAL_FADE_HOLD_SECONDS, true);
        assert_eq!(transition.stage, TutorialTransitionStage::FadingIn);
    }

    #[test]
    fn movement_objective_waits_for_input_release_before_success() {
        let mut app = App::new();
        let mut session = TutorialSession::default();
        session.start(TutorialChapterId::Basics);
        session.step_index = 1;
        session.phase = TutorialPhase::Playing;
        session.reset_requested = false;
        session.objective_baseline.last_player_position = Vec3::ZERO;
        let mut state = MatchState::default();
        state.phase = MatchPhase::Fighting;
        state.stocks = [STOCK_LIVES; FIGHTER_COUNT];
        app.insert_resource(session)
            .insert_resource(TutorialProgress::default())
            .insert_resource(state)
            .insert_resource(MatchTelemetry::default())
            .insert_resource(GameplayPauseOwners::default())
            .insert_resource(TutorialTransition::default())
            .insert_resource(UserModeState::default())
            .insert_resource(Time::<()>::default())
            .add_systems(Update, observe_tutorial_objective);
        let player = app
            .world_mut()
            .spawn((
                Fighter {
                    id: TUTORIAL_PLAYER_ID,
                    name: "Cat",
                    color: Color::WHITE,
                    spawn: Vec3::ZERO,
                },
                FighterInput {
                    movement: Vec2::NEG_X,
                    ..default()
                },
                FighterActionState::default(),
                FighterStats::default(),
                FighterMotor::default(),
                FighterGrabState::default(),
                Transform::from_xyz(-3.0, 0.0, 0.0),
                FighterInventory::default(),
            ))
            .id();

        app.update();

        let session = app.world().resource::<TutorialSession>();
        assert_eq!(session.step_index, 1);
        assert_eq!(session.phase, TutorialPhase::Playing);
        assert_eq!(session.completion_state, TutorialCompletionState::Settling);
        assert_eq!(
            app.world().resource::<TutorialTransition>().pending_action,
            None
        );

        app.world_mut()
            .entity_mut(player)
            .get_mut::<FighterInput>()
            .unwrap()
            .movement = Vec2::ZERO;
        advance_virtual_time(&mut app, TUTORIAL_SETTLE_STABLE_SECONDS);
        app.update();

        let session = app.world().resource::<TutorialSession>();
        assert_eq!(session.phase, TutorialPhase::Success);
        assert_eq!(session.completion_state, TutorialCompletionState::Succeeded);
        assert!(
            app.world()
                .resource::<GameplayPauseOwners>()
                .contains(GameplayPauseOwner::TutorialSuccess)
        );
    }

    #[test]
    fn jump_objective_waits_for_airborne_cycle_and_landing_recovery() {
        let mut app = App::new();
        let mut session = TutorialSession::default();
        session.start(TutorialChapterId::Basics);
        session.step_index = 5;
        session.phase = TutorialPhase::Playing;
        session.reset_requested = false;
        let mut state = MatchState::default();
        state.phase = MatchPhase::Fighting;
        state.stocks = [STOCK_LIVES; FIGHTER_COUNT];
        app.insert_resource(session)
            .insert_resource(state)
            .insert_resource(MatchTelemetry::default())
            .insert_resource(GameplayPauseOwners::default())
            .insert_resource(TutorialTransition::default())
            .insert_resource(Time::<()>::default())
            .add_systems(Update, observe_tutorial_objective);
        let player = app
            .world_mut()
            .spawn((
                Fighter {
                    id: TUTORIAL_PLAYER_ID,
                    name: "Cat",
                    color: Color::WHITE,
                    spawn: Vec3::ZERO,
                },
                FighterInput {
                    jump: true,
                    ..default()
                },
                FighterActionState {
                    action: FighterAction::Jumping,
                    ..default()
                },
                FighterStats::default(),
                FighterMotor::default(),
                FighterGrabState::default(),
                Transform::default(),
                FighterInventory::default(),
            ))
            .id();

        app.update();
        assert_eq!(
            app.world().resource::<TutorialSession>().completion_state,
            TutorialCompletionState::Settling
        );
        assert_eq!(
            app.world().resource::<TutorialSession>().phase,
            TutorialPhase::Playing,
            "the button press/takeoff frame must not finish Jump"
        );

        {
            let mut entity = app.world_mut().entity_mut(player);
            let mut motor = entity.get_mut::<FighterMotor>().unwrap();
            motor.grounded = false;
            motor.velocity.y = 3.0;
        }
        advance_virtual_time(&mut app, 0.3);
        app.update();
        assert_eq!(
            app.world().resource::<TutorialSession>().phase,
            TutorialPhase::Playing,
            "airborne Jump must remain active"
        );

        {
            let mut entity = app.world_mut().entity_mut(player);
            let mut motor = entity.get_mut::<FighterMotor>().unwrap();
            motor.grounded = true;
            motor.velocity = Vec3::ZERO;
            entity.get_mut::<FighterActionState>().unwrap().action = FighterAction::LandingRecovery;
        }
        advance_virtual_time(&mut app, TUTORIAL_SETTLE_STABLE_SECONDS);
        app.update();
        assert_eq!(
            app.world().resource::<TutorialSession>().phase,
            TutorialPhase::Playing,
            "landing recovery must finish before praise"
        );

        app.world_mut()
            .entity_mut(player)
            .get_mut::<FighterActionState>()
            .unwrap()
            .action = FighterAction::Idle;
        advance_virtual_time(&mut app, TUTORIAL_SETTLE_STABLE_SECONDS);
        app.update();
        assert_eq!(
            app.world().resource::<TutorialSession>().phase,
            TutorialPhase::Success
        );
    }

    #[test]
    fn aim_objective_requires_a_deliberate_hold_then_release() {
        let mut app = App::new();
        let mut session = TutorialSession::default();
        session.start(TutorialChapterId::Basics);
        session.step_index = 7;
        session.phase = TutorialPhase::Playing;
        session.reset_requested = false;
        let mut state = MatchState::default();
        state.phase = MatchPhase::Fighting;
        state.stocks = [STOCK_LIVES; FIGHTER_COUNT];
        app.insert_resource(session)
            .insert_resource(state)
            .insert_resource(MatchTelemetry::default())
            .insert_resource(GameplayPauseOwners::default())
            .insert_resource(TutorialTransition::default())
            .insert_resource(Time::<()>::default())
            .add_systems(Update, observe_tutorial_objective);
        let player = app
            .world_mut()
            .spawn((
                Fighter {
                    id: TUTORIAL_PLAYER_ID,
                    name: "Cat",
                    color: Color::WHITE,
                    spawn: Vec3::ZERO,
                },
                FighterInput {
                    movement: Vec2::X,
                    aim: true,
                    ..default()
                },
                FighterActionState::default(),
                FighterStats::default(),
                FighterMotor::default(),
                FighterGrabState::default(),
                Transform::default(),
                FighterInventory::default(),
            ))
            .id();

        advance_virtual_time(&mut app, TUTORIAL_AIM_HOLD_SECONDS - 0.01);
        app.update();
        assert_eq!(
            app.world().resource::<TutorialSession>().completion_state,
            TutorialCompletionState::Observing
        );

        advance_virtual_time(&mut app, 0.02);
        app.update();
        assert_eq!(
            app.world().resource::<TutorialSession>().completion_state,
            TutorialCompletionState::Settling
        );

        *app.world_mut()
            .entity_mut(player)
            .get_mut::<FighterInput>()
            .unwrap() = FighterInput::default();
        advance_virtual_time(&mut app, TUTORIAL_SETTLE_STABLE_SECONDS);
        app.update();
        assert_eq!(
            app.world().resource::<TutorialSession>().phase,
            TutorialPhase::Success
        );
    }

    #[test]
    fn confirmed_technique_waits_until_the_fighter_recovers() {
        let mut app = App::new();
        let mut session = TutorialSession::default();
        session.start(TutorialChapterId::Combat);
        session.step_index = 1;
        session.phase = TutorialPhase::Playing;
        session.reset_requested = false;
        let mut state = MatchState::default();
        state.phase = MatchPhase::Fighting;
        state.stocks = [STOCK_LIVES; FIGHTER_COUNT];
        app.insert_resource(session)
            .insert_resource(state)
            .insert_resource(MatchTelemetry::default())
            .insert_resource(GameplayPauseOwners::default())
            .insert_resource(TutorialTransition::default())
            .insert_resource(Time::<()>::default())
            .add_systems(Update, observe_tutorial_objective);
        let player = app
            .world_mut()
            .spawn((
                Fighter {
                    id: TUTORIAL_PLAYER_ID,
                    name: "Cat",
                    color: Color::WHITE,
                    spawn: Vec3::ZERO,
                },
                FighterInput::default(),
                FighterActionState {
                    action: FighterAction::ComboFinisher,
                    technique_id: Some(TechniqueId::CatComboFinisher),
                    confirmed_hit: true,
                    ..default()
                },
                FighterStats::default(),
                FighterMotor::default(),
                FighterGrabState::default(),
                Transform::default(),
                FighterInventory::default(),
            ))
            .id();

        app.update();
        assert_eq!(
            app.world().resource::<TutorialSession>().completion_state,
            TutorialCompletionState::Settling
        );
        assert_eq!(
            app.world().resource::<TutorialTransition>().pending_action,
            None
        );

        *app.world_mut()
            .entity_mut(player)
            .get_mut::<FighterActionState>()
            .unwrap() = FighterActionState::default();
        advance_virtual_time(&mut app, TUTORIAL_SETTLE_STABLE_SECONDS);
        app.update();
        assert_eq!(
            app.world().resource::<TutorialSession>().phase,
            TutorialPhase::Success
        );
    }

    #[test]
    fn steamer_waits_for_the_blast_after_throw_recovery() {
        let (mut app, _) =
            item_objective_test_app(9, ItemKind::Steamer, |owner| ItemState::Armed {
                owner,
                owner_id: TUTORIAL_PLAYER_ID,
                timer: 0.5,
                grace: 0.0,
            });

        app.update();
        assert_eq!(
            app.world().resource::<TutorialSession>().completion_state,
            TutorialCompletionState::Settling
        );
        advance_virtual_time(&mut app, TUTORIAL_SETTLE_STABLE_SECONDS * 2.0);
        app.update();
        assert_eq!(
            app.world().resource::<TutorialSession>().phase,
            TutorialPhase::Playing,
            "arming and throw recovery alone must not complete Steamer"
        );

        app.world_mut().spawn(VisualEffect {
            kind: EffectKind::PopBombBlast,
            lifetime: 0.5,
            age: 0.0,
            velocity: Vec3::ZERO,
            spin: Vec3::ZERO,
            start_scale: Vec3::ONE,
            end_scale: Vec3::ONE,
        });
        advance_virtual_time(&mut app, TUTORIAL_SETTLE_STABLE_SECONDS);
        app.update();
        assert_eq!(
            app.world().resource::<TutorialSession>().phase,
            TutorialPhase::Success
        );
    }

    #[test]
    fn barrel_waits_for_the_visible_spray() {
        let (mut app, _) =
            item_objective_test_app(5, ItemKind::Barrel, |owner| ItemState::Spraying {
                owner,
                owner_id: TUTORIAL_PLAYER_ID,
                lifetime: 4.0,
                spray_timer: 0.0,
                spiral_phase: 0.0,
                spiral_radius: 1.0,
            });

        app.update();
        assert_eq!(
            app.world().resource::<TutorialSession>().completion_state,
            TutorialCompletionState::Settling
        );
        advance_virtual_time(&mut app, TUTORIAL_SETTLE_STABLE_SECONDS * 2.0);
        app.update();
        assert_eq!(
            app.world().resource::<TutorialSession>().phase,
            TutorialPhase::Playing,
            "the Spraying state alone must not replace the visible teaching cue"
        );

        app.world_mut().spawn(VisualEffect {
            kind: EffectKind::AlcoholSpray,
            lifetime: 0.5,
            age: 0.0,
            velocity: Vec3::ZERO,
            spin: Vec3::ZERO,
            start_scale: Vec3::ONE,
            end_scale: Vec3::ONE,
        });
        advance_virtual_time(&mut app, TUTORIAL_SETTLE_STABLE_SECONDS);
        app.update();
        assert_eq!(
            app.world().resource::<TutorialSession>().phase,
            TutorialPhase::Success
        );
    }

    #[test]
    fn mystery_crate_waits_until_its_reward_is_revealed() {
        let (mut app, crate_entity) =
            item_objective_test_app(8, ItemKind::Crate, |owner| ItemState::Thrown {
                owner,
                owner_id: TUTORIAL_PLAYER_ID,
                lifetime: 1.0,
                grace: 0.0,
            });

        app.update();
        assert_eq!(
            app.world().resource::<TutorialSession>().completion_state,
            TutorialCompletionState::Settling
        );
        advance_virtual_time(&mut app, TUTORIAL_SETTLE_STABLE_SECONDS * 2.0);
        app.update();
        assert_eq!(
            app.world().resource::<TutorialSession>().phase,
            TutorialPhase::Playing
        );

        app.world_mut()
            .entity_mut(crate_entity)
            .get_mut::<ArenaItem>()
            .unwrap()
            .state = ItemState::Respawning;
        app.world_mut()
            .spawn(ArenaItem::new(ItemKind::Apple, Vec3::ZERO, 0.0));
        advance_virtual_time(&mut app, TUTORIAL_SETTLE_STABLE_SECONDS);
        app.update();
        assert_eq!(
            app.world().resource::<TutorialSession>().phase,
            TutorialPhase::Success
        );
    }

    #[test]
    fn ring_out_objective_waits_for_the_dummy_to_finish_respawning() {
        let mut app = App::new();
        let mut session = TutorialSession::default();
        session.start(TutorialChapterId::HudWinning);
        session.step_index = 5;
        session.phase = TutorialPhase::Playing;
        session.reset_requested = false;
        session.objective_baseline.player_stock = STOCK_LIVES;
        session.objective_baseline.dummy_stock = STOCK_LIVES;
        let mut state = MatchState::default();
        state.phase = MatchPhase::Fighting;
        state.stocks = [STOCK_LIVES, STOCK_LIVES - 1, STOCK_LIVES, STOCK_LIVES];
        let mut telemetry = MatchTelemetry::default();
        telemetry.ring_outs = 1;
        app.insert_resource(session)
            .insert_resource(state)
            .insert_resource(telemetry)
            .insert_resource(GameplayPauseOwners::default())
            .insert_resource(TutorialTransition::default())
            .insert_resource(Time::<()>::default())
            .add_systems(Update, observe_tutorial_objective);
        app.world_mut().spawn((
            Fighter {
                id: TUTORIAL_PLAYER_ID,
                name: "Cat",
                color: Color::WHITE,
                spawn: Vec3::ZERO,
            },
            FighterInput::default(),
            FighterActionState::default(),
            FighterStats::default(),
            FighterMotor::default(),
            FighterGrabState::default(),
            Transform::default(),
            FighterInventory::default(),
        ));
        let dummy = app
            .world_mut()
            .spawn((
                Fighter {
                    id: TUTORIAL_DUMMY_ID,
                    name: "Pig",
                    color: Color::WHITE,
                    spawn: Vec3::X,
                },
                FighterInput::default(),
                FighterActionState {
                    action: FighterAction::Respawning,
                    ..default()
                },
                FighterStats::default(),
                FighterMotor::default(),
                FighterGrabState::default(),
                Transform::default(),
                FighterInventory::default(),
            ))
            .id();

        app.update();
        assert_eq!(
            app.world().resource::<TutorialSession>().completion_state,
            TutorialCompletionState::Settling
        );
        advance_virtual_time(&mut app, TUTORIAL_SETTLE_STABLE_SECONDS * 2.0);
        app.update();
        assert_eq!(
            app.world().resource::<TutorialSession>().phase,
            TutorialPhase::Playing
        );

        app.world_mut()
            .entity_mut(dummy)
            .get_mut::<FighterActionState>()
            .unwrap()
            .action = FighterAction::Idle;
        advance_virtual_time(&mut app, TUTORIAL_SETTLE_STABLE_SECONDS);
        app.update();
        assert_eq!(
            app.world().resource::<TutorialSession>().phase,
            TutorialPhase::Success
        );
    }

    #[test]
    fn success_auto_advance_pauses_for_controller_reconnect() {
        let mut app = App::new();
        let mut session = TutorialSession::default();
        session.phase = TutorialPhase::Success;
        session.completion_state = TutorialCompletionState::Succeeded;
        let mut owners = GameplayPauseOwners::default();
        owners.set(GameplayPauseOwner::TutorialSuccess, true);
        app.insert_resource(session)
            .insert_resource(owners)
            .insert_resource(TutorialTransition::default())
            .insert_resource(Time::<Real>::default())
            .add_systems(Update, advance_tutorial_success);

        advance_real_time(&mut app, 0.9);
        app.update();
        assert_eq!(
            app.world().resource::<TutorialTransition>().pending_action,
            None
        );

        app.world_mut()
            .resource_mut::<GameplayPauseOwners>()
            .set(GameplayPauseOwner::ControllerReconnect, true);
        advance_real_time(&mut app, 0.5);
        app.update();
        assert!((app.world().resource::<TutorialSession>().success_elapsed - 0.9).abs() < 0.001);

        app.world_mut()
            .resource_mut::<GameplayPauseOwners>()
            .set(GameplayPauseOwner::ControllerReconnect, false);
        advance_real_time(&mut app, 0.21);
        app.update();
        assert_eq!(
            app.world().resource::<TutorialTransition>().pending_action,
            Some(TutorialTransitionAction::AdvanceStep { skipped: false })
        );
        assert!(
            app.world()
                .resource::<GameplayPauseOwners>()
                .contains(GameplayPauseOwner::TutorialTransition)
        );
    }

    #[test]
    fn success_confirm_has_a_minimum_display_time() {
        let mut session = TutorialSession {
            phase: TutorialPhase::Success,
            ..default()
        };
        session.success_elapsed = TUTORIAL_SUCCESS_MIN_SECONDS - 0.01;
        assert!(!tutorial_success_ready_for_confirm(&session));
        session.success_elapsed = TUTORIAL_SUCCESS_MIN_SECONDS;
        assert!(tutorial_success_ready_for_confirm(&session));
        session.phase = TutorialPhase::Playing;
        assert!(!tutorial_success_ready_for_confirm(&session));
    }

    #[test]
    fn fighter_recovery_rejects_active_air_and_dash_states() {
        let mut action = FighterActionState::default();
        let mut motor = FighterMotor::default();
        assert!(tutorial_fighter_has_recovered(&action, &motor));

        action.action = FighterAction::Jumping;
        assert!(!tutorial_fighter_has_recovered(&action, &motor));
        action.action = FighterAction::LandingRecovery;
        assert!(!tutorial_fighter_has_recovered(&action, &motor));
        action.action = FighterAction::Idle;
        motor.grounded = false;
        assert!(!tutorial_fighter_has_recovered(&action, &motor));
        motor.grounded = true;
        motor.dash_slide_timer = 0.1;
        assert!(!tutorial_fighter_has_recovered(&action, &motor));
    }

    #[test]
    fn special_spawn_waits_for_the_expected_active_effect() {
        assert!(!tutorial_special_activation_matches(
            TUTORIAL_PLAYER_ID,
            SpecialKind::Projectile,
            false,
            SpecialKind::Projectile,
        ));
        assert!(!tutorial_special_activation_matches(
            TUTORIAL_DUMMY_ID,
            SpecialKind::Projectile,
            true,
            SpecialKind::Projectile,
        ));
        assert!(!tutorial_special_activation_matches(
            TUTORIAL_PLAYER_ID,
            SpecialKind::Trap,
            true,
            SpecialKind::Projectile,
        ));
        assert!(tutorial_special_activation_matches(
            TUTORIAL_PLAYER_ID,
            SpecialKind::Projectile,
            true,
            SpecialKind::Projectile,
        ));
    }

    #[test]
    fn guarding_objective_requires_a_confirmed_guard_impact() {
        let mut app = App::new();
        let mut session = TutorialSession::default();
        session.start(TutorialChapterId::DefenseRecovery);
        session.step_index = 1;
        session.phase = TutorialPhase::Playing;
        session.reset_requested = false;
        let mut state = MatchState::default();
        state.phase = MatchPhase::Fighting;
        state.stocks = [STOCK_LIVES; FIGHTER_COUNT];
        app.insert_resource(session)
            .insert_resource(TutorialProgress::default())
            .insert_resource(state)
            .insert_resource(MatchTelemetry::default())
            .insert_resource(GameplayPauseOwners::default())
            .insert_resource(TutorialTransition::default())
            .insert_resource(UserModeState::default())
            .insert_resource(Time::<()>::default())
            .add_systems(Update, observe_tutorial_objective);
        let player = app
            .world_mut()
            .spawn((
                Fighter {
                    id: TUTORIAL_PLAYER_ID,
                    name: "Cat",
                    color: Color::WHITE,
                    spawn: Vec3::ZERO,
                },
                FighterInput {
                    guard: true,
                    ..default()
                },
                FighterActionState {
                    action: FighterAction::Guarding,
                    ..default()
                },
                FighterStats::default(),
                FighterMotor::default(),
                FighterGrabState::default(),
                Transform::default(),
                FighterInventory::default(),
            ))
            .id();

        app.update();
        assert_eq!(
            app.world().resource::<TutorialSession>().step_index,
            1,
            "holding guard without contact must not complete the lesson"
        );

        app.world_mut()
            .entity_mut(player)
            .get_mut::<FighterMotor>()
            .unwrap()
            .guard_counter_window_timer = 0.1;
        app.update();
        assert_eq!(app.world().resource::<TutorialSession>().step_index, 1);
        assert_eq!(
            app.world().resource::<TutorialSession>().completion_state,
            TutorialCompletionState::Settling
        );
        assert_eq!(
            app.world().resource::<TutorialTransition>().pending_action,
            None
        );

        {
            let mut entity = app.world_mut().entity_mut(player);
            entity.get_mut::<FighterInput>().unwrap().guard = false;
            entity.get_mut::<FighterActionState>().unwrap().action = FighterAction::Idle;
        }
        advance_virtual_time(&mut app, TUTORIAL_SETTLE_STABLE_SECONDS);
        app.update();
        assert_eq!(
            app.world().resource::<TutorialSession>().phase,
            TutorialPhase::Success
        );
    }

    #[test]
    fn grab_escape_requires_grab_then_guarded_movement_away() {
        let mut app = App::new();
        let mut session = TutorialSession::default();
        session.start(TutorialChapterId::DefenseRecovery);
        session.step_index = 4;
        session.phase = TutorialPhase::Playing;
        session.reset_requested = false;
        let mut state = MatchState::default();
        state.phase = MatchPhase::Fighting;
        state.stocks = [STOCK_LIVES; FIGHTER_COUNT];
        app.insert_resource(session)
            .insert_resource(TutorialProgress::default())
            .insert_resource(state)
            .insert_resource(MatchTelemetry::default())
            .insert_resource(GameplayPauseOwners::default())
            .insert_resource(TutorialTransition::default())
            .insert_resource(UserModeState::default())
            .insert_resource(Time::<()>::default())
            .add_systems(Update, observe_tutorial_objective);
        let dummy = app
            .world_mut()
            .spawn((
                Fighter {
                    id: TUTORIAL_DUMMY_ID,
                    name: "Pig",
                    color: Color::WHITE,
                    spawn: Vec3::X,
                },
                FighterInput::default(),
                FighterActionState::default(),
                FighterStats::default(),
                FighterMotor::default(),
                FighterGrabState::default(),
                Transform::from_xyz(1.0, 0.0, 0.0),
                FighterInventory::default(),
            ))
            .id();
        let player = app
            .world_mut()
            .spawn((
                Fighter {
                    id: TUTORIAL_PLAYER_ID,
                    name: "Cat",
                    color: Color::WHITE,
                    spawn: Vec3::NEG_X,
                },
                FighterInput {
                    movement: Vec2::NEG_X,
                    guard: true,
                    ..default()
                },
                FighterActionState {
                    action: FighterAction::Grabbed,
                    ..default()
                },
                FighterStats::default(),
                FighterMotor::default(),
                FighterGrabState {
                    held_by: Some(dummy),
                    ..default()
                },
                Transform::from_xyz(-1.0, 0.0, 0.0),
                FighterInventory::default(),
            ))
            .id();

        app.update();
        assert_eq!(app.world().resource::<TutorialSession>().step_index, 4);

        app.world_mut()
            .entity_mut(player)
            .get_mut::<FighterActionState>()
            .unwrap()
            .action = FighterAction::Idle;
        app.world_mut()
            .entity_mut(player)
            .get_mut::<FighterGrabState>()
            .unwrap()
            .held_by = None;
        advance_virtual_time(&mut app, TUTORIAL_SETTLE_STABLE_SECONDS);
        app.update();
        assert_eq!(app.world().resource::<TutorialSession>().step_index, 4);
        assert_eq!(
            app.world().resource::<TutorialSession>().phase,
            TutorialPhase::Success
        );
    }

    #[test]
    fn cleanup_restores_setup_but_keeps_the_tutorial_player_assignment() {
        let mut return_setup = LocalSetup::default();
        return_setup.set_rule(1);
        return_setup.arena_index = 7;
        return_setup.replay_seed = 0x1234;
        return_setup.set_character(TUTORIAL_PLAYER_ID, CharacterKind::Bee);
        return_setup.slots[TUTORIAL_PLAYER_ID].input = LocalInputAssignment::Keyboard(0);

        let mut session = TutorialSession::default();
        session.return_setup = Some(return_setup.clone());
        let mut setup = LocalSetup::default();
        setup.set_rule(2);
        setup.arena_index = 0;
        setup.set_character(TUTORIAL_PLAYER_ID, CharacterKind::Cat);
        let mut state = MatchState::default();

        restore_tutorial_setup(
            &mut session,
            &mut setup,
            LocalInputAssignment::Keyboard(2),
            &mut state,
        );

        assert_eq!(setup.rule_index, return_setup.rule_index);
        assert_eq!(setup.arena_index, return_setup.arena_index);
        assert_eq!(setup.replay_seed, return_setup.replay_seed);
        assert_eq!(
            setup.slots[TUTORIAL_PLAYER_ID].character,
            CharacterKind::Bee
        );
        assert_eq!(
            setup.slots[TUTORIAL_PLAYER_ID].input,
            LocalInputAssignment::Keyboard(2)
        );
        assert_eq!(state.phase, MatchPhase::Setup);
        assert_eq!(state.rule_index, setup.rule_index);
        assert_eq!(state.arena_index, setup.arena_index);
        assert!(session.return_setup.is_none());
    }

    #[test]
    fn final_exam_result_requires_player_stock_and_dummy_elimination() {
        let mut state = MatchState::default();
        state.stocks = [1, 0, 0, 0];
        assert!(tutorial_final_exam_won(&state));
        state.stocks = [0, 1, 0, 0];
        assert!(!tutorial_final_exam_won(&state));
        state.stocks = [1, 1, 0, 0];
        assert!(!tutorial_final_exam_won(&state));
    }

    #[test]
    fn scripted_dummy_pulses_are_timed_and_bounded() {
        assert!(scripted_input_pulse(0.0, 2.2));
        assert!(scripted_input_pulse(2.25, 2.2));
        assert!(!scripted_input_pulse(0.2, 2.2));
        assert!(!scripted_input_pulse(2.19, 2.2));
    }

    #[test]
    fn keyboard_and_controller_prompts_follow_active_assignment() {
        use bevy::ecs::system::SystemState;

        let bindings = PlayerKeyBindings::default();
        let step = &SPECIAL_STEPS[1];
        let mut world = World::new();
        let controller = world
            .spawn(ControllerDeviceInfo {
                display_name: "DualSense".to_string(),
                family: ControllerFamily::PlayStation,
                vendor_id: Some(0x054c),
                product_id: None,
                connected: true,
                haptics: crate::controller_haptics::HapticAvailability::Supported,
            })
            .id();
        let mut system_state: SystemState<Query<&ControllerDeviceInfo>> =
            SystemState::new(&mut world);
        let metadata = system_state.get(&world);

        let keyboard = tutorial_control_prompt(
            step,
            LocalInputAssignment::Keyboard(0),
            &bindings,
            &metadata,
        );
        let controller_prompt = tutorial_control_prompt(
            step,
            LocalInputAssignment::Gamepad(controller),
            &bindings,
            &metadata,
        );

        assert_eq!(keyboard, "E + C");
        assert_eq!(controller_prompt, "R1 + Square");
    }
}
