use bevy::prelude::*;

use crate::characters::CharacterKind;
use crate::constants::{
    FIGHTER_COUNT, GUARD_COUNTER_WINDOW, ITEM_COFFEE_SPEED_MULTIPLIER,
    ITEM_GIANT_DAMAGE_TAKEN_MULTIPLIER, ITEM_GIANT_SIZE_MULTIPLIER, MAX_HEALTH, MAX_STAMINA,
};
use crate::effects::HitImpactEffectId;
use crate::equipment::EquipmentKind;
use crate::reactions::{QueuedAftermath, ReactionFamilyId};
use crate::styles::FighterStyleKind;
use crate::techniques::{
    AttackPayloadId, AttackShapeId, DamageElement, DamageProfileId, TechniqueButton, TechniqueId,
};

#[derive(Component, Clone)]
pub struct Fighter {
    pub id: usize,
    pub name: &'static str,
    pub color: Color,
    pub spawn: Vec3,
}

#[derive(Component)]
pub struct FighterStats {
    pub health: f32,
    pub stamina: f32,
    pub score: i32,
    pub last_attacker: Option<usize>,
    pub invulnerability: f32,
    pub health_refill_timer: f32,
    pub respawn_timer: f32,
    pub hud_flash: f32,
    pub element_carry: Option<DamageElement>,
    pub element_carry_strength: f32,
    pub element_carry_timer: f32,
    pub item_speed_timer: f32,
    pub item_giant_timer: f32,
}

impl Default for FighterStats {
    fn default() -> Self {
        Self {
            health: MAX_HEALTH,
            stamina: MAX_STAMINA,
            score: 0,
            last_attacker: None,
            invulnerability: 0.0,
            health_refill_timer: 0.0,
            respawn_timer: 0.0,
            hud_flash: 0.0,
            element_carry: None,
            element_carry_strength: 0.0,
            element_carry_timer: 0.0,
            item_speed_timer: 0.0,
            item_giant_timer: 0.0,
        }
    }
}

impl FighterStats {
    pub fn item_speed_multiplier(&self) -> f32 {
        if self.item_speed_timer > 0.0 {
            ITEM_COFFEE_SPEED_MULTIPLIER
        } else {
            1.0
        }
    }

    pub fn item_size_multiplier(&self) -> f32 {
        if self.item_giant_timer > 0.0 {
            ITEM_GIANT_SIZE_MULTIPLIER
        } else {
            1.0
        }
    }

    pub fn item_damage_taken_multiplier(&self) -> f32 {
        if self.item_giant_timer > 0.0 {
            ITEM_GIANT_DAMAGE_TAKEN_MULTIPLIER
        } else {
            1.0
        }
    }
}

#[derive(Component)]
pub struct FighterMotor {
    pub velocity: Vec3,
    pub facing: Vec3,
    pub grounded: bool,
    pub knockdown_on_land: bool,
    pub landing_aftermath: Option<QueuedAftermath>,
    pub air_attack_used: bool,
    pub queued_air_attack: Option<TechniqueButton>,
    pub queued_air_attack_timer: f32,
    pub jump_attack_landing_recovery: bool,
    pub bee_air_dash_motion_active: bool,
    pub bee_air_dash_shot_available: bool,
    pub ledge_grace_timer: f32,
    pub landing_stick_timer: f32,
    pub jump_takeoff_timer: f32,
    pub reaction_bounces: u8,
    pub pig_air_meat_slam_air_hits: u8,
    pub dash_trail_timer: f32,
    pub dash_slide_timer: f32,
    pub dash_jump_carry_timer: f32,
    pub dash_jump_carry_speed_limit: f32,
    pub impact_speed_limit_timer: f32,
    pub impact_speed_limit: f32,
    pub penguin_ice_slide_direction: Option<Vec3>,
    pub penguin_ice_slide_speed: f32,
    pub guard_active_timer: f32,
    pub guard_cooldown_timer: f32,
    pub guard_start_buffer_timer: f32,
    pub guard_was_requested: bool,
    pub guard_counter_window_timer: f32,
    pub guard_counter_source: Option<Vec3>,
    pub guard_counter_buffered: bool,
}

impl Default for FighterMotor {
    fn default() -> Self {
        Self {
            velocity: Vec3::ZERO,
            facing: Vec3::Z,
            grounded: true,
            knockdown_on_land: false,
            landing_aftermath: None,
            air_attack_used: false,
            queued_air_attack: None,
            queued_air_attack_timer: 0.0,
            jump_attack_landing_recovery: false,
            bee_air_dash_motion_active: false,
            bee_air_dash_shot_available: false,
            ledge_grace_timer: 0.0,
            landing_stick_timer: 0.0,
            jump_takeoff_timer: 0.0,
            reaction_bounces: 0,
            pig_air_meat_slam_air_hits: 0,
            dash_trail_timer: 0.0,
            dash_slide_timer: 0.0,
            dash_jump_carry_timer: 0.0,
            dash_jump_carry_speed_limit: 0.0,
            impact_speed_limit_timer: 0.0,
            impact_speed_limit: 0.0,
            penguin_ice_slide_direction: None,
            penguin_ice_slide_speed: 0.0,
            guard_active_timer: 0.0,
            guard_cooldown_timer: 0.0,
            guard_start_buffer_timer: 0.0,
            guard_was_requested: false,
            guard_counter_window_timer: 0.0,
            guard_counter_source: None,
            guard_counter_buffered: false,
        }
    }
}

impl FighterMotor {
    pub fn open_guard_counter_window(&mut self, source: Vec3) {
        self.guard_counter_window_timer = GUARD_COUNTER_WINDOW;
        self.guard_counter_source = Some(source);
    }

    pub fn clear_guard_counter_window(&mut self) {
        self.guard_counter_window_timer = 0.0;
        self.guard_counter_source = None;
        self.guard_counter_buffered = false;
    }
}

#[derive(Component, Default)]
pub struct FighterInput {
    pub movement: Vec2,
    pub aim: bool,
    pub jump: bool,
    pub dash: bool,
    pub light: bool,
    pub light_held: bool,
    pub raw_light_pressed: bool,
    pub heavy: bool,
    pub heavy_held: bool,
    pub raw_heavy_pressed: bool,
    pub heavy_released: bool,
    pub grab: bool,
    pub guard: bool,
    pub ultimate: bool,
    pub special: bool,
}

#[derive(Component, Default)]
pub struct FighterInventory {
    pub held: Option<Entity>,
}

#[derive(Component, Default)]
pub struct FighterGrabState {
    pub holding: Option<Entity>,
    pub held_by: Option<Entity>,
    pub regrab_lockout: f32,
}

#[derive(Component, Default)]
pub struct FighterSpecialState {
    pub cooldown: f32,
}

#[derive(Component, Default)]
pub struct FighterUltimateState {
    pub target: Option<Entity>,
    pub owner: Option<Entity>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PlayerSlotId(usize);

impl PlayerSlotId {
    pub const fn new(index: usize) -> Option<Self> {
        if index < FIGHTER_COUNT {
            Some(Self(index))
        } else {
            None
        }
    }

    pub const fn index(self) -> usize {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParticipantKind {
    Human,
    Bot,
    Closed,
}

impl ParticipantKind {
    pub const fn is_occupied(self) -> bool {
        !matches!(self, Self::Closed)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalInputAssignment {
    Keyboard(usize),
    Unassigned,
}

impl Default for LocalInputAssignment {
    fn default() -> Self {
        Self::Unassigned
    }
}

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Controller {
    pub slot: PlayerSlotId,
    pub participant: ParticipantKind,
    pub input: LocalInputAssignment,
}

impl Controller {
    pub const fn new(
        slot: PlayerSlotId,
        participant: ParticipantKind,
        input: LocalInputAssignment,
    ) -> Self {
        Self {
            slot,
            participant,
            input,
        }
    }

    pub const fn closed(slot: PlayerSlotId) -> Self {
        Self::new(
            slot,
            ParticipantKind::Closed,
            LocalInputAssignment::Unassigned,
        )
    }

    pub const fn is_human(self) -> bool {
        matches!(self.participant, ParticipantKind::Human)
    }

    pub const fn is_bot(self) -> bool {
        matches!(self.participant, ParticipantKind::Bot)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlAction {
    Left,
    Right,
    Up,
    Down,
    AimGrab,
    Heavy,
    Light,
    Jump,
}

impl ControlAction {
    pub const ALL: [Self; 8] = [
        Self::Left,
        Self::Right,
        Self::Up,
        Self::Down,
        Self::AimGrab,
        Self::Heavy,
        Self::Light,
        Self::Jump,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Left => "Left",
            Self::Right => "Right",
            Self::Up => "Up",
            Self::Down => "Down",
            Self::AimGrab => "Aim",
            Self::Heavy => "Heavy",
            Self::Light => "Light",
            Self::Jump => "Jump",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayerControlBindings {
    pub left: KeyCode,
    pub right: KeyCode,
    pub up: KeyCode,
    pub down: KeyCode,
    pub aim_grab: KeyCode,
    pub heavy: KeyCode,
    pub light: KeyCode,
    pub jump: KeyCode,
}

impl PlayerControlBindings {
    pub fn player_one_default() -> Self {
        Self {
            left: KeyCode::ArrowLeft,
            right: KeyCode::ArrowRight,
            up: KeyCode::ArrowUp,
            down: KeyCode::ArrowDown,
            aim_grab: KeyCode::KeyZ,
            heavy: KeyCode::KeyX,
            light: KeyCode::KeyC,
            jump: KeyCode::KeyV,
        }
    }

    pub fn player_two_default() -> Self {
        Self {
            left: KeyCode::KeyA,
            right: KeyCode::KeyD,
            up: KeyCode::KeyW,
            down: KeyCode::KeyS,
            aim_grab: KeyCode::KeyT,
            heavy: KeyCode::KeyY,
            light: KeyCode::KeyU,
            jump: KeyCode::KeyI,
        }
    }

    pub fn key(self, action: ControlAction) -> KeyCode {
        match action {
            ControlAction::Left => self.left,
            ControlAction::Right => self.right,
            ControlAction::Up => self.up,
            ControlAction::Down => self.down,
            ControlAction::AimGrab => self.aim_grab,
            ControlAction::Heavy => self.heavy,
            ControlAction::Light => self.light,
            ControlAction::Jump => self.jump,
        }
    }

    pub fn set_key(&mut self, action: ControlAction, key: KeyCode) {
        match action {
            ControlAction::Left => self.left = key,
            ControlAction::Right => self.right = key,
            ControlAction::Up => self.up = key,
            ControlAction::Down => self.down = key,
            ControlAction::AimGrab => self.aim_grab = key,
            ControlAction::Heavy => self.heavy = key,
            ControlAction::Light => self.light = key,
            ControlAction::Jump => self.jump = key,
        }
    }

    pub fn keys(self) -> [KeyCode; 8] {
        [
            self.left,
            self.right,
            self.up,
            self.down,
            self.aim_grab,
            self.heavy,
            self.light,
            self.jump,
        ]
    }
}

#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct PlayerKeyBindings {
    pub p1: PlayerControlBindings,
    pub p2: PlayerControlBindings,
}

impl Default for PlayerKeyBindings {
    fn default() -> Self {
        Self {
            p1: PlayerControlBindings::player_one_default(),
            p2: PlayerControlBindings::player_two_default(),
        }
    }
}

impl PlayerKeyBindings {
    pub fn bindings_for_assignment(
        &self,
        assignment: LocalInputAssignment,
    ) -> Option<PlayerControlBindings> {
        match assignment {
            LocalInputAssignment::Keyboard(0) => Some(self.p1),
            LocalInputAssignment::Keyboard(1) => Some(self.p2),
            _ => None,
        }
    }

    pub fn key_for(&self, player: usize, action: ControlAction) -> Option<KeyCode> {
        match player {
            0 => Some(self.p1.key(action)),
            1 => Some(self.p2.key(action)),
            _ => None,
        }
    }

    #[allow(dead_code)]
    pub fn try_set_key(
        &mut self,
        player: usize,
        action: ControlAction,
        key: KeyCode,
    ) -> Result<(), &'static str> {
        if reserved_binding_key(key) {
            return Err("reserved");
        }
        if self.all_keys().into_iter().any(|existing| existing == key) {
            return Err("duplicate");
        }
        self.set_key_for(player, action, key)?;
        Ok(())
    }

    pub fn try_set_key_swapping(
        &mut self,
        player: usize,
        action: ControlAction,
        key: KeyCode,
    ) -> Result<Option<(usize, ControlAction)>, &'static str> {
        if reserved_binding_key(key) {
            return Err("reserved");
        }

        let old_key = self.key_for(player, action).ok_or("player")?;
        if old_key == key {
            return Ok(None);
        }

        let displaced = self.key_owner(key);
        self.set_key_for(player, action, key)?;
        if let Some((displaced_player, displaced_action)) = displaced {
            self.set_key_for(displaced_player, displaced_action, old_key)?;
        }
        Ok(displaced)
    }

    fn set_key_for(
        &mut self,
        player: usize,
        action: ControlAction,
        key: KeyCode,
    ) -> Result<(), &'static str> {
        match player {
            0 => self.p1.set_key(action, key),
            1 => self.p2.set_key(action, key),
            _ => return Err("player"),
        }
        Ok(())
    }

    fn key_owner(&self, key: KeyCode) -> Option<(usize, ControlAction)> {
        for player in 0..2 {
            for action in ControlAction::ALL {
                if self.key_for(player, action) == Some(key) {
                    return Some((player, action));
                }
            }
        }
        None
    }

    #[allow(dead_code)]
    pub fn has_duplicate_keys(&self) -> bool {
        let keys = self.all_keys();
        keys.iter()
            .enumerate()
            .any(|(index, key)| keys.iter().skip(index + 1).any(|other| other == key))
    }

    pub fn all_keys(&self) -> [KeyCode; 16] {
        let p1 = self.p1.keys();
        let p2 = self.p2.keys();
        [
            p1[0], p1[1], p1[2], p1[3], p1[4], p1[5], p1[6], p1[7], p2[0], p2[1], p2[2], p2[3],
            p2[4], p2[5], p2[6], p2[7],
        ]
    }
}

pub fn reserved_binding_key(key: KeyCode) -> bool {
    matches!(
        key,
        KeyCode::Escape
            | KeyCode::Enter
            | KeyCode::Tab
            | KeyCode::ShiftLeft
            | KeyCode::ShiftRight
            | KeyCode::ControlLeft
            | KeyCode::ControlRight
            | KeyCode::AltLeft
            | KeyCode::AltRight
            | KeyCode::SuperLeft
            | KeyCode::SuperRight
    )
}

#[derive(Component, Clone, Copy, PartialEq, Eq, Debug, serde::Deserialize)]
pub enum FighterAction {
    Idle,
    Moving,
    Jumping,
    Dashing,
    DashAttack,
    JumpAttack,
    JumpHeavyAttack,
    LandingRecovery,
    LightAttack1,
    LightAttack2,
    ComboFinisher,
    HeavyAttack,
    HeavyAttack2,
    UltimateStartup,
    UltimateRush,
    UltimateVictim,
    GrabStartup,
    GrabHold,
    Grabbed,
    Throwing,
    SpecialCast,
    ItemPickup,
    ItemSwing,
    ItemThrow,
    ItemDrop,
    Guarding,
    GuardCounter,
    GuardStep,
    Hitstun,
    Knockdown,
    QuickStand,
    RecoveryRoll,
    GetUp,
    GuardBroken,
    RingOut,
    Respawning,
}

#[derive(Component)]
pub struct FighterActionState {
    pub action: FighterAction,
    pub elapsed: f32,
    pub hitbox_spawned: bool,
    pub queued_combo: bool,
    pub queued_technique: Option<TechniqueId>,
    pub queued_button: Option<TechniqueButton>,
    pub buffered_button: Option<TechniqueButton>,
    pub buffered_button_elapsed: f32,
    pub confirmed_hit: bool,
    pub technique_id: Option<TechniqueId>,
    pub cancel_window_open: bool,
    pub branch_window_open: bool,
    pub timeline_events_fired: u64,
    pub reaction_getup_ms: Option<u32>,
    pub reaction_recover_ms: Option<u32>,
    pub reaction_family: Option<ReactionFamilyId>,
    pub reaction_visual_side: f32,
    pub charge_elapsed: f32,
    pub charge_release_requested: bool,
}

impl FighterActionState {
    pub fn set_reaction_visual(&mut self, family: ReactionFamilyId, side: f32) {
        self.reaction_family = Some(family);
        self.reaction_visual_side = if side < 0.0 { -1.0 } else { 1.0 };
    }

    pub fn clear_reaction_visual(&mut self) {
        self.reaction_family = None;
        self.reaction_visual_side = 1.0;
    }
}

impl Default for FighterActionState {
    fn default() -> Self {
        Self {
            action: FighterAction::Idle,
            elapsed: 0.0,
            hitbox_spawned: false,
            queued_combo: false,
            queued_technique: None,
            queued_button: None,
            buffered_button: None,
            buffered_button_elapsed: 0.0,
            confirmed_hit: false,
            technique_id: None,
            cancel_window_open: false,
            branch_window_open: false,
            timeline_events_fired: 0,
            reaction_getup_ms: None,
            reaction_recover_ms: None,
            reaction_family: None,
            reaction_visual_side: 1.0,
            charge_elapsed: 0.0,
            charge_release_requested: false,
        }
    }
}

#[derive(Component)]
pub struct FighterVisualRoot;

#[derive(Component)]
pub struct FighterPoseRoot;

#[derive(Component)]
pub struct FighterSceneModel {
    pub fighter_id: usize,
}

#[derive(Component)]
pub struct FighterMarker;

#[derive(Component)]
pub struct FighterBody;

#[derive(Component)]
pub struct FighterHead;

#[derive(Component)]
pub struct FighterHand;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotBehaviorMode {
    TrainingDummy,
    Combatant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotMovementPlan {
    Approach,
    Circle,
    Backstep,
    Pressure,
    Retreat,
}

#[derive(Component)]
pub struct BotBrain {
    pub behavior: BotBehaviorMode,
    pub decision_timer: f32,
    pub movement_plan_timer: f32,
    pub dash_timer: f32,
    pub attack_timer: f32,
    pub strafe_sign: f32,
    pub movement_plan: BotMovementPlan,
}

#[derive(Component)]
pub struct Hitbox {
    pub owner: Entity,
    pub owner_id: usize,
    pub kind: AttackKind,
    pub payload_id: Option<AttackPayloadId>,
    pub attacker_character: Option<CharacterKind>,
    pub technique_id: Option<TechniqueId>,
    pub hit_effect: Option<HitImpactEffectId>,
    pub shape_id: AttackShapeId,
    pub reaction_family: ReactionFamilyId,
    pub damage_profile: DamageProfileId,
    pub element: DamageElement,
    pub attacker_equipment: Option<EquipmentKind>,
    pub attacker_style: Option<FighterStyleKind>,
    pub power: f32,
    pub str_scale: f32,
    pub damage: f32,
    pub knockback: f32,
    pub vertical_knockback: f32,
    pub guardable: bool,
    pub base_radius: f32,
    pub radius: f32,
    pub lifetime: f32,
    pub elapsed: f32,
    pub total_lifetime: f32,
    pub spawn_origin: Vec3,
    pub facing: Vec3,
    pub base_range: f32,
    pub range: f32,
    pub scales_with_owner_size: bool,
    pub vertical_offset_scale: f32,
    pub parented: bool,
    pub path: &'static [[f32; 3]],
    pub expires_on_owner_landing: bool,
    pub landing_linger: f32,
    pub landing_linger_started: bool,
    pub ground_path_end: bool,
    pub ground_path_clearance: f32,
    pub impact_cue: &'static str,
    pub hitstop_scale: f32,
    pub shake_scale: f32,
    pub feedback_priority_bonus: u8,
    pub already_hit: Vec<Entity>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttackKind {
    Light1,
    Light2,
    ComboFinisher,
    Heavy,
    Ultimate,
    Grab,
    Dash,
    Jump,
    GuardCounter,
    ItemSwing,
    ItemThrow,
    ItemBlast,
    Special,
}

impl AttackKind {
    pub fn is_heavy_feedback(self) -> bool {
        matches!(
            self,
            Self::ComboFinisher
                | Self::Heavy
                | Self::Ultimate
                | Self::Grab
                | Self::Dash
                | Self::GuardCounter
                | Self::ItemSwing
                | Self::ItemThrow
                | Self::ItemBlast
                | Self::Special
        )
    }

    #[allow(dead_code)]
    pub fn forces_knockdown(self) -> bool {
        matches!(
            self,
            Self::ComboFinisher | Self::Heavy | Self::Ultimate | Self::Grab
        )
    }

    #[allow(dead_code)]
    pub fn ignores_guard(self) -> bool {
        matches!(self, Self::Grab)
    }
}

#[derive(Component)]
pub struct TimerText;

#[derive(Component)]
pub struct PhaseText;

#[derive(Component)]
pub struct HealthBar {
    pub fighter_id: usize,
}

#[derive(Component)]
pub struct StaminaBar {
    pub fighter_id: usize,
}

#[derive(Component)]
pub struct TeamScoreText {
    pub team: usize,
}

#[derive(Component)]
pub struct AnnouncementText;

#[derive(Component)]
pub struct ResultPanel;

#[derive(Component)]
pub struct ResultText;

#[derive(Component)]
pub struct DebugOverlayPanel;

#[derive(Component)]
pub struct DebugOverlayText;
