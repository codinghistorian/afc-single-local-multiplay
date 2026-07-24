use crate::arena::ground_support_for_arena_with_radius;
use crate::arena_defs::{ActiveArena, ArenaDefinition};
use crate::canonical_math;
use crate::combat::{
    HitEffects, ImpactFeedbackIntensity, ImpactProfile, ImpactSource, can_receive_impact,
    impact_feedback_profile, impact_profile_from_payload, impact_profile_from_payload_with_feel,
    radial_falloff,
};
use crate::combat_sfx::{CombatSfxCue, CombatSfxKind};
use crate::components::{
    AttackKind, DrunkStatus, Fighter, FighterAction, FighterActionState, FighterInput,
    FighterInventory, FighterMotor, FighterStats, Hitbox, SimPosition,
};
use crate::constants::*;
use crate::contact_arbitration::{
    ContactBuffer, ContactFlags, ContactOutcomeKind, ContactPhase, ContactRecord, ContactSourceKind,
};
use crate::determinism::{
    CanonicalHash64, DEFAULT_F32_QUANTIZATION, DeterministicRngStream, FighterHitMask, FighterId,
    RngStreamName, SimEntityId, SimEntityKind, canonicalize_f32,
};
use crate::ecs_identity::{
    SIM_ENTITY_POOL_CAPACITIES, SimulationIdentityAllocator, StableSimEntity, despawn_stable,
    try_spawn_stable,
};
use crate::effects::{
    EffectAssets, spawn_alcohol_spray, spawn_dust_puff, spawn_guard_flash, spawn_pop_bomb_blast,
};
use crate::feel::CombatFeelTuning;
use crate::fighter::cancel_dash_slide_for_action;
use crate::game_state::{Hitstop, MatchAnnouncements, MatchState};
use crate::reactions::ReactionFamilyId;
use crate::rollback::RollbackEventDiscard;
use crate::sim_event::{
    EventEmitError, ItemLifecycleEvent, MAX_SIM_EVENTS_PER_TICK, SIM_EVENT_HISTORY_TICKS, SimEvent,
    SimEventId, SimEventKind, SimEventSource, TickEventBuffer,
};
use crate::simulation::{ElapsedTicks, SIM_HZ_U32, SimTick, TickTimer, seconds_to_ticks_ceil};
use crate::techniques::{
    AttackPayloadId, AttackShapeId, DamageElement, DamageProfileId, attack_shape_definition,
};
use bevy::gltf::GltfAssetLabel;
use bevy::prelude::*;

const STEAMER_BLAST_ARC_MIN_PLANAR_SPEED: f32 = 18.0;
const STEAMER_BLAST_ARC_MAX_PLANAR_SPEED: f32 = 27.0;
const STEAMER_BLAST_ARC_MIN_VERTICAL_SPEED: f32 = 12.0;
const STEAMER_BLAST_ARC_MAX_VERTICAL_SPEED: f32 = 15.5;
const STEAMER_BLAST_ARC_SPEED_LIMIT_TIME: f32 = 1.25;
const ITEM_DROP_SFX_PRIORITY: u8 = 58;
const STEAMER_EXPLOSION_SFX_PRIORITY: u8 = 96;
const MUSHROOM_BIGGER_SFX_PRIORITY: u8 = 64;
const ITEM_FIXED_DELTA: f32 = 1.0 / SIM_HZ_U32 as f32;
// Frozen 60 Hz barrel integration constants. Keeping these authored values
// avoids platform libm (`exp`/`sin_cos`) in authoritative trajectory updates.
const BARREL_PLANAR_DAMPING_PER_TICK: f32 = 0.964;
const BARREL_TURN_SIN: f32 = 0.099_833_414;
const BARREL_TURN_COS: f32 = 0.995_004_2;
const ITEM_ENTITY_CAPACITY: usize =
    SIM_ENTITY_POOL_CAPACITIES[SimEntityKind::Item.code() as usize] as usize;

#[derive(Clone, Copy)]
struct FixedItemIdSet {
    ids: [Option<SimEntityId>; ITEM_ENTITY_CAPACITY],
    len: usize,
}

impl Default for FixedItemIdSet {
    fn default() -> Self {
        Self {
            ids: [None; ITEM_ENTITY_CAPACITY],
            len: 0,
        }
    }
}

impl FixedItemIdSet {
    fn contains(&self, id: SimEntityId) -> bool {
        self.ids[..self.len].contains(&Some(id))
    }

    /// Inserts a new ID. Returns false for a duplicate or if the fixed item
    /// namespace is already fully represented, preserving fail-closed bounds.
    fn insert(&mut self, id: SimEntityId) -> bool {
        if self.contains(id) || self.len == self.ids.len() {
            return false;
        }
        self.ids[self.len] = Some(id);
        self.len += 1;
        true
    }
}

#[derive(Clone, Copy, Debug)]
struct PendingBarrelSpray {
    source: SimEntityId,
    owner: FighterId,
    position: Vec3,
    spiral_phase: f32,
}

/// Fixed per-tick handoff from item geometry collection to source-outcome
/// consumption. This resource is intentionally excluded from snapshots: every
/// fixed tick clears and rebuilds it before contacts are resolved.
#[derive(Resource, Default)]
pub struct ItemContactFrame {
    tick: Option<SimTick>,
    sprays: [Option<PendingBarrelSpray>; ITEM_ENTITY_CAPACITY],
    spray_len: usize,
}

impl ItemContactFrame {
    fn begin_tick(&mut self, tick: SimTick) {
        for pending in &mut self.sprays[..self.spray_len] {
            *pending = None;
        }
        self.tick = Some(tick);
        self.spray_len = 0;
    }

    fn record_spray(&mut self, pending: PendingBarrelSpray) {
        if self.spray_len == self.sprays.len() {
            return;
        }
        self.sprays[self.spray_len] = Some(pending);
        self.spray_len += 1;
    }

    fn sprays(&self) -> impl Iterator<Item = PendingBarrelSpray> + '_ {
        self.sprays[..self.spray_len].iter().flatten().copied()
    }
}
const ITEM_VISUAL_WAVE: [f32; 16] = [
    0.0,
    0.382_683_43,
    0.707_106_77,
    0.923_879_5,
    1.0,
    0.923_879_5,
    0.707_106_77,
    0.382_683_43,
    0.0,
    -0.382_683_43,
    -0.707_106_77,
    -0.923_879_5,
    -1.0,
    -0.923_879_5,
    -0.707_106_77,
    -0.382_683_43,
];

fn item_visual_wave(age: ElapsedTicks, authored_phase: f32) -> f32 {
    let phase_steps = (authored_phase.abs() * 8.0) as usize;
    ITEM_VISUAL_WAVE[(age.get() as usize + phase_steps) % ITEM_VISUAL_WAVE.len()]
}

fn pop_bomb_overlap_distance(flat_distance: f32, fighter_radius: f32) -> f32 {
    (flat_distance - fighter_radius).max(0.0)
}

fn pop_bomb_body_overlaps(flat_distance: f32, fighter_radius: f32) -> bool {
    pop_bomb_overlap_distance(flat_distance, fighter_radius) <= POP_BOMB_RADIUS
}

fn forced_item_drop_action(action: FighterAction) -> bool {
    visible_forced_item_drop_action(action) || hidden_forced_item_cleanup_action(action)
}

fn visible_forced_item_drop_action(action: FighterAction) -> bool {
    matches!(
        action,
        FighterAction::Knockdown | FighterAction::Grabbed | FighterAction::GuardBroken
    )
}

fn hidden_forced_item_cleanup_action(action: FighterAction) -> bool {
    matches!(action, FighterAction::RingOut | FighterAction::Respawning)
}

fn item_drop_sfx_cue(position: Vec3) -> CombatSfxCue {
    CombatSfxCue::new(CombatSfxKind::ItemDrop, position, ITEM_DROP_SFX_PRIORITY)
}

fn steamer_explosion_sfx_cue(position: Vec3) -> CombatSfxCue {
    CombatSfxCue::new(
        CombatSfxKind::SteamerExplosion,
        position,
        STEAMER_EXPLOSION_SFX_PRIORITY,
    )
}

fn item_use_sfx_cue(kind: ItemKind, position: Vec3) -> Option<CombatSfxCue> {
    match kind {
        ItemKind::Mushroom => Some(CombatSfxCue::new(
            CombatSfxKind::MushroomBigger,
            position,
            MUSHROOM_BIGGER_SFX_PRIORITY,
        )),
        _ => None,
    }
}

#[cfg(test)]
mod steamer_blast_overlap_tests {
    use super::*;

    #[test]
    fn pop_bomb_body_overlap_hits_when_body_touches_red_circle() {
        let fighter_radius = 0.5;
        assert!(pop_bomb_body_overlaps(
            POP_BOMB_RADIUS + fighter_radius,
            fighter_radius,
        ));
    }

    #[test]
    fn pop_bomb_body_overlap_rejects_when_body_is_outside_red_circle() {
        let fighter_radius = 0.5;
        assert!(!pop_bomb_body_overlaps(
            POP_BOMB_RADIUS + fighter_radius + 0.01,
            fighter_radius,
        ));
    }

    #[test]
    fn pop_bomb_falloff_distance_uses_body_edge_not_center() {
        let fighter_radius = 0.5;
        assert_eq!(
            pop_bomb_overlap_distance(POP_BOMB_RADIUS + fighter_radius, fighter_radius),
            POP_BOMB_RADIUS
        );
    }
}

#[derive(Clone, Copy)]
struct ItemDefinition {
    label: &'static str,
    role: ItemRole,
    portable: bool,
    loose_offset: f32,
    max_durability: i32,
    throw_speed: f32,
    throw_arc: f32,
    throw_lifetime: f32,
    throw_owner_grace: f32,
    pickup_lockout: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemRole {
    Recovery,
    Explosive,
    Utility,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    Crate,
    Steamer,
    Apple,
    WineWhite,
    Turkey,
    Barrel,
    CupCoffee,
    Mushroom,
}

/// Renderer-facing work paired with one authoritative item lifecycle event.
/// This sidecar is never captured in rollback snapshots or sent over the wire.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ItemPresentationKind {
    PickedUp {
        position: Vec3,
    },
    Thrown {
        position: Vec3,
    },
    Used {
        position: Vec3,
        announcement: &'static str,
    },
    Dropped {
        position: Vec3,
    },
    Broken {
        position: Vec3,
    },
    CrateOpened {
        position: Vec3,
    },
    AlcoholSprayed {
        position: Vec3,
        spiral_phase: f32,
        affected_fighters: FighterHitMask,
    },
    Exploded {
        position: Vec3,
        camera_shake: f32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ItemPresentationIntent {
    pub event_id: SimEventId,
    pub item: SimEntityId,
    pub item_kind: ItemKind,
    pub fighter: Option<FighterId>,
    pub fighter_name: Option<&'static str>,
    pub kind: ItemPresentationKind,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ItemPresentationIntentSlot {
    tick: SimTick,
    len: u16,
    occupied: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ItemPresentationIntentMetrics {
    pub recorded: u64,
    pub replaced: u64,
    pub rejected: u64,
    pub discarded: u64,
}

/// Fixed-capacity render-side item journal indexed by deterministic event ID.
///
/// The resource exists only in rendered clients. Headless worlds still emit
/// compact semantic events but do not allocate or retain any item VFX/audio
/// payloads. New work is rejected on invalid ordinals and modulo eviction is
/// deterministic, so render stalls cannot cause unbounded growth.
#[derive(Resource, Clone, Debug)]
pub struct ItemPresentationIntentJournal {
    slots: [ItemPresentationIntentSlot; SIM_EVENT_HISTORY_TICKS],
    intents: Box<[Option<ItemPresentationIntent>]>,
    len: usize,
    metrics: ItemPresentationIntentMetrics,
}

impl Default for ItemPresentationIntentJournal {
    fn default() -> Self {
        Self {
            slots: [ItemPresentationIntentSlot::default(); SIM_EVENT_HISTORY_TICKS],
            intents: vec![None; SIM_EVENT_HISTORY_TICKS * MAX_SIM_EVENTS_PER_TICK]
                .into_boxed_slice(),
            len: 0,
            metrics: ItemPresentationIntentMetrics::default(),
        }
    }
}

impl ItemPresentationIntentJournal {
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
    pub const fn metrics(&self) -> ItemPresentationIntentMetrics {
        self.metrics
    }

    pub fn record(&mut self, intent: ItemPresentationIntent) -> Result<(), EventEmitError> {
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
            *slot = ItemPresentationIntentSlot {
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

    pub fn get(&self, event_id: SimEventId) -> Option<ItemPresentationIntent> {
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
            self.slots[slot_index] = ItemPresentationIntentSlot::default();
            self.len = self.len.saturating_sub(usize::from(slot.len));
            self.metrics.discarded = self.metrics.discarded.saturating_add(u64::from(slot.len));
        }
    }
}

impl RollbackEventDiscard for ItemPresentationIntentJournal {
    fn discard_after(&mut self, retained_through: SimTick) {
        Self::discard_after(self, retained_through);
    }
}

impl ItemKind {
    pub fn label(self) -> &'static str {
        self.definition().label
    }

    pub fn role(self) -> ItemRole {
        self.definition().role
    }

    pub fn bot_pickup_priority(self) -> f32 {
        match self.role() {
            ItemRole::Recovery => 0.64,
            ItemRole::Explosive => 0.88,
            ItemRole::Utility => 0.7,
        }
    }

    fn definition(self) -> ItemDefinition {
        match self {
            ItemKind::Crate => ItemDefinition {
                label: "Mystery Crate",
                role: ItemRole::Utility,
                portable: true,
                loose_offset: 0.5,
                max_durability: 1,
                throw_speed: ITEM_STONE_CRATE_THROW_SPEED,
                throw_arc: ITEM_STONE_CRATE_THROW_ARC,
                throw_lifetime: ITEM_THROW_LIFETIME,
                throw_owner_grace: ITEM_MALLET_THROW_GRACE,
                pickup_lockout: ITEM_STONE_CRATE_PICKUP_LOCKOUT,
            },
            ItemKind::Steamer => ItemDefinition {
                label: "Steamer",
                role: ItemRole::Explosive,
                portable: true,
                loose_offset: 0.46,
                max_durability: 1,
                throw_speed: ITEM_BOMB_THROW_SPEED,
                throw_arc: ITEM_BOMB_THROW_ARC,
                throw_lifetime: ITEM_THROW_LIFETIME,
                throw_owner_grace: ITEM_BOMB_THROW_GRACE,
                pickup_lockout: ITEM_BOMB_PICKUP_LOCKOUT,
            },
            ItemKind::Apple => ItemDefinition {
                label: "Apple",
                role: ItemRole::Recovery,
                portable: true,
                loose_offset: 0.44,
                max_durability: 1,
                throw_speed: 8.0,
                throw_arc: 0.8,
                throw_lifetime: ITEM_THROW_LIFETIME,
                throw_owner_grace: ITEM_MALLET_THROW_GRACE,
                pickup_lockout: 0.28,
            },
            ItemKind::WineWhite => ItemDefinition {
                label: "White Wine",
                role: ItemRole::Recovery,
                portable: true,
                loose_offset: 0.48,
                max_durability: 1,
                throw_speed: 7.4,
                throw_arc: 0.9,
                throw_lifetime: ITEM_THROW_LIFETIME,
                throw_owner_grace: ITEM_BOMB_THROW_GRACE,
                pickup_lockout: 0.32,
            },
            ItemKind::Turkey => ItemDefinition {
                label: "Turkey",
                role: ItemRole::Recovery,
                portable: true,
                loose_offset: 0.5,
                max_durability: 3,
                throw_speed: 8.6,
                throw_arc: 0.9,
                throw_lifetime: ITEM_THROW_LIFETIME,
                throw_owner_grace: ITEM_MALLET_THROW_GRACE,
                pickup_lockout: 0.34,
            },
            ItemKind::Barrel => ItemDefinition {
                label: "Barrel",
                role: ItemRole::Recovery,
                portable: true,
                loose_offset: 0.56,
                max_durability: 3,
                throw_speed: ITEM_STONE_CRATE_THROW_SPEED,
                throw_arc: ITEM_STONE_CRATE_THROW_ARC,
                throw_lifetime: ITEM_THROW_LIFETIME,
                throw_owner_grace: ITEM_MALLET_THROW_GRACE,
                pickup_lockout: ITEM_STONE_CRATE_PICKUP_LOCKOUT,
            },
            ItemKind::CupCoffee => ItemDefinition {
                label: "Coffee",
                role: ItemRole::Utility,
                portable: true,
                loose_offset: 0.42,
                max_durability: 1,
                throw_speed: 7.2,
                throw_arc: 1.0,
                throw_lifetime: ITEM_THROW_LIFETIME,
                throw_owner_grace: ITEM_MALLET_THROW_GRACE,
                pickup_lockout: 0.32,
            },
            ItemKind::Mushroom => ItemDefinition {
                label: "Mushroom",
                role: ItemRole::Utility,
                portable: true,
                loose_offset: 0.46,
                max_durability: 1,
                throw_speed: 7.6,
                throw_arc: 1.1,
                throw_lifetime: ITEM_THROW_LIFETIME,
                throw_owner_grace: ITEM_MALLET_THROW_GRACE,
                pickup_lockout: 0.36,
            },
        }
    }

    fn is_portable(self) -> bool {
        self.definition().portable
    }

    fn loose_offset(self) -> f32 {
        self.definition().loose_offset
    }

    fn max_durability(self) -> i32 {
        self.definition().max_durability
    }

    fn throw_speed(self) -> f32 {
        self.definition().throw_speed
    }

    fn throw_arc(self) -> f32 {
        self.definition().throw_arc
    }

    fn throw_lifetime(self) -> f32 {
        self.definition().throw_lifetime
    }

    fn throw_owner_grace(self) -> f32 {
        self.definition().throw_owner_grace
    }

    fn pickup_lockout(self) -> f32 {
        self.definition().pickup_lockout
    }
}

#[derive(Clone, Copy)]
struct PendingItemPresentationIntent {
    item: SimEntityId,
    item_kind: ItemKind,
    fighter: Option<FighterId>,
    fighter_name: Option<&'static str>,
    event: ItemLifecycleEvent,
    kind: ItemPresentationKind,
}

fn emit_item_presentation_intent(
    sim_events: &mut TickEventBuffer,
    presentation_intents: Option<&mut ItemPresentationIntentJournal>,
    pending: PendingItemPresentationIntent,
) {
    let Ok(event_id) = sim_events.emit(
        SimEventSource::Entity(pending.item),
        SimEventKind::ItemLifecycle {
            item: pending.item,
            fighter: pending.fighter,
            event: pending.event,
        },
    ) else {
        return;
    };
    if let Some(presentation_intents) = presentation_intents {
        let _ = presentation_intents.record(ItemPresentationIntent {
            event_id,
            item: pending.item,
            item_kind: pending.item_kind,
            fighter: pending.fighter,
            fighter_name: pending.fighter_name,
            kind: pending.kind,
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ItemState {
    Loose,
    Held {
        holder: FighterId,
    },
    Thrown {
        owner: FighterId,
        lifetime: TickTimer,
        grace: TickTimer,
    },
    Armed {
        owner: FighterId,
        timer: TickTimer,
        grace: TickTimer,
    },
    Spraying {
        owner: FighterId,
        lifetime: TickTimer,
        spray_timer: TickTimer,
        spiral_phase: f32,
        spiral_radius: f32,
    },
    Rolling {
        lifetime: TickTimer,
    },
    Respawning,
}

#[derive(Component)]
pub struct ArenaItem {
    pub kind: ItemKind,
    pub state: ItemState,
    pub respawn_timer: TickTimer,
    pub durability: i32,
    pub max_durability: i32,
    pub pickup_lockout: TickTimer,
    /// The stable arena-crate item that produced this transient reward. Arena
    /// anchor items have no source. At most one live reward may reference a
    /// given source, and reward entities are released instead of respawning.
    crate_source: Option<SimEntityId>,
    /// Canonical gameplay pose. Render [`Transform`] values are derived from
    /// this field and must never feed back into item simulation or snapshots.
    pub position: Vec3,
    pub anchor: Vec3,
    pub velocity: Vec3,
    pub already_hit: FighterHitMask,
    pub base_y: f32,
    pub state_age: ElapsedTicks,
    phase: f32,
}

impl ArenaItem {
    pub fn new(kind: ItemKind, anchor: Vec3, phase: f32) -> Self {
        Self {
            kind,
            state: ItemState::Loose,
            respawn_timer: TickTimer::ZERO,
            durability: kind.max_durability(),
            max_durability: kind.max_durability(),
            pickup_lockout: TickTimer::ZERO,
            crate_source: None,
            position: anchor,
            anchor,
            velocity: Vec3::ZERO,
            already_hit: FighterHitMask::default(),
            base_y: anchor.y,
            state_age: ElapsedTicks::ZERO,
            phase,
        }
    }

    /// Rebuilds the authoritative item component after a dynamic-snapshot
    /// payload has passed its wire-format checks. Keeping `phase` private and
    /// accepting it only through this validating constructor prevents restore
    /// code from bypassing the same component invariants used by live items.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_snapshot_parts(
        kind: ItemKind,
        state: ItemState,
        respawn_timer: TickTimer,
        durability: i32,
        max_durability: i32,
        pickup_lockout: TickTimer,
        crate_source: Option<SimEntityId>,
        position: Vec3,
        anchor: Vec3,
        velocity: Vec3,
        already_hit: FighterHitMask,
        base_y: f32,
        state_age: ElapsedTicks,
        phase: f32,
    ) -> Option<Self> {
        let finite = position.is_finite()
            && anchor.is_finite()
            && velocity.is_finite()
            && base_y.is_finite()
            && phase.is_finite();
        let durability_valid =
            max_durability == kind.max_durability() && (0..=max_durability).contains(&durability);
        let crate_source_valid = crate_source
            .is_none_or(|source| source.kind() == SimEntityKind::Item && kind != ItemKind::Crate);
        let respawn_timer_valid = match state {
            ItemState::Respawning => respawn_timer.active(),
            _ => !respawn_timer.active(),
        };
        let state_valid = match state {
            ItemState::Loose => velocity == Vec3::ZERO && already_hit.is_empty(),
            ItemState::Held { .. } => {
                velocity == Vec3::ZERO
                    && already_hit.is_empty()
                    && !pickup_lockout.active()
                    && state_age == ElapsedTicks::ZERO
            }
            ItemState::Thrown {
                lifetime, grace, ..
            } => lifetime.active() && !lifetime.is_indefinite() && !grace.is_indefinite(),
            ItemState::Armed { timer, grace, .. } => {
                kind == ItemKind::Steamer
                    && timer.active()
                    && !timer.is_indefinite()
                    && !grace.is_indefinite()
            }
            ItemState::Spraying {
                lifetime,
                spray_timer,
                spiral_phase,
                spiral_radius,
                ..
            } => {
                kind == ItemKind::Barrel
                    && lifetime.active()
                    && !lifetime.is_indefinite()
                    && !spray_timer.is_indefinite()
                    && spiral_phase.is_finite()
                    && spiral_radius.is_finite()
                    && spiral_radius >= 0.0
            }
            ItemState::Rolling { lifetime } => {
                lifetime.active() && !lifetime.is_indefinite() && already_hit.is_empty()
            }
            ItemState::Respawning => {
                velocity == Vec3::ZERO
                    && already_hit.is_empty()
                    && !pickup_lockout.active()
                    && state_age == ElapsedTicks::ZERO
            }
        };

        (finite && durability_valid && crate_source_valid && respawn_timer_valid && state_valid)
            .then_some(Self {
                kind,
                state,
                respawn_timer,
                durability,
                max_durability,
                pickup_lockout,
                crate_source,
                position,
                anchor,
                velocity,
                already_hit,
                base_y,
                state_age,
                phase,
            })
    }

    pub(crate) fn new_crate_reward(
        kind: ItemKind,
        position: Vec3,
        phase: f32,
        crate_source: SimEntityId,
    ) -> Self {
        debug_assert_eq!(crate_source.kind(), SimEntityKind::Item);
        debug_assert_ne!(kind, ItemKind::Crate);
        let mut item = Self::new(kind, position, phase);
        item.crate_source = Some(crate_source);
        item
    }

    pub(crate) const fn crate_source(&self) -> Option<SimEntityId> {
        self.crate_source
    }

    const fn is_crate_reward(&self) -> bool {
        self.crate_source.is_some()
    }

    /// Snapshot-only read access for the authored bob phase. The phase is used
    /// only to reconstruct presentation, but is retained so rollback does not
    /// visibly jump a loose item to a different point in its cycle.
    pub(crate) fn snapshot_phase(&self) -> f32 {
        self.phase
    }

    /// Quantizes private/state-variant item floats at the shared TickEnd phase.
    /// Public vectors/scalars are handled by the caller. This method exists so
    /// the private bob phase and variant payloads cannot evade the same grid.
    pub(crate) fn canonicalize_snapshot_floats(&mut self) {
        self.phase = canonicalize_f32(self.phase, DEFAULT_F32_QUANTIZATION);
        if let ItemState::Spraying {
            spiral_phase,
            spiral_radius,
            ..
        } = &mut self.state
        {
            *spiral_phase = canonicalize_f32(*spiral_phase, DEFAULT_F32_QUANTIZATION);
            *spiral_radius = canonicalize_f32(*spiral_radius, DEFAULT_F32_QUANTIZATION);
        }
    }

    /// Quantizes the complete authoritative item pose/state without requiring
    /// a render [`Transform`]. Headless items intentionally do not carry one.
    pub(crate) fn canonicalize_authoritative_floats(&mut self) {
        let scalar = |value| canonicalize_f32(value, DEFAULT_F32_QUANTIZATION);
        let vector = |value: Vec3| Vec3::new(scalar(value.x), scalar(value.y), scalar(value.z));
        self.position = vector(self.position);
        self.anchor = vector(self.anchor);
        self.velocity = vector(self.velocity);
        self.base_y = scalar(self.base_y);
        self.canonicalize_snapshot_floats();
    }

    pub fn is_held_by(&self, holder: FighterId) -> bool {
        matches!(self.state, ItemState::Held { holder: active_holder } if active_holder == holder)
    }

    pub fn reset_for_match(&mut self) {
        self.state = ItemState::Loose;
        self.respawn_timer.clear();
        self.durability = self.max_durability;
        self.pickup_lockout.clear();
        self.position = self.anchor;
        self.velocity = Vec3::ZERO;
        self.already_hit.clear();
        self.base_y = self.anchor.y;
        self.state_age.reset();
    }

    pub fn retarget_for_anchor(&mut self, kind: ItemKind, anchor: Vec3, phase: f32) {
        self.kind = kind;
        self.crate_source = None;
        self.anchor = anchor;
        self.phase = phase;
        self.max_durability = kind.max_durability();
        self.reset_for_match();
    }

    #[cfg(test)]
    pub fn deactivate_for_match(&mut self) {
        self.state = ItemState::Respawning;
        self.respawn_timer = TickTimer::INDEFINITE;
        self.durability = self.max_durability;
        self.position = self.anchor;
        self.velocity = Vec3::ZERO;
        self.pickup_lockout.clear();
        self.already_hit.clear();
        self.base_y = self.anchor.y;
        self.state_age.reset();
    }

    #[allow(dead_code)]
    pub fn status_label(&self) -> String {
        match self.kind {
            ItemKind::Turkey | ItemKind::Barrel => {
                format!(
                    "{} {}/{}",
                    self.kind.label(),
                    self.durability.max(0),
                    self.max_durability
                )
            }
            _ => self.kind.label().to_string(),
        }
    }

    fn loose_pickup_ready(&self) -> bool {
        matches!(self.state, ItemState::Loose)
            && !self.pickup_lockout.active()
            && self.kind.is_portable()
    }

    pub fn pickup_as(&mut self, holder: FighterId) {
        self.state = ItemState::Held { holder };
        self.velocity = Vec3::ZERO;
        self.already_hit.clear();
        self.pickup_lockout.clear();
        self.state_age.reset();
    }

    pub fn launch_as_thrown(&mut self, owner: FighterId, velocity: Vec3) {
        self.velocity = velocity;
        self.already_hit.clear();
        self.state = ItemState::Thrown {
            owner,
            lifetime: TickTimer::from_seconds_ceil(self.kind.throw_lifetime()),
            grace: TickTimer::from_seconds_ceil(self.kind.throw_owner_grace()),
        };
        self.pickup_lockout = TickTimer::from_seconds_ceil(self.kind.pickup_lockout());
        self.state_age.reset();
    }

    pub fn arm_as_bomb(&mut self, owner: FighterId, velocity: Vec3) {
        self.velocity = velocity;
        self.already_hit.clear();
        self.state = ItemState::Armed {
            owner,
            timer: TickTimer::from_seconds_ceil(POP_BOMB_FUSE),
            grace: TickTimer::from_seconds_ceil(self.kind.throw_owner_grace()),
        };
        self.pickup_lockout = TickTimer::from_seconds_ceil(self.kind.pickup_lockout());
        self.state_age.reset();
    }

    pub fn start_barrel_spray(&mut self, owner: FighterId) {
        let planar_speed = canonical_math::vec2_length(Vec2::new(self.velocity.x, self.velocity.z));
        self.state = ItemState::Spraying {
            owner,
            lifetime: TickTimer::from_seconds_ceil(BARREL_SPRAY_DURATION),
            spray_timer: TickTimer::ZERO,
            spiral_phase: 0.0,
            spiral_radius: planar_speed.max(0.2),
        };
        self.velocity.y = 0.0;
        self.pickup_lockout = TickTimer::from_seconds_ceil(BARREL_SPRAY_DURATION);
        self.already_hit.clear();
        self.state_age.reset();
    }

    pub fn roll_loose(&mut self, velocity: Vec3) {
        self.velocity = velocity;
        self.already_hit.clear();
        self.pickup_lockout = TickTimer::from_seconds_ceil(ITEM_DROP_ROLL_PICKUP_LOCKOUT);
        self.state = ItemState::Rolling {
            lifetime: TickTimer::from_seconds_ceil(ITEM_DROP_ROLL_LIFETIME),
        };
        self.state_age.reset();
    }

    fn set_respawning(&mut self) {
        self.state = ItemState::Respawning;
        self.respawn_timer = TickTimer::from_seconds_ceil(ITEM_RESPAWN_SECONDS);
        self.velocity = Vec3::ZERO;
        self.pickup_lockout.clear();
        self.already_hit.clear();
        self.state_age.reset();
    }
}

#[derive(Resource, Default)]
pub struct ItemAssets {
    item_mesh: Handle<Mesh>,
    steamer_scene: Handle<Scene>,
    apple_scene: Handle<Scene>,
    wine_white_scene: Handle<Scene>,
    turkey_scene: Handle<Scene>,
    barrel_scene: Handle<Scene>,
    cup_coffee_scene: Handle<Scene>,
    mushroom_scene: Handle<Scene>,
    crate_scene: Handle<Scene>,
    steamer_material: Handle<StandardMaterial>,
    apple_material: Handle<StandardMaterial>,
    wine_white_material: Handle<StandardMaterial>,
    turkey_material: Handle<StandardMaterial>,
    barrel_material: Handle<StandardMaterial>,
    coffee_material: Handle<StandardMaterial>,
    mushroom_material: Handle<StandardMaterial>,
    crate_material: Handle<StandardMaterial>,
    live_bomb_material: Handle<StandardMaterial>,
}

impl ItemAssets {
    pub fn material_for(&self, kind: ItemKind, live_bomb: bool) -> Handle<StandardMaterial> {
        match kind {
            ItemKind::Crate => self.crate_material.clone(),
            ItemKind::Steamer if live_bomb => self.live_bomb_material.clone(),
            ItemKind::Steamer => self.steamer_material.clone(),
            ItemKind::Apple => self.apple_material.clone(),
            ItemKind::WineWhite => self.wine_white_material.clone(),
            ItemKind::Turkey => self.turkey_material.clone(),
            ItemKind::Barrel => self.barrel_material.clone(),
            ItemKind::CupCoffee => self.coffee_material.clone(),
            ItemKind::Mushroom => self.mushroom_material.clone(),
        }
    }

    pub fn scene_for(&self, kind: ItemKind) -> Handle<Scene> {
        match kind {
            ItemKind::Crate => self.crate_scene.clone(),
            ItemKind::Steamer => self.steamer_scene.clone(),
            ItemKind::Apple => self.apple_scene.clone(),
            ItemKind::WineWhite => self.wine_white_scene.clone(),
            ItemKind::Turkey => self.turkey_scene.clone(),
            ItemKind::Barrel => self.barrel_scene.clone(),
            ItemKind::CupCoffee => self.cup_coffee_scene.clone(),
            ItemKind::Mushroom => self.mushroom_scene.clone(),
        }
    }
}

pub fn setup_items(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut identities: ResMut<SimulationIdentityAllocator>,
    active_arena: Res<ActiveArena>,
) {
    let assets = ItemAssets {
        item_mesh: meshes.add(Cuboid::new(0.01, 0.01, 0.01)),
        steamer_scene: food_scene(&asset_server, "steamer.glb"),
        apple_scene: food_scene(&asset_server, "apple.glb"),
        wine_white_scene: food_scene(&asset_server, "wine-white.glb"),
        turkey_scene: food_scene(&asset_server, "turkey.glb"),
        barrel_scene: food_scene(&asset_server, "barrel.glb"),
        cup_coffee_scene: food_scene(&asset_server, "cup-coffee.glb"),
        mushroom_scene: food_scene(&asset_server, "mushroom.glb"),
        crate_scene: asset_server
            .load(GltfAssetLabel::Scene(0).from_asset("arena/kits/platformer/crate-strong.glb")),
        steamer_material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.72, 0.62, 0.52),
            perceptual_roughness: 0.46,
            ..default()
        }),
        apple_material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.95, 0.08, 0.04),
            perceptual_roughness: 0.44,
            ..default()
        }),
        wine_white_material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.94, 0.88, 0.58),
            emissive: LinearRgba::rgb(0.06, 0.05, 0.01),
            perceptual_roughness: 0.34,
            ..default()
        }),
        turkey_material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.8, 0.45, 0.22),
            perceptual_roughness: 0.52,
            ..default()
        }),
        barrel_material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.48, 0.28, 0.14),
            perceptual_roughness: 0.7,
            ..default()
        }),
        coffee_material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.94, 0.72, 0.48),
            emissive: LinearRgba::rgb(0.08, 0.04, 0.01),
            perceptual_roughness: 0.38,
            ..default()
        }),
        mushroom_material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.86, 0.24, 0.18),
            emissive: LinearRgba::rgb(0.08, 0.01, 0.01),
            perceptual_roughness: 0.42,
            ..default()
        }),
        crate_material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.54, 0.31, 0.12),
            perceptual_roughness: 0.78,
            ..default()
        }),
        live_bomb_material: materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.44, 0.08),
            emissive: LinearRgba::rgb(0.24, 0.08, 0.01),
            perceptual_roughness: 0.32,
            ..default()
        }),
    };

    for anchor in active_arena.definition().item_anchors {
        let _ = spawn_pickup(
            &mut commands,
            &mut identities,
            &assets,
            anchor.kind,
            anchor.position,
            anchor.phase,
            None,
        );
    }

    commands.insert_resource(assets);
}

fn food_scene(asset_server: &AssetServer, file: &str) -> Handle<Scene> {
    asset_server.load(GltfAssetLabel::Scene(0).from_asset(format!("food/kenney_food_kit/{file}")))
}

fn spawn_pickup(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    assets: &ItemAssets,
    kind: ItemKind,
    position: Vec3,
    phase: f32,
    crate_source: Option<SimEntityId>,
) -> Option<Entity> {
    let (mesh, material, scale) = item_visuals(assets, kind, false);
    let (entity, _) =
        spawn_canonical_pickup(commands, identities, kind, position, phase, crate_source)?;
    commands.entity(entity).insert((
        Mesh3d(mesh),
        MeshMaterial3d(material),
        SceneRoot(assets.scene_for(kind)),
        Transform::from_translation(position).with_scale(scale),
        Name::new(kind.label()),
    ));
    Some(entity)
}

/// Allocates only rollback-relevant item state. Render components are attached
/// later by the client-side projection system, never by FixedUpdate.
fn spawn_canonical_pickup(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    kind: ItemKind,
    position: Vec3,
    phase: f32,
    crate_source: Option<SimEntityId>,
) -> Option<(Entity, SimEntityId)> {
    let item = if let Some(source) = crate_source {
        ArenaItem::new_crate_reward(kind, position, phase, source)
    } else {
        ArenaItem::new(kind, position, phase)
    };
    let entity = commands.spawn_empty().id();
    let stable = match identities.try_allocate(SimEntityKind::Item, entity) {
        Ok(stable) => stable,
        Err(_) => {
            commands.entity(entity).despawn();
            return None;
        }
    };
    let id = stable.id();
    commands.entity(entity).insert((stable, item));
    Some((entity, id))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ItemArenaResetReport {
    pub retained_anchors: usize,
    pub spawned_anchors: usize,
    pub released_rewards: usize,
    pub released_excess: usize,
    pub rejected_anchors: usize,
}

/// Rebuilds only the rollback-relevant item anchors in a bare simulation
/// [`World`]. The resulting entities contain [`StableSimEntity`] and
/// [`ArenaItem`] only: render transforms, meshes, materials, scenes, names,
/// visibility, and asset resources remain the presentation world's concern.
///
/// Existing anchor entities are consumed in stable-ID order, transient crate
/// rewards and excess entities release their generational slots, and missing
/// anchors are allocated in authored order. This is the immediate-world
/// counterpart of [`reset_items_for_arena`] for headless authority bootstrap.
pub fn reset_canonical_items_for_arena(
    world: &mut World,
    arena: &ArenaDefinition,
) -> ItemArenaResetReport {
    world.init_resource::<SimulationIdentityAllocator>();

    let mut ordered = {
        let identities = world.resource::<SimulationIdentityAllocator>();
        (0..identities.capacity(SimEntityKind::Item))
            .filter_map(|index| identities.entry_at(SimEntityKind::Item, index))
            .collect::<Vec<_>>()
    };
    ordered.sort_unstable_by_key(|(id, _)| *id);

    let mut report = ItemArenaResetReport::default();
    let mut next_anchor = 0;
    for (stable_id, entity) in ordered {
        let stable_matches = world
            .get::<StableSimEntity>(entity)
            .is_some_and(|stable| stable.id() == stable_id);
        let Some(item) = world.get::<ArenaItem>(entity) else {
            release_canonical_item(world, entity, stable_id);
            report.released_excess += 1;
            continue;
        };
        let is_reward = item.is_crate_reward();
        if !stable_matches {
            release_canonical_item(world, entity, stable_id);
            report.released_excess += 1;
            continue;
        }
        if is_reward {
            release_canonical_item(world, entity, stable_id);
            report.released_rewards += 1;
            continue;
        }

        let Some(anchor) = arena.item_anchors.get(next_anchor) else {
            release_canonical_item(world, entity, stable_id);
            report.released_excess += 1;
            continue;
        };
        let mut item = world
            .get_mut::<ArenaItem>(entity)
            .expect("validated item entity must remain live");
        item.retarget_for_anchor(anchor.kind, anchor.position, anchor.phase);
        item.canonicalize_authoritative_floats();
        next_anchor += 1;
        report.retained_anchors += 1;
    }

    for anchor in &arena.item_anchors[next_anchor..] {
        let entity = world.spawn_empty().id();
        let allocation = world
            .resource_mut::<SimulationIdentityAllocator>()
            .try_allocate(SimEntityKind::Item, entity);
        match allocation {
            Ok(stable) => {
                let mut item = ArenaItem::new(anchor.kind, anchor.position, anchor.phase);
                item.canonicalize_authoritative_floats();
                world.entity_mut(entity).insert((stable, item));
                report.spawned_anchors += 1;
            }
            Err(_) => {
                let despawned = world.despawn(entity);
                debug_assert!(despawned);
                report.rejected_anchors += 1;
            }
        }
    }

    report
}

fn release_canonical_item(world: &mut World, entity: Entity, id: SimEntityId) {
    let released = world
        .resource_mut::<SimulationIdentityAllocator>()
        .release(entity, StableSimEntity::new(id));
    debug_assert!(released, "canonical item must release exactly once");
    let despawned = world.despawn(entity);
    debug_assert!(despawned, "released canonical item must still be live");
}

/// Rebuilds the arena item set in stable-ID order at a match/setup reset.
/// Transient crate rewards and anchor entities beyond the selected arena's
/// authored count release their generational slots. Missing anchors are then
/// allocated in authored order, so every peer reaches the same pool layout.
pub(crate) fn reset_items_for_arena(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    items: &mut Query<(Entity, &StableSimEntity, &mut ArenaItem)>,
    assets: &ItemAssets,
    arena: &ArenaDefinition,
) -> ItemArenaResetReport {
    let mut ordered = (0..identities.capacity(SimEntityKind::Item))
        .filter_map(|index| identities.entry_at(SimEntityKind::Item, index))
        .collect::<Vec<_>>();
    ordered.sort_unstable_by_key(|(id, _)| *id);

    let mut report = ItemArenaResetReport::default();
    let mut next_anchor = 0;
    for (stable_id, entity) in ordered {
        let Ok((_, stable, mut item)) = items.get_mut(entity) else {
            despawn_stable(
                commands,
                identities,
                entity,
                StableSimEntity::new(stable_id),
            );
            report.released_excess += 1;
            continue;
        };
        if stable.id() != stable_id {
            drop(item);
            despawn_stable(
                commands,
                identities,
                entity,
                StableSimEntity::new(stable_id),
            );
            report.released_excess += 1;
            continue;
        }
        if item.is_crate_reward() {
            let stable = *stable;
            drop(item);
            despawn_stable(commands, identities, entity, stable);
            report.released_rewards += 1;
            continue;
        }

        let Some(anchor) = arena.item_anchors.get(next_anchor) else {
            let stable = *stable;
            drop(item);
            despawn_stable(commands, identities, entity, stable);
            report.released_excess += 1;
            continue;
        };
        item.retarget_for_anchor(anchor.kind, anchor.position, anchor.phase);
        let (mesh, material, scale) = item_visuals(assets, anchor.kind, false);
        commands.entity(entity).insert((
            Mesh3d(mesh),
            MeshMaterial3d(material),
            SceneRoot(assets.scene_for(anchor.kind)),
            Transform::from_translation(anchor.position).with_scale(scale),
            Visibility::Visible,
            Name::new(anchor.kind.label()),
        ));
        next_anchor += 1;
        report.retained_anchors += 1;
    }

    for anchor in &arena.item_anchors[next_anchor..] {
        if spawn_pickup(
            commands,
            identities,
            assets,
            anchor.kind,
            anchor.position,
            anchor.phase,
            None,
        )
        .is_some()
        {
            report.spawned_anchors += 1;
        } else {
            report.rejected_anchors += 1;
        }
    }
    report
}

fn item_visuals(
    assets: &ItemAssets,
    kind: ItemKind,
    live_bomb: bool,
) -> (Handle<Mesh>, Handle<StandardMaterial>, Vec3) {
    match kind {
        ItemKind::Crate
        | ItemKind::Steamer
        | ItemKind::Apple
        | ItemKind::WineWhite
        | ItemKind::Turkey
        | ItemKind::Barrel
        | ItemKind::CupCoffee
        | ItemKind::Mushroom => (
            assets.item_mesh.clone(),
            assets.material_for(kind, live_bomb),
            item_scale(kind),
        ),
    }
}

pub fn handle_item_inputs(
    hitstop: Res<Hitstop>,
    identities: Res<SimulationIdentityAllocator>,
    active_arena: Res<ActiveArena>,
    mut sim_events: ResMut<TickEventBuffer>,
    mut presentation_intents: Option<ResMut<ItemPresentationIntentJournal>>,
    mut fighters: Query<
        (
            Entity,
            &Fighter,
            &mut FighterInput,
            &mut FighterMotor,
            &mut FighterStats,
            &mut FighterInventory,
            &mut FighterActionState,
            &SimPosition,
        ),
        Without<ArenaItem>,
    >,
    mut items: Query<(Entity, &StableSimEntity, &mut ArenaItem)>,
) {
    if hitstop.active() {
        return;
    }

    for fighter_id in FighterId::ALL {
        let Some((
            _fighter_entity,
            fighter,
            mut input,
            mut motor,
            mut stats,
            mut inventory,
            mut action,
            fighter_transform,
        )) = fighters
            .iter_mut()
            .find(|(_, fighter, ..)| fighter.id == fighter_id.index())
        else {
            continue;
        };
        if !can_use_item_input(action.action) {
            continue;
        }

        if let Some(held_item) = inventory.held {
            let Some(held_entity) = identities.mapped_entity(held_item) else {
                inventory.held = None;
                continue;
            };
            let Ok((_, _, mut item)) = items.get_mut(held_entity) else {
                inventory.held = None;
                continue;
            };
            if held_reference_is_stale(Some(&*item), fighter_id) {
                inventory.held = None;
                continue;
            }

            let command = held_item_command(&input, item.kind);
            sanitize_held_item_inputs(&mut input);

            let facing = canonical_math::vec3_normalize_or_zero(motor.facing);
            item.position = fighter_transform.translation + Vec3::Y * 0.95;
            match command {
                HeldItemCommand::Throw => {
                    cancel_dash_slide_for_action(&mut motor);
                    let throw_velocity = facing * item.kind.throw_speed()
                        + motor.velocity * 0.45
                        + Vec3::Y * item.kind.throw_arc();
                    throw_item(
                        fighter_id,
                        &mut item,
                        active_arena.definition(),
                        fighter_transform.translation + Vec3::Y * 0.82 + facing * 0.65,
                        throw_velocity,
                    );
                    inventory.held = None;
                    set_item_action(&mut action, FighterAction::ItemThrow);
                    input.light = false;
                    input.heavy = false;
                    emit_item_presentation_intent(
                        &mut sim_events,
                        presentation_intents.as_deref_mut(),
                        PendingItemPresentationIntent {
                            item: held_item,
                            item_kind: item.kind,
                            fighter: Some(fighter_id),
                            fighter_name: Some(fighter.name),
                            event: ItemLifecycleEvent::Thrown,
                            kind: ItemPresentationKind::Thrown {
                                position: item.position,
                            },
                        },
                    );
                    continue;
                }
                HeldItemCommand::Use => {
                    cancel_dash_slide_for_action(&mut motor);
                    if let Some(announcement) = use_held_item(&mut stats, &mut item) {
                        emit_item_presentation_intent(
                            &mut sim_events,
                            presentation_intents.as_deref_mut(),
                            PendingItemPresentationIntent {
                                item: held_item,
                                item_kind: item.kind,
                                fighter: Some(fighter_id),
                                fighter_name: Some(fighter.name),
                                event: ItemLifecycleEvent::Used,
                                kind: ItemPresentationKind::Used {
                                    position: fighter_transform.translation + Vec3::Y * 1.05,
                                    announcement,
                                },
                            },
                        );
                        item.durability -= 1;
                        if item.durability <= 0 {
                            item.set_respawning();
                            inventory.held = None;
                        }
                        set_item_action(&mut action, FighterAction::ItemSwing);
                        input.light = false;
                    } else {
                        set_item_action(&mut action, FighterAction::ItemSwing);
                        input.light = false;
                    }
                    continue;
                }
                HeldItemCommand::None => {}
            }

            continue;
        }

        if !input.light || !motor.grounded || portable_pickup_blocked(action.action, &motor) {
            continue;
        }

        let Some((item_entity, item_id)) = nearest_portable_item(
            fighter_transform.translation,
            motor.facing,
            stats.item_size_multiplier(),
            &mut items,
        ) else {
            continue;
        };
        let Ok((_, _, mut item)) = items.get_mut(item_entity) else {
            continue;
        };
        item.pickup_as(fighter_id);
        item.position = fighter_transform.translation + Vec3::Y * 0.95;
        inventory.held = Some(item_id);
        cancel_dash_slide_for_action(&mut motor);
        set_item_action(&mut action, FighterAction::ItemPickup);
        input.light = false;
        emit_item_presentation_intent(
            &mut sim_events,
            presentation_intents.as_deref_mut(),
            PendingItemPresentationIntent {
                item: item_id,
                item_kind: item.kind,
                fighter: Some(fighter_id),
                fighter_name: Some(fighter.name),
                event: ItemLifecycleEvent::PickedUp,
                kind: ItemPresentationKind::PickedUp {
                    position: item.position,
                },
            },
        );
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum HeldItemCommand {
    Use,
    Throw,
    None,
}

fn held_item_command(input: &FighterInput, kind: ItemKind) -> HeldItemCommand {
    if matches!(kind, ItemKind::Steamer | ItemKind::Crate) {
        if input.heavy || input.light {
            return HeldItemCommand::Throw;
        }
        return HeldItemCommand::None;
    }

    if input.heavy {
        return HeldItemCommand::Throw;
    }

    if input.light {
        return HeldItemCommand::Use;
    }

    HeldItemCommand::None
}

fn use_held_item(stats: &mut FighterStats, item: &mut ArenaItem) -> Option<&'static str> {
    let message = match item.kind {
        ItemKind::Crate => return None,
        ItemKind::Apple => {
            stats.health = (stats.health + ITEM_APPLE_HEALTH).min(MAX_HEALTH);
            "ate Apple"
        }
        ItemKind::WineWhite => {
            stats.stamina = (stats.stamina + ITEM_WINE_WHITE_STAMINA).min(MAX_STAMINA);
            "drank White Wine"
        }
        ItemKind::Turkey => {
            stats.health = (stats.health + ITEM_TURKEY_HEALTH).min(MAX_HEALTH);
            "ate Turkey"
        }
        ItemKind::Barrel => {
            stats.stamina = (stats.stamina + ITEM_BARREL_STAMINA).min(MAX_STAMINA);
            "drank Barrel"
        }
        ItemKind::CupCoffee => {
            stats.item_speed_timer = TickTimer::from_seconds_ceil(ITEM_COFFEE_SPEED_SECONDS);
            "drank Coffee"
        }
        ItemKind::Mushroom => {
            stats.item_giant_timer = TickTimer::from_seconds_ceil(ITEM_MUSHROOM_GIANT_SECONDS);
            "ate Mushroom"
        }
        ItemKind::Steamer => return None,
    };
    Some(message)
}

fn sanitize_held_item_inputs(input: &mut FighterInput) {
    input.grab = false;
    input.ultimate = false;
    input.special = false;
    input.light = false;
    input.light_held = false;
    input.raw_light_pressed = false;
    input.heavy_held = false;
    input.raw_heavy_pressed = false;
    input.heavy_released = false;
    input.heavy = false;
}

fn can_use_item_input(action: FighterAction) -> bool {
    matches!(
        action,
        FighterAction::Idle
            | FighterAction::Moving
            | FighterAction::Jumping
            | FighterAction::Dashing
            | FighterAction::Guarding
    )
}

fn portable_pickup_blocked(action: FighterAction, motor: &FighterMotor) -> bool {
    action == FighterAction::Dashing || motor.dash_slide_timer.active()
}

fn set_item_action(action: &mut FighterActionState, next: FighterAction) {
    action.action = next;
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
}

fn nearest_portable_item(
    fighter_pos: Vec3,
    facing: Vec3,
    fighter_size: f32,
    items: &mut Query<(Entity, &StableSimEntity, &mut ArenaItem)>,
) -> Option<(Entity, SimEntityId)> {
    let mut best: Option<(Entity, SimEntityId, f32)> = None;
    let facing = canonical_math::vec3_normalize_or_zero(facing);
    for (entity, stable, item) in items.iter_mut() {
        if !item.loose_pickup_ready() {
            continue;
        }
        let delta = item.position - fighter_pos;
        let flat = Vec2::new(delta.x, delta.z);
        let distance_squared = canonical_math::vec2_length_squared(flat);
        let contact_range = FIGHTER_RADIUS * fighter_size + item_pickup_radius(item.kind);
        let pickup_range = ITEM_PICKUP_RANGE.max(contact_range);
        debug_assert!(contact_range >= 0.0 && pickup_range >= 0.0);
        if distance_squared > pickup_range * pickup_range {
            continue;
        }
        let dir = canonical_math::vec3_normalize_or_zero(Vec3::new(delta.x, 0.0, delta.z));
        if distance_squared > contact_range * contact_range
            && facing.dot(dir) < ITEM_PICKUP_CONE_DOT
        {
            continue;
        }
        if best.is_none_or(|(_, best_id, best_dist)| {
            distance_squared < best_dist || (distance_squared == best_dist && stable.id() < best_id)
        }) {
            best = Some((entity, stable.id(), distance_squared));
        }
    }
    best.map(|(entity, stable, _)| (entity, stable))
}

fn item_pickup_radius(kind: ItemKind) -> f32 {
    item_scale(kind).x.max(item_scale(kind).z) * 0.34
}

fn throw_item(
    owner: FighterId,
    item: &mut ArenaItem,
    arena: &ArenaDefinition,
    position: Vec3,
    velocity: Vec3,
) {
    let position = if canonical_math::vec3_length_squared(velocity) < 0.01 {
        let ground = item_ground_height(arena, position.x, position.z).unwrap_or(ARENA_TOP_Y);
        Vec3::new(position.x, ground + item.kind.loose_offset(), position.z)
    } else {
        position
    };
    item.position = position;
    if item.kind == ItemKind::Steamer {
        arm_bomb(item, position, velocity, owner, arena);
    } else {
        item.launch_as_thrown(owner, velocity);
    }
}

fn arm_bomb(
    item: &mut ArenaItem,
    position: Vec3,
    velocity: Vec3,
    owner: FighterId,
    arena: &ArenaDefinition,
) {
    let position = if canonical_math::vec3_length_squared(velocity) < 0.01 {
        let ground = item_ground_height(arena, position.x, position.z).unwrap_or(ARENA_TOP_Y);
        Vec3::new(position.x, ground + item.kind.loose_offset(), position.z)
    } else {
        position
    };
    item.position = position;
    item.arm_as_bomb(owner, velocity);
}

fn place_loose(item: &mut ArenaItem, position: Vec3, arena: &ArenaDefinition) {
    let ground = item_ground_height(arena, position.x, position.z).unwrap_or(ARENA_TOP_Y);
    item.position = Vec3::new(position.x, ground + item.kind.loose_offset(), position.z);
    item.base_y = item.position.y;
    item.velocity = Vec3::ZERO;
    item.pickup_lockout = TickTimer::from_seconds_ceil(item.kind.pickup_lockout());
    item.already_hit.clear();
    item.state = ItemState::Loose;
    item.state_age.reset();
}

fn begin_barrel_spray(item: &mut ArenaItem, owner: FighterId, arena: &ArenaDefinition) {
    if let Some(ground_y) = item_ground_height(arena, item.position.x, item.position.z) {
        item.position.y = ground_y + item.kind.loose_offset();
    }
    item.start_barrel_spray(owner);
}

fn advance_barrel_spray_timer(timer: &mut TickTimer) -> bool {
    let due = !timer.active() || timer.tick();
    if due {
        timer.set(TickTimer::from_seconds_ceil(BARREL_SPRAY_CADENCE));
    }
    due
}

fn item_ground_height(arena: &ArenaDefinition, x: f32, z: f32) -> Option<f32> {
    ground_support_for_arena_with_radius(arena, x, z, 0.0).height()
}

fn integrate_item_position(item: &mut ArenaItem, dt: f32) {
    let displacement = item.velocity * dt;
    item.position += displacement;
}

fn thrown_item_overlaps_fighter(
    item_position: Vec3,
    hurt_center: Vec3,
    fighter_radius: f32,
) -> bool {
    let combined_radius = ITEM_THROW_RADIUS + fighter_radius;
    debug_assert!(combined_radius >= 0.0);
    canonical_math::vec3_distance_squared(hurt_center, item_position)
        <= combined_radius * combined_radius
}

pub fn spawn_item_hitboxes(
    mut commands: Commands,
    mut identities: ResMut<SimulationIdentityAllocator>,
    hitstop: Res<Hitstop>,
    mut fighters: Query<
        (
            &Fighter,
            &FighterMotor,
            &FighterInventory,
            &mut FighterActionState,
            &SimPosition,
        ),
        Without<ArenaItem>,
    >,
    mut items: Query<&mut ArenaItem>,
) {
    if hitstop.active() {
        return;
    }

    for stable_owner in FighterId::ALL {
        let Some((_fighter, motor, inventory, mut action, transform)) = fighters
            .iter_mut()
            .find(|(fighter, ..)| fighter.id == stable_owner.index())
        else {
            continue;
        };
        if action.action != FighterAction::ItemSwing
            || action.hitbox_spawned
            || action.elapsed.as_seconds() < ITEM_SWING_STARTUP
        {
            continue;
        }

        action.hitbox_spawned = true;
        let Some(item_id) = inventory.held else {
            continue;
        };
        let Some(item_entity) = identities.mapped_entity(item_id) else {
            continue;
        };
        let Ok(mut item) = items.get_mut(item_entity) else {
            continue;
        };
        let Some(config) = item_swing_config(item.kind) else {
            continue;
        };

        item.durability -= 1;
        let facing = canonical_math::vec3_normalize_or_zero(motor.facing);
        let shape = attack_shape_definition(AttackShapeId::ItemMelee);
        let center =
            transform.translation + Vec3::Y * (FIGHTER_HEIGHT * 0.62) + facing * config.range;
        let _ = try_spawn_stable(
            &mut commands,
            &mut identities,
            SimEntityKind::Hitbox,
            (
                Hitbox {
                    owner: stable_owner,
                    kind: AttackKind::ItemSwing,
                    payload_id: None,
                    attacker_character: None,
                    technique_id: None,
                    hit_effect: None,
                    shape_id: AttackShapeId::ItemMelee,
                    reaction_family: ReactionFamilyId::GroundedDownGetup,
                    damage_profile: DamageProfileId::ItemHeavy,
                    element: DamageElement::Earth,
                    attacker_equipment: None,
                    attacker_style: None,
                    power: config.damage,
                    str_scale: 1.0,
                    damage: config.damage,
                    knockback: config.knockback,
                    vertical_knockback: 1.2,
                    guardable: true,
                    base_radius: config.radius,
                    radius: config.radius,
                    lifetime: TickTimer::from_seconds_ceil(ITEM_SWING_ACTIVE),
                    elapsed: ElapsedTicks::ZERO,
                    total_lifetime: seconds_to_ticks_ceil(ITEM_SWING_ACTIVE),
                    spawn_origin: transform.translation,
                    facing,
                    base_range: config.range,
                    range: config.range,
                    scales_with_owner_size: false,
                    vertical_offset_scale: shape.vertical_offset_scale,
                    parented: shape.parented,
                    path: shape.path,
                    expires_on_owner_landing: false,
                    landing_linger: TickTimer::ZERO,
                    landing_linger_started: false,
                    ground_path_end: false,
                    ground_path_clearance: 0.0,
                    impact_cue: "impact_item_swing",
                    hitstop_scale: 1.1,
                    shake_scale: 1.1,
                    feedback_priority_bonus: 4,
                    already_hit: FighterHitMask::default(),
                },
                SimPosition::new(center),
            ),
        );
    }
}

struct ItemSwingConfig {
    damage: f32,
    knockback: f32,
    range: f32,
    radius: f32,
}

fn item_swing_config(kind: ItemKind) -> Option<ItemSwingConfig> {
    match kind {
        ItemKind::Steamer
        | ItemKind::Crate
        | ItemKind::Apple
        | ItemKind::WineWhite
        | ItemKind::Turkey
        | ItemKind::Barrel
        | ItemKind::CupCoffee
        | ItemKind::Mushroom => None,
    }
}

pub fn update_items(
    mut commands: Commands,
    mut identities: ResMut<SimulationIdentityAllocator>,
    mut items: Query<(Entity, &StableSimEntity, &mut ArenaItem)>,
) {
    let mut crate_sources = FixedItemIdSet::default();
    for index in 0..identities.capacity(SimEntityKind::Item) {
        let Some((stable_id, entity)) = identities.entry_at(SimEntityKind::Item, index) else {
            continue;
        };
        let Ok((_, stable, item)) = items.get_mut(entity) else {
            continue;
        };
        debug_assert_eq!(stable.id(), stable_id);
        if !item.is_crate_reward() && item.kind == ItemKind::Crate {
            let inserted = crate_sources.insert(stable_id);
            debug_assert!(inserted, "stable item IDs are unique");
        }
    }

    let mut retained_rewards = FixedItemIdSet::default();
    for index in 0..identities.capacity(SimEntityKind::Item) {
        let Some((stable_id, entity)) = identities.entry_at(SimEntityKind::Item, index) else {
            continue;
        };
        let Ok((_, stable, mut item)) = items.get_mut(entity) else {
            continue;
        };
        debug_assert_eq!(stable.id(), stable_id);

        if let Some(source) = item.crate_source() {
            if matches!(item.state, ItemState::Respawning) {
                despawn_stable(&mut commands, &mut identities, entity, *stable);
                continue;
            }
            let valid_unique_source =
                crate_sources.contains(source) && retained_rewards.insert(source);
            if !valid_unique_source {
                despawn_stable(&mut commands, &mut identities, entity, *stable);
            }
            continue;
        }

        match item.state {
            ItemState::Respawning => {
                if item.respawn_timer.tick() {
                    item.reset_for_match();
                }
            }
            ItemState::Loose => {
                item.pickup_lockout.tick();
                item.state_age.advance();
            }
            _ => {}
        }
    }
}

pub fn drop_items_from_disabled_fighters(
    identities: Res<SimulationIdentityAllocator>,
    active_arena: Res<ActiveArena>,
    tick: Res<SimTick>,
    state: Res<MatchState>,
    mut sim_events: ResMut<TickEventBuffer>,
    mut presentation_intents: Option<ResMut<ItemPresentationIntentJournal>>,
    mut fighters: Query<
        (
            &Fighter,
            &mut FighterInventory,
            &FighterActionState,
            &FighterMotor,
            &SimPosition,
        ),
        Without<ArenaItem>,
    >,
    mut items: Query<(&StableSimEntity, &mut ArenaItem), Without<Fighter>>,
) {
    for fighter_id in FighterId::ALL {
        let Some((_fighter, mut inventory, action, motor, fighter_transform)) = fighters
            .iter_mut()
            .find(|(fighter, ..)| fighter.id == fighter_id.index())
        else {
            continue;
        };
        let Some(item_id) = inventory.held else {
            continue;
        };
        let Some(item_entity) = identities.mapped_entity(item_id) else {
            inventory.held = None;
            continue;
        };
        if action.action != FighterAction::ItemSwing {
            if let Ok((_, mut item)) = items.get_mut(item_entity) {
                if held_reference_is_stale(Some(&*item), fighter_id) {
                    inventory.held = None;
                    continue;
                }
                if item.durability <= 0 {
                    inventory.held = None;
                    item.set_respawning();
                    continue;
                }
            } else {
                inventory.held = None;
                continue;
            }
        }
        if !forced_item_drop_action(action.action) {
            continue;
        }
        let Ok((stable, mut item)) = items.get_mut(item_entity) else {
            inventory.held = None;
            continue;
        };
        if held_reference_is_stale(Some(&*item), fighter_id) {
            inventory.held = None;
            continue;
        }
        inventory.held = None;
        if hidden_forced_item_cleanup_action(action.action) {
            item.set_respawning();
        } else {
            let facing = canonical_math::vec3_normalize_or_zero(motor.facing);
            place_rolling(
                &mut item,
                fighter_transform.translation + facing * 0.45,
                dropped_item_roll_velocity(
                    facing,
                    state.replay_seed,
                    *tick,
                    stable.id(),
                    fighter_id,
                ),
                active_arena.definition(),
            );
            emit_item_presentation_intent(
                &mut sim_events,
                presentation_intents.as_deref_mut(),
                PendingItemPresentationIntent {
                    item: stable.id(),
                    item_kind: item.kind,
                    fighter: Some(fighter_id),
                    fighter_name: None,
                    event: ItemLifecycleEvent::Dropped,
                    kind: ItemPresentationKind::Dropped {
                        position: item.position,
                    },
                },
            );
        }
    }
}

fn place_rolling(item: &mut ArenaItem, position: Vec3, velocity: Vec3, arena: &ArenaDefinition) {
    let ground = item_ground_height(arena, position.x, position.z).unwrap_or(ARENA_TOP_Y);
    item.position = Vec3::new(position.x, ground + item.kind.loose_offset(), position.z);
    item.base_y = item.position.y;
    item.roll_loose(velocity);
}

fn dropped_item_roll_velocity(
    facing: Vec3,
    master_seed: u64,
    tick: SimTick,
    item: SimEntityId,
    fighter: FighterId,
) -> Vec3 {
    const DIRECTIONS: [Vec2; 8] = [
        Vec2::new(1.0, 0.0),
        Vec2::new(0.707_106_77, 0.707_106_77),
        Vec2::new(0.0, 1.0),
        Vec2::new(-0.707_106_77, 0.707_106_77),
        Vec2::new(-1.0, 0.0),
        Vec2::new(-0.707_106_77, -0.707_106_77),
        Vec2::new(0.0, -1.0),
        Vec2::new(0.707_106_77, -0.707_106_77),
    ];
    let mut rng = item_event_rng(master_seed, tick, item, fighter, "items/drop");
    let local = DIRECTIONS[rng.gen_range_u32(0..DIRECTIONS.len() as u32).unwrap() as usize];
    let speed_step = rng.gen_range_u32(0..23).unwrap() as f32;
    let forward = canonical_math::vec3_normalize_or(Vec3::new(facing.x, 0.0, facing.z), Vec3::X);
    let right = Vec3::new(forward.z, 0.0, -forward.x);
    let direction = canonical_math::vec3_normalize_or(forward * local.x + right * local.y, Vec3::X);
    direction * (2.0 + speed_step * 0.1) + Vec3::Y * 1.15
}

pub fn advance_moving_items_and_collect_contacts(
    identities: Res<SimulationIdentityAllocator>,
    tick: Res<SimTick>,
    active_arena: Res<ActiveArena>,
    state: Res<MatchState>,
    feel: Res<CombatFeelTuning>,
    hitstop: Res<Hitstop>,
    mut contact_frame: ResMut<ItemContactFrame>,
    mut contact_buffer: ResMut<ContactBuffer>,
    mut items: Query<(&StableSimEntity, &mut ArenaItem)>,
    fighters: Query<
        (&Fighter, &FighterStats, &FighterActionState, &SimPosition),
        Without<ArenaItem>,
    >,
) {
    if hitstop.active() {
        contact_frame.tick = None;
        return;
    }
    contact_frame.begin_tick(*tick);

    let dt = ITEM_FIXED_DELTA;
    let arena = active_arena.definition();
    let item_capacity = identities.capacity(SimEntityKind::Item);
    for item_index in 0..item_capacity {
        let Some((stable_id, item_entity)) = identities.entry_at(SimEntityKind::Item, item_index)
        else {
            continue;
        };
        let Ok((stable, mut item)) = items.get_mut(item_entity) else {
            continue;
        };
        debug_assert_eq!(stable.id(), stable_id);
        match item.state {
            ItemState::Thrown {
                owner,
                mut lifetime,
                mut grace,
            } => {
                lifetime.tick();
                grace.tick();
                item.state_age.advance();
                item.velocity.y -= GRAVITY * dt;
                integrate_item_position(&mut item, dt);

                let impact = item_throw_profile_with_feel(item.kind, owner.index(), &feel);
                for target_id in FighterId::ALL {
                    let Some((target, stats, action, target_transform)) = fighters
                        .iter()
                        .find(|(fighter, ..)| fighter.id == target_id.index())
                    else {
                        continue;
                    };
                    if target_id == owner && grace.active() {
                        continue;
                    }
                    if !state.combat_target_allowed_for_state(owner.index(), target.id) {
                        continue;
                    }
                    if item.already_hit.contains(target_id) || !can_receive_impact(&stats, &action)
                    {
                        continue;
                    }
                    let hurt_center =
                        target_transform.translation + Vec3::Y * (FIGHTER_HEIGHT * 0.58);
                    if !thrown_item_overlaps_fighter(
                        item.position,
                        hurt_center,
                        FIGHTER_RADIUS * stats.item_size_multiplier(),
                    ) {
                        continue;
                    }
                    let _ = contact_buffer.push(ContactRecord::new(
                        ContactPhase::Strike,
                        ContactSourceKind::ItemMeleeOrThrow,
                        stable_id,
                        Some(owner),
                        target_id,
                        impact.payload_id.map_or(u16::MAX, |payload| payload as u16),
                        AttackShapeId::ItemLob as u16,
                        0,
                        item.position,
                        item.position,
                        impact,
                        ContactFlags::default(),
                    ));
                }

                item.state = ItemState::Thrown {
                    owner,
                    lifetime,
                    grace,
                };
            }
            ItemState::Rolling { mut lifetime } => {
                let lifetime_expired = lifetime.tick();
                item.pickup_lockout.tick();
                item.state_age.advance();
                item.velocity.y -= GRAVITY * dt;
                integrate_item_position(&mut item, dt);

                if should_respawn_item(item.position, arena) {
                    item.set_respawning();
                    continue;
                }

                if let Some(ground_y) = item_ground_height(arena, item.position.x, item.position.z)
                {
                    if item.position.y <= ground_y + item.kind.loose_offset()
                        && item.velocity.y <= 0.0
                    {
                        item.position.y = ground_y + item.kind.loose_offset();
                        item.velocity.x *= 0.76;
                        item.velocity.z *= 0.76;
                        item.velocity.y = 0.0;
                    }
                }

                let rolling_speed_squared = canonical_math::vec2_length_squared(Vec2::new(
                    item.velocity.x,
                    item.velocity.z,
                ));
                if lifetime_expired || rolling_speed_squared <= 0.18 * 0.18 {
                    let settle_position = item.position;
                    place_loose(&mut item, settle_position, arena);
                    continue;
                }

                item.state = ItemState::Rolling { lifetime };
            }
            ItemState::Spraying {
                owner,
                mut lifetime,
                mut spray_timer,
                mut spiral_phase,
                mut spiral_radius,
            } => {
                let lifetime_expired = lifetime.tick();
                let spray_due = advance_barrel_spray_timer(&mut spray_timer);
                item.state_age.advance();
                spiral_phase += dt * (5.0 + spiral_radius * 1.8);
                spiral_radius = (spiral_radius - dt * 0.9).max(0.16);

                let planar = Vec2::new(item.velocity.x, item.velocity.z);
                let damped = if canonical_math::vec2_length_squared(planar) > 0.0001 {
                    planar * BARREL_PLANAR_DAMPING_PER_TICK
                } else {
                    const FALLBACK: [Vec2; 4] = [Vec2::X, Vec2::Y, Vec2::NEG_X, Vec2::NEG_Y];
                    FALLBACK[(item.state_age.get() as usize / 15) % FALLBACK.len()]
                        * (spiral_radius * BARREL_PLANAR_DAMPING_PER_TICK)
                };
                let turned = Vec2::new(
                    damped.x * BARREL_TURN_COS - damped.y * BARREL_TURN_SIN,
                    damped.x * BARREL_TURN_SIN + damped.y * BARREL_TURN_COS,
                );
                item.velocity = Vec3::new(turned.x, 0.0, turned.y);
                integrate_item_position(&mut item, dt);

                if should_respawn_item(item.position, arena) {
                    item.set_respawning();
                    continue;
                }

                if spray_due {
                    for target_id in FighterId::ALL {
                        let Some((fighter, stats, action, fighter_transform)) = fighters
                            .iter()
                            .find(|(fighter, ..)| fighter.id == target_id.index())
                        else {
                            continue;
                        };
                        if target_id == owner
                            || !state.combat_target_allowed_for_state(owner.index(), fighter.id)
                            || !can_receive_impact(&stats, &action)
                        {
                            continue;
                        }
                        let delta = fighter_transform.translation - item.position;
                        debug_assert!(BARREL_SPRAY_RADIUS >= 0.0);
                        if canonical_math::vec2_length_squared(Vec2::new(delta.x, delta.z))
                            > BARREL_SPRAY_RADIUS * BARREL_SPRAY_RADIUS
                        {
                            continue;
                        }
                        let _ = contact_buffer.push(ContactRecord::new_status(
                            ContactSourceKind::ItemMeleeOrThrow,
                            stable_id,
                            Some(owner),
                            target_id,
                            u16::MAX,
                            AttackShapeId::HazardField as u16,
                            2,
                            fighter_transform.translation,
                            item.position,
                            ContactFlags::default(),
                        ));
                    }
                    contact_frame.record_spray(PendingBarrelSpray {
                        source: stable_id,
                        owner,
                        position: item.position,
                        spiral_phase,
                    });
                }

                if lifetime_expired {
                    lifetime.clear();
                }

                item.state = ItemState::Spraying {
                    owner,
                    lifetime,
                    spray_timer,
                    spiral_phase,
                    spiral_radius,
                };
            }
            ItemState::Armed {
                owner,
                mut timer,
                mut grace,
            } => {
                let fuse_expired = timer.tick();
                grace.tick();
                item.state_age.advance();
                item.velocity.y -= GRAVITY * dt;
                integrate_item_position(&mut item, dt);
                if let Some(ground_y) = item_ground_height(arena, item.position.x, item.position.z)
                {
                    if item.position.y <= ground_y + item.kind.loose_offset()
                        && item.velocity.y <= 0.0
                    {
                        item.position.y = ground_y + item.kind.loose_offset();
                        item.velocity.x *= 0.82;
                        item.velocity.z *= 0.82;
                        item.velocity.y = 0.0;
                    }
                }

                if should_respawn_item(item.position, arena) {
                    item.set_respawning();
                    continue;
                }

                if !fuse_expired {
                    item.state = ItemState::Armed {
                        owner,
                        timer,
                        grace,
                    };
                    continue;
                }

                let origin = item.position;

                for target_id in FighterId::ALL {
                    let Some((fighter, stats, action, fighter_transform)) = fighters
                        .iter()
                        .find(|(fighter, ..)| fighter.id == target_id.index())
                    else {
                        continue;
                    };
                    if target_id == owner && grace.active() {
                        continue;
                    }
                    if !state.combat_target_allowed_for_state(owner.index(), fighter.id) {
                        continue;
                    }
                    if !can_receive_impact(&stats, &action) {
                        continue;
                    }
                    let hurt_center = fighter_transform.translation + Vec3::Y * 0.82;
                    let delta = hurt_center - origin;
                    let flat_distance = canonical_math::vec2_length(Vec2::new(delta.x, delta.z));
                    let fighter_radius = FIGHTER_RADIUS * stats.item_size_multiplier();
                    if !pop_bomb_body_overlaps(flat_distance, fighter_radius) {
                        continue;
                    }
                    let blast_distance = pop_bomb_overlap_distance(flat_distance, fighter_radius);

                    let falloff = radial_falloff(blast_distance, POP_BOMB_RADIUS);
                    let mut blast_profile = impact_profile_from_payload_with_feel(
                        owner.index(),
                        ImpactSource::ItemBlast,
                        AttackPayloadId::BombBlast,
                        falloff.max(0.45),
                        falloff.max(0.55),
                        1.0,
                        28.0,
                        &feel,
                    );
                    let radial =
                        canonical_math::vec3_normalize_or_zero(Vec3::new(delta.x, 0.0, delta.z));
                    blast_profile.knockback_direction =
                        Some(if canonical_math::vec3_length_squared(radial) > 0.01 {
                            radial
                        } else {
                            Vec3::Z
                        });
                    let proximity = 1.0 - (blast_distance / POP_BOMB_RADIUS).clamp(0.0, 1.0);
                    let arc_planar_speed = STEAMER_BLAST_ARC_MIN_PLANAR_SPEED
                        .lerp(STEAMER_BLAST_ARC_MAX_PLANAR_SPEED, proximity);
                    let arc_vertical_speed = STEAMER_BLAST_ARC_MIN_VERTICAL_SPEED
                        .lerp(STEAMER_BLAST_ARC_MAX_VERTICAL_SPEED, proximity);
                    blast_profile.knockback =
                        arc_planar_speed / blast_profile.reaction.horizontal_scale.max(0.01);
                    blast_profile.vertical_knockback =
                        if blast_profile.reaction.vertical_scale > 0.01 {
                            arc_vertical_speed / blast_profile.reaction.vertical_scale
                        } else {
                            arc_vertical_speed
                        };
                    let _ = contact_buffer.push(ContactRecord::new(
                        ContactPhase::Strike,
                        ContactSourceKind::ItemMeleeOrThrow,
                        stable_id,
                        Some(owner),
                        target_id,
                        AttackPayloadId::BombBlast as u16,
                        AttackShapeId::BombBurst as u16,
                        1,
                        hurt_center,
                        origin,
                        blast_profile,
                        ContactFlags::default(),
                    ));
                }

                item.state = ItemState::Armed {
                    owner,
                    timer,
                    grace,
                };
            }
            _ => {}
        }
    }
}

fn committed_contact_outcome(kind: ContactOutcomeKind) -> bool {
    matches!(
        kind,
        ContactOutcomeKind::Accepted | ContactOutcomeKind::Guarded
    )
}

/// Applies item durability, hit memory, detonation, and lifecycle transitions
/// only after the shared frozen contact set has resolved.
pub fn apply_item_contact_outcomes(
    mut commands: Commands,
    mut identities: ResMut<SimulationIdentityAllocator>,
    tick: Res<SimTick>,
    active_arena: Res<ActiveArena>,
    state: Res<MatchState>,
    contact_frame: Res<ItemContactFrame>,
    contact_buffer: Res<ContactBuffer>,
    mut hitstop: ResMut<Hitstop>,
    mut sim_events: ResMut<TickEventBuffer>,
    mut item_presentation_intents: Option<ResMut<ItemPresentationIntentJournal>>,
    mut items: Query<(Entity, &StableSimEntity, &mut ArenaItem)>,
    mut fighters: Query<
        (
            &Fighter,
            &mut FighterStats,
            &mut FighterMotor,
            &mut DrunkStatus,
        ),
        Without<ArenaItem>,
    >,
) {
    if contact_frame.tick != Some(*tick) {
        return;
    }

    let arena = active_arena.definition();
    let item_capacity = identities.capacity(SimEntityKind::Item);
    let mut outstanding_crate_rewards = FixedItemIdSet::default();
    for item_index in 0..item_capacity {
        let Some((_, item_entity)) = identities.entry_at(SimEntityKind::Item, item_index) else {
            continue;
        };
        let Ok((_, _, item)) = items.get_mut(item_entity) else {
            continue;
        };
        if let Some(source) = item.crate_source() {
            let _ = outstanding_crate_rewards.insert(source);
        }
    }

    for item_index in 0..item_capacity {
        let Some((stable_id, item_entity)) = identities.entry_at(SimEntityKind::Item, item_index)
        else {
            continue;
        };
        let Ok((_entity, stable, mut item)) = items.get_mut(item_entity) else {
            continue;
        };
        if stable.id() != stable_id {
            continue;
        }

        if let Some(pending) = contact_frame
            .sprays()
            .find(|pending| pending.source == stable_id)
        {
            let mut affected_fighters = FighterHitMask::default();
            for contact_index in 0..contact_buffer.len() {
                let Some(contact) = contact_buffer.record(contact_index) else {
                    continue;
                };
                if contact.source.entity() != Some(stable_id)
                    || contact.phase != ContactPhase::Status
                    || contact.shape_id != AttackShapeId::HazardField as u16
                {
                    continue;
                }
                let outcome = contact_buffer
                    .outcome(contact_index)
                    .map_or(ContactOutcomeKind::Invalidated, |outcome| outcome.kind);
                if outcome != ContactOutcomeKind::Accepted {
                    continue;
                }
                if let Some((_, _, _, mut drunk)) = fighters
                    .iter_mut()
                    .find(|(fighter, ..)| fighter.id == contact.target.index())
                {
                    drunk.refresh();
                    affected_fighters.insert(contact.target);
                }
            }
            emit_item_presentation_intent(
                &mut sim_events,
                item_presentation_intents.as_deref_mut(),
                PendingItemPresentationIntent {
                    item: stable_id,
                    item_kind: item.kind,
                    fighter: Some(pending.owner),
                    fighter_name: None,
                    event: ItemLifecycleEvent::AlcoholSprayed,
                    kind: ItemPresentationKind::AlcoholSprayed {
                        position: pending.position,
                        spiral_phase: pending.spiral_phase,
                        affected_fighters,
                    },
                },
            );
        }

        match item.state {
            ItemState::Thrown {
                owner,
                lifetime,
                grace: _,
            } => {
                let mut contacted = false;
                for contact_index in 0..contact_buffer.len() {
                    let Some(contact) = contact_buffer.record(contact_index) else {
                        continue;
                    };
                    if contact.source.entity() != Some(stable_id)
                        || contact.phase != ContactPhase::Strike
                        || contact.shape_id != AttackShapeId::ItemLob as u16
                    {
                        continue;
                    }
                    let kind = contact_buffer
                        .outcome(contact_index)
                        .map_or(ContactOutcomeKind::Invalidated, |outcome| outcome.kind);
                    if committed_contact_outcome(kind) {
                        item.already_hit.insert(contact.target);
                        contacted = true;
                    }
                }

                if contacted {
                    // Durability belongs to the projectile contact as a whole,
                    // not to fighter query order. A frozen multi-target hit
                    // consumes exactly one use and then transitions once.
                    item.durability -= 1;
                    if item.kind == ItemKind::Crate {
                        let crate_position = item.position;
                        emit_item_presentation_intent(
                            &mut sim_events,
                            item_presentation_intents.as_deref_mut(),
                            PendingItemPresentationIntent {
                                item: stable_id,
                                item_kind: item.kind,
                                fighter: Some(owner),
                                fighter_name: None,
                                event: ItemLifecycleEvent::CrateOpened,
                                kind: ItemPresentationKind::CrateOpened {
                                    position: crate_position,
                                },
                            },
                        );
                        open_mystery_crate(
                            &mut commands,
                            &mut identities,
                            &mut item,
                            crate_position,
                            state.replay_seed,
                            *tick,
                            stable_id,
                            arena,
                            &mut outstanding_crate_rewards,
                        );
                    } else if item.kind == ItemKind::Barrel {
                        begin_barrel_spray(&mut item, owner, arena);
                    } else if item.durability <= 0 {
                        emit_item_presentation_intent(
                            &mut sim_events,
                            item_presentation_intents.as_deref_mut(),
                            PendingItemPresentationIntent {
                                item: stable_id,
                                item_kind: item.kind,
                                fighter: Some(owner),
                                fighter_name: None,
                                event: ItemLifecycleEvent::Broken,
                                kind: ItemPresentationKind::Broken {
                                    position: item.position,
                                },
                            },
                        );
                        item.set_respawning();
                    } else {
                        let settle_position = item.position;
                        place_loose(&mut item, settle_position, arena);
                    }
                    continue;
                }

                if should_respawn_item(item.position, arena) || !lifetime.active() {
                    item.set_respawning();
                    continue;
                }
                if item_ground_height(arena, item.position.x, item.position.z).is_some_and(
                    |ground_y| {
                        item.position.y <= ground_y + item.kind.loose_offset()
                            && item.velocity.y <= 0.0
                    },
                ) {
                    if item.kind == ItemKind::Crate {
                        let crate_position = item.position;
                        emit_item_presentation_intent(
                            &mut sim_events,
                            item_presentation_intents.as_deref_mut(),
                            PendingItemPresentationIntent {
                                item: stable_id,
                                item_kind: item.kind,
                                fighter: Some(owner),
                                fighter_name: None,
                                event: ItemLifecycleEvent::CrateOpened,
                                kind: ItemPresentationKind::CrateOpened {
                                    position: crate_position,
                                },
                            },
                        );
                        open_mystery_crate(
                            &mut commands,
                            &mut identities,
                            &mut item,
                            crate_position,
                            state.replay_seed,
                            *tick,
                            stable_id,
                            arena,
                            &mut outstanding_crate_rewards,
                        );
                    } else if item.kind == ItemKind::Barrel {
                        item.durability -= 1;
                        begin_barrel_spray(&mut item, owner, arena);
                    } else {
                        let settle_position = item.position;
                        place_loose(&mut item, settle_position, arena);
                    }
                }
            }
            ItemState::Armed { owner, timer, .. } if !timer.active() => {
                let origin = item.position;
                let blast_feedback = impact_feedback_profile(
                    ImpactSource::ItemBlast,
                    ImpactFeedbackIntensity::Heavy,
                );
                hitstop.trigger(blast_feedback.hitstop);
                emit_item_presentation_intent(
                    &mut sim_events,
                    item_presentation_intents.as_deref_mut(),
                    PendingItemPresentationIntent {
                        item: stable_id,
                        item_kind: item.kind,
                        fighter: Some(owner),
                        fighter_name: None,
                        event: ItemLifecycleEvent::Exploded,
                        kind: ItemPresentationKind::Exploded {
                            position: origin,
                            camera_shake: blast_feedback.shake,
                        },
                    },
                );

                for contact_index in 0..contact_buffer.len() {
                    let Some(contact) = contact_buffer.record(contact_index) else {
                        continue;
                    };
                    if contact.source.entity() != Some(stable_id)
                        || contact.phase != ContactPhase::Strike
                        || contact.shape_id != AttackShapeId::BombBurst as u16
                    {
                        continue;
                    }
                    let kind = contact_buffer
                        .outcome(contact_index)
                        .map_or(ContactOutcomeKind::Invalidated, |outcome| outcome.kind);
                    if !committed_contact_outcome(kind) {
                        continue;
                    }
                    if let Some((_, mut stats, mut motor, _)) = fighters
                        .iter_mut()
                        .find(|(fighter, ..)| fighter.id == contact.target.index())
                    {
                        let launched_planar_speed = canonical_math::vec2_length(Vec2::new(
                            motor.velocity.x,
                            motor.velocity.z,
                        ));
                        motor
                            .impact_speed_limit_timer
                            .set_max(TickTimer::from_seconds_ceil(
                                STEAMER_BLAST_ARC_SPEED_LIMIT_TIME,
                            ));
                        motor.impact_speed_limit =
                            motor.impact_speed_limit.max(launched_planar_speed);
                        if contact.target == owner {
                            stats.stamina = (stats.stamina - 8.0).max(0.0);
                        }
                    }
                }
                item.set_respawning();
            }
            ItemState::Spraying { lifetime, .. } if !lifetime.active() => {
                if item.durability <= 0 {
                    item.set_respawning();
                } else {
                    let settle_position = item.position;
                    place_loose(&mut item, settle_position, arena);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
fn item_throw_profile(kind: ItemKind, owner_id: usize) -> ImpactProfile {
    item_throw_profile_from_payload(kind, owner_id, None)
}

fn item_throw_profile_with_feel(
    kind: ItemKind,
    owner_id: usize,
    feel: &CombatFeelTuning,
) -> ImpactProfile {
    item_throw_profile_from_payload(kind, owner_id, Some(feel))
}

fn item_throw_profile_from_payload(
    kind: ItemKind,
    owner_id: usize,
    feel: Option<&CombatFeelTuning>,
) -> ImpactProfile {
    let (payload_id, damage_scale, knockback_scale, vertical_scale) = match kind {
        ItemKind::Apple | ItemKind::WineWhite | ItemKind::CupCoffee | ItemKind::Mushroom => {
            (AttackPayloadId::ItemThrowLight, 0.55, 0.72, 0.86)
        }
        ItemKind::Turkey | ItemKind::Barrel | ItemKind::Crate => {
            (AttackPayloadId::ItemThrowHeavy, 1.05, 0.98, 0.96)
        }
        ItemKind::Steamer => (AttackPayloadId::ItemThrowHeavy, 1.0, 1.0, 1.0),
    };

    if let Some(feel) = feel {
        return impact_profile_from_payload_with_feel(
            owner_id,
            ImpactSource::ItemThrow,
            payload_id,
            damage_scale,
            knockback_scale,
            vertical_scale,
            24.0,
            feel,
        );
    }

    impact_profile_from_payload(
        owner_id,
        ImpactSource::ItemThrow,
        payload_id,
        damage_scale,
        knockback_scale,
        vertical_scale,
        24.0,
    )
}

fn should_respawn_item(position: Vec3, arena: &ArenaDefinition) -> bool {
    debug_assert!(arena.ringout_radius >= 0.0);
    position.y < arena.ringout_y
        || canonical_math::vec2_length_squared(Vec2::new(position.x, position.z))
            > arena.ringout_radius * arena.ringout_radius
}

pub fn item_scale(kind: ItemKind) -> Vec3 {
    match kind {
        ItemKind::Crate => Vec3::splat(1.7),
        ItemKind::Steamer => Vec3::splat(0.72 * 2.0),
        ItemKind::Apple => Vec3::splat(0.82 * 3.0),
        ItemKind::WineWhite => Vec3::splat(0.78 * 2.0),
        ItemKind::Turkey => Vec3::splat(0.85 * 2.0),
        ItemKind::Barrel => Vec3::splat(0.72 * 2.0),
        ItemKind::CupCoffee => Vec3::splat(0.7 * 3.0),
        ItemKind::Mushroom => Vec3::splat(0.78 * 4.5),
    }
}

fn item_presentation_event_kind(kind: ItemPresentationKind) -> ItemLifecycleEvent {
    match kind {
        ItemPresentationKind::PickedUp { .. } => ItemLifecycleEvent::PickedUp,
        ItemPresentationKind::Thrown { .. } => ItemLifecycleEvent::Thrown,
        ItemPresentationKind::Used { .. } => ItemLifecycleEvent::Used,
        ItemPresentationKind::Dropped { .. } => ItemLifecycleEvent::Dropped,
        ItemPresentationKind::Broken { .. } => ItemLifecycleEvent::Broken,
        ItemPresentationKind::CrateOpened { .. } => ItemLifecycleEvent::CrateOpened,
        ItemPresentationKind::AlcoholSprayed { .. } => ItemLifecycleEvent::AlcoholSprayed,
        ItemPresentationKind::Exploded { .. } => ItemLifecycleEvent::Exploded,
    }
}

fn item_presentation_matches_event(event: SimEvent, intent: ItemPresentationIntent) -> bool {
    if event.id != intent.event_id
        || event.id.source != SimEventSource::Entity(intent.item)
        || intent.item.kind() != SimEntityKind::Item
    {
        return false;
    }
    matches!(
        event.kind,
        SimEventKind::ItemLifecycle {
            item,
            fighter,
            event,
        } if item == intent.item
            && fighter == intent.fighter
            && event == item_presentation_event_kind(intent.kind)
    )
}

/// Applies one validated item sidecar from the shared Update-side event router.
/// Stable IDs suppress duplicate one-shots across rollback re-simulation.
pub(crate) fn present_item_lifecycle_event(
    event: SimEvent,
    intent: ItemPresentationIntent,
    commands: &mut Commands,
    effect_assets: &EffectAssets,
    feedback: &mut HitEffects,
    announcements: Option<&mut MatchAnnouncements>,
    fighters: &mut Query<
        (
            Entity,
            &Fighter,
            &mut FighterStats,
            &mut FighterMotor,
            &mut FighterActionState,
        ),
        With<Fighter>,
    >,
) -> bool {
    if !item_presentation_matches_event(event, intent) {
        return false;
    }

    match intent.kind {
        ItemPresentationKind::PickedUp { .. } => {
            feedback.push_feedback_cue("item_pickup", ImpactSource::ItemUtility, 18);
            if let (Some(name), Some(announcements)) = (intent.fighter_name, announcements) {
                announcements.show(
                    format!("{} picked up {}", name, intent.item_kind.label()),
                    1.0,
                );
            }
        }
        ItemPresentationKind::Thrown { .. } => {
            feedback.push_feedback_cue("item_throw", ImpactSource::ItemThrow, 24);
            if let (Some(name), Some(announcements)) = (intent.fighter_name, announcements) {
                announcements.show(format!("{} threw {}", name, intent.item_kind.label()), 1.0);
            }
        }
        ItemPresentationKind::Used {
            position,
            announcement,
        } => {
            spawn_guard_flash(commands, effect_assets, position);
            feedback.push_feedback_cue("item_use", ImpactSource::ItemUtility, 22);
            if let Some(cue) = item_use_sfx_cue(intent.item_kind, position) {
                feedback.push_combat_sfx(cue);
            }
            if let Some(fighter_id) = intent.fighter
                && let Some((_, _, mut stats, _, _)) = fighters
                    .iter_mut()
                    .find(|(_, fighter, ..)| fighter.id == fighter_id.index())
            {
                stats.hud_flash = stats.hud_flash.max(0.28);
            }
            if let (Some(name), Some(announcements)) = (intent.fighter_name, announcements) {
                announcements.show(format!("{name} {announcement}"), 1.0);
            }
        }
        ItemPresentationKind::Dropped { position } => {
            feedback.push_combat_sfx(item_drop_sfx_cue(position));
        }
        ItemPresentationKind::Broken { position }
        | ItemPresentationKind::CrateOpened { position } => {
            spawn_dust_puff(commands, effect_assets, position);
        }
        ItemPresentationKind::AlcoholSprayed {
            position,
            spiral_phase,
            affected_fighters,
        } => {
            spawn_alcohol_spray(commands, effect_assets, position, spiral_phase);
            for (_, fighter, mut stats, _, _) in fighters.iter_mut() {
                if let Some(fighter_id) = FighterId::from_index(fighter.id)
                    && affected_fighters.contains(fighter_id)
                {
                    stats.hud_flash = stats.hud_flash.max(0.12);
                }
            }
        }
        ItemPresentationKind::Exploded {
            position,
            camera_shake,
        } => {
            spawn_pop_bomb_blast(commands, effect_assets, position);
            feedback.push_combat_sfx(steamer_explosion_sfx_cue(position));
            feedback.shake = feedback.shake.max(camera_shake);
        }
    }
    true
}

/// Projects newly restored/spawned canonical items into the rendered client.
/// Authorities never schedule this system and therefore keep item entities free
/// of meshes, scenes, transforms, names, and visibility components.
pub fn attach_missing_item_visuals(
    mut commands: Commands,
    assets: Res<ItemAssets>,
    items: Query<(Entity, &ArenaItem), Without<Mesh3d>>,
) {
    for (entity, item) in &items {
        let (mesh, material, scale) = item_visuals(&assets, item.kind, false);
        commands.entity(entity).insert((
            Mesh3d(mesh),
            MeshMaterial3d(material),
            SceneRoot(assets.scene_for(item.kind)),
            Transform::from_translation(item.position).with_scale(scale),
            Name::new(item.kind.label()),
        ));
    }
}

pub fn sync_item_visuals(
    assets: Res<ItemAssets>,
    fighters: Query<
        (
            &Fighter,
            &FighterInventory,
            &FighterMotor,
            &FighterActionState,
            &Transform,
        ),
        Without<ArenaItem>,
    >,
    mut items: Query<
        (
            &StableSimEntity,
            &ArenaItem,
            &mut Transform,
            &mut Visibility,
            &mut MeshMaterial3d<StandardMaterial>,
        ),
        Without<Fighter>,
    >,
) {
    for (stable, item, mut transform, mut visibility, mut material) in &mut items {
        let mut pose = item_unheld_visual_pose(item);
        let mut visible = !matches!(item.state, ItemState::Respawning);

        if let ItemState::Held { holder } = item.state {
            let held_pose = fighters.iter().find_map(
                |(fighter, inventory, motor, action, fighter_transform)| {
                    (FighterId::from_index(fighter.id) == Some(holder)
                        && inventory.held == Some(stable.id()))
                    .then(|| held_item_visual_pose(item, motor, action, fighter_transform))
                },
            );
            if let Some(held_pose) = held_pose {
                pose = held_pose;
            } else {
                // A stale relationship is cleaned up by fixed simulation. Do
                // not leave a held visual frozen at a render-frame-dependent
                // pose while waiting for that deterministic cleanup.
                visible = false;
            }
        }

        *transform = pose;
        material.0 = assets.material_for(item.kind, matches!(item.state, ItemState::Armed { .. }));
        *visibility = if visible {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
    }
}

fn item_unheld_visual_pose(item: &ArenaItem) -> Transform {
    let age = item.state_age.as_seconds();
    let mut translation = item.position;
    let (rotation, scale) = match item.state {
        ItemState::Loose => {
            translation.y = item.base_y + item_visual_wave(item.state_age, item.phase) * 0.08;
            (Quat::from_rotation_y(age * 1.6), item_scale(item.kind))
        }
        ItemState::Thrown { .. } => (
            Quat::from_rotation_x(age * 9.0) * Quat::from_rotation_z(age * 4.0),
            item_scale(item.kind),
        ),
        ItemState::Rolling { .. } => (
            Quat::from_rotation_x(age * 8.0) * Quat::from_rotation_z(age * 5.0),
            item_scale(item.kind),
        ),
        ItemState::Spraying { .. } => (
            Quat::from_rotation_y(age * 24.0)
                * Quat::from_rotation_x(item_visual_wave(item.state_age, 0.0) * 0.12)
                * Quat::from_rotation_z(item_visual_wave(item.state_age, 4.0) * 0.1),
            item_scale(item.kind),
        ),
        ItemState::Armed { .. } => (
            Quat::from_rotation_y(age * 5.0),
            item_scale(item.kind) * (1.0 + item_visual_wave(item.state_age, 0.0).abs() * 0.18),
        ),
        ItemState::Held { .. } | ItemState::Respawning => (Quat::IDENTITY, item_scale(item.kind)),
    };
    Transform::from_translation(translation)
        .with_rotation(rotation)
        .with_scale(scale)
}

fn held_item_visual_pose(
    item: &ArenaItem,
    motor: &FighterMotor,
    action: &FighterActionState,
    fighter_transform: &Transform,
) -> Transform {
    let facing = motor.facing.normalize_or_zero();
    let right = Vec3::new(facing.z, 0.0, -facing.x).normalize_or_zero();
    let swing_forward = if action.action == FighterAction::ItemSwing {
        0.32
    } else {
        0.0
    };
    let translation = fighter_transform.translation
        + Vec3::Y * 0.88
        + right * 0.42
        + facing * (0.36 + swing_forward);
    let yaw = facing.x.atan2(facing.z);
    let pulse = if item.kind == ItemKind::Steamer && action.action == FighterAction::ItemThrow {
        1.12
    } else {
        1.0
    };
    Transform::from_translation(translation)
        .with_rotation(Quat::from_rotation_y(yaw))
        .with_scale(item_scale(item.kind) * pulse)
}

fn open_mystery_crate(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    crate_item: &mut ArenaItem,
    position: Vec3,
    master_seed: u64,
    tick: SimTick,
    crate_id: SimEntityId,
    arena: &ArenaDefinition,
    outstanding_rewards: &mut FixedItemIdSet,
) {
    let reward = mystery_crate_reward(master_seed, tick, crate_id);
    let ground_y = item_ground_height(arena, position.x, position.z).unwrap_or(ARENA_TOP_Y);
    let reward_position = Vec3::new(position.x, ground_y + reward.loose_offset(), position.z);
    let mut phase_rng = item_event_rng(
        master_seed,
        tick,
        crate_id,
        FighterId::ZERO,
        "items/reward-phase",
    );
    let phase = phase_rng.gen_range_u32(0..6_284).unwrap() as f32 * 0.001;
    if outstanding_rewards.insert(crate_id) {
        let _ = spawn_canonical_pickup(
            commands,
            identities,
            reward,
            reward_position,
            phase,
            Some(crate_id),
        );
    }
    crate_item.set_respawning();
}

fn mystery_crate_reward(master_seed: u64, tick: SimTick, crate_id: SimEntityId) -> ItemKind {
    const REWARDS: [ItemKind; 7] = [
        ItemKind::Steamer,
        ItemKind::Apple,
        ItemKind::WineWhite,
        ItemKind::Turkey,
        ItemKind::Barrel,
        ItemKind::CupCoffee,
        ItemKind::Mushroom,
    ];
    let mut rng = item_event_rng(
        master_seed,
        tick,
        crate_id,
        FighterId::ZERO,
        "items/rewards",
    );
    REWARDS[rng.gen_range_u32(0..REWARDS.len() as u32).unwrap() as usize]
}

fn item_event_rng(
    master_seed: u64,
    tick: SimTick,
    item: SimEntityId,
    fighter: FighterId,
    stream: &str,
) -> DeterministicRngStream {
    // Counterless keyed randomness: event identity, rather than mutable draw
    // order, is the replay contract. Purpose names isolate subsystem changes.
    let mut key = CanonicalHash64::new();
    key.write_str("items/event/v1")
        .write_u64(master_seed)
        .write_u64(tick.get())
        .write_sim_entity_id(item)
        .write_fighter_id(fighter)
        .write_str(stream);
    let event_seed = key.finish();
    DeterministicRngStream::from_master_seed(event_seed, RngStreamName::from_label(stream))
}

#[allow(dead_code)]
pub fn held_item_label(
    inventory: &FighterInventory,
    items: &Query<(&StableSimEntity, &ArenaItem)>,
) -> Option<String> {
    let item_id = inventory.held?;
    let (_, item) = items.iter().find(|(stable, _)| stable.id() == item_id)?;
    Some(item.status_label())
}

fn held_reference_is_stale(item: Option<&ArenaItem>, fighter: FighterId) -> bool {
    match item {
        Some(item) => !item.is_held_by(fighter),
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena_defs::arena_definitions;
    use crate::characters::{CharacterKind, CharacterMoveCatalog, FighterCharacter};
    use crate::combat::{
        CombatPresentationIntentJournal, begin_contact_collection, present_committed_combat_events,
        resolve_contacts,
    };
    use crate::components::{FighterGrabState, FighterUltimateState};
    use crate::equipment::FighterEquipment;
    use crate::game_state::MatchTelemetry;
    use crate::live_dynamic_snapshot::LiveItemSnapshotCodec;
    use crate::sim_event::{PresentationEventCursor, PresentationEventRouter, SimEventJournal};
    use crate::snapshot_ecs::DynamicSnapshotCodec;
    use crate::styles::FighterStyle;

    fn fighter(index: u8) -> FighterId {
        FighterId::new(index).unwrap()
    }

    fn item_id(index: u32, generation: u32) -> SimEntityId {
        SimEntityId::new(SimEntityKind::Item, index, generation)
    }

    fn canonical_item_rows(
        world: &mut World,
    ) -> Vec<(SimEntityId, ItemKind, Vec3, Vec3, Option<SimEntityId>)> {
        let mut query = world.query::<(&StableSimEntity, &ArenaItem)>();
        let mut rows = query
            .iter(world)
            .map(|(stable, item)| {
                (
                    stable.id(),
                    item.kind,
                    item.position,
                    item.anchor,
                    item.crate_source(),
                )
            })
            .collect::<Vec<_>>();
        rows.sort_unstable_by_key(|row| row.0);
        rows
    }

    #[test]
    fn canonical_item_bootstrap_populates_exact_anchors_without_presentation() {
        let mut world = World::new();
        let arena = &arena_definitions()[0];
        let report = reset_canonical_items_for_arena(&mut world, arena);
        assert_eq!(
            report,
            ItemArenaResetReport {
                spawned_anchors: arena.item_anchors.len(),
                ..default()
            }
        );

        let rows = canonical_item_rows(&mut world);
        assert_eq!(rows.len(), arena.item_anchors.len());
        for (index, (row, anchor)) in rows.iter().zip(arena.item_anchors).enumerate() {
            let canonical_position = Vec3::new(
                canonicalize_f32(anchor.position.x, DEFAULT_F32_QUANTIZATION),
                canonicalize_f32(anchor.position.y, DEFAULT_F32_QUANTIZATION),
                canonicalize_f32(anchor.position.z, DEFAULT_F32_QUANTIZATION),
            );
            assert_eq!(row.0, item_id(index as u32, 1));
            assert_eq!(row.1, anchor.kind);
            assert_eq!(row.2, canonical_position);
            assert_eq!(row.3, canonical_position);
            assert_eq!(row.4, None);
            let entity = world
                .resource::<SimulationIdentityAllocator>()
                .mapped_entity(row.0)
                .unwrap();
            LiveItemSnapshotCodec
                .capture(&world, entity, row.0)
                .expect("canonical item bootstrap is snapshot-ready before its first tick");
            assert!(world.get::<Transform>(entity).is_none());
            assert!(world.get::<Visibility>(entity).is_none());
            assert!(world.get::<Mesh3d>(entity).is_none());
            assert!(
                world
                    .get::<MeshMaterial3d<StandardMaterial>>(entity)
                    .is_none()
            );
            assert!(world.get::<SceneRoot>(entity).is_none());
            assert!(world.get::<ChildOf>(entity).is_none());
            assert!(world.get::<Children>(entity).is_none());
            assert!(world.get::<Name>(entity).is_none());
            assert!(world.get::<crate::arena::ArenaGeometry>(entity).is_none());
        }
        assert!(world.get_resource::<ItemAssets>().is_none());
        assert!(world.get_resource::<AssetServer>().is_none());
        assert!(world.get_resource::<Assets<Mesh>>().is_none());
        assert!(world.get_resource::<Assets<StandardMaterial>>().is_none());

        let before_repeat = rows;
        assert_eq!(
            reset_canonical_items_for_arena(&mut world, arena),
            ItemArenaResetReport {
                retained_anchors: arena.item_anchors.len(),
                ..default()
            }
        );
        assert_eq!(canonical_item_rows(&mut world), before_repeat);
    }

    #[test]
    fn canonical_item_arena_switch_is_deterministic_and_pool_bounded() {
        fn run_switch_sequence() -> (
            Vec<(SimEntityId, ItemKind, Vec3, Vec3, Option<SimEntityId>)>,
            ItemArenaResetReport,
        ) {
            let mut world = World::new();
            let larger = &arena_definitions()[0];
            let smaller = &arena_definitions()[1];
            assert!(larger.item_anchors.len() > smaller.item_anchors.len());

            reset_canonical_items_for_arena(&mut world, larger);
            let shrink = reset_canonical_items_for_arena(&mut world, smaller);
            assert_eq!(shrink.retained_anchors, smaller.item_anchors.len());
            assert_eq!(
                shrink.released_excess,
                larger.item_anchors.len() - smaller.item_anchors.len()
            );
            let grow = reset_canonical_items_for_arena(&mut world, larger);
            assert_eq!(grow.retained_anchors, smaller.item_anchors.len());
            assert_eq!(
                grow.spawned_anchors,
                larger.item_anchors.len() - smaller.item_anchors.len()
            );
            assert_eq!(grow.rejected_anchors, 0);
            assert_eq!(
                world
                    .resource::<SimulationIdentityAllocator>()
                    .live_count(SimEntityKind::Item) as usize,
                larger.item_anchors.len()
            );
            assert!(larger.item_anchors.len() <= ITEM_ENTITY_CAPACITY);
            (canonical_item_rows(&mut world), grow)
        }

        let first = run_switch_sequence();
        let second = run_switch_sequence();
        assert_eq!(first, second);
    }

    fn test_item_assets() -> ItemAssets {
        ItemAssets {
            item_mesh: Handle::default(),
            steamer_scene: Handle::default(),
            apple_scene: Handle::default(),
            wine_white_scene: Handle::default(),
            turkey_scene: Handle::default(),
            barrel_scene: Handle::default(),
            cup_coffee_scene: Handle::default(),
            mushroom_scene: Handle::default(),
            crate_scene: Handle::default(),
            steamer_material: Handle::default(),
            apple_material: Handle::default(),
            wine_white_material: Handle::default(),
            turkey_material: Handle::default(),
            barrel_material: Handle::default(),
            coffee_material: Handle::default(),
            mushroom_material: Handle::default(),
            crate_material: Handle::default(),
            live_bomb_material: Handle::default(),
        }
    }

    fn spawn_test_item(world: &mut World, item: ArenaItem) -> (Entity, SimEntityId) {
        let entity = world.spawn_empty().id();
        let stable = world
            .resource_mut::<SimulationIdentityAllocator>()
            .try_allocate(SimEntityKind::Item, entity)
            .unwrap();
        let id = stable.id();
        world.entity_mut(entity).insert((
            stable,
            Transform::from_translation(item.position),
            Visibility::Visible,
            MeshMaterial3d::<StandardMaterial>(Handle::default()),
            item,
        ));
        (entity, id)
    }

    #[derive(Debug, PartialEq)]
    struct ThrownMultiTargetFixture {
        health: [f32; 2],
        victims: Vec<FighterId>,
        source: SimEntityId,
        durability: i32,
        state: ItemState,
    }

    fn run_thrown_multi_target_fixture(reverse_ecs_allocation: bool) -> ThrownMultiTargetFixture {
        let owner = FighterId::ZERO;
        let target_a = FighterId::new(1).unwrap();
        let target_b = FighterId::new(2).unwrap();
        let mut match_state = MatchState::default();
        match_state.rules = crate::game_state::RULE_PRESETS[1];
        match_state.rule_index = 1;
        match_state.set_active_slots([true, true, true, false]);
        match_state.reset_for_new_match();

        let mut app = App::new();
        app.insert_resource(ActiveArena::default())
            .insert_resource(SimulationIdentityAllocator::default())
            .insert_resource(match_state)
            .insert_resource(CombatFeelTuning::default())
            .insert_resource(CharacterMoveCatalog::default())
            .insert_resource(Hitstop::default())
            .insert_resource(MatchTelemetry::default())
            .insert_resource(ContactBuffer::default())
            .insert_resource(ItemContactFrame::default())
            .insert_resource(SimTick::default())
            .insert_resource(TickEventBuffer::default())
            .add_systems(
                Update,
                (
                    begin_contact_collection,
                    advance_moving_items_and_collect_contacts,
                    resolve_contacts,
                    apply_item_contact_outcomes,
                )
                    .chain(),
            );
        if reverse_ecs_allocation {
            app.world_mut().spawn_empty();
        }

        let target_position = Vec3::new(0.0, ARENA_TOP_Y, 0.0);
        let spawn_fighter = |world: &mut World, fighter_id: FighterId| {
            let position = if fighter_id == owner {
                target_position + Vec3::X * 5.0
            } else {
                target_position
            };
            world.spawn((
                Fighter {
                    id: fighter_id.index(),
                    name: "Thrown multi-target",
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
                DrunkStatus::default(),
                FighterStyle {
                    kind: crate::styles::FighterStyleKind::Anchor,
                },
                FighterEquipment::new(crate::equipment::EquipmentKind::CounterCell),
                SimPosition::new(position),
            ));
        };
        let fighter_order = if reverse_ecs_allocation {
            [target_b, target_a, owner]
        } else {
            [owner, target_a, target_b]
        };
        for fighter_id in fighter_order {
            spawn_fighter(app.world_mut(), fighter_id);
        }

        let item_position = target_position + Vec3::Y * (FIGHTER_HEIGHT * 0.58);
        let mut item = ArenaItem::new(ItemKind::Turkey, item_position, 0.0);
        item.launch_as_thrown(owner, Vec3::ZERO);
        let (item_entity, source) = spawn_test_item(app.world_mut(), item);

        app.update();

        let mut health = [0.0; 2];
        {
            let world = app.world_mut();
            let mut fighters = world.query::<(&Fighter, &FighterStats)>();
            for (fighter, stats) in fighters.iter(world) {
                if fighter.id == target_a.index() {
                    health[0] = stats.health;
                } else if fighter.id == target_b.index() {
                    health[1] = stats.health;
                }
            }
        }
        let victims = app
            .world()
            .resource::<TickEventBuffer>()
            .iter()
            .filter_map(|event| match event.kind {
                SimEventKind::HitConfirmed { victim, .. } => Some(victim),
                _ => None,
            })
            .collect();
        let item = app.world().get::<ArenaItem>(item_entity).unwrap();
        ThrownMultiTargetFixture {
            health,
            victims,
            source,
            durability: item.durability,
            state: item.state,
        }
    }

    #[test]
    fn thrown_projectile_freezes_all_targets_and_consumes_durability_once() {
        let forward = run_thrown_multi_target_fixture(false);
        let reversed = run_thrown_multi_target_fixture(true);

        assert_eq!(forward, reversed);
        assert!(
            forward
                .health
                .iter()
                .all(|health| *health < crate::constants::MAX_HEALTH),
            "{forward:?}"
        );
        assert_eq!(
            forward.victims,
            vec![FighterId::new(1).unwrap(), FighterId::new(2).unwrap()]
        );
        assert_eq!(forward.durability, ItemKind::Turkey.max_durability() - 1);
        assert_eq!(forward.state, ItemState::Loose);
        assert_eq!(forward.source.kind(), SimEntityKind::Item);
    }

    #[test]
    fn barrel_spray_status_is_applied_only_from_an_accepted_geometry_record() {
        let owner = FighterId::ZERO;
        let target = FighterId::new(1).unwrap();
        let mut match_state = MatchState::default();
        match_state.rules = crate::game_state::RULE_PRESETS[1];
        match_state.rule_index = 1;
        match_state.set_active_slots([true, true, false, false]);
        match_state.reset_for_new_match();
        let mut app = App::new();
        app.insert_resource(ActiveArena::default())
            .insert_resource(SimulationIdentityAllocator::default())
            .insert_resource(match_state)
            .insert_resource(CombatFeelTuning::default())
            .insert_resource(CharacterMoveCatalog::default())
            .insert_resource(Hitstop::default())
            .insert_resource(MatchTelemetry::default())
            .insert_resource(ContactBuffer::default())
            .insert_resource(ItemContactFrame::default())
            .insert_resource(ItemPresentationIntentJournal::default())
            .insert_resource(SimTick::default())
            .insert_resource(TickEventBuffer::default())
            .add_systems(
                Update,
                (
                    begin_contact_collection,
                    advance_moving_items_and_collect_contacts,
                    resolve_contacts,
                    apply_item_contact_outcomes,
                )
                    .chain(),
            );

        let target_position = Vec3::new(0.0, ARENA_TOP_Y, 0.0);
        for (fighter_id, position) in [
            (owner, target_position + Vec3::X * 5.0),
            (target, target_position),
        ] {
            app.world_mut().spawn((
                Fighter {
                    id: fighter_id.index(),
                    name: "Barrel spray status",
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
                DrunkStatus::default(),
                FighterStyle {
                    kind: crate::styles::FighterStyleKind::Anchor,
                },
                FighterEquipment::new(crate::equipment::EquipmentKind::CounterCell),
                SimPosition::new(position),
            ));
        }
        let barrel_position = target_position + Vec3::Y * 0.2;
        let mut barrel = ArenaItem::new(ItemKind::Barrel, barrel_position, 0.0);
        barrel.start_barrel_spray(owner);
        spawn_test_item(app.world_mut(), barrel);

        app.update();

        let (health, drunk_active) = {
            let world = app.world_mut();
            let mut fighters = world.query::<(&Fighter, &FighterStats, &DrunkStatus)>();
            let (_, stats, drunk) = fighters
                .iter(world)
                .find(|(fighter, ..)| fighter.id == target.index())
                .unwrap();
            (stats.health, drunk.active())
        };
        assert_eq!(health, crate::constants::MAX_HEALTH);
        assert!(drunk_active);
        let contacts = app.world().resource::<ContactBuffer>();
        assert_eq!(contacts.len(), 1);
        assert_eq!(contacts.record(0).unwrap().phase, ContactPhase::Status);
        assert!(contacts.record(0).unwrap().impact.is_none());
        assert_eq!(
            contacts.outcome(0).unwrap().kind,
            ContactOutcomeKind::Accepted
        );
        assert!(contacts.outcome(0).unwrap().event_id.is_none());

        let event = app
            .world()
            .resource::<TickEventBuffer>()
            .iter()
            .next()
            .copied()
            .unwrap();
        assert!(matches!(
            event.kind,
            SimEventKind::ItemLifecycle {
                fighter: Some(event_owner),
                event: ItemLifecycleEvent::AlcoholSprayed,
                ..
            } if event_owner == owner
        ));
        let intent = app
            .world()
            .resource::<ItemPresentationIntentJournal>()
            .get(event.id)
            .unwrap();
        assert!(matches!(
            intent.kind,
            ItemPresentationKind::AlcoholSprayed {
                affected_fighters,
                ..
            } if affected_fighters.contains(target)
        ));
    }

    fn presentation_intent_at(tick: u64, ordinal: u16) -> ItemPresentationIntent {
        let item = item_id(1, 1);
        ItemPresentationIntent {
            event_id: SimEventId {
                tick: SimTick(tick),
                source: SimEventSource::Entity(item),
                ordinal,
            },
            item,
            item_kind: ItemKind::Apple,
            fighter: None,
            fighter_name: None,
            kind: ItemPresentationKind::Broken { position: Vec3::X },
        }
    }

    fn commit_item_presentation_event(
        journal: &mut SimEventJournal,
        intents: &mut ItemPresentationIntentJournal,
        tick: u64,
    ) -> SimEventId {
        let item = item_id(1, 1);
        let mut buffer = TickEventBuffer::new(SimTick(tick));
        let event_id = buffer
            .emit(
                SimEventSource::Entity(item),
                SimEventKind::ItemLifecycle {
                    item,
                    fighter: None,
                    event: ItemLifecycleEvent::Broken,
                },
            )
            .unwrap();
        journal.commit(&buffer);
        intents
            .record(ItemPresentationIntent {
                event_id,
                ..presentation_intent_at(tick, event_id.ordinal)
            })
            .unwrap();
        event_id
    }

    #[derive(Resource, Default)]
    struct ResetTestCapture(Option<ItemArenaResetReport>);

    fn reset_items_once(
        mut commands: Commands,
        mut identities: ResMut<SimulationIdentityAllocator>,
        assets: Res<ItemAssets>,
        arena: Res<ActiveArena>,
        mut items: Query<(Entity, &StableSimEntity, &mut ArenaItem)>,
        mut capture: ResMut<ResetTestCapture>,
    ) {
        if capture.0.is_none() {
            capture.0 = Some(reset_items_for_arena(
                &mut commands,
                &mut identities,
                &mut items,
                &assets,
                arena.definition(),
            ));
        }
    }

    #[derive(Resource, Default)]
    struct CrateStressCycles(u32);

    fn force_one_crate_open_per_update(
        mut commands: Commands,
        mut identities: ResMut<SimulationIdentityAllocator>,
        arena: Res<ActiveArena>,
        mut cycles: ResMut<CrateStressCycles>,
        mut items: Query<(Entity, &StableSimEntity, &mut ArenaItem)>,
    ) {
        if cycles.0 >= 1_000 {
            return;
        }

        let mut outstanding = FixedItemIdSet::default();
        let mut source = None;
        for (entity, stable, item) in items.iter_mut() {
            if let Some(crate_source) = item.crate_source() {
                assert!(outstanding.insert(crate_source));
            } else if item.kind == ItemKind::Crate {
                source = Some((stable.id(), entity));
            }
        }
        let (source_id, source_entity) =
            source.expect("stress fixture must retain its source crate");
        let Ok((_, _, mut crate_item)) = items.get_mut(source_entity) else {
            unreachable!();
        };
        let crate_position = crate_item.position;
        open_mystery_crate(
            &mut commands,
            &mut identities,
            &mut crate_item,
            crate_position,
            0xCAFE_BABE,
            SimTick(u64::from(cycles.0)),
            source_id,
            arena.definition(),
            &mut outstanding,
        );
        cycles.0 += 1;
    }

    #[test]
    fn one_thousand_crate_cycles_keep_exactly_one_outstanding_reward() {
        let mut app = App::new();
        app.insert_resource(SimulationIdentityAllocator::default())
            .insert_resource(test_item_assets())
            .insert_resource(ActiveArena::new(3))
            .init_resource::<CrateStressCycles>()
            .add_systems(Update, force_one_crate_open_per_update);
        let (_, source_id) = spawn_test_item(
            app.world_mut(),
            ArenaItem::new(ItemKind::Crate, Vec3::new(-1.0, 0.5, 2.0), 0.0),
        );

        for _ in 0..1_000 {
            app.update();
        }

        let identities = app.world().resource::<SimulationIdentityAllocator>();
        assert_eq!(identities.live_count(SimEntityKind::Item), 2);
        assert_eq!(identities.rejected_spawns(SimEntityKind::Item), 0);
        let mut item_query = app.world_mut().query::<(Entity, &ArenaItem)>();
        let rewards = item_query
            .iter(app.world())
            .filter(|(_, item)| item.crate_source() == Some(source_id))
            .collect::<Vec<_>>();
        assert_eq!(rewards.len(), 1);
        assert!(
            app.world()
                .get::<crate::arena::ArenaGeometry>(rewards[0].0)
                .is_none(),
            "crate rewards are authoritative items, not arena geometry"
        );
        assert!(app.world().get::<Transform>(rewards[0].0).is_none());
        assert!(app.world().get::<Visibility>(rewards[0].0).is_none());
        assert!(app.world().get::<Mesh3d>(rewards[0].0).is_none());
        assert!(app.world().get::<SceneRoot>(rewards[0].0).is_none());
    }

    #[test]
    fn item_presentation_journal_is_fixed_capacity_and_rejects_bad_ordinals() {
        let mut intents = ItemPresentationIntentJournal::default();
        for tick in 0..SIM_EVENT_HISTORY_TICKS as u64 {
            for ordinal in 0..MAX_SIM_EVENTS_PER_TICK as u16 {
                intents
                    .record(presentation_intent_at(tick, ordinal))
                    .unwrap();
            }
        }
        assert_eq!(intents.len(), intents.capacity());

        for ordinal in 0..MAX_SIM_EVENTS_PER_TICK as u16 {
            intents
                .record(presentation_intent_at(
                    SIM_EVENT_HISTORY_TICKS as u64,
                    ordinal,
                ))
                .unwrap();
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
    }

    #[test]
    fn render_stall_and_rollback_replay_item_events_exactly_once() {
        let mut journal = SimEventJournal::default();
        let mut intents = ItemPresentationIntentJournal::default();
        for tick in 20..23 {
            commit_item_presentation_event(&mut journal, &mut intents, tick);
        }

        let mut cursor = PresentationEventCursor::default();
        let mut router = PresentationEventRouter::default();
        let mut presented = Vec::new();
        cursor
            .route_available(&journal, &mut router, Some(SimTick(22)), |event| {
                if let Some(intent) = intents.get(event.id)
                    && item_presentation_matches_event(event, intent)
                {
                    presented.push(event.id);
                }
            })
            .unwrap();
        assert_eq!(presented.len(), 3, "a stalled render consumes every tick");

        let retained = SimTick(20);
        journal.discard_after(retained);
        cursor.discard_after(retained);
        router.discard_after(retained);
        intents.discard_after(retained);
        for tick in 21..23 {
            commit_item_presentation_event(&mut journal, &mut intents, tick);
        }
        cursor
            .route_available(&journal, &mut router, Some(SimTick(22)), |event| {
                if let Some(intent) = intents.get(event.id)
                    && item_presentation_matches_event(event, intent)
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
    fn committed_item_use_is_presented_only_from_update() {
        let mut app = App::new();
        app.insert_resource(EffectAssets::default())
            .insert_resource(HitEffects::default())
            .insert_resource(MatchAnnouncements::default())
            .insert_resource(SimEventJournal::default())
            .insert_resource(CombatPresentationIntentJournal::default())
            .insert_resource(ItemPresentationIntentJournal::default())
            .insert_resource(PresentationEventCursor::default())
            .insert_resource(PresentationEventRouter::default())
            .add_systems(Update, present_committed_combat_events);

        let fighter_entity = app
            .world_mut()
            .spawn((
                Fighter {
                    id: FighterId::ZERO.index(),
                    name: "Fixture",
                    color: Color::WHITE,
                    spawn: Vec3::ZERO,
                },
                FighterStats::default(),
                FighterMotor::default(),
                FighterActionState::default(),
            ))
            .id();
        let item = item_id(4, 1);
        let mut buffer = TickEventBuffer::new(SimTick(9));
        let event_id = buffer
            .emit(
                SimEventSource::Entity(item),
                SimEventKind::ItemLifecycle {
                    item,
                    fighter: Some(FighterId::ZERO),
                    event: ItemLifecycleEvent::Used,
                },
            )
            .unwrap();
        app.world_mut()
            .resource_mut::<SimEventJournal>()
            .commit(&buffer);
        app.world_mut()
            .resource_mut::<ItemPresentationIntentJournal>()
            .record(ItemPresentationIntent {
                event_id,
                item,
                item_kind: ItemKind::Apple,
                fighter: Some(FighterId::ZERO),
                fighter_name: Some("Fixture"),
                kind: ItemPresentationKind::Used {
                    position: Vec3::Y,
                    announcement: "ate Apple",
                },
            })
            .unwrap();

        assert_eq!(
            app.world()
                .get::<FighterStats>(fighter_entity)
                .unwrap()
                .hud_flash,
            0.0
        );
        app.update();
        assert_eq!(
            app.world()
                .get::<FighterStats>(fighter_entity)
                .unwrap()
                .hud_flash,
            0.28
        );
        assert_eq!(
            app.world().resource::<MatchAnnouncements>().message,
            "Fixture ate Apple"
        );
        assert_eq!(
            app.world().resource::<HitEffects>().last_cue.unwrap().id,
            "item_use"
        );
        assert_eq!(
            app.world()
                .resource::<PresentationEventRouter>()
                .metrics()
                .deduplicated_dispatched,
            1
        );

        app.update();
        assert_eq!(
            app.world()
                .resource::<PresentationEventRouter>()
                .metrics()
                .deduplicated_dispatched,
            1
        );
    }

    #[test]
    fn reward_cleanup_releases_slots_and_keeps_lowest_live_duplicate() {
        let mut app = App::new();
        app.insert_resource(SimulationIdentityAllocator::default())
            .add_systems(Update, update_items);
        let (_, source_id) = spawn_test_item(
            app.world_mut(),
            ArenaItem::new(ItemKind::Crate, Vec3::ZERO, 0.0),
        );
        let (expired_entity, expired_id) = spawn_test_item(
            app.world_mut(),
            ArenaItem::new_crate_reward(ItemKind::Apple, Vec3::X, 0.0, source_id),
        );
        let (retained_entity, retained_id) = spawn_test_item(
            app.world_mut(),
            ArenaItem::new_crate_reward(ItemKind::Turkey, Vec3::Z, 0.0, source_id),
        );
        app.world_mut()
            .get_mut::<ArenaItem>(expired_entity)
            .unwrap()
            .set_respawning();

        app.update();
        let identities = app.world().resource::<SimulationIdentityAllocator>();
        assert_eq!(identities.live_count(SimEntityKind::Item), 2);
        assert!(identities.mapped_entity(expired_id).is_none());
        assert_eq!(identities.mapped_entity(retained_id), Some(retained_entity));
        assert_eq!(
            identities.generation_at(SimEntityKind::Item, expired_id.index()),
            Some(expired_id.generation() + 1)
        );

        app.world_mut()
            .get_mut::<ArenaItem>(retained_entity)
            .unwrap()
            .set_respawning();
        app.update();
        assert_eq!(
            app.world()
                .resource::<SimulationIdentityAllocator>()
                .live_count(SimEntityKind::Item),
            1
        );
    }

    #[test]
    fn arena_reset_sorts_anchor_items_and_releases_rewards_and_excess() {
        let mut app = App::new();
        app.insert_resource(SimulationIdentityAllocator::default())
            .insert_resource(test_item_assets())
            .insert_resource(ActiveArena::new(1))
            .init_resource::<ResetTestCapture>()
            .add_systems(Update, reset_items_once);

        let (_, source_id) = spawn_test_item(
            app.world_mut(),
            ArenaItem::new(ItemKind::Crate, Vec3::ZERO, 0.0),
        );
        let _ = spawn_test_item(
            app.world_mut(),
            ArenaItem::new_crate_reward(ItemKind::Apple, Vec3::X, 0.0, source_id),
        );
        for index in 0..2 {
            let _ = spawn_test_item(
                app.world_mut(),
                ArenaItem::new(ItemKind::Apple, Vec3::splat(index as f32 + 1.0), 0.0),
            );
        }
        let _ = spawn_test_item(
            app.world_mut(),
            ArenaItem::new_crate_reward(ItemKind::Turkey, Vec3::Z, 0.0, source_id),
        );
        for index in 0..3 {
            let _ = spawn_test_item(
                app.world_mut(),
                ArenaItem::new(ItemKind::Mushroom, Vec3::splat(index as f32 + 4.0), 0.0),
            );
        }

        app.update();

        let arena = ActiveArena::new(1);
        let report = app.world().resource::<ResetTestCapture>().0.unwrap();
        assert_eq!(
            report,
            ItemArenaResetReport {
                retained_anchors: arena.definition().item_anchors.len(),
                spawned_anchors: 0,
                released_rewards: 2,
                released_excess: 2,
                rejected_anchors: 0,
            }
        );
        assert_eq!(
            app.world()
                .resource::<SimulationIdentityAllocator>()
                .live_count(SimEntityKind::Item) as usize,
            arena.definition().item_anchors.len()
        );

        let mut query = app.world_mut().query::<(&StableSimEntity, &ArenaItem)>();
        let mut retained = query.iter(app.world()).collect::<Vec<_>>();
        retained.sort_unstable_by_key(|(stable, _)| stable.id());
        for ((_, item), anchor) in retained.iter().zip(arena.definition().item_anchors) {
            assert_eq!(item.kind, anchor.kind);
            assert_eq!(item.position, anchor.position);
            assert_eq!(item.crate_source(), None);
        }
    }

    #[test]
    fn repeated_render_updates_cannot_change_item_snapshot_or_gameplay_position() {
        let mut app = App::new();
        app.insert_resource(test_item_assets())
            .add_systems(Update, sync_item_visuals);

        let id = item_id(3, 2);
        let gameplay_position = Vec3::new(1.25, 0.5, -2.0);
        let item = ArenaItem::new(ItemKind::Apple, gameplay_position, 0.25);
        let entity = app
            .world_mut()
            .spawn((
                StableSimEntity::new(id),
                item,
                Transform::from_translation(Vec3::splat(999.0))
                    .with_rotation(Quat::from_rotation_x(1.7))
                    .with_scale(Vec3::splat(40.0)),
                Visibility::Hidden,
                MeshMaterial3d::<StandardMaterial>(Handle::default()),
            ))
            .id();

        let expected = LiveItemSnapshotCodec
            .capture(app.world(), entity, id)
            .unwrap();
        for index in 0..8 {
            *app.world_mut().get_mut::<Transform>(entity).unwrap() =
                Transform::from_translation(Vec3::new(index as f32 * 10_000.0, -500.0, 700.0))
                    .with_rotation(Quat::from_rotation_z(index as f32 * 0.37))
                    .with_scale(Vec3::new(0.01, 99.0, 3.0));
            app.update();
            assert_eq!(
                LiveItemSnapshotCodec
                    .capture(app.world(), entity, id)
                    .unwrap(),
                expected
            );
            assert_eq!(
                app.world().get::<ArenaItem>(entity).unwrap().position,
                gameplay_position
            );
        }
    }

    #[test]
    fn item_motion_and_throw_collision_ignore_render_pose() {
        let mut first = ArenaItem::new(ItemKind::Apple, Vec3::new(1.0, 2.0, 3.0), 0.0);
        let mut second = ArenaItem::new(ItemKind::Apple, Vec3::new(1.0, 2.0, 3.0), 0.0);
        first.velocity = Vec3::new(6.0, -1.5, 2.25);
        second.velocity = first.velocity;
        let mut first_render = Transform::from_translation(Vec3::splat(-50_000.0));
        let mut second_render = Transform::from_translation(Vec3::splat(50_000.0));

        for step in 0..12 {
            first_render.rotation = Quat::from_rotation_x(step as f32);
            second_render.scale = Vec3::splat(step as f32 + 1.0);
            integrate_item_position(&mut first, ITEM_FIXED_DELTA);
            integrate_item_position(&mut second, ITEM_FIXED_DELTA);
        }

        assert_eq!(first.position, second.position);
        let hurt_center = first.position + Vec3::X * (ITEM_THROW_RADIUS + FIGHTER_RADIUS - 0.01);
        assert!(thrown_item_overlaps_fighter(
            first.position,
            hurt_center,
            FIGHTER_RADIUS,
        ));
        assert_eq!(
            thrown_item_overlaps_fighter(first.position, hurt_center, FIGHTER_RADIUS),
            thrown_item_overlaps_fighter(second.position, hurt_center, FIGHTER_RADIUS)
        );
        assert_ne!(first_render, second_render);
    }

    #[test]
    fn item_state_transitions_reset_transient_state() {
        let holder = fighter(1);
        let owner = fighter(2);
        let mut item = ArenaItem::new(ItemKind::Apple, Vec3::new(1.0, 0.5, 0.0), 0.0);
        item.velocity = Vec3::new(9.0, 0.0, 0.0);
        item.already_hit.insert(fighter(3));
        item.pickup_lockout = TickTimer::from_seconds_ceil(0.4);

        item.pickup_as(holder);
        assert_eq!(item.state, ItemState::Held { holder });
        assert_eq!(item.velocity, Vec3::ZERO);
        assert!(item.already_hit.is_empty());
        assert_eq!(item.pickup_lockout, TickTimer::ZERO);

        item.already_hit.insert(fighter(0));
        item.launch_as_thrown(owner, Vec3::new(3.0, 1.0, 0.0));
        assert_eq!(item.velocity, Vec3::new(3.0, 1.0, 0.0));
        assert!(item.already_hit.is_empty());
        assert_eq!(
            item.pickup_lockout,
            TickTimer::from_seconds_ceil(item.kind.pickup_lockout())
        );
        assert_eq!(
            item.state,
            ItemState::Thrown {
                owner,
                lifetime: TickTimer::from_seconds_ceil(ITEM_THROW_LIFETIME),
                grace: TickTimer::from_seconds_ceil(ITEM_MALLET_THROW_GRACE),
            }
        );
    }

    #[test]
    fn barrel_activation_consumes_once_and_enters_four_second_spray() {
        let owner = fighter(2);
        let mut item = ArenaItem::new(ItemKind::Barrel, Vec3::ZERO, 0.0);
        item.velocity = Vec3::new(4.0, -1.0, 1.0);
        item.durability -= 1;
        item.start_barrel_spray(owner);

        assert_eq!(item.durability, 2);
        assert!(matches!(
            item.state,
            ItemState::Spraying {
                owner: active_owner,
                lifetime,
                spray_timer: TickTimer::ZERO,
                ..
            } if active_owner == owner
                && lifetime == TickTimer::from_seconds_ceil(BARREL_SPRAY_DURATION)
        ));
        assert_eq!(item.velocity.y, 0.0);
    }

    #[test]
    fn barrel_spray_cadence_fires_immediately_then_every_quarter_second() {
        let mut timer = TickTimer::ZERO;
        assert!(advance_barrel_spray_timer(&mut timer));
        let cadence = TickTimer::from_seconds_ceil(BARREL_SPRAY_CADENCE).remaining();
        assert_eq!(timer.remaining(), cadence);
        for _ in 1..cadence {
            assert!(!advance_barrel_spray_timer(&mut timer));
        }
        assert!(advance_barrel_spray_timer(&mut timer));
        assert_eq!(timer.remaining(), cadence);
    }

    #[test]
    fn armed_bomb_uses_bomb_tuning() {
        let owner = fighter(3);
        let mut item = ArenaItem::new(ItemKind::Steamer, Vec3::ZERO, 0.0);

        item.arm_as_bomb(owner, Vec3::new(1.0, 2.0, 0.0));

        assert_eq!(item.velocity, Vec3::new(1.0, 2.0, 0.0));
        assert_eq!(
            item.pickup_lockout,
            TickTimer::from_seconds_ceil(ITEM_BOMB_PICKUP_LOCKOUT)
        );
        assert_eq!(
            item.state,
            ItemState::Armed {
                owner,
                timer: TickTimer::from_seconds_ceil(POP_BOMB_FUSE),
                grace: TickTimer::from_seconds_ceil(ITEM_BOMB_THROW_GRACE),
            }
        );
    }

    #[test]
    fn stale_held_reference_detects_missing_or_wrong_holder() {
        let holder = fighter(1);
        let other = fighter(2);
        let mut item = ArenaItem::new(ItemKind::Apple, Vec3::ZERO, 0.0);

        assert!(held_reference_is_stale(None, holder));
        assert!(held_reference_is_stale(Some(&item), holder));

        item.pickup_as(holder);
        assert!(!held_reference_is_stale(Some(&item), holder));
        assert!(held_reference_is_stale(Some(&item), other));
    }

    #[test]
    fn item_durations_have_exact_sixty_hz_boundaries() {
        assert_eq!(
            TickTimer::from_seconds_ceil(ITEM_THROW_LIFETIME).remaining(),
            132
        );
        assert_eq!(
            TickTimer::from_seconds_ceil(ITEM_MALLET_THROW_GRACE).remaining(),
            10
        );
        assert_eq!(
            TickTimer::from_seconds_ceil(ITEM_BOMB_THROW_GRACE).remaining(),
            12
        );
        assert_eq!(TickTimer::from_seconds_ceil(POP_BOMB_FUSE).remaining(), 42);
        assert_eq!(
            TickTimer::from_seconds_ceil(ITEM_DROP_ROLL_LIFETIME).remaining(),
            54
        );
        assert_eq!(
            TickTimer::from_seconds_ceil(ITEM_RESPAWN_SECONDS).remaining(),
            600
        );
        assert_eq!(
            TickTimer::from_seconds_ceil(BARREL_SPRAY_DURATION).remaining(),
            240
        );
        assert_eq!(
            TickTimer::from_seconds_ceil(BARREL_SPRAY_CADENCE).remaining(),
            15
        );

        let mut item = ArenaItem::new(ItemKind::Apple, Vec3::ZERO, 0.0);
        item.deactivate_for_match();
        for _ in 0..10_000 {
            assert!(!item.respawn_timer.tick());
        }
        assert_eq!(item.respawn_timer, TickTimer::INDEFINITE);
    }

    #[test]
    fn item_hit_history_is_a_fixed_fighter_mask() {
        let mut item = ArenaItem::new(ItemKind::Apple, Vec3::ZERO, 0.0);
        assert!(item.already_hit.insert(fighter(3)));
        assert!(!item.already_hit.insert(fighter(3)));
        assert!(item.already_hit.contains(fighter(3)));
        assert_eq!(item.already_hit.len(), 1);
        item.reset_for_match();
        assert!(item.already_hit.is_empty());
    }

    #[test]
    fn keyed_item_randomness_is_replay_stable_and_purpose_isolated() {
        let id = item_id(17, 4);
        let tick = SimTick(8_192);
        let first = dropped_item_roll_velocity(Vec3::Z, 99, tick, id, fighter(2));
        let replay = dropped_item_roll_velocity(Vec3::Z, 99, tick, id, fighter(2));
        assert_eq!(first, replay);
        assert_ne!(
            item_event_rng(99, tick, id, fighter(2), "items/drop").next_u64(),
            item_event_rng(99, tick, id, fighter(2), "items/rewards").next_u64()
        );
        assert_ne!(
            item_event_rng(99, tick, id, fighter(2), "items/drop").next_u64(),
            item_event_rng(100, tick, id, fighter(2), "items/drop").next_u64()
        );
    }

    #[test]
    fn per_world_arena_bounds_drive_item_ringout() {
        let first = ActiveArena::new(0);
        let second = ActiveArena::new(1);
        let low_y = first
            .definition()
            .ringout_y
            .min(second.definition().ringout_y)
            - 0.01;
        assert!(should_respawn_item(
            Vec3::new(0.0, low_y, 0.0),
            first.definition()
        ));
        assert!(should_respawn_item(
            Vec3::new(0.0, low_y, 0.0),
            second.definition()
        ));
    }

    #[test]
    fn expanded_items_have_distinct_roles() {
        assert_eq!(ItemKind::Steamer.role(), ItemRole::Explosive);
        assert_eq!(ItemKind::Crate.role(), ItemRole::Utility);
        assert_eq!(ItemKind::CupCoffee.role(), ItemRole::Utility);
        assert_eq!(ItemKind::Apple.role(), ItemRole::Recovery);
        assert_eq!(ItemKind::Mushroom.role(), ItemRole::Utility);
        assert!(item_swing_config(ItemKind::Apple).is_none());
        let barrel_throw = item_throw_profile(ItemKind::Barrel, 0);
        assert!(barrel_throw.knockback > 0.0);
        assert_eq!(
            barrel_throw.payload_id,
            Some(AttackPayloadId::ItemThrowHeavy)
        );
        assert_eq!(barrel_throw.shape_id, Some(AttackShapeId::ItemLob));
        assert_eq!(
            ArenaItem::new(ItemKind::Turkey, Vec3::ZERO, 0.0).max_durability,
            3
        );
        assert_eq!(
            ArenaItem::new(ItemKind::Barrel, Vec3::ZERO, 0.0).max_durability,
            3
        );
        assert_ne!(
            mystery_crate_reward(1, SimTick(5), item_id(2, 1)),
            ItemKind::Crate
        );
    }

    #[test]
    fn white_wine_restores_one_ultimate_cost_and_caps_mp() {
        assert_eq!(
            ITEM_WINE_WHITE_STAMINA,
            crate::constants::ULTIMATE_STAMINA_COST
        );

        let near_full_stamina = MAX_STAMINA - 1.0;
        assert_eq!(
            (near_full_stamina + ITEM_WINE_WHITE_STAMINA).min(MAX_STAMINA),
            MAX_STAMINA
        );
    }

    #[test]
    fn item_role_priorities_keep_recovery_and_utility_distinct() {
        assert!(ItemKind::Steamer.bot_pickup_priority() > ItemKind::Apple.bot_pickup_priority());
        assert!(ItemKind::CupCoffee.bot_pickup_priority() > ItemKind::Apple.bot_pickup_priority());
        assert_eq!(ItemKind::Turkey.max_durability(), 3);
    }

    #[test]
    fn forced_item_drop_sfx_only_targets_visible_combat_drops() {
        assert!(forced_item_drop_action(FighterAction::Knockdown));
        assert!(forced_item_drop_action(FighterAction::Grabbed));
        assert!(forced_item_drop_action(FighterAction::GuardBroken));
        assert!(forced_item_drop_action(FighterAction::RingOut));
        assert!(forced_item_drop_action(FighterAction::Respawning));
        assert!(visible_forced_item_drop_action(FighterAction::Knockdown));
        assert!(!visible_forced_item_drop_action(FighterAction::RingOut));
        assert!(!forced_item_drop_action(FighterAction::Idle));

        assert_eq!(
            item_drop_sfx_cue(Vec3::X),
            CombatSfxCue::new(CombatSfxKind::ItemDrop, Vec3::X, ITEM_DROP_SFX_PRIORITY)
        );
    }

    #[test]
    fn item_specific_sfx_helpers_route_mushroom_and_steamer() {
        assert_eq!(
            item_use_sfx_cue(ItemKind::Mushroom, Vec3::Y),
            Some(CombatSfxCue::new(
                CombatSfxKind::MushroomBigger,
                Vec3::Y,
                MUSHROOM_BIGGER_SFX_PRIORITY,
            ))
        );
        assert_eq!(item_use_sfx_cue(ItemKind::Apple, Vec3::Y), None);
        assert_eq!(
            steamer_explosion_sfx_cue(Vec3::Z),
            CombatSfxCue::new(
                CombatSfxKind::SteamerExplosion,
                Vec3::Z,
                STEAMER_EXPLOSION_SFX_PRIORITY,
            )
        );
    }

    #[test]
    fn portable_pickup_is_blocked_by_active_dash_and_dash_slide() {
        let idle_motor = FighterMotor::default();
        assert!(!portable_pickup_blocked(FighterAction::Idle, &idle_motor));
        assert!(portable_pickup_blocked(FighterAction::Dashing, &idle_motor));

        let sliding_motor = FighterMotor {
            dash_slide_timer: TickTimer::from_seconds_ceil(0.1),
            ..default()
        };
        assert!(portable_pickup_blocked(FighterAction::Idle, &sliding_motor));
    }

    #[test]
    fn held_item_inputs_are_sanitized_to_item_only_commands() {
        let mut input = FighterInput {
            movement: Vec2::new(0.5, -0.25),
            aim: true,
            jump: true,
            dash: true,
            light: true,
            light_held: true,
            heavy: false,
            heavy_held: true,
            heavy_released: true,
            grab: true,
            guard: true,
            ultimate: true,
            special: true,
            ..default()
        };

        sanitize_held_item_inputs(&mut input);

        assert_eq!(input.movement, Vec2::new(0.5, -0.25));
        assert!(input.aim);
        assert!(input.jump);
        assert!(input.dash);
        assert!(!input.grab);
        assert!(input.guard);
        assert!(!input.ultimate);
        assert!(!input.special);
        assert!(!input.light);
        assert!(!input.light_held);
        assert!(!input.heavy_held);
        assert!(!input.heavy_released);
        assert!(!input.heavy);
    }

    #[test]
    fn held_item_command_routes_pop_bomb_inputs_to_throw() {
        assert_eq!(
            held_item_command(
                &FighterInput {
                    light: true,
                    ..default()
                },
                ItemKind::Steamer
            ),
            HeldItemCommand::Throw
        );

        assert_eq!(
            held_item_command(
                &FighterInput {
                    heavy: true,
                    ..default()
                },
                ItemKind::Steamer
            ),
            HeldItemCommand::Throw
        );

        assert_eq!(
            held_item_command(
                &FighterInput {
                    light: true,
                    heavy: true,
                    ..default()
                },
                ItemKind::Apple
            ),
            HeldItemCommand::Throw
        );
    }

    #[test]
    fn guard_and_grab_are_not_used_to_drop_item_or_block_guard() {
        let mut input = FighterInput {
            guard: true,
            grab: true,
            ..default()
        };

        assert_eq!(
            held_item_command(&input, ItemKind::Apple),
            HeldItemCommand::None
        );
        assert_eq!(
            held_item_command(&input, ItemKind::Steamer),
            HeldItemCommand::None
        );

        sanitize_held_item_inputs(&mut input);
        assert!(input.guard);
        assert!(!input.grab);
        assert!(!input.special);
        assert!(!input.ultimate);
    }

    #[test]
    fn held_item_inputs_prevent_skill_routing_controls() {
        let mut input = FighterInput {
            movement: Vec2::new(-0.75, 0.2),
            aim: true,
            jump: true,
            dash: true,
            special: true,
            ultimate: true,
            light: true,
            ..default()
        };

        let command = held_item_command(&input, ItemKind::Apple);
        sanitize_held_item_inputs(&mut input);

        assert_eq!(command, HeldItemCommand::Use);
        assert_eq!(input.movement, Vec2::new(-0.75, 0.2));
        assert!(input.aim);
        assert!(input.jump);
        assert!(input.dash);
        assert!(!input.guard);
        assert!(!input.grab);
        assert!(!input.special);
        assert!(!input.ultimate);
        assert!(!input.light);
    }
}
