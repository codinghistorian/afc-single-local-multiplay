#![cfg_attr(test, allow(dead_code))]

use arrayvec::ArrayVec;
use bevy::ecs::system::{SystemParam, SystemState};
use bevy::prelude::*;
#[cfg(any(
    test,
    all(
        feature = "dev-hot-reload",
        not(feature = "shipping"),
        not(target_arch = "wasm32")
    )
))]
use bevy::window::PrimaryWindow;
use std::error::Error;
use std::fmt;

mod intelligence;
mod navigation;
mod tactics;

pub(crate) use intelligence::BotRuntimeStore;
#[cfg(all(
    feature = "bot-quality",
    feature = "native",
    not(target_arch = "wasm32")
))]
pub(crate) use intelligence::run_tactics_quality_fixture;
pub(crate) use navigation::BotNavigationCache;

use crate::arena::{
    ArenaHazardState, SplitCausewayDoorState, arena_hazard_affects_height,
    arena_hazard_is_active_for_kind_ticks, ground_support_for_arena_with_radius,
};
use crate::arena_defs::{ActiveArena, ArenaDefinition, ArenaHazardDefinition, ArenaHazardKind};
use crate::bot_profiles::BotProfileCatalog;
#[cfg(any(
    test,
    all(
        feature = "dev-hot-reload",
        not(feature = "shipping"),
        not(target_arch = "wasm32")
    )
))]
use crate::camera::ArenaCamera;
#[cfg(any(
    test,
    all(
        feature = "dev-hot-reload",
        not(feature = "shipping"),
        not(target_arch = "wasm32")
    )
))]
use crate::characters::{CharacterKind, character_label, next_character_kind};
use crate::characters::{CharacterMoveCatalog, FighterCharacter};
use crate::components::{
    BotBehaviorMode, BotBrain, BotMovementPlan, Controller, Fighter, FighterAction,
    FighterActionState, FighterContactState, FighterInput, FighterInventory, FighterMotor,
    FighterSpecialState, FighterStats, SimPosition,
};
#[cfg(any(
    test,
    all(
        feature = "dev-hot-reload",
        not(feature = "shipping"),
        not(target_arch = "wasm32")
    )
))]
use crate::constants::{ARENA_TOP_Y, FIGHTER_RADIUS};
use crate::constants::{
    ITEM_BREEZE_BUOY_STAMINA, ITEM_THROW_RADIUS, MAX_STAMINA, POP_BOMB_RADIUS, QUICK_STAND_AFTER,
    SHARED_SPECIALS_ENABLED, SPECIAL_HAZARD_RADIUS, SPECIAL_PROJECTILE_RADIUS,
    SPECIAL_SHOCKWAVE_RADIUS, SPECIAL_TRAP_RADIUS,
};
#[cfg(test)]
use crate::determinism::{DeterministicRngStream, RngStreamName};
use crate::determinism::{FighterId, SimEntityId, SimEntityKind, SimTick};
use crate::ecs_identity::{SIM_ENTITY_POOL_CAPACITIES, StableSimEntity};
use crate::equipment::{EquipmentKind, FighterEquipment};
#[cfg(any(
    test,
    all(
        feature = "dev-hot-reload",
        not(feature = "shipping"),
        not(target_arch = "wasm32")
    )
))]
use crate::game_state::MatchAnnouncements;
use crate::game_state::{Hitstop, MatchState};
use crate::items::{ArenaItem, ItemKind, ItemRole, ItemState};
use crate::live_input::fighter_input_to_network_input;
use crate::network_protocol::{
    InputButtons, InputFrame, InputSequence, MAX_SEATS, ProtocolValidationError, SeatOwner,
    SeatOwnership,
};
use crate::simulation::TickTimer;
use crate::specials::{ActiveSpecial, SpecialKind};
use crate::styles::FighterStyle;
#[cfg(test)]
use crate::styles::style_tuning;
use crate::user_mode::UserModeState;

#[cfg(any(
    test,
    all(
        feature = "dev-hot-reload",
        not(feature = "shipping"),
        not(target_arch = "wasm32")
    )
))]
const BOT_ACTION_SELECT_RADIUS: f32 = FIGHTER_RADIUS * 2.65;
const BOT_EDGE_WARNING_DISTANCE: f32 = 1.35;
#[cfg(test)]
const BOT_CHOICE_HOLD_TICKS: u64 = 12;
#[cfg(test)]
const BOT_ITEM_PICKUP_READY: TickTimer = TickTimer::from_millis_ceil(250);
#[cfg(test)]
const BOT_CLOSE_HEAVY_READY: TickTimer = TickTimer::from_millis_ceil(180);
const BOT_SPATIAL_QUANTIZATION: f32 = 1_024.0;
#[cfg(test)]
const BOT_CHOICE_STREAM_DOMAIN: u64 = 0x424f_545f_4348_4f49;
#[cfg(test)]
const BOT_CHOICE_TICK_DOMAIN: u64 = 0x9e37_79b9_7f4a_7c15;
#[cfg(test)]
const BOT_CHOICE_ID_DOMAIN: u64 = 0xbf58_476d_1ce4_e5b9;
const BOT_ITEM_SOURCE_CAPACITY: usize =
    SIM_ENTITY_POOL_CAPACITIES[SimEntityKind::Item.code() as usize] as usize;
const BOT_SPECIAL_SOURCE_CAPACITY: usize =
    SIM_ENTITY_POOL_CAPACITIES[SimEntityKind::Special.code() as usize] as usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StableSourceCollectionError {
    UnexpectedKind {
        expected: SimEntityKind,
        found: SimEntityKind,
    },
    IndexOutsidePool {
        id: SimEntityId,
        capacity: usize,
    },
    DuplicatePoolSlot {
        id: SimEntityId,
    },
    CapacityExceeded {
        kind: SimEntityKind,
        capacity: usize,
    },
}

fn try_push_stable_source<'a, T, const N: usize>(
    entries: &mut ArrayVec<(&'a StableSimEntity, T), N>,
    stable: &'a StableSimEntity,
    value: T,
    expected_kind: SimEntityKind,
) -> Result<(), StableSourceCollectionError> {
    let id = stable.id();
    if id.kind() != expected_kind {
        return Err(StableSourceCollectionError::UnexpectedKind {
            expected: expected_kind,
            found: id.kind(),
        });
    }
    if id.index() as usize >= N {
        return Err(StableSourceCollectionError::IndexOutsidePool { id, capacity: N });
    }
    if entries
        .iter()
        .any(|(existing, _)| existing.id().index() == id.index())
    {
        return Err(StableSourceCollectionError::DuplicatePoolSlot { id });
    }
    entries
        .try_push((stable, value))
        .map_err(|_| StableSourceCollectionError::CapacityExceeded {
            kind: expected_kind,
            capacity: N,
        })
}

/// Authored unit-circle samples. Gameplay edge probes use this table instead of
/// platform libm trigonometry.
const BOT_PROBE_DIRECTIONS: [(f32, f32); 16] = [
    (1.0, 0.0),
    (0.923_879_5, 0.382_683_43),
    (0.707_106_77, 0.707_106_77),
    (0.382_683_43, 0.923_879_5),
    (0.0, 1.0),
    (-0.382_683_43, 0.923_879_5),
    (-0.707_106_77, 0.707_106_77),
    (-0.923_879_5, 0.382_683_43),
    (-1.0, 0.0),
    (-0.923_879_5, -0.382_683_43),
    (-0.707_106_77, -0.707_106_77),
    (-0.382_683_43, -0.923_879_5),
    (0.0, -1.0),
    (0.382_683_43, -0.923_879_5),
    (0.707_106_77, -0.707_106_77),
    (0.923_879_5, -0.382_683_43),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
#[cfg(test)]
enum BotChoicePurpose {
    Mistake = 0x01,
    HeldItem = 0x02,
    SpecialRanged = 0x03,
    SpecialGrab = 0x04,
    SpecialGuard = 0x05,
    SpecialHeavy = 0x06,
    MovementPressure = 0x07,
    CloseGrab = 0x08,
    CloseJump = 0x09,
    CloseHeavy = 0x0a,
    CloseGuard = 0x0b,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(test)]
struct BotDecisionKey {
    replay_seed: u64,
    bot_id: usize,
    tick: SimTick,
}

#[cfg(test)]
impl BotDecisionKey {
    const fn new(replay_seed: u64, bot_id: usize, tick: SimTick) -> Self {
        Self {
            replay_seed,
            bot_id,
            tick,
        }
    }
}

#[cfg(any(
    test,
    all(
        feature = "dev-hot-reload",
        not(feature = "shipping"),
        not(target_arch = "wasm32")
    )
))]
#[derive(Resource, Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct BotActionControl {
    selected_bot_id: Option<usize>,
    jump_bot_id: Option<usize>,
    guard_bot_id: Option<usize>,
    refill_bot_id: Option<usize>,
    last_trace_signature: Option<u64>,
}

#[cfg(any(
    test,
    all(
        feature = "dev-hot-reload",
        not(feature = "shipping"),
        not(target_arch = "wasm32")
    )
))]
pub fn setup_bot_action_control(mut commands: Commands) {
    let mut control = BotActionControl::default();
    if let Ok(value) = std::env::var("AFC_BOT_TRACE_FIGHTER")
        && let Ok(fighter_number) = value.parse::<usize>()
        && (1..=crate::constants::FIGHTER_COUNT).contains(&fighter_number)
    {
        control.select(fighter_number - 1);
    }
    commands.insert_resource(control);
}

#[cfg(any(
    test,
    all(
        feature = "dev-hot-reload",
        not(feature = "shipping"),
        not(target_arch = "wasm32")
    )
))]
impl BotActionControl {
    fn select(&mut self, bot_id: usize) {
        if self.selected_bot_id != Some(bot_id) {
            self.last_trace_signature = None;
        }
        self.selected_bot_id = Some(bot_id);
        if self.jump_bot_id.is_some_and(|id| id != bot_id) {
            self.jump_bot_id = None;
        }
        if self.guard_bot_id.is_some_and(|id| id != bot_id) {
            self.guard_bot_id = None;
        }
    }

    pub fn refill_bot_id(&self) -> Option<usize> {
        self.refill_bot_id
    }

    #[cfg(test)]
    pub(crate) fn set_refill_bot_id_for_test(&mut self, bot_id: usize) {
        self.refill_bot_id = Some(bot_id);
    }

    fn select_and_toggle_refill(&mut self, bot_id: usize) -> bool {
        self.select(bot_id);
        if self.refill_bot_id == Some(bot_id) {
            self.refill_bot_id = None;
            false
        } else {
            self.refill_bot_id = Some(bot_id);
            true
        }
    }

    fn clear(&mut self) -> bool {
        let had_control = self.selected_bot_id.is_some()
            || self.jump_bot_id.is_some()
            || self.guard_bot_id.is_some()
            || self.refill_bot_id.is_some();
        self.selected_bot_id = None;
        self.jump_bot_id = None;
        self.guard_bot_id = None;
        self.refill_bot_id = None;
        self.last_trace_signature = None;
        had_control
    }

    fn toggle_jump(&mut self) -> Option<bool> {
        let bot_id = self.selected_bot_id?;
        self.guard_bot_id = None;
        if self.jump_bot_id == Some(bot_id) {
            self.jump_bot_id = None;
            Some(false)
        } else {
            self.jump_bot_id = Some(bot_id);
            Some(true)
        }
    }

    fn toggle_guard(&mut self) -> Option<bool> {
        let bot_id = self.selected_bot_id?;
        self.jump_bot_id = None;
        if self.guard_bot_id == Some(bot_id) {
            self.guard_bot_id = None;
            Some(false)
        } else {
            self.guard_bot_id = Some(bot_id);
            Some(true)
        }
    }

    fn activate_movement(&mut self) -> Option<usize> {
        if self.jump_bot_id.is_none() && self.guard_bot_id.is_none() {
            return self.selected_bot_id;
        }
        self.jump_bot_id = None;
        self.guard_bot_id = None;
        self.selected_bot_id
    }
}

#[derive(Clone, Copy, Debug)]
struct BotPersonality {
    aggression: f32,
    item_greed: f32,
    hazard_fear: f32,
    #[cfg(test)]
    special_bias: f32,
    #[cfg(test)]
    mistake_rate: f32,
    #[cfg(test)]
    panic_health: f32,
}

#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BotDifficulty {
    #[default]
    Standard,
    Tutorial,
}

#[cfg(test)]
impl BotDifficulty {
    fn attack_timer_advances(self, tick: SimTick) -> bool {
        match self {
            Self::Standard => true,
            // Tick 20 of every 33 committed ticks: the exact fixed-tick
            // equivalent of the authored 1.65x tutorial recovery.
            Self::Tutorial => tick.get() % 33 < 20,
        }
    }

    fn normal_grab_allowed(self) -> bool {
        matches!(self, Self::Standard)
    }
}

#[derive(Clone, Copy)]
struct BotTargetSnapshot {
    fighter_id: FighterId,
    position: Vec3,
    distance: f32,
    facing: Vec3,
    action: FighterAction,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct BotRangeBand {
    min: f32,
    ideal: f32,
    max: f32,
}

#[derive(Debug, PartialEq)]
enum BotRecoveryDecision {
    Wait,
    QuickStand,
    Roll(Vec2),
}

#[derive(Debug, PartialEq, Eq)]
enum BotHeldItemDecision {
    Light,
    Heavy,
}

#[cfg(any(
    test,
    all(
        feature = "dev-hot-reload",
        not(feature = "shipping"),
        not(target_arch = "wasm32")
    )
))]
pub fn bot_action_control_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window, With<PrimaryWindow>>,
    cameras: Query<(&Camera, &GlobalTransform), With<ArenaCamera>>,
    state: Res<MatchState>,
    user_mode: Res<UserModeState>,
    fighters: Query<(&Fighter, &Controller, &Transform)>,
    mut bot_characters: Query<(&Fighter, &mut FighterCharacter)>,
    mut bot_brains: Query<(&Fighter, &mut BotBrain)>,
    mut control: ResMut<BotActionControl>,
    mut announcements: ResMut<MatchAnnouncements>,
    simulation_drive: Res<crate::simulation::SimulationDriveMode>,
) {
    if *simulation_drive == crate::simulation::SimulationDriveMode::ExternalProjection {
        control.clear();
        return;
    }
    if user_mode.blocks_dev_input() {
        control.clear();
        return;
    }

    if mouse.just_pressed(MouseButton::Left) {
        let selected = cursor_floor_position(&windows, &cameras)
            .and_then(|cursor| selectable_bot_at_cursor(cursor, &state, &fighters));
        if let Some(bot_id) = selected {
            let refill_enabled = control.select_and_toggle_refill(bot_id);
            announcements.show(
                format!(
                    "Bot target: P{} | HP refill {} | 0 Jump  1 Guard  2 Animal  3 Move",
                    bot_id + 1,
                    if refill_enabled { "on" } else { "off" }
                ),
                1.0,
            );
        } else if control.clear() {
            announcements.show("Bot control cleared", 0.75);
        }
    }

    if keys.just_pressed(KeyCode::Digit0) {
        match control.toggle_jump() {
            Some(true) => {
                if let Some(bot_id) = control.selected_bot_id {
                    announcements.show(format!("Bot P{} repeat jump", bot_id + 1), 0.75);
                }
            }
            Some(false) => {
                if let Some(bot_id) = control.selected_bot_id {
                    announcements.show(format!("Bot P{} jump off", bot_id + 1), 0.75);
                }
            }
            None => announcements.show("Click a bot first", 0.65),
        }
    }

    if keys.just_pressed(KeyCode::Digit1) {
        match control.toggle_guard() {
            Some(true) => {
                if let Some(bot_id) = control.selected_bot_id {
                    announcements.show(format!("Bot P{} guard", bot_id + 1), 0.75);
                }
            }
            Some(false) => {
                if let Some(bot_id) = control.selected_bot_id {
                    announcements.show(format!("Bot P{} guard off", bot_id + 1), 0.75);
                }
            }
            None => announcements.show("Click a bot first", 0.65),
        }
    }

    if keys.just_pressed(KeyCode::Digit2) {
        match control.selected_bot_id {
            Some(bot_id) => match cycle_bot_character(bot_id, &mut bot_characters) {
                Some(next_character) => {
                    announcements.show(
                        format!(
                            "Bot P{} animal -> {}",
                            bot_id + 1,
                            character_label(next_character)
                        ),
                        0.75,
                    );
                }
                None => announcements.show("Bot not found", 0.65),
            },
            None => announcements.show("Click a bot first", 0.65),
        }
    }

    if keys.just_pressed(KeyCode::Digit3) {
        match control.activate_movement() {
            Some(bot_id) => {
                if enable_bot_ai_for_selected(bot_id, &mut bot_brains) {
                    announcements.show(format!("Bot P{} combat AI enabled", bot_id + 1), 0.75);
                } else {
                    announcements.show("Bot not found", 0.65);
                }
            }
            None => announcements.show("Click a bot first", 0.65),
        }
    }
}

#[derive(SystemParam)]
pub(crate) struct BotDecisionQueries<'w, 's> {
    bots: Query<
        'w,
        's,
        (
            &'static Fighter,
            &'static Controller,
            &'static mut FighterInput,
            &'static mut BotBrain,
            &'static FighterMotor,
            &'static FighterInventory,
            &'static SimPosition,
            &'static FighterSpecialState,
            &'static FighterCharacter,
            &'static FighterStyle,
            &'static FighterEquipment,
            &'static FighterStats,
            &'static FighterActionState,
            Option<&'static BotDifficulty>,
        ),
    >,
    all_fighters: Query<
        'w,
        's,
        (
            &'static Fighter,
            &'static SimPosition,
            &'static FighterActionState,
            &'static FighterMotor,
            &'static FighterStats,
            &'static FighterCharacter,
            &'static FighterStyle,
            &'static FighterEquipment,
            Option<&'static FighterContactState>,
        ),
    >,
    items: Query<'w, 's, (&'static StableSimEntity, &'static ArenaItem)>,
    specials: Query<
        'w,
        's,
        (
            &'static StableSimEntity,
            &'static ActiveSpecial,
            &'static SimPosition,
        ),
    >,
}

/// Presence means fixed simulation receives externally committed frames. The
/// authority-side generator is the only code allowed to advance bot brains;
/// rollback clients and replay drivers merely apply the recorded frames.
#[derive(Resource, Default)]
pub(crate) struct ExternallyStagedBotInput;

pub fn bot_input(
    tick: Res<crate::simulation::SimTick>,
    hitstop: Res<Hitstop>,
    state: Res<MatchState>,
    user_mode: Res<UserModeState>,
    active_arena: Res<ActiveArena>,
    hazard_state: Res<ArenaHazardState>,
    split_causeway_doors: Res<SplitCausewayDoorState>,
    profiles: Res<BotProfileCatalog>,
    move_catalog: Res<CharacterMoveCatalog>,
    mut bot_runtime: ResMut<BotRuntimeStore>,
    mut bot_navigation: ResMut<BotNavigationCache>,
    mut bot_snapshot: Local<intelligence::BotSnapshotBuffer>,
    external_input: Option<Res<ExternallyStagedBotInput>>,
    #[cfg(all(
        feature = "dev-hot-reload",
        not(feature = "shipping"),
        not(target_arch = "wasm32")
    ))]
    mut action_control: ResMut<BotActionControl>,
    mut queries: BotDecisionQueries,
) {
    if external_input.is_some() {
        return;
    }
    drive_bot_inputs(
        *tick,
        &hitstop,
        &state,
        bot_ai_special_inputs_allowed(&user_mode),
        &active_arena,
        &hazard_state,
        &split_causeway_doors,
        &profiles,
        &move_catalog,
        &mut bot_runtime,
        &mut bot_navigation,
        &mut bot_snapshot,
        None,
        #[cfg(all(
            feature = "dev-hot-reload",
            not(feature = "shipping"),
            not(target_arch = "wasm32")
        ))]
        Some(&mut action_control),
        &mut queries,
    );
}

fn drive_bot_inputs(
    _tick: SimTick,
    hitstop: &Hitstop,
    state: &MatchState,
    special_inputs_allowed: bool,
    active_arena: &ActiveArena,
    hazard_state: &ArenaHazardState,
    split_causeway_doors: &SplitCausewayDoorState,
    profiles: &BotProfileCatalog,
    move_catalog: &CharacterMoveCatalog,
    bot_runtime: &mut BotRuntimeStore,
    bot_navigation: &mut BotNavigationCache,
    bot_snapshot: &mut intelligence::BotSnapshotBuffer,
    authority_fighter_mask: Option<u8>,
    #[cfg(all(
        feature = "dev-hot-reload",
        not(feature = "shipping"),
        not(target_arch = "wasm32")
    ))]
    mut action_control: Option<&mut BotActionControl>,
    queries: &mut BotDecisionQueries,
) {
    if hitstop.active() {
        return;
    }

    let mut ordered_items = ArrayVec::<_, BOT_ITEM_SOURCE_CAPACITY>::new();
    let item_collection = queries.items.iter().try_for_each(|(stable, item)| {
        try_push_stable_source(&mut ordered_items, stable, item, SimEntityKind::Item)
    });
    if let Err(error) = item_collection {
        error!(?error, "bot item-source collection failed closed");
        for (_, _, mut input, ..) in &mut queries.bots {
            *input = FighterInput::default();
        }
        return;
    }
    sort_stable_entries(&mut ordered_items);
    let mut ordered_specials = ArrayVec::<_, BOT_SPECIAL_SOURCE_CAPACITY>::new();
    let special_collection =
        queries
            .specials
            .iter()
            .try_for_each(|(stable, special, transform)| {
                try_push_stable_source(
                    &mut ordered_specials,
                    stable,
                    (special, transform),
                    SimEntityKind::Special,
                )
            });
    if let Err(error) = special_collection {
        error!(?error, "bot special-source collection failed closed");
        for (_, _, mut input, ..) in &mut queries.bots {
            *input = FighterInput::default();
        }
        return;
    }
    sort_stable_entries(&mut ordered_specials);
    intelligence::build_world_snapshot(
        bot_snapshot,
        state,
        active_arena.index(),
        hazard_state.elapsed_ticks(),
        &queries.all_fighters,
        move_catalog,
        ordered_items.as_slice(),
        ordered_specials.as_slice(),
    );
    bot_runtime.begin_frame(state.replay_seed);
    for (
        bot,
        controller,
        mut input,
        mut brain,
        motor,
        inventory,
        transform,
        special_state,
        character,
        style,
        equipment,
        stats,
        action,
        difficulty,
    ) in &mut queries.bots
    {
        if !controller.is_bot() {
            continue;
        }
        let Some(bot_id) = FighterId::from_index(bot.id) else {
            continue;
        };
        if authority_fighter_mask.is_some_and(|mask| mask & (1 << bot_id.get()) == 0) {
            continue;
        }
        if !state.fighter_can_participate(bot.id) {
            *input = FighterInput::default();
            continue;
        }
        let difficulty = difficulty.copied().unwrap_or_default();
        *input = FighterInput::default();

        #[cfg(all(
            feature = "dev-hot-reload",
            not(feature = "shipping"),
            not(target_arch = "wasm32")
        ))]
        {
            if let Some(action_control) = action_control.as_deref_mut() {
                if let Some(forced_input) =
                    controlled_bot_input(action_control, bot.id, motor, action)
                {
                    *input = forced_input;
                    continue;
                }
            }
        }

        if !bot_should_drive_autonomous_inputs(brain.behavior) {
            brain.decision_timer.clear();
            brain.movement_plan_timer.clear();
            brain.dash_timer.clear();
            brain.attack_timer.clear();
            continue;
        }

        let held_kind = inventory.held.and_then(|held_id| {
            ordered_items
                .iter()
                .find(|(stable, _)| stable.id() == held_id)
                .map(|(_, item)| item.kind)
        });
        intelligence::drive_bot(
            active_arena.index(),
            bot.id,
            &mut brain,
            motor,
            transform.translation,
            special_state,
            character,
            style,
            equipment,
            stats,
            action,
            difficulty,
            held_kind,
            special_inputs_allowed,
            bot_snapshot,
            profiles,
            move_catalog,
            bot_runtime,
            bot_navigation,
            split_causeway_doors,
            &mut input,
        );
    }
}

#[derive(SystemParam)]
struct AuthorityBotWorld<'w, 's> {
    hitstop: Res<'w, Hitstop>,
    state: Res<'w, MatchState>,
    active_arena: Res<'w, ActiveArena>,
    hazard_state: Res<'w, ArenaHazardState>,
    split_causeway_doors: Res<'w, SplitCausewayDoorState>,
    profiles: Res<'w, BotProfileCatalog>,
    move_catalog: Res<'w, CharacterMoveCatalog>,
    bot_runtime: ResMut<'w, BotRuntimeStore>,
    bot_navigation: ResMut<'w, BotNavigationCache>,
    queries: BotDecisionQueries<'w, 's>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorityBotInputError {
    Protocol(ProtocolValidationError),
    Timeline { expected: SimTick, found: SimTick },
    WorldTick { expected: SimTick, found: SimTick },
    MissingBot(FighterId),
    DuplicateBot(FighterId),
    NonBotController(FighterId),
}

impl fmt::Display for AuthorityBotInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "authority bot input generation failed: {self:?}")
    }
}

impl Error for AuthorityBotInputError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Protocol(source) => Some(source),
            _ => None,
        }
    }
}

impl From<ProtocolValidationError> for AuthorityBotInputError {
    fn from(error: ProtocolValidationError) -> Self {
        Self::Protocol(error)
    }
}

/// Authority-owned, tick-addressed AI tape producer.
///
/// Retrying the same deadline returns the cached frame without advancing a
/// brain twice. Sequence numbers are a pure tick projection, so every bot tape
/// is stable and independent of entity or ownership iteration order.
pub(crate) struct AuthorityBotInputGenerator {
    world_state: SystemState<AuthorityBotWorld<'static, 'static>>,
    bot_snapshot: intelligence::BotSnapshotBuffer,
    last_generated_tick: Option<SimTick>,
    last_frames: [Option<InputFrame>; MAX_SEATS],
    cached_frames: [Option<InputFrame>; MAX_SEATS],
}

impl AuthorityBotInputGenerator {
    pub(crate) fn new(world: &mut World) -> Self {
        Self {
            world_state: SystemState::new(world),
            bot_snapshot: intelligence::BotSnapshotBuffer::default(),
            last_generated_tick: None,
            last_frames: [None; MAX_SEATS],
            cached_frames: [None; MAX_SEATS],
        }
    }

    pub(crate) fn generate(
        &mut self,
        world: &mut World,
        ownership: SeatOwnership,
        tick: SimTick,
    ) -> Result<[Option<InputFrame>; MAX_SEATS], AuthorityBotInputError> {
        ownership.validate()?;
        if self.last_generated_tick == Some(tick) {
            return Ok(self.cached_frames);
        }

        let world_tick =
            *world
                .get_resource::<SimTick>()
                .ok_or(AuthorityBotInputError::WorldTick {
                    expected: tick,
                    found: SimTick::ZERO,
                })?;
        let expected_world_tick = world_tick.next();
        if tick != expected_world_tick {
            return Err(AuthorityBotInputError::WorldTick {
                expected: expected_world_tick,
                found: tick,
            });
        }
        if let Some(last_tick) = self.last_generated_tick {
            let expected = last_tick.next();
            if tick != expected {
                return Err(AuthorityBotInputError::Timeline {
                    expected,
                    found: tick,
                });
            }
        }

        let mut authority_mask = 0_u8;
        for assignment in ownership.as_slice() {
            if assignment.owner == SeatOwner::AuthorityBot {
                authority_mask |= 1 << assignment.fighter.get();
            }
        }
        let prior_frames = self.last_frames;
        let frames = {
            let AuthorityBotWorld {
                hitstop,
                state,
                active_arena,
                hazard_state,
                split_causeway_doors,
                profiles,
                move_catalog,
                mut bot_runtime,
                mut bot_navigation,
                mut queries,
            } = self.world_state.get_mut(world);
            let paused = hitstop.active();

            // Validate the authority roster before touching either brain or
            // gameplay-facing input state. The generator is an authority
            // orchestration concern, not part of the canonical fixed step.
            let mut configured = [false; MAX_SEATS];
            for (fighter, controller, ..) in queries.bots.iter_mut() {
                let Some(fighter_id) = FighterId::from_index(fighter.id) else {
                    continue;
                };
                if authority_mask & (1 << fighter_id.get()) == 0 {
                    continue;
                }
                if !controller.is_bot() {
                    return Err(AuthorityBotInputError::NonBotController(fighter_id));
                }
                if configured[fighter_id.index()] {
                    return Err(AuthorityBotInputError::DuplicateBot(fighter_id));
                }
                configured[fighter_id.index()] = true;
            }
            for assignment in ownership.as_slice() {
                if assignment.owner == SeatOwner::AuthorityBot
                    && !configured[assignment.fighter.index()]
                {
                    return Err(AuthorityBotInputError::MissingBot(assignment.fighter));
                }
            }

            // `drive_bot_inputs` is shared with the local-training path and
            // writes FighterInput. Preserve those canonical components while
            // using them as a short-lived encoding buffer. Only BotBrain is
            // authority-private state advanced by this producer.
            let mut preserved_inputs: [Option<FighterInput>; MAX_SEATS] =
                std::array::from_fn(|_| None);
            for (fighter, _, mut input, ..) in queries.bots.iter_mut() {
                let Some(fighter_id) = FighterId::from_index(fighter.id) else {
                    continue;
                };
                if authority_mask & (1 << fighter_id.get()) == 0 {
                    continue;
                }
                preserved_inputs[fighter_id.index()] = Some(std::mem::take(&mut *input));
            }

            drive_bot_inputs(
                tick,
                &hitstop,
                &state,
                true,
                &active_arena,
                &hazard_state,
                &split_causeway_doors,
                &profiles,
                &move_catalog,
                &mut bot_runtime,
                &mut bot_navigation,
                &mut self.bot_snapshot,
                Some(authority_mask),
                #[cfg(all(
                    feature = "dev-hot-reload",
                    not(feature = "shipping"),
                    not(target_arch = "wasm32")
                ))]
                None,
                &mut queries,
            );

            let mut generated = [None; MAX_SEATS];
            let mut validation_error = None;
            for (fighter, controller, mut input, ..) in queries.bots.iter_mut() {
                let Some(fighter_id) = FighterId::from_index(fighter.id) else {
                    continue;
                };
                if authority_mask & (1 << fighter_id.get()) == 0 {
                    continue;
                }
                debug_assert!(controller.is_bot());
                let assignment = ownership
                    .assignment_for_fighter(fighter_id)
                    .expect("the validated authority mask came from this ownership");
                let seat_index = usize::from(assignment.seat.get());
                let sequence = InputSequence(tick.0 as u16);
                let frame = if paused {
                    let mut repeated = prior_frames[seat_index].unwrap_or_default();
                    repeated.tick = tick;
                    repeated.seat = assignment.seat;
                    repeated.sequence = sequence;
                    repeated.pressed_buttons = InputButtons::default();
                    repeated.released_buttons = InputButtons::default();
                    repeated
                } else {
                    fighter_input_to_network_input(&input, tick, assignment.seat, sequence)
                };
                if let Err(error) = frame.validate() {
                    validation_error.get_or_insert(error);
                } else {
                    generated[seat_index] = Some(frame);
                }
                *input = preserved_inputs[fighter_id.index()]
                    .take()
                    .expect("the validated authority bot input was preserved");
            }
            if let Some(error) = validation_error {
                return Err(error.into());
            }
            generated
        };

        self.last_generated_tick = Some(tick);
        self.last_frames = frames;
        self.cached_frames = frames;
        Ok(frames)
    }
}

#[cfg(test)]
fn nearest_bot_target(
    bot: FighterId,
    bot_position: Vec3,
    state: &MatchState,
    candidates: impl IntoIterator<Item = (FighterId, Vec3, FighterAction, Vec3)>,
) -> Option<BotTargetSnapshot> {
    let mut nearest: Option<BotTargetSnapshot> = None;
    for (fighter_id, position, action, facing) in candidates {
        if fighter_id == bot
            || !state.fighter_can_participate(fighter_id.index())
            || !state.combat_target_allowed_for_state(bot.index(), fighter_id.index())
            || matches!(action, FighterAction::RingOut | FighterAction::Respawning)
        {
            continue;
        }

        let delta = position - bot_position;
        let distance = deterministic_flat_distance(Vec2::new(delta.x, delta.z));
        if nearest.is_none_or(|target| {
            distance
                .total_cmp(&target.distance)
                .then_with(|| fighter_id.cmp(&target.fighter_id))
                .is_lt()
        }) {
            nearest = Some(BotTargetSnapshot {
                fighter_id,
                position,
                distance,
                facing,
                action,
            });
        }
    }
    nearest
}

fn sort_stable_entries<T>(entries: &mut [(&StableSimEntity, T)]) {
    entries.sort_unstable_by_key(|(stable, _)| stable.id());
}

#[cfg(any(
    test,
    all(
        feature = "dev-hot-reload",
        not(feature = "shipping"),
        not(target_arch = "wasm32")
    )
))]
fn controlled_bot_input(
    control: &mut BotActionControl,
    bot_id: usize,
    motor: &FighterMotor,
    action: &FighterActionState,
) -> Option<FighterInput> {
    if control.jump_bot_id == Some(bot_id) {
        return Some(FighterInput {
            jump: true,
            ..default()
        });
    }

    if control.guard_bot_id == Some(bot_id) {
        if !bot_guard_should_press(motor, action) {
            return Some(FighterInput::default());
        }
        return Some(FighterInput {
            guard: true,
            ..default()
        });
    }

    None
}

#[cfg(any(
    test,
    all(
        feature = "dev-hot-reload",
        not(feature = "shipping"),
        not(target_arch = "wasm32")
    )
))]
fn bot_guard_should_press(motor: &FighterMotor, action: &FighterActionState) -> bool {
    if action.action == FighterAction::Guarding {
        return true;
    }
    motor.grounded
        && !motor.guard_cooldown_timer.active()
        && !motor.guard_was_requested
        && matches!(action.action, FighterAction::Idle | FighterAction::Moving)
}

#[cfg(any(
    test,
    all(
        feature = "dev-hot-reload",
        not(feature = "shipping"),
        not(target_arch = "wasm32")
    )
))]
fn selectable_bot_at_cursor(
    cursor: Vec3,
    state: &MatchState,
    fighters: &Query<(&Fighter, &Controller, &Transform)>,
) -> Option<usize> {
    let mut selected_bot_id = None;
    let mut best_distance = BOT_ACTION_SELECT_RADIUS;
    for (fighter, controller, transform) in fighters {
        if !controller.is_bot() || !state.fighter_can_participate(fighter.id) {
            continue;
        }
        let distance = bot_selection_distance(cursor, transform.translation);
        if distance <= best_distance {
            best_distance = distance;
            selected_bot_id = Some(fighter.id);
        }
    }
    selected_bot_id
}

#[cfg(any(
    test,
    all(
        feature = "dev-hot-reload",
        not(feature = "shipping"),
        not(target_arch = "wasm32")
    )
))]
fn cycle_bot_character(
    bot_id: usize,
    bot_characters: &mut Query<(&Fighter, &mut FighterCharacter)>,
) -> Option<CharacterKind> {
    for (fighter, mut character) in bot_characters.iter_mut() {
        if fighter.id != bot_id {
            continue;
        }

        let next = next_character_kind(character.kind);
        character.kind = next;
        return Some(next);
    }
    None
}

#[cfg(any(
    test,
    all(
        feature = "dev-hot-reload",
        not(feature = "shipping"),
        not(target_arch = "wasm32")
    )
))]
fn enable_bot_ai_for_selected(
    bot_id: usize,
    bot_brains: &mut Query<(&Fighter, &mut BotBrain)>,
) -> bool {
    for (fighter, mut brain) in bot_brains.iter_mut() {
        if fighter.id != bot_id {
            continue;
        }
        start_bot_combat_ai(&mut brain);
        return true;
    }
    false
}

pub(crate) fn start_bot_combat_ai(brain: &mut BotBrain) {
    brain.behavior = BotBehaviorMode::Combatant;
    brain.decision_timer = bot_duration(0.05);
    brain.movement_plan_timer = bot_duration(0.05);
    brain.dash_timer = bot_duration(0.05);
    brain.attack_timer = bot_duration(0.05);
}

pub fn default_bot_brain_for_fighter(fighter_id: usize) -> BotBrain {
    BotBrain {
        // Authority ownership promotes the brain explicitly during deterministic
        // match bootstrap. Never let a process-local environment variable alter
        // a world that may be replayed or compared across machines.
        behavior: BotBehaviorMode::TrainingDummy,
        decision_timer: TickTimer::ZERO,
        movement_plan_timer: TickTimer::ZERO,
        dash_timer: bot_duration(0.7 + fighter_id as f32 * 0.45),
        attack_timer: bot_duration(0.25),
        strafe_sign: if fighter_id == 2 { 1.0 } else { -1.0 },
        movement_plan: BotMovementPlan::Circle,
    }
}

#[cfg(any(
    test,
    all(
        feature = "dev-hot-reload",
        not(feature = "shipping"),
        not(target_arch = "wasm32")
    )
))]
fn bot_selection_distance(cursor: Vec3, fighter_position: Vec3) -> f32 {
    deterministic_flat_distance(Vec2::new(
        cursor.x - fighter_position.x,
        cursor.z - fighter_position.z,
    ))
}

#[cfg(any(
    test,
    all(
        feature = "dev-hot-reload",
        not(feature = "shipping"),
        not(target_arch = "wasm32")
    )
))]
fn cursor_floor_position(
    windows: &Query<&Window, With<PrimaryWindow>>,
    cameras: &Query<(&Camera, &GlobalTransform), With<ArenaCamera>>,
) -> Option<Vec3> {
    let window = windows.iter().next()?;
    let cursor_position = window.cursor_position()?;
    let (camera, camera_transform) = cameras.iter().next()?;
    let ray = camera
        .viewport_to_world(camera_transform, cursor_position)
        .ok()?;
    ray.plane_intersection_point(
        Vec3::new(0.0, ARENA_TOP_Y + 0.04, 0.0),
        InfinitePlane3d::new(Vec3::Y),
    )
}

fn bot_should_drive_autonomous_inputs(behavior: BotBehaviorMode) -> bool {
    matches!(behavior, BotBehaviorMode::Combatant)
}

fn bot_ai_special_inputs_allowed(user_mode: &UserModeState) -> bool {
    SHARED_SPECIALS_ENABLED && !user_mode.restricts_bot_special_inputs()
}

/// Converts authored bot timing values into canonical simulation ticks at the
/// boundary where they enter authoritative state.
fn bot_duration(seconds: f32) -> TickTimer {
    TickTimer::from_seconds_ceil(seconds)
}

#[cfg(test)]
fn bot_timer_at_most(timer: TickTimer, threshold: TickTimer) -> bool {
    timer <= threshold
}

#[cfg(test)]
fn advance_bot_brain_timers(brain: &mut BotBrain, difficulty: BotDifficulty, tick: SimTick) {
    brain.decision_timer.tick();
    brain.movement_plan_timer.tick();
    brain.dash_timer.tick();
    if difficulty.attack_timer_advances(tick) {
        brain.attack_timer.tick();
    }
}

#[cfg(test)]
fn bot_choice_sample(key: BotDecisionKey, purpose: BotChoicePurpose) -> u32 {
    let choice_tick = key.tick.get() / BOT_CHOICE_HOLD_TICKS;
    let stream_code = BOT_CHOICE_STREAM_DOMAIN
        ^ (purpose as u64)
        ^ (key.bot_id as u64).wrapping_mul(BOT_CHOICE_ID_DOMAIN);
    let keyed_seed = key.replay_seed ^ choice_tick.wrapping_mul(BOT_CHOICE_TICK_DOMAIN);
    let mut stream =
        DeterministicRngStream::from_master_seed(keyed_seed, RngStreamName::from_code(stream_code));
    stream.next_u32()
}

#[cfg(test)]
fn bot_choice_ratio(
    key: BotDecisionKey,
    purpose: BotChoicePurpose,
    numerator: u32,
    denominator: u32,
) -> bool {
    if denominator == 0 || numerator == 0 {
        return false;
    }
    if numerator >= denominator {
        return true;
    }
    let threshold = (1u64 << 32) * u64::from(numerator) / u64::from(denominator);
    u64::from(bot_choice_sample(key, purpose)) < threshold
}

#[cfg(test)]
fn bot_choice_per_10k(key: BotDecisionKey, purpose: BotChoicePurpose, rate_per_10k: u32) -> bool {
    bot_choice_ratio(key, purpose, rate_per_10k.min(10_000), 10_000)
}

fn quantized_bot_component(value: f32) -> i64 {
    const MAX_COORDINATE: f32 = 1_000_000.0;
    (value.clamp(-MAX_COORDINATE, MAX_COORDINATE) * BOT_SPATIAL_QUANTIZATION) as i64
}

fn deterministic_integer_root(value: u64) -> u64 {
    let mut remainder = value;
    let mut result = 0u64;
    let mut bit = 1u64 << 62;
    while bit > remainder {
        bit >>= 2;
    }
    while bit != 0 {
        if remainder >= result + bit {
            remainder -= result + bit;
            result = (result >> 1) + bit;
        } else {
            result >>= 1;
        }
        bit >>= 2;
    }
    result
}

fn deterministic_flat_magnitude_units(flat: Vec2) -> (i64, i64, u64) {
    let x = quantized_bot_component(flat.x);
    let y = quantized_bot_component(flat.y);
    let squared = (i128::from(x) * i128::from(x) + i128::from(y) * i128::from(y)) as u64;
    (x, y, deterministic_integer_root(squared))
}

fn deterministic_flat_distance(flat: Vec2) -> f32 {
    let (_, _, magnitude) = deterministic_flat_magnitude_units(flat);
    magnitude as f32 / BOT_SPATIAL_QUANTIZATION
}

fn deterministic_normalize(flat: Vec2) -> Vec2 {
    let (x, y, magnitude) = deterministic_flat_magnitude_units(flat);
    if magnitude == 0 {
        return Vec2::ZERO;
    }
    Vec2::new(x as f32 / magnitude as f32, y as f32 / magnitude as f32)
}

fn bot_recovery_decision(
    elapsed: f32,
    bot_position: Vec3,
    target: Option<BotTargetSnapshot>,
) -> BotRecoveryDecision {
    if elapsed < QUICK_STAND_AFTER {
        return BotRecoveryDecision::Wait;
    }

    if let Some(target) = target {
        if target.distance < 3.2 {
            return BotRecoveryDecision::Roll(defensive_away_from(bot_position, target.position));
        }
    }

    BotRecoveryDecision::QuickStand
}

fn bot_should_guard_threat(bot_position: Vec3, target: BotTargetSnapshot) -> bool {
    debug_assert!(target.fighter_id.index() < crate::constants::FIGHTER_COUNT);
    let Some(range) = guard_threat_range(target.action) else {
        return false;
    };
    if target.distance > range {
        return false;
    }

    let target_to_bot = deterministic_normalize(Vec2::new(
        bot_position.x - target.position.x,
        bot_position.z - target.position.z,
    ));
    deterministic_normalize(Vec2::new(target.facing.x, target.facing.z)).dot(target_to_bot) > 0.2
}

fn bot_personality(
    style: crate::styles::FighterStyleKind,
    equipment: EquipmentKind,
) -> BotPersonality {
    let mut personality = match style {
        crate::styles::FighterStyleKind::Anchor => BotPersonality {
            aggression: 0.92,
            item_greed: 0.9,
            hazard_fear: 1.1,
            #[cfg(test)]
            special_bias: 0.85,
            #[cfg(test)]
            mistake_rate: 0.08,
            #[cfg(test)]
            panic_health: 34.0,
        },
        crate::styles::FighterStyleKind::Vector => BotPersonality {
            aggression: 1.16,
            item_greed: 0.96,
            hazard_fear: 0.92,
            #[cfg(test)]
            special_bias: 1.0,
            #[cfg(test)]
            mistake_rate: 0.12,
            #[cfg(test)]
            panic_health: 24.0,
        },
        crate::styles::FighterStyleKind::Catalyst => BotPersonality {
            aggression: 0.98,
            item_greed: 0.86,
            hazard_fear: 1.0,
            #[cfg(test)]
            special_bias: 1.22,
            #[cfg(test)]
            mistake_rate: 0.07,
            #[cfg(test)]
            panic_health: 28.0,
        },
    };

    match equipment {
        EquipmentKind::DashCoil => personality.aggression *= 1.06,
        EquipmentKind::AerialSpur => {
            #[cfg(test)]
            {
                personality.mistake_rate *= 1.03;
            }
        }
        EquipmentKind::CounterCell => personality.hazard_fear *= 1.08,
        EquipmentKind::HeavySeal => personality.item_greed *= 1.08,
    }

    personality
}

#[cfg(test)]
fn bot_personality_for_difficulty(
    style: crate::styles::FighterStyleKind,
    equipment: EquipmentKind,
    difficulty: BotDifficulty,
) -> BotPersonality {
    let mut personality = bot_personality(style, equipment);
    if difficulty == BotDifficulty::Tutorial {
        personality.aggression *= 0.62;
        personality.item_greed *= 0.82;
        personality.special_bias *= 0.7;
        personality.mistake_rate = (personality.mistake_rate + 0.24).clamp(0.24, 0.38);
        personality.panic_health += 12.0;
    }
    personality
}

#[cfg(test)]
fn bot_should_panic(health: f32, personality: BotPersonality) -> bool {
    health <= personality.panic_health
}

#[cfg(test)]
fn bot_should_make_mistake(key: BotDecisionKey, personality: BotPersonality) -> bool {
    let rate_per_10k = (personality.mistake_rate.clamp(0.0, 1.0) * 10_000.0) as u32;
    bot_choice_per_10k(key, BotChoicePurpose::Mistake, rate_per_10k)
}

fn guard_threat_range(action: FighterAction) -> Option<f32> {
    match action {
        FighterAction::LightAttack1 | FighterAction::LightAttack2 => Some(1.85),
        FighterAction::ComboFinisher | FighterAction::HeavyAttack | FighterAction::HeavyAttack2 => {
            Some(2.25)
        }
        FighterAction::DashAttack | FighterAction::JumpAttack | FighterAction::JumpHeavyAttack => {
            Some(2.45)
        }
        FighterAction::ItemSwing | FighterAction::ItemThrow => Some(2.6),
        FighterAction::SpecialCast => Some(3.1),
        FighterAction::GuardCounter => Some(1.8),
        _ => None,
    }
}

fn special_avoid_radius(kind: SpecialKind) -> Option<f32> {
    match kind {
        SpecialKind::Projectile => Some(SPECIAL_PROJECTILE_RADIUS + 1.05),
        SpecialKind::Trap => Some(SPECIAL_TRAP_RADIUS + 0.85),
        SpecialKind::Shockwave => Some(SPECIAL_SHOCKWAVE_RADIUS + 0.8),
        SpecialKind::Hazard => Some(SPECIAL_HAZARD_RADIUS + 1.15),
    }
}

fn item_avoidance_radius(item: &ArenaItem) -> Option<(usize, f32)> {
    match item.state {
        ItemState::Thrown { owner, .. } => Some((owner.index(), ITEM_THROW_RADIUS + 0.9)),
        ItemState::Armed { owner, .. } => Some((owner.index(), POP_BOMB_RADIUS + 0.65)),
        _ => None,
    }
}

fn bot_held_item_decision(
    kind: ItemKind,
    stamina: f32,
    distance: f32,
    attack_timer: TickTimer,
    use_consumable: bool,
) -> Option<BotHeldItemDecision> {
    if attack_timer.active() {
        return None;
    }

    match kind {
        ItemKind::Crate => return Some(BotHeldItemDecision::Heavy),
        ItemKind::Apple | ItemKind::Turkey => return Some(BotHeldItemDecision::Light),
        ItemKind::WineWhite | ItemKind::Barrel => {
            if stamina <= MAX_STAMINA - 10.0 {
                return Some(BotHeldItemDecision::Light);
            }
            return None;
        }
        ItemKind::CupCoffee | ItemKind::Mushroom => {
            if use_consumable {
                return Some(BotHeldItemDecision::Light);
            }
        }
        ItemKind::Steamer => {}
    }

    match kind.role() {
        ItemRole::Explosive => {
            if distance < 1.15 {
                Some(BotHeldItemDecision::Light)
            } else if (1.6..5.0).contains(&distance) {
                Some(BotHeldItemDecision::Heavy)
            } else {
                None
            }
        }
        ItemRole::Utility => {
            if stamina <= MAX_STAMINA - ITEM_BREEZE_BUOY_STAMINA * 0.45 {
                Some(BotHeldItemDecision::Light)
            } else {
                None
            }
        }
        ItemRole::Recovery => Some(BotHeldItemDecision::Light),
    }
}

#[cfg(test)]
fn bot_held_item_recovery(decision: BotHeldItemDecision) -> TickTimer {
    bot_duration(match decision {
        BotHeldItemDecision::Light => 0.72,
        BotHeldItemDecision::Heavy => 1.12,
    })
}

fn bot_pickup_score(kind: ItemKind, stamina: f32, target_distance: f32, item_distance: f32) -> f32 {
    let role_bonus = match kind.role() {
        ItemRole::Recovery => 0.2,
        ItemRole::Utility if stamina <= MAX_STAMINA - 10.0 => 0.28,
        ItemRole::Utility if stamina > MAX_STAMINA - 7.0 => -0.12,
        ItemRole::Explosive if (1.6..5.2).contains(&target_distance) => 0.18,
        _ => 0.0,
    };

    kind.bot_pickup_priority() + role_bonus - item_distance * 0.2
}

fn bot_range_band(preferred_range: f32, personality: BotPersonality) -> BotRangeBand {
    // First-order inverse-square-root approximation around aggression 1.0.
    // Personalities stay close to that point, preserving the legacy spacing
    // relationship without platform libm in an authoritative decision.
    let aggression_scale = (1.5 - personality.aggression * 0.5).clamp(0.72, 1.22);
    let ideal = (preferred_range * aggression_scale).clamp(1.05, 2.65);
    BotRangeBand {
        min: (ideal * 0.68).clamp(0.85, 1.55),
        ideal,
        max: ideal * 1.45 + 0.35,
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn choose_bot_movement_plan_for_arena(
    bot_position: Vec3,
    target_position: Vec3,
    distance: f32,
    range: BotRangeBand,
    personality: BotPersonality,
    health: f32,
    decision_key: BotDecisionKey,
    arena: &ArenaDefinition,
) -> BotMovementPlan {
    if bot_should_panic(health, personality) && distance < range.max + 0.9 {
        return BotMovementPlan::Retreat;
    }
    if distance > range.max {
        return BotMovementPlan::Approach;
    }
    if distance < range.min {
        return BotMovementPlan::Backstep;
    }

    let bot_edge = edge_danger_for_arena(bot_position, arena);
    let target_edge = edge_danger_for_arena(target_position, arena);
    if target_edge > 0.3 && bot_edge < target_edge {
        return BotMovementPlan::Pressure;
    }
    if bot_edge > 0.55 {
        return BotMovementPlan::Circle;
    }

    if distance < range.ideal * 1.15
        && bot_choice_ratio(decision_key, BotChoicePurpose::MovementPressure, 1, 4)
    {
        BotMovementPlan::Pressure
    } else {
        BotMovementPlan::Circle
    }
}

#[cfg(test)]
fn bot_movement_plan_duration(
    plan: BotMovementPlan,
    bot_id: usize,
    personality: BotPersonality,
) -> TickTimer {
    let base = match plan {
        BotMovementPlan::Approach => 0.38,
        BotMovementPlan::Circle => 0.55,
        BotMovementPlan::Backstep => 0.28,
        BotMovementPlan::Pressure => 0.34,
        BotMovementPlan::Retreat => 0.42,
    };
    bot_duration(base / personality.aggression.clamp(0.75, 1.35) + bot_id as f32 * 0.015)
}

#[cfg(test)]
fn bot_tactical_movement(
    plan: BotMovementPlan,
    bot_position: Vec3,
    target_position: Vec3,
    toward: Vec2,
    strafe: Vec2,
    distance: f32,
    range: BotRangeBand,
    arena: &ArenaDefinition,
) -> Vec2 {
    let movement = match plan {
        BotMovementPlan::Approach => {
            let strafe_weight = if distance > range.max + 0.9 {
                0.12
            } else {
                0.28
            };
            toward + strafe * strafe_weight
        }
        BotMovementPlan::Circle => {
            let radial = if distance < range.min {
                -toward * 0.45
            } else if distance > range.ideal {
                toward * 0.35
            } else {
                Vec2::ZERO
            };
            radial + strafe * 0.95
        }
        BotMovementPlan::Backstep => -toward * 0.72 + strafe * 0.45,
        BotMovementPlan::Pressure => {
            let target_edge = edge_danger_for_arena(target_position, arena);
            let strafe_weight = if target_edge > 0.0 { 0.16 } else { 0.34 };
            toward * 0.78 + strafe * strafe_weight
        }
        BotMovementPlan::Retreat => -toward * 0.82 + strafe * 0.28,
    };
    apply_edge_steering_for_arena(bot_position, deterministic_normalize(movement), arena)
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
fn bot_should_dash_for_movement_for_arena(
    plan: BotMovementPlan,
    bot_position: Vec3,
    movement: Vec2,
    distance: f32,
    range: BotRangeBand,
    grounded: bool,
    dash_timer: TickTimer,
    arena: &ArenaDefinition,
) -> bool {
    if !grounded
        || dash_timer.active()
        || movement.length_squared() <= 0.01
        || movement_points_toward_edge_for_arena(bot_position, movement, arena)
    {
        return false;
    }

    match plan {
        BotMovementPlan::Approach => distance > range.max + 0.45,
        BotMovementPlan::Pressure => distance > range.ideal + 0.35 && distance < range.max + 1.4,
        BotMovementPlan::Backstep | BotMovementPlan::Retreat => distance < range.min + 0.25,
        BotMovementPlan::Circle => false,
    }
}

#[cfg(test)]
fn bot_movement_dash_cooldown(plan: BotMovementPlan, bot_id: usize) -> TickTimer {
    let base = match plan {
        BotMovementPlan::Approach => 1.45,
        BotMovementPlan::Pressure => 1.65,
        BotMovementPlan::Backstep | BotMovementPlan::Retreat => 1.25,
        BotMovementPlan::Circle => 2.2,
    };
    bot_duration(base + bot_id as f32 * 0.12)
}

fn edge_danger_for_arena(position: Vec3, arena: &ArenaDefinition) -> f32 {
    if !arena_point_supported(arena, position.x, position.z) {
        return 1.0;
    }

    const PROBE_STEPS: usize = 6;
    for step in 1..=PROBE_STEPS {
        let distance = BOT_EDGE_WARNING_DISTANCE * step as f32 / PROBE_STEPS as f32;
        for (x, y) in BOT_PROBE_DIRECTIONS {
            let probe = Vec2::new(position.x, position.z) + Vec2::new(x, y) * distance;
            if !arena_point_supported(arena, probe.x, probe.y) {
                return 1.0 - (step.saturating_sub(1) as f32 / PROBE_STEPS as f32);
            }
        }
    }

    0.0
}

fn edge_inward_direction_for_arena(position: Vec3, arena: &ArenaDefinition) -> Vec2 {
    let origin = Vec2::new(position.x, position.z);
    let mut inward = Vec2::ZERO;
    for (x, y) in BOT_PROBE_DIRECTIONS {
        let direction = Vec2::new(x, y);
        for (distance, weight) in [(0.65, 0.7), (1.25, 1.0), (1.9, 1.35)] {
            let probe = origin + direction * distance;
            if arena_point_supported(arena, probe.x, probe.y) {
                inward += direction * weight;
            }
        }
    }

    if inward.length_squared() > 0.001 {
        deterministic_normalize(inward)
    } else {
        let arena_center = arena
            .spawn_points
            .iter()
            .map(|point| Vec2::new(point.x, point.z))
            .sum::<Vec2>()
            / arena.spawn_points.len() as f32;
        deterministic_normalize(arena_center - origin)
    }
}

#[cfg(test)]
fn movement_points_toward_edge_for_arena(
    position: Vec3,
    movement: Vec2,
    arena: &ArenaDefinition,
) -> bool {
    let movement = deterministic_normalize(movement);
    if movement.length_squared() <= 0.001 {
        return false;
    }
    let probe = Vec2::new(position.x, position.z) + movement * BOT_EDGE_WARNING_DISTANCE;
    !arena_point_supported(arena, probe.x, probe.y)
        || (edge_danger_for_arena(position, arena) > 0.0
            && movement.dot(edge_inward_direction_for_arena(position, arena)) < -0.25)
}

fn apply_edge_steering_for_arena(position: Vec3, movement: Vec2, arena: &ArenaDefinition) -> Vec2 {
    let movement = deterministic_normalize(movement);
    let movement_probe = Vec2::new(position.x, position.z) + movement * BOT_EDGE_WARNING_DISTANCE;
    let points_off_stage = movement.length_squared() > 0.001
        && !arena_point_supported(arena, movement_probe.x, movement_probe.y);
    let danger =
        edge_danger_for_arena(position, arena).max(if points_off_stage { 0.85 } else { 0.0 });
    if danger <= 0.0 {
        return movement;
    }

    let inward = edge_inward_direction_for_arena(position, arena);
    if movement.length_squared() <= 0.01 {
        return inward;
    }
    deterministic_normalize(movement * (1.0 - danger * 0.85) + inward * (0.75 + danger * 0.9))
}

fn arena_point_supported(arena: &ArenaDefinition, x: f32, z: f32) -> bool {
    ground_support_for_arena_with_radius(arena, x, z, 0.0)
        .height()
        .is_some()
}

fn bot_should_jump_for_elevation(
    position: Vec3,
    movement: Vec2,
    grounded: bool,
    arena: &ArenaDefinition,
) -> bool {
    if !grounded || movement.length_squared() <= 0.01 {
        return false;
    }

    let Some(current_height) =
        ground_support_for_arena_with_radius(arena, position.x, position.z, 0.0).height()
    else {
        return false;
    };
    let probe = Vec2::new(position.x, position.z) + deterministic_normalize(movement) * 1.05;
    let Some(next_height) =
        ground_support_for_arena_with_radius(arena, probe.x, probe.y, 0.0).height()
    else {
        return false;
    };
    let rise = next_height - current_height;
    rise >= 0.3 && rise <= 0.75
}

fn arena_hazard_avoidance(
    position: Vec3,
    hazard_elapsed: crate::simulation::ElapsedTicks,
    hazards: &[ArenaHazardDefinition],
    hazard_fear: f32,
) -> Vec2 {
    let mut avoidance = Vec2::ZERO;
    for hazard in hazards {
        if !arena_hazard_is_active_for_kind_ticks(hazard_elapsed, hazard)
            || !arena_hazard_affects_height(hazard, position.y)
        {
            continue;
        }
        let flat = Vec2::new(position.x - hazard.center.x, position.z - hazard.center.z);
        let avoid_radius = arena_hazard_avoid_radius(hazard) * hazard_fear;
        let flat_distance = deterministic_flat_distance(flat);
        if flat_distance < avoid_radius {
            avoidance += away_from_flat(flat) * (avoid_radius - flat_distance);
        }
    }
    avoidance
}

fn arena_hazard_avoid_radius(hazard: &ArenaHazardDefinition) -> f32 {
    hazard.radius
        + match hazard.kind {
            ArenaHazardKind::PulseVent => 1.15,
            ArenaHazardKind::SnareField => 0.8,
            ArenaHazardKind::BumperNode => 1.35,
            ArenaHazardKind::Campfire => 1.0,
            ArenaHazardKind::SawBlade => 1.25,
        }
}

fn defensive_away_from(bot_position: Vec3, target_position: Vec3) -> Vec2 {
    away_from_flat(Vec2::new(
        bot_position.x - target_position.x,
        bot_position.z - target_position.z,
    ))
}

fn away_from_flat(flat: Vec2) -> Vec2 {
    if flat.length_squared() > 0.001 {
        deterministic_normalize(flat)
    } else {
        Vec2::X
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{LocalInputAssignment, ParticipantKind, PlayerSlotId};
    use crate::game_state::MatchPhase;
    use crate::network_protocol::{PeerId, SeatAssignment, SeatId};

    fn authority_bot_fixture(
        spawn_order: [usize; 3],
        reverse_ownership: bool,
    ) -> (World, AuthorityBotInputGenerator, SeatOwnership) {
        let mut world = World::new();
        let active_arena = ActiveArena::default();
        let mut state = MatchState::default();
        state.phase = MatchPhase::Fighting;
        state.replay_seed = 0xAFC0_7711;
        state.set_active_slots([true, true, true, false]);
        world.insert_resource(SimTick::ZERO);
        world.insert_resource(Hitstop::default());
        world.insert_resource(ArenaHazardState::new(
            active_arena.index(),
            active_arena.definition().hazards.len(),
        ));
        world.insert_resource(active_arena);
        world.insert_resource(state);
        world.insert_resource(SplitCausewayDoorState::default());
        world.insert_resource(BotProfileCatalog::from_embedded_gameplay().unwrap());
        world.insert_resource(CharacterMoveCatalog::from_embedded_gameplay().unwrap());
        world.insert_resource(BotRuntimeStore::default());
        world.insert_resource(BotNavigationCache::default());

        let positions = [
            Vec3::ZERO,
            Vec3::new(-2.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
        ];
        for fighter_id in spawn_order {
            let is_bot = fighter_id != 0;
            let controller = Controller::new(
                PlayerSlotId::new(fighter_id).unwrap(),
                if is_bot {
                    ParticipantKind::Bot
                } else {
                    ParticipantKind::Human
                },
                LocalInputAssignment::Unassigned,
            );
            let entity = world
                .spawn((
                    Fighter {
                        id: fighter_id,
                        name: "Authority bot fixture",
                        color: Color::WHITE,
                        spawn: positions[fighter_id],
                    },
                    controller,
                    FighterInput::default(),
                    FighterMotor::default(),
                    FighterInventory::default(),
                    SimPosition::new(positions[fighter_id]),
                    FighterSpecialState::default(),
                    FighterCharacter::new(crate::characters::CharacterKind::Cat),
                    FighterStyle {
                        kind: crate::styles::FighterStyleKind::Anchor,
                    },
                    FighterEquipment::new(EquipmentKind::DashCoil),
                    FighterStats::default(),
                    FighterActionState::default(),
                ))
                .id();
            if is_bot {
                let mut brain = default_bot_brain_for_fighter(fighter_id);
                start_bot_combat_ai(&mut brain);
                world.entity_mut(entity).insert(brain);
            }
        }

        let peer = PeerId::new(77).unwrap();
        let mut assignments = [
            SeatAssignment {
                seat: SeatId::new(0).unwrap(),
                fighter: FighterId::ZERO,
                owner: SeatOwner::Peer(peer),
            },
            SeatAssignment {
                seat: SeatId::new(1).unwrap(),
                fighter: FighterId::new(1).unwrap(),
                owner: SeatOwner::AuthorityBot,
            },
            SeatAssignment {
                seat: SeatId::new(2).unwrap(),
                fighter: FighterId::new(2).unwrap(),
                owner: SeatOwner::AuthorityBot,
            },
        ];
        if reverse_ownership {
            assignments.reverse();
        }
        let ownership = SeatOwnership::from_assignments(&assignments).unwrap();
        let generator = AuthorityBotInputGenerator::new(&mut world);
        (world, generator, ownership)
    }

    fn bot_brain_state(
        world: &World,
        fighter_id: usize,
    ) -> (
        BotBehaviorMode,
        TickTimer,
        TickTimer,
        TickTimer,
        TickTimer,
        u32,
        BotMovementPlan,
    ) {
        for archetype in world.archetypes().iter() {
            for entry in archetype.entities() {
                let entity = entry.id();
                if world
                    .get::<Fighter>(entity)
                    .is_some_and(|fighter| fighter.id == fighter_id)
                {
                    let brain = world.get::<BotBrain>(entity).unwrap();
                    return (
                        brain.behavior,
                        brain.decision_timer,
                        brain.movement_plan_timer,
                        brain.dash_timer,
                        brain.attack_timer,
                        brain.strafe_sign.to_bits(),
                        brain.movement_plan,
                    );
                }
            }
        }
        panic!("missing bot fighter {fighter_id}")
    }

    #[test]
    fn authority_bot_tapes_are_order_independent_and_advance_once_per_tick() {
        let (mut first_world, mut first, first_ownership) = authority_bot_fixture([0, 1, 2], false);
        let (mut reversed_world, mut reversed, reversed_ownership) =
            authority_bot_fixture([2, 1, 0], true);

        for tick in 1..=48 {
            let tick = SimTick(tick);
            let first_frames = first
                .generate(&mut first_world, first_ownership, tick)
                .unwrap();
            let first_brains = [
                bot_brain_state(&first_world, 1),
                bot_brain_state(&first_world, 2),
            ];
            assert_eq!(
                first
                    .generate(&mut first_world, first_ownership, tick)
                    .unwrap(),
                first_frames,
                "same-tick retry must return the cached tape"
            );
            assert_eq!(
                [
                    bot_brain_state(&first_world, 1),
                    bot_brain_state(&first_world, 2),
                ],
                first_brains,
                "same-tick retry must not advance either brain"
            );

            let reversed_frames = reversed
                .generate(&mut reversed_world, reversed_ownership, tick)
                .unwrap();
            assert_eq!(first_frames, reversed_frames);
            for seat in [1, 2] {
                let frame = first_frames[seat].unwrap();
                assert_eq!(frame.tick, tick);
                assert_eq!(frame.seat, SeatId::new(seat as u8).unwrap());
                assert_eq!(frame.sequence, InputSequence(tick.0 as u16));
                frame.validate().unwrap();
            }
            assert!(first_frames[0].is_none());

            *first_world.resource_mut::<SimTick>() = tick;
            *reversed_world.resource_mut::<SimTick>() = tick;
        }
    }

    #[test]
    fn default_bot_brain_has_no_process_environment_behavior_switch() {
        assert_eq!(
            default_bot_brain_for_fighter(1).behavior,
            BotBehaviorMode::TrainingDummy
        );
    }

    fn target(position: Vec3, facing: Vec3, action: FighterAction) -> BotTargetSnapshot {
        BotTargetSnapshot {
            fighter_id: FighterId::ZERO,
            position,
            distance: deterministic_flat_distance(Vec2::new(position.x, position.z)),
            facing,
            action,
        }
    }

    fn nearest_target_for_spawn_order(order: [usize; 3]) -> FighterId {
        let positions = [Vec3::ZERO, Vec3::X, Vec3::NEG_X];
        let mut world = World::new();
        for fighter_id in order {
            world.spawn((
                Fighter {
                    id: fighter_id,
                    name: "Bot target fixture",
                    color: Color::WHITE,
                    spawn: positions[fighter_id],
                },
                SimPosition::new(positions[fighter_id]),
                FighterActionState::default(),
                FighterMotor::default(),
            ));
        }
        let mut state = MatchState::default();
        state.rules = crate::game_state::RULE_PRESETS[1];
        state.set_active_slots([true, true, true, false]);
        let mut fighters =
            world.query::<(&Fighter, &SimPosition, &FighterActionState, &FighterMotor)>();
        nearest_bot_target(
            FighterId::ZERO,
            Vec3::ZERO,
            &state,
            fighters
                .iter(&world)
                .map(|(fighter, transform, action, motor)| {
                    (
                        FighterId::from_index(fighter.id).unwrap(),
                        transform.translation,
                        action.action,
                        motor.facing,
                    )
                }),
        )
        .unwrap()
        .fighter_id
    }

    #[test]
    fn equal_distance_bot_target_uses_fighter_id_when_entity_order_is_reversed() {
        let forward = nearest_target_for_spawn_order([0, 1, 2]);
        let reversed = nearest_target_for_spawn_order([2, 1, 0]);

        assert_eq!(forward, FighterId::new(1).unwrap());
        assert_eq!(reversed, forward);
    }

    fn stable_source_order_for_spawn_order(
        order: [u32; 3],
    ) -> Vec<crate::determinism::SimEntityId> {
        let mut world = World::new();
        for index in order {
            world.spawn(StableSimEntity::new(crate::determinism::SimEntityId::new(
                crate::determinism::SimEntityKind::Special,
                index,
                0,
            )));
        }
        let mut query = world.query::<(Entity, &StableSimEntity)>();
        let mut entries: Vec<_> = query
            .iter(&world)
            .map(|(entity, stable)| (stable, entity))
            .collect();
        sort_stable_entries(&mut entries);
        entries.into_iter().map(|(stable, _)| stable.id()).collect()
    }

    #[test]
    fn bot_dynamic_sources_use_stable_id_when_entity_order_is_reversed() {
        let forward = stable_source_order_for_spawn_order([0, 1, 2]);
        let reversed = stable_source_order_for_spawn_order([2, 1, 0]);

        assert_eq!(reversed, forward);
        assert_eq!(
            forward.iter().map(|id| id.index()).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn bot_dynamic_source_collection_rejects_entries_beyond_the_fixed_pool() {
        let first = StableSimEntity::new(crate::determinism::SimEntityId::new(
            SimEntityKind::Item,
            0,
            1,
        ));
        let second = StableSimEntity::new(crate::determinism::SimEntityId::new(
            SimEntityKind::Item,
            1,
            1,
        ));
        let overflow = StableSimEntity::new(crate::determinism::SimEntityId::new(
            SimEntityKind::Item,
            2,
            1,
        ));
        let mut entries = ArrayVec::<_, 2>::new();

        assert_eq!(
            try_push_stable_source(&mut entries, &first, (), SimEntityKind::Item),
            Ok(())
        );
        assert_eq!(
            try_push_stable_source(&mut entries, &second, (), SimEntityKind::Item),
            Ok(())
        );
        assert_eq!(
            try_push_stable_source(&mut entries, &overflow, (), SimEntityKind::Item),
            Err(StableSourceCollectionError::IndexOutsidePool {
                id: overflow.id(),
                capacity: 2,
            })
        );
        assert_eq!(entries.len(), 2);
    }

    fn default_motor_and_action() -> (FighterMotor, FighterActionState) {
        (FighterMotor::default(), FighterActionState::default())
    }

    #[test]
    fn bot_action_control_jump_and_guard_repeat_until_toggled() {
        let mut control = BotActionControl::default();
        control.select(1);
        let (motor, action) = default_motor_and_action();

        assert_eq!(control.toggle_jump(), Some(true));
        let jump = controlled_bot_input(&mut control, 1, &motor, &action).unwrap();
        assert!(jump.jump);
        assert!(!jump.guard);
        assert!(
            controlled_bot_input(&mut control, 1, &motor, &action)
                .unwrap()
                .jump
        );

        assert_eq!(control.toggle_jump(), Some(false));
        assert!(controlled_bot_input(&mut control, 1, &motor, &action).is_none());

        assert_eq!(control.toggle_guard(), Some(true));
        let guard = controlled_bot_input(&mut control, 1, &motor, &action).unwrap();
        assert!(guard.guard);
        assert!(!guard.jump);
        assert!(
            controlled_bot_input(&mut control, 1, &motor, &action)
                .unwrap()
                .guard
        );

        assert_eq!(control.toggle_guard(), Some(false));
        assert!(controlled_bot_input(&mut control, 1, &motor, &action).is_none());
    }

    #[test]
    fn bot_action_control_selecting_new_bot_clears_old_actions() {
        let mut control = BotActionControl::default();
        let (motor, action) = default_motor_and_action();
        control.select(1);
        assert_eq!(control.toggle_guard(), Some(true));
        control.select(2);

        assert!(controlled_bot_input(&mut control, 1, &motor, &action).is_none());
        assert!(controlled_bot_input(&mut control, 2, &motor, &action).is_none());
        assert_eq!(control.toggle_jump(), Some(true));
        assert!(
            controlled_bot_input(&mut control, 2, &motor, &action)
                .unwrap()
                .jump
        );
    }

    #[test]
    fn bot_click_refill_toggle_moves_between_bots() {
        let mut control = BotActionControl::default();

        assert!(control.select_and_toggle_refill(1));
        assert_eq!(control.refill_bot_id(), Some(1));

        assert!(!control.select_and_toggle_refill(1));
        assert_eq!(control.refill_bot_id(), None);

        assert!(control.select_and_toggle_refill(2));
        assert_eq!(control.refill_bot_id(), Some(2));

        assert!(control.clear());
        assert_eq!(control.refill_bot_id(), None);
    }

    #[test]
    fn bot_action_control_clear_removes_forced_debug_inputs() {
        let mut control = BotActionControl::default();
        let (motor, action) = default_motor_and_action();
        control.select(1);
        assert_eq!(control.toggle_jump(), Some(true));
        assert!(controlled_bot_input(&mut control, 1, &motor, &action).is_some());

        assert!(control.clear());

        assert_eq!(control.selected_bot_id, None);
        assert_eq!(control.refill_bot_id(), None);
        assert!(controlled_bot_input(&mut control, 1, &motor, &action).is_none());
    }

    #[test]
    fn bot_digit3_enables_combat_ai() {
        let mut brain = BotBrain {
            behavior: BotBehaviorMode::TrainingDummy,
            decision_timer: bot_duration(2.8),
            movement_plan_timer: bot_duration(1.6),
            dash_timer: bot_duration(1.9),
            attack_timer: bot_duration(3.4),
            strafe_sign: -1.0,
            movement_plan: BotMovementPlan::Circle,
        };
        start_bot_combat_ai(&mut brain);

        assert_eq!(brain.behavior, BotBehaviorMode::Combatant);
        assert_eq!(brain.decision_timer, TickTimer::from_ticks(3));
        assert_eq!(brain.movement_plan_timer, TickTimer::from_ticks(3));
        assert_eq!(brain.dash_timer, TickTimer::from_ticks(3));
        assert_eq!(brain.attack_timer, TickTimer::from_ticks(3));
        assert_eq!(brain.strafe_sign, -1.0);
    }

    #[test]
    fn bot_brain_timers_advance_by_exactly_one_fixed_tick() {
        let mut brain = BotBrain {
            behavior: BotBehaviorMode::Combatant,
            decision_timer: TickTimer::from_ticks(1),
            movement_plan_timer: TickTimer::from_ticks(2),
            dash_timer: TickTimer::from_ticks(3),
            attack_timer: TickTimer::from_ticks(4),
            strafe_sign: 1.0,
            movement_plan: BotMovementPlan::Circle,
        };

        advance_bot_brain_timers(&mut brain, BotDifficulty::Standard, SimTick(1));

        assert_eq!(brain.decision_timer, TickTimer::ZERO);
        assert_eq!(brain.movement_plan_timer, TickTimer::from_ticks(1));
        assert_eq!(brain.dash_timer, TickTimer::from_ticks(2));
        assert_eq!(brain.attack_timer, TickTimer::from_ticks(3));

        advance_bot_brain_timers(&mut brain, BotDifficulty::Standard, SimTick(2));
        assert_eq!(brain.decision_timer, TickTimer::ZERO);
        assert_eq!(brain.movement_plan_timer, TickTimer::ZERO);
    }

    #[test]
    fn bot_timer_boundaries_use_ceiling_conversion() {
        assert_eq!(bot_duration(0.0), TickTimer::ZERO);
        assert_eq!(bot_duration(0.05), TickTimer::from_ticks(3));
        assert_eq!(bot_duration(0.18), TickTimer::from_ticks(11));
        assert_eq!(bot_duration(0.25), TickTimer::from_ticks(15));
        assert_eq!(BOT_CLOSE_HEAVY_READY, TickTimer::from_ticks(11));
        assert_eq!(BOT_ITEM_PICKUP_READY, TickTimer::from_ticks(15));

        assert!(bot_timer_at_most(
            TickTimer::from_ticks(11),
            BOT_CLOSE_HEAVY_READY,
        ));
        assert!(!bot_timer_at_most(
            TickTimer::from_ticks(12),
            BOT_CLOSE_HEAVY_READY,
        ));
    }

    #[test]
    fn bot_timer_trace_replays_exactly() {
        fn trace() -> Vec<[u32; 4]> {
            let mut brain = BotBrain {
                behavior: BotBehaviorMode::Combatant,
                decision_timer: bot_duration(0.05),
                movement_plan_timer: bot_duration(0.28),
                dash_timer: bot_duration(0.7),
                attack_timer: bot_duration(0.25),
                strafe_sign: 1.0,
                movement_plan: BotMovementPlan::Circle,
            };

            (0..90)
                .map(|tick| {
                    advance_bot_brain_timers(
                        &mut brain,
                        BotDifficulty::Standard,
                        SimTick(tick + 1),
                    );
                    if tick == 17 {
                        brain.attack_timer.set_max(bot_duration(0.35));
                    }
                    if !brain.decision_timer.active() {
                        brain.decision_timer = bot_duration(0.65);
                    }
                    [
                        brain.decision_timer.remaining(),
                        brain.movement_plan_timer.remaining(),
                        brain.dash_timer.remaining(),
                        brain.attack_timer.remaining(),
                    ]
                })
                .collect()
        }

        assert_eq!(trace(), trace());
    }

    #[test]
    fn bot_guard_control_releases_until_guard_can_restart() {
        let mut control = BotActionControl::default();
        control.select(1);
        assert_eq!(control.toggle_guard(), Some(true));

        let mut motor = FighterMotor::default();
        let mut action = FighterActionState::default();
        assert!(
            controlled_bot_input(&mut control, 1, &motor, &action)
                .unwrap()
                .guard
        );

        motor.guard_cooldown_timer = TickTimer::from_seconds_ceil(0.05);
        motor.guard_was_requested = true;
        assert!(
            !controlled_bot_input(&mut control, 1, &motor, &action)
                .unwrap()
                .guard
        );

        motor.guard_cooldown_timer.clear();
        motor.guard_was_requested = false;
        assert!(
            controlled_bot_input(&mut control, 1, &motor, &action)
                .unwrap()
                .guard
        );

        action.action = FighterAction::Hitstun;
        assert!(
            !controlled_bot_input(&mut control, 1, &motor, &action)
                .unwrap()
                .guard
        );

        action.action = FighterAction::Guarding;
        motor.grounded = false;
        assert!(
            controlled_bot_input(&mut control, 1, &motor, &action)
                .unwrap()
                .guard
        );
    }

    #[test]
    fn bot_selection_distance_ignores_height() {
        let cursor = Vec3::new(1.0, ARENA_TOP_Y + 5.0, -2.0);
        let fighter = Vec3::new(1.0, ARENA_TOP_Y, -2.0);

        assert_eq!(bot_selection_distance(cursor, fighter), 0.0);
        assert!(bot_selection_distance(cursor, fighter + Vec3::X) > FIGHTER_RADIUS);
    }

    #[test]
    fn bot_recovery_rolls_away_from_close_threat() {
        let decision = bot_recovery_decision(
            QUICK_STAND_AFTER,
            Vec3::ZERO,
            Some(target(Vec3::X * 2.0, -Vec3::X, FighterAction::HeavyAttack)),
        );

        assert_eq!(decision, BotRecoveryDecision::Roll(-Vec2::X));
        assert_eq!(
            bot_recovery_decision(QUICK_STAND_AFTER, Vec3::ZERO, None),
            BotRecoveryDecision::QuickStand
        );
    }

    #[test]
    fn training_dummy_bots_do_not_drive_autonomous_inputs() {
        assert!(!bot_should_drive_autonomous_inputs(
            BotBehaviorMode::TrainingDummy
        ));
        assert!(bot_should_drive_autonomous_inputs(
            BotBehaviorMode::Combatant
        ));
    }

    #[test]
    fn bot_guard_threat_requires_enemy_facing_bot() {
        let threatening = target(Vec3::X * 1.4, -Vec3::X, FighterAction::HeavyAttack);
        let turned_away = target(Vec3::X * 1.4, Vec3::X, FighterAction::HeavyAttack);
        let grab = target(Vec3::X, -Vec3::X, FighterAction::GrabStartup);

        assert!(bot_should_guard_threat(Vec3::ZERO, threatening));
        assert!(!bot_should_guard_threat(Vec3::ZERO, turned_away));
        assert!(!bot_should_guard_threat(Vec3::ZERO, grab));
    }

    #[test]
    fn bot_avoids_active_arena_hazard_only_when_pulsing() {
        let hazards = [ArenaHazardDefinition {
            kind: ArenaHazardKind::PulseVent,
            center: Vec3::new(0.0, ARENA_TOP_Y + 0.04, 0.0),
            radius: 1.0,
            pulse_seconds: 2.0,
            phase: 0.0,
        }];
        let position = Vec3::new(0.5, ARENA_TOP_Y, 0.0);

        assert!(
            arena_hazard_avoidance(
                position,
                crate::simulation::ElapsedTicks::from_ticks(12),
                &hazards,
                1.0,
            )
            .length_squared()
                > 0.01
        );
        assert_eq!(
            arena_hazard_avoidance(
                position,
                crate::simulation::ElapsedTicks::from_ticks(72),
                &hazards,
                1.0,
            ),
            Vec2::ZERO
        );
        assert_eq!(
            arena_hazard_avoidance(
                Vec3::new(0.5, ARENA_TOP_Y - 0.65, 0.0),
                crate::simulation::ElapsedTicks::from_ticks(12),
                &hazards,
                1.0,
            ),
            Vec2::ZERO
        );
    }

    #[test]
    fn bot_avoidance_accounts_for_hazard_kind() {
        let pulse = ArenaHazardDefinition {
            kind: ArenaHazardKind::PulseVent,
            center: Vec3::ZERO,
            radius: 1.0,
            pulse_seconds: 2.0,
            phase: 0.0,
        };
        let bumper = ArenaHazardDefinition {
            kind: ArenaHazardKind::BumperNode,
            center: Vec3::ZERO,
            radius: 1.0,
            pulse_seconds: 2.0,
            phase: 0.0,
        };
        let saw = ArenaHazardDefinition {
            kind: ArenaHazardKind::SawBlade,
            center: Vec3::ZERO,
            radius: 1.0,
            pulse_seconds: 1.0,
            phase: 0.0,
        };

        assert!(arena_hazard_avoid_radius(&bumper) > arena_hazard_avoid_radius(&pulse));
        assert!(arena_hazard_avoid_radius(&saw) > arena_hazard_avoid_radius(&pulse));
    }

    #[test]
    fn bot_jumps_when_the_vent_spiral_route_rises_a_tier() {
        let vent = crate::arena_defs::arena_definition(4);
        let approach = Vec3::new(4.15, ARENA_TOP_Y, 2.2);

        assert!(bot_should_jump_for_elevation(approach, Vec2::Y, true, vent,));
        assert!(!bot_should_jump_for_elevation(
            approach,
            Vec2::Y,
            false,
            vent,
        ));
        assert!(!bot_should_jump_for_elevation(
            approach,
            -Vec2::Y,
            true,
            vent,
        ));
    }

    #[test]
    fn bot_range_bands_track_style_spacing() {
        let anchor = bot_range_band(
            style_tuning(crate::styles::FighterStyleKind::Anchor).bot_preferred_range,
            bot_personality(
                crate::styles::FighterStyleKind::Anchor,
                EquipmentKind::CounterCell,
            ),
        );
        let catalyst = bot_range_band(
            style_tuning(crate::styles::FighterStyleKind::Catalyst).bot_preferred_range,
            bot_personality(
                crate::styles::FighterStyleKind::Catalyst,
                EquipmentKind::CounterCell,
            ),
        );

        assert!(anchor.min < anchor.ideal && anchor.ideal < anchor.max);
        assert!(catalyst.ideal > anchor.ideal);
        assert!(catalyst.max > anchor.max);
    }

    #[test]
    fn bot_movement_plan_uses_range_and_edge_pressure() {
        let arena =
            crate::arena_defs::arena_definition(crate::arena_defs::TRAINING_GROUND_ARENA_INDEX);
        let personality = bot_personality(
            crate::styles::FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let range = bot_range_band(1.25, personality);

        assert_eq!(
            choose_bot_movement_plan_for_arena(
                Vec3::ZERO,
                Vec3::X * 4.0,
                range.max + 0.2,
                range,
                personality,
                100.0,
                BotDecisionKey::new(7, 1, SimTick(0)),
                arena,
            ),
            BotMovementPlan::Approach
        );
        assert_eq!(
            choose_bot_movement_plan_for_arena(
                Vec3::ZERO,
                Vec3::X,
                range.min - 0.1,
                range,
                personality,
                100.0,
                BotDecisionKey::new(7, 1, SimTick(0)),
                arena,
            ),
            BotMovementPlan::Backstep
        );
        assert_eq!(
            choose_bot_movement_plan_for_arena(
                Vec3::new(7.0, ARENA_TOP_Y, 0.0),
                Vec3::new(8.8, ARENA_TOP_Y, 0.0),
                range.ideal,
                range,
                personality,
                100.0,
                BotDecisionKey::new(7, 1, SimTick(0)),
                arena,
            ),
            BotMovementPlan::Pressure
        );
    }

    #[test]
    fn edge_steering_pulls_outward_motion_back_inward() {
        let arena =
            crate::arena_defs::arena_definition(crate::arena_defs::TRAINING_GROUND_ARENA_INDEX);
        let outward = Vec2::X;
        let position = Vec3::new(8.9, ARENA_TOP_Y, 0.0);
        let steered = apply_edge_steering_for_arena(position, outward, arena);

        assert!(movement_points_toward_edge_for_arena(
            position, outward, arena
        ));
        assert!(steered.dot(outward) < 0.0);
    }

    #[test]
    fn dash_planning_rejects_edgeward_dashes() {
        let arena =
            crate::arena_defs::arena_definition(crate::arena_defs::TRAINING_GROUND_ARENA_INDEX);
        let personality = bot_personality(
            crate::styles::FighterStyleKind::Vector,
            EquipmentKind::DashCoil,
        );
        let range = bot_range_band(1.55, personality);

        assert!(bot_should_dash_for_movement_for_arena(
            BotMovementPlan::Approach,
            Vec3::ZERO,
            Vec2::X,
            range.max + 1.0,
            range,
            true,
            TickTimer::ZERO,
            arena,
        ));
        let outward = Vec2::X;
        assert!(!bot_should_dash_for_movement_for_arena(
            BotMovementPlan::Approach,
            Vec3::new(8.8, ARENA_TOP_Y, 0.0),
            outward,
            range.max + 1.0,
            range,
            true,
            TickTimer::ZERO,
            arena,
        ));
    }

    #[test]
    fn bot_personality_varies_by_style_and_equipment() {
        let anchor_counter = bot_personality(
            crate::styles::FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
        );
        let vector_dash = bot_personality(
            crate::styles::FighterStyleKind::Vector,
            EquipmentKind::DashCoil,
        );
        let catalyst_heavy = bot_personality(
            crate::styles::FighterStyleKind::Catalyst,
            EquipmentKind::HeavySeal,
        );

        assert!(vector_dash.aggression > anchor_counter.aggression);
        assert!(anchor_counter.hazard_fear > vector_dash.hazard_fear);
        assert!(catalyst_heavy.special_bias > anchor_counter.special_bias);
        assert!(
            catalyst_heavy.item_greed
                > bot_personality(
                    crate::styles::FighterStyleKind::Catalyst,
                    EquipmentKind::CounterCell
                )
                .item_greed
        );
    }

    #[test]
    fn tutorial_difficulty_is_forgiving_but_bounded() {
        let normal = bot_personality_for_difficulty(
            crate::styles::FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
            BotDifficulty::Standard,
        );
        let tutorial = bot_personality_for_difficulty(
            crate::styles::FighterStyleKind::Anchor,
            EquipmentKind::CounterCell,
            BotDifficulty::Tutorial,
        );

        assert!(tutorial.aggression < normal.aggression);
        assert!(tutorial.special_bias < normal.special_bias);
        assert!(tutorial.mistake_rate > normal.mistake_rate);
        assert!((0.0..=0.4).contains(&tutorial.mistake_rate));
        assert_eq!(
            (0_u64..33)
                .filter(|tick| BotDifficulty::Tutorial.attack_timer_advances(SimTick(*tick)))
                .count(),
            20
        );
    }

    #[test]
    fn tutorial_difficulty_disables_normal_grabs_only() {
        assert!(BotDifficulty::Standard.normal_grab_allowed());
        assert!(!BotDifficulty::Tutorial.normal_grab_allowed());
    }

    #[test]
    fn disabled_shared_special_gate_skips_the_bot_special_selection_branch() {
        let mut normal = UserModeState::default();
        assert!(!normal.restricts_bot_special_inputs());
        assert!(!bot_ai_special_inputs_allowed(&normal));

        normal.enter_tutorial_lesson();
        assert!(!normal.restricts_bot_special_inputs());
        assert!(!bot_ai_special_inputs_allowed(&normal));
    }

    #[test]
    fn bot_mistake_and_panic_helpers_are_bounded() {
        let personality = bot_personality(
            crate::styles::FighterStyleKind::Vector,
            EquipmentKind::DashCoil,
        );
        assert!(bot_should_panic(personality.panic_health, personality));
        assert!(!bot_should_panic(
            personality.panic_health + 1.0,
            personality
        ));
        let key = BotDecisionKey::new(17, 2, SimTick(240));
        assert_eq!(
            bot_should_make_mistake(key, personality),
            bot_should_make_mistake(key, personality)
        );
        assert!(personality.mistake_rate > 0.0 && personality.mistake_rate < 0.25);
    }

    #[test]
    fn bot_choice_hash_is_replay_seeded_tick_keyed_and_purpose_isolated() {
        let key = BotDecisionKey::new(0x1234_5678, 2, SimTick(240));
        let sample = bot_choice_sample(key, BotChoicePurpose::CloseGrab);

        assert_eq!(sample, bot_choice_sample(key, BotChoicePurpose::CloseGrab));
        assert_eq!(
            sample,
            bot_choice_sample(
                BotDecisionKey::new(key.replay_seed, key.bot_id, SimTick(251)),
                BotChoicePurpose::CloseGrab,
            )
        );
        assert_ne!(
            sample,
            bot_choice_sample(
                BotDecisionKey::new(key.replay_seed, key.bot_id, SimTick(252)),
                BotChoicePurpose::CloseGrab,
            )
        );
        assert_ne!(
            sample,
            bot_choice_sample(
                BotDecisionKey::new(key.replay_seed + 1, key.bot_id, key.tick),
                BotChoicePurpose::CloseGrab,
            )
        );
        assert_ne!(
            sample,
            bot_choice_sample(
                BotDecisionKey::new(key.replay_seed, key.bot_id + 1, key.tick),
                BotChoicePurpose::CloseGrab,
            )
        );
        assert_ne!(sample, bot_choice_sample(key, BotChoicePurpose::CloseGuard));
    }

    #[test]
    fn bot_spatial_helpers_are_fixed_quantized_without_runtime_libm() {
        assert_eq!(deterministic_flat_distance(Vec2::new(3.0, 4.0)), 5.0);
        assert_eq!(deterministic_normalize(Vec2::X), Vec2::X);
        assert_eq!(deterministic_normalize(Vec2::ZERO), Vec2::ZERO);
        assert_eq!(BOT_PROBE_DIRECTIONS[0], (1.0, 0.0));
        assert_eq!(BOT_PROBE_DIRECTIONS[4], (0.0, 1.0));
        assert_eq!(BOT_PROBE_DIRECTIONS[8], (-1.0, 0.0));
        assert_eq!(BOT_PROBE_DIRECTIONS[12], (0.0, -1.0));
    }

    #[test]
    fn bot_uses_mp_food_only_when_stamina_is_missing() {
        assert_eq!(
            bot_held_item_decision(
                ItemKind::WineWhite,
                MAX_STAMINA - ITEM_BREEZE_BUOY_STAMINA * 0.5,
                2.0,
                TickTimer::ZERO,
                false
            ),
            Some(BotHeldItemDecision::Light)
        );
        assert_eq!(
            bot_held_item_decision(
                ItemKind::WineWhite,
                MAX_STAMINA,
                2.0,
                TickTimer::ZERO,
                false,
            ),
            None
        );
    }

    #[test]
    fn bot_mushroom_uses_buff_directly() {
        assert_eq!(
            bot_held_item_decision(ItemKind::Mushroom, 100.0, 1.8, TickTimer::ZERO, true,),
            Some(BotHeldItemDecision::Light)
        );
    }

    #[test]
    fn bot_pickup_score_uses_item_role_context() {
        assert!(
            bot_pickup_score(ItemKind::Steamer, MAX_STAMINA, 4.0, 0.4)
                > bot_pickup_score(ItemKind::Apple, MAX_STAMINA, 4.0, 0.4)
        );
        assert!(
            bot_pickup_score(
                ItemKind::CupCoffee,
                MAX_STAMINA - ITEM_BREEZE_BUOY_STAMINA * 0.6,
                2.0,
                0.4
            ) > bot_pickup_score(
                ItemKind::Apple,
                MAX_STAMINA - ITEM_BREEZE_BUOY_STAMINA * 0.6,
                4.0,
                0.4
            )
        );
    }
}
