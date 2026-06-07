use bevy::prelude::*;

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::arena_defs::arena_definitions;
use crate::arena_defs::{active_arena_definition, set_active_arena_index};
use crate::bee_skills::ActiveBeeSkill;
use crate::bot::default_bot_brain_for_fighter;
use crate::characters::{
    CharacterKind, CharacterMoveCatalog, FighterCharacter, character_for_fighter_id,
    character_scene_model,
};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::characters::{character_label, next_character_kind, previous_character_kind};
use crate::combat::{HitEffects, ImpactSource};
use crate::components::{
    BotBrain, Controller, Fighter, FighterAction, FighterActionState, FighterGrabState,
    FighterInput, FighterInventory, FighterMotor, FighterSceneModel, FighterSpecialState,
    FighterStats, Hitbox, LocalInputAssignment, ParticipantKind, PlayerSlotId,
};
#[cfg(test)]
use crate::constants::MATCH_SECONDS;
use crate::constants::{FIGHTER_COUNT, MAX_HEALTH, MAX_STAMINA, STOCK_LIVES, TIME_UP_SECONDS};
use crate::effects::VisualEffect;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::equipment::next_equipment_kind;
use crate::equipment::{DEFAULT_FIGHTER_EQUIPMENT, EquipmentKind, FighterEquipment};
use crate::items::{ArenaItem, ItemAssets, item_scale};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::map_editor::{MapEditorState, map_editor_allows_setup_input};
use crate::penguin_skills::{ActivePenguinSkill, ActivePenguinSurface};
use crate::specials::ActiveSpecial;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use crate::styles::next_style_kind;
use crate::styles::{DEFAULT_FIGHTER_STYLES, FighterStyle, FighterStyleKind};
use crate::user_mode::UserModeState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchPhase {
    Setup,
    Fighting,
    TimeUp,
    Results,
    Resetting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RulePreset {
    TimedTeamScore,
    FreeForAll,
    StockRingOut,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MatchRules {
    pub preset: RulePreset,
    pub label: &'static str,
    pub time_limit: Option<f32>,
    pub starting_stocks: Option<i32>,
    pub team_scoring: bool,
    pub friendly_fire: bool,
}

impl MatchRules {
    pub fn uses_timer(self) -> bool {
        self.time_limit.is_some()
    }

    pub fn uses_stocks(self) -> bool {
        self.starting_stocks.is_some()
    }
}

pub const RULE_PRESETS: [MatchRules; 3] = [
    MatchRules {
        preset: RulePreset::TimedTeamScore,
        label: "Team Lives (No Timer)",
        time_limit: None,
        starting_stocks: Some(STOCK_LIVES),
        team_scoring: true,
        friendly_fire: false,
    },
    MatchRules {
        preset: RulePreset::FreeForAll,
        label: "Free-for-All Lives (No Timer)",
        time_limit: None,
        starting_stocks: Some(STOCK_LIVES),
        team_scoring: false,
        friendly_fire: true,
    },
    MatchRules {
        preset: RulePreset::StockRingOut,
        label: "Life Ring-Out",
        time_limit: None,
        starting_stocks: Some(STOCK_LIVES),
        team_scoring: false,
        friendly_fire: true,
    },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TeamId {
    Red,
    Blue,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotSetup {
    pub participant: ParticipantKind,
    pub character: CharacterKind,
    pub style: FighterStyleKind,
    pub equipment: EquipmentKind,
    pub team: TeamId,
    pub input: LocalInputAssignment,
}

impl SlotSetup {
    fn default_for(
        fighter_id: usize,
        participant: ParticipantKind,
        input: LocalInputAssignment,
    ) -> Self {
        Self {
            participant,
            character: character_for_fighter_id(fighter_id),
            style: DEFAULT_FIGHTER_STYLES[fighter_id],
            equipment: DEFAULT_FIGHTER_EQUIPMENT[fighter_id],
            team: default_slot_team(fighter_id),
            input,
        }
    }
}

fn default_slot_team(fighter_id: usize) -> TeamId {
    if fighter_id % 2 == 0 {
        TeamId::Red
    } else {
        TeamId::Blue
    }
}

#[cfg(test)]
pub fn fighter_team(fighter_id: usize) -> Option<TeamId> {
    (fighter_id < FIGHTER_COUNT).then_some(default_slot_team(fighter_id))
}

#[cfg(test)]
pub fn fighters_share_team(first_id: usize, second_id: usize) -> bool {
    matches!(
        (fighter_team(first_id), fighter_team(second_id)),
        (Some(first), Some(second)) if first == second
    )
}

#[cfg(test)]
pub fn combat_target_allowed(rules: MatchRules, attacker_id: usize, target_id: usize) -> bool {
    if attacker_id >= FIGHTER_COUNT {
        return true;
    }
    if target_id >= FIGHTER_COUNT {
        return false;
    }
    if !rules.team_scoring || rules.friendly_fire {
        return true;
    }

    attacker_id != target_id && !fighters_share_team(attacker_id, target_id)
}

fn apply_setup_loadout(
    fighter_id: usize,
    setup: &LocalSetup,
    character: &mut FighterCharacter,
    style: &mut FighterStyle,
    equipment: &mut FighterEquipment,
) {
    if let Some(slot) = setup.slot(fighter_id) {
        if character.kind != slot.character {
            character.kind = slot.character;
        }
        if style.kind != slot.style {
            style.kind = slot.style;
        }
        if equipment.kind != slot.equipment {
            equipment.kind = slot.equipment;
        }
    }
    equipment.cooldown = 0.0;
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn match_reset_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    keys.just_pressed(KeyCode::KeyR) && !input_modifier_pressed(keys)
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn shift_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight)
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn input_modifier_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    shift_pressed(keys)
        || keys.pressed(KeyCode::ControlLeft)
        || keys.pressed(KeyCode::ControlRight)
        || keys.pressed(KeyCode::AltLeft)
        || keys.pressed(KeyCode::AltRight)
        || keys.pressed(KeyCode::SuperLeft)
        || keys.pressed(KeyCode::SuperRight)
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn dev_character_cycle_pressed(keys: &ButtonInput<KeyCode>) -> bool {
    keys.just_pressed(KeyCode::KeyP) && shift_pressed(keys)
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn setup_rule_hotkeys_active(phase: MatchPhase) -> bool {
    phase == MatchPhase::Setup
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn cycle_dev_player_character(setup: &mut LocalSetup) -> CharacterKind {
    setup.selected_character_fighter = 0;
    setup.cycle_character(0);
    setup.player_character()
}

fn sync_setup_character_scene(
    fighter_id: usize,
    character: CharacterKind,
    asset_server: &AssetServer,
    catalog: &CharacterMoveCatalog,
    commands: &mut Commands,
    scene_models: &mut Query<(Entity, &FighterSceneModel, &ChildOf, &Transform)>,
) {
    let Some(scene) = character_scene_model(asset_server, catalog, character) else {
        return;
    };
    for (entity, scene_model, parent, transform) in scene_models {
        if scene_model.fighter_id == fighter_id {
            commands.entity(entity).despawn();
            commands.entity(parent.parent()).with_children(|pose_root| {
                pose_root.spawn((
                    SceneRoot(scene.clone()),
                    *transform,
                    FighterSceneModel { fighter_id },
                    Name::new(format!("Fighter {fighter_id} Kenney cube pet")),
                ));
            });
        }
    }
}

#[derive(Resource, Clone)]
pub struct LocalSetup {
    pub rule_index: usize,
    pub arena_index: usize,
    pub selected_character_fighter: usize,
    pub slots: [SlotSetup; FIGHTER_COUNT],
    pub replay_seed: u64,
}

impl Default for LocalSetup {
    fn default() -> Self {
        Self {
            rule_index: 0,
            arena_index: 0,
            selected_character_fighter: 0,
            slots: [
                SlotSetup::default_for(
                    0,
                    ParticipantKind::Human,
                    LocalInputAssignment::Keyboard(0),
                ),
                SlotSetup::default_for(1, ParticipantKind::Bot, LocalInputAssignment::Unassigned),
                SlotSetup::default_for(
                    2,
                    ParticipantKind::Closed,
                    LocalInputAssignment::Unassigned,
                ),
                SlotSetup::default_for(
                    3,
                    ParticipantKind::Closed,
                    LocalInputAssignment::Unassigned,
                ),
            ],
            replay_seed: DEFAULT_REPLAY_SEED,
        }
    }
}

impl LocalSetup {
    pub fn slot(&self, fighter_id: usize) -> Option<&SlotSetup> {
        self.slots.get(fighter_id)
    }

    pub fn slot_mut(&mut self, fighter_id: usize) -> Option<&mut SlotSetup> {
        self.slots.get_mut(fighter_id)
    }

    pub fn controller_for_fighter(&self, fighter_id: usize) -> Option<Controller> {
        let slot_id = PlayerSlotId::new(fighter_id)?;
        let setup = self.slot(fighter_id)?;
        Some(Controller::new(slot_id, setup.participant, setup.input))
    }

    pub fn active_slots(&self) -> [bool; FIGHTER_COUNT] {
        std::array::from_fn(|fighter_id| self.is_slot_occupied(fighter_id))
    }

    pub fn slot_teams(&self) -> [TeamId; FIGHTER_COUNT] {
        std::array::from_fn(|fighter_id| self.slots[fighter_id].team)
    }

    pub fn is_slot_occupied(&self, fighter_id: usize) -> bool {
        self.slot(fighter_id)
            .map(|slot| slot.participant.is_occupied())
            .unwrap_or(false)
    }

    pub fn active_bot_count(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.participant == ParticipantKind::Bot)
            .count()
    }

    pub fn active_rule(&self) -> MatchRules {
        RULE_PRESETS[self.rule_index.min(RULE_PRESETS.len() - 1)]
    }

    pub fn active_rule_label(&self) -> &'static str {
        self.active_rule().label
    }

    pub fn set_rule(&mut self, index: usize) {
        self.rule_index = index.min(RULE_PRESETS.len() - 1);
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    pub fn cycle_arena(&mut self, arena_count: usize) {
        let arena_count = arena_count.max(1);
        self.arena_index = (self.arena_index + 1) % arena_count;
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    pub fn cycle_bot_count(&mut self) {
        if let Some(slot) = self.slot_mut(1) {
            slot.participant = ParticipantKind::Bot;
            slot.input = LocalInputAssignment::Unassigned;
        }
        for fighter_id in 2..FIGHTER_COUNT {
            if let Some(slot) = self.slot_mut(fighter_id) {
                slot.participant = ParticipantKind::Closed;
                slot.input = LocalInputAssignment::Unassigned;
            }
        }
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    pub fn cycle_style(&mut self, fighter_id: usize) {
        if let Some(slot) = self.slot_mut(fighter_id) {
            slot.style = next_style_kind(slot.style);
        }
    }

    pub fn set_character(&mut self, fighter_id: usize, character_kind: CharacterKind) {
        if let Some(slot) = self.slot_mut(fighter_id) {
            slot.character = character_kind;
        }
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    pub fn cycle_character(&mut self, fighter_id: usize) {
        if let Some(slot) = self.slot_mut(fighter_id) {
            slot.character = next_character_kind(slot.character);
        }
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    pub fn cycle_character_previous(&mut self, fighter_id: usize) {
        if let Some(slot) = self.slot_mut(fighter_id) {
            slot.character = previous_character_kind(slot.character);
        }
    }

    pub fn selected_character_fighter(&self) -> usize {
        let selected = self.selected_character_fighter.min(FIGHTER_COUNT - 1);
        if self.is_slot_occupied(selected) {
            return selected;
        }
        self.slots
            .iter()
            .position(|slot| slot.participant.is_occupied())
            .unwrap_or(0)
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    pub fn cycle_selected_character_fighter(&mut self) {
        let start = self.selected_character_fighter();
        for offset in 1..=FIGHTER_COUNT {
            let fighter_id = (start + offset) % FIGHTER_COUNT;
            if self.is_slot_occupied(fighter_id) {
                self.selected_character_fighter = fighter_id;
                return;
            }
        }
        self.selected_character_fighter = 0;
    }

    pub fn player_character(&self) -> CharacterKind {
        self.slots[0].character
    }

    pub fn configure_single_player_duel(
        &mut self,
        player_character: CharacterKind,
        bot_character: CharacterKind,
    ) {
        self.configure_duel_slots(
            player_character,
            bot_character,
            ParticipantKind::Bot,
            LocalInputAssignment::Unassigned,
        );
    }

    pub fn configure_two_player_duel(
        &mut self,
        p1_character: CharacterKind,
        p2_character: CharacterKind,
    ) {
        self.configure_duel_slots(
            p1_character,
            p2_character,
            ParticipantKind::Human,
            LocalInputAssignment::Keyboard(1),
        );
    }

    fn configure_duel_slots(
        &mut self,
        p1_character: CharacterKind,
        p2_character: CharacterKind,
        second_participant: ParticipantKind,
        second_input: LocalInputAssignment,
    ) {
        self.slots[0].participant = ParticipantKind::Human;
        self.slots[0].input = LocalInputAssignment::Keyboard(0);
        self.slots[0].character = p1_character;
        self.slots[1].participant = second_participant;
        self.slots[1].input = second_input;
        self.slots[1].character = p2_character;
        for fighter_id in 2..FIGHTER_COUNT {
            self.slots[fighter_id].participant = ParticipantKind::Closed;
            self.slots[fighter_id].input = LocalInputAssignment::Unassigned;
        }
        self.selected_character_fighter = 0;
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    pub fn cycle_equipment(&mut self, fighter_id: usize) {
        if let Some(slot) = self.slot_mut(fighter_id) {
            slot.equipment = next_equipment_kind(slot.equipment);
        }
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    pub fn advance_replay_seed(&mut self) -> u64 {
        self.replay_seed = next_replay_seed(self.replay_seed);
        self.replay_seed
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchPhaseEvent {
    TimeUp,
    Results,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingoutResolution {
    pub awarded_to: Option<usize>,
    pub remaining_stock: Option<i32>,
    pub eliminated: bool,
    pub match_finished: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifeLossCause {
    RingOut,
    Knockout,
}

#[derive(Resource)]
pub struct MatchState {
    pub timer: f32,
    pub phase: MatchPhase,
    pub phase_timer: f32,
    pub rules: MatchRules,
    pub rule_index: usize,
    pub arena_index: usize,
    pub active_fighter_count: usize,
    pub active_slots: [bool; FIGHTER_COUNT],
    pub teams: [TeamId; FIGHTER_COUNT],
    pub stocks: [i32; FIGHTER_COUNT],
    pub replay_seed: u64,
    pub debug_hitboxes: bool,
    pub reset_requested: bool,
}

#[derive(Resource, Clone)]
pub struct MatchTelemetry {
    pub replay_seed: u64,
    pub ring_outs: u32,
    pub falls: u32,
    pub item_hits: u32,
    pub throws: u32,
    pub guard_breaks: u32,
    pub damage_by_fighter: [f32; FIGHTER_COUNT],
}

impl Default for MatchTelemetry {
    fn default() -> Self {
        Self {
            replay_seed: DEFAULT_REPLAY_SEED,
            ring_outs: 0,
            falls: 0,
            item_hits: 0,
            throws: 0,
            guard_breaks: 0,
            damage_by_fighter: [0.0; FIGHTER_COUNT],
        }
    }
}

impl MatchTelemetry {
    pub fn reset_for_seed(&mut self, replay_seed: u64) {
        *self = Self {
            replay_seed,
            ..default()
        };
    }

    pub fn record_ringout(&mut self, credited: bool) {
        if credited {
            self.ring_outs += 1;
        } else {
            self.falls += 1;
        }
    }

    pub fn record_damage(&mut self, owner_id: usize, damage: f32) {
        if let Some(total) = self.damage_by_fighter.get_mut(owner_id) {
            *total += damage.max(0.0);
        }
    }

    pub fn record_item_hit(&mut self) {
        self.item_hits += 1;
    }

    pub fn record_throw(&mut self) {
        self.throws += 1;
    }

    #[allow(dead_code)]
    pub fn record_guard_break(&mut self) {
        self.guard_breaks += 1;
    }

    pub fn total_damage(&self) -> f32 {
        self.damage_by_fighter.iter().sum()
    }
}

pub const DEFAULT_REPLAY_SEED: u64 = 0xFFC0_0001;

#[derive(Resource, Default)]
pub struct Hitstop {
    pub remaining: f32,
}

impl Hitstop {
    pub fn active(&self) -> bool {
        self.remaining > 0.0
    }

    pub fn trigger(&mut self, duration: f32) {
        self.remaining = self.remaining.max(duration);
    }
}

#[derive(Resource, Default)]
pub struct MatchAnnouncements {
    pub message: String,
    pub timer: f32,
}

impl MatchAnnouncements {
    pub fn show(&mut self, message: impl Into<String>, duration: f32) {
        self.message = message.into();
        self.timer = duration;
    }
}

impl Default for MatchState {
    fn default() -> Self {
        let rules = RULE_PRESETS[0];
        let active_slots = LocalSetup::default().active_slots();
        Self {
            timer: rules.time_limit.unwrap_or(0.0),
            phase: MatchPhase::Setup,
            phase_timer: 0.0,
            rules,
            rule_index: 0,
            arena_index: 0,
            active_fighter_count: active_slot_count(active_slots),
            active_slots,
            teams: std::array::from_fn(default_slot_team),
            stocks: stock_array(rules),
            replay_seed: DEFAULT_REPLAY_SEED,
            debug_hitboxes: false,
            reset_requested: false,
        }
    }
}

impl MatchState {
    pub fn is_fighting(&self) -> bool {
        self.phase == MatchPhase::Fighting
    }

    pub fn active_rule_label(&self) -> &'static str {
        self.rules.label
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    pub fn select_rule(&mut self, index: usize) {
        self.rule_index = index.min(RULE_PRESETS.len() - 1);
        self.rules = RULE_PRESETS[self.rule_index];
        if self.phase != MatchPhase::Setup {
            self.request_rematch();
        }
    }

    pub fn request_rematch(&mut self) {
        self.reset_requested = true;
        self.phase = MatchPhase::Resetting;
        self.phase_timer = 0.0;
    }

    pub fn return_to_setup(&mut self) {
        self.phase = MatchPhase::Setup;
        self.phase_timer = 0.0;
        self.reset_requested = false;
        self.timer = self.rules.time_limit.unwrap_or(0.0);
    }

    pub fn reset_for_new_match(&mut self) {
        self.timer = self.rules.time_limit.unwrap_or(0.0);
        self.phase = MatchPhase::Fighting;
        self.phase_timer = 0.0;
        self.stocks = stock_array(self.rules);
        self.reset_requested = false;
    }

    pub fn apply_local_setup(&mut self, setup: &LocalSetup) {
        self.set_active_slots(setup.active_slots());
        self.teams = setup.slot_teams();
    }

    pub fn set_active_slots(&mut self, active_slots: [bool; FIGHTER_COUNT]) {
        self.active_slots = active_slots;
        self.active_fighter_count = active_slot_count(active_slots);
    }

    pub fn fighter_active(&self, fighter_id: usize) -> bool {
        self.active_slots.get(fighter_id).copied().unwrap_or(false)
    }

    pub fn fighter_can_participate(&self, fighter_id: usize) -> bool {
        self.fighter_active(fighter_id)
            && !(self.rules.uses_stocks()
                && fighter_id < FIGHTER_COUNT
                && self.stocks[fighter_id] <= 0)
    }

    pub fn combat_owner_active(&self, owner_id: usize) -> bool {
        owner_id >= FIGHTER_COUNT || self.fighter_can_participate(owner_id)
    }

    pub fn fighter_team(&self, fighter_id: usize) -> Option<TeamId> {
        (fighter_id < FIGHTER_COUNT).then_some(self.teams[fighter_id])
    }

    pub fn fighters_share_team(&self, first_id: usize, second_id: usize) -> bool {
        matches!(
            (self.fighter_team(first_id), self.fighter_team(second_id)),
            (Some(first), Some(second)) if first == second
        )
    }

    pub fn combat_target_allowed_for_state(&self, attacker_id: usize, target_id: usize) -> bool {
        if !self.combat_owner_active(attacker_id) || !self.fighter_can_participate(target_id) {
            return false;
        }
        if attacker_id >= FIGHTER_COUNT {
            return true;
        }
        if !self.rules.team_scoring || self.rules.friendly_fire {
            return true;
        }

        attacker_id != target_id && !self.fighters_share_team(attacker_id, target_id)
    }

    pub fn ringout_credit_allowed(&self, victim_id: usize, attacker_id: usize) -> bool {
        attacker_id < FIGHTER_COUNT
            && attacker_id != victim_id
            && self.combat_target_allowed_for_state(attacker_id, victim_id)
    }

    pub fn advance_replay_seed(&mut self) -> u64 {
        self.replay_seed = next_replay_seed(self.replay_seed);
        self.replay_seed
    }

    pub fn advance_phase(&mut self, dt: f32) -> Option<MatchPhaseEvent> {
        match self.phase {
            MatchPhase::Setup => {}
            MatchPhase::Fighting => {
                if self.rules.uses_timer() {
                    self.timer = (self.timer - dt).max(0.0);
                    if self.timer == 0.0 {
                        self.phase = MatchPhase::TimeUp;
                        self.phase_timer = TIME_UP_SECONDS;
                        return Some(MatchPhaseEvent::TimeUp);
                    }
                }
            }
            MatchPhase::TimeUp => {
                self.phase_timer = (self.phase_timer - dt).max(0.0);
                if self.phase_timer == 0.0 {
                    self.phase = MatchPhase::Results;
                    return Some(MatchPhaseEvent::Results);
                }
            }
            MatchPhase::Results | MatchPhase::Resetting => {}
        }
        None
    }

    pub fn record_ringout(
        &mut self,
        victim_id: usize,
        attacker_id: Option<usize>,
    ) -> RingoutResolution {
        self.record_life_loss(victim_id, attacker_id, LifeLossCause::RingOut)
    }

    pub fn record_life_loss(
        &mut self,
        victim_id: usize,
        attacker_id: Option<usize>,
        _cause: LifeLossCause,
    ) -> RingoutResolution {
        let awarded_to =
            attacker_id.filter(|attacker| self.ringout_credit_allowed(victim_id, *attacker));
        let mut remaining_stock = None;
        let mut eliminated = false;
        let mut match_finished = false;

        if self.rules.uses_stocks() && victim_id < FIGHTER_COUNT {
            let stock = (self.stocks[victim_id] - 1).max(0);
            self.stocks[victim_id] = stock;
            remaining_stock = Some(stock);
            eliminated = stock == 0;
            let active_fighters = self
                .stocks
                .iter()
                .enumerate()
                .filter(|(fighter_id, stock)| self.fighter_active(*fighter_id) && **stock > 0)
                .count();
            if active_fighters <= 1 {
                self.phase = MatchPhase::Results;
                self.phase_timer = 0.0;
                match_finished = true;
            }
        }

        RingoutResolution {
            awarded_to,
            remaining_stock,
            eliminated,
            match_finished,
        }
    }

    pub fn fighter_eliminated(&self, fighter_id: usize) -> bool {
        !self.fighter_active(fighter_id)
            || (self.rules.uses_stocks()
                && fighter_id < FIGHTER_COUNT
                && self.stocks[fighter_id] <= 0)
    }

    pub fn stock_for(&self, fighter_id: usize) -> Option<i32> {
        if !self.fighter_active(fighter_id) {
            return None;
        }
        self.rules
            .uses_stocks()
            .then_some(())
            .and_then(|_| self.stocks.get(fighter_id).copied())
    }
}

fn stock_array(rules: MatchRules) -> [i32; FIGHTER_COUNT] {
    [rules.starting_stocks.unwrap_or(0); FIGHTER_COUNT]
}

fn active_slot_count(active_slots: [bool; FIGHTER_COUNT]) -> usize {
    active_slots.into_iter().filter(|active| *active).count()
}

pub(crate) fn reconcile_fighter_control_from_setup(
    commands: &mut Commands,
    entity: Entity,
    fighter: &Fighter,
    setup: &LocalSetup,
    controller: &mut Controller,
    has_bot_brain: bool,
) {
    let Some(next_controller) = setup.controller_for_fighter(fighter.id) else {
        return;
    };

    *controller = next_controller;
    if controller.is_bot() {
        if !has_bot_brain {
            commands
                .entity(entity)
                .insert(default_bot_brain_for_fighter(fighter.id));
        }
    } else if has_bot_brain {
        commands.entity(entity).remove::<BotBrain>();
    }
}

fn next_replay_seed(seed: u64) -> u64 {
    seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223)
}

pub fn match_accepts_gameplay(state: Res<MatchState>) -> bool {
    state.is_fighting()
}

pub fn handle_global_input(
    #[cfg(all(feature = "native", not(target_arch = "wasm32")))] keys: Res<ButtonInput<KeyCode>>,
    #[cfg(all(feature = "native", not(target_arch = "wasm32")))] editor: Option<
        Res<MapEditorState>,
    >,
    user_mode: Res<UserModeState>,
    gameplay_scene: Option<Res<crate::user_mode::UserModeGameplayScene>>,
    mut setup: ResMut<LocalSetup>,
    mut state: ResMut<MatchState>,
    mut telemetry: ResMut<MatchTelemetry>,
    mut fighters: Query<(
        Entity,
        (
            &mut Fighter,
            &mut FighterStats,
            &mut FighterMotor,
            &mut FighterInput,
            &mut FighterActionState,
            &mut FighterInventory,
            &mut FighterGrabState,
        ),
        (
            &mut FighterSpecialState,
            &mut FighterCharacter,
            &mut FighterStyle,
            &mut FighterEquipment,
            &mut Transform,
            &mut Visibility,
        ),
        &mut Controller,
        Has<BotBrain>,
    )>,
    hitboxes: Query<Entity, With<Hitbox>>,
    specials: Query<
        Entity,
        Or<(
            With<ActiveSpecial>,
            With<ActiveBeeSkill>,
            With<ActivePenguinSkill>,
            With<ActivePenguinSurface>,
        )>,
    >,
    effects: Query<Entity, With<VisualEffect>>,
    mut items: Query<
        (
            &mut ArenaItem,
            &mut Transform,
            &mut Visibility,
            &mut MeshMaterial3d<StandardMaterial>,
            &mut SceneRoot,
        ),
        Without<Fighter>,
    >,
    item_assets: Option<Res<ItemAssets>>,
    mut commands: Commands,
    mut hitstop: ResMut<Hitstop>,
    mut announcements: ResMut<MatchAnnouncements>,
) {
    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    let dev_input_blocked = user_mode.blocks_dev_input();
    #[cfg(target_arch = "wasm32")]
    let dev_input_blocked = true;
    let starting_from_setup = state.phase == MatchPhase::Setup;
    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    let rematch_pressed = !dev_input_blocked && match_reset_pressed(&keys);
    #[cfg(target_arch = "wasm32")]
    let rematch_pressed = false;
    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    let setup_from_results_pressed = !dev_input_blocked
        && state.phase == MatchPhase::Results
        && keys.just_pressed(KeyCode::Enter);

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    {
        if !dev_input_blocked && keys.just_pressed(KeyCode::KeyH) {
            state.debug_hitboxes = !state.debug_hitboxes;
        }
    }

    if dev_input_blocked && !state.reset_requested {
        return;
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    {
        if !dev_input_blocked
            && state.phase == MatchPhase::Setup
            && !map_editor_allows_setup_input(editor)
        {
            return;
        }
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    {
        if !dev_input_blocked
            && state.is_fighting()
            && dev_character_cycle_pressed(&keys)
            && state.fighter_active(0)
        {
            let next_character = cycle_dev_player_character(&mut setup);
            for (
                _entity,
                (fighter, _stats, _motor, _input, _action, _inventory, _grab_state),
                (_special_state, mut character, _style, _equipment, _transform, _visibility),
                _controller,
                _has_bot_brain,
            ) in &mut fighters
            {
                if fighter.id == 0 {
                    character.kind = next_character;
                    announcements.show(
                        format!("Dev character: {}", character_label(next_character)),
                        0.9,
                    );
                    break;
                }
            }
        }
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    {
        if !dev_input_blocked && setup_rule_hotkeys_active(state.phase) {
            for (key, index) in [
                (KeyCode::Digit1, 0),
                (KeyCode::Digit2, 1),
                (KeyCode::Digit3, 2),
            ] {
                if keys.just_pressed(key) {
                    setup.set_rule(index);
                    state.select_rule(setup.rule_index);
                }
            }
        }
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    if setup_from_results_pressed {
        setup.rule_index = state.rule_index;
        setup.arena_index = state.arena_index;
        setup.replay_seed = state.replay_seed;
        state.return_to_setup();
        state.apply_local_setup(&setup);
        hitstop.remaining = 0.0;
        announcements.show("Setup - adjust or press Enter", 1.2);
        set_active_arena_index(setup.arena_index);
        let arena = active_arena_definition();
        let Some(item_assets) = item_assets.as_ref() else {
            return;
        };

        for entity in &hitboxes {
            commands.entity(entity).despawn();
        }
        for entity in &specials {
            commands.entity(entity).despawn();
        }
        for entity in &effects {
            commands.entity(entity).despawn();
        }
        for (index, (mut item, mut transform, mut visibility, mut material, mut scene_root)) in
            (&mut items).into_iter().enumerate()
        {
            if let Some(anchor) = arena.item_anchors.get(index) {
                item.retarget_for_anchor(anchor.kind, anchor.position, anchor.phase);
                transform.translation = item.anchor;
                transform.scale = item_scale(item.kind);
                material.0 = item_assets.material_for(item.kind, false);
                scene_root.0 = item_assets.scene_for(item.kind);
                *visibility = Visibility::Visible;
            } else {
                item.deactivate_for_match();
                *visibility = Visibility::Hidden;
            }
        }

        for (
            entity,
            (
                mut fighter,
                mut stats,
                mut motor,
                mut input,
                mut action,
                mut inventory,
                mut grab_state,
            ),
            (
                mut special_state,
                mut character,
                mut style,
                mut equipment,
                mut transform,
                mut visibility,
            ),
            mut controller,
            has_bot_brain,
        ) in &mut fighters
        {
            reconcile_fighter_control_from_setup(
                &mut commands,
                entity,
                &fighter,
                &setup,
                &mut controller,
                has_bot_brain,
            );
            fighter.spawn = arena.spawn_points[fighter.id];
            stats.health = MAX_HEALTH;
            stats.stamina = MAX_STAMINA;
            stats.health_refill_timer = 0.0;
            stats.item_speed_timer = 0.0;
            stats.item_giant_timer = 0.0;
            stats.score = 0;
            stats.last_attacker = None;
            stats.invulnerability = 0.0;
            stats.respawn_timer = 0.0;
            stats.hud_flash = 0.0;
            *input = FighterInput::default();
            inventory.held = None;
            grab_state.holding = None;
            grab_state.held_by = None;
            grab_state.regrab_lockout = 0.0;
            special_state.cooldown = 0.0;
            apply_setup_loadout(
                fighter.id,
                &setup,
                &mut character,
                &mut style,
                &mut equipment,
            );
            motor.velocity = Vec3::ZERO;
            motor.grounded = true;
            motor.knockdown_on_land = false;
            motor.landing_aftermath = None;
            motor.reaction_bounces = 0;
            motor.pig_air_meat_slam_air_hits = 0;
            motor.air_attack_used = false;
            motor.jump_attack_landing_recovery = false;
            motor.bee_air_dash_motion_active = false;
            motor.bee_air_dash_shot_available = false;
            motor.ledge_grace_timer = 0.0;
            motor.landing_stick_timer = 0.0;
            motor.jump_takeoff_timer = 0.0;
            motor.impact_speed_limit_timer = 0.0;
            motor.impact_speed_limit = 0.0;
            transform.translation = fighter.spawn;
            transform.rotation = Quat::IDENTITY;
            if state.fighter_active(fighter.id) {
                action.action = FighterAction::Idle;
                action.elapsed = 0.0;
                action.hitbox_spawned = false;
                action.queued_combo = false;
                action.queued_technique = None;
                action.queued_button = None;
                action.buffered_button = None;
                action.buffered_button_elapsed = 0.0;
                action.timeline_events_fired = 0;
                action.reaction_getup_ms = None;
                action.reaction_recover_ms = None;
                action.clear_reaction_visual();
                *visibility = Visibility::Visible;
            } else {
                action.action = FighterAction::RingOut;
                action.elapsed = 0.0;
                action.hitbox_spawned = false;
                action.queued_combo = false;
                action.queued_technique = None;
                action.queued_button = None;
                action.buffered_button = None;
                action.buffered_button_elapsed = 0.0;
                action.timeline_events_fired = 0;
                action.reaction_getup_ms = None;
                action.reaction_recover_ms = None;
                action.clear_reaction_visual();
                stats.respawn_timer = f32::INFINITY;
                *visibility = Visibility::Hidden;
            }
        }
        return;
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    {
        if !dev_input_blocked && state.phase == MatchPhase::Setup {
            if keys.just_pressed(KeyCode::Tab) {
                setup.cycle_selected_character_fighter();
            }
            if keys.just_pressed(KeyCode::KeyQ) {
                setup.selected_character_fighter = 0;
                setup.cycle_character_previous(0);
                announcements.show(
                    format!(
                        "Player character: {}",
                        character_label(setup.player_character())
                    ),
                    0.9,
                );
            }
            if keys.just_pressed(KeyCode::KeyE) {
                setup.selected_character_fighter = 0;
                setup.cycle_character(0);
                announcements.show(
                    format!(
                        "Player character: {}",
                        character_label(setup.player_character())
                    ),
                    0.9,
                );
            }
            if keys.just_pressed(KeyCode::KeyP) && !shift_pressed(&keys) {
                setup.selected_character_fighter = 0;
                setup.set_character(0, CharacterKind::Pig);
                announcements.show("Player character: Pig", 0.9);
            }
            if keys.just_pressed(KeyCode::KeyA) {
                setup.cycle_arena(arena_definitions().len());
                set_active_arena_index(setup.arena_index);
            }
            if keys.just_pressed(KeyCode::KeyB) {
                setup.cycle_bot_count();
            }
            for (key, fighter_id) in [(KeyCode::KeyZ, 0), (KeyCode::KeyX, 1)] {
                if keys.just_pressed(key) {
                    setup.cycle_style(fighter_id);
                }
            }
            for (key, fighter_id) in [(KeyCode::KeyC, 0), (KeyCode::KeyV, 1)] {
                if keys.just_pressed(key) {
                    setup.cycle_character(fighter_id);
                }
            }
            for (key, fighter_id) in [(KeyCode::KeyT, 0), (KeyCode::KeyY, 1)] {
                if keys.just_pressed(key) {
                    setup.cycle_equipment(fighter_id);
                }
            }
            if rematch_pressed {
                state.replay_seed = setup.advance_replay_seed();
                return;
            }

            state.apply_local_setup(&setup);
            for (
                entity,
                (fighter, _stats, _motor, mut input, mut action, _inventory, _grab_state),
                (
                    _special_state,
                    mut character,
                    mut style,
                    mut equipment,
                    _transform,
                    mut visibility,
                ),
                mut controller,
                has_bot_brain,
            ) in &mut fighters
            {
                reconcile_fighter_control_from_setup(
                    &mut commands,
                    entity,
                    &fighter,
                    &setup,
                    &mut controller,
                    has_bot_brain,
                );
                apply_setup_loadout(
                    fighter.id,
                    &setup,
                    &mut character,
                    &mut style,
                    &mut equipment,
                );
                if state.fighter_active(fighter.id) {
                    if action.action == FighterAction::RingOut {
                        action.action = FighterAction::Idle;
                        action.elapsed = 0.0;
                    }
                    *visibility = Visibility::Visible;
                } else {
                    *input = FighterInput::default();
                    action.action = FighterAction::RingOut;
                    action.elapsed = 0.0;
                    *visibility = Visibility::Hidden;
                }
            }
        }
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    {
        if !dev_input_blocked
            && state.phase == MatchPhase::Setup
            && keys.just_pressed(KeyCode::Enter)
        {
            state.rule_index = setup.rule_index;
            state.rules = setup.active_rule();
            state.arena_index = setup.arena_index;
            state.apply_local_setup(&setup);
            state.replay_seed = setup.replay_seed;
            set_active_arena_index(state.arena_index);
            state.request_rematch();
        }
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    if rematch_pressed {
        state.request_rematch();
    }

    if !state.reset_requested {
        return;
    }
    if user_mode.hides_dev_controls() {
        let gameplay_ready = gameplay_scene
            .as_ref()
            .map(|scene| scene.ready_for_battle())
            .unwrap_or(true);
        if !gameplay_ready {
            return;
        }
    }
    let Some(item_assets) = item_assets.as_ref() else {
        return;
    };

    let replay_seed = if starting_from_setup && !rematch_pressed {
        setup.replay_seed
    } else {
        let seed = state.advance_replay_seed();
        setup.replay_seed = seed;
        seed
    };
    state.replay_seed = replay_seed;
    state.reset_for_new_match();
    telemetry.reset_for_seed(replay_seed);
    hitstop.remaining = 0.0;
    announcements.show("Fight!", 0.9);
    set_active_arena_index(state.arena_index);
    let arena = active_arena_definition();

    for entity in &hitboxes {
        commands.entity(entity).despawn();
    }
    for entity in &specials {
        commands.entity(entity).despawn();
    }
    for entity in &effects {
        commands.entity(entity).despawn();
    }
    for (index, (mut item, mut transform, mut visibility, mut material, mut scene_root)) in
        (&mut items).into_iter().enumerate()
    {
        if let Some(anchor) = arena.item_anchors.get(index) {
            item.retarget_for_anchor(anchor.kind, anchor.position, anchor.phase);
            transform.translation = item.anchor;
            transform.scale = item_scale(item.kind);
            material.0 = item_assets.material_for(item.kind, false);
            scene_root.0 = item_assets.scene_for(item.kind);
            *visibility = Visibility::Visible;
        } else {
            item.deactivate_for_match();
            *visibility = Visibility::Hidden;
        }
    }

    for (
        entity,
        (mut fighter, mut stats, mut motor, mut input, mut action, mut inventory, mut grab_state),
        (mut special_state, mut character, mut style, mut equipment, mut transform, mut visibility),
        mut controller,
        has_bot_brain,
    ) in &mut fighters
    {
        reconcile_fighter_control_from_setup(
            &mut commands,
            entity,
            &fighter,
            &setup,
            &mut controller,
            has_bot_brain,
        );
        fighter.spawn = arena.spawn_points[fighter.id];
        stats.health = MAX_HEALTH;
        stats.stamina = MAX_STAMINA;
        stats.health_refill_timer = 0.0;
        stats.item_speed_timer = 0.0;
        stats.item_giant_timer = 0.0;
        stats.score = 0;
        stats.last_attacker = None;
        stats.invulnerability = 0.0;
        stats.respawn_timer = 0.0;
        stats.hud_flash = 0.0;
        *input = FighterInput::default();
        inventory.held = None;
        grab_state.holding = None;
        grab_state.held_by = None;
        grab_state.regrab_lockout = 0.0;
        special_state.cooldown = 0.0;
        apply_setup_loadout(
            fighter.id,
            &setup,
            &mut character,
            &mut style,
            &mut equipment,
        );
        if !state.fighter_active(fighter.id) {
            action.action = FighterAction::RingOut;
            action.elapsed = 0.0;
            action.hitbox_spawned = false;
            action.queued_combo = false;
            action.queued_technique = None;
            action.queued_button = None;
            action.buffered_button = None;
            action.buffered_button_elapsed = 0.0;
            action.timeline_events_fired = 0;
            action.reaction_getup_ms = None;
            action.reaction_recover_ms = None;
            action.clear_reaction_visual();
            stats.respawn_timer = f32::INFINITY;
            motor.velocity = Vec3::ZERO;
            motor.grounded = true;
            motor.knockdown_on_land = false;
            motor.landing_aftermath = None;
            motor.reaction_bounces = 0;
            motor.pig_air_meat_slam_air_hits = 0;
            motor.air_attack_used = false;
            motor.jump_attack_landing_recovery = false;
            motor.bee_air_dash_motion_active = false;
            motor.bee_air_dash_shot_available = false;
            motor.ledge_grace_timer = 0.0;
            motor.landing_stick_timer = 0.0;
            motor.jump_takeoff_timer = 0.0;
            motor.impact_speed_limit_timer = 0.0;
            motor.impact_speed_limit = 0.0;
            transform.translation = fighter.spawn;
            transform.rotation = Quat::IDENTITY;
            *visibility = Visibility::Hidden;
            continue;
        }
        action.action = FighterAction::Idle;
        action.elapsed = 0.0;
        action.hitbox_spawned = false;
        action.queued_combo = false;
        action.queued_technique = None;
        action.queued_button = None;
        action.buffered_button = None;
        action.buffered_button_elapsed = 0.0;
        action.timeline_events_fired = 0;
        action.reaction_getup_ms = None;
        action.reaction_recover_ms = None;
        action.clear_reaction_visual();
        motor.velocity = Vec3::ZERO;
        motor.grounded = true;
        motor.knockdown_on_land = false;
        motor.landing_aftermath = None;
        motor.reaction_bounces = 0;
        motor.pig_air_meat_slam_air_hits = 0;
        motor.air_attack_used = false;
        motor.jump_attack_landing_recovery = false;
        motor.bee_air_dash_motion_active = false;
        motor.bee_air_dash_shot_available = false;
        motor.ledge_grace_timer = 0.0;
        motor.landing_stick_timer = 0.0;
        motor.jump_takeoff_timer = 0.0;
        motor.impact_speed_limit_timer = 0.0;
        motor.impact_speed_limit = 0.0;
        transform.translation = fighter.spawn;
        transform.rotation = Quat::IDENTITY;
        *visibility = Visibility::Visible;
    }
}

pub fn sync_setup_character_scene_models(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    character_catalog: Res<CharacterMoveCatalog>,
    fighters: Query<(&Fighter, &FighterCharacter), Changed<FighterCharacter>>,
    mut scene_models: Query<(Entity, &FighterSceneModel, &ChildOf, &Transform)>,
) {
    for (fighter, character) in &fighters {
        sync_setup_character_scene(
            fighter.id,
            character.kind,
            &asset_server,
            &character_catalog,
            &mut commands,
            &mut scene_models,
        );
    }
}

pub fn tick_match_timer(
    time: Res<Time>,
    mut state: ResMut<MatchState>,
    mut announcements: ResMut<MatchAnnouncements>,
    mut feedback: ResMut<HitEffects>,
) {
    if let Some(event) = state.advance_phase(time.delta_secs()) {
        match event {
            MatchPhaseEvent::TimeUp => {
                announcements.show("Time up", 1.1);
                feedback.push_feedback_cue("match_timeup", ImpactSource::MatchFlow, 22);
            }
            MatchPhaseEvent::Results => {
                announcements.show("Results - press R", 1.4);
                feedback.push_feedback_cue("match_results", ImpactSource::MatchFlow, 28);
            }
        }
    }
}

pub fn tick_hitstop(time: Res<Time>, mut hitstop: ResMut<Hitstop>) {
    if hitstop.remaining > 0.0 {
        hitstop.remaining = (hitstop.remaining - time.delta_secs()).max(0.0);
    }
}

pub fn tick_announcements(time: Res<Time>, mut announcements: ResMut<MatchAnnouncements>) {
    if announcements.timer > 0.0 {
        announcements.timer = (announcements.timer - time.delta_secs()).max(0.0);
        if announcements.timer == 0.0 {
            announcements.message.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{
        BotBrain, Controller, Fighter, LocalInputAssignment, ParticipantKind, PlayerSlotId,
    };

    fn reconcile_test_system(
        mut commands: Commands,
        setup: Res<LocalSetup>,
        mut fighters: Query<(Entity, &Fighter, &mut Controller, Has<BotBrain>)>,
    ) {
        for (entity, fighter, mut controller, has_bot_brain) in &mut fighters {
            reconcile_fighter_control_from_setup(
                &mut commands,
                entity,
                fighter,
                &setup,
                &mut controller,
                has_bot_brain,
            );
        }
    }

    fn test_fighter(fighter_id: usize) -> Fighter {
        Fighter {
            id: fighter_id,
            name: "Test",
            color: Color::srgb(1.0, 1.0, 1.0),
            spawn: Vec3::ZERO,
        }
    }

    fn controller_for(fighter_id: usize, participant: ParticipantKind) -> Controller {
        Controller::new(
            PlayerSlotId::new(fighter_id).unwrap(),
            participant,
            LocalInputAssignment::Unassigned,
        )
    }

    fn reconcile_test_entity(
        setup: LocalSetup,
        fighter_id: usize,
        stale_controller: Controller,
        starts_with_bot_brain: bool,
    ) -> (Controller, bool) {
        let mut app = App::new();
        app.insert_resource(setup);
        let entity = {
            let mut entity = app
                .world_mut()
                .spawn((test_fighter(fighter_id), stale_controller));
            if starts_with_bot_brain {
                entity.insert(default_bot_brain_for_fighter(fighter_id));
            }
            entity.id()
        };
        app.add_systems(Update, reconcile_test_system);
        app.update();

        let controller = *app.world().get::<Controller>(entity).unwrap();
        let has_bot_brain = app.world().get::<BotBrain>(entity).is_some();
        (controller, has_bot_brain)
    }

    #[test]
    fn rule_presets_cover_timed_ffa_and_stock() {
        assert_eq!(RULE_PRESETS.len(), 3);
        assert!(
            RULE_PRESETS
                .iter()
                .any(|rules| rules.preset == RulePreset::TimedTeamScore
                    && rules.team_scoring
                    && !rules.friendly_fire)
        );
        assert!(
            RULE_PRESETS
                .iter()
                .any(|rules| rules.preset == RulePreset::FreeForAll
                    && !rules.team_scoring
                    && rules.friendly_fire)
        );
        assert!(
            RULE_PRESETS
                .iter()
                .any(|rules| rules.preset == RulePreset::StockRingOut
                    && rules.uses_stocks()
                    && rules.friendly_fire)
        );
    }

    #[test]
    fn team_helpers_block_friendly_fire_only_for_team_rules() {
        let team_rules = RULE_PRESETS[0];
        let ffa_rules = RULE_PRESETS[1];

        assert_eq!(fighter_team(0), Some(TeamId::Red));
        assert_eq!(fighter_team(1), Some(TeamId::Blue));
        assert_eq!(fighter_team(2), Some(TeamId::Red));
        assert_eq!(fighter_team(usize::MAX), None);
        assert!(fighters_share_team(0, 2));
        assert!(!fighters_share_team(0, 1));
        assert!(!combat_target_allowed(team_rules, 0, 0));
        assert!(!combat_target_allowed(team_rules, 0, 2));
        assert!(combat_target_allowed(team_rules, 0, 1));
        assert!(combat_target_allowed(team_rules, usize::MAX, 0));
        assert!(combat_target_allowed(ffa_rules, 0, 2));
    }

    #[test]
    fn local_setup_defaults_match_current_starting_loadout() {
        let setup = LocalSetup::default();
        assert_eq!(setup.rule_index, 0);
        assert_eq!(setup.arena_index, 0);
        assert_eq!(setup.selected_character_fighter, 0);
        assert_eq!(setup.active_slots(), [true, true, false, false]);
        assert_eq!(setup.active_bot_count(), 1);
        assert_eq!(setup.slots[0].participant, ParticipantKind::Human);
        assert_eq!(setup.slots[0].input, LocalInputAssignment::Keyboard(0));
        assert_eq!(setup.slots[1].participant, ParticipantKind::Bot);
        assert_eq!(setup.slots[2].participant, ParticipantKind::Closed);
        assert_eq!(setup.slots[0].style, DEFAULT_FIGHTER_STYLES[0]);
        assert_eq!(setup.slots[0].equipment, DEFAULT_FIGHTER_EQUIPMENT[0]);
        assert_eq!(setup.replay_seed, DEFAULT_REPLAY_SEED);
    }

    #[test]
    fn local_setup_maps_classic_modes_into_slots() {
        let mut setup = LocalSetup::default();

        setup.configure_single_player_duel(CharacterKind::Pig, CharacterKind::Cat);
        assert_eq!(setup.active_slots(), [true, true, false, false]);
        assert_eq!(setup.active_bot_count(), 1);
        assert_eq!(setup.slots[0].participant, ParticipantKind::Human);
        assert_eq!(setup.slots[0].input, LocalInputAssignment::Keyboard(0));
        assert_eq!(setup.slots[0].character, CharacterKind::Pig);
        assert_eq!(setup.slots[1].participant, ParticipantKind::Bot);
        assert_eq!(setup.slots[1].input, LocalInputAssignment::Unassigned);
        assert_eq!(setup.slots[1].character, CharacterKind::Cat);

        setup.configure_two_player_duel(CharacterKind::Cat, CharacterKind::Pig);
        assert_eq!(setup.active_slots(), [true, true, false, false]);
        assert_eq!(setup.active_bot_count(), 0);
        assert_eq!(setup.slots[0].participant, ParticipantKind::Human);
        assert_eq!(setup.slots[0].input, LocalInputAssignment::Keyboard(0));
        assert_eq!(setup.slots[1].participant, ParticipantKind::Human);
        assert_eq!(setup.slots[1].input, LocalInputAssignment::Keyboard(1));
        assert_eq!(setup.slots[2].participant, ParticipantKind::Closed);
    }

    #[test]
    fn reconcile_switching_slot_from_human_back_to_bot_updates_controller() {
        let mut setup = LocalSetup::default();
        setup.configure_two_player_duel(CharacterKind::Cat, CharacterKind::Pig);
        let stale_human_controller = setup.controller_for_fighter(1).unwrap();
        setup.configure_single_player_duel(CharacterKind::Cat, CharacterKind::Pig);

        let (controller, has_bot_brain) =
            reconcile_test_entity(setup, 1, stale_human_controller, false);

        assert_eq!(controller.participant, ParticipantKind::Bot);
        assert_eq!(controller.input, LocalInputAssignment::Unassigned);
        assert!(has_bot_brain);
    }

    #[test]
    fn reconcile_new_bot_slot_gets_bot_control_state() {
        let mut setup = LocalSetup::default();
        setup.slots[2].participant = ParticipantKind::Bot;
        let stale_closed_controller = controller_for(2, ParticipantKind::Closed);

        let (controller, has_bot_brain) =
            reconcile_test_entity(setup, 2, stale_closed_controller, false);

        assert_eq!(controller.participant, ParticipantKind::Bot);
        assert!(has_bot_brain);
    }

    #[test]
    fn reconcile_human_and_closed_slots_remove_stale_bot_brain() {
        for participant in [ParticipantKind::Human, ParticipantKind::Closed] {
            let mut setup = LocalSetup::default();
            setup.slots[1].participant = participant;
            setup.slots[1].input = if participant == ParticipantKind::Human {
                LocalInputAssignment::Keyboard(1)
            } else {
                LocalInputAssignment::Unassigned
            };
            let stale_bot_controller = controller_for(1, ParticipantKind::Bot);

            let (controller, has_bot_brain) =
                reconcile_test_entity(setup, 1, stale_bot_controller, true);

            assert_eq!(controller.participant, participant);
            assert!(!has_bot_brain);
        }
    }

    #[test]
    fn match_reset_requires_unmodified_r() {
        let mut plain_r = ButtonInput::default();
        plain_r.press(KeyCode::KeyR);
        assert!(match_reset_pressed(&plain_r));

        for modifier in [
            KeyCode::ShiftLeft,
            KeyCode::ShiftRight,
            KeyCode::ControlLeft,
            KeyCode::ControlRight,
            KeyCode::AltLeft,
            KeyCode::AltRight,
            KeyCode::SuperLeft,
            KeyCode::SuperRight,
        ] {
            let mut modified_r = ButtonInput::default();
            modified_r.press(modifier);
            modified_r.press(KeyCode::KeyR);
            assert!(!match_reset_pressed(&modified_r));
        }
    }

    #[test]
    fn dev_character_cycle_requires_shift_p() {
        let mut plain_p = ButtonInput::default();
        plain_p.press(KeyCode::KeyP);
        assert!(!dev_character_cycle_pressed(&plain_p));

        let mut shifted_p = ButtonInput::default();
        shifted_p.press(KeyCode::ShiftLeft);
        shifted_p.press(KeyCode::KeyP);
        assert!(dev_character_cycle_pressed(&shifted_p));

        let mut shifted_other = ButtonInput::default();
        shifted_other.press(KeyCode::ShiftRight);
        shifted_other.press(KeyCode::KeyC);
        assert!(!dev_character_cycle_pressed(&shifted_other));
    }

    #[test]
    fn setup_rule_hotkeys_are_setup_only() {
        assert!(setup_rule_hotkeys_active(MatchPhase::Setup));
        assert!(!setup_rule_hotkeys_active(MatchPhase::Fighting));
        assert!(!setup_rule_hotkeys_active(MatchPhase::Results));
        assert!(!setup_rule_hotkeys_active(MatchPhase::Resetting));
    }

    #[test]
    fn local_setup_cycles_rule_arena_bots_styles_equipment_and_seed() {
        let mut setup = LocalSetup::default();
        setup.set_rule(2);
        setup.cycle_arena(2);
        setup.cycle_bot_count();
        setup.cycle_style(0);
        setup.cycle_equipment(0);
        let seed = setup.advance_replay_seed();

        assert_eq!(setup.active_rule().preset, RulePreset::StockRingOut);
        assert_eq!(setup.arena_index, 1);
        assert_eq!(setup.active_bot_count(), 1);
        assert_ne!(setup.slots[0].style, DEFAULT_FIGHTER_STYLES[0]);
        assert_ne!(setup.slots[0].equipment, DEFAULT_FIGHTER_EQUIPMENT[0]);
        assert_ne!(seed, DEFAULT_REPLAY_SEED);
    }

    #[test]
    fn local_setup_selected_character_controls_player_and_bot_slots() {
        let mut setup = LocalSetup::default();
        assert_eq!(setup.selected_character_fighter(), 0);
        assert_eq!(setup.slots[0].character, CharacterKind::Cat);

        setup.set_character(0, CharacterKind::Pig);
        assert_eq!(setup.player_character(), CharacterKind::Pig);

        setup.set_character(0, CharacterKind::Cat);
        setup.cycle_character(0);
        assert_eq!(setup.slots[0].character, CharacterKind::Pig);

        setup.cycle_character_previous(0);
        assert_eq!(setup.slots[0].character, CharacterKind::Cat);

        setup.cycle_selected_character_fighter();
        assert_eq!(setup.selected_character_fighter(), 1);
        setup.cycle_character(setup.selected_character_fighter());
        assert_ne!(setup.slots[1].character, character_for_fighter_id(1));
    }

    #[test]
    fn dev_character_cycle_updates_player_slot() {
        let mut setup = LocalSetup::default();

        let next = cycle_dev_player_character(&mut setup);

        assert_eq!(next, CharacterKind::Pig);
        assert_eq!(setup.player_character(), CharacterKind::Pig);
        assert_eq!(setup.selected_character_fighter(), 0);
    }

    #[test]
    fn active_fighter_count_tracks_occupied_slots() {
        let mut setup = LocalSetup::default();
        let mut state = MatchState::default();
        state.apply_local_setup(&setup);
        assert!(state.fighter_active(1));
        assert!(!state.fighter_active(2));
        assert_eq!(state.stock_for(2), None);

        setup.slots[2].participant = ParticipantKind::Bot;
        state.apply_local_setup(&setup);
        assert_eq!(state.active_fighter_count, 3);
        assert!(state.fighter_active(2));
    }

    #[test]
    fn combat_state_filters_inactive_owners_and_targets() {
        let state = MatchState::default();

        assert!(state.combat_target_allowed_for_state(0, 1));
        assert!(!state.combat_target_allowed_for_state(0, 2));
        assert!(!state.combat_target_allowed_for_state(2, 0));
        assert!(state.combat_target_allowed_for_state(usize::MAX, 0));
    }

    #[test]
    fn match_phase_advances_from_countdown_to_results() {
        let mut state = MatchState::default();
        assert_eq!(state.phase, MatchPhase::Setup);
        state.arena_index = 1;
        state.rules.time_limit = Some(MATCH_SECONDS);
        state.reset_for_new_match();
        assert_eq!(state.phase, MatchPhase::Fighting);
        assert_eq!(state.arena_index, 1);
        assert_eq!(
            state.advance_phase(MATCH_SECONDS),
            Some(MatchPhaseEvent::TimeUp)
        );
        assert_eq!(state.phase, MatchPhase::TimeUp);
        assert_eq!(
            state.advance_phase(TIME_UP_SECONDS),
            Some(MatchPhaseEvent::Results)
        );
        assert_eq!(state.phase, MatchPhase::Results);
        state.return_to_setup();
        assert_eq!(state.phase, MatchPhase::Setup);
        assert!(!state.reset_requested);
    }

    #[test]
    fn telemetry_tracks_local_match_stats() {
        let mut telemetry = MatchTelemetry::default();
        telemetry.record_ringout(true);
        telemetry.record_ringout(false);
        telemetry.record_damage(1, 12.5);
        telemetry.record_item_hit();
        telemetry.record_throw();
        telemetry.record_guard_break();

        assert_eq!(telemetry.ring_outs, 1);
        assert_eq!(telemetry.falls, 1);
        assert_eq!(telemetry.item_hits, 1);
        assert_eq!(telemetry.throws, 1);
        assert_eq!(telemetry.guard_breaks, 1);
        assert_eq!(telemetry.total_damage(), 12.5);
    }

    #[test]
    fn all_rule_presets_use_lives() {
        assert!(RULE_PRESETS.iter().all(|rules| rules.uses_stocks()));
    }

    #[test]
    fn knockout_life_loss_tracks_elimination_and_match_finish() {
        let mut state = MatchState::default();
        state.select_rule(1);
        state.reset_for_new_match();
        state.phase = MatchPhase::Fighting;

        let first = state.record_life_loss(1, Some(0), LifeLossCause::Knockout);
        assert_eq!(first.awarded_to, Some(0));
        assert_eq!(first.remaining_stock, Some(STOCK_LIVES - 1));
        assert!(!first.eliminated);

        state.stocks = [1, 1, 0, 0];
        let final_ko = state.record_life_loss(1, Some(0), LifeLossCause::Knockout);
        assert!(final_ko.eliminated);
        assert!(final_ko.match_finished);
        assert_eq!(state.phase, MatchPhase::Results);
    }

    #[test]
    fn stock_ringouts_track_elimination_and_match_finish() {
        let mut state = MatchState::default();
        state.select_rule(2);
        state.reset_for_new_match();
        state.phase = MatchPhase::Fighting;

        let first = state.record_ringout(1, Some(0));
        assert_eq!(first.awarded_to, Some(0));
        assert_eq!(first.remaining_stock, Some(STOCK_LIVES - 1));
        assert!(!first.eliminated);

        state.stocks = [1, 1, 0, 0];
        let final_ringout = state.record_ringout(1, Some(0));
        assert!(final_ringout.eliminated);
        assert!(final_ringout.match_finished);
        assert_eq!(state.phase, MatchPhase::Results);
    }

    #[test]
    fn stock_ringouts_ignore_inactive_bot_slots() {
        let mut state = MatchState::default();
        state.select_rule(2);
        state.reset_for_new_match();
        state.phase = MatchPhase::Fighting;
        state.stocks = [1, 1, 0, STOCK_LIVES];

        let final_ringout = state.record_ringout(1, Some(0));

        assert!(final_ringout.match_finished);
        assert_eq!(final_ringout.awarded_to, Some(0));
        assert_eq!(state.phase, MatchPhase::Results);
    }

    #[test]
    fn team_ringout_credit_rejects_teammates_and_invalid_attackers() {
        let mut state = MatchState::default();

        assert_eq!(state.record_ringout(2, Some(0)).awarded_to, None);
        assert_eq!(state.record_ringout(1, Some(0)).awarded_to, Some(0));
        assert_eq!(state.record_ringout(1, Some(usize::MAX)).awarded_to, None);
    }

    #[test]
    fn setup_loadout_applies_selected_style_and_equipment() {
        let mut setup = LocalSetup::default();
        setup.slots[0].character = CharacterKind::Panda;
        setup.slots[0].style = FighterStyleKind::Catalyst;
        setup.slots[0].equipment = EquipmentKind::HeavySeal;
        let mut character = FighterCharacter::new(CharacterKind::Cat);
        let mut style = FighterStyle {
            kind: FighterStyleKind::Anchor,
        };
        let mut equipment = FighterEquipment {
            kind: EquipmentKind::DashCoil,
            cooldown: 2.0,
        };

        apply_setup_loadout(0, &setup, &mut character, &mut style, &mut equipment);

        assert_eq!(character.kind, CharacterKind::Panda);
        assert_eq!(style.kind, FighterStyleKind::Catalyst);
        assert_eq!(equipment.kind, EquipmentKind::HeavySeal);
        assert_eq!(equipment.cooldown, 0.0);
    }
}
