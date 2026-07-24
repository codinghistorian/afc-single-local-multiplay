use arrayvec::ArrayVec;
use bevy::ecs::system::SystemParam;
use bevy::gltf::GltfAssetLabel;
use bevy::prelude::*;

use crate::arena::ground_support_for_arena_with_radius;
use crate::arena_defs::{ActiveArena, ArenaDefinition};
use crate::bee_skills::{
    BeePresentationEmitter, BeeSkillSpawnMode, BeeSkillTargetSnapshot, spawn_bee_skill,
};
use crate::body_collision::{FighterBodyBox, fighter_body_box, sphere_body_box_contact};
use crate::characters::{CharacterKind, CharacterMoveCatalog, CharacterMoveSlot, FighterCharacter};
use crate::chick_skills::{
    ActiveChickSkill, ActiveChickSkillSnapshot, CHICK_EGG_HALF_ASSET, ChickPresentationEmitter,
    spawn_chick_skill_with_presentation,
};
use crate::combat_sfx::{CombatSfxCue, CombatSfxKind, combat_sfx_kind_for_impact};
use crate::components::{
    AttackKind, Fighter, FighterAction, FighterActionState, FighterGrabState, FighterInput,
    FighterMotor, FighterStats, FighterUltimateState, Hitbox, SimPosition,
};
use crate::constants::*;
use crate::contact_arbitration::{
    ContactBuffer, ContactFlags, ContactOutcomeKind, ContactPhase, ContactRecord, ContactSourceId,
    ContactSourceKind, MAX_CONTACTS_PER_TICK,
};
use crate::determinism::{
    DEFAULT_F32_QUANTIZATION, FighterHitMask, FighterId, SimEntityId, SimEntityKind, quantize_f32,
};
use crate::ecs_identity::{
    SIM_ENTITY_POOL_CAPACITIES, SimulationIdentityAllocator, StableSimEntity, despawn_stable,
};
use crate::effects::{
    EffectAssets, FeedbackPackageId, FirePunchPalette, HitImpactEffectId,
    feedback_package_for_named_cue, feedback_package_for_timeline_cue,
    hit_impact_effect_for_feedback_package, spawn_element_hit_spark, spawn_feedback_package,
    spawn_guard_flash, spawn_hit_impact_effect, spawn_light_fire_punch,
};
use crate::equipment::{
    EquipmentKind, FighterEquipment, LoadoutContext, LoadoutModifierSource,
    loadout_attack_modifiers,
};
use crate::feel::CombatFeelTuning;
use crate::game_state::{Hitstop, MatchState, MatchTelemetry, RulePreset};
use crate::penguin_skills::{
    ActivePenguinSkill, PENGUIN_FISH_BONES_ASSET, PENGUIN_POPSICLE_ASSET, PENGUIN_SNOW_PILE_ASSET,
    PENGUIN_SNOWFLAKE_ASSET, PENGUIN_SPRING_ASSET, PenguinSkillKind, penguin_snowflake_swap_target,
    queue_penguin_snowflake_swap_presentation, spawn_penguin_skill_with_presentation,
};
use crate::reactions::{
    QueuedAftermath, ReactionFamilyId, ReactionKind, ReactionProfile, reaction_profile_for_family,
};
use crate::rollback::RollbackEventDiscard;
use crate::sim_event::{
    EventEmitError, MAX_SIM_EVENTS_PER_TICK, PresentationEventCursor, PresentationEventRouter,
    PresentedEventHistory, SIM_EVENT_HISTORY_TICKS, SimEvent, SimEventId, SimEventJournal,
    SimEventKind, SimEventSource, TickEventBuffer,
};
#[cfg(test)]
use crate::simulation::milliseconds_to_ticks_ceil;
use crate::simulation::{ElapsedTicks, SIM_DT_SECONDS, TickTimer, seconds_to_ticks_ceil};
use crate::styles::{FighterStyle, FighterStyleKind, style_tuning};
use crate::techniques::{
    AttackPayloadDef, AttackPayloadId, AttackShapeId, DamageCondition, DamageDefenseMode,
    DamageElement, DamageElementAffinity, DamageModifierDef, DamageProfileId, DamageSideEffectDef,
    DamageSideEffectId, DamageTargetStatus, DamageTerminalDef, DamageTerminalKind, FeedbackPhase,
    MoveTimelineEventKind, PENGUIN_SLOPE_TOTAL_FORWARD, PenguinSkillId, TechniqueId,
    active_technique_definition_in_catalog, attack_payload_definition, attack_shape_definition,
    charged_payload_for_elapsed, damage_profile_definition, payload_is_jump_fish,
    payload_is_jump_spike, payload_is_ultimate_bomb, payload_is_ultimate_catch,
    payload_is_ultimate_scratch, technique_slot_for_loadout,
};

pub const NEUTRAL_IMPACT_OWNER_ID: usize = usize::MAX;
const FOOD_FISH_ASSET: &str = "food/kenney_food_kit/fish.glb";
const FOOD_WHOLE_HAM_ASSET: &str = "food/kenney_food_kit/whole-ham.glb";
const FOOD_ROLLING_PIN_ASSET: &str = "food/kenney_food_kit/rollingPin.glb";
const FOOD_BURGER_ASSET: &str = "food/kenney_food_kit/burger.glb";
const FOOD_FRYING_PAN_ASSET: &str = "food/kenney_food_kit/frying-pan.glb";
const HOLIDAY_SLED_ASSET: &str = "holiday/kenney_holiday_kit/sled.glb";
const FOOD_FISH_SCALE: f32 = 3.0;
const PIG_AIR_MEAT_SLAM_METEOR_AIR_HITS: u8 = 2;
const PIG_AIR_MEAT_SLAM_METEOR_VERTICAL_KNOCKBACK: f32 = 18.0;
const PIG_AIR_MEAT_SLAM_METEOR_VERTICAL_SCALE: f32 = -0.82;
const PLAYER_DEFAULT_SUCCESS_HIT_CAMERA_SHAKE_MULTIPLIER: f32 = 1.08;
const PLAYER_DEFAULT_SUCCESS_HIT_CAMERA_SHAKE_BONUS: f32 = 0.01;
const PLAYER_DEFAULT_SUCCESS_HIT_CAMERA_SHAKE_MAX: f32 = 0.48;
const PLAYER_ULTIMATE_SUCCESS_HIT_CAMERA_SHAKE_MULTIPLIER: f32 = 2.35;
const PLAYER_ULTIMATE_SUCCESS_HIT_CAMERA_SHAKE_BONUS: f32 = 0.08;
const PLAYER_ULTIMATE_SUCCESS_HIT_CAMERA_SHAKE_MAX: f32 = 1.25;
const LIGHT_FIRE_PUNCH_CARD_ENABLED: bool = true;
const PENGUIN_SLOPE_ULTIMATE_ATTACKER_RECOIL_SPEED: f32 = 3.2;
const PENGUIN_SLOPE_ULTIMATE_ATTACKER_RECOIL_LIFT: f32 = 0.75;
const PENGUIN_SLOPE_ULTIMATE_EXIT_MOTION_EVENT_MASK: u64 = 1_u64 << 3;
const ACTIVE_PENGUIN_SKILL_SNAPSHOT_CAPACITY: usize =
    SIM_ENTITY_POOL_CAPACITIES[SimEntityKind::PenguinSkill.code() as usize] as usize;
const ACTIVE_CHICK_SKILL_SNAPSHOT_CAPACITY: usize =
    SIM_ENTITY_POOL_CAPACITIES[SimEntityKind::ChickSkill.code() as usize] as usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FixedCollectionOverflow {
    collection: &'static str,
    capacity: usize,
}

fn try_push_fixed<T, const N: usize>(
    values: &mut ArrayVec<T, N>,
    value: T,
    collection: &'static str,
) -> Result<(), FixedCollectionOverflow> {
    values.try_push(value).map_err(|_| FixedCollectionOverflow {
        collection,
        capacity: N,
    })
}

fn light_fire_punch_visual_side(technique_id: TechniqueId) -> Option<f32> {
    match technique_id {
        TechniqueId::CatLight1 | TechniqueId::PigLight1 => Some(-1.0),
        TechniqueId::CatLight2 | TechniqueId::PigLight2 => Some(1.0),
        _ => None,
    }
}

fn light_fire_punch_palette(character: CharacterKind) -> FirePunchPalette {
    match character {
        CharacterKind::Pig => FirePunchPalette::Blue,
        _ => FirePunchPalette::Red,
    }
}

#[derive(Resource, Default)]
pub struct HitEffects {
    pub shake: f32,
    pub last_cue: Option<FeedbackCue>,
    pub last_reaction: Option<ReactionCue>,
    combat_sfx_cues: Vec<CombatSfxCue>,
    dropped_combat_sfx_cues: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FeedbackCue {
    pub id: &'static str,
    pub source: ImpactSource,
    pub priority: u8,
    pub remaining: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReactionCue {
    pub kind: ReactionKind,
    pub source: ImpactSource,
    pub remaining: f32,
}

#[derive(Resource, Default)]
pub struct CombatVisualAssets {
    scenes: Vec<(&'static str, Handle<Scene>)>,
}

#[derive(Component)]
pub(crate) struct HitboxSceneVisual {
    pub(crate) spawn_tick: crate::simulation::SimTick,
    pub(crate) surface: AttackSurfacePresentation,
    elapsed: f32,
    lifetime: f32,
    path_duration: f32,
    range: f32,
}

impl HitboxSceneVisual {
    pub(crate) fn rewind_to_canonical_elapsed(&mut self, elapsed_seconds: f32) {
        self.elapsed = elapsed_seconds.max(0.0);
        self.lifetime =
            (hitbox_scene_visual_lifetime(self.surface.active_seconds) - self.elapsed).max(0.0);
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct HitboxSceneDef {
    asset_path: &'static str,
    scale: f32,
    yaw_offset: f32,
    pitch: f32,
    lift: f32,
    orientation: HitboxSceneOrientation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HitboxSceneOrientation {
    Facing,
    HamMeatOutwardHalfCircle,
    HamMeatOutwardVerticalArc,
}

const FEEDBACK_CUE_TTL: f32 = 0.82;
/// An authority may have no audio consumer and a render client may stall. Keep
/// audio intents bounded at the same hostile per-tick ceiling as simulation
/// events, rejecting newest presentation work without affecting gameplay.
pub const MAX_PENDING_COMBAT_SFX_CUES: usize = crate::sim_event::MAX_SIM_EVENTS_PER_TICK;
const MAX_TRACKED_ATTACK_SURFACES: usize = 32;

pub fn setup_combat_visual_assets(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.insert_resource(CombatPresentationDispatchHistory::default());
    commands.insert_resource(CombatVisualAssets {
        scenes: vec![
            (
                FOOD_FISH_ASSET,
                asset_server.load(GltfAssetLabel::Scene(0).from_asset(FOOD_FISH_ASSET)),
            ),
            (
                FOOD_WHOLE_HAM_ASSET,
                asset_server.load(GltfAssetLabel::Scene(0).from_asset(FOOD_WHOLE_HAM_ASSET)),
            ),
            (
                FOOD_ROLLING_PIN_ASSET,
                asset_server.load(GltfAssetLabel::Scene(0).from_asset(FOOD_ROLLING_PIN_ASSET)),
            ),
            (
                FOOD_BURGER_ASSET,
                asset_server.load(GltfAssetLabel::Scene(0).from_asset(FOOD_BURGER_ASSET)),
            ),
            (
                FOOD_FRYING_PAN_ASSET,
                asset_server.load(GltfAssetLabel::Scene(0).from_asset(FOOD_FRYING_PAN_ASSET)),
            ),
            (
                PENGUIN_FISH_BONES_ASSET,
                asset_server.load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_FISH_BONES_ASSET)),
            ),
            (
                PENGUIN_POPSICLE_ASSET,
                asset_server.load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_POPSICLE_ASSET)),
            ),
            (
                PENGUIN_SPRING_ASSET,
                asset_server.load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_SPRING_ASSET)),
            ),
            (
                HOLIDAY_SLED_ASSET,
                asset_server.load(GltfAssetLabel::Scene(0).from_asset(HOLIDAY_SLED_ASSET)),
            ),
            (
                PENGUIN_SNOW_PILE_ASSET,
                asset_server.load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_SNOW_PILE_ASSET)),
            ),
            (
                PENGUIN_SNOWFLAKE_ASSET,
                asset_server.load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_SNOWFLAKE_ASSET)),
            ),
            (
                CHICK_EGG_HALF_ASSET,
                asset_server.load(GltfAssetLabel::Scene(0).from_asset(CHICK_EGG_HALF_ASSET)),
            ),
        ],
    });
}

impl CombatVisualAssets {
    fn scene_for_path(&self, path: &'static str) -> Option<Handle<Scene>> {
        self.scenes
            .iter()
            .find(|(asset_path, _)| *asset_path == path)
            .map(|(_, scene)| scene.clone())
    }
}

impl HitEffects {
    pub fn push_feedback_cue(&mut self, id: &'static str, source: ImpactSource, priority: u8) {
        let replace = self
            .last_cue
            .map_or(true, |cue| cue.remaining <= 0.0 || priority >= cue.priority);
        if replace {
            self.last_cue = Some(FeedbackCue {
                id,
                source,
                priority,
                remaining: FEEDBACK_CUE_TTL,
            });
        }
    }

    pub fn cue_label(&self) -> Option<String> {
        self.last_cue
            .filter(|cue| cue.remaining > 0.0)
            .map(|cue| format!("{} {:?} p{}", cue.id, cue.source, cue.priority))
    }

    pub fn reaction_label(&self) -> Option<String> {
        self.last_reaction
            .filter(|reaction| reaction.remaining > 0.0)
            .map(|reaction| format!("{:?} {:?}", reaction.kind, reaction.source))
    }

    pub fn clear_feedback_cue(&mut self) {
        self.last_cue = None;
    }

    pub fn push_reaction_cue(&mut self, kind: ReactionKind, source: ImpactSource) {
        self.last_reaction = Some(ReactionCue {
            kind,
            source,
            remaining: FEEDBACK_CUE_TTL,
        });
    }

    pub fn clear_reaction_cue(&mut self) {
        self.last_reaction = None;
    }

    pub fn push_combat_sfx(&mut self, cue: CombatSfxCue) {
        if self.combat_sfx_cues.len() < MAX_PENDING_COMBAT_SFX_CUES {
            self.combat_sfx_cues.push(cue);
        } else {
            self.dropped_combat_sfx_cues = self.dropped_combat_sfx_cues.saturating_add(1);
        }
    }

    pub fn drain_combat_sfx_cues(&mut self) -> Vec<CombatSfxCue> {
        self.combat_sfx_cues.drain(..).collect()
    }

    #[cfg(test)]
    pub const fn dropped_combat_sfx_cues(&self) -> u64 {
        self.dropped_combat_sfx_cues
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImpactSource {
    FighterStrike,
    GrabThrow,
    ItemMelee,
    ItemThrow,
    ItemBlast,
    Projectile,
    Trap,
    Shockwave,
    Hazard,
    ItemUtility,
    RingOut,
    MatchFlow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImpactFeedbackIntensity {
    Light,
    Heavy,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImpactFeedbackProfile {
    pub cue: &'static str,
    pub heavy_spark: bool,
    pub spark_scale: f32,
    pub hitstop: f32,
    pub guard_hitstop: f32,
    pub shake: f32,
    pub guard_shake: f32,
    pub hit_hud_flash: f32,
    pub guard_hud_flash: f32,
    pub priority: u8,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ReactionFeedbackWeight {
    hitstop_scale: f32,
    spark_scale: f32,
    shake_add: f32,
    priority_bonus: u8,
}

fn reaction_feedback_weight(reaction: ReactionProfile) -> ReactionFeedbackWeight {
    match reaction.kind {
        ReactionKind::Hitstun => ReactionFeedbackWeight {
            hitstop_scale: 1.0,
            spark_scale: 1.0,
            shake_add: 0.0,
            priority_bonus: 0,
        },
        ReactionKind::Launch | ReactionKind::Tumble => ReactionFeedbackWeight {
            hitstop_scale: 1.12,
            spark_scale: 1.12,
            shake_add: 0.06,
            priority_bonus: 4,
        },
        ReactionKind::GroundBounce | ReactionKind::WallBounce => ReactionFeedbackWeight {
            hitstop_scale: 1.18,
            spark_scale: 1.18,
            shake_add: 0.1,
            priority_bonus: 6,
        },
        ReactionKind::HardKnockdown | ReactionKind::LandingRecovery => ReactionFeedbackWeight {
            hitstop_scale: 1.1,
            spark_scale: 1.08,
            shake_add: 0.08,
            priority_bonus: 5,
        },
    }
}

impl ImpactSource {
    pub fn from_attack_kind(kind: AttackKind) -> Self {
        match kind {
            AttackKind::Grab => Self::GrabThrow,
            AttackKind::ItemSwing => Self::ItemMelee,
            AttackKind::ItemThrow => Self::ItemThrow,
            AttackKind::ItemBlast => Self::ItemBlast,
            AttackKind::Special => Self::Projectile,
            _ => Self::FighterStrike,
        }
    }
}

#[derive(Clone, Copy)]
pub struct ImpactProfile {
    pub owner_id: usize,
    pub source: ImpactSource,
    pub payload_id: Option<AttackPayloadId>,
    pub attacker_character: Option<CharacterKind>,
    pub technique_id: Option<TechniqueId>,
    pub hit_effect: Option<HitImpactEffectId>,
    pub hit_effects_enabled: bool,
    pub shape_id: Option<AttackShapeId>,
    pub knockback_direction: Option<Vec3>,
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
    pub force_knockdown: bool,
    pub guardable: bool,
    pub guard_stamina_damage: f32,
    pub feedback: ImpactFeedbackProfile,
    pub reaction: ReactionProfile,
}

impl ImpactProfile {
    pub fn with_hit_effects_enabled(mut self, enabled: bool) -> Self {
        self.hit_effects_enabled = enabled;
        self
    }
}

/// Authoritative result plus the transient presentation work produced by one
/// impact.  The result is deliberately value-only so a headless authority can
/// resolve combat without constructing renderer/audio resources.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImpactOutcome {
    pub guarded: bool,
    pub committed_damage: f32,
    pub resolved_reaction: Option<ReactionFamilyId>,
    pub presentation: ImpactPresentation,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImpactPresentation {
    pub position: Vec3,
    pub direction: Vec3,
    pub source: ImpactSource,
    pub feedback_cue: &'static str,
    pub feedback_priority: u8,
    pub reaction: Option<ImpactReactionPresentation>,
    pub side_effect_cue: Option<&'static str>,
    pub side_effect_priority: u8,
    pub visual: ImpactVisualPresentation,
    pub combat_sfx: CombatSfxKind,
    pub combat_sfx_priority: u8,
    pub hud_flash: f32,
    pub reaction_visual_side: f32,
    pub camera_shake: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImpactReactionPresentation {
    pub kind: ReactionKind,
    pub cue: &'static str,
    pub priority: u8,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ImpactVisualPresentation {
    Guard {
        package: FeedbackPackageId,
    },
    Hit {
        element: DamageElement,
        heavy_spark: bool,
        spark_scale: f32,
        hit_effect: HitImpactEffectId,
        hit_effects_enabled: bool,
        include_skill_accent: bool,
    },
}

/// Render-only description of a canonical hitbox surface at its deterministic
/// spawn boundary. The canonical [`Hitbox`] remains the source of collision and
/// lifetime truth; this copy exists only so a render world can create the same
/// authored scene after prediction has crossed a thread/world boundary.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AttackSurfacePresentation {
    entity: SimEntityId,
    owner: FighterId,
    scene: Option<HitboxSceneDef>,
    center: Vec3,
    spawn_origin: Vec3,
    facing: Vec3,
    base_range: f32,
    range: f32,
    scales_with_owner_size: bool,
    vertical_offset_scale: f32,
    parented: bool,
    path: &'static [[f32; 3]],
    ground_path_end: bool,
    ground_path_clearance: f32,
    active_seconds: f32,
}

impl AttackSurfacePresentation {
    pub(crate) const fn entity(self) -> SimEntityId {
        self.entity
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LoadoutPresentationCue {
    fighter: FighterId,
    source: LoadoutModifierSource,
    cue: &'static str,
    hud_flash: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum TimelineFeedbackVisual {
    Package(FeedbackPackageId),
    FirePunch {
        side: f32,
        palette: FirePunchPalette,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TimelineFeedbackPresentation {
    fighter: FighterId,
    position: Vec3,
    direction: Vec3,
    cue: &'static str,
    priority: u8,
    shake: f32,
    hud_flash: f32,
    visual: Option<TimelineFeedbackVisual>,
}

/// Presentation-only work paired with a deterministic canonical event ID.
///
/// Stateful attack surfaces use `EntitySpawned`/`EntityDespawned`; action
/// timeline and loadout accents use `ActionStarted` as their compact semantic
/// identity. None of these authored assets or cue strings enter snapshots.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum CombatPresentationCueKind {
    AttackSurfaceSpawn(AttackSurfacePresentation),
    AttackSurfaceDespawn { entity: SimEntityId },
    Loadout(LoadoutPresentationCue),
    TimelineFeedback(TimelineFeedbackPresentation),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CombatPresentationCueIntent {
    pub(crate) event_id: SimEventId,
    pub(crate) kind: CombatPresentationCueKind,
}

/// Render-local data paired with one canonical impact event.
///
/// This value is deliberately absent from snapshots and the network protocol:
/// authored cue names and renderer-facing effect choices stay on the client,
/// while [`SimEventKind`] carries only compact semantic combat results.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CombatPresentationIntent {
    pub event_id: SimEventId,
    pub victim: FighterId,
    pub outcome: ImpactOutcome,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CombatPresentationIntentSlot {
    tick: crate::simulation::SimTick,
    len: u16,
    occupied: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CombatPresentationIntentMetrics {
    pub recorded: u64,
    pub replaced: u64,
    pub rejected: u64,
    pub discarded: u64,
    pub cue_recorded: u64,
    pub cue_replaced: u64,
    pub cue_rejected: u64,
    pub cue_discarded: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CombatPresentationDispatchMetrics {
    pub presented: u64,
    pub duplicates_suppressed: u64,
    pub capacity_rejections: u64,
}

/// Fixed-capacity, match-local deduplication for combat cues whose semantic
/// policy is otherwise replayable (`ActionStarted`). Runtime render worlds
/// install this resource during combat asset setup; online target preparation
/// resets it at every match boundary.
#[derive(Resource, Clone, Debug, Default)]
pub struct CombatPresentationDispatchHistory {
    presented: PresentedEventHistory,
    metrics: CombatPresentationDispatchMetrics,
}

impl CombatPresentationDispatchHistory {
    #[cfg(test)]
    pub const fn metrics(&self) -> CombatPresentationDispatchMetrics {
        self.metrics
    }

    fn reset(&mut self) {
        *self = Self::default();
    }

    fn mark_if_new(&mut self, event_id: SimEventId) -> bool {
        if self.presented.contains(event_id) {
            self.metrics.duplicates_suppressed =
                self.metrics.duplicates_suppressed.saturating_add(1);
            return false;
        }
        if self.presented.mark_if_new(event_id) {
            self.metrics.presented = self.metrics.presented.saturating_add(1);
            true
        } else {
            // PresentedEventHistory fails closed when its fixed table cannot
            // represent more correction churn for this tick.
            self.metrics.capacity_rejections = self.metrics.capacity_rejections.saturating_add(1);
            false
        }
    }
}

/// Fixed-capacity render-side journal keyed by deterministic [`SimEventId`].
///
/// Each tick owns one slot per possible simulation-event ordinal. This makes
/// lookup constant-time, prevents match traffic from growing memory, and lets a
/// rollback discard speculative intents without touching canonical state.
#[derive(Resource, Clone, Debug)]
pub struct CombatPresentationIntentJournal {
    slots: [CombatPresentationIntentSlot; SIM_EVENT_HISTORY_TICKS],
    intents: Box<[Option<CombatPresentationIntent>]>,
    cue_slots: [CombatPresentationIntentSlot; SIM_EVENT_HISTORY_TICKS],
    cues: Box<[Option<CombatPresentationCueIntent>]>,
    attack_surfaces: [Option<CombatPresentationCueIntent>; MAX_TRACKED_ATTACK_SURFACES],
    len: usize,
    cue_len: usize,
    metrics: CombatPresentationIntentMetrics,
}

impl Default for CombatPresentationIntentJournal {
    fn default() -> Self {
        Self {
            slots: [CombatPresentationIntentSlot::default(); SIM_EVENT_HISTORY_TICKS],
            intents: vec![None; SIM_EVENT_HISTORY_TICKS * MAX_SIM_EVENTS_PER_TICK]
                .into_boxed_slice(),
            cue_slots: [CombatPresentationIntentSlot::default(); SIM_EVENT_HISTORY_TICKS],
            cues: vec![None; SIM_EVENT_HISTORY_TICKS * MAX_SIM_EVENTS_PER_TICK].into_boxed_slice(),
            attack_surfaces: [None; MAX_TRACKED_ATTACK_SURFACES],
            len: 0,
            cue_len: 0,
            metrics: CombatPresentationIntentMetrics::default(),
        }
    }
}

impl CombatPresentationIntentJournal {
    const fn slot_index(tick: crate::simulation::SimTick) -> usize {
        tick.0 as usize % SIM_EVENT_HISTORY_TICKS
    }

    const fn slot_offset(slot: usize) -> usize {
        slot * MAX_SIM_EVENTS_PER_TICK
    }

    #[cfg(test)]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[cfg(test)]
    pub const fn cue_len(&self) -> usize {
        self.cue_len
    }

    #[cfg(test)]
    pub const fn capacity(&self) -> usize {
        SIM_EVENT_HISTORY_TICKS * MAX_SIM_EVENTS_PER_TICK
    }

    #[cfg(test)]
    pub const fn metrics(&self) -> CombatPresentationIntentMetrics {
        self.metrics
    }

    /// Records or deterministically replaces the render intent for an emitted
    /// event. Event-buffer ordinals are already bounded, but the check keeps
    /// this client-only API fail-closed under hostile direct input.
    pub fn record(&mut self, intent: CombatPresentationIntent) -> Result<(), EventEmitError> {
        let ordinal = usize::from(intent.event_id.ordinal);
        if ordinal >= MAX_SIM_EVENTS_PER_TICK {
            self.metrics.rejected = self.metrics.rejected.saturating_add(1);
            return Err(EventEmitError::CapacityExceeded {
                capacity: MAX_SIM_EVENTS_PER_TICK,
            });
        }

        let slot_index = Self::slot_index(intent.event_id.tick);
        let offset = Self::slot_offset(slot_index);
        let slot = &mut self.slots[slot_index];
        if slot.occupied && slot.tick != intent.event_id.tick {
            for entry in &mut self.intents[offset..offset + MAX_SIM_EVENTS_PER_TICK] {
                *entry = None;
            }
            self.len = self.len.saturating_sub(usize::from(slot.len));
        }
        if !slot.occupied || slot.tick != intent.event_id.tick {
            *slot = CombatPresentationIntentSlot {
                tick: intent.event_id.tick,
                len: 0,
                occupied: true,
            };
        }

        let entry = &mut self.intents[offset + ordinal];
        if entry.is_some() {
            self.metrics.replaced = self.metrics.replaced.saturating_add(1);
        } else {
            slot.len += 1;
            self.len += 1;
        }
        *entry = Some(intent);
        self.metrics.recorded = self.metrics.recorded.saturating_add(1);
        Ok(())
    }

    pub fn get(&self, event_id: SimEventId) -> Option<CombatPresentationIntent> {
        let ordinal = usize::from(event_id.ordinal);
        if ordinal >= MAX_SIM_EVENTS_PER_TICK {
            return None;
        }
        let slot_index = Self::slot_index(event_id.tick);
        let slot = self.slots[slot_index];
        if !slot.occupied || slot.tick != event_id.tick {
            return None;
        }
        self.intents[Self::slot_offset(slot_index) + ordinal]
            .filter(|intent| intent.event_id == event_id)
    }

    pub(crate) fn record_cue(
        &mut self,
        intent: CombatPresentationCueIntent,
    ) -> Result<(), EventEmitError> {
        let ordinal = usize::from(intent.event_id.ordinal);
        if ordinal >= MAX_SIM_EVENTS_PER_TICK {
            self.metrics.cue_rejected = self.metrics.cue_rejected.saturating_add(1);
            return Err(EventEmitError::CapacityExceeded {
                capacity: MAX_SIM_EVENTS_PER_TICK,
            });
        }

        let slot_index = Self::slot_index(intent.event_id.tick);
        let offset = Self::slot_offset(slot_index);
        let slot = &mut self.cue_slots[slot_index];
        if slot.occupied && slot.tick != intent.event_id.tick {
            for entry in &mut self.cues[offset..offset + MAX_SIM_EVENTS_PER_TICK] {
                *entry = None;
            }
            for surface in &mut self.attack_surfaces {
                if surface.is_some_and(|surface| surface.event_id.tick == slot.tick) {
                    *surface = None;
                }
            }
            self.cue_len = self.cue_len.saturating_sub(usize::from(slot.len));
        }
        if !slot.occupied || slot.tick != intent.event_id.tick {
            *slot = CombatPresentationIntentSlot {
                tick: intent.event_id.tick,
                len: 0,
                occupied: true,
            };
        }

        let entry = &mut self.cues[offset + ordinal];
        if entry.is_some() {
            self.metrics.cue_replaced = self.metrics.cue_replaced.saturating_add(1);
        } else {
            slot.len += 1;
            self.cue_len += 1;
        }
        *entry = Some(intent);
        if let CombatPresentationCueKind::AttackSurfaceSpawn(surface) = intent.kind
            && surface.entity.kind() == SimEntityKind::Hitbox
            && let Ok(index) = usize::try_from(surface.entity.index())
            && let Some(tracked) = self.attack_surfaces.get_mut(index)
        {
            *tracked = Some(intent);
        }
        self.metrics.cue_recorded = self.metrics.cue_recorded.saturating_add(1);
        Ok(())
    }

    pub(crate) fn cue(&self, event_id: SimEventId) -> Option<CombatPresentationCueIntent> {
        let ordinal = usize::from(event_id.ordinal);
        if ordinal >= MAX_SIM_EVENTS_PER_TICK {
            return None;
        }
        let slot_index = Self::slot_index(event_id.tick);
        let slot = self.cue_slots[slot_index];
        if !slot.occupied || slot.tick != event_id.tick {
            return None;
        }
        self.cues[Self::slot_offset(slot_index) + ordinal]
            .filter(|intent| intent.event_id == event_id)
    }

    fn attack_surface(&self, entity: SimEntityId) -> Option<CombatPresentationCueIntent> {
        if entity.kind() != SimEntityKind::Hitbox {
            return None;
        }
        let index = usize::try_from(entity.index()).ok()?;
        self.attack_surfaces
            .get(index)
            .copied()
            .flatten()
            .filter(|intent| {
                matches!(
                    intent.kind,
                    CombatPresentationCueKind::AttackSurfaceSpawn(surface)
                        if surface.entity == entity
                )
            })
    }

    pub fn discard_after(&mut self, retained_through: crate::simulation::SimTick) {
        for slot_index in 0..SIM_EVENT_HISTORY_TICKS {
            let slot = self.slots[slot_index];
            if !slot.occupied || slot.tick <= retained_through {
                continue;
            }
            let offset = Self::slot_offset(slot_index);
            for entry in &mut self.intents[offset..offset + MAX_SIM_EVENTS_PER_TICK] {
                *entry = None;
            }
            self.slots[slot_index] = CombatPresentationIntentSlot::default();
            self.len = self.len.saturating_sub(usize::from(slot.len));
            self.metrics.discarded = self.metrics.discarded.saturating_add(u64::from(slot.len));
        }
        for slot_index in 0..SIM_EVENT_HISTORY_TICKS {
            let slot = self.cue_slots[slot_index];
            if !slot.occupied || slot.tick <= retained_through {
                continue;
            }
            let offset = Self::slot_offset(slot_index);
            for entry in &mut self.cues[offset..offset + MAX_SIM_EVENTS_PER_TICK] {
                *entry = None;
            }
            self.cue_slots[slot_index] = CombatPresentationIntentSlot::default();
            self.cue_len = self.cue_len.saturating_sub(usize::from(slot.len));
            self.metrics.cue_discarded = self
                .metrics
                .cue_discarded
                .saturating_add(u64::from(slot.len));
        }
        for surface in &mut self.attack_surfaces {
            if surface.is_some_and(|surface| surface.event_id.tick > retained_through) {
                *surface = None;
            }
        }
    }
}

impl RollbackEventDiscard for CombatPresentationIntentJournal {
    fn discard_after(&mut self, retained_through: crate::simulation::SimTick) {
        Self::discard_after(self, retained_through);
    }
}

/// One deterministic emission boundary for canonical hitbox lifecycle and its
/// optional render-only sidecar. Authorities emit the semantic event even when
/// the presentation journal is intentionally absent.
#[derive(SystemParam)]
pub(crate) struct CombatPresentationEmitter<'w> {
    sim_events: ResMut<'w, TickEventBuffer>,
    intents: Option<ResMut<'w, CombatPresentationIntentJournal>>,
}

impl CombatPresentationEmitter<'_> {
    fn record(
        &mut self,
        source: SimEventSource,
        event: SimEventKind,
        kind: CombatPresentationCueKind,
    ) {
        let Ok(event_id) = self.sim_events.emit(source, event) else {
            return;
        };
        if let Some(intents) = self.intents.as_deref_mut() {
            let _ = intents.record_cue(CombatPresentationCueIntent { event_id, kind });
        }
    }

    fn attack_surface_spawned(&mut self, presentation: AttackSurfacePresentation) {
        self.record(
            SimEventSource::Entity(presentation.entity),
            SimEventKind::EntitySpawned {
                entity: presentation.entity,
            },
            CombatPresentationCueKind::AttackSurfaceSpawn(presentation),
        );
    }

    fn attack_surface_despawned(&mut self, entity: SimEntityId) {
        self.record(
            SimEventSource::Entity(entity),
            SimEventKind::EntityDespawned { entity },
            CombatPresentationCueKind::AttackSurfaceDespawn { entity },
        );
    }

    fn loadout_cue(
        &mut self,
        fighter: FighterId,
        technique: TechniqueId,
        applied: AppliedLoadoutAttackModifier,
    ) {
        self.record(
            SimEventSource::Fighter(fighter),
            SimEventKind::ActionStarted {
                fighter,
                action_id: crate::live_snapshot::technique_code(technique),
            },
            CombatPresentationCueKind::Loadout(LoadoutPresentationCue {
                fighter,
                source: applied.source,
                cue: applied.cue,
                hud_flash: 0.22,
            }),
        );
    }

    fn timeline_feedback(
        &mut self,
        fighter: FighterId,
        technique: TechniqueId,
        presentation: TimelineFeedbackPresentation,
    ) {
        self.record(
            SimEventSource::Fighter(fighter),
            SimEventKind::ActionStarted {
                fighter,
                action_id: crate::live_snapshot::technique_code(technique),
            },
            CombatPresentationCueKind::TimelineFeedback(presentation),
        );
    }
}

fn emit_attack_surface_despawn(
    sim_events: &mut TickEventBuffer,
    intents: Option<&mut CombatPresentationIntentJournal>,
    entity: SimEntityId,
) {
    let Ok(event_id) = sim_events.emit(
        SimEventSource::Entity(entity),
        SimEventKind::EntityDespawned { entity },
    ) else {
        return;
    };
    if let Some(intents) = intents {
        let _ = intents.record_cue(CombatPresentationCueIntent {
            event_id,
            kind: CombatPresentationCueKind::AttackSurfaceDespawn { entity },
        });
    }
}

pub fn impact_feedback_profile(
    source: ImpactSource,
    intensity: ImpactFeedbackIntensity,
) -> ImpactFeedbackProfile {
    match (source, intensity) {
        (ImpactSource::FighterStrike, ImpactFeedbackIntensity::Light) => ImpactFeedbackProfile {
            cue: "strike_light",
            heavy_spark: false,
            spark_scale: 1.0,
            hitstop: 0.055,
            guard_hitstop: 0.04,
            shake: 0.22,
            guard_shake: 0.12,
            hit_hud_flash: 0.22,
            guard_hud_flash: 0.15,
            priority: 20,
        },
        (ImpactSource::FighterStrike, ImpactFeedbackIntensity::Heavy) => ImpactFeedbackProfile {
            cue: "strike_heavy",
            heavy_spark: true,
            spark_scale: 1.16,
            hitstop: 0.085,
            guard_hitstop: 0.045,
            shake: 0.38,
            guard_shake: 0.15,
            hit_hud_flash: 0.28,
            guard_hud_flash: 0.18,
            priority: 40,
        },
        (ImpactSource::GrabThrow, ImpactFeedbackIntensity::Light) => ImpactFeedbackProfile {
            cue: "throw_quick",
            heavy_spark: true,
            spark_scale: 0.98,
            hitstop: 0.06,
            guard_hitstop: 0.0,
            shake: 0.24,
            guard_shake: 0.0,
            hit_hud_flash: 0.24,
            guard_hud_flash: 0.0,
            priority: 35,
        },
        (ImpactSource::GrabThrow, ImpactFeedbackIntensity::Heavy) => ImpactFeedbackProfile {
            cue: "throw_heavy",
            heavy_spark: true,
            spark_scale: 1.24,
            hitstop: 0.085,
            guard_hitstop: 0.0,
            shake: 0.34,
            guard_shake: 0.0,
            hit_hud_flash: 0.3,
            guard_hud_flash: 0.0,
            priority: 50,
        },
        (ImpactSource::ItemMelee, ImpactFeedbackIntensity::Light) => ImpactFeedbackProfile {
            cue: "item_melee_light",
            heavy_spark: false,
            spark_scale: 1.08,
            hitstop: 0.055,
            guard_hitstop: 0.04,
            shake: 0.22,
            guard_shake: 0.12,
            hit_hud_flash: 0.22,
            guard_hud_flash: 0.15,
            priority: 25,
        },
        (ImpactSource::ItemMelee, ImpactFeedbackIntensity::Heavy) => ImpactFeedbackProfile {
            cue: "item_melee_heavy",
            heavy_spark: true,
            spark_scale: 1.22,
            hitstop: 0.085,
            guard_hitstop: 0.045,
            shake: 0.38,
            guard_shake: 0.16,
            hit_hud_flash: 0.28,
            guard_hud_flash: 0.18,
            priority: 45,
        },
        (ImpactSource::ItemThrow, ImpactFeedbackIntensity::Light) => ImpactFeedbackProfile {
            cue: "item_throw_light",
            heavy_spark: false,
            spark_scale: 1.03,
            hitstop: 0.055,
            guard_hitstop: 0.04,
            shake: 0.2,
            guard_shake: 0.12,
            hit_hud_flash: 0.22,
            guard_hud_flash: 0.15,
            priority: 28,
        },
        (ImpactSource::ItemThrow, ImpactFeedbackIntensity::Heavy) => ImpactFeedbackProfile {
            cue: "item_throw_heavy",
            heavy_spark: true,
            spark_scale: 1.28,
            hitstop: 0.075,
            guard_hitstop: 0.045,
            shake: 0.32,
            guard_shake: 0.16,
            hit_hud_flash: 0.28,
            guard_hud_flash: 0.18,
            priority: 44,
        },
        (ImpactSource::ItemBlast, _) => ImpactFeedbackProfile {
            cue: "item_blast",
            heavy_spark: true,
            spark_scale: 1.35,
            hitstop: 0.08,
            guard_hitstop: 0.045,
            shake: 0.32,
            guard_shake: 0.16,
            hit_hud_flash: 0.3,
            guard_hud_flash: 0.18,
            priority: 55,
        },
        (ImpactSource::Projectile, ImpactFeedbackIntensity::Light) => ImpactFeedbackProfile {
            cue: "projectile_ping",
            heavy_spark: false,
            spark_scale: 0.9,
            hitstop: 0.045,
            guard_hitstop: 0.035,
            shake: 0.14,
            guard_shake: 0.1,
            hit_hud_flash: 0.2,
            guard_hud_flash: 0.14,
            priority: 22,
        },
        (ImpactSource::Projectile, ImpactFeedbackIntensity::Heavy) => ImpactFeedbackProfile {
            cue: "projectile_burst",
            heavy_spark: true,
            spark_scale: 1.12,
            hitstop: 0.06,
            guard_hitstop: 0.04,
            shake: 0.22,
            guard_shake: 0.12,
            hit_hud_flash: 0.24,
            guard_hud_flash: 0.16,
            priority: 34,
        },
        (ImpactSource::Trap, _) => ImpactFeedbackProfile {
            cue: "trap_snap",
            heavy_spark: true,
            spark_scale: 1.16,
            hitstop: 0.07,
            guard_hitstop: 0.035,
            shake: 0.28,
            guard_shake: 0.1,
            hit_hud_flash: 0.27,
            guard_hud_flash: 0.16,
            priority: 42,
        },
        (ImpactSource::Shockwave, _) => ImpactFeedbackProfile {
            cue: "shockwave_push",
            heavy_spark: true,
            spark_scale: 1.1,
            hitstop: 0.07,
            guard_hitstop: 0.04,
            shake: 0.28,
            guard_shake: 0.12,
            hit_hud_flash: 0.26,
            guard_hud_flash: 0.16,
            priority: 42,
        },
        (ImpactSource::Hazard, ImpactFeedbackIntensity::Light) => ImpactFeedbackProfile {
            cue: "hazard_tick",
            heavy_spark: false,
            spark_scale: 0.86,
            hitstop: 0.045,
            guard_hitstop: 0.035,
            shake: 0.16,
            guard_shake: 0.09,
            hit_hud_flash: 0.2,
            guard_hud_flash: 0.14,
            priority: 24,
        },
        (ImpactSource::Hazard, ImpactFeedbackIntensity::Heavy) => ImpactFeedbackProfile {
            cue: "hazard_launch",
            heavy_spark: true,
            spark_scale: 1.12,
            hitstop: 0.065,
            guard_hitstop: 0.04,
            shake: 0.26,
            guard_shake: 0.12,
            hit_hud_flash: 0.25,
            guard_hud_flash: 0.16,
            priority: 38,
        },
        (ImpactSource::ItemUtility, _) => ImpactFeedbackProfile {
            cue: "item_utility",
            heavy_spark: false,
            spark_scale: 0.75,
            hitstop: 0.0,
            guard_hitstop: 0.0,
            shake: 0.0,
            guard_shake: 0.0,
            hit_hud_flash: 0.18,
            guard_hud_flash: 0.0,
            priority: 18,
        },
        (ImpactSource::RingOut, _) => ImpactFeedbackProfile {
            cue: "ringout_burst",
            heavy_spark: true,
            spark_scale: 1.45,
            hitstop: 0.0,
            guard_hitstop: 0.0,
            shake: 0.44,
            guard_shake: 0.0,
            hit_hud_flash: 0.34,
            guard_hud_flash: 0.0,
            priority: 80,
        },
        (ImpactSource::MatchFlow, _) => ImpactFeedbackProfile {
            cue: "match_flow",
            heavy_spark: false,
            spark_scale: 0.7,
            hitstop: 0.0,
            guard_hitstop: 0.0,
            shake: 0.0,
            guard_shake: 0.0,
            hit_hud_flash: 0.0,
            guard_hud_flash: 0.0,
            priority: 12,
        },
    }
}

#[allow(clippy::too_many_arguments)]
pub fn impact_profile(
    owner_id: usize,
    source: ImpactSource,
    damage: f32,
    knockback: f32,
    vertical_knockback: f32,
    force_knockdown: bool,
    guardable: bool,
    guard_stamina_damage: f32,
    intensity: ImpactFeedbackIntensity,
    reaction_family: ReactionFamilyId,
) -> ImpactProfile {
    ImpactProfile {
        owner_id,
        source,
        payload_id: None,
        attacker_character: None,
        technique_id: None,
        hit_effect: None,
        hit_effects_enabled: true,
        shape_id: None,
        knockback_direction: None,
        reaction_family,
        damage_profile: DamageProfileId::Direct,
        element: DamageElement::Neutral,
        attacker_equipment: None,
        attacker_style: None,
        power: damage,
        str_scale: 1.0,
        damage,
        knockback,
        vertical_knockback: scaled_vertical_knockback(vertical_knockback),
        force_knockdown,
        guardable,
        guard_stamina_damage,
        feedback: impact_feedback_profile(source, intensity),
        reaction: reaction_profile_for_family(reaction_family),
    }
}

fn scaled_vertical_knockback(vertical_knockback: f32) -> f32 {
    vertical_knockback * VERTICAL_KNOCKBACK_SCALE
}

fn hitbox_scene_for_payload(payload_id: AttackPayloadId) -> Option<HitboxSceneDef> {
    match payload_id {
        AttackPayloadId::HeavyStep
        | AttackPayloadId::KiriageBeat1
        | AttackPayloadId::KiriageBeat2 => Some(HitboxSceneDef {
            asset_path: FOOD_FISH_ASSET,
            scale: FOOD_FISH_SCALE,
            yaw_offset: std::f32::consts::FRAC_PI_2,
            pitch: -0.28,
            lift: 0.0,
            orientation: HitboxSceneOrientation::Facing,
        }),
        AttackPayloadId::PigRollingPinStep => Some(HitboxSceneDef {
            asset_path: FOOD_ROLLING_PIN_ASSET,
            scale: 2.5,
            yaw_offset: std::f32::consts::FRAC_PI_2,
            pitch: -0.08,
            lift: 0.05,
            orientation: HitboxSceneOrientation::Facing,
        }),
        AttackPayloadId::PigHamLauncher => Some(HitboxSceneDef {
            asset_path: FOOD_WHOLE_HAM_ASSET,
            scale: 3.0,
            yaw_offset: std::f32::consts::FRAC_PI_2,
            pitch: -0.22,
            lift: 0.08,
            orientation: HitboxSceneOrientation::Facing,
        }),
        AttackPayloadId::PigHamSlam => Some(HitboxSceneDef {
            asset_path: FOOD_WHOLE_HAM_ASSET,
            scale: 3.15,
            yaw_offset: std::f32::consts::FRAC_PI_2,
            pitch: -0.72,
            lift: -0.04,
            orientation: HitboxSceneOrientation::Facing,
        }),
        AttackPayloadId::PigHamSwingTap
        | AttackPayloadId::PigHamSwingPartial
        | AttackPayloadId::PigHamSwingFull => Some(HitboxSceneDef {
            asset_path: FOOD_WHOLE_HAM_ASSET,
            scale: 2.85,
            yaw_offset: 0.0,
            pitch: -0.12,
            lift: 0.03,
            orientation: HitboxSceneOrientation::HamMeatOutwardHalfCircle,
        }),
        AttackPayloadId::PigHamLob
        | AttackPayloadId::PigUltimateScratchLight
        | AttackPayloadId::PigUltimateScratchHeavy => Some(HitboxSceneDef {
            asset_path: FOOD_WHOLE_HAM_ASSET,
            scale: 2.85,
            yaw_offset: std::f32::consts::FRAC_PI_2,
            pitch: -0.12,
            lift: 0.03,
            orientation: HitboxSceneOrientation::Facing,
        }),
        AttackPayloadId::PigUltimateCatch => Some(HitboxSceneDef {
            asset_path: FOOD_WHOLE_HAM_ASSET,
            scale: 3.05,
            yaw_offset: std::f32::consts::FRAC_PI_2,
            pitch: -0.22,
            lift: 0.05,
            orientation: HitboxSceneOrientation::Facing,
        }),
        AttackPayloadId::PigAirMeatSlam => Some(HitboxSceneDef {
            asset_path: FOOD_WHOLE_HAM_ASSET,
            scale: 3.25,
            yaw_offset: 0.0,
            pitch: 0.0,
            lift: 0.0,
            orientation: HitboxSceneOrientation::HamMeatOutwardVerticalArc,
        }),
        AttackPayloadId::PigUltimateBomb => Some(HitboxSceneDef {
            asset_path: FOOD_BURGER_ASSET,
            scale: 3.25,
            yaw_offset: 0.0,
            pitch: -0.1,
            lift: 0.16,
            orientation: HitboxSceneOrientation::Facing,
        }),
        AttackPayloadId::PenguinFishSlap1
        | AttackPayloadId::PenguinFishSlap2
        | AttackPayloadId::PenguinUltimateScratchLight => Some(HitboxSceneDef {
            asset_path: FOOD_FISH_ASSET,
            scale: 2.55,
            yaw_offset: std::f32::consts::FRAC_PI_2,
            pitch: -0.18,
            lift: 0.0,
            orientation: HitboxSceneOrientation::Facing,
        }),
        AttackPayloadId::PenguinFrozenFishDive => Some(HitboxSceneDef {
            asset_path: PENGUIN_FISH_BONES_ASSET,
            scale: 2.8,
            yaw_offset: std::f32::consts::FRAC_PI_2,
            pitch: -0.36,
            lift: 0.02,
            orientation: HitboxSceneOrientation::Facing,
        }),
        AttackPayloadId::PenguinPanBonk
        | AttackPayloadId::PenguinUltimateCatch
        | AttackPayloadId::PenguinUltimateScratchHeavy => Some(HitboxSceneDef {
            asset_path: FOOD_FRYING_PAN_ASSET,
            scale: 2.7,
            yaw_offset: std::f32::consts::FRAC_PI_2,
            pitch: -0.18,
            lift: 0.05,
            orientation: HitboxSceneOrientation::Facing,
        }),
        AttackPayloadId::PenguinSledScoop
        | AttackPayloadId::PenguinIceSlide
        | AttackPayloadId::PenguinBellySlide => Some(HitboxSceneDef {
            asset_path: HOLIDAY_SLED_ASSET,
            scale: 2.45,
            yaw_offset: 0.0,
            pitch: -0.04,
            lift: -0.02,
            orientation: HitboxSceneOrientation::Facing,
        }),
        AttackPayloadId::PenguinPopsiclePeck => Some(HitboxSceneDef {
            asset_path: PENGUIN_SPRING_ASSET,
            scale: 2.05,
            yaw_offset: 0.0,
            pitch: -0.04,
            lift: 0.04,
            orientation: HitboxSceneOrientation::Facing,
        }),
        AttackPayloadId::PenguinUltimateBomb => Some(HitboxSceneDef {
            asset_path: PENGUIN_SNOWFLAKE_ASSET,
            scale: 2.75,
            yaw_offset: 0.0,
            pitch: 0.0,
            lift: 0.18,
            orientation: HitboxSceneOrientation::Facing,
        }),
        AttackPayloadId::ChickShellScoot | AttackPayloadId::ChickShellScramble => {
            Some(HitboxSceneDef {
                asset_path: CHICK_EGG_HALF_ASSET,
                scale: 2.6,
                yaw_offset: std::f32::consts::FRAC_PI_2,
                pitch: -0.16,
                lift: 0.02,
                orientation: HitboxSceneOrientation::Facing,
            })
        }
        payload_id if payload_is_jump_fish(payload_id) => Some(HitboxSceneDef {
            asset_path: FOOD_FISH_ASSET,
            scale: FOOD_FISH_SCALE,
            yaw_offset: std::f32::consts::FRAC_PI_2,
            pitch: -0.16,
            lift: 0.0,
            orientation: HitboxSceneOrientation::Facing,
        }),
        _ => None,
    }
}

fn hitbox_transform(center: Vec3, facing: Vec3) -> Transform {
    let yaw = facing.x.atan2(facing.z);
    Transform::from_translation(center).with_rotation(Quat::from_rotation_y(yaw))
}

fn hitbox_scene_transform(def: HitboxSceneDef) -> Transform {
    Transform::from_xyz(0.0, def.lift, 0.0)
        .with_rotation(Quat::from_rotation_y(def.yaw_offset) * Quat::from_rotation_x(def.pitch))
        .with_scale(Vec3::splat(def.scale))
}

fn hitbox_scene_world_transform(
    center: Vec3,
    owner_base: Vec3,
    facing: Vec3,
    range: f32,
    def: HitboxSceneDef,
) -> Transform {
    let mut transform = hitbox_scene_base_transform(center, owner_base, facing, range, def);
    let local = hitbox_scene_transform(def);
    transform.translation += transform.rotation * local.translation;
    transform.rotation *= local.rotation;
    transform.scale *= local.scale;
    transform
}

fn hitbox_scene_base_transform(
    center: Vec3,
    owner_base: Vec3,
    facing: Vec3,
    range: f32,
    def: HitboxSceneDef,
) -> Transform {
    if def.orientation == HitboxSceneOrientation::HamMeatOutwardVerticalArc {
        return Transform::from_translation(center).with_rotation(
            hitbox_scene_vertical_arc_rotation(center, owner_base, facing),
        );
    }

    let visual_facing = hitbox_scene_visual_facing(center, owner_base, facing, range, def);
    hitbox_transform(center, visual_facing)
}

fn hitbox_scene_vertical_arc_rotation(center: Vec3, owner_base: Vec3, facing: Vec3) -> Quat {
    let forward = facing.normalize_or_zero();
    let pivot = owner_base + Vec3::Y * (FIGHTER_HEIGHT * 0.82) - forward * 0.1;
    let radial = center - pivot;
    if radial.length_squared() > 0.0001 {
        Quat::from_rotation_arc(Vec3::Z, radial.normalize())
    } else {
        hitbox_transform(center, facing).rotation
    }
}

fn hitbox_scene_visual_facing(
    center: Vec3,
    owner_base: Vec3,
    facing: Vec3,
    range: f32,
    def: HitboxSceneDef,
) -> Vec3 {
    match def.orientation {
        HitboxSceneOrientation::Facing => facing,
        HitboxSceneOrientation::HamMeatOutwardHalfCircle => {
            let forward = facing.normalize_or_zero();
            let arc_center = owner_base + forward * (range - 0.2);
            let radial = Vec3::new(center.x - arc_center.x, 0.0, center.z - arc_center.z);
            if radial.length_squared() > 0.0001 {
                radial.normalize()
            } else {
                facing
            }
        }
        HitboxSceneOrientation::HamMeatOutwardVerticalArc => facing,
    }
}

fn hitbox_scene_visual_lifetime(active: f32) -> f32 {
    active.max(0.65)
}

fn hitbox_scene_path_duration(active: f32) -> f32 {
    active.max(0.001)
}

fn hitbox_landing_linger(payload_id: AttackPayloadId) -> Option<TickTimer> {
    if payload_is_jump_spike(payload_id) || matches!(payload_id, AttackPayloadId::PigAirMeatSlam) {
        Some(TickTimer::from_seconds_ceil(
            JUMP_ATTACK_LANDING_HITBOX_LINGER,
        ))
    } else {
        None
    }
}

fn hitbox_ground_path_clearance(payload_id: AttackPayloadId) -> Option<f32> {
    if payload_is_jump_fish(payload_id) || matches!(payload_id, AttackPayloadId::PigAirMeatSlam) {
        Some(JUMP_HEAVY_FISH_GROUND_CLEARANCE)
    } else {
        None
    }
}

fn start_hitbox_landing_linger(hitbox: &mut Hitbox, owner_grounded: bool) {
    if hitbox.expires_on_owner_landing && !hitbox.landing_linger_started && owner_grounded {
        hitbox.landing_linger_started = true;
        hitbox.lifetime = hitbox.landing_linger;
    }
}

fn spawn_canonical_hitbox(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    hitbox: Hitbox,
    position: SimPosition,
) -> Option<(Entity, SimEntityId)> {
    let entity = commands.spawn_empty().id();
    let stable = match identities.try_allocate(SimEntityKind::Hitbox, entity) {
        Ok(stable) => stable,
        Err(_) => {
            commands.entity(entity).despawn();
            return None;
        }
    };
    commands.entity(entity).insert((stable, hitbox, position));
    Some((entity, stable.id()))
}

pub fn spawn_attack_hitboxes(
    mut commands: Commands,
    mut identities: ResMut<SimulationIdentityAllocator>,
    mut skill_presentations: ParamSet<(
        BeePresentationEmitter,
        ChickPresentationEmitter,
        CombatPresentationEmitter,
    )>,
    hitstop: Res<Hitstop>,
    state: Res<MatchState>,
    active_arena: Res<ActiveArena>,
    feel: Res<CombatFeelTuning>,
    character_catalog: Res<CharacterMoveCatalog>,
    active_penguin_skills: Query<
        (&StableSimEntity, &ActivePenguinSkill, &SimPosition),
        Without<Fighter>,
    >,
    active_chick_skills: Query<
        (&StableSimEntity, &ActiveChickSkill, &SimPosition),
        Without<Fighter>,
    >,
    mut fighters: ParamSet<(
        Query<(&Fighter, &SimPosition), With<Fighter>>,
        Query<
            (
                &Fighter,
                &FighterCharacter,
                &FighterStyle,
                &FighterInput,
                &mut FighterMotor,
                &mut FighterStats,
                &mut FighterEquipment,
                &mut FighterActionState,
                &mut SimPosition,
            ),
            With<Fighter>,
        >,
    )>,
) {
    if hitstop.active() {
        return;
    }

    let mut skill_targets = ArrayVec::<_, { FighterId::ALL.len() }>::new();
    {
        let target_fighters = fighters.p0();
        for fighter_id in FighterId::ALL {
            let Some((_, transform)) = target_fighters
                .iter()
                .find(|(fighter, _)| fighter.id == fighter_id.index())
            else {
                continue;
            };
            if let Err(error) = try_push_fixed(
                &mut skill_targets,
                BeeSkillTargetSnapshot {
                    fighter_id,
                    position: transform.translation,
                },
                "fighter skill targets",
            ) {
                error!(?error, "attack snapshot collection failed closed");
                return;
            }
        }
    }
    let mut active_penguin_skill_snapshots = ArrayVec::<
        (SimEntityId, PenguinSkillKind, FighterId, TickTimer, Vec3),
        ACTIVE_PENGUIN_SKILL_SNAPSHOT_CAPACITY,
    >::new();
    for (stable, skill, skill_transform) in active_penguin_skills.iter() {
        let id = stable.id();
        if id.kind() != SimEntityKind::PenguinSkill
            || id.index() as usize >= ACTIVE_PENGUIN_SKILL_SNAPSHOT_CAPACITY
            || active_penguin_skill_snapshots
                .iter()
                .any(|(existing, ..)| existing.index() == id.index())
        {
            error!(
                ?id,
                "invalid Penguin skill identity; attack snapshot collection failed closed"
            );
            return;
        }
        if let Err(error) = try_push_fixed(
            &mut active_penguin_skill_snapshots,
            (
                id,
                skill.kind,
                skill.owner,
                skill.lifetime,
                skill_transform.translation,
            ),
            "active Penguin skills",
        ) {
            error!(?error, "attack snapshot collection failed closed");
            return;
        }
    }
    active_penguin_skill_snapshots.sort_unstable_by_key(|(id, ..)| *id);
    let mut active_chick_skill_snapshots =
        ArrayVec::<_, ACTIVE_CHICK_SKILL_SNAPSHOT_CAPACITY>::new();
    for (stable, skill, skill_transform) in active_chick_skills.iter() {
        let id = stable.id();
        if id.kind() != SimEntityKind::ChickSkill
            || id.index() as usize >= ACTIVE_CHICK_SKILL_SNAPSHOT_CAPACITY
            || active_chick_skill_snapshots
                .iter()
                .any(|existing: &ActiveChickSkillSnapshot| existing.id.index() == id.index())
        {
            error!(
                ?id,
                "invalid Chick skill identity; attack snapshot collection failed closed"
            );
            return;
        }
        if let Err(error) = try_push_fixed(
            &mut active_chick_skill_snapshots,
            ActiveChickSkillSnapshot {
                id: stable.id(),
                owner: skill.owner,
                kind: skill.kind,
                position: skill_transform.translation,
            },
            "active Chick skills",
        ) {
            error!(?error, "attack snapshot collection failed closed");
            return;
        }
    }
    active_chick_skill_snapshots.sort_unstable_by_key(|skill| skill.id);

    let mut fighter_query = fighters.p1();
    for stable_owner in FighterId::ALL {
        let Some((
            fighter,
            character,
            style,
            input,
            mut motor,
            stats,
            mut equipment,
            mut action,
            mut transform,
        )) = fighter_query
            .iter_mut()
            .find(|(fighter, ..)| fighter.id == stable_owner.index())
        else {
            continue;
        };
        let loadout = LoadoutContext::for_character(character.kind, style.kind, equipment.kind);
        let Some(technique) = active_technique_definition_in_catalog(
            action.action,
            action.technique_id,
            loadout,
            &character_catalog,
        )
        .map(|technique| feel.apply_technique(technique)) else {
            continue;
        };

        let elapsed_ms = action.elapsed.as_millis_floor();
        for (event_index, event) in technique.script.events.iter().enumerate() {
            let event_mask = 1_u64 << event_index;
            let event_at_ms = feel.timeline_event_at_ms(&technique, event_index, event);
            if action.timeline_events_fired & event_mask != 0 || elapsed_ms < event_at_ms {
                continue;
            }
            let attack_payload_id = match event.kind {
                MoveTimelineEventKind::Attack(payload_id) => Some(payload_id),
                MoveTimelineEventKind::ChargedAttack { tap, partial, full } => {
                    if !action.charge_release_requested {
                        continue;
                    }
                    Some(charged_payload_for_elapsed(
                        action.charge_elapsed.as_seconds(),
                        tap,
                        partial,
                        full,
                    ))
                }
                _ => None,
            };
            action.timeline_events_fired |= event_mask;

            if let Some(payload_id) = attack_payload_id {
                let mut config = attack_config_from_payload_with_feel(payload_id, style, &feel);
                let hit_effect =
                    feel.hit_effect_for_payload(character.kind, technique.id, payload_id);
                let landing_linger = hitbox_landing_linger(payload_id);
                let ground_path_clearance = hitbox_ground_path_clearance(payload_id);
                debug_assert_eq!(config.payload_id, payload_id);
                if let Some(applied_modifier) =
                    apply_loadout_to_attack(action.action, loadout, &mut equipment, &mut config)
                {
                    skill_presentations.p2().loadout_cue(
                        stable_owner,
                        technique.id,
                        applied_modifier,
                    );
                }

                action.hitbox_spawned = true;
                let facing = crate::canonical_math::vec3_normalize_or_zero(motor.facing);
                let item_size = stats.item_size_multiplier();
                let base_range = config.range;
                let base_radius = config.radius;
                let range = base_range * item_size;
                let radius = base_radius * item_size;
                let center = shape_center(
                    transform.translation,
                    facing,
                    range,
                    config.vertical_offset_scale,
                    config.path,
                    0.0,
                );

                let hitbox = Hitbox {
                    owner: stable_owner,
                    kind: config.kind,
                    payload_id: Some(payload_id),
                    attacker_character: Some(character.kind),
                    technique_id: Some(technique.id),
                    hit_effect,
                    shape_id: config.shape_id,
                    reaction_family: config.reaction_family,
                    damage_profile: config.damage_profile,
                    element: config.element,
                    attacker_equipment: config.attacker_equipment,
                    attacker_style: config.attacker_style,
                    power: config.power,
                    str_scale: config.str_scale,
                    damage: config.damage,
                    knockback: config.knockback,
                    vertical_knockback: config.vertical_knockback,
                    guardable: config.guardable,
                    base_radius,
                    radius,
                    lifetime: TickTimer::from_seconds_ceil(config.active),
                    elapsed: ElapsedTicks::ZERO,
                    total_lifetime: seconds_to_ticks_ceil(config.active),
                    spawn_origin: transform.translation,
                    facing,
                    base_range,
                    range,
                    scales_with_owner_size: true,
                    vertical_offset_scale: config.vertical_offset_scale,
                    parented: config.parented,
                    path: config.path,
                    expires_on_owner_landing: landing_linger.is_some(),
                    landing_linger: landing_linger.unwrap_or(TickTimer::ZERO),
                    landing_linger_started: false,
                    ground_path_end: ground_path_clearance.is_some(),
                    ground_path_clearance: ground_path_clearance.unwrap_or(0.0),
                    impact_cue: config.impact_cue,
                    hitstop_scale: config.hitstop_scale,
                    shake_scale: config.shake_scale,
                    feedback_priority_bonus: config.feedback_priority_bonus,
                    already_hit: FighterHitMask::default(),
                };
                if let Some((_, hitbox_id)) = spawn_canonical_hitbox(
                    &mut commands,
                    &mut identities,
                    hitbox,
                    SimPosition::new(center),
                ) {
                    skill_presentations
                        .p2()
                        .attack_surface_spawned(AttackSurfacePresentation {
                            entity: hitbox_id,
                            owner: stable_owner,
                            scene: hitbox_scene_for_payload(payload_id),
                            center,
                            spawn_origin: transform.translation,
                            facing,
                            base_range,
                            range,
                            scales_with_owner_size: true,
                            vertical_offset_scale: config.vertical_offset_scale,
                            parented: config.parented,
                            path: config.path,
                            ground_path_end: ground_path_clearance.is_some(),
                            ground_path_clearance: ground_path_clearance.unwrap_or(0.0),
                            active_seconds: config.active,
                        });
                }
                continue;
            }

            match event.kind {
                MoveTimelineEventKind::SpawnBeeSkill(skill_id) => {
                    let bee_skill_spawn_mode =
                        if technique.id == TechniqueId::BeeLegacyUltimateStartup {
                            BeeSkillSpawnMode::AreaSwarm
                        } else {
                            BeeSkillSpawnMode::Standard
                        };
                    let mut bee_presentation = skill_presentations.p0();
                    spawn_bee_skill(
                        &mut commands,
                        &mut identities,
                        &mut bee_presentation,
                        &state,
                        active_arena.definition(),
                        stable_owner,
                        fighter.id,
                        style.kind,
                        transform.translation,
                        motor.facing,
                        input.aim,
                        stats.item_size_multiplier(),
                        bee_skill_spawn_mode,
                        skill_id,
                        skill_targets.as_slice(),
                    );
                }
                MoveTimelineEventKind::SpawnPenguinSkill(skill_id) => {
                    let spawned = if skill_id == PenguinSkillId::SnowflakeSwapShot {
                        if let Some(swap) = penguin_snowflake_swap_target(
                            stable_owner,
                            active_penguin_skill_snapshots.iter().copied(),
                        ) {
                            transform.translation = swap.penguin_destination;
                            motor.velocity = Vec3::ZERO;
                            motor.grounded = false;
                            motor.landing_aftermath = None;
                            motor.knockdown_on_land = false;
                            motor.reaction_bounces = 0;
                            if let Some(snowflake_entity) = identities.mapped_entity(swap.snowflake)
                            {
                                despawn_stable(
                                    &mut commands,
                                    &mut identities,
                                    snowflake_entity,
                                    StableSimEntity::new(swap.snowflake),
                                );
                            }
                            queue_penguin_snowflake_swap_presentation(
                                &mut commands,
                                swap.snowflake,
                                stable_owner,
                                swap.penguin_destination,
                                motor.facing,
                            );
                        }
                        false
                    } else {
                        spawn_penguin_skill_with_presentation(
                            &mut commands,
                            &mut identities,
                            &state,
                            active_arena.definition(),
                            stable_owner,
                            fighter.id,
                            style.kind,
                            transform.translation,
                            motor.facing,
                            input.aim,
                            stats.item_size_multiplier(),
                            skill_id,
                            skill_targets.as_slice(),
                            active_penguin_skill_snapshots
                                .iter()
                                .map(|(_, kind, owner, lifetime, _)| (*kind, *owner, *lifetime)),
                        )
                    };
                    let _ = spawned;
                }
                MoveTimelineEventKind::SpawnChickSkill(skill_id) => {
                    let mut chick_presentation = skill_presentations.p1();
                    spawn_chick_skill_with_presentation(
                        &mut commands,
                        &mut identities,
                        &mut chick_presentation,
                        &state,
                        active_arena.definition(),
                        stable_owner,
                        fighter.id,
                        style.kind,
                        transform.translation,
                        motor.facing,
                        input.aim,
                        stats.item_size_multiplier(),
                        skill_id,
                        skill_targets.as_slice(),
                        active_chick_skill_snapshots.as_slice(),
                    );
                }
                MoveTimelineEventKind::Feedback(phase, cue) => {
                    let phase_profile = timeline_feedback_profile(phase, cue);
                    let package = feedback_package_for_timeline_cue(phase, cue);
                    let visual = if package.id == FeedbackPackageId::FirePunchTrail
                        && let Some(side) = light_fire_punch_visual_side(technique.id)
                    {
                        LIGHT_FIRE_PUNCH_CARD_ENABLED.then_some(TimelineFeedbackVisual::FirePunch {
                            side,
                            palette: light_fire_punch_palette(character.kind),
                        })
                    } else {
                        Some(TimelineFeedbackVisual::Package(package.id))
                    };
                    skill_presentations.p2().timeline_feedback(
                        stable_owner,
                        technique.id,
                        TimelineFeedbackPresentation {
                            fighter: stable_owner,
                            position: transform.translation,
                            direction: motor.facing,
                            cue,
                            priority: phase_profile.priority,
                            shake: phase_profile.shake * package.shake_scale,
                            hud_flash: phase_profile.hud_flash * package.hud_flash_scale,
                            visual,
                        },
                    );
                }
                MoveTimelineEventKind::Attack(_) | MoveTimelineEventKind::ChargedAttack { .. } => {
                    unreachable!()
                }
                MoveTimelineEventKind::Motion { forward, lift } => {
                    let (forward, lift) =
                        feel.timeline_motion(&technique, event_index, event, forward, lift);
                    let facing = crate::canonical_math::vec3_normalize_or_zero(motor.facing);
                    motor.velocity.x += facing.x * forward;
                    motor.velocity.z += facing.z * forward;
                    if lift.abs() > 0.0 {
                        motor.velocity.y = motor.velocity.y.max(lift);
                        motor.grounded = false;
                    }
                }
                MoveTimelineEventKind::NextTech | MoveTimelineEventKind::Recover => {}
                MoveTimelineEventKind::Stop => {
                    motor.velocity.x = 0.0;
                    motor.velocity.z = 0.0;
                }
            }
        }
    }
}

pub fn can_receive_impact(stats: &FighterStats, action: &FighterActionState) -> bool {
    !stats.invulnerability.active()
        && !matches!(
            action.action,
            FighterAction::Knockdown
                | FighterAction::Grabbed
                | FighterAction::GetUp
                | FighterAction::RingOut
                | FighterAction::Respawning
        )
}

fn fighter_hurt_box(
    position: &SimPosition,
    motor: &FighterMotor,
    character: &FighterCharacter,
    stats: &FighterStats,
    character_catalog: &CharacterMoveCatalog,
) -> FighterBodyBox {
    fighter_body_box(
        position.translation,
        motor.facing,
        character_catalog.body(character.kind),
        stats.item_size_multiplier(),
    )
}

pub fn radial_falloff(flat_distance: f32, radius: f32) -> f32 {
    1.0 - (flat_distance / radius).clamp(0.0, 0.72)
}

pub fn incoming_direction(target_position: Vec3, origin: Vec3) -> Vec3 {
    crate::canonical_math::vec3_normalize_or_zero(Vec3::new(
        origin.x - target_position.x,
        0.0,
        origin.z - target_position.z,
    ))
}

pub fn knockback_direction(target_position: Vec3, origin: Vec3) -> Vec3 {
    crate::canonical_math::vec3_normalize_or_zero(Vec3::new(
        target_position.x - origin.x,
        0.0,
        target_position.z - origin.z,
    ))
}

fn impact_knockback_direction(
    profile: &ImpactProfile,
    target_position: Vec3,
    origin: Vec3,
) -> Vec3 {
    profile
        .knockback_direction
        .unwrap_or_else(|| knockback_direction(target_position, origin))
}

fn pig_ham_swing_impact_travel(payload_id: AttackPayloadId) -> Option<(f32, f32, bool)> {
    match payload_id {
        AttackPayloadId::PigHamSwingTap => Some((0.18, 0.0, false)),
        AttackPayloadId::PigHamSwingPartial => Some((0.34, 0.32, true)),
        AttackPayloadId::PigHamSwingFull => Some((0.58, 0.0, false)),
        _ => None,
    }
}

fn apply_pig_ham_swing_impact_travel(
    motor: &mut FighterMotor,
    profile: &ImpactProfile,
    planar_speed: f32,
) {
    let Some((speed_limit_timer, slide_timer, clear_landing_stick)) =
        profile.payload_id.and_then(pig_ham_swing_impact_travel)
    else {
        return;
    };

    motor
        .impact_speed_limit_timer
        .set_max(TickTimer::from_seconds_ceil(speed_limit_timer));
    motor.impact_speed_limit = motor.impact_speed_limit.max(planar_speed);
    motor
        .dash_slide_timer
        .set_max(TickTimer::from_seconds_ceil(slide_timer));
    if clear_landing_stick {
        motor.landing_stick_timer.clear();
    }
}

fn apply_penguin_slope_ultimate_impact_travel(
    motor: &mut FighterMotor,
    profile: &ImpactProfile,
    planar_speed: f32,
) {
    if profile.payload_id != Some(AttackPayloadId::PenguinUltimateSlopeCrash) {
        return;
    }

    let carry_time =
        (PENGUIN_SLOPE_TOTAL_FORWARD / planar_speed.max(0.01) * 1.22).clamp(0.48, 0.82);
    motor
        .impact_speed_limit_timer
        .set_max(TickTimer::from_seconds_ceil(carry_time));
    motor.impact_speed_limit = motor.impact_speed_limit.max(planar_speed);
    motor
        .dash_slide_timer
        .set_max(TickTimer::from_seconds_ceil(carry_time + 0.18));
    motor.landing_stick_timer.clear();
}

fn penguin_slope_ultimate_attacker_recoil_direction(
    payload_id: Option<AttackPayloadId>,
    guarded: bool,
    owner_position: Option<Vec3>,
    contact_point: Option<Vec3>,
    fallback_facing: Vec3,
) -> Option<Vec3> {
    if guarded || payload_id != Some(AttackPayloadId::PenguinUltimateSlopeCrash) {
        return None;
    }

    owner_position
        .zip(contact_point)
        .and_then(|(owner_position, contact_point)| {
            planar_direction(contact_point - owner_position)
        })
        .or_else(|| planar_direction(fallback_facing))
}

fn apply_penguin_slope_ultimate_attacker_recoil(
    motor: &mut FighterMotor,
    action: &mut FighterActionState,
    hit_facing: Vec3,
) {
    let facing = planar_direction(hit_facing).unwrap_or(Vec3::Z);
    motor.velocity.x = -facing.x * PENGUIN_SLOPE_ULTIMATE_ATTACKER_RECOIL_SPEED;
    motor.velocity.z = -facing.z * PENGUIN_SLOPE_ULTIMATE_ATTACKER_RECOIL_SPEED;
    motor.velocity.y = motor
        .velocity
        .y
        .max(PENGUIN_SLOPE_ULTIMATE_ATTACKER_RECOIL_LIFT);
    motor.grounded = false;
    motor.dash_slide_timer.clear();
    motor.impact_speed_limit_timer.clear();
    motor.impact_speed_limit = 0.0;
    motor.landing_stick_timer.clear();
    action.timeline_events_fired |= PENGUIN_SLOPE_ULTIMATE_EXIT_MOTION_EVENT_MASK;
}

fn pig_air_meat_slam_air_hits_after_impact(
    payload_id: Option<AttackPayloadId>,
    was_airborne: bool,
    current_hits: u8,
) -> u8 {
    if payload_id != Some(AttackPayloadId::PigAirMeatSlam) {
        return current_hits;
    }
    if was_airborne {
        current_hits.saturating_add(1)
    } else {
        0
    }
}

fn pig_air_meat_slam_should_meteor(
    profile: &ImpactProfile,
    was_airborne: bool,
    air_hits: u8,
) -> bool {
    profile.payload_id == Some(AttackPayloadId::PigAirMeatSlam)
        && was_airborne
        && air_hits.saturating_add(1) >= PIG_AIR_MEAT_SLAM_METEOR_AIR_HITS
}

fn apply_pig_air_meat_slam_meteor_profile(profile: &mut ImpactProfile) {
    let mut reaction = reaction_profile_for_family(ReactionFamilyId::AerialSpikeDown);
    reaction.horizontal_scale = 0.36;
    reaction.vertical_scale = PIG_AIR_MEAT_SLAM_METEOR_VERTICAL_SCALE;
    reaction.priority_bonus = reaction.priority_bonus.saturating_add(4);
    profile.reaction_family = ReactionFamilyId::AerialSpikeDown;
    profile.reaction = reaction;
    profile.damage_profile = DamageProfileId::AerialSpike;
    profile.vertical_knockback =
        scaled_vertical_knockback(PIG_AIR_MEAT_SLAM_METEOR_VERTICAL_KNOCKBACK);
    profile.knockback *= 0.48;
    profile.feedback.cue = "impact_jump_spike";
    profile.feedback.hitstop *= 1.05;
    profile.feedback.shake *= 1.18;
    profile.feedback.priority = profile.feedback.priority.saturating_add(6);
}

fn should_defer_ground_reaction_until_landing(
    was_airborne: bool,
    pending_landing_aftermath: Option<QueuedAftermath>,
    pending_knockdown_on_land: bool,
) -> bool {
    was_airborne && (pending_landing_aftermath.is_some() || pending_knockdown_on_land)
}

fn landing_aftermath_after_airborne_rehit(
    was_airborne: bool,
    incoming_landing_aftermath: Option<QueuedAftermath>,
    pending_landing_aftermath: Option<QueuedAftermath>,
) -> Option<QueuedAftermath> {
    if was_airborne {
        incoming_landing_aftermath.or(pending_landing_aftermath)
    } else {
        incoming_landing_aftermath
    }
}

fn apply_airborne_juggle_hitstun(
    motor: &mut FighterMotor,
    action: &mut FighterActionState,
    preserved_velocity_y: f32,
    reaction: ReactionProfile,
    pending_landing_aftermath: Option<QueuedAftermath>,
    pending_knockdown_on_land: bool,
    pending_reaction_bounces: u8,
) {
    motor.velocity.y = preserved_velocity_y;
    motor.grounded = false;
    motor.landing_aftermath = pending_landing_aftermath;
    motor.knockdown_on_land = pending_knockdown_on_land;
    motor.reaction_bounces = pending_reaction_bounces;
    action.action = FighterAction::Hitstun;
    action.reaction_getup_ms = None;
    action.reaction_recover_ms = reaction.hitstun_recover_ms;
    action.reaction_family = Some(reaction.id);
}

fn reaction_visual_side(facing: Vec3, impact_direction: Vec3) -> f32 {
    let facing = facing.normalize_or_zero();
    let impact_direction = impact_direction.normalize_or_zero();
    let side = facing.cross(impact_direction).y;
    if side < -0.05 { -1.0 } else { 1.0 }
}

const GUARD_SIDE_DOT_THRESHOLD: f32 = 0.0;

pub fn guard_faces_impact(facing: Vec3, target_position: Vec3, origin: Vec3) -> bool {
    crate::canonical_math::vec3_normalize_or_zero(facing)
        .dot(incoming_direction(target_position, origin))
        >= GUARD_SIDE_DOT_THRESHOLD
}

pub fn impact_is_guarded(
    profile: &ImpactProfile,
    stamina: f32,
    action: FighterAction,
    facing: Vec3,
    target_position: Vec3,
    origin: Vec3,
) -> bool {
    profile.guardable
        && stamina > 0.0
        && action == FighterAction::Guarding
        && guard_faces_impact(facing, target_position, origin)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DamageContext {
    pub guarded: bool,
    pub perfect_guard: bool,
    pub airborne: bool,
    pub downed: bool,
    pub counter_hit: bool,
    pub low_health: bool,
    pub ignore_damage: bool,
    pub global_damage_scale: f32,
    pub rule_damage_correction: f32,
    pub source: ImpactSource,
    pub guard_stamina: f32,
    pub incoming_guard_stamina_damage: f32,
    pub target_health: f32,
    pub target_status: DamageTargetStatus,
    pub element: DamageElement,
    pub carryover_element: Option<DamageElement>,
    pub carryover_strength: f32,
    pub element_affinity: DamageElementAffinity,
    pub attacker_equipment: Option<EquipmentKind>,
    pub attacker_style: Option<FighterStyleKind>,
    pub defender_equipment: Option<EquipmentKind>,
    pub defender_style: Option<FighterStyleKind>,
    pub heavy_impact: bool,
    pub high_power: bool,
    pub lethal_raw: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DamageDefenderProfile {
    pub style: Option<FighterStyleKind>,
    pub equipment: Option<EquipmentKind>,
}

impl DamageDefenderProfile {
    pub fn from_loadout(style: &FighterStyle, equipment: &FighterEquipment) -> Self {
        Self {
            style: Some(style.kind),
            equipment: Some(equipment.kind),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DamageOutcome {
    pub health_damage: f32,
    pub guard_stamina_damage: f32,
    pub ignore_time_ms: u32,
    pub terminal: DamageTerminalKind,
    pub score_scale: f32,
    pub nonlethal: bool,
    pub side_effect: DamageSideEffectOutcome,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DamageSideEffectOutcome {
    pub id: Option<DamageSideEffectId>,
    pub cue: Option<&'static str>,
    pub invulnerability_ms: u32,
    pub stamina_delta: f32,
    pub hud_flash: f32,
    pub score_scale_add: f32,
}

impl DamageSideEffectOutcome {
    pub const fn none() -> Self {
        Self {
            id: None,
            cue: None,
            invulnerability_ms: 0,
            stamina_delta: 0.0,
            hud_flash: 0.0,
            score_scale_add: 0.0,
        }
    }
}

#[cfg(test)]
pub fn damage_context(
    state: &MatchState,
    stats: &FighterStats,
    motor: &FighterMotor,
    action: &FighterActionState,
    profile: &ImpactProfile,
    guarded: bool,
    perfect_guard: bool,
) -> DamageContext {
    damage_context_for_defender(
        state,
        stats,
        motor,
        action,
        profile,
        guarded,
        perfect_guard,
        DamageDefenderProfile::default(),
    )
}

pub fn damage_context_for_defender(
    state: &MatchState,
    stats: &FighterStats,
    motor: &FighterMotor,
    action: &FighterActionState,
    profile: &ImpactProfile,
    guarded: bool,
    perfect_guard: bool,
    defender: DamageDefenderProfile,
) -> DamageContext {
    let raw_damage = profile.damage.max(profile.power * profile.str_scale);
    let target_status = target_damage_status(action.action, motor.grounded);
    let carryover_element = stats.element_carry;
    let carryover_strength = stats.element_carry_strength.clamp(0.0, 1.0);
    DamageContext {
        guarded,
        perfect_guard,
        airborne: !motor.grounded,
        downed: matches!(
            action.action,
            FighterAction::Knockdown
                | FighterAction::GetUp
                | FighterAction::LandingRecovery
                | FighterAction::QuickStand
                | FighterAction::RecoveryRoll
        ),
        counter_hit: matches!(
            action.action,
            FighterAction::LightAttack1
                | FighterAction::LightAttack2
                | FighterAction::ComboFinisher
                | FighterAction::HeavyAttack
                | FighterAction::HeavyAttack2
                | FighterAction::UltimateStartup
                | FighterAction::UltimateRush
                | FighterAction::DashAttack
                | FighterAction::JumpAttack
                | FighterAction::JumpHeavyAttack
                | FighterAction::GrabStartup
                | FighterAction::SpecialCast
                | FighterAction::ItemSwing
                | FighterAction::GuardCounter
        ),
        low_health: stats.health <= MAX_HEALTH * 0.35,
        ignore_damage: false,
        global_damage_scale: global_damage_scale(state),
        rule_damage_correction: rule_damage_correction(state),
        source: profile.source,
        guard_stamina: stats.stamina,
        incoming_guard_stamina_damage: profile.guard_stamina_damage,
        target_health: stats.health,
        target_status,
        element: profile.element,
        carryover_element,
        carryover_strength,
        element_affinity: stacked_element_affinity_for_defender(
            profile.element,
            carryover_element,
            carryover_strength,
            defender.style,
            defender.equipment,
            target_status,
        ),
        attacker_equipment: profile.attacker_equipment,
        attacker_style: profile.attacker_style,
        defender_equipment: defender.equipment,
        defender_style: defender.style,
        heavy_impact: profile.feedback.heavy_spark
            || profile.feedback.priority >= 40
            || profile.knockback >= 8.0,
        high_power: profile.power >= 13.0,
        lethal_raw: raw_damage >= stats.health,
    }
}

pub fn target_damage_status(action: FighterAction, grounded: bool) -> DamageTargetStatus {
    if !grounded {
        return DamageTargetStatus::Airborne;
    }
    match action {
        FighterAction::Guarding => DamageTargetStatus::Guarding,
        FighterAction::GuardBroken => DamageTargetStatus::GuardBroken,
        FighterAction::Grabbed | FighterAction::GrabHold | FighterAction::UltimateVictim => {
            DamageTargetStatus::Grabbed
        }
        FighterAction::Knockdown | FighterAction::GetUp => DamageTargetStatus::Downed,
        FighterAction::QuickStand
        | FighterAction::RecoveryRoll
        | FighterAction::LandingRecovery => DamageTargetStatus::Recovering,
        FighterAction::LightAttack1
        | FighterAction::LightAttack2
        | FighterAction::ComboFinisher
        | FighterAction::HeavyAttack
        | FighterAction::HeavyAttack2
        | FighterAction::UltimateStartup
        | FighterAction::UltimateRush
        | FighterAction::DashAttack
        | FighterAction::JumpAttack
        | FighterAction::JumpHeavyAttack
        | FighterAction::GrabStartup
        | FighterAction::Throwing
        | FighterAction::SpecialCast
        | FighterAction::ItemSwing
        | FighterAction::ItemThrow
        | FighterAction::GuardCounter => DamageTargetStatus::Attacking,
        _ => DamageTargetStatus::Standing,
    }
}

pub fn element_affinity_for_defender(
    element: DamageElement,
    defender_style: Option<FighterStyleKind>,
    defender_equipment: Option<EquipmentKind>,
    target_status: DamageTargetStatus,
) -> DamageElementAffinity {
    if matches!(element, DamageElement::Neutral) {
        return DamageElementAffinity::Neutral;
    }
    if target_status == DamageTargetStatus::GuardBroken {
        return DamageElementAffinity::Weak;
    }
    if target_status == DamageTargetStatus::Guarding
        && defender_equipment == Some(EquipmentKind::CounterCell)
        && matches!(element, DamageElement::Shock | DamageElement::Strike)
    {
        return DamageElementAffinity::Absorbed;
    }
    if target_status == DamageTargetStatus::Airborne
        && defender_equipment == Some(EquipmentKind::AerialSpur)
        && matches!(element, DamageElement::Wind | DamageElement::Launch)
    {
        return DamageElementAffinity::Absorbed;
    }

    match (defender_equipment, element) {
        (Some(EquipmentKind::DashCoil), DamageElement::Wind)
        | (Some(EquipmentKind::AerialSpur), DamageElement::Launch | DamageElement::Wind)
        | (Some(EquipmentKind::CounterCell), DamageElement::Shock | DamageElement::Strike)
        | (Some(EquipmentKind::HeavySeal), DamageElement::Earth | DamageElement::Blast) => {
            return DamageElementAffinity::Resistant;
        }
        (Some(EquipmentKind::DashCoil), DamageElement::Earth | DamageElement::Shock)
        | (Some(EquipmentKind::AerialSpur), DamageElement::Earth | DamageElement::Blast)
        | (Some(EquipmentKind::CounterCell), DamageElement::Blast)
        | (Some(EquipmentKind::HeavySeal), DamageElement::Wind | DamageElement::Shock) => {
            return DamageElementAffinity::Weak;
        }
        _ => {}
    }

    match (defender_style, element) {
        (Some(FighterStyleKind::Anchor), DamageElement::Earth | DamageElement::Launch) => {
            DamageElementAffinity::Resistant
        }
        (Some(FighterStyleKind::Anchor), DamageElement::Wind | DamageElement::Shock) => {
            DamageElementAffinity::Weak
        }
        (Some(FighterStyleKind::Vector), DamageElement::Wind | DamageElement::Strike) => {
            DamageElementAffinity::Resistant
        }
        (Some(FighterStyleKind::Vector), DamageElement::Earth | DamageElement::Blast) => {
            DamageElementAffinity::Weak
        }
        (Some(FighterStyleKind::Catalyst), DamageElement::Shock | DamageElement::Hazard) => {
            DamageElementAffinity::Resistant
        }
        (Some(FighterStyleKind::Catalyst), DamageElement::Strike | DamageElement::Launch) => {
            DamageElementAffinity::Weak
        }
        _ => DamageElementAffinity::Neutral,
    }
}

fn affinity_score(affinity: DamageElementAffinity) -> f32 {
    match affinity {
        DamageElementAffinity::Neutral => 0.0,
        DamageElementAffinity::Resistant => -1.0,
        DamageElementAffinity::Weak => 1.0,
        DamageElementAffinity::Absorbed => -2.0,
    }
}

fn score_to_affinity(score: f32) -> DamageElementAffinity {
    if score <= -0.45 {
        DamageElementAffinity::Resistant
    } else if score >= 0.45 {
        DamageElementAffinity::Weak
    } else {
        DamageElementAffinity::Neutral
    }
}

pub fn stacked_element_affinity_for_defender(
    element: DamageElement,
    carryover_element: Option<DamageElement>,
    carryover_strength: f32,
    defender_style: Option<FighterStyleKind>,
    defender_equipment: Option<EquipmentKind>,
    target_status: DamageTargetStatus,
) -> DamageElementAffinity {
    let primary =
        element_affinity_for_defender(element, defender_style, defender_equipment, target_status);
    let mut weighted_score = affinity_score(primary);
    let mut strongest_abs = weighted_score.abs();
    let mut absorbed_present = matches!(primary, DamageElementAffinity::Absorbed);

    if let Some(carry) = carryover_element
        && !matches!(carry, DamageElement::Neutral)
    {
        let carry_affinity =
            element_affinity_for_defender(carry, defender_style, defender_equipment, target_status);
        let weight = carryover_strength.clamp(0.0, 1.0) * 0.75;
        let carry_score = affinity_score(carry_affinity) * weight;
        absorbed_present |= matches!(carry_affinity, DamageElementAffinity::Absorbed);
        weighted_score += carry_score;
        strongest_abs = strongest_abs.max(carry_score.abs());
    }

    if strongest_abs <= 0.02 {
        DamageElementAffinity::Neutral
    } else if absorbed_present && weighted_score <= -1.2 {
        DamageElementAffinity::Absorbed
    } else {
        score_to_affinity(weighted_score)
    }
}

pub fn resolve_authored_damage(profile: &ImpactProfile, context: DamageContext) -> DamageOutcome {
    let damage_profile = damage_profile_definition(profile.damage_profile);
    if context.perfect_guard {
        return DamageOutcome {
            health_damage: 0.0,
            guard_stamina_damage: 0.0,
            ignore_time_ms: 0,
            terminal: DamageTerminalKind::Normal,
            score_scale: 0.0,
            nonlethal: false,
            side_effect: DamageSideEffectOutcome::none(),
        };
    }

    let authored_raw = profile.damage.max(profile.power * profile.str_scale);
    let mut damage = base_damage(authored_raw, damage_profile.defense_mode, context);
    for reduction in damage_profile.reductions {
        if damage_condition_matches(reduction.condition, context) {
            damage = definitive_damage(damage * reduction.factor, damage_profile.minimum_damage);
        }
    }

    for modifier in damage_profile.modifiers {
        damage = apply_damage_modifier(damage, *modifier, context);
    }

    damage *= context.global_damage_scale * context.rule_damage_correction;

    let terminal = resolve_damage_terminal(
        damage_profile.terminal,
        damage_profile.terminal_overrides,
        context,
    );

    if matches!(terminal.kind, DamageTerminalKind::NoHpLoss) {
        damage = 0.0;
    }
    let side_effect = resolve_damage_side_effects(damage_profile.side_effects, context);

    DamageOutcome {
        health_damage: definitive_damage(damage, damage_profile.minimum_damage),
        guard_stamina_damage: profile.guard_stamina_damage * damage_profile.guard_stamina_scale,
        ignore_time_ms: terminal.ignore_time_ms,
        terminal: terminal.kind,
        score_scale: (terminal.score_scale + side_effect.score_scale_add).max(0.0),
        nonlethal: matches!(terminal.kind, DamageTerminalKind::Nonlethal),
        side_effect,
    }
}

fn guarded_damage_outcome(mut outcome: DamageOutcome) -> DamageOutcome {
    outcome.health_damage *= GUARD_HEALTH_DAMAGE_SCALE;
    outcome
}

fn resolve_damage_terminal(
    base: DamageTerminalDef,
    overrides: &[crate::techniques::DamageTerminalOverrideDef],
    context: DamageContext,
) -> DamageTerminalDef {
    let mut terminal = base;
    for override_def in overrides {
        if damage_condition_matches(override_def.condition, context) {
            terminal = override_def.terminal;
        }
    }
    terminal
}

fn base_damage(raw: f32, defense_mode: DamageDefenseMode, context: DamageContext) -> f32 {
    if context.ignore_damage {
        return 0.0;
    }
    let defense_factor = match defense_mode {
        DamageDefenseMode::Normal { factor } | DamageDefenseMode::Fixed { factor } => factor,
        DamageDefenseMode::Ignore => 1.0,
        DamageDefenseMode::NoDamage => return 0.0,
    };
    if defense_factor == 0.0 {
        0.0
    } else {
        definitive_damage(raw * defense_factor, 1.0)
    }
}

fn apply_damage_modifier(damage: f32, modifier: DamageModifierDef, context: DamageContext) -> f32 {
    if damage_condition_matches(modifier.condition, context) {
        damage * modifier.scale + modifier.add
    } else {
        damage
    }
}

fn damage_condition_matches(condition: DamageCondition, context: DamageContext) -> bool {
    match condition {
        DamageCondition::Guarded => context.guarded,
        DamageCondition::Unguarded => !context.guarded,
        DamageCondition::Airborne => context.airborne,
        DamageCondition::Downed => context.downed,
        DamageCondition::CounterHit => context.counter_hit,
        DamageCondition::LowHealth => context.low_health,
        DamageCondition::WeakGuard => {
            context.guarded && context.guard_stamina <= MAX_STAMINA * 0.35
        }
        DamageCondition::GuardBreak => {
            context.guarded && context.incoming_guard_stamina_damage >= context.guard_stamina
        }
        DamageCondition::ProjectileSource => matches!(context.source, ImpactSource::Projectile),
        DamageCondition::ItemSource => matches!(
            context.source,
            ImpactSource::ItemMelee | ImpactSource::ItemThrow | ImpactSource::ItemBlast
        ),
        DamageCondition::HazardSource => matches!(context.source, ImpactSource::Hazard),
        DamageCondition::HeavyImpact => context.heavy_impact,
        DamageCondition::HighPower => context.high_power,
        DamageCondition::LethalRaw => context.lethal_raw,
        DamageCondition::Element(element) => context.element == element,
        DamageCondition::ElementAffinity(affinity) => context.element_affinity == affinity,
        DamageCondition::TargetStatus(status) => context.target_status == status,
        DamageCondition::AttackerEquipment(equipment) => {
            context.attacker_equipment == Some(equipment)
        }
        DamageCondition::AttackerStyle(style) => context.attacker_style == Some(style),
        DamageCondition::DefenderEquipment(equipment) => {
            context.defender_equipment == Some(equipment)
        }
        DamageCondition::DefenderStyle(style) => context.defender_style == Some(style),
    }
}

fn resolve_damage_side_effects(
    side_effects: &[DamageSideEffectDef],
    context: DamageContext,
) -> DamageSideEffectOutcome {
    let mut outcome = DamageSideEffectOutcome::none();
    for side_effect in side_effects {
        if !damage_condition_matches(side_effect.condition, context) {
            continue;
        }
        outcome.id = Some(side_effect.id);
        outcome.cue = Some(side_effect.cue);
        outcome.invulnerability_ms = outcome
            .invulnerability_ms
            .max(side_effect.invulnerability_ms);
        outcome.stamina_delta += side_effect.stamina_delta;
        outcome.hud_flash = outcome.hud_flash.max(side_effect.hud_flash);
        outcome.score_scale_add += side_effect.score_scale_add;
    }
    outcome
}

fn global_damage_scale(state: &MatchState) -> f32 {
    match state.rules.preset {
        RulePreset::TimedTeamScore => 1.0,
        RulePreset::FreeForAll => 1.05,
        RulePreset::StockRingOut => 0.92,
    }
}

fn rule_damage_correction(state: &MatchState) -> f32 {
    if state.rules.team_scoring && !state.rules.friendly_fire {
        0.96
    } else if state.rules.uses_stocks() {
        0.9
    } else {
        1.0
    }
}

fn definitive_damage(damage: f32, minimum_damage: f32) -> f32 {
    if damage <= 0.0 {
        0.0
    } else {
        damage.floor().max(minimum_damage)
    }
}

fn commit_health_damage(stats: &mut FighterStats, outcome: DamageOutcome) -> f32 {
    let health_damage = outcome.health_damage * stats.item_damage_taken_multiplier();
    let committed_damage = if outcome.nonlethal {
        health_damage.min((stats.health - 1.0).max(0.0))
    } else {
        health_damage.min(stats.health)
    };
    stats.health = (stats.health - committed_damage).max(if outcome.nonlethal { 1.0 } else { 0.0 });
    if committed_damage > 0.0 && outcome.ignore_time_ms > 0 {
        stats
            .invulnerability
            .set_max(TickTimer::from_millis_ceil(outcome.ignore_time_ms));
    }
    committed_damage
}

fn apply_damage_side_effect_state(stats: &mut FighterStats, outcome: DamageOutcome) {
    let side_effect = outcome.side_effect;
    if side_effect.stamina_delta.abs() > f32::EPSILON {
        stats.stamina = (stats.stamina + side_effect.stamina_delta).clamp(0.0, MAX_STAMINA);
    }
    if side_effect.invulnerability_ms > 0 {
        stats
            .invulnerability
            .set_max(TickTimer::from_millis_ceil(side_effect.invulnerability_ms));
    }
}

fn update_element_carryover(
    stats: &mut FighterStats,
    element: DamageElement,
    outcome: DamageOutcome,
    guarded: bool,
) {
    if matches!(element, DamageElement::Neutral) {
        stats.element_carry_strength = (stats.element_carry_strength - 0.16).max(0.0);
        if stats.element_carry_strength <= 0.0 {
            stats.element_carry = None;
            stats.element_carry_timer.clear();
        }
        return;
    }

    let mut infusion = if guarded { 0.16 } else { 0.3 };
    infusion += (outcome.health_damage / 20.0).clamp(0.0, 0.28);
    if matches!(
        outcome.side_effect.id,
        Some(DamageSideEffectId::ElementBurst)
    ) {
        infusion += 0.12;
    }
    if matches!(
        outcome.side_effect.id,
        Some(DamageSideEffectId::ElementWeakness)
    ) {
        infusion += 0.08;
    }
    if matches!(
        outcome.side_effect.id,
        Some(DamageSideEffectId::ElementAbsorb)
    ) {
        infusion *= 0.58;
    }
    infusion = infusion.clamp(0.0, 0.56);

    if stats.element_carry == Some(element) {
        stats.element_carry_strength =
            (stats.element_carry_strength * 0.68 + infusion).clamp(0.0, 1.0);
    } else {
        stats.element_carry = Some(element);
        stats.element_carry_strength =
            (stats.element_carry_strength * 0.42 + infusion).clamp(0.0, 0.88);
    }
    stats.element_carry_timer =
        TickTimer::from_seconds_ceil((1.6 + stats.element_carry_strength * 1.8).max(0.65));
}

pub fn credit_last_attacker(stats: &mut FighterStats, owner_id: usize) {
    stats.last_attacker = u8::try_from(owner_id).ok().and_then(FighterId::new);
}

pub fn impact_owner_can_receive_credit(owner_id: usize) -> bool {
    owner_id < FIGHTER_COUNT
}

#[cfg(test)]
pub fn impact_profile_from_hitbox(hitbox: &Hitbox) -> ImpactProfile {
    impact_profile_from_hitbox_with_reaction(
        hitbox,
        reaction_profile_for_family(hitbox.reaction_family),
    )
}

pub fn impact_profile_from_hitbox_with_feel(
    hitbox: &Hitbox,
    feel: &CombatFeelTuning,
) -> ImpactProfile {
    impact_profile_from_hitbox_with_reaction(
        hitbox,
        feel.apply_reaction(reaction_profile_for_family(hitbox.reaction_family)),
    )
    .with_hit_effects_enabled(feel.hit_effects_enabled())
}

fn impact_profile_from_hitbox_with_reaction(
    hitbox: &Hitbox,
    reaction: ReactionProfile,
) -> ImpactProfile {
    let _authored_payload = hitbox.payload_id;
    let _authored_shape = hitbox.shape_id;
    let mut profile = impact_profile(
        hitbox.owner.index(),
        ImpactSource::from_attack_kind(hitbox.kind),
        hitbox.damage,
        hitbox.knockback,
        hitbox.vertical_knockback,
        matches!(hitbox.reaction_family, ReactionFamilyId::GroundedDownGetup),
        hitbox.guardable,
        24.0,
        if hitbox.kind.is_heavy_feedback() {
            ImpactFeedbackIntensity::Heavy
        } else {
            ImpactFeedbackIntensity::Light
        },
        hitbox.reaction_family,
    );
    profile.reaction = reaction;
    profile.knockback_direction = hitbox_knockback_direction_override(hitbox);
    profile.feedback.cue = hitbox.impact_cue;
    profile.feedback.hitstop *= hitbox.hitstop_scale;
    profile.feedback.guard_hitstop *= hitbox.hitstop_scale;
    profile.feedback.shake *= hitbox.shake_scale;
    profile.feedback.guard_shake *= hitbox.shake_scale;
    profile.feedback.priority = profile
        .feedback
        .priority
        .saturating_add(hitbox.feedback_priority_bonus);
    profile.payload_id = hitbox.payload_id;
    profile.attacker_character = hitbox.attacker_character;
    profile.technique_id = hitbox.technique_id;
    profile.hit_effect = hitbox.hit_effect;
    profile.shape_id = Some(hitbox.shape_id);
    profile.damage_profile = hitbox.damage_profile;
    profile.element = hitbox.element;
    profile.attacker_equipment = hitbox.attacker_equipment;
    profile.attacker_style = hitbox.attacker_style;
    profile.power = hitbox.power;
    profile.str_scale = hitbox.str_scale;
    profile
}

fn hitbox_knockback_direction_override(hitbox: &Hitbox) -> Option<Vec3> {
    match hitbox.payload_id {
        Some(AttackPayloadId::KiriageBeat1 | AttackPayloadId::KiriageBeat2) => {
            hitbox_path_direction(hitbox).or_else(|| planar_direction(hitbox.facing))
        }
        Some(payload_id) if payload_is_jump_fish(payload_id) => {
            hitbox_path_direction(hitbox).or_else(|| planar_direction(hitbox.facing))
        }
        Some(payload_id) if payload_is_ultimate_scratch(payload_id) => {
            planar_direction(hitbox.facing)
        }
        Some(AttackPayloadId::PenguinUltimateSlopeCrash) => planar_direction(hitbox.facing),
        Some(
            AttackPayloadId::PigHamSwingTap
            | AttackPayloadId::PigHamSwingPartial
            | AttackPayloadId::PigHamSwingFull
            | AttackPayloadId::PigAirMeatSlam,
        ) => planar_direction(hitbox.facing),
        _ => None,
    }
}

fn hitbox_path_direction(hitbox: &Hitbox) -> Option<Vec3> {
    let start = shape_center(
        Vec3::ZERO,
        hitbox.facing,
        hitbox.range,
        hitbox.vertical_offset_scale,
        hitbox.path,
        0.0,
    );
    let end = shape_center(
        Vec3::ZERO,
        hitbox.facing,
        hitbox.range,
        hitbox.vertical_offset_scale,
        hitbox.path,
        1.0,
    );
    planar_direction(end - start)
}

fn planar_direction(direction: Vec3) -> Option<Vec3> {
    let planar = Vec3::new(direction.x, 0.0, direction.z);
    (crate::canonical_math::vec3_length_squared(planar) > 0.0001)
        .then(|| crate::canonical_math::vec3_normalize_or_zero(planar))
}

pub fn impact_profile_from_payload(
    owner_id: usize,
    source: ImpactSource,
    payload_id: AttackPayloadId,
    damage_scale: f32,
    knockback_scale: f32,
    vertical_scale: f32,
    guard_stamina_damage: f32,
) -> ImpactProfile {
    let payload = attack_payload_definition(payload_id);
    impact_profile_from_payload_def(
        owner_id,
        source,
        payload,
        damage_scale,
        knockback_scale,
        vertical_scale,
        guard_stamina_damage,
        reaction_profile_for_family(payload.reaction_family),
    )
}

pub fn impact_profile_from_payload_with_feel(
    owner_id: usize,
    source: ImpactSource,
    payload_id: AttackPayloadId,
    damage_scale: f32,
    knockback_scale: f32,
    vertical_scale: f32,
    guard_stamina_damage: f32,
    feel: &CombatFeelTuning,
) -> ImpactProfile {
    let payload = feel.apply_payload(attack_payload_definition(payload_id));
    impact_profile_from_payload_def(
        owner_id,
        source,
        payload,
        damage_scale,
        knockback_scale,
        vertical_scale,
        guard_stamina_damage,
        feel.apply_reaction(reaction_profile_for_family(payload.reaction_family)),
    )
    .with_hit_effects_enabled(feel.hit_effects_enabled())
}

fn impact_profile_from_payload_def(
    owner_id: usize,
    source: ImpactSource,
    payload: AttackPayloadDef,
    damage_scale: f32,
    knockback_scale: f32,
    vertical_scale: f32,
    guard_stamina_damage: f32,
    reaction: ReactionProfile,
) -> ImpactProfile {
    let mut profile = impact_profile(
        owner_id,
        source,
        payload.damage * damage_scale,
        payload.knockback * knockback_scale,
        payload.vertical_knockback * vertical_scale,
        matches!(
            payload.reaction_family,
            ReactionFamilyId::GroundedDownGetup | ReactionFamilyId::SlidingKnockdown
        ),
        payload.guardable,
        guard_stamina_damage,
        if payload.kind.is_heavy_feedback() || payload.feedback_priority_bonus >= 5 {
            ImpactFeedbackIntensity::Heavy
        } else {
            ImpactFeedbackIntensity::Light
        },
        payload.reaction_family,
    );
    profile.reaction = reaction;
    profile.payload_id = Some(payload.id);
    profile.shape_id = Some(payload.shape_id);
    profile.damage_profile = payload.damage_profile;
    profile.element = payload.element;
    profile.power = payload.power * damage_scale;
    profile.str_scale = payload.str_scale;
    profile.feedback.cue = payload.impact_cue;
    profile.feedback.hitstop *= payload.hitstop_scale;
    profile.feedback.guard_hitstop *= payload.hitstop_scale;
    profile.feedback.shake *= payload.shake_scale;
    profile.feedback.guard_shake *= payload.shake_scale;
    profile.feedback.priority = profile
        .feedback
        .priority
        .saturating_add(payload.feedback_priority_bonus);
    profile
}

#[cfg(test)]
pub fn apply_impact(
    commands: &mut Commands,
    effect_assets: &EffectAssets,
    effects: &mut HitEffects,
    hitstop: &mut Hitstop,
    state: &MatchState,
    stats: &mut FighterStats,
    motor: &mut FighterMotor,
    action: &mut FighterActionState,
    target_position: &SimPosition,
    contact_point: Option<Vec3>,
    origin: Vec3,
    profile: ImpactProfile,
    defender: DamageDefenderProfile,
    telemetry: &mut MatchTelemetry,
) {
    let outcome = apply_impact_core(
        hitstop,
        state,
        stats,
        motor,
        action,
        target_position.translation,
        contact_point,
        origin,
        profile,
        defender,
        telemetry,
    );
    apply_impact_presentation(
        commands,
        effect_assets,
        effects,
        stats,
        motor,
        action,
        outcome,
    );
}

/// Resolves the rollback-relevant part of an impact without renderer, audio,
/// command-buffer, or presentation-resource access.
pub fn apply_impact_core(
    hitstop: &mut Hitstop,
    state: &MatchState,
    stats: &mut FighterStats,
    motor: &mut FighterMotor,
    action: &mut FighterActionState,
    target_position: Vec3,
    contact_point: Option<Vec3>,
    origin: Vec3,
    profile: ImpactProfile,
    defender: DamageDefenderProfile,
    telemetry: &mut MatchTelemetry,
) -> ImpactOutcome {
    let mut profile = profile;
    let source = profile.source;
    let hurt_center = contact_point.unwrap_or(target_position + Vec3::Y * (FIGHTER_HEIGHT * 0.58));
    let was_airborne = !motor.grounded;
    let preserved_air_velocity_y = motor.velocity.y;
    let pending_landing_aftermath = motor.landing_aftermath;
    let pending_knockdown_on_land = motor.knockdown_on_land;
    let pending_reaction_bounces = motor.reaction_bounces;
    let guarded = impact_is_guarded(
        &profile,
        stats.stamina,
        action.action,
        motor.facing,
        target_position,
        origin,
    );

    if guarded {
        let impact_direction = impact_knockback_direction(&profile, target_position, origin);
        let damage_outcome = guarded_damage_outcome(resolve_authored_damage(
            &profile,
            damage_context_for_defender(
                state, stats, motor, action, &profile, guarded, false, defender,
            ),
        ));
        let knockback = profile.knockback * 0.35;
        let feedback = profile.feedback;
        hitstop.trigger(feedback.guard_hitstop);
        let committed_damage = commit_health_damage(stats, damage_outcome);
        record_impact_telemetry(
            telemetry,
            source,
            profile.owner_id,
            committed_damage * damage_outcome.score_scale,
        );
        if impact_owner_can_receive_credit(profile.owner_id) {
            credit_last_attacker(stats, profile.owner_id);
        }
        let guard_stamina_before = stats.stamina;
        stats.stamina = (stats.stamina - damage_outcome.guard_stamina_damage.max(0.0)).max(0.0);
        apply_damage_side_effect_state(stats, damage_outcome);
        update_element_carryover(stats, profile.element, damage_outcome, true);
        // `reaction_family` participates in rollback.  The visual side does not
        // and is applied by the compatibility presentation adapter below.
        action.reaction_family = None;
        let guard_broken = guard_stamina_before > 0.0 && stats.stamina <= 0.0;
        if guard_broken {
            telemetry.record_guard_break();
            action.action = FighterAction::GuardBroken;
            action.elapsed.reset();
            action.hitbox_spawned = false;
            action.queued_combo = false;
            action.queued_technique = None;
            action.queued_button = None;
            action.buffered_button = None;
            action.buffered_button_elapsed.reset();
            action.timeline_events_fired = 0;
            action.reaction_getup_ms = None;
            action.reaction_recover_ms = None;
            motor.clear_guard_counter_window();
        } else {
            motor.open_guard_counter_window(origin);
        }
        let guard_package = feedback_package_for_named_cue("guard_clang");
        motor.velocity.x = impact_direction.x * knockback;
        motor.velocity.z = impact_direction.z * knockback;
        return ImpactOutcome {
            guarded: true,
            committed_damage,
            resolved_reaction: None,
            presentation: ImpactPresentation {
                position: hurt_center,
                direction: impact_direction,
                source,
                feedback_cue: "guard_clang",
                feedback_priority: feedback.priority.saturating_add(8),
                reaction: None,
                side_effect_cue: damage_outcome.side_effect.cue,
                side_effect_priority: feedback.priority.saturating_add(14),
                visual: ImpactVisualPresentation::Guard {
                    package: guard_package.id,
                },
                combat_sfx: CombatSfxKind::Guarded,
                combat_sfx_priority: feedback.priority.saturating_add(10),
                hud_flash: feedback
                    .guard_hud_flash
                    .max(feedback.guard_hud_flash * guard_package.hud_flash_scale)
                    .max(damage_outcome.side_effect.hud_flash),
                reaction_visual_side: 1.0,
                camera_shake: feedback.guard_shake * guard_package.shake_scale,
            },
        };
    }

    motor.clear_guard_counter_window();

    let pig_air_meat_slam_air_hits = pig_air_meat_slam_air_hits_after_impact(
        profile.payload_id,
        was_airborne,
        motor.pig_air_meat_slam_air_hits,
    );
    let pig_air_meat_slam_meteor =
        pig_air_meat_slam_should_meteor(&profile, was_airborne, motor.pig_air_meat_slam_air_hits);
    if pig_air_meat_slam_meteor {
        apply_pig_air_meat_slam_meteor_profile(&mut profile);
    }

    let feedback = profile.feedback;
    let reaction = profile.reaction;
    let impact_direction = impact_knockback_direction(&profile, target_position, origin);
    debug_assert_eq!(reaction.id, profile.reaction_family);
    let reaction_weight = reaction_feedback_weight(reaction);
    let impact_package = feedback_package_for_named_cue(feedback.cue);
    let include_skill_accent = !matches!(
        source,
        ImpactSource::Projectile
            | ImpactSource::Trap
            | ImpactSource::Shockwave
            | ImpactSource::Hazard
    );
    let hit_effect = profile.hit_effect.unwrap_or_else(|| {
        hit_impact_effect_for_feedback_package(impact_package.id, feedback.heavy_spark)
    });
    hitstop.trigger(feedback.hitstop * reaction_weight.hitstop_scale);
    let reaction_priority = feedback
        .priority
        .saturating_add(reaction.priority_bonus)
        .saturating_add(reaction_weight.priority_bonus);
    let combat_sfx = combat_sfx_kind_for_impact(&profile);

    let damage_outcome = resolve_authored_damage(
        &profile,
        damage_context_for_defender(
            state, stats, motor, action, &profile, guarded, false, defender,
        ),
    );
    let committed_damage = commit_health_damage(stats, damage_outcome);
    record_impact_telemetry(
        telemetry,
        source,
        profile.owner_id,
        committed_damage * damage_outcome.score_scale,
    );
    if impact_owner_can_receive_credit(profile.owner_id) {
        credit_last_attacker(stats, profile.owner_id);
    }
    apply_damage_side_effect_state(stats, damage_outcome);
    update_element_carryover(stats, profile.element, damage_outcome, false);

    let health_scale = if stats.health <= 0.0 { 1.45 } else { 1.0 };
    let planar_impact_speed = profile.knockback * health_scale * reaction.horizontal_scale;
    motor.velocity.x = impact_direction.x * planar_impact_speed;
    motor.velocity.z = impact_direction.z * planar_impact_speed;
    motor.knockdown_on_land = false;
    motor.landing_aftermath = None;
    motor.jump_takeoff_timer.clear();
    motor.landing_stick_timer.clear();
    motor.dash_slide_timer.clear();
    motor.dash_jump_carry_timer.clear();
    motor.impact_speed_limit_timer.clear();
    motor.impact_speed_limit = 0.0;
    motor.guard_active_timer.reset();
    motor.guard_cooldown_timer.clear();
    motor.guard_start_buffer_timer.clear();
    motor.pig_air_meat_slam_air_hits = pig_air_meat_slam_air_hits;

    if reaction.immediate_down {
        if should_defer_ground_reaction_until_landing(
            was_airborne,
            pending_landing_aftermath,
            pending_knockdown_on_land,
        ) {
            apply_airborne_juggle_hitstun(
                motor,
                action,
                preserved_air_velocity_y,
                reaction,
                pending_landing_aftermath,
                true,
                pending_reaction_bounces,
            );
        } else {
            action.action = FighterAction::Knockdown;
            action.reaction_getup_ms = reaction.grounded_getup_ms.or(Some(500));
            action.reaction_recover_ms = reaction.grounded_recover_ms.or(Some(700));
            motor.velocity.y = 0.0;
            motor.grounded = true;
            motor.landing_stick_timer = TickTimer::from_millis_ceil(reaction.grounded_stick_ms);
            motor.reaction_bounces = 0;
        }
    } else if reaction.airborne {
        motor.velocity.y = profile.vertical_knockback * reaction.vertical_scale;
        motor.grounded = false;
        motor.landing_aftermath = landing_aftermath_after_airborne_rehit(
            was_airborne,
            reaction.landing_aftermath,
            pending_landing_aftermath,
        );
        motor.knockdown_on_land = pending_knockdown_on_land && motor.landing_aftermath.is_none();
        motor.reaction_bounces = if matches!(
            reaction.kind,
            ReactionKind::Launch
                | ReactionKind::Tumble
                | ReactionKind::GroundBounce
                | ReactionKind::WallBounce
        ) {
            1
        } else {
            0
        };
        action.action = FighterAction::Hitstun;
        action.reaction_getup_ms = None;
        action.reaction_recover_ms = reaction.hitstun_recover_ms;
    } else if profile.force_knockdown {
        if should_defer_ground_reaction_until_landing(
            was_airborne,
            pending_landing_aftermath,
            pending_knockdown_on_land,
        ) {
            apply_airborne_juggle_hitstun(
                motor,
                action,
                preserved_air_velocity_y,
                reaction,
                pending_landing_aftermath,
                true,
                pending_reaction_bounces,
            );
        } else {
            action.action = FighterAction::Knockdown;
            action.reaction_getup_ms = Some(500);
            action.reaction_recover_ms = Some(700);
            motor.velocity.y = 0.0;
            motor.grounded = true;
            motor.landing_stick_timer = TickTimer::from_seconds_ceil(0.1);
            motor.reaction_bounces = 0;
        }
    } else {
        if should_defer_ground_reaction_until_landing(
            was_airborne,
            pending_landing_aftermath,
            pending_knockdown_on_land,
        ) {
            apply_airborne_juggle_hitstun(
                motor,
                action,
                preserved_air_velocity_y,
                reaction,
                pending_landing_aftermath,
                pending_knockdown_on_land,
                pending_reaction_bounces,
            );
        } else {
            motor.velocity.y = 0.0;
            motor.grounded = true;
            motor.reaction_bounces = 0;
            action.action = FighterAction::Hitstun;
            action.reaction_getup_ms = None;
            action.reaction_recover_ms = reaction.hitstun_recover_ms;
        }
    }
    if pig_air_meat_slam_meteor {
        motor.reaction_bounces = 0;
    }
    if motor.grounded {
        motor.pig_air_meat_slam_air_hits = 0;
    }
    let reaction_visual_side = if action.action == FighterAction::Hitstun {
        action.reaction_family = Some(reaction.id);
        reaction_visual_side(motor.facing, impact_direction)
    } else {
        action.reaction_family = None;
        1.0
    };
    apply_pig_ham_swing_impact_travel(motor, &profile, planar_impact_speed);
    apply_penguin_slope_ultimate_impact_travel(motor, &profile, planar_impact_speed);
    action.elapsed.reset();
    action.hitbox_spawned = false;
    action.queued_combo = false;
    action.queued_technique = None;
    action.queued_button = None;
    action.buffered_button = None;
    action.buffered_button_elapsed.reset();
    action.timeline_events_fired = 0;
    let base_shake = feedback.shake * impact_package.shake_scale + reaction_weight.shake_add;
    let camera_shake = successful_hit_camera_shake(
        profile.owner_id,
        profile.payload_id,
        committed_damage,
        base_shake,
    );
    ImpactOutcome {
        guarded: false,
        committed_damage,
        resolved_reaction: Some(reaction.id),
        presentation: ImpactPresentation {
            position: hurt_center,
            direction: impact_direction,
            source,
            feedback_cue: feedback.cue,
            feedback_priority: feedback.priority,
            reaction: Some(ImpactReactionPresentation {
                kind: reaction.kind,
                cue: reaction.cue,
                priority: reaction_priority,
            }),
            side_effect_cue: damage_outcome.side_effect.cue,
            side_effect_priority: feedback.priority.saturating_add(14),
            visual: ImpactVisualPresentation::Hit {
                element: profile.element,
                heavy_spark: feedback.heavy_spark,
                spark_scale: feedback.spark_scale * reaction_weight.spark_scale,
                hit_effect,
                hit_effects_enabled: profile.hit_effects_enabled,
                include_skill_accent,
            },
            combat_sfx,
            combat_sfx_priority: reaction_priority,
            hud_flash: feedback
                .hit_hud_flash
                .max(feedback.hit_hud_flash * impact_package.hud_flash_scale)
                .max(damage_outcome.side_effect.hud_flash),
            reaction_visual_side,
            camera_shake,
        },
    }
}

pub fn apply_impact_presentation(
    commands: &mut Commands,
    effect_assets: &EffectAssets,
    effects: &mut HitEffects,
    stats: &mut FighterStats,
    _motor: &mut FighterMotor,
    action: &mut FighterActionState,
    outcome: ImpactOutcome,
) {
    let presentation = outcome.presentation;
    match presentation.visual {
        ImpactVisualPresentation::Guard { package } => {
            spawn_guard_flash(commands, effect_assets, presentation.position);
            spawn_feedback_package(
                commands,
                effect_assets,
                presentation.position,
                presentation.direction,
                package,
            );

            // Preserve the legacy cue arbitration order: authored side-effect
            // punctuation precedes the guard clang.
            if let Some(cue) = presentation.side_effect_cue {
                effects.push_feedback_cue(
                    cue,
                    presentation.source,
                    presentation.side_effect_priority,
                );
            }
            effects.push_feedback_cue(
                presentation.feedback_cue,
                presentation.source,
                presentation.feedback_priority,
            );
        }
        ImpactVisualPresentation::Hit {
            element,
            heavy_spark,
            spark_scale,
            hit_effect,
            hit_effects_enabled,
            include_skill_accent,
        } => {
            if hit_effects_enabled {
                spawn_hit_impact_effect(
                    commands,
                    effect_assets,
                    presentation.position,
                    presentation.direction,
                    element,
                    heavy_spark,
                    spark_scale,
                    hit_effect,
                    include_skill_accent,
                );
            } else {
                spawn_element_hit_spark(
                    commands,
                    effect_assets,
                    presentation.position,
                    element,
                    heavy_spark,
                    spark_scale,
                );
            }

            effects.push_feedback_cue(
                presentation.feedback_cue,
                presentation.source,
                presentation.feedback_priority,
            );
            if let Some(reaction) = presentation.reaction {
                effects.push_reaction_cue(reaction.kind, presentation.source);
                effects.push_feedback_cue(reaction.cue, presentation.source, reaction.priority);
            }
            if let Some(cue) = presentation.side_effect_cue {
                effects.push_feedback_cue(
                    cue,
                    presentation.source,
                    presentation.side_effect_priority,
                );
            }
        }
    }

    effects.push_combat_sfx(CombatSfxCue::new(
        presentation.combat_sfx,
        presentation.position,
        presentation.combat_sfx_priority,
    ));
    effects.shake = effects.shake.max(presentation.camera_shake);

    // These fields are explicitly presentation-only in the rollback schema.
    // Keep the public compatibility path visually identical while the headless
    // core remains independent from transient HUD/pose state.
    stats.hud_flash = presentation.hud_flash;
    action.reaction_visual_side = presentation.reaction_visual_side;
}

pub(crate) fn impact_sim_event_kind(
    outcome: ImpactOutcome,
    attacker: Option<FighterId>,
    victim: FighterId,
) -> SimEventKind {
    if outcome.guarded {
        SimEventKind::Guarded {
            attacker,
            defender: victim,
        }
    } else {
        SimEventKind::HitConfirmed {
            attacker,
            victim,
            damage_q: quantize_f32(outcome.committed_damage, DEFAULT_F32_QUANTIZATION),
            reaction: outcome
                .resolved_reaction
                .expect("unguarded impact outcomes always resolve a reaction family"),
        }
    }
}

fn successful_hit_camera_shake(
    owner_id: usize,
    payload_id: Option<AttackPayloadId>,
    committed_damage: f32,
    base_shake: f32,
) -> f32 {
    if owner_id != 0 || committed_damage <= 0.0 {
        return base_shake;
    }

    if payload_id.is_some_and(payload_is_ultimate_camera_hit) {
        return (base_shake * PLAYER_ULTIMATE_SUCCESS_HIT_CAMERA_SHAKE_MULTIPLIER
            + PLAYER_ULTIMATE_SUCCESS_HIT_CAMERA_SHAKE_BONUS)
            .min(PLAYER_ULTIMATE_SUCCESS_HIT_CAMERA_SHAKE_MAX);
    }

    (base_shake * PLAYER_DEFAULT_SUCCESS_HIT_CAMERA_SHAKE_MULTIPLIER
        + PLAYER_DEFAULT_SUCCESS_HIT_CAMERA_SHAKE_BONUS)
        .min(PLAYER_DEFAULT_SUCCESS_HIT_CAMERA_SHAKE_MAX)
}

fn payload_is_ultimate_camera_hit(payload_id: AttackPayloadId) -> bool {
    payload_is_ultimate_catch(payload_id)
        || payload_is_ultimate_scratch(payload_id)
        || payload_is_ultimate_bomb(payload_id)
}

fn record_impact_telemetry(
    telemetry: &mut MatchTelemetry,
    source: ImpactSource,
    owner_id: usize,
    damage: f32,
) {
    if damage <= 0.0 {
        return;
    }
    telemetry.record_damage(owner_id, damage);
    match source {
        ImpactSource::ItemMelee | ImpactSource::ItemThrow | ImpactSource::ItemBlast => {
            telemetry.record_item_hit();
        }
        ImpactSource::GrabThrow => {
            telemetry.record_throw();
        }
        _ => {}
    }
}

pub fn tick_feedback_cues(time: Res<Time>, mut effects: ResMut<HitEffects>) {
    if let Some(cue) = effects.last_cue.as_mut() {
        cue.remaining = (cue.remaining - time.delta_secs()).max(0.0);
        if cue.remaining == 0.0 {
            effects.clear_feedback_cue();
        }
    }
    if let Some(reaction) = effects.last_reaction.as_mut() {
        reaction.remaining = (reaction.remaining - time.delta_secs()).max(0.0);
        if reaction.remaining == 0.0 {
            effects.clear_reaction_cue();
        }
    }
}

fn hitbox_owner_size_scale(
    scales_with_owner_size: bool,
    owner_size_multiplier: Option<f32>,
) -> f32 {
    if scales_with_owner_size {
        owner_size_multiplier.unwrap_or(1.0)
    } else {
        1.0
    }
}

fn refresh_hitbox_dimensions(hitbox: &mut Hitbox, owner_size_multiplier: Option<f32>) {
    let scale = hitbox_owner_size_scale(hitbox.scales_with_owner_size, owner_size_multiplier);
    hitbox.range = hitbox.base_range * scale;
    hitbox.radius = hitbox.base_radius * scale;
}

fn refresh_hitbox_position(
    hitbox: &Hitbox,
    position: &mut SimPosition,
    owner_translation: Option<Vec3>,
    arena: &ArenaDefinition,
) {
    let progress = if hitbox.total_lifetime > 0 {
        (hitbox.elapsed.get() as f32 / hitbox.total_lifetime as f32).clamp(0.0, 1.0)
    } else {
        1.0
    };
    let base = if hitbox.parented {
        owner_translation.unwrap_or(hitbox.spawn_origin)
    } else {
        hitbox.spawn_origin
    };
    position.translation = shape_center_with_ground_path_end(
        base,
        hitbox.facing,
        hitbox.range,
        hitbox.vertical_offset_scale,
        hitbox.path,
        progress,
        hitbox.ground_path_end,
        hitbox.ground_path_clearance,
        arena,
    );
}

pub fn update_hitboxes(
    active_arena: Res<ActiveArena>,
    mut commands: Commands,
    mut identities: ResMut<SimulationIdentityAllocator>,
    mut presentation: CombatPresentationEmitter,
    hitstop: Res<Hitstop>,
    mut hitboxes: Query<(&StableSimEntity, &mut Hitbox, &mut SimPosition), Without<Fighter>>,
    owners: Query<
        (&Fighter, &SimPosition, &FighterMotor, &FighterStats),
        (With<Fighter>, Without<Hitbox>),
    >,
) {
    if hitstop.active() {
        return;
    }

    for index in 0..identities.capacity(SimEntityKind::Hitbox) {
        let Some((stable_id, entity)) = identities.entry_at(SimEntityKind::Hitbox, index) else {
            continue;
        };
        let Ok((stable, mut hitbox, mut position)) = hitboxes.get_mut(entity) else {
            continue;
        };
        if stable.id() != stable_id {
            continue;
        }
        hitbox.elapsed.advance();
        let expired = !hitbox.lifetime.active() || hitbox.lifetime.tick();
        let owner = owners
            .iter()
            .find(|(fighter, ..)| fighter.id == hitbox.owner.index());
        let owner_translation = owner.map(|(_, owner_transform, _, _)| owner_transform.translation);
        let owner_size_multiplier = owner.map(|(_, _, _, stats)| stats.item_size_multiplier());
        refresh_hitbox_dimensions(&mut hitbox, owner_size_multiplier);
        start_hitbox_landing_linger(
            &mut hitbox,
            owner.is_some_and(|(_, _, motor, _)| motor.grounded),
        );
        refresh_hitbox_position(
            &hitbox,
            &mut position,
            owner_translation,
            active_arena.definition(),
        );
        if expired {
            presentation.attack_surface_despawned(stable.id());
            despawn_stable(&mut commands, &mut identities, entity, *stable);
        }
    }
}

struct FighterSlots<T> {
    entries: [Option<T>; FighterId::ALL.len()],
}

impl<T> Default for FighterSlots<T> {
    fn default() -> Self {
        Self {
            entries: std::array::from_fn(|_| None),
        }
    }
}

impl<T> FighterSlots<T> {
    fn insert_first(&mut self, fighter: FighterId, value: T) {
        if self.entries[fighter.index()].is_none() {
            self.entries[fighter.index()] = Some(value);
        }
    }

    fn get(&self, fighter: FighterId) -> Option<&T> {
        self.entries[fighter.index()].as_ref()
    }

    fn contains(&self, fighter: FighterId) -> bool {
        self.entries[fighter.index()].is_some()
    }

    fn is_empty(&self) -> bool {
        self.entries.iter().all(Option::is_none)
    }
}

const MAX_CONSUMED_HITBOX_SOURCES: usize = 32;

#[derive(Default)]
struct ConsumedHitboxSources {
    entries: [Option<SimEntityId>; MAX_CONSUMED_HITBOX_SOURCES],
}

impl ConsumedHitboxSources {
    fn insert(&mut self, source: SimEntityId) {
        if self.entries.iter().flatten().any(|entry| *entry == source) {
            return;
        }
        let slot = self
            .entries
            .iter_mut()
            .find(|entry| entry.is_none())
            .expect("consumed hitbox set covers the complete hitbox pool");
        *slot = Some(source);
    }

    fn iter(&self) -> impl Iterator<Item = SimEntityId> + '_ {
        self.entries.iter().flatten().copied()
    }
}

fn contact_payload_code(payload: Option<AttackPayloadId>) -> u16 {
    payload.map_or(u16::MAX, |payload| payload as u16)
}

fn contact_shape_code(shape: AttackShapeId) -> u16 {
    shape as u16
}

fn claim_record_precedes(left: ContactRecord, right: ContactRecord) -> bool {
    (
        left.payload_id,
        left.contact_ordinal,
        left.source,
        left.target,
    ) < (
        right.payload_id,
        right.contact_ordinal,
        right.source,
        right.target,
    )
}

fn dynamic_contact_source_kind_matches(
    source_kind: ContactSourceKind,
    entity_kind: SimEntityKind,
) -> bool {
    match source_kind {
        ContactSourceKind::FighterStrike => entity_kind == SimEntityKind::Hitbox,
        ContactSourceKind::ItemMeleeOrThrow => {
            matches!(entity_kind, SimEntityKind::Hitbox | SimEntityKind::Item)
        }
        ContactSourceKind::CharacterAbility => matches!(
            entity_kind,
            SimEntityKind::BeeSkill | SimEntityKind::ChickSkill | SimEntityKind::PenguinSkill
        ),
        ContactSourceKind::GenericSpecial => entity_kind == SimEntityKind::Special,
        ContactSourceKind::ArenaOrdnance => entity_kind == SimEntityKind::ArenaOrdnance,
        ContactSourceKind::PersistentArenaHazard => false,
    }
}

fn contact_source_is_valid(
    contact: ContactRecord,
    identities: &SimulationIdentityAllocator,
    stable_sources: &Query<&StableSimEntity>,
    active_arena: ActiveArena,
) -> bool {
    match contact.source {
        ContactSourceId::Entity(source) => {
            if !dynamic_contact_source_kind_matches(contact.source_kind, source.kind()) {
                return false;
            }
            identities
                .mapped_entity(source)
                .and_then(|entity| stable_sources.get(entity).ok())
                .is_some_and(|stable| stable.id() == source)
        }
        ContactSourceId::ArenaHazard {
            arena_index,
            hazard_index,
        } => {
            contact.source_kind == ContactSourceKind::PersistentArenaHazard
                && usize::from(arena_index) == active_arena.index()
                && usize::from(hazard_index) < active_arena.definition().hazards.len()
        }
    }
}

fn contact_event_source(source: ContactSourceId) -> SimEventSource {
    match source {
        ContactSourceId::Entity(entity) => SimEventSource::Entity(entity),
        ContactSourceId::ArenaHazard {
            arena_index,
            hazard_index,
        } => SimEventSource::ArenaHazard {
            arena_index,
            hazard_index,
        },
    }
}

pub fn begin_contact_collection(mut contact_buffer: ResMut<ContactBuffer>) {
    contact_buffer.begin_tick();
}

pub fn collect_hitbox_contacts(
    mut commands: Commands,
    mut identities: ResMut<SimulationIdentityAllocator>,
    state: Res<MatchState>,
    feel: Res<CombatFeelTuning>,
    character_catalog: Res<CharacterMoveCatalog>,
    active_arena: Res<ActiveArena>,
    mut contact_buffer: ResMut<ContactBuffer>,
    mut hitboxes: Query<(&StableSimEntity, &mut Hitbox, &mut SimPosition), Without<Fighter>>,
    fighters: Query<
        (
            &Fighter,
            &FighterCharacter,
            &FighterStats,
            &FighterMotor,
            &FighterActionState,
            &FighterGrabState,
            &FighterUltimateState,
            &FighterStyle,
            &FighterEquipment,
            &SimPosition,
        ),
        With<Fighter>,
    >,
    mut sim_events: ResMut<TickEventBuffer>,
    mut presentation_intents: Option<ResMut<CombatPresentationIntentJournal>>,
) {
    // Collection reads a single frozen fighter pose. No stats, action, guard,
    // grab relationship, hit memory, or source lifetime is mutated until every
    // hitbox has contributed all geometrically valid contacts.
    for index in 0..identities.capacity(SimEntityKind::Hitbox) {
        let Some((stable_id, hitbox_entity)) = identities.entry_at(SimEntityKind::Hitbox, index)
        else {
            continue;
        };
        let Ok((stable, mut hitbox, mut hitbox_position)) = hitboxes.get_mut(hitbox_entity) else {
            continue;
        };
        if stable.id() != stable_id {
            continue;
        }
        let Some((owner_translation, owner_size_multiplier)) = ({
            fighters
                .iter()
                .find(|(fighter, ..)| fighter.id == hitbox.owner.index())
                .map(|(_, _, stats, _, _, _, _, _, _, owner_transform)| {
                    (owner_transform.translation, stats.item_size_multiplier())
                })
        }) else {
            emit_attack_surface_despawn(
                &mut sim_events,
                presentation_intents.as_deref_mut(),
                stable.id(),
            );
            despawn_stable(&mut commands, &mut identities, hitbox_entity, *stable);
            continue;
        };
        refresh_hitbox_dimensions(&mut hitbox, Some(owner_size_multiplier));
        refresh_hitbox_position(
            &hitbox,
            &mut hitbox_position,
            Some(owner_translation),
            active_arena.definition(),
        );

        let profile = impact_profile_from_hitbox_with_feel(&hitbox, &feel);
        for target_id in FighterId::ALL {
            let Some((
                target,
                target_character,
                stats,
                motor,
                action,
                grab_state,
                ultimate_state,
                _,
                _,
                target_transform,
            )) = fighters
                .iter()
                .find(|(target, ..)| target.id == target_id.index())
            else {
                continue;
            };
            if target_id == hitbox.owner || hitbox.already_hit.contains(target_id) {
                continue;
            }
            if !state.combat_target_allowed_for_state(hitbox.owner.index(), target.id) {
                continue;
            }
            if !can_receive_impact(&stats, &action) {
                continue;
            }

            let hurt_box = fighter_hurt_box(
                target_transform,
                &motor,
                target_character,
                &stats,
                &character_catalog,
            );
            let Some(contact_point) =
                sphere_body_box_contact(hitbox_position.translation, hitbox.radius, hurt_box)
            else {
                continue;
            };

            let phase = if hitbox.kind == AttackKind::Grab {
                if grab_state.regrab_lockout.active() || grab_state.held_by.is_some() {
                    continue;
                }
                ContactPhase::Grab
            } else if hitbox.payload_id.is_some_and(payload_is_ultimate_catch) {
                ContactPhase::CinematicCatch
            } else {
                ContactPhase::Strike
            };
            let mut flags = 0;
            if phase.is_claim() {
                flags |= ContactFlags::SINGLE_USE_CLAIM;
            }
            let locked_final_victim = hitbox.payload_id.is_some_and(payload_is_ultimate_bomb)
                && ultimate_state.owner == Some(hitbox.owner)
                && action.action == FighterAction::UltimateVictim;
            let locked_scratch_victim = hitbox.payload_id.is_some_and(payload_is_ultimate_scratch)
                && ultimate_state.owner == Some(hitbox.owner)
                && action.action == FighterAction::UltimateVictim;
            if locked_final_victim {
                flags |= ContactFlags::LOCKED_FINAL_VICTIM;
            }
            if locked_scratch_victim {
                flags |= ContactFlags::LOCKED_SCRATCH_VICTIM;
            }
            if hitbox.kind == AttackKind::Jump {
                flags |= ContactFlags::JUMP_ATTACK;
            }

            let _ = contact_buffer.push(ContactRecord::new(
                phase,
                if matches!(hitbox.kind, AttackKind::ItemSwing | AttackKind::ItemThrow) {
                    ContactSourceKind::ItemMeleeOrThrow
                } else {
                    ContactSourceKind::FighterStrike
                },
                stable_id,
                Some(hitbox.owner),
                target_id,
                contact_payload_code(hitbox.payload_id),
                contact_shape_code(hitbox.shape_id),
                0,
                contact_point,
                hitbox_position.translation,
                profile,
                ContactFlags::from_bits(flags),
            ));
        }
    }
}

pub fn resolve_contacts(
    identities: Res<SimulationIdentityAllocator>,
    state: Res<MatchState>,
    character_catalog: Res<CharacterMoveCatalog>,
    active_arena: Res<ActiveArena>,
    mut contact_buffer: ResMut<ContactBuffer>,
    stable_sources: Query<&StableSimEntity>,
    mut fighters: ParamSet<(
        Query<
            (
                &Fighter,
                &FighterCharacter,
                &FighterStats,
                &FighterMotor,
                &FighterActionState,
                &FighterGrabState,
                &FighterUltimateState,
                &FighterStyle,
                &FighterEquipment,
                &SimPosition,
            ),
            With<Fighter>,
        >,
        Query<
            (
                &Fighter,
                &FighterCharacter,
                &mut FighterStats,
                &mut FighterMotor,
                &mut FighterActionState,
                &mut FighterGrabState,
                &mut FighterUltimateState,
                &FighterStyle,
                &FighterEquipment,
                &SimPosition,
            ),
            With<Fighter>,
        >,
    )>,
    mut hitstop: ResMut<Hitstop>,
    mut telemetry: ResMut<MatchTelemetry>,
    mut sim_events: ResMut<TickEventBuffer>,
    mut presentation_intents: Option<ResMut<CombatPresentationIntentJournal>>,
) {
    contact_buffer.sort_for_resolution();

    // Validate canonical source identity and target existence once before any
    // impact mutates fighter state. Dynamic generations fail closed; static
    // hazards must name the active immutable arena definition and an in-range
    // hazard index. Eligibility itself remains the frozen collector decision.
    let mut target_exists = [false; FighterId::ALL.len()];
    {
        let frozen_fighters = fighters.p0();
        for (fighter, ..) in &frozen_fighters {
            if let Some(fighter_id) = FighterId::from_index(fighter.id) {
                target_exists[fighter_id.index()] = true;
            }
        }
    }
    for contact_index in 0..contact_buffer.len() {
        let Some(contact) = contact_buffer.record(contact_index) else {
            continue;
        };
        if !target_exists[contact.target.index()]
            || !contact_source_is_valid(contact, &identities, &stable_sources, *active_arena)
        {
            contact_buffer.mark_outcome(contact_index, ContactOutcomeKind::Invalidated);
        }
    }

    let mut grabbed_victim_by_holder = FighterSlots::default();
    let mut ultimate_victim_by_attacker = FighterSlots::default();
    let mut ultimate_attacker_by_victim = FighterSlots::default();
    let mut ultimate_release_fighters = FighterSlots::default();
    let mut jump_attackers_landed = FighterSlots::default();
    let mut confirmed_attackers = FighterSlots::default();
    let mut penguin_slope_ultimate_recoil = FighterSlots::default();
    let mut ordinary_damage_participants = [false; FighterId::ALL.len()];

    // Resolve all frozen damaging contacts first. Eligibility is deliberately
    // not re-checked here: a reaction caused by an earlier record cannot erase
    // an already-collected trade or multi-hit contact.
    for contact_index in 0..contact_buffer.len() {
        let Some(contact) = contact_buffer.record(contact_index) else {
            continue;
        };
        if !contact.phase.has_impact() {
            continue;
        }
        if contact_buffer
            .outcome(contact_index)
            .is_some_and(|outcome| outcome.kind != ContactOutcomeKind::Pending)
        {
            continue;
        }
        let Some(impact) = contact.impact else {
            contact_buffer.mark_outcome(contact_index, ContactOutcomeKind::Invalidated);
            continue;
        };
        let source_facing = impact.knockback_direction.unwrap_or(Vec3::Z);

        let resolved = {
            let mut target_fighters = fighters.p1();
            let Some((
                _,
                _,
                mut stats,
                mut motor,
                mut action,
                _,
                mut ultimate_state,
                target_style,
                target_equipment,
                target_transform,
            )) = target_fighters
                .iter_mut()
                .find(|(target, ..)| target.id == contact.target.index())
            else {
                contact_buffer.mark_outcome(contact_index, ContactOutcomeKind::Invalidated);
                continue;
            };

            let outcome = apply_impact_core(
                &mut hitstop,
                &state,
                &mut stats,
                &mut motor,
                &mut action,
                target_transform.translation,
                Some(contact.contact_point.to_vec3()),
                contact.origin.to_vec3(),
                impact,
                DamageDefenderProfile::from_loadout(target_style, target_equipment),
                &mut telemetry,
            );
            let neutralize_victim_reaction_visual =
                contact.flags.contains(ContactFlags::LOCKED_SCRATCH_VICTIM)
                    || (!outcome.guarded && contact.phase == ContactPhase::CinematicCatch);
            let mut presentation_outcome = outcome;
            if neutralize_victim_reaction_visual {
                presentation_outcome.presentation.reaction_visual_side = 1.0;
            }
            if let Ok(event_id) = sim_events.emit(
                contact_event_source(contact.source),
                impact_sim_event_kind(outcome, contact.owner, contact.target),
            ) {
                contact_buffer.mark_event_id(contact_index, event_id);
                if let Some(intents) = presentation_intents.as_mut() {
                    let _ = intents.record(CombatPresentationIntent {
                        event_id,
                        victim: contact.target,
                        outcome: presentation_outcome,
                    });
                }
            }
            if contact.flags.contains(ContactFlags::LOCKED_SCRATCH_VICTIM) {
                action.action = FighterAction::UltimateVictim;
                action.elapsed.reset();
                action.reaction_getup_ms = None;
                action.reaction_recover_ms = None;
                action.reaction_family = None;
                motor.velocity = Vec3::ZERO;
            }
            if contact.flags.contains(ContactFlags::LOCKED_FINAL_VICTIM) {
                ultimate_state.owner = None;
            }
            outcome
        };

        contact_buffer.mark_outcome(
            contact_index,
            if resolved.guarded {
                ContactOutcomeKind::Guarded
            } else {
                ContactOutcomeKind::Accepted
            },
        );
        if contact.phase == ContactPhase::Strike && contact.has_authored_damage() {
            ordinary_damage_participants[contact.target.index()] = true;
            if let Some(owner) = contact.owner {
                ordinary_damage_participants[owner.index()] = true;
            }
        }
        if let Some(owner) = contact.owner {
            confirmed_attackers.insert_first(owner, ());
            let owner_translation = {
                let owners = fighters.p0();
                owners
                    .iter()
                    .find(|(fighter, ..)| fighter.id == owner.index())
                    .map(|(_, _, _, _, _, _, _, _, _, transform)| transform.translation)
            };
            if let Some(recoil_direction) = penguin_slope_ultimate_attacker_recoil_direction(
                impact.payload_id,
                resolved.guarded,
                owner_translation,
                Some(contact.contact_point.to_vec3()),
                source_facing,
            ) {
                penguin_slope_ultimate_recoil.insert_first(owner, recoil_direction);
            }
            if contact.flags.contains(ContactFlags::LOCKED_FINAL_VICTIM) {
                ultimate_release_fighters.insert_first(owner, ());
                ultimate_release_fighters.insert_first(contact.target, ());
            }
            if contact.flags.contains(ContactFlags::JUMP_ATTACK) {
                jump_attackers_landed.insert_first(owner, ());
            }
        }
    }

    // Status contacts are geometry-only. Their source-specific consumers apply
    // the authored status after the full batch resolves; central arbitration
    // only validates and records deterministic multi-target acceptance.
    for contact_index in 0..contact_buffer.len() {
        let Some(contact) = contact_buffer.record(contact_index) else {
            continue;
        };
        if contact.phase == ContactPhase::Status
            && contact_buffer
                .outcome(contact_index)
                .is_some_and(|outcome| outcome.kind == ContactOutcomeKind::Pending)
        {
            contact_buffer.mark_outcome(contact_index, ContactOutcomeKind::Accepted);
        }
    }

    // Relationship claims use a second canonical ordering: cinematic catches
    // before grabs, then holder ID and semantic source identity. A fixed
    // processed bitmap avoids an allocation even at the theoretical maximum.
    let mut processed_claims = [false; MAX_CONTACTS_PER_TICK];
    let mut claim_role_occupied = [false; FighterId::ALL.len()];
    for claim_phase in [ContactPhase::CinematicCatch, ContactPhase::Grab] {
        for holder in FighterId::ALL {
            loop {
                let mut best = None;
                for contact_index in 0..contact_buffer.len() {
                    if processed_claims[contact_index] {
                        continue;
                    }
                    let Some(candidate) = contact_buffer.record(contact_index) else {
                        continue;
                    };
                    if candidate.phase != claim_phase || candidate.owner != Some(holder) {
                        continue;
                    }
                    if best.is_none_or(|best_index| {
                        claim_record_precedes(candidate, contact_buffer.record(best_index).unwrap())
                    }) {
                        best = Some(contact_index);
                    }
                }
                let Some(contact_index) = best else {
                    break;
                };
                processed_claims[contact_index] = true;
                let contact = contact_buffer.record(contact_index).unwrap();
                if contact_buffer
                    .outcome(contact_index)
                    .is_some_and(|outcome| outcome.kind == ContactOutcomeKind::Invalidated)
                {
                    continue;
                }
                if claim_phase == ContactPhase::CinematicCatch
                    && contact_buffer
                        .outcome(contact_index)
                        .is_none_or(|outcome| outcome.kind != ContactOutcomeKind::Accepted)
                {
                    // Guarded catches already own their authored guarded-impact
                    // outcome and intentionally do not create a relationship.
                    continue;
                }
                if ordinary_damage_participants[holder.index()]
                    || ordinary_damage_participants[contact.target.index()]
                    || claim_role_occupied[holder.index()]
                    || claim_role_occupied[contact.target.index()]
                {
                    contact_buffer
                        .mark_outcome(contact_index, ContactOutcomeKind::RejectedByConflict);
                    continue;
                }

                claim_role_occupied[holder.index()] = true;
                claim_role_occupied[contact.target.index()] = true;
                if claim_phase == ContactPhase::CinematicCatch {
                    ultimate_victim_by_attacker.insert_first(holder, contact.target);
                    ultimate_attacker_by_victim.insert_first(contact.target, holder);
                } else {
                    let accepted = {
                        let mut target_fighters = fighters.p1();
                        target_fighters
                            .iter_mut()
                            .find(|(target, ..)| target.id == contact.target.index())
                            .map(
                                |(
                                    _,
                                    _,
                                    mut stats,
                                    mut motor,
                                    mut action,
                                    mut grab_state,
                                    _,
                                    _,
                                    _,
                                    _,
                                )| {
                                    grab_state.held_by = Some(holder);
                                    stats.last_attacker = Some(holder);
                                    stats.hud_flash = 0.16;
                                    motor.velocity = Vec3::ZERO;
                                    motor.clear_guard_counter_window();
                                    action.action = FighterAction::Grabbed;
                                    action.elapsed.reset();
                                    action.hitbox_spawned = false;
                                    action.queued_combo = false;
                                    action.queued_technique = None;
                                    action.queued_button = None;
                                    action.buffered_button = None;
                                    action.buffered_button_elapsed.reset();
                                    action.timeline_events_fired = 0;
                                    action.reaction_getup_ms = None;
                                    action.reaction_recover_ms = None;
                                    action.clear_reaction_visual();
                                },
                            )
                            .is_some()
                    };
                    if !accepted {
                        claim_role_occupied[holder.index()] = false;
                        claim_role_occupied[contact.target.index()] = false;
                        contact_buffer.mark_outcome(contact_index, ContactOutcomeKind::Invalidated);
                        continue;
                    }
                    grabbed_victim_by_holder.insert_first(holder, contact.target);
                    contact_buffer.mark_outcome(contact_index, ContactOutcomeKind::Accepted);
                }
            }
        }
    }

    if !grabbed_victim_by_holder.is_empty()
        || !ultimate_victim_by_attacker.is_empty()
        || !ultimate_release_fighters.is_empty()
        || !jump_attackers_landed.is_empty()
        || !confirmed_attackers.is_empty()
        || !penguin_slope_ultimate_recoil.is_empty()
    {
        let mut followup_fighters = fighters.p1();
        for (
            fighter,
            fighter_character,
            _,
            mut motor,
            mut action,
            mut grab_state,
            mut ultimate_state,
            style,
            equipment,
            _,
        ) in &mut followup_fighters
        {
            let Some(fighter_id) = FighterId::from_index(fighter.id) else {
                continue;
            };
            if confirmed_attackers.contains(fighter_id) {
                action.confirmed_hit = true;
            }
            if let Some(recoil_direction) = penguin_slope_ultimate_recoil.get(fighter_id) {
                apply_penguin_slope_ultimate_attacker_recoil(
                    &mut motor,
                    &mut action,
                    *recoil_direction,
                );
            }
            if let Some(victim) = ultimate_victim_by_attacker.get(fighter_id) {
                let loadout = LoadoutContext::for_character(
                    fighter_character.kind,
                    style.kind,
                    equipment.kind,
                );
                let technique = technique_slot_for_loadout(
                    CharacterMoveSlot::UltimateRush,
                    loadout,
                    &character_catalog,
                );
                ultimate_state.target = Some(*victim);
                action.action = FighterAction::UltimateRush;
                action.elapsed.reset();
                action.hitbox_spawned = true;
                action.technique_id = technique.map(|definition| definition.id);
                action.queued_combo = false;
                action.queued_technique = None;
                action.queued_button = None;
                action.buffered_button = None;
                action.buffered_button_elapsed.reset();
                action.timeline_events_fired = 0;
                action.reaction_getup_ms = None;
                action.reaction_recover_ms = None;
                action.reaction_family = None;
                motor.velocity *= 0.1;
            }
            if let Some(attacker) = ultimate_attacker_by_victim.get(fighter_id) {
                ultimate_state.owner = Some(*attacker);
                action.action = FighterAction::UltimateVictim;
                action.elapsed.reset();
                action.hitbox_spawned = false;
                action.queued_combo = false;
                action.queued_technique = None;
                action.queued_button = None;
                action.buffered_button = None;
                action.buffered_button_elapsed.reset();
                action.timeline_events_fired = 0;
                action.reaction_getup_ms = None;
                action.reaction_recover_ms = None;
                action.reaction_family = None;
                motor.velocity = Vec3::ZERO;
            }
            if ultimate_release_fighters.contains(fighter_id) {
                ultimate_state.target = None;
                ultimate_state.owner = None;
            }
            if let Some(victim) = grabbed_victim_by_holder.get(fighter_id) {
                grab_state.holding = Some(*victim);
                action.action = FighterAction::GrabHold;
                action.elapsed.reset();
                action.hitbox_spawned = true;
                action.queued_combo = false;
                action.queued_technique = None;
                action.queued_button = None;
                action.buffered_button = None;
                action.buffered_button_elapsed.reset();
                action.timeline_events_fired = 0;
                action.reaction_getup_ms = None;
                action.reaction_recover_ms = None;
                action.clear_reaction_visual();
                motor.velocity *= 0.2;
            }
            if jump_attackers_landed.contains(fighter_id) {
                motor.jump_attack_landing_recovery = false;
            }
        }
    }
}

/// Applies stable hitbox bookkeeping only after the shared contact batch has
/// resolved. Other source families consume the same outcome table in their own
/// post-resolution systems.
pub fn apply_hitbox_contact_outcomes(
    mut commands: Commands,
    mut identities: ResMut<SimulationIdentityAllocator>,
    mut contact_buffer: ResMut<ContactBuffer>,
    mut hitboxes: Query<(&StableSimEntity, &mut Hitbox), Without<Fighter>>,
    mut sim_events: ResMut<TickEventBuffer>,
    mut presentation_intents: Option<ResMut<CombatPresentationIntentJournal>>,
) {
    let mut consumed_sources = ConsumedHitboxSources::default();
    for contact_index in 0..contact_buffer.len() {
        let Some(contact) = contact_buffer.record(contact_index) else {
            continue;
        };
        let Some(source) = contact.source.entity() else {
            continue;
        };
        if source.kind() != SimEntityKind::Hitbox {
            continue;
        }
        let Some(source_entity) = identities.mapped_entity(source) else {
            contact_buffer.mark_outcome(contact_index, ContactOutcomeKind::Invalidated);
            continue;
        };
        let Ok((stable, mut hitbox)) = hitboxes.get_mut(source_entity) else {
            contact_buffer.mark_outcome(contact_index, ContactOutcomeKind::Invalidated);
            continue;
        };
        if stable.id() != source {
            contact_buffer.mark_outcome(contact_index, ContactOutcomeKind::Invalidated);
            continue;
        }
        let outcome = contact_buffer
            .outcome(contact_index)
            .map_or(ContactOutcomeKind::Invalidated, |outcome| outcome.kind);
        if matches!(
            outcome,
            ContactOutcomeKind::Accepted | ContactOutcomeKind::Guarded
        ) || (contact.phase.has_impact() && outcome == ContactOutcomeKind::RejectedByConflict)
        {
            hitbox.already_hit.insert(contact.target);
        }
        if contact.flags.contains(ContactFlags::SINGLE_USE_CLAIM) {
            consumed_sources.insert(source);
        }
    }

    for source in consumed_sources.iter() {
        let Some(entity) = identities.mapped_entity(source) else {
            continue;
        };
        let Ok((stable, _)) = hitboxes.get_mut(entity) else {
            continue;
        };
        if stable.id() != source {
            continue;
        }
        let stable = *stable;
        emit_attack_surface_despawn(&mut sim_events, presentation_intents.as_deref_mut(), source);
        despawn_stable(&mut commands, &mut identities, entity, stable);
    }
}

fn combat_event_victim(event: SimEvent) -> Option<FighterId> {
    match event.kind {
        SimEventKind::HitConfirmed { victim, .. } => Some(victim),
        SimEventKind::Guarded { defender, .. } => Some(defender),
        _ => None,
    }
}

#[derive(SystemParam)]
pub struct CommittedPresentationSidecars<'w> {
    fighter: Option<Res<'w, crate::fighter::FighterPresentationIntentJournal>>,
    item: Option<Res<'w, crate::items::ItemPresentationIntentJournal>>,
    arena: Option<Res<'w, crate::arena::ArenaPresentationIntentJournal>>,
    special: Option<Res<'w, crate::specials::SpecialPresentationIntentJournal>>,
    bee: Option<Res<'w, crate::bee_skills::BeePresentationIntentJournal>>,
    chick: Option<Res<'w, crate::chick_skills::ChickPresentationIntentJournal>>,
    penguin: Option<Res<'w, crate::penguin_skills::PenguinPresentationIntentJournal>>,
}

#[derive(SystemParam)]
pub struct CommittedCombatPresentation<'w> {
    effect_assets: Res<'w, EffectAssets>,
    visual_assets: Option<Res<'w, CombatVisualAssets>>,
    time: Option<Res<'w, Time>>,
    active_arena: Option<Res<'w, ActiveArena>>,
    hitstop: Option<Res<'w, Hitstop>>,
    effects: ResMut<'w, HitEffects>,
    sim_events: Res<'w, SimEventJournal>,
    intents: Res<'w, CombatPresentationIntentJournal>,
    authority_frontier:
        Option<Res<'w, crate::presentation_projection::PresentationAuthorityFrontier>>,
    announcements: Option<ResMut<'w, crate::game_state::MatchAnnouncements>>,
    cursor: ResMut<'w, PresentationEventCursor>,
    router: ResMut<'w, PresentationEventRouter>,
    dispatch_history: Option<ResMut<'w, CombatPresentationDispatchHistory>>,
}

type PresentationFighterQuery<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static Fighter,
        &'static mut FighterStats,
        &'static mut FighterMotor,
        &'static mut FighterActionState,
    ),
    With<Fighter>,
>;

type PresentationFighterOwnerQuery<'w, 's> = Query<
    'w,
    's,
    (&'static Fighter, &'static Transform, &'static FighterStats),
    (With<Fighter>, Without<HitboxSceneVisual>),
>;

fn action_cue_matches(event: SimEvent, fighter: FighterId) -> bool {
    matches!(
        event.kind,
        SimEventKind::ActionStarted {
            fighter: event_fighter,
            ..
        } if event_fighter == fighter
    )
}

#[allow(clippy::too_many_arguments)]
fn present_combat_cue(
    event: SimEvent,
    intent: CombatPresentationCueIntent,
    commands: &mut Commands,
    effect_assets: &EffectAssets,
    effects: &mut HitEffects,
    fighters: &mut PresentationFighterQuery,
) -> bool {
    match intent.kind {
        CombatPresentationCueKind::AttackSurfaceSpawn(surface) => matches!(
            event.kind,
            SimEventKind::EntitySpawned { entity } if entity == surface.entity
        ),
        CombatPresentationCueKind::AttackSurfaceDespawn { entity } => matches!(
            event.kind,
            SimEventKind::EntityDespawned {
                entity: event_entity
            } if event_entity == entity
        ),
        CombatPresentationCueKind::Loadout(cue) => {
            if !action_cue_matches(event, cue.fighter) {
                return false;
            }
            if let Some((_, _, mut stats, _, _)) = fighters
                .iter_mut()
                .find(|(_, fighter, ..)| fighter.id == cue.fighter.index())
            {
                stats.hud_flash = stats.hud_flash.max(cue.hud_flash);
            }
            let _ = cue.source;
            effects.push_feedback_cue(cue.cue, ImpactSource::MatchFlow, 26);
            true
        }
        CombatPresentationCueKind::TimelineFeedback(cue) => {
            if !action_cue_matches(event, cue.fighter) {
                return false;
            }
            effects.push_feedback_cue(cue.cue, ImpactSource::FighterStrike, cue.priority);
            effects.shake = effects.shake.max(cue.shake);
            if let Some((_, _, mut stats, _, _)) = fighters
                .iter_mut()
                .find(|(_, fighter, ..)| fighter.id == cue.fighter.index())
            {
                stats.hud_flash = stats.hud_flash.max(cue.hud_flash);
            }
            match cue.visual {
                Some(TimelineFeedbackVisual::Package(package)) => spawn_feedback_package(
                    commands,
                    effect_assets,
                    cue.position,
                    cue.direction,
                    package,
                ),
                Some(TimelineFeedbackVisual::FirePunch { side, palette }) => {
                    spawn_light_fire_punch(
                        commands,
                        effect_assets,
                        cue.position,
                        cue.direction,
                        side,
                        palette,
                    );
                }
                None => {}
            }
            true
        }
    }
}

fn install_attack_surface_visual(
    commands: &mut Commands,
    assets: &CombatVisualAssets,
    existing: Option<Entity>,
    event_id: SimEventId,
    surface: AttackSurfacePresentation,
) {
    let Some(scene_def) = surface.scene else {
        if let Some(existing) = existing {
            commands.entity(existing).despawn();
        }
        return;
    };
    let Some(scene) = assets.scene_for_path(scene_def.asset_path) else {
        return;
    };
    let bundle = (
        SceneRoot(scene),
        hitbox_scene_world_transform(
            surface.center,
            surface.spawn_origin,
            surface.facing,
            surface.range,
            scene_def,
        ),
        HitboxSceneVisual {
            spawn_tick: event_id.tick,
            surface,
            elapsed: 0.0,
            lifetime: hitbox_scene_visual_lifetime(surface.active_seconds),
            path_duration: hitbox_scene_path_duration(surface.active_seconds),
            range: surface.range,
        },
        Name::new("Food attack surface"),
    );
    if let Some(existing) = existing {
        commands.entity(existing).insert(bundle);
    } else {
        commands.spawn(bundle);
    }
}

fn reconcile_attack_surface_visuals(
    commands: &mut Commands,
    assets: Option<&CombatVisualAssets>,
    intents: &CombatPresentationIntentJournal,
    hitboxes: &Query<&StableSimEntity, With<Hitbox>>,
    visuals: &mut Query<
        (Entity, &mut HitboxSceneVisual, &mut Transform),
        (Without<Hitbox>, Without<Fighter>),
    >,
) {
    let Some(assets) = assets else {
        return;
    };
    for stable in hitboxes.iter() {
        let Some(intent) = intents.attack_surface(stable.id()) else {
            continue;
        };
        let CombatPresentationCueKind::AttackSurfaceSpawn(surface) = intent.kind else {
            continue;
        };
        let existing = visuals
            .iter_mut()
            .find(|(_, visual, _)| visual.surface.entity == stable.id())
            .map(|(entity, visual, _)| (entity, visual.surface));
        if existing.is_some_and(|(_, current)| current == surface) {
            continue;
        }
        install_attack_surface_visual(
            commands,
            assets,
            existing.map(|(entity, _)| entity),
            intent.event_id,
            surface,
        );
    }
}

fn update_hitbox_scene_visuals(
    commands: &mut Commands,
    delta_seconds: f32,
    arena: Option<&ArenaDefinition>,
    owners: &mut PresentationFighterOwnerQuery,
    visuals: &mut Query<
        (Entity, &mut HitboxSceneVisual, &mut Transform),
        (Without<Hitbox>, Without<Fighter>),
    >,
) {
    for (entity, mut visual, mut transform) in visuals.iter_mut() {
        visual.elapsed += delta_seconds;
        visual.lifetime -= delta_seconds;
        let progress = if visual.path_duration > 0.0 {
            (visual.elapsed / visual.path_duration).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let owner = owners
            .iter()
            .find(|(fighter, ..)| fighter.id == visual.surface.owner.index())
            .map(|(_, transform, stats)| (transform.translation, stats.item_size_multiplier()));
        let owner_translation = owner.map(|(translation, _)| translation);
        let owner_size_multiplier = owner.map(|(_, size)| size);
        let scale =
            hitbox_owner_size_scale(visual.surface.scales_with_owner_size, owner_size_multiplier);
        visual.range = visual.surface.base_range * scale;
        let base = if visual.surface.parented {
            owner_translation.unwrap_or(visual.surface.spawn_origin)
        } else {
            visual.surface.spawn_origin
        };
        if let Some(arena) = arena {
            let center = shape_center_with_ground_path_end(
                base,
                visual.surface.facing,
                visual.range,
                visual.surface.vertical_offset_scale,
                visual.surface.path,
                progress,
                visual.surface.ground_path_end,
                visual.surface.ground_path_clearance,
                arena,
            );
            *transform = hitbox_scene_world_transform(
                center,
                base,
                visual.surface.facing,
                visual.range,
                visual
                    .surface
                    .scene
                    .expect("spawned attack surface always has a scene"),
            );
        }
        if visual.lifetime <= 0.0 {
            commands.entity(entity).despawn();
        }
    }
}

/// Routes every committed simulation event observed since the prior render
/// frame through the game's presentation adapters at most once by stable event
/// ID. One cursor owns traversal so a stalled render frame cannot make combat
/// and fighter lifecycle consumers race or skip different fixed ticks.
pub fn present_committed_combat_events(
    mut commands: Commands,
    presentation: CommittedCombatPresentation,
    sidecars: CommittedPresentationSidecars,
    mut local_dispatch_history: Local<CombatPresentationDispatchHistory>,
    mut fighter_queries: ParamSet<(PresentationFighterQuery, PresentationFighterOwnerQuery)>,
    hitboxes: Query<&StableSimEntity, With<Hitbox>>,
    mut visuals: Query<
        (Entity, &mut HitboxSceneVisual, &mut Transform),
        (Without<Hitbox>, Without<Fighter>),
    >,
) {
    let CommittedCombatPresentation {
        effect_assets,
        visual_assets,
        time,
        active_arena,
        hitstop,
        mut effects,
        sim_events,
        intents: presentation_intents,
        authority_frontier,
        mut announcements,
        mut cursor,
        mut router,
        mut dispatch_history,
    } = presentation;
    // Offline/local rendering treats the newest locally committed tick as
    // confirmed. Online projection installs an explicit authority frontier, so
    // stock loss, results, and progression never become irreversible merely
    // because the predicted world reached that tick.
    if cursor.observed_through().is_none() {
        if let Some(history) = dispatch_history.as_deref_mut() {
            history.reset();
        } else {
            local_dispatch_history.reset();
        }
    }
    let confirmed_through = authority_frontier
        .as_deref()
        .and_then(|frontier| frontier.confirmed_through())
        .or_else(|| {
            authority_frontier
                .is_none()
                .then(|| sim_events.newest_tick())
                .flatten()
        });
    {
        let mut fighters = fighter_queries.p0();
        let _ = cursor.route_available(&sim_events, &mut router, confirmed_through, |event| {
            if crate::game_state::present_match_event(
                event,
                announcements.as_deref_mut(),
                &mut effects,
            ) {
                return;
            }
            if let Some(intent) = presentation_intents.cue(event.id) {
                let is_new = dispatch_history.as_deref_mut().map_or_else(
                    || local_dispatch_history.mark_if_new(event.id),
                    |history| history.mark_if_new(event.id),
                );
                if is_new {
                    let _ = present_combat_cue(
                        event,
                        intent,
                        &mut commands,
                        &effect_assets,
                        &mut effects,
                        &mut fighters,
                    );
                }
                return;
            }
            if let Some(intent) = sidecars
                .fighter
                .as_ref()
                .and_then(|intents| intents.get(event.id))
                && crate::fighter::present_fighter_lifecycle_event(
                    event,
                    intent,
                    &mut commands,
                    &effect_assets,
                    &mut effects,
                    announcements.as_deref_mut(),
                )
            {
                return;
            }

            if let Some(intent) = sidecars
                .item
                .as_ref()
                .and_then(|intents| intents.get(event.id))
                && crate::items::present_item_lifecycle_event(
                    event,
                    intent,
                    &mut commands,
                    &effect_assets,
                    &mut effects,
                    announcements.as_deref_mut(),
                    &mut fighters,
                )
            {
                return;
            }

            if let Some(intent) = sidecars
                .special
                .as_ref()
                .and_then(|intents| intents.get(event.id))
            {
                crate::specials::present_special_event(
                    event,
                    intent,
                    &mut commands,
                    &effect_assets,
                    &mut effects,
                );
            }

            if let Some(intent) = sidecars
                .bee
                .as_ref()
                .and_then(|intents| intents.get(event.id))
            {
                crate::bee_skills::present_bee_event(
                    event,
                    intent,
                    &mut commands,
                    &effect_assets,
                    &mut effects,
                );
            }

            if let Some(intent) = sidecars
                .chick
                .as_ref()
                .and_then(|intents| intents.get(event.id))
            {
                let result = crate::chick_skills::present_chick_event(
                    event,
                    intent,
                    &mut commands,
                    &effect_assets,
                    &mut effects,
                );
                if let Some((fighter_id, intensity)) = result.hud_flash
                    && let Some((_, _, mut stats, _, _)) = fighters
                        .iter_mut()
                        .find(|(_, fighter, ..)| fighter.id == fighter_id.index())
                {
                    stats.hud_flash = stats.hud_flash.max(intensity);
                }
            }

            if let Some(intent) = sidecars
                .penguin
                .as_ref()
                .and_then(|intents| intents.get(event.id))
            {
                let result = crate::penguin_skills::present_penguin_event(
                    event,
                    intent,
                    &mut commands,
                    &effect_assets,
                    &mut effects,
                );
                if let Some((fighter_id, intensity)) = result.hud_flash
                    && let Some((_, _, mut stats, _, _)) = fighters
                        .iter_mut()
                        .find(|(_, fighter, ..)| fighter.id == fighter_id.index())
                {
                    stats.hud_flash = stats.hud_flash.max(intensity);
                }
            }

            if let Some(intent) = sidecars
                .arena
                .as_ref()
                .and_then(|intents| intents.get(event.id))
            {
                let Some(event_victim) = combat_event_victim(event) else {
                    return;
                };
                if intent.victim != event_victim {
                    return;
                }
                let Some((fighter_entity, _, mut stats, mut motor, mut action)) = fighters
                    .iter_mut()
                    .find(|(_, fighter, ..)| fighter.id == event_victim.index())
                else {
                    return;
                };
                crate::arena::present_arena_impact_accent(
                    &mut commands,
                    &effect_assets,
                    fighter_entity,
                    intent,
                );
                apply_impact_presentation(
                    &mut commands,
                    &effect_assets,
                    &mut effects,
                    &mut stats,
                    &mut motor,
                    &mut action,
                    intent.outcome,
                );
                return;
            }

            let Some(event_victim) = combat_event_victim(event) else {
                return;
            };
            let Some(intent) = presentation_intents.get(event.id) else {
                return;
            };
            // A stale or mismatched sidecar can never redirect a semantic hit
            // to another fighter.
            if intent.victim != event_victim {
                return;
            }
            let Some((_, _, mut stats, mut motor, mut action)) = fighters
                .iter_mut()
                .find(|(_, fighter, ..)| fighter.id == event_victim.index())
            else {
                return;
            };
            apply_impact_presentation(
                &mut commands,
                &effect_assets,
                &mut effects,
                &mut stats,
                &mut motor,
                &mut action,
                intent.outcome,
            );
        });
    }

    reconcile_attack_surface_visuals(
        &mut commands,
        visual_assets.as_deref(),
        &presentation_intents,
        &hitboxes,
        &mut visuals,
    );

    let delta_seconds = if hitstop.as_deref().is_some_and(|hitstop| hitstop.active()) {
        0.0
    } else {
        time.as_deref()
            .map_or(SIM_DT_SECONDS, |time| time.delta_secs())
    };
    let mut owners = fighter_queries.p1();
    update_hitbox_scene_visuals(
        &mut commands,
        delta_seconds,
        active_arena.as_deref().map(|arena| (*arena).definition()),
        &mut owners,
        &mut visuals,
    );
}

#[cfg(test)]
mod tests {
    use bevy::ecs::world::CommandQueue;

    use super::*;

    #[test]
    fn fixed_attack_snapshot_collection_reports_overflow_without_growing() {
        let mut values = ArrayVec::<_, 1>::new();

        assert_eq!(try_push_fixed(&mut values, 7_u8, "test"), Ok(()));
        assert_eq!(
            try_push_fixed(&mut values, 9_u8, "test"),
            Err(FixedCollectionOverflow {
                collection: "test",
                capacity: 1,
            })
        );
        assert_eq!(values.as_slice(), &[7]);
    }

    fn profile_for_test(guardable: bool) -> ImpactProfile {
        impact_profile(
            9,
            ImpactSource::FighterStrike,
            10.0,
            6.0,
            2.0,
            false,
            guardable,
            12.0,
            ImpactFeedbackIntensity::Light,
            ReactionFamilyId::ShortStandingStagger,
        )
    }

    fn assert_canonical_impact_state_eq(
        left_stats: &FighterStats,
        right_stats: &FighterStats,
        left_motor: &FighterMotor,
        right_motor: &FighterMotor,
        left_action: &FighterActionState,
        right_action: &FighterActionState,
        left_hitstop: &Hitstop,
        right_hitstop: &Hitstop,
        left_telemetry: &MatchTelemetry,
        right_telemetry: &MatchTelemetry,
    ) {
        assert_eq!(left_stats.health, right_stats.health);
        assert_eq!(left_stats.stamina, right_stats.stamina);
        assert_eq!(left_stats.score, right_stats.score);
        assert_eq!(left_stats.last_attacker, right_stats.last_attacker);
        assert_eq!(left_stats.invulnerability, right_stats.invulnerability);
        assert_eq!(
            left_stats.health_refill_timer,
            right_stats.health_refill_timer
        );
        assert_eq!(left_stats.respawn_timer, right_stats.respawn_timer);
        assert_eq!(left_stats.element_carry, right_stats.element_carry);
        assert_eq!(
            left_stats.element_carry_strength,
            right_stats.element_carry_strength
        );
        assert_eq!(
            left_stats.element_carry_timer,
            right_stats.element_carry_timer
        );
        assert_eq!(left_stats.item_speed_timer, right_stats.item_speed_timer);
        assert_eq!(left_stats.item_giant_timer, right_stats.item_giant_timer);

        assert_eq!(left_motor.velocity, right_motor.velocity);
        assert_eq!(left_motor.facing, right_motor.facing);
        assert_eq!(left_motor.grounded, right_motor.grounded);
        assert_eq!(left_motor.knockdown_on_land, right_motor.knockdown_on_land);
        assert_eq!(left_motor.landing_aftermath, right_motor.landing_aftermath);
        assert_eq!(left_motor.air_attack_used, right_motor.air_attack_used);
        assert_eq!(left_motor.queued_air_attack, right_motor.queued_air_attack);
        assert_eq!(
            left_motor.queued_air_attack_timer,
            right_motor.queued_air_attack_timer
        );
        assert_eq!(
            left_motor.jump_attack_landing_recovery,
            right_motor.jump_attack_landing_recovery
        );
        assert_eq!(
            left_motor.bee_air_dash_motion_active,
            right_motor.bee_air_dash_motion_active
        );
        assert_eq!(
            left_motor.bee_air_dash_shot_available,
            right_motor.bee_air_dash_shot_available
        );
        assert_eq!(left_motor.ledge_grace_timer, right_motor.ledge_grace_timer);
        assert_eq!(
            left_motor.landing_stick_timer,
            right_motor.landing_stick_timer
        );
        assert_eq!(
            left_motor.jump_takeoff_timer,
            right_motor.jump_takeoff_timer
        );
        assert_eq!(left_motor.reaction_bounces, right_motor.reaction_bounces);
        assert_eq!(
            left_motor.pig_air_meat_slam_air_hits,
            right_motor.pig_air_meat_slam_air_hits
        );
        assert_eq!(left_motor.dash_slide_timer, right_motor.dash_slide_timer);
        assert_eq!(
            left_motor.dash_jump_carry_timer,
            right_motor.dash_jump_carry_timer
        );
        assert_eq!(
            left_motor.dash_jump_carry_speed_limit,
            right_motor.dash_jump_carry_speed_limit
        );
        assert_eq!(
            left_motor.impact_speed_limit_timer,
            right_motor.impact_speed_limit_timer
        );
        assert_eq!(
            left_motor.impact_speed_limit,
            right_motor.impact_speed_limit
        );
        assert_eq!(
            left_motor.penguin_ice_slide_direction,
            right_motor.penguin_ice_slide_direction
        );
        assert_eq!(
            left_motor.penguin_ice_slide_speed,
            right_motor.penguin_ice_slide_speed
        );
        assert_eq!(
            left_motor.guard_active_timer,
            right_motor.guard_active_timer
        );
        assert_eq!(
            left_motor.guard_cooldown_timer,
            right_motor.guard_cooldown_timer
        );
        assert_eq!(
            left_motor.guard_start_buffer_timer,
            right_motor.guard_start_buffer_timer
        );
        assert_eq!(
            left_motor.guard_was_requested,
            right_motor.guard_was_requested
        );
        assert_eq!(
            left_motor.guard_counter_window_timer,
            right_motor.guard_counter_window_timer
        );
        assert_eq!(
            left_motor.guard_counter_source,
            right_motor.guard_counter_source
        );
        assert_eq!(
            left_motor.guard_counter_buffered,
            right_motor.guard_counter_buffered
        );

        assert_eq!(left_action.action, right_action.action);
        assert_eq!(left_action.elapsed, right_action.elapsed);
        assert_eq!(left_action.hitbox_spawned, right_action.hitbox_spawned);
        assert_eq!(left_action.queued_combo, right_action.queued_combo);
        assert_eq!(left_action.queued_technique, right_action.queued_technique);
        assert_eq!(left_action.queued_button, right_action.queued_button);
        assert_eq!(left_action.buffered_button, right_action.buffered_button);
        assert_eq!(
            left_action.buffered_button_elapsed,
            right_action.buffered_button_elapsed
        );
        assert_eq!(left_action.confirmed_hit, right_action.confirmed_hit);
        assert_eq!(left_action.technique_id, right_action.technique_id);
        assert_eq!(
            left_action.cancel_window_open,
            right_action.cancel_window_open
        );
        assert_eq!(
            left_action.branch_window_open,
            right_action.branch_window_open
        );
        assert_eq!(
            left_action.timeline_events_fired,
            right_action.timeline_events_fired
        );
        assert_eq!(
            left_action.reaction_getup_ms,
            right_action.reaction_getup_ms
        );
        assert_eq!(
            left_action.reaction_recover_ms,
            right_action.reaction_recover_ms
        );
        assert_eq!(left_action.reaction_family, right_action.reaction_family);
        assert_eq!(left_action.charge_elapsed, right_action.charge_elapsed);
        assert_eq!(
            left_action.charge_release_requested,
            right_action.charge_release_requested
        );

        assert_eq!(left_hitstop.remaining_ticks, right_hitstop.remaining_ticks);
        assert_eq!(left_telemetry.replay_seed, right_telemetry.replay_seed);
        assert_eq!(left_telemetry.ring_outs, right_telemetry.ring_outs);
        assert_eq!(left_telemetry.falls, right_telemetry.falls);
        assert_eq!(left_telemetry.item_hits, right_telemetry.item_hits);
        assert_eq!(left_telemetry.throws, right_telemetry.throws);
        assert_eq!(left_telemetry.guard_breaks, right_telemetry.guard_breaks);
        assert_eq!(
            left_telemetry.damage_by_fighter,
            right_telemetry.damage_by_fighter
        );
    }

    fn impact_parity_components(
        guarded: bool,
    ) -> (
        FighterStats,
        FighterMotor,
        FighterActionState,
        Hitstop,
        MatchTelemetry,
    ) {
        (
            FighterStats {
                health: 83.0,
                stamina: 67.0,
                hud_flash: 0.91,
                element_carry: Some(DamageElement::Wind),
                element_carry_strength: 0.21,
                element_carry_timer: TickTimer::from_seconds_ceil(0.7),
                ..default()
            },
            FighterMotor {
                velocity: Vec3::new(0.4, 0.0, -0.2),
                facing: Vec3::Z,
                grounded: true,
                guard_counter_buffered: true,
                ..default()
            },
            FighterActionState {
                action: if guarded {
                    FighterAction::Guarding
                } else {
                    FighterAction::Moving
                },
                elapsed: ElapsedTicks::from_ticks(4),
                hitbox_spawned: true,
                queued_combo: true,
                reaction_family: Some(ReactionFamilyId::LightAirPop),
                reaction_visual_side: -1.0,
                ..default()
            },
            Hitstop::default(),
            MatchTelemetry::default(),
        )
    }

    fn assert_wrapper_matches_core_for_impact(guarded: bool) {
        let state = MatchState::default();
        let target_position = SimPosition::new(Vec3::new(2.0, ARENA_TOP_Y, -3.0));
        let origin = target_position.translation + Vec3::Z * 1.5;
        let contact = Some(target_position.translation + Vec3::Y * 0.8);
        let mut profile = impact_profile(
            1,
            ImpactSource::ItemThrow,
            13.0,
            7.0,
            2.5,
            false,
            true,
            18.0,
            ImpactFeedbackIntensity::Heavy,
            ReactionFamilyId::MediumStandingStagger,
        );
        profile.element = DamageElement::Shock;

        let (
            mut wrapper_stats,
            mut wrapper_motor,
            mut wrapper_action,
            mut wrapper_hitstop,
            mut wrapper_telemetry,
        ) = impact_parity_components(guarded);
        let (mut core_stats, mut core_motor, mut core_action, mut core_hitstop, mut core_telemetry) =
            impact_parity_components(guarded);

        let world = World::new();
        let mut command_queue = CommandQueue::default();
        let mut effects = HitEffects::default();
        {
            let mut commands = Commands::new(&mut command_queue, &world);
            apply_impact(
                &mut commands,
                &EffectAssets::default(),
                &mut effects,
                &mut wrapper_hitstop,
                &state,
                &mut wrapper_stats,
                &mut wrapper_motor,
                &mut wrapper_action,
                &target_position,
                contact,
                origin,
                profile,
                DamageDefenderProfile::default(),
                &mut wrapper_telemetry,
            );
        }

        let outcome = apply_impact_core(
            &mut core_hitstop,
            &state,
            &mut core_stats,
            &mut core_motor,
            &mut core_action,
            target_position.translation,
            contact,
            origin,
            profile,
            DamageDefenderProfile::default(),
            &mut core_telemetry,
        );

        assert_eq!(outcome.guarded, guarded);
        assert_canonical_impact_state_eq(
            &wrapper_stats,
            &core_stats,
            &wrapper_motor,
            &core_motor,
            &wrapper_action,
            &core_action,
            &wrapper_hitstop,
            &core_hitstop,
            &wrapper_telemetry,
            &core_telemetry,
        );
        assert_eq!(wrapper_stats.hud_flash, outcome.presentation.hud_flash);
        assert_eq!(
            wrapper_action.reaction_visual_side,
            outcome.presentation.reaction_visual_side
        );
        assert_eq!(core_stats.hud_flash, 0.91);
        assert_eq!(core_action.reaction_visual_side, -1.0);
        assert_eq!(effects.shake, outcome.presentation.camera_shake);
        assert_eq!(effects.drain_combat_sfx_cues().len(), 1);
    }

    #[test]
    fn guarded_impact_wrapper_preserves_authoritative_core_results() {
        assert_wrapper_matches_core_for_impact(true);
    }

    #[test]
    fn unguarded_impact_wrapper_preserves_authoritative_core_results() {
        assert_wrapper_matches_core_for_impact(false);
    }

    #[test]
    fn headless_impact_core_resolves_without_commands_assets_or_hit_effects() {
        let state = MatchState::default();
        let target_position = SimPosition::new(Vec3::new(-1.0, ARENA_TOP_Y, 2.0));
        let (mut stats, mut motor, mut action, mut hitstop, mut telemetry) =
            impact_parity_components(false);
        let profile = impact_profile(
            2,
            ImpactSource::Projectile,
            8.0,
            4.0,
            1.0,
            false,
            false,
            0.0,
            ImpactFeedbackIntensity::Light,
            ReactionFamilyId::ShortStandingStagger,
        );

        let outcome = apply_impact_core(
            &mut hitstop,
            &state,
            &mut stats,
            &mut motor,
            &mut action,
            target_position.translation,
            None,
            Vec3::ZERO,
            profile,
            DamageDefenderProfile::default(),
            &mut telemetry,
        );

        assert!(!outcome.guarded);
        assert!(outcome.committed_damage > 0.0);
        assert!(stats.health < 83.0);
        assert!(hitstop.active());
        assert!(telemetry.damage_by_fighter[2] > 0.0);
        assert_eq!(stats.hud_flash, 0.91);
        assert_eq!(action.reaction_visual_side, -1.0);
    }

    fn presentation_test_outcome() -> ImpactOutcome {
        let state = MatchState::default();
        let mut stats = FighterStats::default();
        let mut motor = FighterMotor {
            facing: Vec3::Z,
            grounded: true,
            ..default()
        };
        let mut action = FighterActionState {
            action: FighterAction::Moving,
            ..default()
        };
        let mut hitstop = Hitstop::default();
        let mut telemetry = MatchTelemetry::default();
        apply_impact_core(
            &mut hitstop,
            &state,
            &mut stats,
            &mut motor,
            &mut action,
            Vec3::new(1.0, ARENA_TOP_Y, 2.0),
            Some(Vec3::new(1.0, ARENA_TOP_Y + 0.7, 2.0)),
            Vec3::new(1.0, ARENA_TOP_Y, 0.0),
            impact_profile(
                0,
                ImpactSource::FighterStrike,
                9.0,
                5.0,
                1.5,
                false,
                false,
                0.0,
                ImpactFeedbackIntensity::Light,
                ReactionFamilyId::ShortStandingStagger,
            ),
            DamageDefenderProfile::default(),
            &mut telemetry,
        )
    }

    fn presentation_test_event(
        tick: u64,
        source_index: u32,
        victim: FighterId,
        outcome: ImpactOutcome,
    ) -> SimEvent {
        SimEvent {
            id: SimEventId {
                tick: crate::simulation::SimTick(tick),
                source: SimEventSource::Entity(crate::determinism::SimEntityId::new(
                    SimEntityKind::Hitbox,
                    source_index,
                    1,
                )),
                ordinal: 0,
            },
            kind: impact_sim_event_kind(outcome, Some(FighterId::ZERO), victim),
        }
    }

    fn target_presentation_components() -> (FighterStats, FighterMotor, FighterActionState) {
        (
            FighterStats {
                hud_flash: 0.91,
                ..default()
            },
            FighterMotor::default(),
            FighterActionState {
                reaction_visual_side: -1.0,
                ..default()
            },
        )
    }

    fn presentation_test_app(fighter_ids: &[FighterId]) -> App {
        let mut app = App::new();
        app.insert_resource(EffectAssets::presentation_enabled_for_test())
            .insert_resource(HitEffects::default())
            .insert_resource(SimEventJournal::default())
            .insert_resource(CombatPresentationIntentJournal::default())
            .insert_resource(CombatPresentationDispatchHistory::default())
            .insert_resource(PresentationEventCursor::default())
            .insert_resource(PresentationEventRouter::default())
            .add_systems(Update, present_committed_combat_events);
        for fighter_id in fighter_ids {
            let (stats, motor, action) = target_presentation_components();
            app.world_mut().spawn((
                Fighter {
                    id: fighter_id.index(),
                    name: "Presentation target",
                    color: Color::WHITE,
                    spawn: Vec3::ZERO,
                },
                stats,
                motor,
                action,
            ));
        }
        app
    }

    fn visual_effect_kinds(world: &mut World) -> Vec<crate::effects::EffectKind> {
        let mut effects = world.query::<&crate::effects::VisualEffect>();
        effects.iter(world).map(|effect| effect.kind).collect()
    }

    fn commit_presentation_test_event(app: &mut App, event: SimEvent, outcome: ImpactOutcome) {
        let mut buffer = TickEventBuffer::new(event.id.tick);
        let emitted = buffer.emit(event.id.source, event.kind).unwrap();
        assert_eq!(emitted, event.id);
        app.world_mut()
            .resource_mut::<SimEventJournal>()
            .commit(&buffer);
        let victim = combat_event_victim(event).unwrap();
        app.world_mut()
            .resource_mut::<CombatPresentationIntentJournal>()
            .record(CombatPresentationIntent {
                event_id: event.id,
                victim,
                outcome,
            })
            .unwrap();
    }

    fn loadout_cue_test_event(tick: u64) -> (SimEvent, CombatPresentationCueIntent) {
        let fighter = FighterId::ZERO;
        let event = SimEvent {
            id: SimEventId {
                tick: crate::simulation::SimTick(tick),
                source: SimEventSource::Fighter(fighter),
                ordinal: 0,
            },
            kind: SimEventKind::ActionStarted {
                fighter,
                action_id: crate::live_snapshot::technique_code(TechniqueId::CatLight1),
            },
        };
        let intent = CombatPresentationCueIntent {
            event_id: event.id,
            kind: CombatPresentationCueKind::Loadout(LoadoutPresentationCue {
                fighter,
                source: LoadoutModifierSource::Equipment(EquipmentKind::DashCoil),
                cue: "equip_dash_coil",
                hud_flash: 0.22,
            }),
        };
        (event, intent)
    }

    fn commit_cue_test_event(app: &mut App, event: SimEvent, intent: CombatPresentationCueIntent) {
        let mut buffer = TickEventBuffer::new(event.id.tick);
        assert_eq!(buffer.emit(event.id.source, event.kind).unwrap(), event.id);
        app.world_mut()
            .resource_mut::<SimEventJournal>()
            .commit(&buffer);
        app.world_mut()
            .resource_mut::<CombatPresentationIntentJournal>()
            .record_cue(intent)
            .unwrap();
    }

    #[test]
    fn routed_impact_matches_the_compatibility_presentation_adapter() {
        let outcome = presentation_test_outcome();
        let victim = FighterId::new(1).unwrap();

        let (mut expected_stats, mut expected_motor, mut expected_action) =
            target_presentation_components();
        let mut expected_effects = HitEffects::default();
        let mut expected_world = World::new();
        let mut expected_queue = CommandQueue::default();
        let expected_assets = EffectAssets::presentation_enabled_for_test();
        {
            let mut commands = Commands::new(&mut expected_queue, &expected_world);
            apply_impact_presentation(
                &mut commands,
                &expected_assets,
                &mut expected_effects,
                &mut expected_stats,
                &mut expected_motor,
                &mut expected_action,
                outcome,
            );
        }
        expected_queue.apply(&mut expected_world);

        let mut app = presentation_test_app(&[victim]);
        let event = presentation_test_event(40, 3, victim, outcome);
        commit_presentation_test_event(&mut app, event, outcome);
        app.update();

        let (actual_hud_flash, actual_reaction_side) = {
            let world = app.world_mut();
            let mut fighters = world.query::<(&Fighter, &FighterStats, &FighterActionState)>();
            let (_, stats, action) = fighters
                .iter(world)
                .find(|(fighter, ..)| fighter.id == victim.index())
                .unwrap();
            (stats.hud_flash, action.reaction_visual_side)
        };
        assert_eq!(actual_hud_flash, expected_stats.hud_flash);
        assert_eq!(actual_reaction_side, expected_action.reaction_visual_side);

        let actual_effects = app.world_mut().resource_mut::<HitEffects>();
        assert_eq!(actual_effects.shake, expected_effects.shake);
        assert_eq!(actual_effects.last_cue, expected_effects.last_cue);
        assert_eq!(actual_effects.last_reaction, expected_effects.last_reaction);
        assert_eq!(
            actual_effects.combat_sfx_cues,
            expected_effects.combat_sfx_cues
        );
        drop(actual_effects);
        assert_eq!(
            visual_effect_kinds(app.world_mut()),
            visual_effect_kinds(&mut expected_world)
        );
    }

    #[test]
    fn one_render_update_routes_impacts_from_two_committed_ticks() {
        let outcome = presentation_test_outcome();
        let victim = FighterId::new(1).unwrap();
        let mut app = presentation_test_app(&[victim]);
        for (tick, source) in [(70, 4), (71, 5)] {
            let event = presentation_test_event(tick, source, victim, outcome);
            commit_presentation_test_event(&mut app, event, outcome);
        }

        app.update();

        assert_eq!(
            app.world().resource::<HitEffects>().combat_sfx_cues.len(),
            2
        );
        let cursor = app.world().resource::<PresentationEventCursor>();
        assert_eq!(cursor.metrics().observed_ticks, 2);
        assert_eq!(cursor.metrics().observed_events, 2);
    }

    #[test]
    fn rollback_resimulation_does_not_replay_consumed_impact_presentation() {
        let outcome = presentation_test_outcome();
        let victim = FighterId::new(1).unwrap();
        let event = presentation_test_event(90, 7, victim, outcome);
        let mut app = presentation_test_app(&[victim]);
        commit_presentation_test_event(&mut app, event, outcome);
        app.update();
        let presented_effect_count = visual_effect_kinds(app.world_mut()).len();
        assert!(presented_effect_count > 0);
        assert_eq!(
            app.world().resource::<HitEffects>().combat_sfx_cues.len(),
            1
        );

        let retained = crate::simulation::SimTick(89);
        app.world_mut()
            .resource_mut::<PresentationEventCursor>()
            .discard_after(retained);
        app.world_mut()
            .resource_mut::<PresentationEventRouter>()
            .discard_after(retained);
        app.world_mut()
            .resource_mut::<SimEventJournal>()
            .discard_after(retained);
        app.world_mut()
            .resource_mut::<CombatPresentationIntentJournal>()
            .discard_after(retained);
        commit_presentation_test_event(&mut app, event, outcome);
        app.update();

        assert_eq!(
            app.world().resource::<HitEffects>().combat_sfx_cues.len(),
            1
        );
        assert_eq!(
            visual_effect_kinds(app.world_mut()).len(),
            presented_effect_count
        );
        assert_eq!(
            app.world()
                .resource::<PresentationEventRouter>()
                .metrics()
                .duplicate_events_suppressed,
            1
        );
    }

    #[test]
    fn rollback_resimulation_deduplicates_action_timeline_cues() {
        let (event, intent) = loadout_cue_test_event(96);
        let mut app = presentation_test_app(&[FighterId::ZERO]);
        commit_cue_test_event(&mut app, event, intent);
        app.update();
        assert_eq!(
            app.world()
                .resource::<CombatPresentationDispatchHistory>()
                .metrics(),
            CombatPresentationDispatchMetrics {
                presented: 1,
                duplicates_suppressed: 0,
                capacity_rejections: 0,
            }
        );

        let retained = crate::simulation::SimTick(95);
        app.world_mut()
            .resource_mut::<PresentationEventCursor>()
            .discard_after(retained);
        app.world_mut()
            .resource_mut::<PresentationEventRouter>()
            .discard_after(retained);
        app.world_mut()
            .resource_mut::<SimEventJournal>()
            .discard_after(retained);
        app.world_mut()
            .resource_mut::<CombatPresentationIntentJournal>()
            .discard_after(retained);
        commit_cue_test_event(&mut app, event, intent);
        app.update();

        assert_eq!(
            app.world()
                .resource::<CombatPresentationDispatchHistory>()
                .metrics(),
            CombatPresentationDispatchMetrics {
                presented: 1,
                duplicates_suppressed: 1,
                capacity_rejections: 0,
            }
        );
    }

    #[test]
    fn combat_cue_sidecars_discard_future_ticks_and_regenerate_exactly() {
        let (_, retained) = loadout_cue_test_event(100);
        let (_, discarded) = loadout_cue_test_event(101);
        let mut intents = CombatPresentationIntentJournal::default();
        intents.record_cue(retained).unwrap();
        intents.record_cue(discarded).unwrap();
        assert_eq!(intents.cue_len(), 2);

        intents.discard_after(crate::simulation::SimTick(100));
        assert_eq!(intents.cue_len(), 1);
        assert_eq!(intents.cue(retained.event_id), Some(retained));
        assert_eq!(intents.cue(discarded.event_id), None);
        assert_eq!(intents.metrics().cue_discarded, 1);

        intents.record_cue(discarded).unwrap();
        assert_eq!(intents.cue(discarded.event_id), Some(discarded));
        assert_eq!(intents.cue_len(), 2);
    }

    #[test]
    fn combat_cue_dedup_history_rejects_unrepresentable_ids_fail_closed() {
        let mut history = CombatPresentationDispatchHistory::default();
        let invalid = SimEventId {
            tick: crate::simulation::SimTick(102),
            source: SimEventSource::Match,
            ordinal: MAX_SIM_EVENTS_PER_TICK as u16,
        };

        assert!(!history.mark_if_new(invalid));
        assert_eq!(
            history.metrics(),
            CombatPresentationDispatchMetrics {
                presented: 0,
                duplicates_suppressed: 0,
                capacity_rejections: 1,
            }
        );
    }

    #[test]
    fn missing_or_mismatched_stable_victim_drops_only_presentation() {
        let outcome = presentation_test_outcome();
        let existing = FighterId::ZERO;
        let missing = FighterId::new(1).unwrap();
        let mut app = presentation_test_app(&[existing]);

        let missing_event = presentation_test_event(100, 8, missing, outcome);
        commit_presentation_test_event(&mut app, missing_event, outcome);

        let mismatched_event = presentation_test_event(101, 9, missing, outcome);
        let mut buffer = TickEventBuffer::new(mismatched_event.id.tick);
        assert_eq!(
            buffer
                .emit(mismatched_event.id.source, mismatched_event.kind)
                .unwrap(),
            mismatched_event.id
        );
        app.world_mut()
            .resource_mut::<SimEventJournal>()
            .commit(&buffer);
        app.world_mut()
            .resource_mut::<CombatPresentationIntentJournal>()
            .record(CombatPresentationIntent {
                event_id: mismatched_event.id,
                victim: existing,
                outcome,
            })
            .unwrap();

        app.update();

        assert!(
            app.world()
                .resource::<HitEffects>()
                .combat_sfx_cues
                .is_empty()
        );
        let world = app.world_mut();
        let mut fighters = world.query::<(&Fighter, &FighterStats)>();
        let (_, stats) = fighters
            .iter(world)
            .find(|(fighter, ..)| fighter.id == existing.index())
            .unwrap();
        assert_eq!(stats.hud_flash, 0.91);
    }

    #[test]
    fn combat_presentation_intent_storage_is_bounded_and_fail_closed() {
        let outcome = presentation_test_outcome();
        let tick = crate::simulation::SimTick(120);
        let mut intents = CombatPresentationIntentJournal::default();
        for ordinal in 0..MAX_SIM_EVENTS_PER_TICK {
            intents
                .record(CombatPresentationIntent {
                    event_id: SimEventId {
                        tick,
                        source: SimEventSource::Match,
                        ordinal: ordinal as u16,
                    },
                    victim: FighterId::ZERO,
                    outcome,
                })
                .unwrap();
        }
        assert_eq!(intents.len(), MAX_SIM_EVENTS_PER_TICK);
        assert_eq!(
            intents.capacity(),
            SIM_EVENT_HISTORY_TICKS * MAX_SIM_EVENTS_PER_TICK
        );
        assert_eq!(
            intents.record(CombatPresentationIntent {
                event_id: SimEventId {
                    tick,
                    source: SimEventSource::Match,
                    ordinal: MAX_SIM_EVENTS_PER_TICK as u16,
                },
                victim: FighterId::ZERO,
                outcome,
            }),
            Err(EventEmitError::CapacityExceeded {
                capacity: MAX_SIM_EVENTS_PER_TICK,
            })
        );
        assert_eq!(intents.len(), MAX_SIM_EVENTS_PER_TICK);
        assert_eq!(intents.metrics().rejected, 1);

        let mut canonical_events = TickEventBuffer::new(tick);
        assert!(
            canonical_events
                .emit(
                    SimEventSource::Match,
                    SimEventKind::FighterRespawned {
                        fighter: FighterId::ZERO,
                    },
                )
                .is_ok()
        );
        assert_eq!(canonical_events.len(), 1);
    }

    struct StableHitboxEventFixture {
        event: Option<crate::sim_event::SimEvent>,
        stable_hitbox: crate::determinism::SimEntityId,
        local_hitbox: Entity,
        target_health: f32,
        target_hud_flash: f32,
        target_reaction_visual_side: f32,
        intent_count: usize,
        pending_sfx_count: usize,
        visual_effect_count: usize,
        overflow_count: u32,
    }

    fn run_stable_hitbox_event_fixture(
        guarded: bool,
        reverse_ecs_allocation: bool,
        saturate_event_buffer: bool,
        with_presentation_sidecar: bool,
    ) -> StableHitboxEventFixture {
        const STATIONARY_PATH: [[f32; 3]; 1] = [[0.0, 0.0, 0.0]];

        let mut app = App::new();
        app.insert_resource(MatchState::default())
            .insert_resource(CombatFeelTuning::default())
            .insert_resource(ContactBuffer::default())
            .insert_resource(CharacterMoveCatalog::default())
            .insert_resource(ActiveArena::default())
            .insert_resource(EffectAssets::default())
            .insert_resource(HitEffects::default())
            .insert_resource(Hitstop::default())
            .insert_resource(MatchTelemetry::default());
        if with_presentation_sidecar {
            app.insert_resource(CombatPresentationIntentJournal::default());
        }

        let mut event_buffer = TickEventBuffer::new(crate::simulation::SimTick(77));
        if saturate_event_buffer {
            for statistic_id in 0..crate::sim_event::MAX_SIM_EVENTS_PER_TICK {
                event_buffer
                    .emit(
                        SimEventSource::Match,
                        SimEventKind::Statistic {
                            fighter: FighterId::ZERO,
                            statistic_id: statistic_id as u16,
                            delta: 1,
                        },
                    )
                    .unwrap();
            }
        }
        app.insert_resource(event_buffer);

        if reverse_ecs_allocation {
            app.world_mut().spawn_empty();
        }

        let spawn_fighter = |world: &mut World, fighter_id: FighterId, target_is_guarding: bool| {
            let is_target = fighter_id == FighterId::new(1).unwrap();
            let position = if is_target {
                Vec3::new(0.0, ARENA_TOP_Y, 0.5)
            } else {
                Vec3::new(0.0, ARENA_TOP_Y, -2.0)
            };
            world.spawn((
                Fighter {
                    id: fighter_id.index(),
                    name: if is_target { "Target" } else { "Owner" },
                    color: Color::WHITE,
                    spawn: position,
                },
                FighterCharacter::new(CharacterKind::Cat),
                FighterStats {
                    hud_flash: if is_target { 0.91 } else { 0.0 },
                    ..default()
                },
                FighterMotor {
                    facing: if is_target { Vec3::NEG_Z } else { Vec3::Z },
                    ..default()
                },
                FighterActionState {
                    action: if is_target && target_is_guarding {
                        FighterAction::Guarding
                    } else {
                        FighterAction::Moving
                    },
                    reaction_visual_side: if is_target { -1.0 } else { 1.0 },
                    ..default()
                },
                FighterGrabState::default(),
                FighterUltimateState::default(),
                FighterStyle {
                    kind: FighterStyleKind::Anchor,
                },
                FighterEquipment::new(EquipmentKind::DashCoil),
                SimPosition::new(position),
            ));
        };

        if reverse_ecs_allocation {
            spawn_fighter(app.world_mut(), FighterId::new(1).unwrap(), guarded);
            spawn_fighter(app.world_mut(), FighterId::ZERO, guarded);
        } else {
            spawn_fighter(app.world_mut(), FighterId::ZERO, guarded);
            spawn_fighter(app.world_mut(), FighterId::new(1).unwrap(), guarded);
        }

        let mut hitbox = hitbox_for_payload(AttackPayloadId::KiriageBeat1, Vec3::Z);
        hitbox.base_radius = 10.0;
        hitbox.radius = 10.0;
        hitbox.spawn_origin = Vec3::new(0.0, ARENA_TOP_Y + 0.5, 0.0);
        hitbox.base_range = 0.0;
        hitbox.range = 0.0;
        hitbox.vertical_offset_scale = 0.0;
        hitbox.parented = false;
        hitbox.path = &STATIONARY_PATH;

        let hitbox_entity = app
            .world_mut()
            .spawn((hitbox, SimPosition::new(Vec3::ZERO)))
            .id();
        let mut identities = SimulationIdentityAllocator::default();
        let stable_hitbox = identities
            .try_allocate(SimEntityKind::Hitbox, hitbox_entity)
            .unwrap();
        app.world_mut()
            .entity_mut(hitbox_entity)
            .insert(stable_hitbox);
        app.insert_resource(identities);
        app.add_systems(
            Update,
            (
                begin_contact_collection,
                collect_hitbox_contacts,
                resolve_contacts,
                apply_hitbox_contact_outcomes,
            )
                .chain(),
        );
        app.update();

        let stable_hitbox = stable_hitbox.id();
        let (event, overflow_count) = {
            let buffer = app.world().resource::<TickEventBuffer>();
            assert_eq!(
                buffer.len(),
                if saturate_event_buffer {
                    crate::sim_event::MAX_SIM_EVENTS_PER_TICK
                } else {
                    1
                }
            );
            (
                buffer
                    .iter()
                    .find(|event| event.id.source == SimEventSource::Entity(stable_hitbox))
                    .copied(),
                buffer.overflow_count(),
            )
        };
        let (target_health, target_hud_flash, target_reaction_visual_side) = {
            let world = app.world_mut();
            let mut fighters = world.query::<(&Fighter, &FighterStats, &FighterActionState)>();
            fighters
                .iter(world)
                .find(|(fighter, ..)| fighter.id == 1)
                .map(|(_, stats, action)| {
                    (stats.health, stats.hud_flash, action.reaction_visual_side)
                })
                .unwrap()
        };
        let intent_count = app
            .world()
            .get_resource::<CombatPresentationIntentJournal>()
            .map_or(0, CombatPresentationIntentJournal::len);
        let pending_sfx_count = app.world().resource::<HitEffects>().combat_sfx_cues.len();
        let visual_effect_count = {
            let world = app.world_mut();
            let mut effects = world.query::<&crate::effects::VisualEffect>();
            effects.iter(world).count()
        };

        StableHitboxEventFixture {
            event,
            stable_hitbox,
            local_hitbox: hitbox_entity,
            target_health,
            target_hud_flash,
            target_reaction_visual_side,
            intent_count,
            pending_sfx_count,
            visual_effect_count,
            overflow_count,
        }
    }

    #[test]
    fn stable_hitboxes_emit_semantic_guarded_and_hit_events() {
        let guarded = run_stable_hitbox_event_fixture(true, false, false, false);
        let hit = run_stable_hitbox_event_fixture(false, false, false, false);

        assert_eq!(
            guarded.event.unwrap().kind,
            SimEventKind::Guarded {
                attacker: Some(FighterId::ZERO),
                defender: FighterId::new(1).unwrap(),
            }
        );
        assert!(matches!(
            hit.event.unwrap().kind,
            SimEventKind::HitConfirmed {
                attacker: Some(FighterId::ZERO),
                victim,
                damage_q,
                reaction: ReactionFamilyId::LauncherDown,
            } if victim == FighterId::new(1).unwrap() && damage_q > 0
        ));
    }

    #[test]
    fn stable_hitbox_event_id_survives_resimulation_and_reversed_ecs_allocation() {
        let first = run_stable_hitbox_event_fixture(false, false, false, false);
        let resimulated = run_stable_hitbox_event_fixture(false, false, false, false);
        let reversed = run_stable_hitbox_event_fixture(false, true, false, false);

        let first_event = first.event.unwrap();
        assert_eq!(first_event, resimulated.event.unwrap());
        assert_eq!(first_event, reversed.event.unwrap());
        assert_eq!(first_event.id.tick, crate::simulation::SimTick(77));
        assert_eq!(
            first_event.id.source,
            SimEventSource::Entity(first.stable_hitbox)
        );
        assert_ne!(first.local_hitbox, reversed.local_hitbox);
        assert_eq!(first.stable_hitbox, reversed.stable_hitbox);
    }

    #[test]
    fn hitbox_event_overflow_does_not_undo_authoritative_impact() {
        let saturated = run_stable_hitbox_event_fixture(false, false, true, true);

        assert!(saturated.event.is_none());
        assert_eq!(saturated.overflow_count, 1);
        assert!(saturated.target_health < MAX_HEALTH);
        assert_eq!(saturated.intent_count, 0);
    }

    #[test]
    fn ordinary_hitbox_resolution_never_presents_inline_or_in_headless_mode() {
        let headless = run_stable_hitbox_event_fixture(false, false, false, false);
        let rendered_fixed_stage = run_stable_hitbox_event_fixture(false, false, false, true);

        for fixture in [&headless, &rendered_fixed_stage] {
            assert!(fixture.target_health < MAX_HEALTH);
            assert_eq!(fixture.target_hud_flash, 0.91);
            assert_eq!(fixture.target_reaction_visual_side, -1.0);
            assert_eq!(fixture.pending_sfx_count, 0);
            assert_eq!(fixture.visual_effect_count, 0);
        }
        assert_eq!(headless.intent_count, 0);
        assert_eq!(rendered_fixed_stage.intent_count, 1);
    }

    #[derive(Clone, Copy)]
    struct ArbitrationHitboxSpec {
        owner: FighterId,
        payload: AttackPayloadId,
        origin: Vec3,
        reaction: Option<ReactionFamilyId>,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct ArbitrationFighterState {
        health_q: i32,
        stamina_q: i32,
        action: FighterAction,
        reaction: Option<ReactionFamilyId>,
        holding: Option<FighterId>,
        held_by: Option<FighterId>,
        ultimate_target: Option<FighterId>,
        ultimate_owner: Option<FighterId>,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct ArbitrationFixtureResult {
        fighters: [Option<ArbitrationFighterState>; FighterId::ALL.len()],
        events: Vec<(SimEventSource, SimEventKind)>,
        outcomes: Vec<ContactOutcomeKind>,
    }

    fn run_arbitration_fixture(
        fighter_positions: &[(FighterId, Vec3)],
        hitbox_specs: &[ArbitrationHitboxSpec],
        reverse_ecs_allocation: bool,
        guarding_fighter: Option<(FighterId, f32)>,
    ) -> ArbitrationFixtureResult {
        const STATIONARY_PATH: [[f32; 3]; 1] = [[0.0, 0.0, 0.0]];

        let mut app = App::new();
        let mut match_state = MatchState::default();
        match_state.rules = crate::game_state::RULE_PRESETS[1];
        match_state.rule_index = 1;
        let mut active_slots = [false; FighterId::ALL.len()];
        for (fighter_id, _) in fighter_positions {
            active_slots[fighter_id.index()] = true;
        }
        match_state.set_active_slots(active_slots);
        app.insert_resource(match_state)
            .insert_resource(CombatFeelTuning::default())
            .insert_resource(CharacterMoveCatalog::default())
            .insert_resource(ActiveArena::default())
            .insert_resource(Hitstop::default())
            .insert_resource(MatchTelemetry::default())
            .insert_resource(ContactBuffer::default())
            .insert_resource(TickEventBuffer::new(crate::simulation::SimTick(91)));

        let spawn_fighter = |world: &mut World, fighter_id: FighterId, position: Vec3| {
            let guarding = guarding_fighter.is_some_and(|(guard, _)| guard == fighter_id);
            let stamina = guarding_fighter
                .filter(|(guard, _)| *guard == fighter_id)
                .map_or(MAX_STAMINA, |(_, stamina)| stamina);
            world.spawn((
                Fighter {
                    id: fighter_id.index(),
                    name: "Arbitration Fighter",
                    color: Color::WHITE,
                    spawn: position,
                },
                FighterCharacter::new(CharacterKind::Cat),
                FighterStats {
                    stamina,
                    ..default()
                },
                FighterMotor {
                    grounded: true,
                    facing: if guarding { Vec3::NEG_Z } else { Vec3::Z },
                    ..default()
                },
                FighterActionState {
                    action: if guarding {
                        FighterAction::Guarding
                    } else {
                        FighterAction::Moving
                    },
                    ..default()
                },
                FighterGrabState::default(),
                FighterUltimateState::default(),
                FighterStyle {
                    kind: FighterStyleKind::Anchor,
                },
                FighterEquipment::new(EquipmentKind::DashCoil),
                SimPosition::new(position),
            ));
        };
        if reverse_ecs_allocation {
            for (fighter_id, position) in fighter_positions.iter().copied().rev() {
                spawn_fighter(app.world_mut(), fighter_id, position);
            }
        } else {
            for (fighter_id, position) in fighter_positions.iter().copied() {
                spawn_fighter(app.world_mut(), fighter_id, position);
            }
        }

        let mut source_entities = vec![None; hitbox_specs.len()];
        let spawn_source = |world: &mut World, spec: ArbitrationHitboxSpec| {
            let mut hitbox = hitbox_for_payload(spec.payload, Vec3::Z);
            hitbox.owner = spec.owner;
            if let Some(reaction) = spec.reaction {
                hitbox.reaction_family = reaction;
            }
            hitbox.base_radius = 0.62;
            hitbox.radius = 0.62;
            hitbox.spawn_origin = spec.origin;
            hitbox.base_range = 0.0;
            hitbox.range = 0.0;
            hitbox.vertical_offset_scale = 0.0;
            hitbox.parented = false;
            hitbox.path = &STATIONARY_PATH;
            hitbox.scales_with_owner_size = false;
            world.spawn((hitbox, SimPosition::new(spec.origin))).id()
        };
        if reverse_ecs_allocation {
            for index in (0..hitbox_specs.len()).rev() {
                source_entities[index] = Some(spawn_source(app.world_mut(), hitbox_specs[index]));
            }
        } else {
            for index in 0..hitbox_specs.len() {
                source_entities[index] = Some(spawn_source(app.world_mut(), hitbox_specs[index]));
            }
        }

        // Stable IDs are assigned in semantic specification order even when
        // unrelated ECS allocation is reversed. Local Entity identity can
        // therefore never become an arbitration tie-break.
        let mut identities = SimulationIdentityAllocator::default();
        for source_entity in source_entities.into_iter().flatten() {
            let stable = identities
                .try_allocate(SimEntityKind::Hitbox, source_entity)
                .unwrap();
            app.world_mut().entity_mut(source_entity).insert(stable);
        }
        app.insert_resource(identities);
        app.add_systems(
            Update,
            (
                begin_contact_collection,
                collect_hitbox_contacts,
                resolve_contacts,
                apply_hitbox_contact_outcomes,
            )
                .chain(),
        );
        app.update();

        let mut fighter_states = [None; FighterId::ALL.len()];
        {
            let world = app.world_mut();
            let mut fighters = world.query::<(
                &Fighter,
                &FighterStats,
                &FighterActionState,
                &FighterGrabState,
                &FighterUltimateState,
            )>();
            for (fighter, stats, action, grab, ultimate) in fighters.iter(world) {
                let fighter_id = FighterId::from_index(fighter.id).unwrap();
                fighter_states[fighter_id.index()] = Some(ArbitrationFighterState {
                    health_q: quantize_f32(stats.health, DEFAULT_F32_QUANTIZATION),
                    stamina_q: quantize_f32(stats.stamina, DEFAULT_F32_QUANTIZATION),
                    action: action.action,
                    reaction: action.reaction_family,
                    holding: grab.holding,
                    held_by: grab.held_by,
                    ultimate_target: ultimate.target,
                    ultimate_owner: ultimate.owner,
                });
            }
        }
        let events = app
            .world()
            .resource::<TickEventBuffer>()
            .iter()
            .map(|event| (event.id.source, event.kind))
            .collect();
        let buffer = app.world().resource::<ContactBuffer>();
        let outcomes = (0..buffer.len())
            .map(|index| buffer.outcome(index).unwrap().kind)
            .collect();
        ArbitrationFixtureResult {
            fighters: fighter_states,
            events,
            outcomes,
        }
    }

    fn assert_reversed_arbitration_fixture(
        fighter_positions: &[(FighterId, Vec3)],
        hitbox_specs: &[ArbitrationHitboxSpec],
    ) -> ArbitrationFixtureResult {
        let forward = run_arbitration_fixture(fighter_positions, hitbox_specs, false, None);
        let reversed = run_arbitration_fixture(fighter_positions, hitbox_specs, true, None);
        assert_eq!(forward, reversed);
        forward
    }

    #[test]
    fn typed_static_hazard_and_status_sources_validate_before_resolution() {
        let arena_index = crate::arena_defs::arena_definitions()
            .iter()
            .position(|arena| !arena.hazards.is_empty())
            .expect("fixture requires one authored hazard arena");
        let active_arena = ActiveArena::new(arena_index);
        let source = ContactSourceId::ArenaHazard {
            arena_index: u16::try_from(arena_index).unwrap(),
            hazard_index: 0,
        };
        let mut contacts = ContactBuffer::default();
        let impact = impact_profile(
            0,
            ImpactSource::Hazard,
            10.0,
            4.0,
            2.0,
            false,
            true,
            3.0,
            ImpactFeedbackIntensity::Light,
            ReactionFamilyId::ShortStandingStagger,
        );
        assert_eq!(
            contacts.push(ContactRecord::new(
                ContactPhase::Strike,
                ContactSourceKind::PersistentArenaHazard,
                source,
                None,
                FighterId::ZERO,
                10,
                0,
                0,
                Vec3::ZERO,
                Vec3::NEG_Z,
                impact,
                ContactFlags::default(),
            )),
            crate::contact_arbitration::ContactInsertResult::Inserted
        );
        assert_eq!(
            contacts.push(ContactRecord::new_status(
                ContactSourceKind::PersistentArenaHazard,
                source,
                None,
                FighterId::ZERO,
                11,
                0,
                1,
                Vec3::ZERO,
                Vec3::NEG_Z,
                ContactFlags::default(),
            )),
            crate::contact_arbitration::ContactInsertResult::Inserted
        );
        assert_eq!(
            contacts.push(ContactRecord::new_status(
                ContactSourceKind::PersistentArenaHazard,
                ContactSourceId::ArenaHazard {
                    arena_index: u16::try_from(arena_index).unwrap(),
                    hazard_index: u16::MAX,
                },
                None,
                FighterId::ZERO,
                12,
                0,
                2,
                Vec3::ZERO,
                Vec3::NEG_Z,
                ContactFlags::default(),
            )),
            crate::contact_arbitration::ContactInsertResult::Inserted
        );

        let mut app = App::new();
        app.insert_resource(MatchState::default())
            .insert_resource(CharacterMoveCatalog::default())
            .insert_resource(active_arena)
            .insert_resource(contacts)
            .insert_resource(SimulationIdentityAllocator::default())
            .insert_resource(Hitstop::default())
            .insert_resource(MatchTelemetry::default())
            .insert_resource(TickEventBuffer::new(crate::simulation::SimTick(103)));
        app.world_mut().spawn((
            Fighter {
                id: FighterId::ZERO.index(),
                name: "Static hazard target",
                color: Color::WHITE,
                spawn: Vec3::ZERO,
            },
            FighterCharacter::new(CharacterKind::Cat),
            FighterStats::default(),
            FighterMotor::default(),
            FighterActionState::default(),
            FighterGrabState::default(),
            FighterUltimateState::default(),
            FighterStyle {
                kind: FighterStyleKind::Anchor,
            },
            FighterEquipment::new(EquipmentKind::DashCoil),
            SimPosition::default(),
        ));
        app.add_systems(Update, resolve_contacts);
        app.update();

        let buffer = app.world().resource::<ContactBuffer>();
        let outcome_for_payload = |payload_id| {
            (0..buffer.len())
                .find(|index| buffer.record(*index).unwrap().payload_id == payload_id)
                .and_then(|index| buffer.outcome(index))
                .unwrap()
        };
        assert_eq!(outcome_for_payload(10).kind, ContactOutcomeKind::Accepted);
        assert_eq!(outcome_for_payload(11).kind, ContactOutcomeKind::Accepted);
        assert!(outcome_for_payload(11).event_id.is_none());
        assert_eq!(
            outcome_for_payload(12).kind,
            ContactOutcomeKind::Invalidated
        );
        let events = app.world().resource::<TickEventBuffer>();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events.iter().next().unwrap().id.source,
            SimEventSource::ArenaHazard {
                arena_index: u16::try_from(arena_index).unwrap(),
                hazard_index: 0,
            }
        );
    }

    #[test]
    fn frozen_hitbox_contacts_allow_a_real_two_strike_trade() {
        let fighter_zero = Vec3::new(-1.0, ARENA_TOP_Y, 0.0);
        let fighter_one = Vec3::new(1.0, ARENA_TOP_Y, 0.0);
        let result = assert_reversed_arbitration_fixture(
            &[
                (FighterId::ZERO, fighter_zero),
                (FighterId::new(1).unwrap(), fighter_one),
            ],
            &[
                ArbitrationHitboxSpec {
                    owner: FighterId::ZERO,
                    payload: AttackPayloadId::KiriageBeat1,
                    origin: fighter_one + Vec3::Y * 0.58,
                    reaction: Some(ReactionFamilyId::ShortStandingStagger),
                },
                ArbitrationHitboxSpec {
                    owner: FighterId::new(1).unwrap(),
                    payload: AttackPayloadId::KiriageBeat1,
                    origin: fighter_zero + Vec3::Y * 0.58,
                    reaction: Some(ReactionFamilyId::ShortStandingStagger),
                },
            ],
        );

        assert!(
            result.fighters[0].unwrap().health_q
                < quantize_f32(MAX_HEALTH, DEFAULT_F32_QUANTIZATION)
        );
        assert!(
            result.fighters[1].unwrap().health_q
                < quantize_f32(MAX_HEALTH, DEFAULT_F32_QUANTIZATION)
        );
        assert_eq!(result.events.len(), 2);
        assert_eq!(result.outcomes, vec![ContactOutcomeKind::Accepted; 2]);
    }

    #[test]
    fn strongest_authored_reaction_wins_final_state_after_all_damage_commits() {
        let target = Vec3::new(0.0, ARENA_TOP_Y, 0.0);
        let result = assert_reversed_arbitration_fixture(
            &[
                (FighterId::ZERO, Vec3::new(-4.0, ARENA_TOP_Y, 0.0)),
                (FighterId::new(1).unwrap(), target),
                (FighterId::new(2).unwrap(), Vec3::new(4.0, ARENA_TOP_Y, 0.0)),
            ],
            &[
                ArbitrationHitboxSpec {
                    owner: FighterId::new(2).unwrap(),
                    payload: AttackPayloadId::KiriageBeat1,
                    origin: target + Vec3::Y * 0.58,
                    reaction: Some(ReactionFamilyId::GroundBounceDown),
                },
                ArbitrationHitboxSpec {
                    owner: FighterId::ZERO,
                    payload: AttackPayloadId::KiriageBeat1,
                    origin: target + Vec3::Y * 0.58,
                    reaction: Some(ReactionFamilyId::ShortStandingStagger),
                },
            ],
        );

        let target_state = result.fighters[1].unwrap();
        assert_eq!(
            target_state.reaction,
            Some(ReactionFamilyId::GroundBounceDown),
            "canonical events: {:?}",
            result.events,
        );
        assert_eq!(result.events.len(), 2);
        assert!(target_state.health_q < quantize_f32(MAX_HEALTH - 10.0, DEFAULT_F32_QUANTIZATION));
    }

    #[test]
    fn same_tick_strike_interrupts_grab_independent_of_ecs_order() {
        let victim = Vec3::new(0.0, ARENA_TOP_Y, 0.0);
        let result = assert_reversed_arbitration_fixture(
            &[
                (FighterId::ZERO, Vec3::new(-4.0, ARENA_TOP_Y, 0.0)),
                (FighterId::new(1).unwrap(), victim),
                (FighterId::new(2).unwrap(), Vec3::new(4.0, ARENA_TOP_Y, 0.0)),
            ],
            &[
                ArbitrationHitboxSpec {
                    owner: FighterId::ZERO,
                    payload: AttackPayloadId::GrabCatch,
                    origin: victim + Vec3::Y * 0.58,
                    reaction: None,
                },
                ArbitrationHitboxSpec {
                    owner: FighterId::new(2).unwrap(),
                    payload: AttackPayloadId::KiriageBeat1,
                    origin: victim + Vec3::Y * 0.58,
                    reaction: Some(ReactionFamilyId::ShortStandingStagger),
                },
            ],
        );

        assert_eq!(result.fighters[0].unwrap().holding, None);
        assert_eq!(result.fighters[1].unwrap().held_by, None);
        assert_eq!(result.fighters[1].unwrap().action, FighterAction::Hitstun);
        assert!(
            result
                .outcomes
                .contains(&ContactOutcomeKind::RejectedByConflict)
        );
    }

    #[test]
    fn competing_grabs_choose_lower_holder_and_one_role_cannot_chain_claims() {
        let first_victim = Vec3::new(0.0, ARENA_TOP_Y, 0.0);
        let second_victim = Vec3::new(4.0, ARENA_TOP_Y, 0.0);
        let result = assert_reversed_arbitration_fixture(
            &[
                (FighterId::ZERO, Vec3::new(-4.0, ARENA_TOP_Y, 0.0)),
                (FighterId::new(1).unwrap(), first_victim),
                (FighterId::new(2).unwrap(), second_victim),
            ],
            &[
                ArbitrationHitboxSpec {
                    owner: FighterId::ZERO,
                    payload: AttackPayloadId::GrabCatch,
                    origin: first_victim + Vec3::Y * 0.58,
                    reaction: None,
                },
                // Fighter 1 is first a victim, then attempts to be a holder.
                ArbitrationHitboxSpec {
                    owner: FighterId::new(1).unwrap(),
                    payload: AttackPayloadId::GrabCatch,
                    origin: second_victim + Vec3::Y * 0.58,
                    reaction: None,
                },
            ],
        );

        assert_eq!(
            result.fighters[0].unwrap().holding,
            Some(FighterId::new(1).unwrap())
        );
        assert_eq!(result.fighters[1].unwrap().held_by, Some(FighterId::ZERO));
        assert_eq!(result.fighters[1].unwrap().holding, None);
        assert_eq!(result.fighters[2].unwrap().held_by, None);
        assert!(
            result
                .outcomes
                .contains(&ContactOutcomeKind::RejectedByConflict)
        );
    }

    #[test]
    fn two_grabs_for_one_victim_choose_the_lower_holder_id() {
        let victim = Vec3::new(0.0, ARENA_TOP_Y, 0.0);
        let result = assert_reversed_arbitration_fixture(
            &[
                (FighterId::ZERO, Vec3::new(-4.0, ARENA_TOP_Y, 0.0)),
                (FighterId::new(1).unwrap(), Vec3::new(4.0, ARENA_TOP_Y, 0.0)),
                (FighterId::new(2).unwrap(), victim),
            ],
            &[
                ArbitrationHitboxSpec {
                    owner: FighterId::new(1).unwrap(),
                    payload: AttackPayloadId::GrabCatch,
                    origin: victim + Vec3::Y * 0.58,
                    reaction: None,
                },
                ArbitrationHitboxSpec {
                    owner: FighterId::ZERO,
                    payload: AttackPayloadId::GrabCatch,
                    origin: victim + Vec3::Y * 0.58,
                    reaction: None,
                },
            ],
        );

        assert_eq!(
            result.fighters[0].unwrap().holding,
            Some(FighterId::new(2).unwrap())
        );
        assert_eq!(result.fighters[1].unwrap().holding, None);
        assert_eq!(result.fighters[2].unwrap().held_by, Some(FighterId::ZERO));
        assert_eq!(
            result
                .outcomes
                .iter()
                .filter(|outcome| **outcome == ContactOutcomeKind::Accepted)
                .count(),
            1
        );
    }

    #[test]
    fn guard_depletion_makes_the_later_same_tick_contact_unguarded() {
        let target_id = FighterId::new(1).unwrap();
        let target = Vec3::new(0.0, ARENA_TOP_Y, 0.0);
        let fighter_positions = [
            (FighterId::ZERO, Vec3::new(-4.0, ARENA_TOP_Y, -2.0)),
            (target_id, target),
            (
                FighterId::new(2).unwrap(),
                Vec3::new(4.0, ARENA_TOP_Y, -2.0),
            ),
        ];
        let contact_origin = target + Vec3::new(0.0, 0.58, -0.3);
        // Put the higher owner first in pool-specification order. Canonical
        // owner ordering still chooses fighter 0 as the guard-breaking hit.
        let hitbox_specs = [
            ArbitrationHitboxSpec {
                owner: FighterId::new(2).unwrap(),
                payload: AttackPayloadId::KiriageBeat1,
                origin: contact_origin,
                reaction: Some(ReactionFamilyId::ShortStandingStagger),
            },
            ArbitrationHitboxSpec {
                owner: FighterId::ZERO,
                payload: AttackPayloadId::KiriageBeat1,
                origin: contact_origin,
                reaction: Some(ReactionFamilyId::ShortStandingStagger),
            },
        ];
        let forward = run_arbitration_fixture(
            &fighter_positions,
            &hitbox_specs,
            false,
            Some((target_id, 20.0)),
        );
        let reversed = run_arbitration_fixture(
            &fighter_positions,
            &hitbox_specs,
            true,
            Some((target_id, 20.0)),
        );
        assert_eq!(forward, reversed);

        let target_state = forward.fighters[target_id.index()].unwrap();
        assert_eq!(target_state.stamina_q, 0);
        assert_eq!(target_state.action, FighterAction::Hitstun);
        assert_eq!(
            forward.outcomes,
            vec![ContactOutcomeKind::Guarded, ContactOutcomeKind::Accepted]
        );
        assert!(matches!(
            forward.events.as_slice(),
            [
                (_, SimEventKind::Guarded { defender, .. }),
                (_, SimEventKind::HitConfirmed { victim, .. })
            ] if *defender == target_id && *victim == target_id
        ));
    }

    #[test]
    fn cinematic_catch_claim_arbitrates_before_ordinary_grab() {
        let victim = Vec3::new(0.0, ARENA_TOP_Y, 0.0);
        let result = assert_reversed_arbitration_fixture(
            &[
                (FighterId::ZERO, Vec3::new(-4.0, ARENA_TOP_Y, 0.0)),
                (FighterId::new(1).unwrap(), Vec3::new(4.0, ARENA_TOP_Y, 0.0)),
                (FighterId::new(2).unwrap(), victim),
            ],
            &[
                ArbitrationHitboxSpec {
                    owner: FighterId::ZERO,
                    payload: AttackPayloadId::GrabCatch,
                    origin: victim + Vec3::Y * 0.58,
                    reaction: None,
                },
                ArbitrationHitboxSpec {
                    owner: FighterId::new(1).unwrap(),
                    payload: AttackPayloadId::UltimateCatch,
                    origin: victim + Vec3::Y * 0.58,
                    reaction: None,
                },
            ],
        );

        assert_eq!(result.fighters[0].unwrap().holding, None);
        assert_eq!(
            result.fighters[1].unwrap().ultimate_target,
            Some(FighterId::new(2).unwrap())
        );
        assert_eq!(
            result.fighters[2].unwrap().ultimate_owner,
            Some(FighterId::new(1).unwrap())
        );
        assert_eq!(result.fighters[2].unwrap().held_by, None);
    }

    #[test]
    fn light_fire_punch_visual_side_matches_light_attack_pose_side() {
        assert_eq!(
            light_fire_punch_visual_side(TechniqueId::CatLight1),
            Some(-1.0)
        );
        assert_eq!(
            light_fire_punch_visual_side(TechniqueId::PigLight1),
            Some(-1.0)
        );
        assert_eq!(
            light_fire_punch_visual_side(TechniqueId::CatLight2),
            Some(1.0)
        );
        assert_eq!(
            light_fire_punch_visual_side(TechniqueId::PigLight2),
            Some(1.0)
        );
        assert_eq!(
            light_fire_punch_visual_side(TechniqueId::PenguinLight1),
            None
        );
        assert_eq!(
            light_fire_punch_visual_side(TechniqueId::PenguinLight2),
            None
        );
        assert_eq!(light_fire_punch_visual_side(TechniqueId::CatHeavy), None);
    }

    #[test]
    fn detached_light_fire_punch_card_is_enabled_for_combined_corner_trial() {
        assert!(LIGHT_FIRE_PUNCH_CARD_ENABLED);
    }

    #[test]
    fn pig_light_fire_punch_uses_blue_palette() {
        assert_eq!(
            light_fire_punch_palette(CharacterKind::Cat),
            FirePunchPalette::Red
        );
        assert_eq!(
            light_fire_punch_palette(CharacterKind::Pig),
            FirePunchPalette::Blue
        );
    }

    #[test]
    fn reaction_visual_side_follows_defender_facing_and_impact_direction() {
        assert_eq!(reaction_visual_side(Vec3::Z, Vec3::X), 1.0);
        assert_eq!(reaction_visual_side(Vec3::Z, Vec3::NEG_X), -1.0);
        assert_eq!(reaction_visual_side(Vec3::Z, Vec3::Z), 1.0);
    }

    #[test]
    fn airborne_juggle_hitstun_records_reaction_visual_family() {
        let reaction = reaction_profile_for_family(ReactionFamilyId::LightAirPop);
        let mut motor = FighterMotor::default();
        let mut action = FighterActionState::default();

        apply_airborne_juggle_hitstun(&mut motor, &mut action, 2.4, reaction, None, false, 0);

        assert_eq!(action.action, FighterAction::Hitstun);
        assert_eq!(action.reaction_family, Some(ReactionFamilyId::LightAirPop));
        assert_eq!(action.reaction_visual_side, 1.0);
    }

    fn hitbox_for_payload(payload_id: AttackPayloadId, facing: Vec3) -> Hitbox {
        let payload = attack_payload_definition(payload_id);
        let shape = attack_shape_definition(payload.shape_id);
        Hitbox {
            owner: FighterId::ZERO,
            kind: payload.kind,
            payload_id: Some(payload.id),
            attacker_character: None,
            technique_id: None,
            hit_effect: None,
            shape_id: payload.shape_id,
            reaction_family: payload.reaction_family,
            damage_profile: payload.damage_profile,
            element: payload.element,
            attacker_equipment: None,
            attacker_style: None,
            power: payload.power,
            str_scale: payload.str_scale,
            damage: payload.damage,
            knockback: payload.knockback,
            vertical_knockback: payload.vertical_knockback,
            guardable: payload.guardable,
            base_radius: shape.radius,
            radius: shape.radius,
            lifetime: TickTimer::from_millis_ceil(payload.time_ms),
            elapsed: ElapsedTicks::ZERO,
            total_lifetime: milliseconds_to_ticks_ceil(payload.time_ms),
            spawn_origin: Vec3::ZERO,
            facing,
            base_range: shape.range,
            range: shape.range,
            scales_with_owner_size: true,
            vertical_offset_scale: shape.vertical_offset_scale,
            parented: shape.parented,
            path: shape.path,
            expires_on_owner_landing: hitbox_landing_linger(payload_id).is_some(),
            landing_linger: hitbox_landing_linger(payload_id).unwrap_or(TickTimer::ZERO),
            landing_linger_started: false,
            ground_path_end: hitbox_ground_path_clearance(payload_id).is_some(),
            ground_path_clearance: hitbox_ground_path_clearance(payload_id).unwrap_or(0.0),
            impact_cue: payload.impact_cue,
            hitstop_scale: payload.hitstop_scale,
            shake_scale: payload.shake_scale,
            feedback_priority_bonus: payload.feedback_priority_bonus,
            already_hit: FighterHitMask::default(),
        }
    }

    fn assert_vec3_close(actual: Vec3, expected: Vec3, tolerance: f32) {
        assert!(
            actual.distance(expected) <= tolerance,
            "expected {actual:?} to be within {tolerance} of {expected:?}"
        );
    }

    fn neutral_damage_context() -> DamageContext {
        DamageContext {
            guarded: false,
            perfect_guard: false,
            airborne: false,
            downed: false,
            counter_hit: false,
            low_health: false,
            ignore_damage: false,
            global_damage_scale: 1.0,
            rule_damage_correction: 1.0,
            source: ImpactSource::FighterStrike,
            guard_stamina: MAX_STAMINA,
            incoming_guard_stamina_damage: 0.0,
            target_health: MAX_HEALTH,
            target_status: DamageTargetStatus::Standing,
            element: DamageElement::Neutral,
            carryover_element: None,
            carryover_strength: 0.0,
            element_affinity: DamageElementAffinity::Neutral,
            attacker_equipment: None,
            attacker_style: None,
            defender_equipment: None,
            defender_style: None,
            heavy_impact: false,
            high_power: false,
            lethal_raw: false,
        }
    }

    #[test]
    fn neutral_impact_owner_cannot_receive_credit() {
        assert!(!impact_owner_can_receive_credit(NEUTRAL_IMPACT_OWNER_ID));
        assert!(impact_owner_can_receive_credit(0));
    }

    #[test]
    fn radial_falloff_keeps_minimum_blast_pressure_inside_radius() {
        assert_eq!(radial_falloff(0.0, 10.0), 1.0);
        assert!((radial_falloff(7.2, 10.0) - 0.28).abs() < 0.001);
        assert!((radial_falloff(10.0, 10.0) - 0.28).abs() < 0.001);
    }

    #[test]
    fn guard_facing_blocks_front_and_sides_but_not_rear() {
        let target = Vec3::ZERO;
        let front_origin = Vec3::Z;
        let front_right_diagonal = Vec3::new(1.0, 0.0, 1.0);
        let front_left_diagonal = Vec3::new(-1.0, 0.0, 1.0);
        let right_side_origin = Vec3::X;
        let left_side_origin = -Vec3::X;
        let shallow_rear_flank_origin = Vec3::new(
            91.0_f32.to_radians().sin(),
            0.0,
            91.0_f32.to_radians().cos(),
        );
        let deep_rear_flank_origin = Vec3::new(
            100.0_f32.to_radians().sin(),
            0.0,
            100.0_f32.to_radians().cos(),
        );
        let back_origin = -Vec3::Z;

        assert!(guard_faces_impact(Vec3::Z, target, front_origin));
        assert!(guard_faces_impact(Vec3::Z, target, front_right_diagonal));
        assert!(guard_faces_impact(Vec3::Z, target, front_left_diagonal));
        assert!(guard_faces_impact(Vec3::Z, target, right_side_origin));
        assert!(guard_faces_impact(Vec3::Z, target, left_side_origin));
        assert!(!guard_faces_impact(
            Vec3::Z,
            target,
            shallow_rear_flank_origin
        ));
        assert!(!guard_faces_impact(Vec3::Z, target, deep_rear_flank_origin));
        assert!(!guard_faces_impact(Vec3::Z, target, back_origin));
        assert!(impact_is_guarded(
            &profile_for_test(true),
            20.0,
            FighterAction::Guarding,
            Vec3::Z,
            target,
            front_origin,
        ));
        assert!(!impact_is_guarded(
            &profile_for_test(true),
            0.0,
            FighterAction::Guarding,
            Vec3::Z,
            target,
            front_origin,
        ));
        assert!(!impact_is_guarded(
            &profile_for_test(false),
            20.0,
            FighterAction::Guarding,
            Vec3::Z,
            target,
            front_origin,
        ));
        assert!(!impact_is_guarded(
            &impact_profile_from_hitbox(&hitbox_for_payload(
                AttackPayloadId::PigUltimateCatch,
                Vec3::Z,
            )),
            20.0,
            FighterAction::Guarding,
            Vec3::Z,
            target,
            front_origin,
        ));
    }

    #[test]
    fn normal_guard_reduces_health_damage_and_commits_authored_stamina_pressure() {
        let profile = profile_for_test(true);
        let pre_clamp_guarded = resolve_authored_damage(
            &profile,
            DamageContext {
                guarded: true,
                target_status: DamageTargetStatus::Guarding,
                ..neutral_damage_context()
            },
        );
        let guarded = guarded_damage_outcome(pre_clamp_guarded);

        assert!(
            (guarded.health_damage - pre_clamp_guarded.health_damage * GUARD_HEALTH_DAMAGE_SCALE)
                .abs()
                < 0.001
        );
        assert!(guarded.guard_stamina_damage > 0.0);
    }

    #[test]
    fn hit_eligibility_rejects_invulnerable_and_disabled_targets() {
        let action = FighterActionState::default();
        let mut stats = FighterStats::default();
        assert!(can_receive_impact(&stats, &action));

        stats.invulnerability = TickTimer::from_seconds_ceil(0.1);
        assert!(!can_receive_impact(&stats, &action));

        stats.invulnerability.clear();
        let knockdown = FighterActionState {
            action: FighterAction::Knockdown,
            ..default()
        };
        assert!(!can_receive_impact(&stats, &knockdown));

        let grabbed = FighterActionState {
            action: FighterAction::Grabbed,
            ..default()
        };
        assert!(!can_receive_impact(&stats, &grabbed));
    }

    #[test]
    fn airborne_hitstun_targets_remain_attackable() {
        let stats = FighterStats::default();
        let launched = FighterActionState {
            action: FighterAction::Hitstun,
            reaction_recover_ms: Some(500),
            ..default()
        };
        assert!(can_receive_impact(&stats, &launched));
    }

    #[test]
    fn airborne_juggle_light_hits_preserve_landing_knockdown() {
        let pending = reaction_profile_for_family(ReactionFamilyId::LauncherDown).landing_aftermath;
        let reaction = reaction_profile_for_family(ReactionFamilyId::ShortStandingStagger);
        let mut motor = FighterMotor {
            grounded: false,
            velocity: Vec3::new(1.0, -3.2, 0.0),
            landing_aftermath: pending,
            reaction_bounces: 1,
            ..default()
        };
        let mut action = FighterActionState {
            action: FighterAction::Hitstun,
            ..default()
        };

        assert!(should_defer_ground_reaction_until_landing(
            true, pending, false
        ));
        apply_airborne_juggle_hitstun(&mut motor, &mut action, -3.2, reaction, pending, false, 1);

        assert_eq!(action.action, FighterAction::Hitstun);
        assert_eq!(action.reaction_recover_ms, reaction.hitstun_recover_ms);
        assert!(!motor.grounded);
        assert_eq!(motor.velocity.y, -3.2);
        assert_eq!(motor.landing_aftermath, pending);
        assert_eq!(motor.reaction_bounces, 1);
    }

    #[test]
    fn airborne_rehit_keeps_prior_landing_aftermath_when_new_launch_has_none() {
        let pending = reaction_profile_for_family(ReactionFamilyId::LauncherDown).landing_aftermath;
        let air_pop = reaction_profile_for_family(ReactionFamilyId::LightAirPop);

        assert_eq!(air_pop.landing_aftermath, None);
        assert_eq!(
            landing_aftermath_after_airborne_rehit(true, air_pop.landing_aftermath, pending),
            pending
        );
        assert_eq!(
            landing_aftermath_after_airborne_rehit(false, air_pop.landing_aftermath, pending),
            None
        );
    }

    #[test]
    fn knockback_direction_points_away_from_origin() {
        assert_eq!(
            knockback_direction(Vec3::ZERO, Vec3::Z),
            Vec3::new(0.0, 0.0, -1.0)
        );
        assert_eq!(
            incoming_direction(Vec3::ZERO, Vec3::Z),
            Vec3::new(0.0, 0.0, 1.0)
        );
    }

    #[test]
    fn mushroom_scaled_hitboxes_refresh_to_current_owner_size() {
        let mut hitbox = hitbox_for_payload(AttackPayloadId::AsBeat1, Vec3::X);
        hitbox.base_range = 1.2;
        hitbox.base_radius = 0.4;
        hitbox.range = hitbox.base_range;
        hitbox.radius = hitbox.base_radius;
        hitbox.scales_with_owner_size = true;

        let mut stats = FighterStats::default();
        stats.item_giant_timer = TickTimer::from_seconds_ceil(1.0);
        let giant_scale = stats.item_size_multiplier();
        refresh_hitbox_dimensions(&mut hitbox, Some(giant_scale));
        assert!((hitbox.range - hitbox.base_range * giant_scale).abs() < 0.001);
        assert!((hitbox.radius - hitbox.base_radius * giant_scale).abs() < 0.001);

        stats.item_giant_timer.clear();
        refresh_hitbox_dimensions(&mut hitbox, Some(stats.item_size_multiplier()));
        assert!((hitbox.range - hitbox.base_range).abs() < 0.001);
        assert!((hitbox.radius - hitbox.base_radius).abs() < 0.001);
    }

    #[test]
    fn non_scaling_hitboxes_ignore_mushroom_size() {
        let mut hitbox = hitbox_for_payload(AttackPayloadId::AsBeat1, Vec3::X);
        hitbox.base_range = 1.2;
        hitbox.base_radius = 0.4;
        hitbox.range = hitbox.base_range;
        hitbox.radius = hitbox.base_radius;
        hitbox.scales_with_owner_size = false;

        let mut stats = FighterStats::default();
        stats.item_giant_timer = TickTimer::from_seconds_ceil(1.0);
        refresh_hitbox_dimensions(&mut hitbox, Some(stats.item_size_multiplier()));
        assert!((hitbox.range - hitbox.base_range).abs() < 0.001);
        assert!((hitbox.radius - hitbox.base_radius).abs() < 0.001);
    }

    #[test]
    fn kiriage_payloads_push_victim_along_hitbox_path() {
        let beat1 = hitbox_for_payload(AttackPayloadId::KiriageBeat1, Vec3::X);
        let beat2 = hitbox_for_payload(AttackPayloadId::KiriageBeat2, Vec3::X);
        let jump_fish = hitbox_for_payload(AttackPayloadId::JumpFishShot, Vec3::X);
        let ultimate_scratch = hitbox_for_payload(AttackPayloadId::UltimateScratchLight, Vec3::X);
        let penguin_slope_ult =
            hitbox_for_payload(AttackPayloadId::PenguinUltimateSlopeCrash, Vec3::X);
        let pig_swing = hitbox_for_payload(AttackPayloadId::PigHamSwingPartial, Vec3::X);
        let step = hitbox_for_payload(AttackPayloadId::HeavyStep, Vec3::X);

        let beat1_profile = impact_profile_from_hitbox(&beat1);
        let beat2_profile = impact_profile_from_hitbox(&beat2);
        let jump_fish_profile = impact_profile_from_hitbox(&jump_fish);
        let ultimate_scratch_profile = impact_profile_from_hitbox(&ultimate_scratch);
        let penguin_slope_ult_profile = impact_profile_from_hitbox(&penguin_slope_ult);
        let pig_swing_profile = impact_profile_from_hitbox(&pig_swing);
        let step_profile = impact_profile_from_hitbox(&step);

        assert_vec3_close(
            impact_knockback_direction(&beat1_profile, Vec3::ZERO, Vec3::Z),
            Vec3::X,
            0.001,
        );
        assert_vec3_close(
            impact_knockback_direction(&beat2_profile, Vec3::ZERO, Vec3::Z),
            Vec3::X,
            0.001,
        );
        assert_vec3_close(
            impact_knockback_direction(&jump_fish_profile, Vec3::ZERO, Vec3::Z),
            Vec3::X,
            0.001,
        );
        assert_vec3_close(
            impact_knockback_direction(&ultimate_scratch_profile, Vec3::ZERO, Vec3::ZERO),
            Vec3::X,
            0.001,
        );
        assert_vec3_close(
            impact_knockback_direction(&penguin_slope_ult_profile, Vec3::ZERO, Vec3::Z),
            Vec3::X,
            0.001,
        );
        assert_vec3_close(
            impact_knockback_direction(&pig_swing_profile, Vec3::ZERO, Vec3::Z),
            Vec3::X,
            0.001,
        );
        assert_eq!(
            jump_fish_profile.reaction.id,
            ReactionFamilyId::AirFishKnockdown
        );
        assert!(step_profile.knockback_direction.is_none());
    }

    #[test]
    fn penguin_slope_ultimate_launches_much_farther_than_dash_x() {
        let dash = impact_profile_from_hitbox(&hitbox_for_payload(
            AttackPayloadId::PenguinSlopeCrash,
            Vec3::X,
        ));
        let ultimate = impact_profile_from_hitbox(&hitbox_for_payload(
            AttackPayloadId::PenguinUltimateSlopeCrash,
            Vec3::X,
        ));
        let dash_planar_speed = dash.knockback * dash.reaction.horizontal_scale;
        let ultimate_planar_speed = ultimate.knockback * ultimate.reaction.horizontal_scale;

        assert_eq!(ultimate.reaction.id, ReactionFamilyId::AirFishKnockdown);
        assert!(ultimate_planar_speed > dash_planar_speed * 1.75);
        assert!(ultimate.vertical_knockback > dash.vertical_knockback);
        assert_eq!(ultimate.feedback.hitstop, 0.0);
        assert_eq!(ultimate.feedback.guard_hitstop, 0.0);

        let mut motor = FighterMotor::default();
        apply_penguin_slope_ultimate_impact_travel(&mut motor, &ultimate, ultimate_planar_speed);
        assert!(motor.impact_speed_limit >= ultimate_planar_speed);
        assert!(
            motor.impact_speed_limit_timer.as_seconds() * ultimate_planar_speed
                >= PENGUIN_SLOPE_TOTAL_FORWARD * 1.15
        );

        let mut dash_motor = FighterMotor::default();
        apply_penguin_slope_ultimate_impact_travel(&mut dash_motor, &dash, dash_planar_speed);
        assert_eq!(dash_motor.impact_speed_limit_timer, TickTimer::ZERO);
    }

    #[test]
    fn penguin_slope_ultimate_recoil_only_on_unguarded_ultimate_hit() {
        assert_vec3_close(
            penguin_slope_ultimate_attacker_recoil_direction(
                Some(AttackPayloadId::PenguinUltimateSlopeCrash),
                false,
                Some(Vec3::ZERO),
                Some(Vec3::X),
                Vec3::Z,
            )
            .unwrap(),
            Vec3::X,
            0.001,
        );
        assert_vec3_close(
            penguin_slope_ultimate_attacker_recoil_direction(
                Some(AttackPayloadId::PenguinUltimateSlopeCrash),
                false,
                None,
                None,
                Vec3::X,
            )
            .unwrap(),
            Vec3::X,
            0.001,
        );
        assert!(
            penguin_slope_ultimate_attacker_recoil_direction(
                Some(AttackPayloadId::PenguinUltimateSlopeCrash),
                true,
                Some(Vec3::ZERO),
                Some(Vec3::X),
                Vec3::X,
            )
            .is_none()
        );
        assert!(
            penguin_slope_ultimate_attacker_recoil_direction(
                Some(AttackPayloadId::PenguinSlopeCrash),
                false,
                Some(Vec3::ZERO),
                Some(Vec3::X),
                Vec3::X,
            )
            .is_none()
        );
    }

    #[test]
    fn penguin_slope_ultimate_recoil_uses_giant_target_contact_direction() {
        let arena = &crate::arena_defs::arena_definitions()[0];
        let catalog = CharacterMoveCatalog::default();
        let target_character = FighterCharacter::new(CharacterKind::Penguin);
        let target_motor = FighterMotor {
            facing: Vec3::Z,
            grounded: true,
            ..default()
        };
        let mut target_stats = FighterStats::default();
        target_stats.item_giant_timer = TickTimer::from_seconds_ceil(1.0);
        let target_position = SimPosition::new(Vec3::new(
            FIGHTER_RADIUS * 2.75,
            ARENA_TOP_Y,
            FIGHTER_RADIUS * 1.15,
        ));
        let target_body = fighter_hurt_box(
            &target_position,
            &target_motor,
            &target_character,
            &target_stats,
            &catalog,
        );
        let owner_airborne_position = Vec3::new(0.0, ARENA_TOP_Y + 1.15, 0.0);
        let mut ultimate_hitbox =
            hitbox_for_payload(AttackPayloadId::PenguinUltimateSlopeCrash, Vec3::Z);
        let mut ultimate_position = SimPosition::default();

        ultimate_hitbox.elapsed = ElapsedTicks::from_ticks(ultimate_hitbox.total_lifetime / 2);
        refresh_hitbox_position(
            &ultimate_hitbox,
            &mut ultimate_position,
            Some(owner_airborne_position),
            arena,
        );
        let contact_point = sphere_body_box_contact(
            ultimate_position.translation,
            ultimate_hitbox.radius,
            target_body,
        )
        .expect("giant target body should overlap penguin slope ultimate hitbox");

        let recoil_direction = penguin_slope_ultimate_attacker_recoil_direction(
            ultimate_hitbox.payload_id,
            false,
            Some(owner_airborne_position),
            Some(contact_point),
            ultimate_hitbox.facing,
        )
        .unwrap();

        assert!(recoil_direction.x > 0.35);
        assert!(recoil_direction.z < 0.95);
    }

    #[test]
    fn penguin_slope_ultimate_recoil_bumps_attacker_backward() {
        let mut motor = FighterMotor {
            velocity: Vec3::new(8.0, 0.2, 0.0),
            grounded: true,
            dash_slide_timer: TickTimer::from_seconds_ceil(0.4),
            impact_speed_limit_timer: TickTimer::from_seconds_ceil(0.2),
            impact_speed_limit: 12.0,
            landing_stick_timer: TickTimer::from_seconds_ceil(0.3),
            ..default()
        };
        let mut action = FighterActionState {
            action: FighterAction::UltimateRush,
            technique_id: Some(TechniqueId::PenguinUltimateRush),
            timeline_events_fired: 0b0111,
            ..default()
        };

        apply_penguin_slope_ultimate_attacker_recoil(&mut motor, &mut action, Vec3::X);

        assert_vec3_close(
            Vec3::new(motor.velocity.x, 0.0, motor.velocity.z),
            Vec3::NEG_X * PENGUIN_SLOPE_ULTIMATE_ATTACKER_RECOIL_SPEED,
            0.001,
        );
        assert_eq!(
            motor.velocity.y,
            PENGUIN_SLOPE_ULTIMATE_ATTACKER_RECOIL_LIFT
        );
        assert!(!motor.grounded);
        assert_eq!(motor.dash_slide_timer, TickTimer::ZERO);
        assert_eq!(motor.impact_speed_limit_timer, TickTimer::ZERO);
        assert_eq!(motor.impact_speed_limit, 0.0);
        assert_eq!(motor.landing_stick_timer, TickTimer::ZERO);
        assert_eq!(action.action, FighterAction::UltimateRush);
        assert_eq!(action.technique_id, Some(TechniqueId::PenguinUltimateRush));
        assert_ne!(
            action.timeline_events_fired & PENGUIN_SLOPE_ULTIMATE_EXIT_MOTION_EVENT_MASK,
            0
        );
    }

    #[test]
    fn penguin_slope_hitboxes_reach_airborne_body_contact() {
        let arena = &crate::arena_defs::arena_definitions()[0];
        let catalog = CharacterMoveCatalog::default();
        let target_character = FighterCharacter::new(CharacterKind::Penguin);
        let target_motor = FighterMotor {
            facing: Vec3::Z,
            grounded: true,
            ..default()
        };
        let target_stats = FighterStats::default();
        let target_position = Vec3::new(0.0, ARENA_TOP_Y, FIGHTER_RADIUS * 2.85);
        let target_position = SimPosition::new(target_position);
        let target_body = fighter_hurt_box(
            &target_position,
            &target_motor,
            &target_character,
            &target_stats,
            &catalog,
        );
        let owner_airborne_position = Vec3::new(0.0, ARENA_TOP_Y + 1.15, 0.0);
        let mut dash_hitbox = hitbox_for_payload(AttackPayloadId::PenguinSlopeCrash, Vec3::Z);
        let mut ultimate_hitbox =
            hitbox_for_payload(AttackPayloadId::PenguinUltimateSlopeCrash, Vec3::Z);
        let mut dash_position = SimPosition::default();
        let mut ultimate_position = SimPosition::default();

        dash_hitbox.elapsed = ElapsedTicks::from_ticks(dash_hitbox.total_lifetime / 2);
        ultimate_hitbox.elapsed = ElapsedTicks::from_ticks(ultimate_hitbox.total_lifetime / 2);
        refresh_hitbox_position(
            &dash_hitbox,
            &mut dash_position,
            Some(owner_airborne_position),
            arena,
        );
        refresh_hitbox_position(
            &ultimate_hitbox,
            &mut ultimate_position,
            Some(owner_airborne_position),
            arena,
        );

        assert!(
            sphere_body_box_contact(dash_position.translation, dash_hitbox.radius, target_body)
                .is_some()
        );
        assert!(
            sphere_body_box_contact(
                ultimate_position.translation,
                ultimate_hitbox.radius,
                target_body
            )
            .is_some()
        );
        assert!(ultimate_hitbox.radius > dash_hitbox.radius);
        assert!(ultimate_position.translation.y < dash_position.translation.y);
    }

    #[test]
    fn penguin_slope_dash_hitbox_still_needs_visible_body_contact() {
        let arena = &crate::arena_defs::arena_definitions()[0];
        let catalog = CharacterMoveCatalog::default();
        let target_character = FighterCharacter::new(CharacterKind::Penguin);
        let target_motor = FighterMotor {
            facing: Vec3::Z,
            grounded: true,
            ..default()
        };
        let target_stats = FighterStats::default();
        let target_position = SimPosition::new(Vec3::new(0.0, ARENA_TOP_Y, FIGHTER_RADIUS * 3.05));
        let target_body = fighter_hurt_box(
            &target_position,
            &target_motor,
            &target_character,
            &target_stats,
            &catalog,
        );
        let owner_airborne_position = Vec3::new(0.0, ARENA_TOP_Y + 1.15, 0.0);
        let mut dash_hitbox = hitbox_for_payload(AttackPayloadId::PenguinSlopeCrash, Vec3::Z);
        let mut dash_position = SimPosition::default();

        dash_hitbox.elapsed = ElapsedTicks::from_ticks(dash_hitbox.total_lifetime / 2);
        refresh_hitbox_position(
            &dash_hitbox,
            &mut dash_position,
            Some(owner_airborne_position),
            arena,
        );

        assert!(
            sphere_body_box_contact(dash_position.translation, dash_hitbox.radius, target_body)
                .is_none()
        );
    }

    #[test]
    fn pig_ham_swing_charge_preserves_increasing_impact_travel() {
        let tap = impact_profile_from_hitbox(&hitbox_for_payload(
            AttackPayloadId::PigHamSwingTap,
            Vec3::X,
        ));
        let partial = impact_profile_from_hitbox(&hitbox_for_payload(
            AttackPayloadId::PigHamSwingPartial,
            Vec3::X,
        ));
        let full = impact_profile_from_hitbox(&hitbox_for_payload(
            AttackPayloadId::PigHamSwingFull,
            Vec3::X,
        ));
        let tap_speed = tap.knockback * tap.reaction.horizontal_scale;
        let partial_speed = partial.knockback * partial.reaction.horizontal_scale;
        let full_speed = full.knockback * full.reaction.horizontal_scale;

        assert!(tap_speed < partial_speed);
        assert!(partial_speed < full_speed);
        assert_eq!(partial.reaction.id, ReactionFamilyId::SlidingKnockdown);
        assert_eq!(full.reaction.id, ReactionFamilyId::GroundBounceDown);
        assert!(full.reaction.landing_aftermath.is_some());

        let mut partial_motor = FighterMotor {
            landing_stick_timer: TickTimer::from_seconds_ceil(0.08),
            ..default()
        };
        apply_pig_ham_swing_impact_travel(&mut partial_motor, &partial, partial_speed);
        assert_eq!(partial_motor.landing_stick_timer, TickTimer::ZERO);
        assert!(partial_motor.dash_slide_timer.active());
        assert!(partial_motor.impact_speed_limit >= partial_speed);

        let mut full_motor = FighterMotor::default();
        apply_pig_ham_swing_impact_travel(&mut full_motor, &full, full_speed);
        assert!(full_motor.impact_speed_limit > partial_motor.impact_speed_limit);
        assert!(full_motor.impact_speed_limit_timer > partial_motor.impact_speed_limit_timer);
    }

    #[test]
    fn pig_air_meat_slam_second_airborne_rehit_becomes_meteor() {
        let base = impact_profile_from_hitbox(&hitbox_for_payload(
            AttackPayloadId::PigAirMeatSlam,
            Vec3::X,
        ));

        assert_eq!(
            pig_air_meat_slam_air_hits_after_impact(base.payload_id, false, 1),
            0
        );
        assert_eq!(
            pig_air_meat_slam_air_hits_after_impact(base.payload_id, true, 0),
            1
        );
        assert!(!pig_air_meat_slam_should_meteor(&base, true, 0));
        assert!(pig_air_meat_slam_should_meteor(&base, true, 1));

        let mut meteor = base;
        apply_pig_air_meat_slam_meteor_profile(&mut meteor);

        assert_eq!(meteor.reaction_family, ReactionFamilyId::AerialSpikeDown);
        assert_eq!(meteor.reaction.id, ReactionFamilyId::AerialSpikeDown);
        assert_eq!(meteor.damage_profile, DamageProfileId::AerialSpike);
        assert!(meteor.reaction.vertical_scale < 0.0);
        assert!(meteor.vertical_knockback > base.vertical_knockback);
        assert!(
            meteor.vertical_knockback * meteor.reaction.vertical_scale
                < -base.vertical_knockback * 0.9
        );
        assert!(meteor.reaction.landing_aftermath.is_some());
    }

    #[test]
    fn jump_spike_hitbox_lives_until_owner_landing_then_lingers() {
        let mut hitbox = hitbox_for_payload(AttackPayloadId::JumpSpike, Vec3::Z);
        let landing_linger = TickTimer::from_seconds_ceil(JUMP_ATTACK_LANDING_HITBOX_LINGER);

        assert!(hitbox.expires_on_owner_landing);
        assert_eq!(hitbox.landing_linger, landing_linger);
        assert_eq!(
            hitbox.total_lifetime,
            seconds_to_ticks_ceil(JUMP_ATTACK_MAX_ACTIVE)
        );
        assert!(!hitbox.landing_linger_started);

        hitbox.lifetime = TickTimer::from_seconds_ceil(0.8);
        start_hitbox_landing_linger(&mut hitbox, false);
        assert_eq!(hitbox.lifetime, TickTimer::from_seconds_ceil(0.8));
        assert!(!hitbox.landing_linger_started);

        start_hitbox_landing_linger(&mut hitbox, true);
        assert_eq!(hitbox.lifetime, landing_linger);
        assert!(hitbox.landing_linger_started);
    }

    #[test]
    fn pig_air_meat_slam_lives_to_landing_and_grounds_its_endpoint() {
        let mut hitbox = hitbox_for_payload(AttackPayloadId::PigAirMeatSlam, Vec3::Z);
        let shape = attack_shape_definition(AttackShapeId::PigAirMeatSlam);
        let base = Vec3::new(0.0, ARENA_TOP_Y + 2.2, 0.0);
        let arena = &crate::arena_defs::arena_definitions()[0];
        let landing_linger = TickTimer::from_seconds_ceil(JUMP_ATTACK_LANDING_HITBOX_LINGER);

        assert!(hitbox.expires_on_owner_landing);
        assert_eq!(hitbox.landing_linger, landing_linger);
        assert!(hitbox.ground_path_end);
        assert_eq!(
            hitbox.ground_path_clearance,
            JUMP_HEAVY_FISH_GROUND_CLEARANCE
        );

        let ungrounded_end = shape_center(
            base,
            Vec3::Z,
            shape.range,
            shape.vertical_offset_scale,
            shape.path,
            1.0,
        );
        let grounded_end = shape_center_with_ground_path_end(
            base,
            Vec3::Z,
            shape.range,
            shape.vertical_offset_scale,
            shape.path,
            1.0,
            true,
            JUMP_HEAVY_FISH_GROUND_CLEARANCE,
            arena,
        );
        let expected_ground = ground_support_for_arena_with_radius(
            arena,
            ungrounded_end.x,
            ungrounded_end.z,
            FIGHTER_RADIUS,
        )
        .height()
        .unwrap_or(ARENA_TOP_Y)
            + JUMP_HEAVY_FISH_GROUND_CLEARANCE;

        assert!((grounded_end.y - expected_ground).abs() < 0.001);

        hitbox.lifetime = TickTimer::from_seconds_ceil(0.8);
        start_hitbox_landing_linger(&mut hitbox, true);
        assert_eq!(hitbox.lifetime, landing_linger);
    }

    #[test]
    fn last_attacker_credit_uses_a_checked_stable_fighter_id() {
        let mut stats = FighterStats::default();
        credit_last_attacker(&mut stats, 3);
        assert_eq!(stats.last_attacker, FighterId::new(3));

        credit_last_attacker(&mut stats, usize::MAX);
        assert_eq!(stats.last_attacker, None);
    }

    #[test]
    fn impact_source_is_separate_from_attack_kind() {
        assert_eq!(
            ImpactSource::from_attack_kind(AttackKind::Heavy),
            ImpactSource::FighterStrike
        );
        assert_eq!(
            ImpactSource::from_attack_kind(AttackKind::Grab),
            ImpactSource::GrabThrow
        );
        let thrown_item_profile = impact_profile(
            9,
            ImpactSource::ItemThrow,
            10.0,
            6.0,
            2.0,
            false,
            true,
            12.0,
            ImpactFeedbackIntensity::Light,
            ReactionFamilyId::GroundedDownGetup,
        );
        assert_eq!(thrown_item_profile.source, ImpactSource::ItemThrow);
    }

    #[test]
    fn impact_feedback_profiles_are_source_driven() {
        let light =
            impact_feedback_profile(ImpactSource::FighterStrike, ImpactFeedbackIntensity::Light);
        let heavy =
            impact_feedback_profile(ImpactSource::FighterStrike, ImpactFeedbackIntensity::Heavy);
        let blast =
            impact_feedback_profile(ImpactSource::ItemBlast, ImpactFeedbackIntensity::Light);

        assert_eq!(light.cue, "strike_light");
        assert!(heavy.hitstop > light.hitstop);
        assert!(heavy.spark_scale > light.spark_scale);
        assert!(blast.priority > heavy.priority);
    }

    #[test]
    fn feedback_cues_keep_highest_priority_recent_route() {
        let mut effects = HitEffects::default();
        effects.push_feedback_cue("small", ImpactSource::FighterStrike, 10);
        effects.push_feedback_cue("quiet", ImpactSource::Projectile, 8);
        assert_eq!(effects.last_cue.unwrap().id, "small");

        effects.push_feedback_cue("loud", ImpactSource::RingOut, 80);
        assert_eq!(effects.last_cue.unwrap().id, "loud");
        assert!(effects.cue_label().unwrap().contains("RingOut"));
    }

    #[test]
    fn timeline_feedback_profiles_make_authored_phases_read_distinctly() {
        let startup = timeline_feedback_profile(FeedbackPhase::Startup, "startup_A_s");
        let prehit = timeline_feedback_profile(FeedbackPhase::PreHit, "trail_A_s");
        let heavy_aftermath =
            timeline_feedback_profile(FeedbackPhase::Aftermath, "post_hit_kiriage");

        assert!(prehit.priority > startup.priority);
        assert!(prehit.shake > startup.shake);
        assert!(heavy_aftermath.priority > phase_priority(FeedbackPhase::Aftermath));
        assert!(heavy_aftermath.hud_flash > startup.hud_flash);
    }

    #[test]
    fn reaction_feedback_weight_makes_launch_and_bounce_read_heavier() {
        let stagger = reaction_feedback_weight(reaction_profile_for_family(
            ReactionFamilyId::ShortStandingStagger,
        ));
        let launch =
            reaction_feedback_weight(reaction_profile_for_family(ReactionFamilyId::LauncherDown));
        let bounce = reaction_feedback_weight(reaction_profile_for_family(
            ReactionFamilyId::GroundBounceDown,
        ));

        assert!(launch.hitstop_scale > stagger.hitstop_scale);
        assert!(launch.spark_scale > stagger.spark_scale);
        assert!(bounce.shake_add > launch.shake_add);
        assert!(bounce.priority_bonus > launch.priority_bonus);
    }

    #[test]
    fn player_successful_hit_camera_shake_requires_damage() {
        let base_shake = 0.25;

        assert_eq!(
            successful_hit_camera_shake(1, Some(AttackPayloadId::PigHamSlam), 10.0, base_shake),
            base_shake
        );
        assert_eq!(
            successful_hit_camera_shake(0, Some(AttackPayloadId::PigHamSlam), 0.0, base_shake),
            base_shake
        );
        assert!(
            successful_hit_camera_shake(0, Some(AttackPayloadId::PigHamSlam), 10.0, base_shake)
                > base_shake
        );
    }

    #[test]
    fn player_successful_hit_camera_shake_keeps_ultimates_heavy() {
        let base_shake = 0.25;
        let normal =
            successful_hit_camera_shake(0, Some(AttackPayloadId::PigHamSlam), 10.0, base_shake);
        let ultimate = successful_hit_camera_shake(
            0,
            Some(AttackPayloadId::PigUltimateBomb),
            10.0,
            base_shake,
        );

        assert!(ultimate > normal * 2.0);
    }

    #[test]
    fn player_successful_hit_camera_shake_caps_by_tier() {
        assert_eq!(
            successful_hit_camera_shake(0, Some(AttackPayloadId::PigHamSlam), 10.0, 10.0),
            PLAYER_DEFAULT_SUCCESS_HIT_CAMERA_SHAKE_MAX
        );
        assert_eq!(
            successful_hit_camera_shake(0, Some(AttackPayloadId::PigUltimateBomb), 10.0, 10.0),
            PLAYER_ULTIMATE_SUCCESS_HIT_CAMERA_SHAKE_MAX
        );
    }

    #[test]
    fn impact_profile_selects_reaction_from_force_and_lift() {
        let light = impact_profile(
            0,
            ImpactSource::FighterStrike,
            8.0,
            4.0,
            2.0,
            false,
            true,
            12.0,
            ImpactFeedbackIntensity::Light,
            ReactionFamilyId::ShortStandingStagger,
        );
        let heavy = impact_profile(
            0,
            ImpactSource::FighterStrike,
            16.0,
            8.2,
            4.0,
            true,
            true,
            24.0,
            ImpactFeedbackIntensity::Heavy,
            ReactionFamilyId::LauncherDown,
        );

        assert_eq!(light.reaction.kind, ReactionKind::Hitstun);
        assert_eq!(heavy.reaction.kind, ReactionKind::Launch);
        assert!(heavy.reaction.landing_aftermath.is_some());
    }

    #[test]
    fn authored_damage_profiles_branch_by_contact_context() {
        let mut profile = impact_profile(
            0,
            ImpactSource::FighterStrike,
            10.0,
            6.0,
            2.0,
            false,
            true,
            12.0,
            ImpactFeedbackIntensity::Light,
            ReactionFamilyId::ShortStandingStagger,
        );
        profile.damage_profile = DamageProfileId::AerialSpike;
        profile.power = 13.0;
        profile.str_scale = 0.8;

        let grounded = resolve_authored_damage(
            &profile,
            DamageContext {
                high_power: true,
                ..neutral_damage_context()
            },
        );
        let airborne = resolve_authored_damage(
            &profile,
            DamageContext {
                airborne: true,
                high_power: true,
                ..neutral_damage_context()
            },
        );
        let guarded = resolve_authored_damage(
            &profile,
            DamageContext {
                guarded: true,
                high_power: true,
                ..neutral_damage_context()
            },
        );

        assert!(airborne.health_damage > grounded.health_damage);
        assert!(guarded.health_damage < grounded.health_damage);
        assert!(guarded.guard_stamina_damage > 0.0);
    }

    #[test]
    fn damage_pipeline_uses_base_reduction_before_late_modifiers() {
        let mut profile = impact_profile(
            0,
            ImpactSource::FighterStrike,
            10.0,
            6.0,
            2.0,
            false,
            true,
            12.0,
            ImpactFeedbackIntensity::Heavy,
            ReactionFamilyId::GroundBounceDown,
        );
        profile.damage_profile = DamageProfileId::GroundBounce;
        profile.power = 10.0;
        profile.str_scale = 1.0;

        let ordinary = resolve_authored_damage(
            &profile,
            DamageContext {
                target_health: 20.0,
                ..neutral_damage_context()
            },
        );
        let downed = resolve_authored_damage(
            &profile,
            DamageContext {
                downed: true,
                target_health: 20.0,
                ..neutral_damage_context()
            },
        );

        assert_eq!(ordinary.health_damage, 10.0);
        assert_eq!(downed.health_damage, 1.0);
    }

    #[test]
    fn source_and_guard_state_conditions_participate_in_damage_branches() {
        let mut direct = impact_profile(
            0,
            ImpactSource::Projectile,
            20.0,
            4.0,
            1.0,
            false,
            true,
            12.0,
            ImpactFeedbackIntensity::Light,
            ReactionFamilyId::ShortStandingStagger,
        );
        direct.damage_profile = DamageProfileId::Direct;

        let projectile = resolve_authored_damage(
            &direct,
            DamageContext {
                source: ImpactSource::Projectile,
                ..neutral_damage_context()
            },
        );
        let fighter_strike = resolve_authored_damage(&direct, neutral_damage_context());

        assert!(projectile.health_damage < fighter_strike.health_damage);

        let weak_guard = DamageContext {
            guarded: true,
            guard_stamina: 4.0,
            incoming_guard_stamina_damage: 12.0,
            ..neutral_damage_context()
        };
        assert!(damage_condition_matches(
            DamageCondition::WeakGuard,
            weak_guard
        ));
        assert!(damage_condition_matches(
            DamageCondition::GuardBreak,
            weak_guard
        ));
    }

    #[test]
    fn element_accessory_and_style_conditions_participate_in_damage_branches() {
        let mut dash = impact_profile(
            0,
            ImpactSource::FighterStrike,
            10.0,
            6.0,
            1.0,
            false,
            true,
            12.0,
            ImpactFeedbackIntensity::Light,
            ReactionFamilyId::MediumStandingStagger,
        );
        dash.damage_profile = DamageProfileId::DashBody;

        let plain = resolve_authored_damage(&dash, neutral_damage_context());
        let element = resolve_authored_damage(
            &dash,
            DamageContext {
                element: DamageElement::Wind,
                ..neutral_damage_context()
            },
        );
        let accessory = resolve_authored_damage(
            &dash,
            DamageContext {
                attacker_equipment: Some(EquipmentKind::DashCoil),
                ..neutral_damage_context()
            },
        );
        let style = resolve_authored_damage(
            &dash,
            DamageContext {
                attacker_style: Some(FighterStyleKind::Vector),
                ..neutral_damage_context()
            },
        );

        assert!(element.health_damage > plain.health_damage);
        assert!(accessory.health_damage > plain.health_damage);
        assert!(style.health_damage > plain.health_damage);
        assert_eq!(
            accessory.side_effect.id,
            Some(DamageSideEffectId::AccessorySurge)
        );
        assert_eq!(style.side_effect.cue, Some("damage_vector_dash"));
    }

    #[test]
    fn defender_affinity_and_status_conditions_participate_in_damage_branches() {
        let mut dash = impact_profile(
            0,
            ImpactSource::FighterStrike,
            10.0,
            6.0,
            1.0,
            false,
            true,
            12.0,
            ImpactFeedbackIntensity::Light,
            ReactionFamilyId::MediumStandingStagger,
        );
        dash.damage_profile = DamageProfileId::DashBody;
        dash.element = DamageElement::Wind;

        let plain = resolve_authored_damage(
            &dash,
            DamageContext {
                element: DamageElement::Wind,
                ..neutral_damage_context()
            },
        );
        let resisted_affinity = element_affinity_for_defender(
            DamageElement::Wind,
            Some(FighterStyleKind::Vector),
            None,
            DamageTargetStatus::Standing,
        );
        let resisted_context = DamageContext {
            element: DamageElement::Wind,
            element_affinity: resisted_affinity,
            defender_style: Some(FighterStyleKind::Vector),
            ..neutral_damage_context()
        };
        let resisted = resolve_authored_damage(&dash, resisted_context);
        let weak_affinity = element_affinity_for_defender(
            DamageElement::Wind,
            Some(FighterStyleKind::Anchor),
            None,
            DamageTargetStatus::Standing,
        );
        let weak_context = DamageContext {
            element: DamageElement::Wind,
            element_affinity: weak_affinity,
            defender_style: Some(FighterStyleKind::Anchor),
            ..neutral_damage_context()
        };
        let weak = resolve_authored_damage(&dash, weak_context);

        assert_eq!(resisted_affinity, DamageElementAffinity::Resistant);
        assert_eq!(weak_affinity, DamageElementAffinity::Weak);
        assert!(resisted.health_damage < plain.health_damage);
        assert!(weak.health_damage > plain.health_damage);
        assert_eq!(
            resisted.side_effect.id,
            Some(DamageSideEffectId::ElementResist)
        );
        assert_eq!(
            weak.side_effect.id,
            Some(DamageSideEffectId::ElementWeakness)
        );
        assert!(damage_condition_matches(
            DamageCondition::DefenderStyle(FighterStyleKind::Anchor),
            weak_context
        ));

        let recovering = resolve_authored_damage(
            &dash,
            DamageContext {
                target_status: DamageTargetStatus::Recovering,
                ..neutral_damage_context()
            },
        );
        assert!(recovering.health_damage <= plain.health_damage);
    }

    #[test]
    fn absorbed_element_hits_use_authored_terminal_override() {
        let mut counter = impact_profile(
            0,
            ImpactSource::FighterStrike,
            12.0,
            5.0,
            1.0,
            false,
            true,
            20.0,
            ImpactFeedbackIntensity::Heavy,
            ReactionFamilyId::MediumStandingStagger,
        );
        counter.damage_profile = DamageProfileId::CounterBlow;
        counter.element = DamageElement::Shock;

        let affinity = element_affinity_for_defender(
            DamageElement::Shock,
            None,
            Some(EquipmentKind::CounterCell),
            DamageTargetStatus::Guarding,
        );
        let outcome = resolve_authored_damage(
            &counter,
            DamageContext {
                guarded: true,
                target_status: DamageTargetStatus::Guarding,
                element: DamageElement::Shock,
                element_affinity: affinity,
                defender_equipment: Some(EquipmentKind::CounterCell),
                ..neutral_damage_context()
            },
        );

        assert_eq!(affinity, DamageElementAffinity::Absorbed);
        assert_eq!(outcome.health_damage, 0.0);
        assert_eq!(outcome.terminal, DamageTerminalKind::NoHpLoss);
        assert_eq!(outcome.ignore_time_ms, 240);
        assert_eq!(
            outcome.side_effect.id,
            Some(DamageSideEffectId::ElementAbsorb)
        );
        assert!(outcome.side_effect.stamina_delta > 0.0);
    }

    #[test]
    fn stacked_affinity_uses_carryover_weighting() {
        let resisted = stacked_element_affinity_for_defender(
            DamageElement::Wind,
            Some(DamageElement::Strike),
            0.9,
            Some(FighterStyleKind::Vector),
            Some(EquipmentKind::DashCoil),
            DamageTargetStatus::Standing,
        );
        let weak = stacked_element_affinity_for_defender(
            DamageElement::Wind,
            Some(DamageElement::Shock),
            0.9,
            Some(FighterStyleKind::Anchor),
            Some(EquipmentKind::HeavySeal),
            DamageTargetStatus::Standing,
        );
        let neutral = stacked_element_affinity_for_defender(
            DamageElement::Neutral,
            Some(DamageElement::Earth),
            0.25,
            Some(FighterStyleKind::Anchor),
            None,
            DamageTargetStatus::Standing,
        );

        assert_eq!(resisted, DamageElementAffinity::Resistant);
        assert_eq!(weak, DamageElementAffinity::Weak);
        assert_eq!(neutral, DamageElementAffinity::Neutral);
    }

    #[test]
    fn element_carryover_tracks_and_decays_by_contact_outcome() {
        let mut stats = FighterStats::default();
        let outcome = DamageOutcome {
            health_damage: 9.0,
            guard_stamina_damage: 0.0,
            ignore_time_ms: 0,
            terminal: DamageTerminalKind::Normal,
            score_scale: 1.0,
            nonlethal: false,
            side_effect: DamageSideEffectOutcome {
                id: Some(DamageSideEffectId::ElementWeakness),
                cue: Some("damage_test"),
                invulnerability_ms: 0,
                stamina_delta: 0.0,
                hud_flash: 0.0,
                score_scale_add: 0.0,
            },
        };

        update_element_carryover(&mut stats, DamageElement::Shock, outcome, false);
        let first_strength = stats.element_carry_strength;
        assert_eq!(stats.element_carry, Some(DamageElement::Shock));
        assert!(first_strength > 0.35);
        assert!(stats.element_carry_timer > TickTimer::from_seconds_ceil(1.5));

        update_element_carryover(
            &mut stats,
            DamageElement::Neutral,
            DamageOutcome {
                side_effect: DamageSideEffectOutcome::none(),
                ..outcome
            },
            true,
        );
        assert!(stats.element_carry_strength < first_strength);
    }

    #[test]
    fn match_rules_feed_global_and_rule_damage_corrections() {
        let mut state = MatchState::default();
        let profile = impact_profile(
            0,
            ImpactSource::FighterStrike,
            10.0,
            4.0,
            1.0,
            false,
            true,
            12.0,
            ImpactFeedbackIntensity::Light,
            ReactionFamilyId::ShortStandingStagger,
        );
        let stats = FighterStats::default();
        let motor = FighterMotor::default();
        let action = FighterActionState::default();

        let team_context = damage_context(&state, &stats, &motor, &action, &profile, false, false);
        state.select_rule(2);
        let stock_context = damage_context(&state, &stats, &motor, &action, &profile, false, false);

        assert!(team_context.rule_damage_correction < 1.0);
        assert!(stock_context.global_damage_scale < team_context.global_damage_scale);
        assert!(stock_context.rule_damage_correction < team_context.rule_damage_correction);
    }

    #[test]
    fn terminal_damage_packaging_can_be_nonlethal_or_add_ignore_time() {
        let mut stats = FighterStats {
            health: 4.0,
            ..default()
        };
        let committed = commit_health_damage(
            &mut stats,
            DamageOutcome {
                health_damage: 10.0,
                guard_stamina_damage: 0.0,
                ignore_time_ms: 200,
                terminal: DamageTerminalKind::Nonlethal,
                score_scale: 1.0,
                nonlethal: true,
                side_effect: DamageSideEffectOutcome::none(),
            },
        );

        assert_eq!(committed, 3.0);
        assert_eq!(stats.health, 1.0);
        assert_eq!(stats.invulnerability, TickTimer::from_seconds_ceil(0.2));
    }

    #[test]
    fn damage_side_effects_are_authored_late_branches() {
        let mut profile = impact_profile(
            0,
            ImpactSource::FighterStrike,
            18.0,
            8.0,
            4.0,
            false,
            true,
            30.0,
            ImpactFeedbackIntensity::Heavy,
            ReactionFamilyId::LauncherDown,
        );
        profile.damage_profile = DamageProfileId::LauncherCommit;
        profile.power = 15.0;
        profile.str_scale = 0.8;

        let outcome = resolve_authored_damage(
            &profile,
            DamageContext {
                guarded: true,
                guard_stamina: 10.0,
                incoming_guard_stamina_damage: 30.0,
                high_power: true,
                ..neutral_damage_context()
            },
        );

        assert_eq!(
            outcome.side_effect.id,
            Some(DamageSideEffectId::GuardPressure)
        );
        assert_eq!(outcome.side_effect.cue, Some("damage_launcher_guard_crush"));
        assert_eq!(outcome.side_effect.invulnerability_ms, 120);
        assert!(outcome.score_scale > 1.0);

        let mut stats = FighterStats {
            stamina: 10.0,
            ..default()
        };
        apply_damage_side_effect_state(&mut stats, outcome);

        assert_eq!(stats.stamina, 2.0);
        assert_eq!(stats.invulnerability, TickTimer::from_seconds_ceil(0.12));
        assert_eq!(outcome.side_effect.cue, Some("damage_launcher_guard_crush"));
    }

    #[test]
    fn multiple_damage_side_effects_accumulate_in_order() {
        let mut profile = impact_profile(
            0,
            ImpactSource::FighterStrike,
            20.0,
            7.0,
            4.0,
            false,
            true,
            24.0,
            ImpactFeedbackIntensity::Heavy,
            ReactionFamilyId::AerialSpikeDown,
        );
        profile.damage_profile = DamageProfileId::AerialSpike;
        profile.power = 13.0;
        profile.str_scale = 0.8;

        let outcome = resolve_authored_damage(
            &profile,
            DamageContext {
                airborne: true,
                lethal_raw: true,
                high_power: true,
                ..neutral_damage_context()
            },
        );

        assert_eq!(
            outcome.side_effect.id,
            Some(DamageSideEffectId::LethalPunctuation)
        );
        assert_eq!(outcome.side_effect.invulnerability_ms, 160);
        assert!(outcome.side_effect.score_scale_add > 0.3);
        assert!(outcome.score_scale > 1.3);
    }

    #[test]
    fn payload_carries_authored_damage_profile_into_impact() {
        let payload = attack_payload_definition(AttackPayloadId::ComboFinisher);
        let config = attack_config_from_payload(
            payload.id,
            &FighterStyle {
                kind: crate::styles::FighterStyleKind::Anchor,
            },
        );

        assert_eq!(config.damage_profile, DamageProfileId::GroundBounce);
        assert_eq!(config.element, payload.element);
        assert_eq!(config.attacker_style, Some(FighterStyleKind::Anchor));
        assert_eq!(config.attacker_equipment, None);
        assert_eq!(config.power, payload.power);
        assert_eq!(config.str_scale, payload.str_scale);
    }

    #[test]
    fn direct_payload_impacts_preserve_authored_layers() {
        let profile = impact_profile_from_payload(
            0,
            ImpactSource::ItemBlast,
            AttackPayloadId::BombBlast,
            0.5,
            0.75,
            1.0,
            28.0,
        );

        assert_eq!(profile.payload_id, Some(AttackPayloadId::BombBlast));
        assert_eq!(profile.shape_id, Some(AttackShapeId::BombBurst));
        assert_eq!(profile.damage_profile, DamageProfileId::LauncherCommit);
        assert_eq!(profile.element, DamageElement::Blast);
        assert_eq!(profile.feedback.cue, "impact_pop_bomb");
        assert!(profile.damage < attack_payload_definition(AttackPayloadId::BombBlast).damage);
    }

    #[test]
    fn impact_profiles_scale_vertical_knockback_globally() {
        let payload = attack_payload_definition(AttackPayloadId::PigAirMeatSlam);
        let from_payload = impact_profile_from_payload(
            0,
            ImpactSource::FighterStrike,
            payload.id,
            1.0,
            1.0,
            1.0,
            24.0,
        );
        let from_hitbox = impact_profile_from_hitbox(&hitbox_for_payload(payload.id, Vec3::Z));
        let direct = impact_profile(
            0,
            ImpactSource::FighterStrike,
            8.0,
            4.0,
            2.0,
            false,
            true,
            12.0,
            ImpactFeedbackIntensity::Light,
            ReactionFamilyId::LightAirPop,
        );

        assert!(
            (from_payload.vertical_knockback
                - payload.vertical_knockback * VERTICAL_KNOCKBACK_SCALE)
                .abs()
                < 0.001
        );
        assert!((from_hitbox.vertical_knockback - from_payload.vertical_knockback).abs() < 0.001);
        assert!((direct.vertical_knockback - 2.0 * VERTICAL_KNOCKBACK_SCALE).abs() < 0.001);
    }

    #[test]
    fn x_payloads_spawn_fish_attack_surface_visuals() {
        let heavy_step = hitbox_scene_for_payload(AttackPayloadId::HeavyStep).unwrap();
        let beat1 = hitbox_scene_for_payload(AttackPayloadId::KiriageBeat1).unwrap();
        let beat2 = hitbox_scene_for_payload(AttackPayloadId::KiriageBeat2).unwrap();
        let jump_fish = hitbox_scene_for_payload(AttackPayloadId::JumpFishShot).unwrap();
        let pig_slam = hitbox_scene_for_payload(AttackPayloadId::PigHamSlam).unwrap();
        let pig_ham = hitbox_scene_for_payload(AttackPayloadId::PigHamLob).unwrap();
        let pig_swing = hitbox_scene_for_payload(AttackPayloadId::PigHamSwingFull).unwrap();
        let pig_launcher = hitbox_scene_for_payload(AttackPayloadId::PigHamLauncher).unwrap();
        let pig_air_slam = hitbox_scene_for_payload(AttackPayloadId::PigAirMeatSlam).unwrap();
        let pig_rolling_pin = hitbox_scene_for_payload(AttackPayloadId::PigRollingPinStep).unwrap();
        let pig_catch = hitbox_scene_for_payload(AttackPayloadId::PigUltimateCatch).unwrap();
        let pig_burger = hitbox_scene_for_payload(AttackPayloadId::PigUltimateBomb).unwrap();
        let penguin_fish = hitbox_scene_for_payload(AttackPayloadId::PenguinFishSlap1).unwrap();
        let penguin_bones =
            hitbox_scene_for_payload(AttackPayloadId::PenguinFrozenFishDive).unwrap();
        let penguin_pan = hitbox_scene_for_payload(AttackPayloadId::PenguinPanBonk).unwrap();
        let penguin_sled = hitbox_scene_for_payload(AttackPayloadId::PenguinSledScoop).unwrap();
        let penguin_popsicle =
            hitbox_scene_for_payload(AttackPayloadId::PenguinPopsiclePeck).unwrap();
        let penguin_snowflake =
            hitbox_scene_for_payload(AttackPayloadId::PenguinUltimateBomb).unwrap();
        let chick_scoot = hitbox_scene_for_payload(AttackPayloadId::ChickShellScoot).unwrap();
        let chick_scramble = hitbox_scene_for_payload(AttackPayloadId::ChickShellScramble).unwrap();

        assert_eq!(heavy_step.asset_path, FOOD_FISH_ASSET);
        assert_eq!(beat1.asset_path, FOOD_FISH_ASSET);
        assert_eq!(beat2.asset_path, FOOD_FISH_ASSET);
        assert_eq!(jump_fish.asset_path, FOOD_FISH_ASSET);
        assert_eq!(pig_slam.asset_path, FOOD_WHOLE_HAM_ASSET);
        assert_eq!(pig_ham.asset_path, FOOD_WHOLE_HAM_ASSET);
        assert_eq!(pig_swing.asset_path, FOOD_WHOLE_HAM_ASSET);
        assert_eq!(pig_launcher.asset_path, FOOD_WHOLE_HAM_ASSET);
        assert_eq!(pig_air_slam.asset_path, FOOD_WHOLE_HAM_ASSET);
        assert_eq!(pig_rolling_pin.asset_path, FOOD_ROLLING_PIN_ASSET);
        assert_eq!(pig_catch.asset_path, FOOD_WHOLE_HAM_ASSET);
        assert_eq!(pig_burger.asset_path, FOOD_BURGER_ASSET);
        assert_eq!(penguin_fish.asset_path, FOOD_FISH_ASSET);
        assert_eq!(penguin_bones.asset_path, PENGUIN_FISH_BONES_ASSET);
        assert_eq!(penguin_pan.asset_path, FOOD_FRYING_PAN_ASSET);
        assert_eq!(penguin_sled.asset_path, HOLIDAY_SLED_ASSET);
        assert_eq!(penguin_popsicle.asset_path, PENGUIN_SPRING_ASSET);
        assert_eq!(penguin_snowflake.asset_path, PENGUIN_SNOWFLAKE_ASSET);
        assert_eq!(chick_scoot.asset_path, CHICK_EGG_HALF_ASSET);
        assert_eq!(chick_scramble.asset_path, CHICK_EGG_HALF_ASSET);
        assert_eq!(chick_scoot.orientation, HitboxSceneOrientation::Facing);
        assert!(chick_scramble.scale > 2.0);
        assert_eq!(
            pig_swing.orientation,
            HitboxSceneOrientation::HamMeatOutwardHalfCircle
        );
        assert_eq!(
            pig_air_slam.orientation,
            HitboxSceneOrientation::HamMeatOutwardVerticalArc
        );
        assert_eq!(pig_swing.yaw_offset, 0.0);
        assert_eq!(pig_launcher.orientation, HitboxSceneOrientation::Facing);
        assert_eq!(pig_ham.orientation, HitboxSceneOrientation::Facing);
        assert_eq!(heavy_step.scale, FOOD_FISH_SCALE);
        assert!(beat1.scale > 2.0);
        assert!(pig_slam.scale > pig_ham.scale);
        assert!(pig_slam.pitch < pig_ham.pitch);
        assert!(pig_slam.lift < pig_ham.lift);
        assert!(pig_ham.scale > 2.0);
        assert!(jump_fish.pitch.abs() < beat1.pitch.abs());
        assert!(hitbox_scene_visual_lifetime(0.1) > 0.1);
        assert_eq!(hitbox_scene_path_duration(0.1), 0.1);
        assert!(std::path::Path::new("assets/food/kenney_food_kit/Textures/colormap.png").exists());
        assert!(std::path::Path::new("assets/food/kenney_food_kit/whole-ham.glb").exists());
        assert!(std::path::Path::new("assets/food/kenney_food_kit/rollingPin.glb").exists());
        assert!(std::path::Path::new("assets/food/kenney_food_kit/burger.glb").exists());
        assert!(std::path::Path::new("assets/food/kenney_food_kit/frying-pan.glb").exists());
        assert!(std::path::Path::new("assets/food/kenney_food_kit/fish-bones.glb").exists());
        assert!(std::path::Path::new("assets/food/kenney_food_kit/popsicle.glb").exists());
        assert!(std::path::Path::new("assets/holiday/kenney_holiday_kit/sled.glb").exists());
        assert!(std::path::Path::new("assets/holiday/kenney_holiday_kit/snowflake-a.glb").exists());
        assert!(std::path::Path::new("assets/holiday/kenney_holiday_kit/snow-pile.glb").exists());
        assert!(std::path::Path::new("assets/food/kenney_food_kit/egg-half.glb").exists());
        assert!(hitbox_scene_for_payload(AttackPayloadId::AsBeat1).is_none());
    }

    #[test]
    fn pig_x_ham_visual_points_meat_outward_on_half_circle() {
        let def = hitbox_scene_for_payload(AttackPayloadId::PigHamSwingFull).unwrap();
        let shape = attack_shape_definition(AttackShapeId::PigHalfCircleSwing);
        let base = Vec3::new(2.0, 0.0, 0.0);
        let facing = Vec3::Z;
        let right = Vec3::X;
        let start_center = shape_center(
            base,
            facing,
            shape.range,
            shape.vertical_offset_scale,
            shape.path,
            0.0,
        );
        let end_center = shape_center(
            base,
            facing,
            shape.range,
            shape.vertical_offset_scale,
            shape.path,
            1.0,
        );
        let start_outward =
            hitbox_scene_visual_facing(start_center, base, facing, shape.range, def);
        let end_outward = hitbox_scene_visual_facing(end_center, base, facing, shape.range, def);
        let start_transform =
            hitbox_scene_world_transform(start_center, base, facing, shape.range, def);
        let start_meat_axis = start_transform.rotation * Vec3::Z;

        assert!(start_outward.dot(right) < -0.92);
        assert!(end_outward.dot(right) > 0.88);
        assert!(start_meat_axis.dot(start_outward) > 0.98);
    }

    #[test]
    fn pig_air_ham_visual_rotates_from_overhead_to_floor_like_axe() {
        let def = hitbox_scene_for_payload(AttackPayloadId::PigAirMeatSlam).unwrap();
        let shape = attack_shape_definition(AttackShapeId::PigAirMeatSlam);
        let arena = &crate::arena_defs::arena_definitions()[0];
        let base = Vec3::ZERO;
        let facing = Vec3::Z;
        let start_center = shape_center(
            base,
            facing,
            shape.range,
            shape.vertical_offset_scale,
            shape.path,
            0.0,
        );
        let end_center = shape_center_with_ground_path_end(
            base,
            facing,
            shape.range,
            shape.vertical_offset_scale,
            shape.path,
            1.0,
            true,
            JUMP_HEAVY_FISH_GROUND_CLEARANCE,
            arena,
        );
        let start_transform =
            hitbox_scene_world_transform(start_center, base, facing, shape.range, def);
        let end_transform =
            hitbox_scene_world_transform(end_center, base, facing, shape.range, def);
        let start_meat_axis = start_transform.rotation * Vec3::Z;
        let end_meat_axis = end_transform.rotation * Vec3::Z;

        assert!(start_center.y > FIGHTER_HEIGHT);
        let end_ground =
            ground_support_for_arena_with_radius(arena, end_center.x, end_center.z, 0.0)
                .height()
                .unwrap_or(ARENA_TOP_Y);
        assert!(end_center.y <= end_ground + JUMP_HEAVY_FISH_GROUND_CLEARANCE + 0.001);
        assert!(start_meat_axis.dot(Vec3::Y) > 0.38);
        assert!(end_meat_axis.dot(Vec3::NEG_Y) > 0.0);
        assert!(end_meat_axis.dot(facing) > 0.2);
    }

    #[test]
    fn pig_ham_slam_profile_knocks_down_without_knockback() {
        let hitbox = hitbox_for_payload(AttackPayloadId::PigHamSlam, Vec3::Z);
        let profile = impact_profile_from_hitbox(&hitbox);

        assert_eq!(profile.reaction_family, ReactionFamilyId::GroundedDownGetup);
        assert_eq!(profile.reaction.id, ReactionFamilyId::GroundedDownGetup);
        assert_eq!(profile.knockback, 0.0);
        assert_eq!(profile.vertical_knockback, 0.0);
        assert!(profile.force_knockdown);
        assert_eq!(profile.feedback.cue, "impact_pig_ham_slam");
    }

    #[test]
    fn equipment_trigger_reports_kind_and_starts_cooldown() {
        let mut equipment = FighterEquipment::new(EquipmentKind::DashCoil);
        let loadout = LoadoutContext::new(FighterStyleKind::Vector, equipment.kind);
        let shape = attack_shape_definition(AttackShapeId::DashShoulder);
        let mut config = AttackConfig {
            active: 0.1,
            payload_id: AttackPayloadId::DashStrike,
            shape_id: AttackShapeId::DashShoulder,
            reaction_family: ReactionFamilyId::LightAirPop,
            damage_profile: DamageProfileId::DashBody,
            element: DamageElement::Wind,
            attacker_equipment: None,
            attacker_style: Some(FighterStyleKind::Vector),
            power: 11.0,
            str_scale: 0.7,
            damage: 10.0,
            knockback: 5.0,
            vertical_knockback: 2.2,
            guardable: true,
            range: 1.0,
            radius: 0.5,
            vertical_offset_scale: 0.56,
            parented: shape.parented,
            path: shape.path,
            impact_cue: "impact_test",
            hitstop_scale: 1.0,
            shake_scale: 1.0,
            feedback_priority_bonus: 0,
            kind: AttackKind::Dash,
        };

        let triggered = apply_loadout_to_attack(
            FighterAction::DashAttack,
            loadout,
            &mut equipment,
            &mut config,
        )
        .expect("dash coil should trigger");
        assert_eq!(
            triggered.source,
            LoadoutModifierSource::Equipment(EquipmentKind::DashCoil)
        );
        assert_eq!(triggered.cue, "equip_dash_coil");
        assert!(equipment.cooldown.active());
        assert!(config.knockback > 5.0);
        assert_eq!(config.attacker_equipment, Some(EquipmentKind::DashCoil));
        assert_eq!(
            apply_loadout_to_attack(
                FighterAction::DashAttack,
                loadout,
                &mut equipment,
                &mut config
            ),
            None
        );
    }

    #[test]
    fn shape_path_sampling_moves_contact_over_lifetime() {
        let shape = attack_shape_definition(AttackShapeId::DashShoulder);
        let start = shape_center(
            Vec3::ZERO,
            Vec3::Z,
            shape.range,
            shape.vertical_offset_scale,
            shape.path,
            0.0,
        );
        let end = shape_center(
            Vec3::ZERO,
            Vec3::Z,
            shape.range,
            shape.vertical_offset_scale,
            shape.path,
            1.0,
        );

        assert!(end.z > start.z);
        assert_eq!(start.y, FIGHTER_HEIGHT * shape.vertical_offset_scale);
    }

    #[test]
    fn jump_fish_path_grounds_endpoint_after_shortened_arc() {
        let shape = attack_shape_definition(AttackShapeId::AirFishShot);
        let base = Vec3::new(0.0, ARENA_TOP_Y + 2.5, 0.0);
        let arena = &crate::arena_defs::arena_definitions()[0];
        let start = shape_center_with_ground_path_end(
            base,
            Vec3::Z,
            shape.range,
            shape.vertical_offset_scale,
            shape.path,
            0.0,
            true,
            JUMP_HEAVY_FISH_GROUND_CLEARANCE,
            arena,
        );
        let ungrounded_end = shape_center(
            base,
            Vec3::Z,
            shape.range,
            shape.vertical_offset_scale,
            shape.path,
            1.0,
        );
        let grounded_end = shape_center_with_ground_path_end(
            base,
            Vec3::Z,
            shape.range,
            shape.vertical_offset_scale,
            shape.path,
            1.0,
            true,
            JUMP_HEAVY_FISH_GROUND_CLEARANCE,
            arena,
        );
        let expected_ground = ground_support_for_arena_with_radius(
            arena,
            ungrounded_end.x,
            ungrounded_end.z,
            FIGHTER_RADIUS,
        )
        .height()
        .unwrap_or(ARENA_TOP_Y)
            + JUMP_HEAVY_FISH_GROUND_CLEARANCE;

        assert_eq!(
            start.y,
            shape_center(
                base,
                Vec3::Z,
                shape.range,
                shape.vertical_offset_scale,
                shape.path,
                0.0
            )
            .y
        );
        assert!(ungrounded_end.y > grounded_end.y + 1.0);
        assert!((grounded_end.y - expected_ground).abs() < 0.001);
    }

    #[test]
    fn payload_feedback_overrides_generic_impact_profile() {
        let payload = attack_payload_definition(AttackPayloadId::JumpSpike);
        let shape = attack_shape_definition(payload.shape_id);
        let hitbox = Hitbox {
            owner: FighterId::ZERO,
            kind: payload.kind,
            payload_id: Some(payload.id),
            attacker_character: Some(CharacterKind::Cat),
            technique_id: Some(TechniqueId::CatJumpAttack),
            hit_effect: Some(HitImpactEffectId::GenericHeavy),
            shape_id: payload.shape_id,
            reaction_family: payload.reaction_family,
            damage_profile: payload.damage_profile,
            element: payload.element,
            attacker_equipment: None,
            attacker_style: Some(FighterStyleKind::Anchor),
            power: payload.power,
            str_scale: payload.str_scale,
            damage: payload.damage,
            knockback: payload.knockback,
            vertical_knockback: payload.vertical_knockback,
            guardable: payload.guardable,
            base_radius: shape.radius,
            radius: shape.radius,
            lifetime: TickTimer::from_millis_ceil(payload.time_ms),
            elapsed: ElapsedTicks::ZERO,
            total_lifetime: milliseconds_to_ticks_ceil(payload.time_ms),
            spawn_origin: Vec3::ZERO,
            facing: Vec3::Z,
            base_range: shape.range,
            range: shape.range,
            scales_with_owner_size: true,
            vertical_offset_scale: shape.vertical_offset_scale,
            parented: shape.parented,
            path: shape.path,
            expires_on_owner_landing: hitbox_landing_linger(payload.id).is_some(),
            landing_linger: hitbox_landing_linger(payload.id).unwrap_or(TickTimer::ZERO),
            landing_linger_started: false,
            ground_path_end: hitbox_ground_path_clearance(payload.id).is_some(),
            ground_path_clearance: hitbox_ground_path_clearance(payload.id).unwrap_or(0.0),
            impact_cue: payload.impact_cue,
            hitstop_scale: payload.hitstop_scale,
            shake_scale: payload.shake_scale,
            feedback_priority_bonus: payload.feedback_priority_bonus,
            already_hit: FighterHitMask::default(),
        };

        let profile = impact_profile_from_hitbox(&hitbox);
        assert_eq!(profile.feedback.cue, "impact_jump_spike");
        assert_eq!(profile.reaction.id, ReactionFamilyId::AerialSpikeDown);
        assert_eq!(profile.element, payload.element);
        assert_eq!(profile.attacker_character, Some(CharacterKind::Cat));
        assert_eq!(profile.technique_id, Some(TechniqueId::CatJumpAttack));
        assert_eq!(profile.hit_effect, Some(HitImpactEffectId::GenericHeavy));
        assert_eq!(profile.attacker_style, Some(FighterStyleKind::Anchor));
        assert!(
            profile.feedback.hitstop
                > impact_feedback_profile(
                    ImpactSource::FighterStrike,
                    ImpactFeedbackIntensity::Light
                )
                .hitstop
        );
    }
}

#[cfg(all(
    feature = "dev-hot-reload",
    not(feature = "shipping"),
    not(target_arch = "wasm32")
))]
pub fn draw_debug_gizmos(
    state: Res<MatchState>,
    character_catalog: Res<CharacterMoveCatalog>,
    fighters: Query<
        (
            &SimPosition,
            &FighterMotor,
            &FighterCharacter,
            &FighterStats,
            &FighterActionState,
        ),
        With<Fighter>,
    >,
    hitboxes: Query<(&SimPosition, &Hitbox)>,
    mut gizmos: Gizmos,
) {
    if !state.debug_hitboxes {
        return;
    }

    for (position, motor, character, stats, action) in &fighters {
        if matches!(
            action.action,
            FighterAction::RingOut | FighterAction::Respawning
        ) {
            continue;
        }
        draw_body_box_gizmo(
            &mut gizmos,
            fighter_hurt_box(position, motor, character, stats, &character_catalog),
            Color::srgba(0.1, 0.9, 1.0, 0.9),
        );
    }

    for (position, hitbox) in &hitboxes {
        gizmos.sphere(
            position.translation,
            hitbox.radius,
            if hitbox.kind.is_heavy_feedback() {
                Color::srgba(1.0, 0.25, 0.1, 0.95)
            } else {
                Color::srgba(1.0, 0.9, 0.1, 0.95)
            },
        );
    }
}

#[cfg(all(
    feature = "dev-hot-reload",
    not(feature = "shipping"),
    not(target_arch = "wasm32")
))]
fn draw_body_box_gizmo(gizmos: &mut Gizmos, body: FighterBodyBox, color: Color) {
    let corners = body.corners();
    for (start, end) in [
        (0, 1),
        (1, 2),
        (2, 3),
        (3, 0),
        (4, 5),
        (5, 6),
        (6, 7),
        (7, 4),
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7),
    ] {
        gizmos.line(corners[start], corners[end], color);
    }
}

pub struct MoveDefinition {
    pub active: f32,
    pub payload_id: AttackPayloadId,
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
    pub range: f32,
    pub radius: f32,
    pub vertical_offset_scale: f32,
    pub parented: bool,
    pub path: &'static [[f32; 3]],
    pub impact_cue: &'static str,
    pub hitstop_scale: f32,
    pub shake_scale: f32,
    pub feedback_priority_bonus: u8,
    pub kind: AttackKind,
}

pub type AttackConfig = MoveDefinition;

#[derive(Clone, Copy, Debug, PartialEq)]
struct AppliedLoadoutAttackModifier {
    source: LoadoutModifierSource,
    cue: &'static str,
}

fn apply_loadout_to_attack(
    action: FighterAction,
    loadout: LoadoutContext,
    equipment: &mut FighterEquipment,
    config: &mut AttackConfig,
) -> Option<AppliedLoadoutAttackModifier> {
    let mut applied = None;
    for modifier in loadout_attack_modifiers(loadout, action, !equipment.cooldown.active()) {
        config.damage *= modifier.damage_scale;
        config.knockback *= modifier.knockback_scale;
        config.vertical_knockback *= modifier.vertical_knockback_scale;
        config.hitstop_scale *= modifier.hitstop_scale;
        config.shake_scale *= modifier.shake_scale;
        config.feedback_priority_bonus = config
            .feedback_priority_bonus
            .saturating_add(modifier.feedback_priority_bonus);
        if let Some(reaction_family) = modifier.reaction_override {
            config.reaction_family = reaction_family;
        }
        if let Some(shape_id) = modifier.shape_override {
            let shape = attack_shape_definition(shape_id);
            config.shape_id = shape.id;
            config.range = shape.range;
            config.radius = shape.radius;
            config.vertical_offset_scale = shape.vertical_offset_scale;
            config.parented = shape.parented;
            config.path = shape.path;
        }
        if matches!(modifier.source, LoadoutModifierSource::Equipment(_)) {
            config.attacker_equipment = Some(equipment.kind);
            equipment.cooldown = TickTimer::from_seconds_ceil(modifier.cooldown);
        }
        applied = Some(AppliedLoadoutAttackModifier {
            source: modifier.source,
            cue: modifier.cue,
        });
    }
    applied
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct TimelineFeedbackProfile {
    priority: u8,
    shake: f32,
    hud_flash: f32,
}

fn timeline_feedback_profile(phase: FeedbackPhase, cue: &'static str) -> TimelineFeedbackProfile {
    let mut profile = TimelineFeedbackProfile {
        priority: phase_priority(phase),
        shake: match phase {
            FeedbackPhase::Startup => 0.035,
            FeedbackPhase::PreHit => 0.07,
            FeedbackPhase::Impact => 0.12,
            FeedbackPhase::Aftermath => 0.08,
        },
        hud_flash: match phase {
            FeedbackPhase::Startup => 0.04,
            FeedbackPhase::PreHit => 0.07,
            FeedbackPhase::Impact => 0.12,
            FeedbackPhase::Aftermath => 0.08,
        },
    };
    match cue {
        "startup_kiriage" | "trail_kiriage" | "post_hit_kiriage" => {
            profile.priority = profile.priority.saturating_add(5);
            profile.shake += 0.06;
            profile.hud_flash += 0.05;
        }
        "startup_combo_finisher" | "trail_combo_finisher" | "combo_finisher_floor_pulse" => {
            profile.priority = profile.priority.saturating_add(4);
            profile.shake += 0.045;
            profile.hud_flash += 0.04;
        }
        "landing_recovery_stick"
        | "landing_recovery_release"
        | "startup_quick_stand"
        | "quick_stand_ready"
        | "startup_recovery_roll"
        | "travel_recovery_roll" => {
            profile.priority = profile.priority.saturating_add(3);
            profile.shake += 0.03;
            profile.hud_flash += 0.035;
        }
        _ => {}
    }
    profile
}

fn phase_priority(phase: FeedbackPhase) -> u8 {
    match phase {
        FeedbackPhase::Startup => 18,
        FeedbackPhase::PreHit => 24,
        FeedbackPhase::Impact => 36,
        FeedbackPhase::Aftermath => 30,
    }
}

#[cfg(test)]
pub fn attack_config_from_payload(
    payload_id: AttackPayloadId,
    style: &FighterStyle,
) -> AttackConfig {
    let payload = attack_payload_for_style(attack_payload_definition(payload_id), style);
    attack_config_from_payload_def(payload_id, payload, style)
}

pub fn attack_config_from_payload_with_feel(
    payload_id: AttackPayloadId,
    style: &FighterStyle,
    feel: &CombatFeelTuning,
) -> AttackConfig {
    let payload = attack_payload_for_style(
        feel.apply_payload(attack_payload_definition(payload_id)),
        style,
    );
    attack_config_from_payload_def(payload_id, payload, style)
}

fn attack_config_from_payload_def(
    payload_id: AttackPayloadId,
    payload: AttackPayloadDef,
    style: &FighterStyle,
) -> AttackConfig {
    let shape = attack_shape_definition(payload.shape_id);

    AttackConfig {
        active: payload.time_ms as f32 / 1000.0,
        payload_id,
        shape_id: shape.id,
        reaction_family: payload.reaction_family,
        damage_profile: payload.damage_profile,
        element: payload.element,
        attacker_equipment: None,
        attacker_style: Some(style.kind),
        power: payload.power,
        str_scale: payload.str_scale,
        damage: payload.damage,
        knockback: payload.knockback,
        vertical_knockback: payload.vertical_knockback,
        guardable: payload.guardable,
        range: shape.range,
        radius: shape.radius,
        vertical_offset_scale: shape.vertical_offset_scale,
        parented: shape.parented,
        path: shape.path,
        impact_cue: payload.impact_cue,
        hitstop_scale: payload.hitstop_scale,
        shake_scale: payload.shake_scale,
        feedback_priority_bonus: payload.feedback_priority_bonus,
        kind: payload.kind,
    }
}

fn attack_payload_for_style(payload: AttackPayloadDef, style: &FighterStyle) -> AttackPayloadDef {
    let tuning = style_tuning(style.kind);
    AttackPayloadDef {
        damage: payload.damage * tuning.damage,
        knockback: payload.knockback * tuning.knockback,
        ..payload
    }
}

fn shape_center(
    base: Vec3,
    facing: Vec3,
    range: f32,
    vertical_offset_scale: f32,
    path: &[[f32; 3]],
    progress: f32,
) -> Vec3 {
    let local = sample_shape_path(path, progress);
    let forward = crate::canonical_math::vec3_normalize_or_zero(facing);
    let right =
        crate::canonical_math::vec3_normalize_or_zero(Vec3::new(forward.z, 0.0, -forward.x));
    base + Vec3::Y * (FIGHTER_HEIGHT * vertical_offset_scale + local.y)
        + forward * (range + local.z)
        + right * local.x
}

fn shape_center_with_ground_path_end(
    base: Vec3,
    facing: Vec3,
    range: f32,
    vertical_offset_scale: f32,
    path: &[[f32; 3]],
    progress: f32,
    ground_path_end: bool,
    ground_path_clearance: f32,
    arena: &ArenaDefinition,
) -> Vec3 {
    let mut center = shape_center(base, facing, range, vertical_offset_scale, path, progress);
    if ground_path_end {
        let end = shape_center(base, facing, range, vertical_offset_scale, path, 1.0);
        let ground_y = ground_support_for_arena_with_radius(arena, end.x, end.z, FIGHTER_RADIUS)
            .height()
            .unwrap_or(ARENA_TOP_Y);
        center.y = center
            .y
            .lerp(ground_y + ground_path_clearance, progress.clamp(0.0, 1.0));
    }
    center
}

fn sample_shape_path(path: &[[f32; 3]], progress: f32) -> Vec3 {
    if path.is_empty() {
        return Vec3::ZERO;
    }
    if path.len() == 1 {
        return path_point(path[0]);
    }

    let scaled = progress.clamp(0.0, 1.0) * (path.len() - 1) as f32;
    let start = scaled.floor() as usize;
    let end = (start + 1).min(path.len() - 1);
    let t = scaled - start as f32;
    path_point(path[start]).lerp(path_point(path[end]), t)
}

fn path_point(point: [f32; 3]) -> Vec3 {
    Vec3::new(point[0], point[1], point[2])
}
