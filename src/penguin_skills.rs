use crate::arena::ground_support_for_arena_with_radius;
use crate::arena_defs::{ActiveArena, ArenaDefinition};
use crate::bee_skills::BeeSkillTargetSnapshot;
use crate::canonical_math;
use crate::characters::CharacterKind;
use crate::combat::{
    HitEffects, ImpactSource, can_receive_impact, impact_profile_from_payload_with_feel,
};
use crate::components::{Fighter, FighterActionState, FighterMotor, FighterStats, SimPosition};
use crate::constants::{ARENA_TOP_Y, FIGHTER_HEIGHT, FIGHTER_RADIUS};
use crate::contact_arbitration::{
    ContactBuffer, ContactFlags, ContactOutcomeKind, ContactPhase, ContactRecord, ContactSourceKind,
};
use crate::determinism::{FighterHitMask, FighterId, SimEntityId, SimEntityKind};
use crate::ecs_identity::{
    SIM_ENTITY_POOL_CAPACITIES, SimulationIdentityAllocator, StableSimEntity, despawn_stable,
};
use crate::effects::{EffectAssets, FeedbackPackageId, spawn_feedback_package};
use crate::feel::CombatFeelTuning;
use crate::game_state::{Hitstop, MatchState};
use crate::rollback::RollbackEventDiscard;
use crate::sim_event::{
    AbilityLifecycleEvent, EventEmitError, MAX_SIM_EVENTS_PER_TICK, SIM_EVENT_HISTORY_TICKS,
    SimEvent, SimEventId, SimEventKind, SimEventSource, TickEventBuffer,
};
use crate::simulation::{ElapsedTicks, SIM_HZ_U32, SimTick, TickTimer};
use crate::styles::FighterStyleKind;
use crate::techniques::{AttackPayloadId, AttackShapeId, PenguinSkillId};
use arrayvec::ArrayVec;
use bevy::gltf::GltfAssetLabel;
use bevy::prelude::*;

pub const PENGUIN_FISH_BONES_ASSET: &str = "food/kenney_food_kit/fish-bones.glb";
pub const PENGUIN_POPSICLE_ASSET: &str = "food/kenney_food_kit/popsicle.glb";
pub const PENGUIN_SNOW_PILE_ASSET: &str = "holiday/kenney_holiday_kit/snow-pile.glb";
pub const PENGUIN_SNOWFLAKE_ASSET: &str = "holiday/kenney_holiday_kit/snowflake-a.glb";
pub const PENGUIN_SNOWMAN_ASSET: &str = "holiday/kenney_holiday_kit/snowman.glb";
pub const PENGUIN_ULTIMATE_SNOW_FLAT_LARGE_ASSET: &str =
    "holiday/kenney_holiday_kit/snow-flat-large.glb";
pub const PENGUIN_ULTIMATE_SNOW_FLAT_ASSET: &str = "holiday/kenney_holiday_kit/snow-flat.glb";
pub const PENGUIN_ICE_TILE_ASSET: &str =
    "tower_defense/kenney_tower_defense_kit/snow-tile-straight.glb";
pub const PENGUIN_SNOW_BUMP_ASSET: &str =
    "tower_defense/kenney_tower_defense_kit/snow-tile-bump.glb";
pub const PENGUIN_SNOW_HILL_ASSET: &str =
    "tower_defense/kenney_tower_defense_kit/snow-tile-hill.glb";
pub const PENGUIN_SNOWFORT_ASSET: &str =
    "tower_defense/kenney_tower_defense_kit/snow-wood-structure.glb";
pub const PENGUIN_CANNON_ASSET: &str = "tower_defense/kenney_tower_defense_kit/weapon-cannon.glb";
pub const PENGUIN_BOULDER_ASSET: &str =
    "tower_defense/kenney_tower_defense_kit/weapon-ammo-boulder.glb";
pub const PENGUIN_SNOW_SLOPE_ASSET: &str =
    "platformer/kenney_platformer_kit/block-snow-large-slope.glb";
pub const PENGUIN_SNOW_STEEP_SLOPE_ASSET: &str =
    "platformer/kenney_platformer_kit/block-snow-large-slope-steep.glb";
pub const PENGUIN_SPRING_ASSET: &str = "platformer/kenney_platformer_kit/spring.glb";

const PENGUIN_SKILL_LOCK_RANGE: f32 = 7.5;
const PENGUIN_SKILL_LOCK_CONE_DOT: f32 = 0.70710677;
const PENGUIN_FISH_TORPEDO_SPEED: f32 = 8.8;
const PENGUIN_FISH_TORPEDO_TURN_RATE: f32 = 5.0;
const PENGUIN_FISH_TORPEDO_LIFETIME: f32 = 0.86;
const PENGUIN_FISH_TORPEDO_RADIUS: f32 = 0.38;
const PENGUIN_POPSICLE_SPEED: f32 = 6.9;
const PENGUIN_POPSICLE_LIFT: f32 = 2.35;
const PENGUIN_POPSICLE_GRAVITY: f32 = 8.8;
const PENGUIN_POPSICLE_LIFETIME: f32 = 1.12;
const PENGUIN_POPSICLE_RADIUS: f32 = 0.34;
const PENGUIN_SLED_WAKE_SPEED: f32 = 2.6;
const PENGUIN_SLED_WAKE_LIFETIME: f32 = 1.18;
const PENGUIN_SLED_WAKE_RADIUS: f32 = 0.92;
const PENGUIN_SLED_WAKE_TICK: f32 = 0.38;
const PENGUIN_SLED_WAKE_DAMPING: f32 = 0.58;
const PENGUIN_SNOWFLAKE_SPEED: f32 = 7.2;
const PENGUIN_SNOWFLAKE_LIFETIME: f32 = 1.08;
const PENGUIN_SNOWFLAKE_RADIUS: f32 = 0.32;
const PENGUIN_SNOWFLAKE_SHOT_FORWARD: f32 = 0.58;
const PENGUIN_SNOWFLAKE_SHOT_HEIGHT: f32 = 0.86;
const PENGUIN_SNOWMAN_DROP_FORWARD: f32 = 1.25;
const PENGUIN_SNOWMAN_DROP_HEIGHT: f32 = 2.15;
const PENGUIN_SNOWMAN_DROP_INITIAL_FALL_SPEED: f32 = 0.5;
const PENGUIN_SNOWMAN_DROP_GRAVITY: f32 = 15.0;
const PENGUIN_SNOWMAN_DROP_LIFETIME: f32 = 1.05;
const PENGUIN_SNOWMAN_DROP_SIZE_MULTIPLIER: f32 = 1.5;
const PENGUIN_SNOWMAN_DROP_RADIUS: f32 = 0.85 * PENGUIN_SNOWMAN_DROP_SIZE_MULTIPLIER;
const PENGUIN_SNOWMAN_DROP_LAND_CLEARANCE: f32 = 0.16;
const PENGUIN_SNOWMAN_DROP_SNOW_TILE_COUNT: usize = 2;
const PENGUIN_BOULDER_SPEED: f32 = 8.4;
const PENGUIN_BOULDER_LIFETIME: f32 = 1.08;
const PENGUIN_BOULDER_RADIUS: f32 = 0.46;
const PENGUIN_BODY_SLAM_LIFETIME: f32 = 0.34;
const PENGUIN_BODY_SLAM_RADIUS: f32 = 1.55;
pub const PENGUIN_ICE_TRAIL_LIFETIME: f32 = 15.0;
const PENGUIN_ICE_TRAIL_RADIUS: f32 = 1.05;
const PENGUIN_ICE_TRAIL_CAP_PER_OWNER: usize = 18;
const PENGUIN_SURFACE_ENTITY_CAPACITY: usize =
    SIM_ENTITY_POOL_CAPACITIES[SimEntityKind::PenguinSurface.code() as usize] as usize;
const PENGUIN_ULTIMATE_ICE_FIELD_LIFETIME: f32 = 10.0;
const PENGUIN_ULTIMATE_ICE_FIELD_GRID_SIDE: i32 = 4;
const PENGUIN_ULTIMATE_ICE_FIELD_TILE_SPACING: f32 = 1.15;
const PENGUIN_ULTIMATE_ICE_FIELD_TILE_RADIUS: f32 = 0.78;
const PENGUIN_ULTIMATE_SNOW_FIELD_CLEARANCE: f32 = 0.024;
const PENGUIN_ULTIMATE_SNOW_FLAT_LARGE_SCALE: f32 = 0.88;
const PENGUIN_ULTIMATE_SNOW_FLAT_DETAIL_SCALE: f32 = 0.34;
const PENGUIN_SNOW_HILL_LIFETIME: f32 = 6.5;
const PENGUIN_SNOW_HILL_RADIUS: f32 = 1.08;
const PENGUIN_SNOW_HILL_LAUNCH: f32 = 4.2;
const PENGUIN_SNOW_HILL_PUSH: f32 = 3.4;
const PENGUIN_SNOW_HILL_RIDE_LIFT: f32 = 1.05;
const PENGUIN_SNOW_HILL_RIDE_PUSH: f32 = 7.2;
const PENGUIN_SNOW_HILL_RIDE_SLIDE: f32 = 0.42;
const PENGUIN_SNOW_HILL_RIDE_SPEED_LIMIT: f32 = 0.46;
const PENGUIN_SNOW_SLOPE_RIDE_LIFETIME: f32 = 2.4;
const PENGUIN_SNOW_SLOPE_RIDE_RADIUS: f32 = 1.08;
const PENGUIN_SNOW_SLOPE_RIDE_HALF_LENGTH: f32 = 0.72;
const PENGUIN_SNOW_SLOPE_RIDE_HALF_WIDTH: f32 = 0.72;
const PENGUIN_SNOW_SLOPE_RIDE_BASE_HEIGHT: f32 = 0.05;
const PENGUIN_SNOW_SLOPE_RIDE_HEIGHT: f32 = 0.52;
const PENGUIN_SNOW_SLOPE_RIDE_LIFT: f32 = 1.45;
const PENGUIN_SNOW_SLOPE_RIDE_PUSH: f32 = 4.8;
const PENGUIN_SNOW_SLOPE_RIDE_SLIDE: f32 = 0.62;
const PENGUIN_SNOW_SLOPE_RIDE_SPEED_LIMIT: f32 = 0.56;
const PENGUIN_SNOW_SLOPE_RIDE_EXIT_PROGRESS: f32 = 0.92;
const PENGUIN_SNOWFORT_LIFETIME: f32 = 1.55;
const PENGUIN_GLACIER_PARADE_LIFETIME: f32 = 15.0;
const PENGUIN_GLACIER_PARADE_TICK: f32 = 0.34;
const PENGUIN_SPRING_PAD_LIFETIME: f32 = 1.6;
const PENGUIN_SPRING_PAD_RADIUS: f32 = 0.64;
const PENGUIN_SPRING_PAD_LIFT: f32 = 5.6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FixedPenguinCollectionOverflow {
    collection: &'static str,
    capacity: usize,
}

fn try_push_fixed_penguin<T, const N: usize>(
    values: &mut ArrayVec<T, N>,
    value: T,
    collection: &'static str,
) -> Result<(), FixedPenguinCollectionOverflow> {
    values
        .try_push(value)
        .map_err(|_| FixedPenguinCollectionOverflow {
            collection,
            capacity: N,
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PenguinSkillKind {
    FishTorpedo,
    PopsicleBounce,
    SledWake,
    SnowflakeShard,
    SnowBoulder,
    SnowmanDrop,
    BodySlamShockwave,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PenguinSurfaceKind {
    IceTrailSegment,
    UltimateIceTile,
    SnowHillRamp,
    SnowSlopeRide,
    SnowfortCannon,
    GlacierTrailPrinter,
    SpringPad,
}

#[derive(Component)]
pub struct ActivePenguinSurface {
    pub kind: PenguinSurfaceKind,
    pub owner: FighterId,
    pub facing: Vec3,
    pub lifetime: TickTimer,
    pub age: ElapsedTicks,
    pub radius: f32,
    pub next_tick: TickTimer,
    pub already_touched: FighterHitMask,
    pub size_scale: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PenguinIceModifier {
    pub ground_friction_scale: f32,
    pub stop_friction_scale: f32,
    pub turn_brake_scale: f32,
    pub input_scale: f32,
    pub dash_slide_timer: f32,
    pub hard_slide: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SnowHillRampTouch {
    forward_push: f32,
    lift: f32,
    slide_timer: f32,
    speed_limit_timer: f32,
    owner_ride: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SnowSlopeRideContact {
    progress: f32,
    target_y: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PenguinSnowflakeSwap {
    pub snowflake: SimEntityId,
    pub penguin_destination: Vec3,
}

#[derive(Component)]
pub struct ActivePenguinSkill {
    pub kind: PenguinSkillKind,
    pub owner: FighterId,
    pub owner_style: FighterStyleKind,
    pub payload_id: AttackPayloadId,
    pub shape_id: AttackShapeId,
    pub source: ImpactSource,
    pub facing: Vec3,
    pub velocity: Vec3,
    pub target: Option<FighterId>,
    pub lifetime: TickTimer,
    pub age: ElapsedTicks,
    pub radius: f32,
    pub guard_stamina_damage: f32,
    pub repeat_interval: Option<TickTimer>,
    pub repeat_timer: Option<TickTimer>,
    pub already_hit: FighterHitMask,
    pub size_scale: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PenguinPresentationKind {
    Lifecycle {
        event: AbilityLifecycleEvent,
        position: Vec3,
        direction: Vec3,
        package: Option<FeedbackPackageId>,
        cue: Option<&'static str>,
        source: ImpactSource,
        priority: u8,
        hud_flash: Option<(FighterId, f32)>,
    },
    Impact {
        victim: FighterId,
        position: Vec3,
        direction: Vec3,
        package: FeedbackPackageId,
        cue: Option<&'static str>,
        source: ImpactSource,
        priority: u8,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PenguinPresentationIntent {
    pub event_id: SimEventId,
    pub entity: SimEntityId,
    pub kind: PenguinPresentationKind,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PenguinPresentationIntentSlot {
    tick: SimTick,
    len: u16,
    occupied: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PenguinPresentationIntentMetrics {
    pub recorded: u64,
    pub replaced: u64,
    pub rejected: u64,
    pub discarded: u64,
}

/// Fixed-capacity render sidecar keyed by deterministic simulation event ID.
#[derive(Resource, Clone, Debug)]
pub struct PenguinPresentationIntentJournal {
    slots: [PenguinPresentationIntentSlot; SIM_EVENT_HISTORY_TICKS],
    intents: Box<[Option<PenguinPresentationIntent>]>,
    len: usize,
    metrics: PenguinPresentationIntentMetrics,
}

impl Default for PenguinPresentationIntentJournal {
    fn default() -> Self {
        Self {
            slots: [PenguinPresentationIntentSlot::default(); SIM_EVENT_HISTORY_TICKS],
            intents: vec![None; SIM_EVENT_HISTORY_TICKS * MAX_SIM_EVENTS_PER_TICK]
                .into_boxed_slice(),
            len: 0,
            metrics: PenguinPresentationIntentMetrics::default(),
        }
    }
}

impl PenguinPresentationIntentJournal {
    const fn slot_index(tick: SimTick) -> usize {
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
    pub const fn capacity(&self) -> usize {
        SIM_EVENT_HISTORY_TICKS * MAX_SIM_EVENTS_PER_TICK
    }

    #[cfg(test)]
    pub const fn metrics(&self) -> PenguinPresentationIntentMetrics {
        self.metrics
    }

    pub(crate) fn record(
        &mut self,
        intent: PenguinPresentationIntent,
    ) -> Result<(), EventEmitError> {
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
            *slot = PenguinPresentationIntentSlot {
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

    pub(crate) fn get(&self, event_id: SimEventId) -> Option<PenguinPresentationIntent> {
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

    pub fn discard_after(&mut self, retained_through: SimTick) {
        for slot_index in 0..SIM_EVENT_HISTORY_TICKS {
            let slot = self.slots[slot_index];
            if !slot.occupied || slot.tick <= retained_through {
                continue;
            }
            let offset = Self::slot_offset(slot_index);
            for entry in &mut self.intents[offset..offset + MAX_SIM_EVENTS_PER_TICK] {
                *entry = None;
            }
            self.slots[slot_index] = PenguinPresentationIntentSlot::default();
            self.len = self.len.saturating_sub(usize::from(slot.len));
            self.metrics.discarded = self.metrics.discarded.saturating_add(u64::from(slot.len));
        }
    }
}

impl RollbackEventDiscard for PenguinPresentationIntentJournal {
    fn discard_after(&mut self, retained_through: SimTick) {
        Self::discard_after(self, retained_through);
    }
}

fn penguin_presentation_matches_event(event: SimEvent, intent: PenguinPresentationIntent) -> bool {
    if event.id != intent.event_id || event.id.source != SimEventSource::Entity(intent.entity) {
        return false;
    }
    match intent.kind {
        PenguinPresentationKind::Lifecycle {
            event: expected, ..
        } => matches!(
            event.kind,
            SimEventKind::AbilityLifecycle { entity, event }
                if entity == intent.entity && event == expected
        ),
        PenguinPresentationKind::Impact { victim, .. } => {
            matches!(
                event.kind,
                SimEventKind::HitConfirmed { victim: event_victim, .. }
                    if event_victim == victim
            ) || matches!(
                event.kind,
                SimEventKind::Guarded { defender, .. } if defender == victim
            )
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct PenguinPresentationResult {
    pub presented: bool,
    pub hud_flash: Option<(FighterId, f32)>,
}

/// Applies a validated render-only Penguin sidecar from the shared event
/// router. Canonical fighter state is never mutated here; the optional HUD
/// accent is returned for the render-world adapter to apply.
pub(crate) fn present_penguin_event(
    event: SimEvent,
    intent: PenguinPresentationIntent,
    commands: &mut Commands,
    effect_assets: &EffectAssets,
    feedback: &mut HitEffects,
) -> PenguinPresentationResult {
    if !penguin_presentation_matches_event(event, intent) {
        return PenguinPresentationResult::default();
    }

    let hud_flash = match intent.kind {
        PenguinPresentationKind::Lifecycle {
            position,
            direction,
            package,
            cue,
            source,
            priority,
            hud_flash,
            ..
        } => {
            if let Some(package) = package {
                spawn_feedback_package(commands, effect_assets, position, direction, package);
            }
            if let Some(cue) = cue {
                feedback.push_feedback_cue(cue, source, priority);
            }
            hud_flash
        }
        PenguinPresentationKind::Impact {
            position,
            direction,
            package,
            cue,
            source,
            priority,
            ..
        } => {
            spawn_feedback_package(commands, effect_assets, position, direction, package);
            if let Some(cue) = cue {
                feedback.push_feedback_cue(cue, source, priority);
            }
            None
        }
    };

    PenguinPresentationResult {
        presented: true,
        hud_flash,
    }
}

#[derive(Clone, Copy)]
struct QueuedPenguinPresentation {
    entity: SimEntityId,
    kind: PenguinPresentationKind,
}

fn queue_penguin_presentation(commands: &mut Commands, queued: QueuedPenguinPresentation) {
    commands.queue(move |world: &mut World| {
        let semantic = match queued.kind {
            PenguinPresentationKind::Lifecycle { event, .. } => SimEventKind::AbilityLifecycle {
                entity: queued.entity,
                event,
            },
            PenguinPresentationKind::Impact { .. } => return,
        };
        let Some(event_id) = world
            .get_resource_mut::<TickEventBuffer>()
            .and_then(|mut events| {
                events
                    .emit(SimEventSource::Entity(queued.entity), semantic)
                    .ok()
            })
        else {
            return;
        };

        if let Some(mut intents) = world.get_resource_mut::<PenguinPresentationIntentJournal>() {
            let _ = intents.record(PenguinPresentationIntent {
                event_id,
                entity: queued.entity,
                kind: queued.kind,
            });
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn queue_penguin_lifecycle(
    commands: &mut Commands,
    entity: SimEntityId,
    event: AbilityLifecycleEvent,
    position: Vec3,
    direction: Vec3,
    package: Option<FeedbackPackageId>,
    cue: Option<&'static str>,
    source: ImpactSource,
    priority: u8,
    hud_flash: Option<(FighterId, f32)>,
) {
    queue_penguin_presentation(
        commands,
        QueuedPenguinPresentation {
            entity,
            kind: PenguinPresentationKind::Lifecycle {
                event,
                position,
                direction,
                package,
                cue,
                source,
                priority,
                hud_flash,
            },
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn emit_penguin_lifecycle(
    sim_events: &mut TickEventBuffer,
    presentation_intents: Option<&mut PenguinPresentationIntentJournal>,
    entity: SimEntityId,
    event: AbilityLifecycleEvent,
    position: Vec3,
    direction: Vec3,
    package: Option<FeedbackPackageId>,
    cue: Option<&'static str>,
    source: ImpactSource,
    priority: u8,
    hud_flash: Option<(FighterId, f32)>,
) {
    let Ok(event_id) = sim_events.emit(
        SimEventSource::Entity(entity),
        SimEventKind::AbilityLifecycle { entity, event },
    ) else {
        return;
    };
    if let Some(intents) = presentation_intents {
        let _ = intents.record(PenguinPresentationIntent {
            event_id,
            entity,
            kind: PenguinPresentationKind::Lifecycle {
                event,
                position,
                direction,
                package,
                cue,
                source,
                priority,
                hud_flash,
            },
        });
    }
}

/// Records the presentation sidecar for the Snowflake Swap performed by the
/// combat timeline. The snowflake remains the stable semantic source even when
/// the canonical entity is despawned in the same command batch.
pub(crate) fn queue_penguin_snowflake_swap_presentation(
    commands: &mut Commands,
    snowflake: SimEntityId,
    owner: FighterId,
    position: Vec3,
    direction: Vec3,
) {
    queue_penguin_lifecycle(
        commands,
        snowflake,
        AbilityLifecycleEvent::Despawned,
        position,
        normalized_or_forward(direction),
        None,
        Some("impact_penguin_snowflake_warp"),
        ImpactSource::Projectile,
        32,
        Some((owner, 0.12)),
    );
}

#[derive(Resource, Default)]
pub struct PenguinSkillAssets {
    fish_bones_scene: Handle<Scene>,
    popsicle_scene: Handle<Scene>,
    snow_pile_scene: Handle<Scene>,
    snowflake_scene: Handle<Scene>,
    snowman_scene: Handle<Scene>,
    ultimate_snow_flat_large_scene: Handle<Scene>,
    ultimate_snow_flat_scene: Handle<Scene>,
    ice_tile_scene: Handle<Scene>,
    snow_bump_scene: Handle<Scene>,
    snow_hill_scene: Handle<Scene>,
    snowfort_scene: Handle<Scene>,
    cannon_scene: Handle<Scene>,
    boulder_scene: Handle<Scene>,
    snow_slope_scene: Handle<Scene>,
    snow_steep_slope_scene: Handle<Scene>,
    spring_scene: Handle<Scene>,
}

/// Render-only markers used to rehydrate canonical Penguin entities after a
/// rollback restore or late join.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PenguinSkillVisualRoot {
    kind: PenguinSkillKind,
}

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PenguinSurfaceVisualRoot {
    kind: PenguinSurfaceKind,
}

pub fn setup_penguin_skill_assets(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.insert_resource(PenguinSkillAssets {
        fish_bones_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_FISH_BONES_ASSET)),
        popsicle_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_POPSICLE_ASSET)),
        snow_pile_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_SNOW_PILE_ASSET)),
        snowflake_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_SNOWFLAKE_ASSET)),
        snowman_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_SNOWMAN_ASSET)),
        ultimate_snow_flat_large_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_ULTIMATE_SNOW_FLAT_LARGE_ASSET)),
        ultimate_snow_flat_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_ULTIMATE_SNOW_FLAT_ASSET)),
        ice_tile_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_ICE_TILE_ASSET)),
        snow_bump_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_SNOW_BUMP_ASSET)),
        snow_hill_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_SNOW_HILL_ASSET)),
        snowfort_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_SNOWFORT_ASSET)),
        cannon_scene: asset_server.load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_CANNON_ASSET)),
        boulder_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_BOULDER_ASSET)),
        snow_slope_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_SNOW_SLOPE_ASSET)),
        snow_steep_slope_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_SNOW_STEEP_SLOPE_ASSET)),
        spring_scene: asset_server.load(GltfAssetLabel::Scene(0).from_asset(PENGUIN_SPRING_ASSET)),
    });
}

/// Attaches client-only scenes to canonical Penguin skill and surface roots.
/// Headless authorities omit this system and therefore never need asset
/// resources, scene children, or render markers.
pub fn attach_missing_penguin_visuals(
    mut commands: Commands,
    assets: Res<PenguinSkillAssets>,
    skills: Query<
        (
            Entity,
            &ActivePenguinSkill,
            &SimPosition,
            Option<&Transform>,
        ),
        Without<PenguinSkillVisualRoot>,
    >,
    surfaces: Query<
        (
            Entity,
            &ActivePenguinSurface,
            &SimPosition,
            Option<&Transform>,
        ),
        (
            Without<ActivePenguinSkill>,
            Without<PenguinSurfaceVisualRoot>,
        ),
    >,
) {
    for (entity, skill, position, transform) in &skills {
        if transform.is_none() {
            commands
                .entity(entity)
                .insert(Transform::from_translation(position.translation));
        }
        let (scene, name) = match skill.kind {
            PenguinSkillKind::FishTorpedo => {
                (assets.fish_bones_scene.clone(), "Penguin fish torpedo")
            }
            PenguinSkillKind::PopsicleBounce => {
                (assets.popsicle_scene.clone(), "Penguin popsicle bounce")
            }
            PenguinSkillKind::SledWake => (assets.snow_pile_scene.clone(), "Penguin sled wake"),
            PenguinSkillKind::SnowflakeShard => {
                (assets.snowflake_scene.clone(), "Penguin snowflake shard")
            }
            PenguinSkillKind::SnowBoulder => (assets.boulder_scene.clone(), "Penguin snow boulder"),
            PenguinSkillKind::SnowmanDrop => (assets.snowman_scene.clone(), "Penguin snowman drop"),
            PenguinSkillKind::BodySlamShockwave => (
                assets.snow_bump_scene.clone(),
                "Penguin body slam shockwave",
            ),
        };
        commands.entity(entity).insert((
            SceneRoot(scene),
            PenguinSkillVisualRoot { kind: skill.kind },
            Name::new(name),
        ));
    }

    for (entity, surface, position, transform) in &surfaces {
        if transform.is_none() {
            commands
                .entity(entity)
                .insert(Transform::from_translation(position.translation));
        }
        let marker = PenguinSurfaceVisualRoot { kind: surface.kind };
        match surface.kind {
            PenguinSurfaceKind::IceTrailSegment => {
                commands.entity(entity).insert((
                    SceneRoot(assets.ice_tile_scene.clone()),
                    marker,
                    Name::new("Penguin ice trail"),
                ));
            }
            PenguinSurfaceKind::UltimateIceTile => {
                commands.entity(entity).insert((
                    SceneRoot(assets.ultimate_snow_flat_large_scene.clone()),
                    marker,
                    Name::new("Penguin ultimate snow field"),
                ));
                commands.entity(entity).with_children(|parent| {
                    for (offset, angle) in [
                        (Vec3::new(-0.31, 0.012, 0.18), 0.68),
                        (Vec3::new(0.27, 0.014, -0.18), -0.54),
                        (Vec3::new(0.04, 0.016, 0.32), 1.36),
                    ] {
                        parent.spawn((
                            SceneRoot(assets.ultimate_snow_flat_scene.clone()),
                            Transform::from_translation(offset)
                                .with_rotation(Quat::from_rotation_y(angle))
                                .with_scale(Vec3::splat(PENGUIN_ULTIMATE_SNOW_FLAT_DETAIL_SCALE)),
                            Name::new("Penguin ultimate snow flat detail"),
                        ));
                    }
                });
            }
            PenguinSurfaceKind::SnowHillRamp => {
                let scene = if surface.size_scale > 1.0 {
                    assets.snow_hill_scene.clone()
                } else {
                    assets.snow_steep_slope_scene.clone()
                };
                commands.entity(entity).insert((
                    SceneRoot(scene),
                    marker,
                    Name::new("Penguin snow hill ramp"),
                ));
            }
            PenguinSurfaceKind::SnowSlopeRide => {
                commands.entity(entity).insert((
                    SceneRoot(assets.snow_slope_scene.clone()),
                    marker,
                    Name::new("Penguin snow slope ride"),
                ));
            }
            PenguinSurfaceKind::SnowfortCannon => {
                commands.entity(entity).insert((
                    SceneRoot(assets.snowfort_scene.clone()),
                    marker,
                    Name::new("Penguin snowfort cannon"),
                ));
                commands.entity(entity).with_children(|parent| {
                    parent.spawn((
                        SceneRoot(assets.cannon_scene.clone()),
                        Transform::from_xyz(0.08, 0.42 * surface.size_scale, 0.18)
                            .with_scale(Vec3::splat(0.72 * surface.size_scale)),
                        Name::new("Penguin snowfort cannon barrel"),
                    ));
                });
            }
            PenguinSurfaceKind::GlacierTrailPrinter => {
                commands
                    .entity(entity)
                    .insert((marker, Name::new("Penguin glacier trail printer")));
            }
            PenguinSurfaceKind::SpringPad => {
                commands.entity(entity).insert((
                    SceneRoot(assets.spring_scene.clone()),
                    marker,
                    Name::new("Penguin spring peck pad"),
                ));
            }
        }
    }
}

/// Derives rotation and scale exclusively in render Update. Canonical motion
/// owns translation only, matching the live snapshot codecs.
pub fn sync_penguin_visuals(
    mut skills: Query<(
        &ActivePenguinSkill,
        &PenguinSkillVisualRoot,
        &SimPosition,
        &mut Transform,
    )>,
    mut surfaces: Query<
        (
            &ActivePenguinSurface,
            &PenguinSurfaceVisualRoot,
            &SimPosition,
            &mut Transform,
        ),
        Without<ActivePenguinSkill>,
    >,
) {
    for (skill, visual, position, mut transform) in &mut skills {
        if visual.kind != skill.kind {
            continue;
        }
        transform.translation = position.translation;
        let age = skill.age.as_seconds();
        let ticks = skill.age.get() as f32;
        transform.scale = penguin_skill_visual_scale(skill.kind, skill.size_scale, age);
        transform.rotation = match skill.kind {
            PenguinSkillKind::FishTorpedo => {
                projectile_rotation(skill.facing) * Quat::from_rotation_y(ticks * 0.18)
            }
            PenguinSkillKind::PopsicleBounce => {
                projectile_rotation(skill.facing)
                    * Quat::from_rotation_y(ticks * 0.16)
                    * Quat::from_rotation_x(ticks * 0.12)
            }
            PenguinSkillKind::SledWake | PenguinSkillKind::BodySlamShockwave => {
                projectile_rotation(skill.facing)
            }
            PenguinSkillKind::SnowflakeShard => {
                projectile_rotation(skill.facing) * Quat::from_rotation_z(age * 12.0)
            }
            PenguinSkillKind::SnowBoulder => {
                projectile_rotation(skill.facing) * Quat::from_rotation_x(age * -12.0)
            }
            PenguinSkillKind::SnowmanDrop => {
                projectile_rotation(skill.facing) * Quat::from_rotation_x(age * -3.4)
            }
        };
    }

    for (surface, visual, position, mut transform) in &mut surfaces {
        if visual.kind != surface.kind {
            continue;
        }
        transform.translation = position.translation;
        transform.rotation = match surface.kind {
            PenguinSurfaceKind::SnowSlopeRide => snow_slope_ride_rotation(surface.facing),
            PenguinSurfaceKind::GlacierTrailPrinter => Quat::IDENTITY,
            _ => projectile_rotation(surface.facing),
        };
        transform.scale = match surface.kind {
            PenguinSurfaceKind::IceTrailSegment => {
                ice_trail_visual_scale(surface.size_scale, surface.lifetime.as_seconds())
            }
            PenguinSurfaceKind::UltimateIceTile => {
                ultimate_snow_field_visual_scale(surface.size_scale, surface.lifetime.as_seconds())
            }
            PenguinSurfaceKind::SnowHillRamp | PenguinSurfaceKind::SnowSlopeRide => {
                Vec3::splat(0.72 * surface.size_scale)
            }
            PenguinSurfaceKind::SnowfortCannon => {
                Vec3::splat((1.0 - surface.age.as_seconds() * 0.08).max(0.82))
            }
            PenguinSurfaceKind::GlacierTrailPrinter => Vec3::ONE,
            PenguinSurfaceKind::SpringPad => {
                let pulse = 1.0 + (surface.age.as_seconds() * 12.0).sin().abs() * 0.06;
                Vec3::splat(0.72 * pulse * surface.size_scale)
            }
        };
    }
}

fn spawn_canonical_penguin_entity<B: Bundle>(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    kind: SimEntityKind,
    position: Vec3,
    bundle: B,
) -> Option<(Entity, SimEntityId)> {
    let entity = commands.spawn_empty().id();
    let stable = match identities.try_allocate(kind, entity) {
        Ok(stable) => stable,
        Err(_) => {
            commands.entity(entity).despawn();
            return None;
        }
    };
    commands
        .entity(entity)
        .insert((stable, SimPosition::new(position), bundle));
    Some((entity, stable.id()))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_penguin_skill_with_presentation(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    state: &MatchState,
    arena: &ArenaDefinition,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    origin: Vec3,
    facing: Vec3,
    aim_held: bool,
    owner_size_scale: f32,
    skill: PenguinSkillId,
    targets: &[BeeSkillTargetSnapshot],
    active_skills: impl IntoIterator<Item = (PenguinSkillKind, FighterId, TickTimer)>,
) -> bool {
    if skill == PenguinSkillId::SnowflakeShot
        && penguin_snowflake_shot_is_active(owner, active_skills)
    {
        return false;
    }

    let facing = normalized_or_forward(facing);
    let size_scale = penguin_skill_size_scale(owner_size_scale);
    let target = penguin_skill_lock_target(owner, origin, facing, aim_held, state, targets);
    match skill {
        PenguinSkillId::FishTorpedo => {
            let spawn = grounded_position(arena, origin + facing * 0.55, 0.26 * size_scale);
            let direction = target
                .and_then(|entity| target_position(entity, targets))
                .map(|position| flat_direction(spawn, position))
                .filter(|direction| canonical_math::vec3_length_squared(*direction) > 0.01)
                .unwrap_or(facing);
            spawn_fish_torpedo(
                commands,
                identities,
                arena,
                owner,
                owner_id,
                owner_style,
                spawn,
                direction,
                target,
                size_scale,
            );
        }
        PenguinSkillId::PopsicleBounce => {
            let spawn = origin + (Vec3::Y * 1.05 + facing * 0.52) * size_scale;
            let direction = target
                .and_then(|entity| target_position(entity, targets))
                .map(|position| flat_direction(spawn, position))
                .filter(|direction| canonical_math::vec3_length_squared(*direction) > 0.01)
                .unwrap_or(facing);
            spawn_popsicle_bounce(
                commands,
                identities,
                arena,
                owner,
                owner_id,
                owner_style,
                spawn,
                direction,
                size_scale,
            );
        }
        PenguinSkillId::SledWake => {
            let spawn = grounded_position(arena, origin + facing * 0.75, 0.05);
            spawn_sled_wake(
                commands,
                identities,
                arena,
                owner,
                owner_id,
                owner_style,
                spawn,
                facing,
                size_scale,
            );
        }
        PenguinSkillId::IceTrail => {
            spawn_ice_trail_line(
                commands,
                identities,
                arena,
                owner,
                owner_id,
                origin,
                facing,
                size_scale,
                if aim_held { 5 } else { 3 },
                true,
            );
        }
        PenguinSkillId::UltimateIceField => {
            spawn_ultimate_ice_field(
                commands, identities, arena, owner, owner_id, origin, facing, size_scale,
            );
        }
        PenguinSkillId::SnowmanDrop => {
            let ground = grounded_position(
                arena,
                origin + facing * PENGUIN_SNOWMAN_DROP_FORWARD * size_scale,
                0.02,
            );
            let spawn = ground + Vec3::Y * PENGUIN_SNOWMAN_DROP_HEIGHT * size_scale;
            spawn_snowman_drop(
                commands,
                identities,
                arena,
                owner,
                owner_id,
                owner_style,
                spawn,
                facing,
                size_scale,
            );
        }
        PenguinSkillId::SnowHillRamp => {
            let distance = if aim_held { 2.05 } else { 1.15 };
            let spawn = grounded_position(arena, origin + facing * distance * size_scale, 0.02);
            spawn_snow_hill_ramp(
                commands, identities, arena, owner, owner_id, spawn, facing, size_scale, true,
            );
        }
        PenguinSkillId::SnowSlopeRide => {
            let spawn = grounded_position(arena, origin + facing * 1.72 * size_scale, 0.02);
            spawn_snow_slope_ride(
                commands, identities, arena, owner, owner_id, spawn, facing, size_scale,
            );
        }
        PenguinSkillId::SnowfortCannon => {
            let spawn = grounded_position(arena, origin + facing * 1.05 * size_scale, 0.02);
            spawn_snowfort_cannon(
                commands,
                identities,
                arena,
                owner,
                owner_id,
                owner_style,
                spawn,
                facing,
                size_scale,
            );
        }
        PenguinSkillId::SpringPeck => {
            let spawn = grounded_position(arena, origin + facing * 0.52 * size_scale, 0.03);
            spawn_spring_pad(
                commands, identities, arena, owner, owner_id, spawn, facing, size_scale,
            );
            spawn_ice_trail_segment(
                commands,
                identities,
                arena,
                owner,
                owner_id,
                spawn,
                facing,
                0.78 * size_scale,
                PENGUIN_ICE_TRAIL_LIFETIME * 0.35,
                size_scale,
                None,
            );
        }
        PenguinSkillId::BodySlam => {
            let distance = if aim_held { 1.35 } else { 0.72 };
            let spawn = grounded_position(arena, origin + facing * distance * size_scale, 0.05);
            spawn_body_slam_shockwave(
                commands,
                identities,
                arena,
                owner,
                owner_id,
                owner_style,
                spawn,
                facing,
                size_scale,
            );
        }
        PenguinSkillId::GlacierParade => {
            spawn_glacier_trail_printer(
                commands, identities, arena, owner, owner_id, origin, facing, size_scale,
            );
        }
        PenguinSkillId::SnowflakeShot => {
            let (spawn, direction) = snowflake_shot_spawn(origin, facing, size_scale);
            spawn_snowflake_shot(
                commands,
                identities,
                arena,
                owner,
                owner_id,
                owner_style,
                spawn,
                direction,
                size_scale,
            );
        }
        PenguinSkillId::SnowflakeSwapShot => {
            return false;
        }
        PenguinSkillId::SnowflakeBurst => {
            let spawn = origin + Vec3::Y * 0.92 * size_scale;
            spawn_snowflake_burst(
                commands,
                identities,
                arena,
                owner,
                owner_id,
                owner_style,
                spawn,
                facing,
                size_scale,
            );
        }
    }
    true
}

fn spawn_fish_torpedo(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    _arena: &ArenaDefinition,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    direction: Vec3,
    target: Option<FighterId>,
    size_scale: f32,
) {
    let facing = normalized_or_forward(direction);
    let Some((_, id)) = spawn_canonical_penguin_entity(
        commands,
        identities,
        SimEntityKind::PenguinSkill,
        position,
        active_penguin_skill(
            PenguinSkillKind::FishTorpedo,
            owner,
            owner_id,
            owner_style,
            facing,
            facing * PENGUIN_FISH_TORPEDO_SPEED,
            target,
            size_scale,
        ),
    ) else {
        return;
    };
    queue_penguin_lifecycle(
        commands,
        id,
        AbilityLifecycleEvent::Spawned,
        position,
        facing,
        Some(FeedbackPackageId::SpecialProjectileStartup),
        Some("release_special_projectile"),
        ImpactSource::Projectile,
        24,
        Some((owner, 0.12)),
    );
}

fn spawn_popsicle_bounce(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    _arena: &ArenaDefinition,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    direction: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(direction);
    let velocity = facing * PENGUIN_POPSICLE_SPEED + Vec3::Y * PENGUIN_POPSICLE_LIFT;
    let Some((_, id)) = spawn_canonical_penguin_entity(
        commands,
        identities,
        SimEntityKind::PenguinSkill,
        position,
        active_penguin_skill(
            PenguinSkillKind::PopsicleBounce,
            owner,
            owner_id,
            owner_style,
            facing,
            velocity,
            None,
            size_scale,
        ),
    ) else {
        return;
    };
    queue_penguin_lifecycle(
        commands,
        id,
        AbilityLifecycleEvent::Spawned,
        position,
        facing,
        Some(FeedbackPackageId::SpecialProjectileStartup),
        Some("release_special_projectile"),
        ImpactSource::Projectile,
        24,
        Some((owner, 0.12)),
    );
}

fn spawn_sled_wake(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    _arena: &ArenaDefinition,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let Some((_, id)) = spawn_canonical_penguin_entity(
        commands,
        identities,
        SimEntityKind::PenguinSkill,
        position,
        active_penguin_skill(
            PenguinSkillKind::SledWake,
            owner,
            owner_id,
            owner_style,
            facing,
            facing * PENGUIN_SLED_WAKE_SPEED,
            None,
            size_scale,
        ),
    ) else {
        return;
    };
    queue_penguin_lifecycle(
        commands,
        id,
        AbilityLifecycleEvent::Spawned,
        position,
        facing,
        Some(FeedbackPackageId::SpecialHazardStartup),
        Some("release_special_projectile"),
        ImpactSource::Hazard,
        24,
        Some((owner, 0.12)),
    );
}

fn spawn_snowflake_shot(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    arena: &ArenaDefinition,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    direction: Vec3,
    size_scale: f32,
) {
    spawn_snowflake_projectile(
        commands,
        identities,
        arena,
        owner,
        owner_id,
        owner_style,
        position,
        direction,
        size_scale,
        PenguinSkillKind::SnowflakeShard,
        "Penguin snowflake shot",
    );
}

fn spawn_snowflake_projectile(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    _arena: &ArenaDefinition,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    direction: Vec3,
    size_scale: f32,
    kind: PenguinSkillKind,
    _name: &'static str,
) {
    let direction = normalized_or_forward(direction);
    let Some((_, id)) = spawn_canonical_penguin_entity(
        commands,
        identities,
        SimEntityKind::PenguinSkill,
        position,
        active_penguin_skill(
            kind,
            owner,
            owner_id,
            owner_style,
            direction,
            direction * PENGUIN_SNOWFLAKE_SPEED,
            None,
            size_scale,
        ),
    ) else {
        return;
    };
    queue_penguin_lifecycle(
        commands,
        id,
        AbilityLifecycleEvent::Spawned,
        position,
        direction,
        Some(FeedbackPackageId::SpecialProjectileStartup),
        Some("release_special_projectile"),
        ImpactSource::Projectile,
        24,
        Some((owner, 0.12)),
    );
}

fn spawn_snowflake_burst(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    _arena: &ArenaDefinition,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    for (index, direction) in snowflake_burst_directions(facing).into_iter().enumerate() {
        let spawn = position + direction * 0.24 * size_scale;
        let Some((_, id)) = spawn_canonical_penguin_entity(
            commands,
            identities,
            SimEntityKind::PenguinSkill,
            spawn,
            active_penguin_skill(
                PenguinSkillKind::SnowflakeShard,
                owner,
                owner_id,
                owner_style,
                direction,
                direction * PENGUIN_SNOWFLAKE_SPEED,
                None,
                size_scale,
            ),
        ) else {
            continue;
        };
        queue_penguin_lifecycle(
            commands,
            id,
            AbilityLifecycleEvent::Spawned,
            spawn,
            direction,
            (index == 0).then_some(FeedbackPackageId::SpecialProjectileStartup),
            (index == 0).then_some("release_special_projectile"),
            ImpactSource::Projectile,
            if index == 0 { 24 } else { 0 },
            (index == 0).then_some((owner, 0.12)),
        );
    }
}

fn spawn_snowman_drop(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    _arena: &ArenaDefinition,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    let Some((_, id)) = spawn_canonical_penguin_entity(
        commands,
        identities,
        SimEntityKind::PenguinSkill,
        position,
        active_penguin_skill(
            PenguinSkillKind::SnowmanDrop,
            owner,
            owner_id,
            owner_style,
            facing,
            Vec3::Y * -PENGUIN_SNOWMAN_DROP_INITIAL_FALL_SPEED,
            None,
            size_scale,
        ),
    ) else {
        return;
    };
    queue_penguin_lifecycle(
        commands,
        id,
        AbilityLifecycleEvent::Spawned,
        position,
        facing,
        Some(FeedbackPackageId::SpecialProjectileStartup),
        Some("release_special_projectile"),
        ImpactSource::Projectile,
        24,
        Some((owner, 0.12)),
    );
}

fn spawn_ice_trail_line(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    arena: &ArenaDefinition,
    owner: FighterId,
    owner_id: usize,
    origin: Vec3,
    facing: Vec3,
    size_scale: f32,
    count: usize,
    announce_cast: bool,
) {
    let facing = normalized_or_forward(facing);
    for index in 0..count {
        let offset = index as f32 * 0.72 * size_scale;
        let position = grounded_position(arena, origin - facing * offset, 0.015);
        spawn_ice_trail_segment(
            commands,
            identities,
            arena,
            owner,
            owner_id,
            position,
            facing,
            PENGUIN_ICE_TRAIL_RADIUS * size_scale,
            PENGUIN_ICE_TRAIL_LIFETIME,
            size_scale,
            (announce_cast && index == 0).then_some(FeedbackPackageId::SpecialHazardStartup),
        );
    }
}

fn spawn_ice_trail_segment(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    arena: &ArenaDefinition,
    owner: FighterId,
    owner_id: usize,
    position: Vec3,
    facing: Vec3,
    radius: f32,
    lifetime: f32,
    size_scale: f32,
    package: Option<FeedbackPackageId>,
) {
    let facing = normalized_or_forward(facing);
    let position = grounded_position(arena, position, 0.012);
    let Some((_, id)) = spawn_canonical_penguin_entity(
        commands,
        identities,
        SimEntityKind::PenguinSurface,
        position,
        active_penguin_surface(
            PenguinSurfaceKind::IceTrailSegment,
            owner,
            owner_id,
            facing,
            radius,
            lifetime,
            size_scale,
        ),
    ) else {
        return;
    };
    queue_penguin_lifecycle(
        commands,
        id,
        AbilityLifecycleEvent::Spawned,
        position,
        facing,
        package,
        package.map(|_| "release_special_projectile"),
        ImpactSource::Hazard,
        if package.is_some() { 24 } else { 0 },
        package.map(|_| (owner, 0.12)),
    );
}

fn spawn_ultimate_ice_field(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    arena: &ArenaDefinition,
    owner: FighterId,
    owner_id: usize,
    origin: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    let right = canonical_math::vec3_normalize_or_zero(Vec3::new(facing.z, 0.0, -facing.x));
    let spacing = PENGUIN_ULTIMATE_ICE_FIELD_TILE_SPACING * size_scale;
    for x in 0..PENGUIN_ULTIMATE_ICE_FIELD_GRID_SIDE {
        for z in 0..PENGUIN_ULTIMATE_ICE_FIELD_GRID_SIDE {
            let offset = right * ultimate_ice_field_grid_axis_offset(x) * spacing
                + facing * ultimate_ice_field_grid_axis_offset(z) * spacing;
            spawn_ultimate_ice_tile(
                commands,
                identities,
                arena,
                owner,
                owner_id,
                grounded_position(arena, origin + offset, 0.014),
                facing,
                size_scale,
                (x == 0 && z == 0).then_some(FeedbackPackageId::SpecialHazardStartup),
            );
        }
    }
}

fn ultimate_ice_field_grid_axis_offset(index: i32) -> f32 {
    index as f32 - (PENGUIN_ULTIMATE_ICE_FIELD_GRID_SIDE as f32 - 1.0) * 0.5
}

fn spawn_ultimate_ice_tile(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    arena: &ArenaDefinition,
    owner: FighterId,
    owner_id: usize,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
    package: Option<FeedbackPackageId>,
) {
    let facing = normalized_or_forward(facing);
    let position = grounded_position(arena, position, PENGUIN_ULTIMATE_SNOW_FIELD_CLEARANCE);
    let Some((_, id)) = spawn_canonical_penguin_entity(
        commands,
        identities,
        SimEntityKind::PenguinSurface,
        position,
        active_penguin_surface(
            PenguinSurfaceKind::UltimateIceTile,
            owner,
            owner_id,
            facing,
            PENGUIN_ULTIMATE_ICE_FIELD_TILE_RADIUS * size_scale,
            PENGUIN_ULTIMATE_ICE_FIELD_LIFETIME,
            size_scale,
        ),
    ) else {
        return;
    };
    queue_penguin_lifecycle(
        commands,
        id,
        AbilityLifecycleEvent::Spawned,
        position,
        facing,
        package,
        package.map(|_| "release_special_projectile"),
        ImpactSource::Hazard,
        if package.is_some() { 24 } else { 0 },
        package.map(|_| (owner, 0.12)),
    );
}

fn spawn_snow_hill_ramp(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    arena: &ArenaDefinition,
    owner: FighterId,
    owner_id: usize,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
    announce_cast: bool,
) {
    let facing = normalized_or_forward(facing);
    let position = grounded_position(arena, position, 0.02);
    let Some((_, id)) = spawn_canonical_penguin_entity(
        commands,
        identities,
        SimEntityKind::PenguinSurface,
        position,
        active_penguin_surface(
            PenguinSurfaceKind::SnowHillRamp,
            owner,
            owner_id,
            facing,
            PENGUIN_SNOW_HILL_RADIUS * size_scale,
            PENGUIN_SNOW_HILL_LIFETIME,
            size_scale,
        ),
    ) else {
        return;
    };
    queue_penguin_lifecycle(
        commands,
        id,
        AbilityLifecycleEvent::Spawned,
        position,
        facing,
        announce_cast.then_some(FeedbackPackageId::SpecialHazardStartup),
        announce_cast.then_some("release_special_projectile"),
        ImpactSource::Hazard,
        if announce_cast { 24 } else { 0 },
        announce_cast.then_some((owner, 0.12)),
    );
}

fn spawn_snow_slope_ride(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    arena: &ArenaDefinition,
    owner: FighterId,
    owner_id: usize,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    let position = grounded_position(arena, position, 0.02);
    let Some((_, id)) = spawn_canonical_penguin_entity(
        commands,
        identities,
        SimEntityKind::PenguinSurface,
        position,
        active_penguin_surface(
            PenguinSurfaceKind::SnowSlopeRide,
            owner,
            owner_id,
            facing,
            PENGUIN_SNOW_SLOPE_RIDE_RADIUS * size_scale,
            PENGUIN_SNOW_SLOPE_RIDE_LIFETIME,
            size_scale,
        ),
    ) else {
        return;
    };
    queue_penguin_lifecycle(
        commands,
        id,
        AbilityLifecycleEvent::Spawned,
        position,
        facing,
        None,
        Some("release_special_projectile"),
        ImpactSource::Hazard,
        24,
        Some((owner, 0.12)),
    );
}

fn snow_slope_ride_rotation(facing: Vec3) -> Quat {
    projectile_rotation(-normalized_or_forward(facing))
}

fn spawn_snowfort_cannon(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    arena: &ArenaDefinition,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    let fort_position = grounded_position(arena, position - facing * 0.22, 0.02);
    if let Some((_, id)) = spawn_canonical_penguin_entity(
        commands,
        identities,
        SimEntityKind::PenguinSurface,
        fort_position,
        active_penguin_surface(
            PenguinSurfaceKind::SnowfortCannon,
            owner,
            owner_id,
            facing,
            0.0,
            PENGUIN_SNOWFORT_LIFETIME,
            size_scale,
        ),
    ) {
        queue_penguin_lifecycle(
            commands,
            id,
            AbilityLifecycleEvent::Spawned,
            fort_position,
            facing,
            None,
            Some("release_special_projectile"),
            ImpactSource::Hazard,
            24,
            Some((owner, 0.12)),
        );
    }
    spawn_snow_boulder(
        commands,
        identities,
        arena,
        owner,
        owner_id,
        owner_style,
        position + facing * 0.48 + Vec3::Y * 0.52 * size_scale,
        facing,
        size_scale,
    );
}

fn spawn_snow_boulder(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    _arena: &ArenaDefinition,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    let Some((_, id)) = spawn_canonical_penguin_entity(
        commands,
        identities,
        SimEntityKind::PenguinSkill,
        position,
        active_penguin_skill(
            PenguinSkillKind::SnowBoulder,
            owner,
            owner_id,
            owner_style,
            facing,
            facing * PENGUIN_BOULDER_SPEED,
            None,
            size_scale,
        ),
    ) else {
        return;
    };
    queue_penguin_lifecycle(
        commands,
        id,
        AbilityLifecycleEvent::Spawned,
        position,
        facing,
        Some(FeedbackPackageId::SpecialProjectileStartup),
        None,
        ImpactSource::Projectile,
        24,
        None,
    );
}

fn spawn_spring_pad(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    _arena: &ArenaDefinition,
    owner: FighterId,
    owner_id: usize,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    let Some((_, id)) = spawn_canonical_penguin_entity(
        commands,
        identities,
        SimEntityKind::PenguinSurface,
        position,
        active_penguin_surface(
            PenguinSurfaceKind::SpringPad,
            owner,
            owner_id,
            facing,
            PENGUIN_SPRING_PAD_RADIUS * size_scale,
            PENGUIN_SPRING_PAD_LIFETIME,
            size_scale,
        ),
    ) else {
        return;
    };
    queue_penguin_lifecycle(
        commands,
        id,
        AbilityLifecycleEvent::Spawned,
        position,
        facing,
        Some(FeedbackPackageId::SpecialHazardStartup),
        Some("release_special_projectile"),
        ImpactSource::Hazard,
        24,
        Some((owner, 0.12)),
    );
}

fn spawn_body_slam_shockwave(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    arena: &ArenaDefinition,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    if let Some((_, id)) = spawn_canonical_penguin_entity(
        commands,
        identities,
        SimEntityKind::PenguinSkill,
        position,
        active_penguin_skill(
            PenguinSkillKind::BodySlamShockwave,
            owner,
            owner_id,
            owner_style,
            facing,
            Vec3::ZERO,
            None,
            size_scale,
        ),
    ) {
        queue_penguin_lifecycle(
            commands,
            id,
            AbilityLifecycleEvent::Spawned,
            position,
            facing,
            Some(FeedbackPackageId::SpecialHazardStartup),
            Some("release_special_projectile"),
            ImpactSource::Hazard,
            24,
            Some((owner, 0.12)),
        );
    }
    spawn_snow_hill_ramp(
        commands,
        identities,
        arena,
        owner,
        owner_id,
        position + facing * 0.58 * size_scale,
        facing,
        size_scale,
        false,
    );
    spawn_ice_trail_line(
        commands, identities, arena, owner, owner_id, position, facing, size_scale, 4, false,
    );
}

fn spawn_glacier_trail_printer(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    _arena: &ArenaDefinition,
    owner: FighterId,
    owner_id: usize,
    origin: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    let facing = normalized_or_forward(facing);
    let Some((_, id)) = spawn_canonical_penguin_entity(
        commands,
        identities,
        SimEntityKind::PenguinSurface,
        origin,
        active_penguin_surface(
            PenguinSurfaceKind::GlacierTrailPrinter,
            owner,
            owner_id,
            facing,
            0.0,
            PENGUIN_GLACIER_PARADE_LIFETIME,
            size_scale,
        ),
    ) else {
        return;
    };
    queue_penguin_lifecycle(
        commands,
        id,
        AbilityLifecycleEvent::Spawned,
        origin,
        facing,
        Some(FeedbackPackageId::SpecialHazardStartup),
        Some("release_special_projectile"),
        ImpactSource::Hazard,
        24,
        Some((owner, 0.12)),
    );
}

fn active_penguin_skill(
    kind: PenguinSkillKind,
    owner: FighterId,
    owner_id: usize,
    owner_style: FighterStyleKind,
    facing: Vec3,
    velocity: Vec3,
    target: Option<FighterId>,
    size_scale: f32,
) -> ActivePenguinSkill {
    debug_assert_eq!(owner.index(), owner_id);
    let size_scale = penguin_skill_size_scale(size_scale);
    let (payload_id, shape_id, source, lifetime, radius, guard_stamina_damage, repeat_interval) =
        match kind {
            PenguinSkillKind::FishTorpedo => (
                AttackPayloadId::PenguinFishTorpedo,
                AttackShapeId::ProjectileBolt,
                ImpactSource::Projectile,
                PENGUIN_FISH_TORPEDO_LIFETIME,
                PENGUIN_FISH_TORPEDO_RADIUS,
                8.0,
                None,
            ),
            PenguinSkillKind::PopsicleBounce => (
                AttackPayloadId::PenguinPopsicleBounce,
                AttackShapeId::ProjectileBolt,
                ImpactSource::Projectile,
                PENGUIN_POPSICLE_LIFETIME,
                PENGUIN_POPSICLE_RADIUS,
                9.0,
                None,
            ),
            PenguinSkillKind::SledWake => (
                AttackPayloadId::PenguinSledWake,
                AttackShapeId::HazardField,
                ImpactSource::Hazard,
                PENGUIN_SLED_WAKE_LIFETIME,
                PENGUIN_SLED_WAKE_RADIUS,
                6.0,
                Some(PENGUIN_SLED_WAKE_TICK),
            ),
            PenguinSkillKind::SnowflakeShard => (
                AttackPayloadId::PenguinSnowflakeShard,
                AttackShapeId::ProjectileBolt,
                ImpactSource::Projectile,
                PENGUIN_SNOWFLAKE_LIFETIME,
                PENGUIN_SNOWFLAKE_RADIUS,
                10.0,
                None,
            ),
            PenguinSkillKind::SnowBoulder => (
                AttackPayloadId::PenguinSnowBoulder,
                AttackShapeId::ProjectileBolt,
                ImpactSource::Projectile,
                PENGUIN_BOULDER_LIFETIME,
                PENGUIN_BOULDER_RADIUS,
                11.0,
                None,
            ),
            PenguinSkillKind::SnowmanDrop => (
                AttackPayloadId::PenguinSnowmanDrop,
                AttackShapeId::ProjectileBolt,
                ImpactSource::Projectile,
                PENGUIN_SNOWMAN_DROP_LIFETIME,
                PENGUIN_SNOWMAN_DROP_RADIUS,
                7.0,
                None,
            ),
            PenguinSkillKind::BodySlamShockwave => (
                AttackPayloadId::PenguinBodySlamShockwave,
                AttackShapeId::ShockwaveRing,
                ImpactSource::Hazard,
                PENGUIN_BODY_SLAM_LIFETIME,
                PENGUIN_BODY_SLAM_RADIUS,
                14.0,
                None,
            ),
        };

    ActivePenguinSkill {
        kind,
        owner,
        owner_style,
        payload_id,
        shape_id,
        source,
        facing: normalized_or_forward(facing),
        velocity,
        target,
        lifetime: TickTimer::from_seconds_ceil(lifetime),
        age: ElapsedTicks::ZERO,
        radius: radius * size_scale,
        guard_stamina_damage,
        repeat_interval: repeat_interval.map(TickTimer::from_seconds_ceil),
        repeat_timer: repeat_interval.map(TickTimer::from_seconds_ceil),
        already_hit: FighterHitMask::default(),
        size_scale,
    }
}

pub fn penguin_snowflake_shot_is_active(
    owner: FighterId,
    active_skills: impl IntoIterator<Item = (PenguinSkillKind, FighterId, TickTimer)>,
) -> bool {
    active_skills
        .into_iter()
        .any(|(kind, skill_owner, lifetime)| {
            kind == PenguinSkillKind::SnowflakeShard && skill_owner == owner && lifetime.active()
        })
}

fn active_penguin_surface(
    kind: PenguinSurfaceKind,
    owner: FighterId,
    owner_id: usize,
    facing: Vec3,
    radius: f32,
    lifetime: f32,
    size_scale: f32,
) -> ActivePenguinSurface {
    debug_assert_eq!(owner.index(), owner_id);
    ActivePenguinSurface {
        kind,
        owner,
        facing: normalized_or_forward(facing),
        lifetime: TickTimer::from_seconds_ceil(lifetime),
        age: ElapsedTicks::ZERO,
        radius,
        next_tick: TickTimer::ZERO,
        already_touched: FighterHitMask::default(),
        size_scale: penguin_skill_size_scale(size_scale),
    }
}

fn penguin_skill_size_scale(owner_size_scale: f32) -> f32 {
    owner_size_scale.max(0.1)
}

fn penguin_skill_visual_scale(kind: PenguinSkillKind, size_scale: f32, age: f32) -> Vec3 {
    let size_scale = penguin_skill_size_scale(size_scale);
    match kind {
        PenguinSkillKind::FishTorpedo => Vec3::splat(1.75 * size_scale),
        PenguinSkillKind::PopsicleBounce => Vec3::splat(1.55 * size_scale),
        PenguinSkillKind::SledWake => Vec3::splat(sled_wake_visual_pulse(age) * size_scale),
        PenguinSkillKind::SnowflakeShard => Vec3::splat(1.35 * size_scale),
        PenguinSkillKind::SnowBoulder => Vec3::splat(0.9 * size_scale),
        PenguinSkillKind::SnowmanDrop => {
            Vec3::splat(0.82 * PENGUIN_SNOWMAN_DROP_SIZE_MULTIPLIER * size_scale)
        }
        PenguinSkillKind::BodySlamShockwave => {
            Vec3::new(1.2, 0.28, 1.2) * (1.0 + age * 1.5) * size_scale
        }
    }
}

fn sled_wake_visual_pulse(age: f32) -> f32 {
    0.84 + (age * 9.0).sin().abs() * 0.12
}

pub fn collect_penguin_skill_contacts(
    identities: Res<SimulationIdentityAllocator>,
    state: Res<MatchState>,
    active_arena: Res<ActiveArena>,
    feel: Res<CombatFeelTuning>,
    hitstop: Res<Hitstop>,
    mut contact_buffer: ResMut<ContactBuffer>,
    mut sim_events: ResMut<TickEventBuffer>,
    mut presentation_intents: Option<ResMut<PenguinPresentationIntentJournal>>,
    mut skills: Query<
        (&StableSimEntity, &mut ActivePenguinSkill, &mut SimPosition),
        (Without<Fighter>, Without<ActivePenguinSurface>),
    >,
    mut fighters: ParamSet<(
        Query<(&Fighter, &SimPosition), With<Fighter>>,
        Query<
            (
                &Fighter,
                &FighterStats,
                &FighterMotor,
                &FighterActionState,
                &SimPosition,
            ),
            With<Fighter>,
        >,
    )>,
) {
    if hitstop.active() {
        return;
    }

    let dt = 1.0 / SIM_HZ_U32 as f32;
    for index in 0..identities.capacity(SimEntityKind::PenguinSkill) {
        let Some((skill_id, skill_entity)) =
            identities.entry_at(SimEntityKind::PenguinSkill, index)
        else {
            continue;
        };
        let Ok((stable, mut skill, mut transform)) = skills.get_mut(skill_entity) else {
            continue;
        };
        if stable.id() != skill_id {
            continue;
        }
        skill.age.advance();
        skill.lifetime.tick();
        if update_skill_repeat_window(&mut skill) {
            emit_penguin_lifecycle(
                &mut sim_events,
                presentation_intents.as_deref_mut(),
                skill_id,
                AbilityLifecycleEvent::Repeated,
                transform.translation,
                skill.facing,
                None,
                Some("pulse_penguin_sled_wake"),
                skill.source,
                24,
                None,
            );
        }
        update_penguin_skill_motion(
            &mut skill,
            &mut transform,
            dt,
            &fighters.p0(),
            active_arena.definition(),
        );

        {
            let target_fighters = fighters.p1();
            for target_id in FighterId::ALL {
                let Some((target, stats, _motor, action, target_transform)) = target_fighters
                    .iter()
                    .find(|(fighter, ..)| fighter.id == target_id.index())
                else {
                    continue;
                };
                if target_id == skill.owner && skill.age.as_millis_floor() < 160 {
                    continue;
                }
                if target_id == skill.owner && skill.kind == PenguinSkillKind::SnowmanDrop {
                    continue;
                }
                if !state.combat_target_allowed_for_state(skill.owner.index(), target.id) {
                    continue;
                }
                if skill.already_hit.contains(target_id)
                    || !can_receive_impact(&stats, &action)
                    || !penguin_skill_overlaps_target(
                        &skill,
                        transform.translation,
                        &target_transform,
                    )
                {
                    continue;
                }

                let profile = penguin_skill_impact_profile(&skill, &feel);
                let _ = contact_buffer.push(ContactRecord::new(
                    ContactPhase::Strike,
                    ContactSourceKind::CharacterAbility,
                    skill_id,
                    Some(skill.owner),
                    target_id,
                    skill.payload_id as u16,
                    skill.shape_id as u16,
                    0,
                    target_transform.translation,
                    transform.translation,
                    profile,
                    ContactFlags::default(),
                ));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn apply_penguin_skill_contact_outcomes(
    mut commands: Commands,
    mut identities: ResMut<SimulationIdentityAllocator>,
    active_arena: Res<ActiveArena>,
    mut contact_buffer: ResMut<ContactBuffer>,
    mut sim_events: ResMut<TickEventBuffer>,
    mut presentation_intents: Option<ResMut<PenguinPresentationIntentJournal>>,
    surfaces: Query<
        (&StableSimEntity, &ActivePenguinSurface, &SimPosition),
        (Without<Fighter>, Without<ActivePenguinSkill>),
    >,
    mut skills: Query<
        (&StableSimEntity, &mut ActivePenguinSkill, &SimPosition),
        (Without<Fighter>, Without<ActivePenguinSurface>),
    >,
    mut fighters: Query<(&Fighter, &mut FighterMotor, &mut SimPosition), With<Fighter>>,
) {
    for contact_index in 0..contact_buffer.len() {
        let Some(contact) = contact_buffer.record(contact_index) else {
            continue;
        };
        if contact.source_kind != ContactSourceKind::CharacterAbility {
            continue;
        }
        let Some(source) = contact.source.entity() else {
            continue;
        };
        if source.kind() != SimEntityKind::PenguinSkill {
            continue;
        }
        let Some(skill_entity) = identities.mapped_entity(source) else {
            contact_buffer.mark_outcome(contact_index, ContactOutcomeKind::Invalidated);
            continue;
        };
        let Ok((stable, mut skill, _)) = skills.get_mut(skill_entity) else {
            contact_buffer.mark_outcome(contact_index, ContactOutcomeKind::Invalidated);
            continue;
        };
        if stable.id() != source {
            contact_buffer.mark_outcome(contact_index, ContactOutcomeKind::Invalidated);
            continue;
        }
        let Some(outcome) = contact_buffer.outcome(contact_index) else {
            continue;
        };
        if !matches!(
            outcome.kind,
            ContactOutcomeKind::Accepted | ContactOutcomeKind::Guarded
        ) {
            continue;
        }

        let mut impact_cue =
            (skill.kind == PenguinSkillKind::SnowmanDrop).then_some("impact_special_projectile");
        if let Some((_, mut motor, mut target_transform)) = fighters
            .iter_mut()
            .find(|(fighter, ..)| fighter.id == contact.target.index())
        {
            if skill.kind == PenguinSkillKind::SledWake && motor.grounded {
                motor.velocity.x *= PENGUIN_SLED_WAKE_DAMPING;
                motor.velocity.z *= PENGUIN_SLED_WAKE_DAMPING;
                impact_cue = Some("impact_penguin_sled_wake");
            }
            if skill.kind == PenguinSkillKind::SnowBoulder {
                spawn_ice_trail_segment(
                    &mut commands,
                    &mut identities,
                    active_arena.definition(),
                    skill.owner,
                    skill.owner.index(),
                    grounded_position(
                        active_arena.definition(),
                        contact.contact_point.to_vec3(),
                        0.02,
                    ),
                    skill.facing,
                    PENGUIN_ICE_TRAIL_RADIUS * 0.82 * skill.size_scale,
                    PENGUIN_ICE_TRAIL_LIFETIME * 0.5,
                    skill.size_scale,
                    None,
                );
            }
            if skill.kind == PenguinSkillKind::SnowmanDrop {
                motor.velocity.x = 0.0;
                motor.velocity.z = 0.0;
            }
            if skill.kind == PenguinSkillKind::SnowflakeShard
                && let Some(destination) = snowflake_magic_destination_from_query(
                    contact.contact_point.to_vec3(),
                    &surfaces,
                    active_arena.definition(),
                )
            {
                target_transform.translation = destination;
                motor.velocity = Vec3::ZERO;
                motor.grounded = true;
                motor.landing_aftermath = None;
                motor.knockdown_on_land = false;
                motor.reaction_bounces = 0;
                emit_penguin_lifecycle(
                    &mut sim_events,
                    presentation_intents.as_deref_mut(),
                    source,
                    AbilityLifecycleEvent::Repeated,
                    destination + Vec3::Y * (FIGHTER_HEIGHT * 0.58),
                    skill.facing,
                    Some(FeedbackPackageId::SpecialHazardImpact),
                    Some("impact_penguin_snowflake_warp"),
                    skill.source,
                    32,
                    None,
                );
            }
        }
        skill.already_hit.insert(contact.target);
        if !penguin_skill_persists_after_hit(skill.kind) {
            skill.lifetime.clear();
        }
        if let (Some(event_id), Some(intents)) = (outcome.event_id, presentation_intents.as_mut()) {
            let _ = intents.record(PenguinPresentationIntent {
                event_id,
                entity: source,
                kind: PenguinPresentationKind::Impact {
                    victim: contact.target,
                    position: contact.contact_point.to_vec3() + Vec3::Y * (FIGHTER_HEIGHT * 0.58),
                    direction: skill.facing,
                    package: impact_package(skill.kind),
                    cue: impact_cue,
                    source: skill.source,
                    priority: if skill.kind == PenguinSkillKind::SnowmanDrop {
                        28
                    } else {
                        24
                    },
                },
            });
        }
    }

    for index in 0..identities.capacity(SimEntityKind::PenguinSkill) {
        let Some((skill_id, skill_entity)) =
            identities.entry_at(SimEntityKind::PenguinSkill, index)
        else {
            continue;
        };
        let Ok((stable, skill, transform)) = skills.get_mut(skill_entity) else {
            continue;
        };
        if stable.id() != skill_id {
            continue;
        }
        let hit_this_tick = (0..contact_buffer.len()).any(|contact_index| {
            contact_buffer
                .record(contact_index)
                .filter(|contact| contact.source.entity() == Some(skill_id))
                .and_then(|_| contact_buffer.outcome(contact_index))
                .is_some_and(|outcome| {
                    matches!(
                        outcome.kind,
                        ContactOutcomeKind::Accepted | ContactOutcomeKind::Guarded
                    )
                })
        });

        let popsicle_grounded =
            popsicle_touched_ground(&skill, transform.translation, active_arena.definition());
        let snowman_grounded =
            snowman_touched_ground(&skill, transform.translation, active_arena.definition());
        if snowman_grounded {
            let landing = grounded_position(active_arena.definition(), transform.translation, 0.02);
            spawn_snowman_landing_snow(
                &mut commands,
                &mut identities,
                active_arena.definition(),
                skill.owner,
                skill.owner.index(),
                landing,
                skill.facing,
                skill.size_scale,
            );
            queue_penguin_lifecycle(
                &mut commands,
                skill_id,
                AbilityLifecycleEvent::Despawned,
                landing,
                skill.facing,
                Some(FeedbackPackageId::SpecialHazardImpact),
                None,
                skill.source,
                0,
                None,
            );
            despawn_stable(&mut commands, &mut identities, skill_entity, *stable);
            continue;
        }
        if !skill.lifetime.active()
            || popsicle_grounded
            || should_despawn_skill(transform.translation, active_arena.definition())
        {
            if !hit_this_tick {
                queue_penguin_lifecycle(
                    &mut commands,
                    skill_id,
                    AbilityLifecycleEvent::Despawned,
                    transform.translation,
                    skill.facing,
                    Some(despawn_package(skill.kind)),
                    None,
                    skill.source,
                    0,
                    None,
                );
            }
            despawn_stable(&mut commands, &mut identities, skill_entity, *stable);
        }
    }
}

pub fn update_penguin_surfaces(
    mut commands: Commands,
    mut identities: ResMut<SimulationIdentityAllocator>,
    state: Res<MatchState>,
    active_arena: Res<ActiveArena>,
    hitstop: Res<Hitstop>,
    mut surfaces: Query<
        (
            &StableSimEntity,
            &mut ActivePenguinSurface,
            &mut SimPosition,
        ),
        Without<Fighter>,
    >,
    mut fighters: Query<(Entity, &Fighter, &mut FighterMotor, &mut SimPosition), With<Fighter>>,
) {
    if hitstop.active() {
        return;
    }

    let mut fighter_snapshots = ArrayVec::<_, { FighterId::ALL.len() }>::new();
    for fighter_id in FighterId::ALL {
        let Some((_, _, motor, transform)) = fighters
            .iter_mut()
            .find(|(_, fighter, ..)| fighter.id == fighter_id.index())
        else {
            continue;
        };
        if let Err(error) = try_push_fixed_penguin(
            &mut fighter_snapshots,
            (
                fighter_id,
                transform.translation,
                normalized_or_forward(motor.facing),
                planar_speed(motor.velocity),
            ),
            "Penguin surface fighter snapshots",
        ) {
            error!(?error, "Penguin surface update failed closed");
            return;
        }
    }
    let mut surface_ids = ArrayVec::<_, PENGUIN_SURFACE_ENTITY_CAPACITY>::new();
    for (stable, ..) in surfaces.iter() {
        let id = stable.id();
        if id.kind() != SimEntityKind::PenguinSurface
            || id.index() as usize >= PENGUIN_SURFACE_ENTITY_CAPACITY
            || surface_ids
                .iter()
                .any(|existing: &SimEntityId| existing.index() == id.index())
        {
            error!(
                ?id,
                "invalid Penguin surface identity; update failed closed"
            );
            return;
        }
        if let Err(error) =
            try_push_fixed_penguin(&mut surface_ids, id, "Penguin surface identities")
        {
            error!(?error, "Penguin surface update failed closed");
            return;
        }
    }
    surface_ids.sort_unstable();
    let mut ice_segments = ArrayVec::<_, PENGUIN_SURFACE_ENTITY_CAPACITY>::new();
    let mut pending_despawns = ArrayVec::<_, PENGUIN_SURFACE_ENTITY_CAPACITY>::new();
    for surface_id in surface_ids {
        let Some(surface_entity) = identities.mapped_entity(surface_id) else {
            continue;
        };
        let Ok((_, mut surface, mut transform)) = surfaces.get_mut(surface_entity) else {
            continue;
        };
        surface.age.advance();
        surface.lifetime.tick();

        match surface.kind {
            PenguinSurfaceKind::IceTrailSegment => {
                if let Err(error) = try_push_fixed_penguin(
                    &mut ice_segments,
                    (surface_id, surface.owner, surface.age),
                    "Penguin ice segments",
                ) {
                    error!(?error, "Penguin surface update failed closed");
                    return;
                }
            }
            PenguinSurfaceKind::UltimateIceTile => {}
            PenguinSurfaceKind::SnowHillRamp => {
                update_ramp_hazard(
                    &mut commands,
                    &state,
                    surface_id,
                    &mut surface,
                    transform.translation,
                    &mut fighters,
                );
            }
            PenguinSurfaceKind::SnowSlopeRide => {
                update_snow_slope_ride(
                    &mut surface,
                    transform.translation,
                    &mut fighters,
                    active_arena.definition(),
                );
            }
            PenguinSurfaceKind::SpringPad => {
                update_spring_pad(
                    &mut commands,
                    &state,
                    surface_id,
                    &mut surface,
                    transform.translation,
                    &mut fighters,
                );
            }
            PenguinSurfaceKind::SnowfortCannon => {}
            PenguinSurfaceKind::GlacierTrailPrinter => {
                update_glacier_trail_printer(
                    &mut commands,
                    &mut identities,
                    active_arena.definition(),
                    surface_id,
                    &mut surface,
                    &mut transform,
                    &fighter_snapshots,
                );
            }
        }

        if !surface.lifetime.active()
            || should_despawn_skill(transform.translation, active_arena.definition())
        {
            if let Err(error) = try_push_fixed_penguin(
                &mut pending_despawns,
                surface_id,
                "pending Penguin surface despawns",
            ) {
                error!(?error, "Penguin surface update failed closed");
                return;
            }
        }
    }

    let oldest_segments =
        match oldest_ice_segments_to_despawn(&ice_segments, PENGUIN_ICE_TRAIL_CAP_PER_OWNER) {
            Ok(oldest_segments) => oldest_segments,
            Err(error) => {
                error!(?error, "Penguin surface update failed closed");
                return;
            }
        };
    for id in oldest_segments {
        if pending_despawns.contains(&id) {
            continue;
        }
        if let Err(error) = try_push_fixed_penguin(
            &mut pending_despawns,
            id,
            "pending Penguin surface despawns",
        ) {
            error!(?error, "Penguin surface update failed closed");
            return;
        }
    }
    pending_despawns.sort_unstable();
    for id in pending_despawns {
        if let Some(entity) = identities.mapped_entity(id) {
            if let Ok((_, surface, transform)) = surfaces.get(entity) {
                queue_penguin_lifecycle(
                    &mut commands,
                    id,
                    AbilityLifecycleEvent::Despawned,
                    transform.translation,
                    surface.facing,
                    None,
                    None,
                    ImpactSource::Hazard,
                    0,
                    None,
                );
            }
            despawn_stable(
                &mut commands,
                &mut identities,
                entity,
                StableSimEntity::new(id),
            );
        }
    }
}

fn update_ramp_hazard(
    commands: &mut Commands,
    state: &MatchState,
    surface_id: SimEntityId,
    surface: &mut ActivePenguinSurface,
    position: Vec3,
    fighters: &mut Query<(Entity, &Fighter, &mut FighterMotor, &mut SimPosition), With<Fighter>>,
) {
    for fighter_id in FighterId::ALL {
        let Some((_, _fighter, mut motor, fighter_transform)) = fighters
            .iter_mut()
            .find(|(_, fighter, ..)| fighter.id == fighter_id.index())
        else {
            continue;
        };
        if surface.already_touched.contains(fighter_id)
            || !surface_can_touch_fighter(surface, fighter_id, state)
            || !surface_overlaps_fighter(surface, position, fighter_transform.translation)
        {
            continue;
        }
        let touch = snow_hill_ramp_touch(surface, fighter_id);
        let push = if touch.owner_ride {
            normalized_or_forward(surface.facing)
        } else {
            ramp_push_direction(surface.facing, position, fighter_transform.translation)
        };
        motor.velocity.x += push.x * touch.forward_push;
        motor.velocity.z += push.z * touch.forward_push;
        motor.velocity.y = motor.velocity.y.max(touch.lift);
        motor.grounded = false;
        if touch.owner_ride {
            let planar_speed =
                canonical_math::vec2_length(Vec2::new(motor.velocity.x, motor.velocity.z));
            motor
                .dash_slide_timer
                .set_max(TickTimer::from_seconds_ceil(touch.slide_timer));
            motor
                .impact_speed_limit_timer
                .set_max(TickTimer::from_seconds_ceil(touch.speed_limit_timer));
            motor.impact_speed_limit = motor.impact_speed_limit.max(planar_speed);
            motor.landing_stick_timer.clear();
        }
        surface.already_touched.insert(fighter_id);
        let cue = if touch.owner_ride {
            "impact_penguin_snow_hill_ski"
        } else {
            "impact_penguin_snow_hill_ramp"
        };
        queue_penguin_lifecycle(
            commands,
            surface_id,
            AbilityLifecycleEvent::Repeated,
            fighter_transform.translation + Vec3::Y * 0.45,
            surface.facing,
            Some(FeedbackPackageId::SpecialHazardImpact),
            Some(cue),
            ImpactSource::Hazard,
            22,
            Some((fighter_id, 0.12)),
        );
    }
}

fn update_snow_slope_ride(
    surface: &mut ActivePenguinSurface,
    position: Vec3,
    fighters: &mut Query<(Entity, &Fighter, &mut FighterMotor, &mut SimPosition), With<Fighter>>,
    arena: &ArenaDefinition,
) {
    for fighter_id in FighterId::ALL {
        let Some((_, _, mut motor, mut fighter_transform)) = fighters
            .iter_mut()
            .find(|(_, fighter, ..)| fighter.id == fighter_id.index())
        else {
            continue;
        };
        if fighter_id != surface.owner || surface.already_touched.contains(fighter_id) {
            continue;
        }

        let Some(contact) = snow_slope_ride_contact(
            position,
            surface.facing,
            fighter_transform.translation,
            surface.size_scale,
            arena,
        ) else {
            continue;
        };

        fighter_transform.translation.y = contact.target_y;
        motor.velocity.y = motor.velocity.y.max(0.0);
        motor.grounded = true;
        motor
            .dash_slide_timer
            .set_max(TickTimer::from_seconds_ceil(PENGUIN_SNOW_SLOPE_RIDE_SLIDE));

        if contact.progress >= PENGUIN_SNOW_SLOPE_RIDE_EXIT_PROGRESS {
            let push = normalized_or_forward(surface.facing);
            motor.velocity.x += push.x * PENGUIN_SNOW_SLOPE_RIDE_PUSH;
            motor.velocity.z += push.z * PENGUIN_SNOW_SLOPE_RIDE_PUSH;
            motor.velocity.y = motor.velocity.y.max(PENGUIN_SNOW_SLOPE_RIDE_LIFT);
            motor.grounded = false;
            let planar_speed =
                canonical_math::vec2_length(Vec2::new(motor.velocity.x, motor.velocity.z));
            motor
                .impact_speed_limit_timer
                .set_max(TickTimer::from_seconds_ceil(
                    PENGUIN_SNOW_SLOPE_RIDE_SPEED_LIMIT,
                ));
            motor.impact_speed_limit = motor.impact_speed_limit.max(planar_speed);
            motor.landing_stick_timer.clear();
            surface.already_touched.insert(fighter_id);
        }
    }
}

fn update_spring_pad(
    commands: &mut Commands,
    state: &MatchState,
    surface_id: SimEntityId,
    surface: &mut ActivePenguinSurface,
    position: Vec3,
    fighters: &mut Query<(Entity, &Fighter, &mut FighterMotor, &mut SimPosition), With<Fighter>>,
) {
    for fighter_id in FighterId::ALL {
        let Some((_, _, mut motor, fighter_transform)) = fighters
            .iter_mut()
            .find(|(_, fighter, ..)| fighter.id == fighter_id.index())
        else {
            continue;
        };
        if surface.already_touched.contains(fighter_id)
            || !surface_can_touch_fighter(surface, fighter_id, state)
            || !surface_overlaps_fighter(surface, position, fighter_transform.translation)
        {
            continue;
        }
        motor.velocity.x += surface.facing.x * 1.25;
        motor.velocity.z += surface.facing.z * 1.25;
        motor.velocity.y = motor.velocity.y.max(PENGUIN_SPRING_PAD_LIFT);
        motor.grounded = false;
        surface.already_touched.insert(fighter_id);
        queue_penguin_lifecycle(
            commands,
            surface_id,
            AbilityLifecycleEvent::Repeated,
            fighter_transform.translation + Vec3::Y * 0.45,
            surface.facing,
            Some(FeedbackPackageId::SpecialHazardImpact),
            Some("impact_penguin_spring_peck"),
            ImpactSource::Hazard,
            20,
            Some((fighter_id, 0.1)),
        );
    }
}

fn update_glacier_trail_printer(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    arena: &ArenaDefinition,
    surface_id: SimEntityId,
    surface: &mut ActivePenguinSurface,
    transform: &mut SimPosition,
    fighters: &[(FighterId, Vec3, Vec3, f32)],
) {
    let Some((_, owner_position, owner_facing, owner_speed)) = fighters
        .iter()
        .find(|(entity, _, _, _)| *entity == surface.owner)
        .copied()
    else {
        surface.lifetime.clear();
        return;
    };
    transform.translation = owner_position;
    surface.facing = owner_facing;
    if owner_speed <= 0.35 && surface.age.as_millis_floor() > 200 {
        return;
    }
    if !surface.next_tick.active() || surface.next_tick.tick() {
        spawn_ice_trail_segment(
            commands,
            identities,
            arena,
            surface.owner,
            surface.owner.index(),
            grounded_position(arena, owner_position, 0.015),
            surface.facing,
            PENGUIN_ICE_TRAIL_RADIUS * 1.08 * surface.size_scale,
            PENGUIN_ICE_TRAIL_LIFETIME,
            surface.size_scale,
            None,
        );
        queue_penguin_lifecycle(
            commands,
            surface_id,
            AbilityLifecycleEvent::Repeated,
            owner_position,
            surface.facing,
            Some(FeedbackPackageId::SpecialHazardStartup),
            None,
            ImpactSource::Hazard,
            0,
            None,
        );
        surface
            .next_tick
            .set(TickTimer::from_seconds_ceil(PENGUIN_GLACIER_PARADE_TICK));
    }
}

fn snowflake_magic_destination_from_query(
    target_position: Vec3,
    surfaces: &Query<
        (&StableSimEntity, &ActivePenguinSurface, &SimPosition),
        (Without<Fighter>, Without<ActivePenguinSkill>),
    >,
    arena: &ArenaDefinition,
) -> Option<Vec3> {
    let mut best: Option<(f32, SimEntityId, Vec3)> = None;
    for (stable, surface, transform) in surfaces.iter() {
        let Some(position) =
            active_snowfield_center(surface.kind, surface.lifetime, transform.translation)
        else {
            continue;
        };
        let candidate = (
            flat_distance_squared(target_position, position),
            stable.id(),
            position,
        );
        if best.is_none_or(|incumbent| {
            candidate
                .0
                .total_cmp(&incumbent.0)
                .then_with(|| candidate.1.cmp(&incumbent.1))
                .is_gt()
        }) {
            best = Some(candidate);
        }
    }
    best.map(|(_, _, position)| grounded_position(arena, position, 0.0))
}

fn active_snowfield_center(
    kind: PenguinSurfaceKind,
    lifetime: TickTimer,
    position: Vec3,
) -> Option<Vec3> {
    (lifetime.active() && snowflake_magic_surface_kind(kind)).then_some(position)
}

fn snowflake_magic_surface_kind(kind: PenguinSurfaceKind) -> bool {
    matches!(
        kind,
        PenguinSurfaceKind::IceTrailSegment | PenguinSurfaceKind::UltimateIceTile
    )
}

#[cfg(test)]
fn snowflake_magic_destination(
    target_position: Vec3,
    snowfield_centers: impl IntoIterator<Item = Vec3>,
    arena: &ArenaDefinition,
) -> Option<Vec3> {
    snowfield_centers
        .into_iter()
        .max_by(|a, b| {
            flat_distance_squared(target_position, *a)
                .total_cmp(&flat_distance_squared(target_position, *b))
        })
        .map(|position| grounded_position(arena, position, 0.0))
}

#[cfg(test)]
fn snowflake_magic_destination_from_surfaces(
    target_position: Vec3,
    surfaces: impl IntoIterator<Item = (PenguinSurfaceKind, f32, Vec3)>,
) -> Option<Vec3> {
    snowflake_magic_destination(
        target_position,
        surfaces
            .into_iter()
            .filter_map(|(kind, lifetime, position)| {
                active_snowfield_center(kind, TickTimer::from_seconds_ceil(lifetime), position)
            }),
        crate::arena_defs::arena_definition(0),
    )
}

pub fn penguin_ice_modifier(
    position: Vec3,
    character_kind: CharacterKind,
    surfaces: &Query<(&ActivePenguinSurface, &SimPosition), Without<Fighter>>,
) -> Option<PenguinIceModifier> {
    let mut on_soft_ice = false;
    let mut on_hard_ice = false;
    for (surface, transform) in surfaces.iter() {
        if !surface_overlaps_position(surface, transform.translation, position, FIGHTER_RADIUS) {
            continue;
        }
        match surface.kind {
            PenguinSurfaceKind::IceTrailSegment => on_soft_ice = true,
            PenguinSurfaceKind::UltimateIceTile => on_hard_ice = true,
            _ => {}
        }
    }
    penguin_ice_modifier_from_overlaps(on_soft_ice, on_hard_ice, character_kind)
}

fn penguin_ice_modifier_from_overlaps(
    on_soft_ice: bool,
    on_hard_ice: bool,
    character_kind: CharacterKind,
) -> Option<PenguinIceModifier> {
    if !on_soft_ice && !on_hard_ice {
        return None;
    }
    let penguin = character_kind == CharacterKind::Penguin;
    if on_hard_ice {
        return Some(PenguinIceModifier {
            ground_friction_scale: 0.04,
            stop_friction_scale: 0.04,
            turn_brake_scale: 0.0,
            input_scale: 0.0,
            dash_slide_timer: 0.28,
            hard_slide: true,
        });
    }
    Some(PenguinIceModifier {
        ground_friction_scale: if penguin { 0.34 } else { 0.18 },
        stop_friction_scale: if penguin { 0.36 } else { 0.14 },
        turn_brake_scale: if penguin { 0.62 } else { 0.28 },
        input_scale: if penguin { 0.98 } else { 0.82 },
        dash_slide_timer: if penguin { 0.12 } else { 0.22 },
        hard_slide: false,
    })
}

fn penguin_skill_persists_after_hit(kind: PenguinSkillKind) -> bool {
    matches!(
        kind,
        PenguinSkillKind::SledWake
            | PenguinSkillKind::SnowmanDrop
            | PenguinSkillKind::BodySlamShockwave
    )
}

fn surface_can_touch_fighter(
    surface: &ActivePenguinSurface,
    fighter: FighterId,
    state: &MatchState,
) -> bool {
    fighter == surface.owner
        || state.combat_target_allowed_for_state(surface.owner.index(), fighter.index())
}

fn surface_overlaps_fighter(
    surface: &ActivePenguinSurface,
    surface_position: Vec3,
    fighter_position: Vec3,
) -> bool {
    surface_overlaps_position(surface, surface_position, fighter_position, FIGHTER_RADIUS)
}

fn surface_overlaps_position(
    surface: &ActivePenguinSurface,
    surface_position: Vec3,
    position: Vec3,
    radius: f32,
) -> bool {
    let combined_radius = surface.radius + radius;
    debug_assert!(combined_radius >= 0.0);
    flat_distance_squared(surface_position, position) <= combined_radius * combined_radius
}

fn snow_slope_ride_contact(
    surface_position: Vec3,
    facing: Vec3,
    fighter_position: Vec3,
    size_scale: f32,
    arena: &ArenaDefinition,
) -> Option<SnowSlopeRideContact> {
    let size_scale = penguin_skill_size_scale(size_scale);
    let forward = normalized_or_forward(facing);
    let right = canonical_math::vec3_normalize_or_zero(Vec3::new(forward.z, 0.0, -forward.x));
    let offset = fighter_position - surface_position;
    let along = offset.dot(forward);
    let side = offset.dot(right);
    let half_length = PENGUIN_SNOW_SLOPE_RIDE_HALF_LENGTH * size_scale;
    let half_width = PENGUIN_SNOW_SLOPE_RIDE_HALF_WIDTH * size_scale;
    if side.abs() > half_width + FIGHTER_RADIUS
        || along < -half_length - FIGHTER_RADIUS
        || along > half_length + FIGHTER_RADIUS
    {
        return None;
    }

    let progress = ((along + half_length) / (half_length * 2.0)).clamp(0.0, 1.0);
    let ground = ground_height(arena, fighter_position.x, fighter_position.z);
    Some(SnowSlopeRideContact {
        progress,
        target_y: ground
            + (PENGUIN_SNOW_SLOPE_RIDE_BASE_HEIGHT + PENGUIN_SNOW_SLOPE_RIDE_HEIGHT * progress)
                * size_scale,
    })
}

fn snow_hill_ramp_touch(surface: &ActivePenguinSurface, fighter: FighterId) -> SnowHillRampTouch {
    if fighter == surface.owner {
        SnowHillRampTouch {
            forward_push: PENGUIN_SNOW_HILL_RIDE_PUSH,
            lift: PENGUIN_SNOW_HILL_RIDE_LIFT,
            slide_timer: PENGUIN_SNOW_HILL_RIDE_SLIDE,
            speed_limit_timer: PENGUIN_SNOW_HILL_RIDE_SPEED_LIMIT,
            owner_ride: true,
        }
    } else {
        SnowHillRampTouch {
            forward_push: PENGUIN_SNOW_HILL_PUSH,
            lift: PENGUIN_SNOW_HILL_LAUNCH,
            slide_timer: 0.0,
            speed_limit_timer: 0.0,
            owner_ride: false,
        }
    }
}

fn ramp_push_direction(facing: Vec3, ramp_position: Vec3, fighter_position: Vec3) -> Vec3 {
    let away = canonical_math::vec3_normalize_or_zero(Vec3::new(
        fighter_position.x - ramp_position.x,
        0.0,
        fighter_position.z - ramp_position.z,
    ));
    let facing = normalized_or_forward(facing);
    canonical_math::vec3_normalize_or_zero(facing * 0.76 + away * 0.24)
}

fn ice_trail_visual_scale(size_scale: f32, lifetime: f32) -> Vec3 {
    let fade = (lifetime / 0.55).clamp(0.28, 1.0);
    Vec3::new(0.95 * fade, 0.16, 1.2 * fade) * size_scale
}

fn ultimate_snow_field_visual_scale(size_scale: f32, lifetime: f32) -> Vec3 {
    let fade = (lifetime / 0.85).clamp(0.22, 1.0);
    Vec3::new(
        PENGUIN_ULTIMATE_SNOW_FLAT_LARGE_SCALE * fade * size_scale,
        size_scale,
        PENGUIN_ULTIMATE_SNOW_FLAT_LARGE_SCALE * fade * size_scale,
    )
}

fn oldest_ice_segments_to_despawn(
    segments: &[(SimEntityId, FighterId, ElapsedTicks)],
    cap_per_owner: usize,
) -> Result<ArrayVec<SimEntityId, PENGUIN_SURFACE_ENTITY_CAPACITY>, FixedPenguinCollectionOverflow>
{
    let mut despawn = ArrayVec::new();
    for owner in FighterId::ALL {
        let mut owner_segments = ArrayVec::<_, PENGUIN_SURFACE_ENTITY_CAPACITY>::new();
        for (id, segment_owner, age) in segments.iter().copied() {
            if segment_owner != owner {
                continue;
            }
            try_push_fixed_penguin(
                &mut owner_segments,
                (id, age),
                "per-owner Penguin ice segments",
            )?;
        }
        if owner_segments.len() <= cap_per_owner {
            continue;
        }
        owner_segments.sort_unstable_by(|(id_a, age_a), (id_b, age_b)| {
            age_b.cmp(age_a).then_with(|| id_a.cmp(id_b))
        });
        for (entity, _) in owner_segments
            .iter()
            .take(owner_segments.len() - cap_per_owner)
        {
            try_push_fixed_penguin(&mut despawn, *entity, "Penguin ice-segment cap despawns")?;
        }
    }
    Ok(despawn)
}

fn planar_speed(velocity: Vec3) -> f32 {
    canonical_math::vec2_length(Vec2::new(velocity.x, velocity.z))
}

fn update_skill_repeat_window(skill: &mut ActivePenguinSkill) -> bool {
    let Some(interval) = skill.repeat_interval else {
        return false;
    };
    let Some(mut repeat_timer) = skill.repeat_timer else {
        return false;
    };
    let repeated = repeat_timer.tick();
    if repeated {
        skill.already_hit.clear();
        repeat_timer.set(interval);
    }
    skill.repeat_timer = Some(repeat_timer);
    repeated
}

fn update_penguin_skill_motion(
    skill: &mut ActivePenguinSkill,
    transform: &mut SimPosition,
    dt: f32,
    targets: &Query<(&Fighter, &SimPosition), With<Fighter>>,
    arena: &ArenaDefinition,
) {
    match skill.kind {
        PenguinSkillKind::FishTorpedo => {
            if let Some(target_id) = skill.target
                && let Some((_, target_transform)) = targets
                    .iter()
                    .find(|(fighter, _)| fighter.id == target_id.index())
            {
                steer_fish_torpedo_toward(
                    skill,
                    transform.translation,
                    target_transform.translation + Vec3::Y * 0.5,
                    dt,
                );
            }
            transform.translation += skill.velocity * dt;
            transform.translation =
                grounded_position(arena, transform.translation, 0.26 * skill.size_scale);
        }
        PenguinSkillKind::PopsicleBounce => {
            skill.velocity.y -= PENGUIN_POPSICLE_GRAVITY * dt;
            transform.translation += skill.velocity * dt;
        }
        PenguinSkillKind::SledWake => {
            transform.translation += skill.velocity * dt;
            transform.translation = grounded_position(arena, transform.translation, 0.05);
        }
        PenguinSkillKind::SnowflakeShard => {
            transform.translation += skill.velocity * dt;
        }
        PenguinSkillKind::SnowBoulder => {
            transform.translation += skill.velocity * dt;
            transform.translation =
                grounded_position(arena, transform.translation, 0.34 * skill.size_scale);
        }
        PenguinSkillKind::SnowmanDrop => {
            skill.velocity.y -= PENGUIN_SNOWMAN_DROP_GRAVITY * dt;
            transform.translation += skill.velocity * dt;
        }
        PenguinSkillKind::BodySlamShockwave => {
            transform.translation = grounded_position(arena, transform.translation, 0.05);
        }
    }
}

fn steer_fish_torpedo_toward(
    skill: &mut ActivePenguinSkill,
    current_position: Vec3,
    target_position: Vec3,
    dt: f32,
) {
    let desired = canonical_math::vec3_normalize_or_zero(target_position - current_position);
    if canonical_math::vec3_length_squared(desired) <= 0.01 {
        return;
    }
    let speed = canonical_math::vec3_length(skill.velocity);
    skill.velocity = skill.velocity.lerp(
        desired * speed,
        (dt * PENGUIN_FISH_TORPEDO_TURN_RATE).clamp(0.0, 1.0),
    );
    skill.facing = normalized_or_forward(skill.velocity);
}

fn penguin_skill_impact_profile(
    skill: &ActivePenguinSkill,
    feel: &CombatFeelTuning,
) -> crate::combat::ImpactProfile {
    let mut profile = impact_profile_from_payload_with_feel(
        skill.owner.index(),
        skill.source,
        skill.payload_id,
        1.0,
        1.0,
        1.0,
        skill.guard_stamina_damage,
        feel,
    );
    profile.shape_id = Some(skill.shape_id);
    profile.attacker_style = Some(skill.owner_style);
    profile
}

fn penguin_skill_overlaps_target(
    skill: &ActivePenguinSkill,
    origin: Vec3,
    target_transform: &SimPosition,
) -> bool {
    let combined_radius = skill.radius + FIGHTER_RADIUS;
    debug_assert!(combined_radius >= 0.0);
    if skill.kind == PenguinSkillKind::SledWake {
        return flat_distance_squared(origin, target_transform.translation)
            <= combined_radius * combined_radius;
    }
    let target = target_transform.translation + Vec3::Y * (FIGHTER_HEIGHT * 0.58);
    canonical_math::vec3_distance_squared(target, origin) <= combined_radius * combined_radius
}

pub fn penguin_skill_lock_target(
    owner: FighterId,
    origin: Vec3,
    facing: Vec3,
    aim_held: bool,
    state: &MatchState,
    targets: &[BeeSkillTargetSnapshot],
) -> Option<FighterId> {
    if !aim_held {
        return None;
    }
    let facing = normalized_or_forward(facing);

    targets
        .iter()
        .filter(|target| {
            state.combat_target_allowed_for_state(owner.index(), target.fighter_id.index())
        })
        .filter_map(|target| {
            let offset = Vec3::new(
                target.position.x - origin.x,
                0.0,
                target.position.z - origin.z,
            );
            let distance_squared = canonical_math::vec3_length_squared(offset);
            if distance_squared > PENGUIN_SKILL_LOCK_RANGE * PENGUIN_SKILL_LOCK_RANGE
                || distance_squared <= 0.01 * 0.01
            {
                return None;
            }
            let direction = canonical_math::vec3_normalize_or_zero(offset);
            (direction.dot(facing) >= PENGUIN_SKILL_LOCK_CONE_DOT)
                .then_some((target.fighter_id, distance_squared))
        })
        .min_by(|(fighter_a, distance_a), (fighter_b, distance_b)| {
            distance_a
                .total_cmp(distance_b)
                .then_with(|| fighter_a.cmp(fighter_b))
        })
        .map(|(entity, _)| entity)
}

fn target_position(fighter: FighterId, targets: &[BeeSkillTargetSnapshot]) -> Option<Vec3> {
    targets
        .iter()
        .find(|target| target.fighter_id == fighter)
        .map(|target| target.position)
}

fn snowflake_shot_spawn(origin: Vec3, facing: Vec3, size_scale: f32) -> (Vec3, Vec3) {
    let direction = normalized_or_forward(facing);
    let size_scale = penguin_skill_size_scale(size_scale);
    (
        origin
            + direction * PENGUIN_SNOWFLAKE_SHOT_FORWARD * size_scale
            + Vec3::Y * PENGUIN_SNOWFLAKE_SHOT_HEIGHT * size_scale,
        direction,
    )
}

pub fn penguin_snowflake_swap_target(
    owner: FighterId,
    active_skills: impl IntoIterator<Item = (SimEntityId, PenguinSkillKind, FighterId, TickTimer, Vec3)>,
) -> Option<PenguinSnowflakeSwap> {
    active_skills
        .into_iter()
        .filter(|(_, kind, skill_owner, lifetime, _)| {
            *kind == PenguinSkillKind::SnowflakeShard && *skill_owner == owner && lifetime.active()
        })
        .min_by_key(|(snowflake, ..)| *snowflake)
        .map(
            |(snowflake, _, _, _, penguin_destination)| PenguinSnowflakeSwap {
                snowflake,
                penguin_destination,
            },
        )
}

fn flat_direction(origin: Vec3, target: Vec3) -> Vec3 {
    canonical_math::vec3_normalize_or_zero(Vec3::new(target.x - origin.x, 0.0, target.z - origin.z))
}

fn flat_distance_squared(a: Vec3, b: Vec3) -> f32 {
    canonical_math::vec2_distance_squared(Vec2::new(a.x, a.z), Vec2::new(b.x, b.z))
}

fn grounded_position(arena: &ArenaDefinition, position: Vec3, clearance: f32) -> Vec3 {
    let ground = ground_height(arena, position.x, position.z);
    Vec3::new(position.x, ground + clearance, position.z)
}

fn popsicle_touched_ground(
    skill: &ActivePenguinSkill,
    position: Vec3,
    arena: &ArenaDefinition,
) -> bool {
    if skill.kind != PenguinSkillKind::PopsicleBounce {
        return false;
    }
    let ground = ground_height(arena, position.x, position.z);
    position.y <= ground + 0.08 && skill.age.as_millis_floor() > 80
}

fn snowman_touched_ground(
    skill: &ActivePenguinSkill,
    position: Vec3,
    arena: &ArenaDefinition,
) -> bool {
    if skill.kind != PenguinSkillKind::SnowmanDrop {
        return false;
    }
    let ground = ground_height(arena, position.x, position.z);
    position.y <= ground + PENGUIN_SNOWMAN_DROP_LAND_CLEARANCE * skill.size_scale
        && skill.age.as_millis_floor() > 80
}

fn snowman_landing_snow_offsets(facing: Vec3, size_scale: f32) -> [Vec3; 2] {
    let facing = normalized_or_forward(facing);
    [
        Vec3::ZERO,
        facing * PENGUIN_ULTIMATE_ICE_FIELD_TILE_SPACING * penguin_skill_size_scale(size_scale),
    ]
}

fn spawn_snowman_landing_snow(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    arena: &ArenaDefinition,
    owner: FighterId,
    owner_id: usize,
    position: Vec3,
    facing: Vec3,
    size_scale: f32,
) {
    for offset in snowman_landing_snow_offsets(facing, size_scale)
        .into_iter()
        .take(PENGUIN_SNOWMAN_DROP_SNOW_TILE_COUNT)
    {
        spawn_ultimate_ice_tile(
            commands,
            identities,
            arena,
            owner,
            owner_id,
            grounded_position(arena, position + offset, 0.014),
            facing,
            size_scale,
            None,
        );
    }
}

fn should_despawn_skill(position: Vec3, arena: &ArenaDefinition) -> bool {
    debug_assert!(arena.ringout_radius >= 0.0);
    position.y < arena.ringout_y
        || canonical_math::vec2_length_squared(Vec2::new(position.x, position.z))
            > arena.ringout_radius * arena.ringout_radius
}

fn ground_height(arena: &ArenaDefinition, x: f32, z: f32) -> f32 {
    ground_support_for_arena_with_radius(arena, x, z, 0.0)
        .height()
        .unwrap_or(ARENA_TOP_Y)
}

fn impact_package(kind: PenguinSkillKind) -> FeedbackPackageId {
    match kind {
        PenguinSkillKind::SledWake
        | PenguinSkillKind::SnowmanDrop
        | PenguinSkillKind::BodySlamShockwave => FeedbackPackageId::SpecialHazardImpact,
        _ => FeedbackPackageId::SpecialProjectileImpact,
    }
}

fn despawn_package(kind: PenguinSkillKind) -> FeedbackPackageId {
    match kind {
        PenguinSkillKind::SledWake
        | PenguinSkillKind::SnowmanDrop
        | PenguinSkillKind::BodySlamShockwave => FeedbackPackageId::SpecialHazardFade,
        _ => FeedbackPackageId::SpecialProjectileRecover,
    }
}

fn snowflake_burst_directions(facing: Vec3) -> [Vec3; 8] {
    let forward = normalized_or_forward(facing);
    let side = canonical_math::vec3_normalize_or_zero(Vec3::new(-forward.z, 0.0, forward.x));
    [
        forward,
        canonical_math::vec3_normalize_or_zero(forward + side),
        side,
        canonical_math::vec3_normalize_or_zero(-forward + side),
        -forward,
        canonical_math::vec3_normalize_or_zero(-forward - side),
        -side,
        canonical_math::vec3_normalize_or_zero(forward - side),
    ]
}

fn normalized_or_forward(value: Vec3) -> Vec3 {
    let normalized = canonical_math::vec3_normalize_or_zero(value);
    if canonical_math::vec3_length_squared(normalized) > 0.01 {
        normalized
    } else {
        Vec3::Z
    }
}

fn projectile_rotation(facing: Vec3) -> Quat {
    Quat::from_rotation_arc(Vec3::Z, normalized_or_forward(facing))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::characters::{CharacterMoveCatalog, FighterCharacter};
    use crate::combat::{begin_contact_collection, resolve_contacts};
    use crate::components::{FighterAction, FighterGrabState, FighterUltimateState};
    use crate::equipment::{EquipmentKind, FighterEquipment};
    use crate::game_state::MatchTelemetry;
    use crate::reactions::ReactionFamilyId;
    use crate::sim_event::{PresentationEventCursor, PresentationEventRouter, SimEventJournal};
    use crate::styles::FighterStyle;

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct FrozenPenguinTargetState {
        fighter: FighterId,
        health_bits: u32,
        stamina_bits: u32,
        last_attacker: Option<FighterId>,
        action: FighterAction,
        reaction: Option<ReactionFamilyId>,
        velocity_bits: [u32; 3],
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct FrozenPenguinContactFixture {
        accepted_targets: Vec<FighterId>,
        events: Vec<SimEvent>,
        source: SimEntityId,
        source_hit_memory: u8,
        source_age_ticks: u32,
        source_lifetime_ticks: u32,
        target_state: Vec<FrozenPenguinTargetState>,
    }

    fn fighter(index: u8) -> FighterId {
        FighterId::new(index).expect("test fighter index should be valid")
    }

    fn sim(kind: SimEntityKind, index: u32) -> SimEntityId {
        SimEntityId::new(kind, index, 1)
    }

    fn arena() -> &'static ArenaDefinition {
        crate::arena_defs::arena_definition(0)
    }

    fn local_entity(index: u32) -> Entity {
        Entity::from_raw_u32(index).expect("test entity index should be valid")
    }

    fn assert_vec3_close(actual: Vec3, expected: Vec3) {
        assert!(
            (actual - expected).length() <= 0.0001,
            "expected {actual:?} to be close to {expected:?}"
        );
    }

    fn contact_fixture_state() -> MatchState {
        let mut state = MatchState::default();
        state.rules = crate::game_state::RULE_PRESETS[1];
        state.rule_index = 1;
        state.set_active_slots([true, true, true, false]);
        state.reset_for_new_match();
        state
    }

    fn contact_fixture_fighter(id: FighterId, position: Vec3) -> impl Bundle {
        (
            Fighter {
                id: id.index(),
                name: "Penguin contact fixture",
                color: Color::WHITE,
                spawn: position,
            },
            FighterCharacter::new(CharacterKind::Cat),
            FighterStats::default(),
            FighterMotor {
                grounded: true,
                facing: Vec3::Z,
                ..default()
            },
            FighterActionState::default(),
            FighterGrabState::default(),
            FighterUltimateState::default(),
            FighterStyle {
                kind: FighterStyleKind::Anchor,
            },
            FighterEquipment::new(EquipmentKind::CounterCell),
            SimPosition::new(position),
        )
    }

    fn spawn_contact_fixture_penguin_skill(app: &mut App, position: Vec3) -> (Entity, SimEntityId) {
        let skill = active_penguin_skill(
            PenguinSkillKind::BodySlamShockwave,
            FighterId::ZERO,
            0,
            FighterStyleKind::Anchor,
            Vec3::Z,
            Vec3::ZERO,
            None,
            1.0,
        );
        let entity = app
            .world_mut()
            .spawn((skill, SimPosition::new(position)))
            .id();
        let stable = app
            .world_mut()
            .resource_mut::<SimulationIdentityAllocator>()
            .try_allocate(SimEntityKind::PenguinSkill, entity)
            .unwrap();
        let source = stable.id();
        app.world_mut().entity_mut(entity).insert(stable);
        (entity, source)
    }

    fn run_frozen_penguin_contact_fixture(
        reverse_ecs_allocation: bool,
    ) -> FrozenPenguinContactFixture {
        let owner = FighterId::ZERO;
        let target_a = fighter(1);
        let target_b = fighter(2);
        let target_position = Vec3::new(0.0, ARENA_TOP_Y, 0.0);

        let mut app = App::new();
        app.insert_resource(contact_fixture_state())
            .insert_resource(ActiveArena::default())
            .init_resource::<SimulationIdentityAllocator>()
            .init_resource::<CombatFeelTuning>()
            .init_resource::<CharacterMoveCatalog>()
            .init_resource::<Hitstop>()
            .init_resource::<MatchTelemetry>()
            .init_resource::<ContactBuffer>()
            .insert_resource(TickEventBuffer::new(SimTick(83)))
            .add_systems(
                Update,
                (
                    begin_contact_collection,
                    collect_penguin_skill_contacts,
                    resolve_contacts,
                    apply_penguin_skill_contact_outcomes,
                )
                    .chain(),
            );

        let early_source = (!reverse_ecs_allocation)
            .then(|| spawn_contact_fixture_penguin_skill(&mut app, target_position));
        let fighter_order = if reverse_ecs_allocation {
            [target_b, target_a, owner]
        } else {
            [owner, target_a, target_b]
        };
        for fighter_id in fighter_order {
            let position = if fighter_id == owner {
                target_position + Vec3::X * 5.0
            } else {
                target_position
            };
            app.world_mut()
                .spawn(contact_fixture_fighter(fighter_id, position));
        }
        let (source_entity, source) = early_source
            .unwrap_or_else(|| spawn_contact_fixture_penguin_skill(&mut app, target_position));

        app.update();

        let accepted_targets = {
            let contacts = app.world().resource::<ContactBuffer>();
            (0..contacts.len())
                .filter_map(|index| {
                    let record = contacts.record(index)?;
                    let outcome = contacts.outcome(index)?;
                    (record.source.entity() == Some(source)
                        && matches!(
                            outcome.kind,
                            ContactOutcomeKind::Accepted | ContactOutcomeKind::Guarded
                        ))
                    .then_some(record.target)
                })
                .collect()
        };
        let events = app
            .world()
            .resource::<TickEventBuffer>()
            .iter()
            .copied()
            .collect();
        let skill = app
            .world()
            .get::<ActivePenguinSkill>(source_entity)
            .expect("body-slam shockwave persists after its frozen multi-target batch");
        let (source_hit_memory, source_age_ticks, source_lifetime_ticks) = (
            skill.already_hit.bits(),
            skill.age.get(),
            skill.lifetime.remaining(),
        );
        let target_state = {
            let world = app.world_mut();
            let mut fighters =
                world.query::<(&Fighter, &FighterStats, &FighterMotor, &FighterActionState)>();
            let mut state = fighters
                .iter(world)
                .filter_map(|(fighter, stats, motor, action)| {
                    let fighter = FighterId::from_index(fighter.id)?;
                    (fighter != owner).then_some(FrozenPenguinTargetState {
                        fighter,
                        health_bits: stats.health.to_bits(),
                        stamina_bits: stats.stamina.to_bits(),
                        last_attacker: stats.last_attacker,
                        action: action.action,
                        reaction: action.reaction_family,
                        velocity_bits: [
                            motor.velocity.x.to_bits(),
                            motor.velocity.y.to_bits(),
                            motor.velocity.z.to_bits(),
                        ],
                    })
                })
                .collect::<Vec<_>>();
            state.sort_by_key(|target| target.fighter);
            state
        };

        FrozenPenguinContactFixture {
            accepted_targets,
            events,
            source,
            source_hit_memory,
            source_age_ticks,
            source_lifetime_ticks,
            target_state,
        }
    }

    #[test]
    fn frozen_penguin_shockwave_is_independent_of_target_and_source_ecs_allocation_order() {
        let forward = run_frozen_penguin_contact_fixture(false);
        let reversed = run_frozen_penguin_contact_fixture(true);

        assert_eq!(forward, reversed);
        assert_eq!(forward.accepted_targets, vec![fighter(1), fighter(2)]);
        assert_eq!(
            forward.source_hit_memory,
            (1 << fighter(1).index()) | (1 << fighter(2).index())
        );
        assert_eq!(forward.source_age_ticks, 1);
        assert!(forward.source_lifetime_ticks > 0);
        assert!(
            forward
                .target_state
                .iter()
                .all(|target| target.health_bits != crate::constants::MAX_HEALTH.to_bits()),
            "{forward:?}"
        );
        assert_eq!(
            forward
                .events
                .iter()
                .filter_map(|event| match event.kind {
                    SimEventKind::HitConfirmed { victim, .. } => Some(victim),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            vec![fighter(1), fighter(2)]
        );
        assert_eq!(
            forward
                .events
                .iter()
                .map(|event| event.id)
                .collect::<Vec<_>>(),
            vec![
                SimEventId {
                    tick: SimTick(83),
                    source: SimEventSource::Entity(forward.source),
                    ordinal: 0,
                },
                SimEventId {
                    tick: SimTick(83),
                    source: SimEventSource::Entity(forward.source),
                    ordinal: 1,
                },
            ]
        );
    }

    fn presentation_intent_at(tick: u64, ordinal: u16) -> PenguinPresentationIntent {
        let entity = sim(SimEntityKind::PenguinSkill, 0);
        PenguinPresentationIntent {
            event_id: SimEventId {
                tick: SimTick(tick),
                source: SimEventSource::Entity(entity),
                ordinal,
            },
            entity,
            kind: PenguinPresentationKind::Lifecycle {
                event: AbilityLifecycleEvent::Spawned,
                position: Vec3::ZERO,
                direction: Vec3::X,
                package: Some(FeedbackPackageId::SpecialProjectileStartup),
                cue: None,
                source: ImpactSource::Projectile,
                priority: 24,
                hud_flash: None,
            },
        }
    }

    fn commit_presentation_event(
        journal: &mut SimEventJournal,
        intents: &mut PenguinPresentationIntentJournal,
        tick: u64,
    ) -> SimEventId {
        let intent = presentation_intent_at(tick, 0);
        let mut buffer = TickEventBuffer::new(SimTick(tick));
        let event_id = buffer
            .emit(
                SimEventSource::Entity(intent.entity),
                SimEventKind::AbilityLifecycle {
                    entity: intent.entity,
                    event: AbilityLifecycleEvent::Spawned,
                },
            )
            .unwrap();
        journal.commit(&buffer);
        intents
            .record(PenguinPresentationIntent { event_id, ..intent })
            .unwrap();
        event_id
    }

    fn spawn_test_penguin_entities(
        mut commands: Commands,
        mut identities: ResMut<SimulationIdentityAllocator>,
        state: Res<MatchState>,
        arena: Res<ActiveArena>,
    ) {
        assert!(spawn_penguin_skill_with_presentation(
            &mut commands,
            &mut identities,
            &state,
            arena.definition(),
            FighterId::ZERO,
            0,
            FighterStyleKind::Anchor,
            Vec3::ZERO,
            Vec3::X,
            false,
            1.0,
            PenguinSkillId::SnowflakeShot,
            &[],
            std::iter::empty(),
        ));
        assert!(spawn_penguin_skill_with_presentation(
            &mut commands,
            &mut identities,
            &state,
            arena.definition(),
            FighterId::ZERO,
            0,
            FighterStyleKind::Anchor,
            Vec3::ZERO,
            Vec3::X,
            false,
            1.0,
            PenguinSkillId::SpringPeck,
            &[],
            std::iter::empty(),
        ));
    }

    #[test]
    fn headless_penguin_spawn_has_only_canonical_components_and_semantic_events() {
        let mut app = App::new();
        app.init_resource::<SimulationIdentityAllocator>()
            .insert_resource(TickEventBuffer::new(SimTick(8)))
            .insert_resource(MatchState::default())
            .insert_resource(ActiveArena::default())
            .init_resource::<CombatFeelTuning>()
            .init_resource::<Hitstop>()
            .init_resource::<ContactBuffer>()
            .add_systems(
                Update,
                (
                    spawn_test_penguin_entities,
                    collect_penguin_skill_contacts,
                    apply_penguin_skill_contact_outcomes,
                    update_penguin_surfaces,
                )
                    .chain(),
            );

        app.update();

        let world = app.world_mut();
        let mut roots = world
            .query_filtered::<Entity, Or<(With<ActivePenguinSkill>, With<ActivePenguinSurface>)>>();
        let entities = roots.iter(world).collect::<Vec<_>>();
        assert_eq!(entities.len(), 3);
        for entity in entities {
            assert!(world.get::<StableSimEntity>(entity).is_some());
            assert!(world.get::<SimPosition>(entity).is_some());
            assert!(world.get::<Transform>(entity).is_none());
            assert!(world.get::<SceneRoot>(entity).is_none());
            assert!(world.get::<PenguinSkillVisualRoot>(entity).is_none());
            assert!(world.get::<PenguinSurfaceVisualRoot>(entity).is_none());
        }
        assert!(world.get_resource::<PenguinSkillAssets>().is_none());
        assert!(world.get_resource::<EffectAssets>().is_none());
        assert!(world.get_resource::<HitEffects>().is_none());
        assert_eq!(world.resource::<TickEventBuffer>().len(), 3);
    }

    #[test]
    fn penguin_presentation_journal_is_bounded_and_validates_semantics() {
        let mut intents = PenguinPresentationIntentJournal::default();
        for tick in 0..SIM_EVENT_HISTORY_TICKS as u64 {
            for ordinal in 0..MAX_SIM_EVENTS_PER_TICK as u16 {
                intents
                    .record(presentation_intent_at(tick, ordinal))
                    .unwrap();
            }
        }
        assert_eq!(intents.len(), intents.capacity());

        let bad = presentation_intent_at(999, MAX_SIM_EVENTS_PER_TICK as u16);
        assert_eq!(
            intents.record(bad),
            Err(EventEmitError::CapacityExceeded {
                capacity: MAX_SIM_EVENTS_PER_TICK,
            })
        );
        assert_eq!(intents.metrics().rejected, 1);

        let intent = presentation_intent_at(7, 0);
        let wrong_semantic = SimEvent {
            id: intent.event_id,
            kind: SimEventKind::AbilityLifecycle {
                entity: intent.entity,
                event: AbilityLifecycleEvent::Despawned,
            },
        };
        assert!(!penguin_presentation_matches_event(wrong_semantic, intent));
    }

    #[test]
    fn penguin_events_survive_render_stall_and_rollback_exactly_once() {
        let mut journal = SimEventJournal::default();
        let mut intents = PenguinPresentationIntentJournal::default();
        for tick in 40..43 {
            commit_presentation_event(&mut journal, &mut intents, tick);
        }

        let mut cursor = PresentationEventCursor::default();
        let mut router = PresentationEventRouter::default();
        let mut presented = Vec::new();
        cursor
            .route_available(&journal, &mut router, Some(SimTick(42)), |event| {
                if let Some(intent) = intents.get(event.id)
                    && penguin_presentation_matches_event(event, intent)
                {
                    presented.push(event.id);
                }
            })
            .unwrap();
        assert_eq!(presented.len(), 3);

        let retained = SimTick(40);
        journal.discard_after(retained);
        cursor.discard_after(retained);
        router.discard_after(retained);
        intents.discard_after(retained);
        for tick in 41..43 {
            commit_presentation_event(&mut journal, &mut intents, tick);
        }
        cursor
            .route_available(&journal, &mut router, Some(SimTick(42)), |event| {
                if let Some(intent) = intents.get(event.id)
                    && penguin_presentation_matches_event(event, intent)
                {
                    presented.push(event.id);
                }
            })
            .unwrap();

        assert_eq!(presented.len(), 3);
        assert_eq!(router.metrics().duplicate_events_suppressed, 2);
        assert_eq!(intents.metrics().discarded, 2);
    }

    #[test]
    fn aim_held_lock_target_selects_valid_enemy_in_front() {
        let mut state = MatchState::default();
        state.active_slots = [true, true, true, false];
        state.active_fighter_count = 3;
        let targets = [
            BeeSkillTargetSnapshot {
                fighter_id: fighter(1),
                position: Vec3::new(4.0, 0.0, 0.0),
            },
            BeeSkillTargetSnapshot {
                fighter_id: fighter(2),
                position: Vec3::new(-1.0, 0.0, 0.0),
            },
        ];

        assert_eq!(
            penguin_skill_lock_target(fighter(0), Vec3::ZERO, Vec3::X, true, &state, &targets),
            Some(fighter(1))
        );
        assert_eq!(
            penguin_skill_lock_target(fighter(0), Vec3::ZERO, Vec3::X, false, &state, &targets),
            None
        );
    }

    #[test]
    fn lock_target_breaks_equal_distance_ties_by_fighter_id() {
        let mut state = MatchState::default();
        state.rules = crate::game_state::RULE_PRESETS[1];
        state.rule_index = 1;
        state.active_slots = [true, true, true, false];
        state.active_fighter_count = 3;
        let targets = [
            BeeSkillTargetSnapshot {
                fighter_id: fighter(2),
                position: Vec3::new(3.0, 0.0, 1.0),
            },
            BeeSkillTargetSnapshot {
                fighter_id: fighter(1),
                position: Vec3::new(3.0, 0.0, -1.0),
            },
        ];

        assert_eq!(
            penguin_skill_lock_target(fighter(0), Vec3::ZERO, Vec3::X, true, &state, &targets),
            Some(fighter(1))
        );
    }

    #[test]
    fn lock_target_ignores_inactive_and_friendly_slots() {
        let mut state = MatchState::default();
        state.active_slots = [true, true, true, false];
        state.active_fighter_count = 3;
        let targets = [
            BeeSkillTargetSnapshot {
                fighter_id: fighter(2),
                position: Vec3::new(2.0, 0.0, 0.0),
            },
            BeeSkillTargetSnapshot {
                fighter_id: fighter(1),
                position: Vec3::new(3.0, 0.0, 0.0),
            },
        ];

        assert_eq!(
            penguin_skill_lock_target(fighter(0), Vec3::ZERO, Vec3::X, true, &state, &targets),
            Some(fighter(1))
        );
    }

    #[test]
    fn fish_torpedo_velocity_turns_toward_captured_target() {
        let mut skill = active_penguin_skill(
            PenguinSkillKind::FishTorpedo,
            fighter(0),
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::X * PENGUIN_FISH_TORPEDO_SPEED,
            Some(fighter(1)),
            1.0,
        );

        steer_fish_torpedo_toward(&mut skill, Vec3::ZERO, Vec3::new(0.0, 0.0, 3.0), 0.08);

        assert!(skill.velocity.z > 0.0);
        assert!(skill.velocity.x < PENGUIN_FISH_TORPEDO_SPEED);
    }

    #[test]
    fn sled_wake_repeat_window_clears_contact_memory() {
        let mut skill = active_penguin_skill(
            PenguinSkillKind::SledWake,
            fighter(0),
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::X,
            None,
            1.0,
        );
        skill.already_hit.insert(fighter(1));
        skill.repeat_timer = Some(TickTimer::from_ticks(1));

        assert!(update_skill_repeat_window(&mut skill));

        assert!(skill.already_hit.is_empty());
        assert_eq!(skill.repeat_timer, skill.repeat_interval);
    }

    #[test]
    fn mushroom_size_scale_enlarges_penguin_skill_collision_radii() {
        let size_scale = crate::constants::ITEM_GIANT_SIZE_MULTIPLIER;
        let cases = [
            (PenguinSkillKind::FishTorpedo, PENGUIN_FISH_TORPEDO_RADIUS),
            (PenguinSkillKind::PopsicleBounce, PENGUIN_POPSICLE_RADIUS),
            (PenguinSkillKind::SledWake, PENGUIN_SLED_WAKE_RADIUS),
            (PenguinSkillKind::SnowflakeShard, PENGUIN_SNOWFLAKE_RADIUS),
            (PenguinSkillKind::SnowBoulder, PENGUIN_BOULDER_RADIUS),
            (PenguinSkillKind::SnowmanDrop, PENGUIN_SNOWMAN_DROP_RADIUS),
            (
                PenguinSkillKind::BodySlamShockwave,
                PENGUIN_BODY_SLAM_RADIUS,
            ),
        ];

        for (kind, base_radius) in cases {
            let skill = active_penguin_skill(
                kind,
                fighter(0),
                0,
                FighterStyleKind::Anchor,
                Vec3::X,
                Vec3::X,
                None,
                size_scale,
            );

            assert_eq!(skill.size_scale, size_scale);
            assert_eq!(skill.radius, base_radius * size_scale);
        }
    }

    #[test]
    fn mushroom_size_scale_enlarges_penguin_skill_visuals() {
        let size_scale = crate::constants::ITEM_GIANT_SIZE_MULTIPLIER;

        assert_vec3_close(
            penguin_skill_visual_scale(PenguinSkillKind::FishTorpedo, size_scale, 0.0),
            Vec3::splat(1.75 * size_scale),
        );
        assert_vec3_close(
            penguin_skill_visual_scale(PenguinSkillKind::PopsicleBounce, size_scale, 0.0),
            Vec3::splat(1.55 * size_scale),
        );
        assert_vec3_close(
            penguin_skill_visual_scale(PenguinSkillKind::SnowflakeShard, size_scale, 0.0),
            Vec3::splat(1.35 * size_scale),
        );
        assert_vec3_close(
            penguin_skill_visual_scale(PenguinSkillKind::SledWake, size_scale, 0.25),
            Vec3::splat(sled_wake_visual_pulse(0.25) * size_scale),
        );
        assert_vec3_close(
            penguin_skill_visual_scale(PenguinSkillKind::SnowBoulder, size_scale, 0.0),
            Vec3::splat(0.9 * size_scale),
        );
        assert_vec3_close(
            penguin_skill_visual_scale(PenguinSkillKind::SnowmanDrop, size_scale, 0.0),
            Vec3::splat(0.82 * PENGUIN_SNOWMAN_DROP_SIZE_MULTIPLIER * size_scale),
        );
        assert!(
            penguin_skill_visual_scale(PenguinSkillKind::BodySlamShockwave, size_scale, 0.2).x
                > size_scale
        );
    }

    #[test]
    fn snowman_drop_payload_persists_and_freezes_until_landing() {
        let skill = active_penguin_skill(
            PenguinSkillKind::SnowmanDrop,
            fighter(0),
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::Y * -PENGUIN_SNOWMAN_DROP_INITIAL_FALL_SPEED,
            None,
            1.0,
        );

        assert_eq!(skill.payload_id, AttackPayloadId::PenguinSnowmanDrop);
        assert_eq!(skill.shape_id, AttackShapeId::ProjectileBolt);
        assert_eq!(skill.radius, PENGUIN_SNOWMAN_DROP_RADIUS);
        assert_eq!(
            skill.lifetime,
            TickTimer::from_seconds_ceil(PENGUIN_SNOWMAN_DROP_LIFETIME)
        );
        assert!(penguin_skill_persists_after_hit(skill.kind));
    }

    #[test]
    fn snowman_landing_snow_offsets_make_two_forward_tiles() {
        let offsets = snowman_landing_snow_offsets(Vec3::Z, 1.0);

        assert_eq!(offsets.len(), PENGUIN_SNOWMAN_DROP_SNOW_TILE_COUNT);
        assert_vec3_close(offsets[0], Vec3::ZERO);
        assert_vec3_close(
            offsets[1],
            Vec3::Z * PENGUIN_ULTIMATE_ICE_FIELD_TILE_SPACING,
        );
    }

    #[test]
    fn snowflake_burst_uses_radial_directions() {
        let directions = snowflake_burst_directions(Vec3::X);

        assert_eq!(directions.len(), 8);
        assert!(directions.iter().all(|direction| {
            (direction.length() - 1.0).abs() <= 0.0001 && direction.y.abs() <= 0.0001
        }));
        assert!(directions.contains(&Vec3::X));
        assert!(directions.contains(&Vec3::NEG_X));
    }

    #[test]
    fn snowflake_shot_spawn_uses_one_forward_projectile() {
        let (spawn, direction) = snowflake_shot_spawn(Vec3::new(1.0, 0.5, -2.0), Vec3::Z, 1.0);

        assert_vec3_close(direction, Vec3::Z);
        assert_vec3_close(
            spawn,
            Vec3::new(1.0, 0.5 + PENGUIN_SNOWFLAKE_SHOT_HEIGHT, -2.0)
                + Vec3::Z * PENGUIN_SNOWFLAKE_SHOT_FORWARD,
        );
    }

    #[test]
    fn snowflake_swap_uses_live_projectile_position_as_penguin_destination() {
        let snowflake = sim(SimEntityKind::PenguinSkill, 3);
        let position = Vec3::new(1.0, 0.5, -2.0);
        let swap = penguin_snowflake_swap_target(
            fighter(0),
            [(
                snowflake,
                PenguinSkillKind::SnowflakeShard,
                fighter(0),
                TickTimer::from_seconds_ceil(0.4),
                position,
            )],
        )
        .unwrap();

        assert_eq!(swap.snowflake, snowflake);
        assert_vec3_close(swap.penguin_destination, position);
    }

    #[test]
    fn snowflake_swap_breaks_multiple_live_projectiles_by_stable_id() {
        let lower = sim(SimEntityKind::PenguinSkill, 2);
        let higher = sim(SimEntityKind::PenguinSkill, 7);
        let lower_position = Vec3::new(-2.0, 0.4, 1.0);
        let higher_position = Vec3::new(4.0, 0.7, -3.0);

        let swap = penguin_snowflake_swap_target(
            fighter(0),
            [
                (
                    higher,
                    PenguinSkillKind::SnowflakeShard,
                    fighter(0),
                    TickTimer::from_ticks(5),
                    higher_position,
                ),
                (
                    lower,
                    PenguinSkillKind::SnowflakeShard,
                    fighter(0),
                    TickTimer::from_ticks(5),
                    lower_position,
                ),
            ],
        )
        .unwrap();

        assert_eq!(swap.snowflake, lower);
        assert_vec3_close(swap.penguin_destination, lower_position);
    }

    #[test]
    fn snowflake_swap_requires_owners_active_snowflake() {
        let position = Vec3::new(1.0, 0.5, -2.0);

        assert!(
            penguin_snowflake_swap_target(
                fighter(0),
                std::iter::empty::<(SimEntityId, PenguinSkillKind, FighterId, TickTimer, Vec3,)>(),
            )
            .is_none()
        );
        assert!(
            penguin_snowflake_swap_target(
                fighter(0),
                [(
                    sim(SimEntityKind::PenguinSkill, 3),
                    PenguinSkillKind::FishTorpedo,
                    fighter(0),
                    TickTimer::from_seconds_ceil(0.4),
                    position,
                )]
            )
            .is_none()
        );
        assert!(
            penguin_snowflake_swap_target(
                fighter(0),
                [(
                    sim(SimEntityKind::PenguinSkill, 3),
                    PenguinSkillKind::SnowflakeShard,
                    fighter(1),
                    TickTimer::from_seconds_ceil(0.4),
                    position
                )]
            )
            .is_none()
        );
        assert!(
            penguin_snowflake_swap_target(
                fighter(0),
                [(
                    sim(SimEntityKind::PenguinSkill, 3),
                    PenguinSkillKind::SnowflakeShard,
                    fighter(0),
                    TickTimer::ZERO,
                    position
                )]
            )
            .is_none()
        );
    }

    #[test]
    fn snowflake_shot_lasts_longer_and_is_single_cast_per_owner() {
        let owner = fighter(0);
        let skill = active_penguin_skill(
            PenguinSkillKind::SnowflakeShard,
            owner,
            0,
            FighterStyleKind::Anchor,
            Vec3::X,
            Vec3::X,
            None,
            1.0,
        );

        assert_eq!(skill.lifetime, TickTimer::from_seconds_ceil(1.08));
        assert!(penguin_snowflake_shot_is_active(
            fighter(0),
            [(PenguinSkillKind::SnowflakeShard, fighter(0), skill.lifetime)]
        ));
        assert!(!penguin_snowflake_shot_is_active(
            fighter(0),
            [(
                PenguinSkillKind::SnowflakeShard,
                fighter(0),
                TickTimer::ZERO
            )]
        ));
        assert!(!penguin_snowflake_shot_is_active(
            fighter(0),
            [(PenguinSkillKind::SnowflakeShard, fighter(1), skill.lifetime)]
        ));
        assert!(!penguin_snowflake_shot_is_active(
            fighter(0),
            [(PenguinSkillKind::FishTorpedo, fighter(0), skill.lifetime)]
        ));
    }

    #[test]
    fn snowflake_magic_destination_chooses_farthest_flat_snowfield() {
        let target = Vec3::new(0.0, ARENA_TOP_Y, 0.0);
        let destination = snowflake_magic_destination_from_surfaces(
            target,
            [
                (
                    PenguinSurfaceKind::IceTrailSegment,
                    PENGUIN_ICE_TRAIL_LIFETIME,
                    Vec3::new(1.0, ARENA_TOP_Y + 0.1, 0.0),
                ),
                (
                    PenguinSurfaceKind::UltimateIceTile,
                    PENGUIN_ULTIMATE_ICE_FIELD_LIFETIME,
                    Vec3::new(-4.0, ARENA_TOP_Y + 0.2, 0.0),
                ),
            ],
        )
        .expect("active snowfield should be selected");

        assert_vec3_close(destination, Vec3::new(-4.0, ARENA_TOP_Y, 0.0));
    }

    #[test]
    fn snowflake_magic_destination_ignores_non_flat_or_expired_snow() {
        let target = Vec3::new(0.0, ARENA_TOP_Y, 0.0);
        let destination = snowflake_magic_destination_from_surfaces(
            target,
            [
                (
                    PenguinSurfaceKind::SnowHillRamp,
                    PENGUIN_SNOW_HILL_LIFETIME,
                    Vec3::new(8.0, ARENA_TOP_Y, 0.0),
                ),
                (
                    PenguinSurfaceKind::SnowSlopeRide,
                    PENGUIN_SNOW_SLOPE_RIDE_LIFETIME,
                    Vec3::new(-8.0, ARENA_TOP_Y, 0.0),
                ),
                (
                    PenguinSurfaceKind::IceTrailSegment,
                    0.0,
                    Vec3::new(12.0, ARENA_TOP_Y, 0.0),
                ),
            ],
        );

        assert!(destination.is_none());
    }

    #[test]
    fn penguin_skill_assets_exist_for_runtime_loading() {
        for path in [
            "assets/food/kenney_food_kit/fish-bones.glb",
            "assets/food/kenney_food_kit/popsicle.glb",
            "assets/holiday/kenney_holiday_kit/snow-pile.glb",
            "assets/holiday/kenney_holiday_kit/snowflake-a.glb",
            "assets/holiday/kenney_holiday_kit/snowman.glb",
            "assets/holiday/kenney_holiday_kit/snow-flat-large.glb",
            "assets/holiday/kenney_holiday_kit/snow-flat.glb",
            "assets/holiday/kenney_holiday_kit/Textures/colormap.png",
            "assets/tower_defense/kenney_tower_defense_kit/snow-tile-straight.glb",
            "assets/tower_defense/kenney_tower_defense_kit/snow-tile-bump.glb",
            "assets/tower_defense/kenney_tower_defense_kit/snow-tile-hill.glb",
            "assets/tower_defense/kenney_tower_defense_kit/snow-wood-structure.glb",
            "assets/tower_defense/kenney_tower_defense_kit/weapon-cannon.glb",
            "assets/tower_defense/kenney_tower_defense_kit/weapon-ammo-boulder.glb",
            "assets/tower_defense/kenney_tower_defense_kit/Textures/colormap.png",
            "assets/platformer/kenney_platformer_kit/block-snow-large-slope.glb",
            "assets/platformer/kenney_platformer_kit/block-snow-large-slope-steep.glb",
            "assets/platformer/kenney_platformer_kit/spring.glb",
            "assets/platformer/kenney_platformer_kit/Textures/colormap.png",
        ] {
            assert!(std::path::Path::new(path).exists(), "{path} should exist");
        }
    }

    #[test]
    fn ice_trail_cap_despawns_oldest_segments_per_owner() {
        let owner = fighter(0);
        let other = fighter(1);
        let segments = [
            (
                sim(SimEntityKind::PenguinSurface, 10),
                owner,
                ElapsedTicks::from_ticks(240),
            ),
            (
                sim(SimEntityKind::PenguinSurface, 11),
                owner,
                ElapsedTicks::from_ticks(60),
            ),
            (
                sim(SimEntityKind::PenguinSurface, 12),
                owner,
                ElapsedTicks::from_ticks(180),
            ),
            (
                sim(SimEntityKind::PenguinSurface, 13),
                other,
                ElapsedTicks::from_ticks(120),
            ),
        ];

        let despawn = oldest_ice_segments_to_despawn(&segments, 2).unwrap();

        assert_eq!(
            despawn.as_slice(),
            &[sim(SimEntityKind::PenguinSurface, 10)]
        );
    }

    #[test]
    fn ice_trail_collection_reports_fixed_surface_capacity_overflow() {
        let segment = (
            sim(SimEntityKind::PenguinSurface, 0),
            fighter(0),
            ElapsedTicks::ZERO,
        );
        let segments = [segment; PENGUIN_SURFACE_ENTITY_CAPACITY + 1];

        assert_eq!(
            oldest_ice_segments_to_despawn(&segments, PENGUIN_ICE_TRAIL_CAP_PER_OWNER),
            Err(FixedPenguinCollectionOverflow {
                collection: "per-owner Penguin ice segments",
                capacity: PENGUIN_SURFACE_ENTITY_CAPACITY,
            })
        );
    }

    #[test]
    fn ultimate_ice_modifier_hard_slide_overrides_soft_ice() {
        let soft = penguin_ice_modifier_from_overlaps(true, false, CharacterKind::Cat).unwrap();
        let hard = penguin_ice_modifier_from_overlaps(true, true, CharacterKind::Cat).unwrap();

        assert!(!soft.hard_slide);
        assert!(hard.hard_slide);
        assert_eq!(hard.input_scale, 0.0);
        assert!(hard.ground_friction_scale < soft.ground_friction_scale);
    }

    #[test]
    fn ultimate_ice_field_constants_make_four_by_four_ten_second_patch() {
        let tile_count = PENGUIN_ULTIMATE_ICE_FIELD_GRID_SIDE.pow(2) as usize;

        assert_eq!(tile_count, 16);
        assert_eq!(PENGUIN_ULTIMATE_ICE_FIELD_LIFETIME, 10.0);
        assert!(PENGUIN_ULTIMATE_ICE_FIELD_TILE_SPACING > PENGUIN_ULTIMATE_ICE_FIELD_TILE_RADIUS);
        assert!(PENGUIN_ULTIMATE_SNOW_FIELD_CLEARANCE < 0.03);
        assert!(PENGUIN_ULTIMATE_SNOW_FLAT_LARGE_SCALE > PENGUIN_ULTIMATE_SNOW_FLAT_DETAIL_SCALE);
    }

    #[test]
    fn ultimate_ice_field_even_grid_offsets_stay_centered() {
        let first = ultimate_ice_field_grid_axis_offset(0);
        let second = ultimate_ice_field_grid_axis_offset(1);
        let third = ultimate_ice_field_grid_axis_offset(2);
        let last = ultimate_ice_field_grid_axis_offset(3);

        assert!((first + 1.5).abs() < 0.001);
        assert!((last - 1.5).abs() < 0.001);
        assert!((second + third).abs() < 0.001);
    }

    #[test]
    fn ultimate_snow_field_scale_fades_only_across_the_floor() {
        let full = ultimate_snow_field_visual_scale(1.4, PENGUIN_ULTIMATE_ICE_FIELD_LIFETIME);
        let fading = ultimate_snow_field_visual_scale(1.4, 0.1);

        assert_eq!(full.y, 1.4);
        assert_eq!(fading.y, 1.4);
        assert!(full.x > fading.x);
        assert!(full.z > fading.z);
    }

    #[test]
    fn snow_hill_ramp_owner_touch_is_ski_ride() {
        let owner = fighter(0);
        let surface = active_penguin_surface(
            PenguinSurfaceKind::SnowHillRamp,
            owner,
            0,
            Vec3::X,
            PENGUIN_SNOW_HILL_RADIUS,
            PENGUIN_SNOW_HILL_LIFETIME,
            1.0,
        );

        let owner_touch = snow_hill_ramp_touch(&surface, owner);
        let opponent_touch = snow_hill_ramp_touch(&surface, fighter(1));

        assert!(owner_touch.owner_ride);
        assert!(owner_touch.forward_push > opponent_touch.forward_push);
        assert!(owner_touch.lift < opponent_touch.lift);
        assert!(owner_touch.slide_timer > 0.0);
        assert!(owner_touch.speed_limit_timer > 0.0);
        assert!(!opponent_touch.owner_ride);
        assert_eq!(opponent_touch.slide_timer, 0.0);
    }

    #[test]
    fn snow_slope_ride_low_side_faces_penguin() {
        let facing = Vec3::X;
        let rotation = snow_slope_ride_rotation(facing);

        assert_vec3_close(rotation * Vec3::Z, -facing);
    }

    #[test]
    fn snow_slope_ride_contact_climbs_from_low_to_high_end() {
        let center = Vec3::ZERO;
        let facing = Vec3::X;
        let low = center - facing * PENGUIN_SNOW_SLOPE_RIDE_HALF_LENGTH;
        let high = center + facing * PENGUIN_SNOW_SLOPE_RIDE_HALF_LENGTH;

        let low_contact = snow_slope_ride_contact(center, facing, low, 1.0, arena()).unwrap();
        let high_contact = snow_slope_ride_contact(center, facing, high, 1.0, arena()).unwrap();

        assert!(low_contact.progress <= 0.001);
        assert!(high_contact.progress >= 0.999);
        assert!(high_contact.target_y > low_contact.target_y);
    }

    #[test]
    fn snow_slope_ride_contact_rejects_side_entries() {
        let center = Vec3::ZERO;
        let facing = Vec3::X;
        let side_entry =
            center + Vec3::Z * (PENGUIN_SNOW_SLOPE_RIDE_HALF_WIDTH + FIGHTER_RADIUS + 0.05);

        assert!(snow_slope_ride_contact(center, facing, side_entry, 1.0, arena()).is_none());
    }

    #[test]
    fn snow_slope_ride_launch_keeps_short_arc_and_landing_slide() {
        assert!(PENGUIN_SNOW_SLOPE_RIDE_LIFT < 1.6);
        assert!(PENGUIN_SNOW_SLOPE_RIDE_PUSH < 5.0);
        assert!(PENGUIN_SNOW_SLOPE_RIDE_SLIDE > PENGUIN_SNOW_SLOPE_RIDE_SPEED_LIMIT);
        assert!(PENGUIN_SNOW_SLOPE_RIDE_EXIT_PROGRESS > 0.9);
    }

    #[test]
    fn active_surface_records_lifetime_radius_and_owner() {
        let surface = active_penguin_surface(
            PenguinSurfaceKind::IceTrailSegment,
            fighter(0),
            0,
            Vec3::X,
            PENGUIN_ICE_TRAIL_RADIUS,
            PENGUIN_ICE_TRAIL_LIFETIME,
            1.25,
        );

        assert_eq!(surface.kind, PenguinSurfaceKind::IceTrailSegment);
        assert_eq!(surface.owner, fighter(0));
        assert_eq!(
            surface.lifetime,
            TickTimer::from_seconds_ceil(PENGUIN_ICE_TRAIL_LIFETIME)
        );
        assert_eq!(surface.radius, PENGUIN_ICE_TRAIL_RADIUS);
        assert_eq!(surface.size_scale, 1.25);
    }

    #[test]
    fn penguin_skill_and_surface_pools_overflow_independently() {
        let mut capacities = [0; SimEntityKind::ALL.len()];
        capacities[SimEntityKind::PenguinSkill.code() as usize] = 1;
        capacities[SimEntityKind::PenguinSurface.code() as usize] = 1;
        let mut identities = SimulationIdentityAllocator::with_capacities(capacities);
        let skill = identities
            .try_allocate(SimEntityKind::PenguinSkill, local_entity(1))
            .unwrap();
        let surface = identities
            .try_allocate(SimEntityKind::PenguinSurface, local_entity(2))
            .unwrap();

        assert!(
            identities
                .try_allocate(SimEntityKind::PenguinSkill, local_entity(3))
                .is_err()
        );
        assert!(
            identities
                .try_allocate(SimEntityKind::PenguinSurface, local_entity(4))
                .is_err()
        );
        assert_eq!(skill.id().kind(), SimEntityKind::PenguinSkill);
        assert_eq!(surface.id().kind(), SimEntityKind::PenguinSurface);
        assert_eq!(identities.mapped_entity(skill.id()), Some(local_entity(1)));
        assert_eq!(
            identities.mapped_entity(surface.id()),
            Some(local_entity(2))
        );
    }
}
