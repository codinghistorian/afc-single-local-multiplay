//! Bounded, canonical contact collection and arbitration primitives.
//!
//! Geometry producers write value-only records into [`ContactBuffer`].  The
//! buffer freezes the complete contact set before authoritative fighter state
//! is mutated, rejects overflow deterministically, and exposes one canonical
//! resolution order.  Source-specific systems consume the recorded outcomes
//! only after resolution.

use std::cmp::Ordering;

use bevy::prelude::*;

use crate::combat::ImpactProfile;
use crate::determinism::{
    DEFAULT_F32_QUANTIZATION, FighterId, SimEntityId, dequantize_f32, quantize_f32,
};
use crate::sim_event::SimEventId;

/// The maximum number of contacts possible when every damage-capable dynamic
/// pool overlaps all four fighters, plus a fixed allowance for authored static
/// arena hazards.
///
/// Penguin surfaces are excluded because they are traversal surfaces rather
/// than damage sources.  The constants mirror `SIM_ENTITY_POOL_CAPACITIES` and
/// deliberately remain a compile-time protocol budget.
pub const MAX_CONTACTS_PER_TICK: usize =
    (32 + 16 + 24 + 128 + 96 + 32 + 4) * FighterId::ALL.len() + 64;

/// Source-class ordering used only when equal-priority reactions target the
/// same fighter. Lower ranks resolve first, leaving the later (stronger tie
/// winner) reaction as the fighter's final motion/action state.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ContactSourceKind {
    FighterStrike = 0,
    ItemMeleeOrThrow = 1,
    CharacterAbility = 2,
    GenericSpecial = 3,
    ArenaOrdnance = 4,
    PersistentArenaHazard = 5,
}

/// Collision-free canonical identity for either a rollback-owned dynamic
/// source or one immutable hazard authored into an arena definition.
///
/// Static hazards deliberately do not impersonate generational entities: they
/// have no allocator slot to validate or recycle. The resolver validates both
/// indices against the active arena before accepting their contacts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ContactSourceId {
    Entity(SimEntityId),
    ArenaHazard { arena_index: u16, hazard_index: u16 },
}

impl ContactSourceId {
    pub const fn entity(self) -> Option<SimEntityId> {
        match self {
            Self::Entity(entity) => Some(entity),
            Self::ArenaHazard { .. } => None,
        }
    }

    pub const fn arena_hazard(self) -> Option<(u16, u16)> {
        match self {
            Self::Entity(_) => None,
            Self::ArenaHazard {
                arena_index,
                hazard_index,
            } => Some((arena_index, hazard_index)),
        }
    }
}

impl From<SimEntityId> for ContactSourceId {
    fn from(value: SimEntityId) -> Self {
        Self::Entity(value)
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ContactPhase {
    /// An ordinary damaging contact.
    Strike = 0,
    /// A damaging cinematic catch whose relationship is claimed after all
    /// frozen damage has resolved and only when its impact was unguarded.
    CinematicCatch = 1,
    /// A non-damaging ordinary grab relationship claim.
    Grab = 2,
    /// A geometry-only status application resolved by its source consumer.
    /// Status records never participate in damage/reaction or claim conflicts.
    Status = 3,
}

impl ContactPhase {
    pub const fn has_impact(self) -> bool {
        matches!(self, Self::Strike | Self::CinematicCatch)
    }

    pub const fn is_claim(self) -> bool {
        matches!(self, Self::CinematicCatch | Self::Grab)
    }
}

/// Canonical flags sampled at collection time. They describe authored
/// source behavior and never depend on resolution-time ECS state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ContactFlags(u8);

impl ContactFlags {
    pub const SINGLE_USE_CLAIM: u8 = 1 << 0;
    pub const LOCKED_FINAL_VICTIM: u8 = 1 << 1;
    pub const LOCKED_SCRATCH_VICTIM: u8 = 1 << 2;
    pub const JUMP_ATTACK: u8 = 1 << 3;

    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    pub const fn contains(self, flag: u8) -> bool {
        self.0 & flag != 0
    }

    pub const fn bits(self) -> u8 {
        self.0
    }
}

/// A point stored in the same fixed-point grid used by canonical snapshots.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct QuantizedContactPoint([i32; 3]);

impl QuantizedContactPoint {
    pub fn from_vec3(value: Vec3) -> Self {
        Self([
            quantize_f32(value.x, DEFAULT_F32_QUANTIZATION),
            quantize_f32(value.y, DEFAULT_F32_QUANTIZATION),
            quantize_f32(value.z, DEFAULT_F32_QUANTIZATION),
        ])
    }

    pub fn to_vec3(self) -> Vec3 {
        Vec3::new(
            dequantize_f32(self.0[0], DEFAULT_F32_QUANTIZATION),
            dequantize_f32(self.0[1], DEFAULT_F32_QUANTIZATION),
            dequantize_f32(self.0[2], DEFAULT_F32_QUANTIZATION),
        )
    }

    pub const fn components(self) -> [i32; 3] {
        self.0
    }
}

/// One geometrically valid contact sampled before any impact in the batch.
///
/// `payload_id` and `shape_id` are stable semantic discriminants. `u16::MAX`
/// denotes an absent optional payload. Impact contacts embed a value-only
/// profile applied using the quantized points stored alongside it. Status
/// contacts carry no inert/dummy impact profile.
#[derive(Clone, Copy)]
pub struct ContactRecord {
    pub phase: ContactPhase,
    pub source_kind: ContactSourceKind,
    pub source: ContactSourceId,
    pub owner: Option<FighterId>,
    pub target: FighterId,
    pub payload_id: u16,
    pub shape_id: u16,
    pub reaction_priority: u16,
    pub contact_ordinal: u8,
    pub contact_point: QuantizedContactPoint,
    pub origin: QuantizedContactPoint,
    pub impact: Option<ImpactProfile>,
    pub flags: ContactFlags,
}

impl ContactRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        phase: ContactPhase,
        source_kind: ContactSourceKind,
        source: impl Into<ContactSourceId>,
        owner: Option<FighterId>,
        target: FighterId,
        payload_id: u16,
        shape_id: u16,
        contact_ordinal: u8,
        contact_point: Vec3,
        origin: Vec3,
        impact: ImpactProfile,
        flags: ContactFlags,
    ) -> Self {
        Self {
            phase,
            source_kind,
            source: source.into(),
            owner,
            target,
            payload_id,
            shape_id,
            reaction_priority: u16::from(impact.reaction.priority_bonus),
            contact_ordinal,
            contact_point: QuantizedContactPoint::from_vec3(contact_point),
            origin: QuantizedContactPoint::from_vec3(origin),
            impact: Some(impact),
            flags,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_status(
        source_kind: ContactSourceKind,
        source: impl Into<ContactSourceId>,
        owner: Option<FighterId>,
        target: FighterId,
        payload_id: u16,
        shape_id: u16,
        contact_ordinal: u8,
        contact_point: Vec3,
        origin: Vec3,
        flags: ContactFlags,
    ) -> Self {
        Self {
            phase: ContactPhase::Status,
            source_kind,
            source: source.into(),
            owner,
            target,
            payload_id,
            shape_id,
            reaction_priority: 0,
            contact_ordinal,
            contact_point: QuantizedContactPoint::from_vec3(contact_point),
            origin: QuantizedContactPoint::from_vec3(origin),
            impact: None,
            flags,
        }
    }

    pub const fn identity_key(self) -> (ContactSourceId, FighterId, u8) {
        (self.source, self.target, self.contact_ordinal)
    }

    pub fn has_authored_damage(self) -> bool {
        self.phase.has_impact()
            && self
                .impact
                .is_some_and(|impact| impact.damage > 0.0 || impact.power > 0.0)
    }

    fn same_semantic_payload(self, other: Self) -> bool {
        self.phase == other.phase
            && self.source_kind == other.source_kind
            && self.owner == other.owner
            && self.payload_id == other.payload_id
            && self.shape_id == other.shape_id
            && self.reaction_priority == other.reaction_priority
            && self.contact_point == other.contact_point
            && self.origin == other.origin
            && self.flags == other.flags
            && optional_impact_profiles_canonically_equal(self.impact, other.impact)
    }
}

fn optional_impact_profiles_canonically_equal(
    left: Option<ImpactProfile>,
    right: Option<ImpactProfile>,
) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => impact_profiles_canonically_equal(left, right),
        _ => false,
    }
}

fn canonical_float_equal(left: f32, right: f32) -> bool {
    quantize_f32(left, DEFAULT_F32_QUANTIZATION) == quantize_f32(right, DEFAULT_F32_QUANTIZATION)
}

fn canonical_vec3_equal(left: Option<Vec3>, right: Option<Vec3>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            QuantizedContactPoint::from_vec3(left) == QuantizedContactPoint::from_vec3(right)
        }
        _ => false,
    }
}

fn impact_profiles_canonically_equal(left: ImpactProfile, right: ImpactProfile) -> bool {
    let left_feedback = left.feedback;
    let right_feedback = right.feedback;
    let left_reaction = left.reaction;
    let right_reaction = right.reaction;
    let aftermath_equal = match (
        left_reaction.landing_aftermath,
        right_reaction.landing_aftermath,
    ) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.family == right.family
                && left.getup_transition_ms == right.getup_transition_ms
                && left.recover_ms == right.recover_ms
                && left.landing_stick_ms == right.landing_stick_ms
                && canonical_float_equal(left.horizontal_damping, right.horizontal_damping)
                && left.cue == right.cue
        }
        _ => false,
    };

    left.owner_id == right.owner_id
        && left.source == right.source
        && left.payload_id == right.payload_id
        && left.attacker_character == right.attacker_character
        && left.technique_id == right.technique_id
        && left.hit_effect == right.hit_effect
        && left.hit_effects_enabled == right.hit_effects_enabled
        && left.shape_id == right.shape_id
        && canonical_vec3_equal(left.knockback_direction, right.knockback_direction)
        && left.reaction_family == right.reaction_family
        && left.damage_profile == right.damage_profile
        && left.element == right.element
        && left.attacker_equipment == right.attacker_equipment
        && left.attacker_style == right.attacker_style
        && canonical_float_equal(left.power, right.power)
        && canonical_float_equal(left.str_scale, right.str_scale)
        && canonical_float_equal(left.damage, right.damage)
        && canonical_float_equal(left.knockback, right.knockback)
        && canonical_float_equal(left.vertical_knockback, right.vertical_knockback)
        && left.force_knockdown == right.force_knockdown
        && left.guardable == right.guardable
        && canonical_float_equal(left.guard_stamina_damage, right.guard_stamina_damage)
        && left_feedback.cue == right_feedback.cue
        && left_feedback.heavy_spark == right_feedback.heavy_spark
        && canonical_float_equal(left_feedback.spark_scale, right_feedback.spark_scale)
        && canonical_float_equal(left_feedback.hitstop, right_feedback.hitstop)
        && canonical_float_equal(left_feedback.guard_hitstop, right_feedback.guard_hitstop)
        && canonical_float_equal(left_feedback.shake, right_feedback.shake)
        && canonical_float_equal(left_feedback.guard_shake, right_feedback.guard_shake)
        && canonical_float_equal(left_feedback.hit_hud_flash, right_feedback.hit_hud_flash)
        && canonical_float_equal(
            left_feedback.guard_hud_flash,
            right_feedback.guard_hud_flash,
        )
        && left_feedback.priority == right_feedback.priority
        && left_reaction.id == right_reaction.id
        && left_reaction.kind == right_reaction.kind
        && canonical_float_equal(
            left_reaction.horizontal_scale,
            right_reaction.horizontal_scale,
        )
        && canonical_float_equal(left_reaction.vertical_scale, right_reaction.vertical_scale)
        && left_reaction.airborne == right_reaction.airborne
        && left_reaction.immediate_down == right_reaction.immediate_down
        && left_reaction.hitstun_recover_ms == right_reaction.hitstun_recover_ms
        && left_reaction.grounded_getup_ms == right_reaction.grounded_getup_ms
        && left_reaction.grounded_recover_ms == right_reaction.grounded_recover_ms
        && left_reaction.grounded_stick_ms == right_reaction.grounded_stick_ms
        && aftermath_equal
        && left_reaction.cue == right_reaction.cue
        && left_reaction.priority_bonus == right_reaction.priority_bonus
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ContactOutcomeKind {
    #[default]
    Pending,
    Accepted,
    Guarded,
    RejectedByConflict,
    Duplicate,
    Invalidated,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ContactOutcome {
    pub kind: ContactOutcomeKind,
    /// Semantic event emitted for this accepted impact, if the bounded event
    /// journal had capacity. Source-specific presentation consumers use this
    /// exact ID instead of reconstructing event order.
    pub event_id: Option<SimEventId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContactInsertResult {
    Inserted,
    Duplicate,
    ConflictingDuplicate,
    RejectedByOverflow,
    ReplacedOverflowRecord,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ContactBufferMetrics {
    pub overflow_this_tick: u32,
    pub overflow_total: u64,
    pub duplicates_this_tick: u32,
    pub conflicting_duplicates_this_tick: u32,
    pub invalidated_this_tick: u32,
}

/// One fixed-capacity contact batch. Heap storage is allocated only when the
/// resource is constructed; beginning, collecting, sorting, and resolving a
/// tick perform no allocation.
#[derive(Resource)]
pub struct ContactBuffer {
    records: Box<[Option<ContactRecord>]>,
    outcomes: Box<[ContactOutcome]>,
    len: usize,
    metrics: ContactBufferMetrics,
}

impl Default for ContactBuffer {
    fn default() -> Self {
        Self::with_capacity(MAX_CONTACTS_PER_TICK)
    }
}

impl ContactBuffer {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            records: vec![None; capacity].into_boxed_slice(),
            outcomes: vec![ContactOutcome::default(); capacity].into_boxed_slice(),
            len: 0,
            metrics: ContactBufferMetrics::default(),
        }
    }

    /// Clears transient records and per-tick counters while retaining the
    /// cumulative canonical overflow counter.
    pub fn begin_tick(&mut self) {
        for index in 0..self.len {
            self.records[index] = None;
            self.outcomes[index] = ContactOutcome::default();
        }
        self.len = 0;
        self.metrics.overflow_this_tick = 0;
        self.metrics.duplicates_this_tick = 0;
        self.metrics.conflicting_duplicates_this_tick = 0;
        self.metrics.invalidated_this_tick = 0;
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn capacity(&self) -> usize {
        self.records.len()
    }

    pub const fn metrics(&self) -> ContactBufferMetrics {
        self.metrics
    }

    pub fn record(&self, index: usize) -> Option<ContactRecord> {
        (index < self.len).then(|| self.records[index]).flatten()
    }

    pub fn outcome(&self, index: usize) -> Option<ContactOutcome> {
        (index < self.len).then_some(self.outcomes[index])
    }

    pub fn mark_outcome(&mut self, index: usize, kind: ContactOutcomeKind) {
        if index >= self.len {
            return;
        }
        self.outcomes[index].kind = kind;
        if kind == ContactOutcomeKind::Invalidated {
            self.metrics.invalidated_this_tick =
                self.metrics.invalidated_this_tick.saturating_add(1);
        }
    }

    pub fn mark_event_id(&mut self, index: usize, event_id: SimEventId) {
        if index < self.len {
            self.outcomes[index].event_id = Some(event_id);
        }
    }

    /// Inserts one record without growing. Duplicates are detected before the
    /// capacity rule. At capacity, the retained set is the same for every input
    /// permutation: retain greater priority, then the lower canonical key.
    pub fn push(&mut self, record: ContactRecord) -> ContactInsertResult {
        for index in 0..self.len {
            let existing = self.records[index].expect("packed contact buffer");
            if existing.identity_key() != record.identity_key() {
                continue;
            }
            if existing.same_semantic_payload(record) {
                self.metrics.duplicates_this_tick =
                    self.metrics.duplicates_this_tick.saturating_add(1);
                return ContactInsertResult::Duplicate;
            }
            self.metrics.conflicting_duplicates_this_tick = self
                .metrics
                .conflicting_duplicates_this_tick
                .saturating_add(1);
            return ContactInsertResult::ConflictingDuplicate;
        }

        if self.len < self.capacity() {
            self.records[self.len] = Some(record);
            self.outcomes[self.len] = ContactOutcome::default();
            self.len += 1;
            return ContactInsertResult::Inserted;
        }

        self.metrics.overflow_this_tick = self.metrics.overflow_this_tick.saturating_add(1);
        self.metrics.overflow_total = self.metrics.overflow_total.saturating_add(1);
        let mut worst = 0;
        for index in 1..self.len {
            let candidate = self.records[index].expect("packed contact buffer");
            let incumbent = self.records[worst].expect("packed contact buffer");
            if retention_cmp(candidate, incumbent) == Ordering::Less {
                worst = index;
            }
        }
        let worst_record = self.records[worst].expect("packed contact buffer");
        if retention_cmp(record, worst_record) == Ordering::Greater {
            self.records[worst] = Some(record);
            self.outcomes[worst] = ContactOutcome::default();
            ContactInsertResult::ReplacedOverflowRecord
        } else {
            ContactInsertResult::RejectedByOverflow
        }
    }

    /// In-place insertion sort over the bounded batch. This performs no
    /// allocation and is intentionally simple because ordinary authored ticks
    /// contain only a small fraction of the theoretical capacity.
    pub fn sort_for_resolution(&mut self) {
        for index in 1..self.len {
            let record = self.records[index].take().expect("packed contact buffer");
            let outcome = self.outcomes[index];
            let mut insertion = index;
            while insertion > 0 {
                let previous = self.records[insertion - 1].expect("packed contact buffer");
                if resolution_cmp(previous, record) != Ordering::Greater {
                    break;
                }
                self.records[insertion] = Some(previous);
                self.outcomes[insertion] = self.outcomes[insertion - 1];
                insertion -= 1;
            }
            self.records[insertion] = Some(record);
            self.outcomes[insertion] = outcome;
        }
    }

    /// Fighters participating in an accepted ordinary damaging strike. Catch
    /// impacts are intentionally excluded so a catch does not interrupt its
    /// own relationship claim.
    pub fn ordinary_damage_participants(&self) -> [bool; FighterId::ALL.len()] {
        let mut participants = [false; FighterId::ALL.len()];
        for index in 0..self.len {
            let record = self.records[index].expect("packed contact buffer");
            if record.phase != ContactPhase::Strike || !record.has_authored_damage() {
                continue;
            }
            participants[record.target.index()] = true;
            if let Some(owner) = record.owner {
                participants[owner.index()] = true;
            }
        }
        participants
    }
}

fn owner_key(owner: Option<FighterId>) -> u8 {
    owner.map_or(u8::MAX, FighterId::get)
}

fn canonical_tie_key(
    record: ContactRecord,
) -> (
    u8,
    u8,
    u16,
    u16,
    u8,
    ContactSourceId,
    FighterId,
    QuantizedContactPoint,
) {
    (
        record.source_kind as u8,
        owner_key(record.owner),
        record.payload_id,
        record.shape_id,
        record.contact_ordinal,
        record.source,
        record.target,
        record.contact_point,
    )
}

/// Greater means more desirable to retain during overflow.
fn retention_cmp(left: ContactRecord, right: ContactRecord) -> Ordering {
    left.reaction_priority
        .cmp(&right.reaction_priority)
        // At equal priority, the lexicographically earlier canonical record is
        // retained and the canonical "newest" (largest key) is rejected.
        .then_with(|| canonical_tie_key(right).cmp(&canonical_tie_key(left)))
}

fn resolution_cmp(left: ContactRecord, right: ContactRecord) -> Ordering {
    let left_category = u8::from(!left.phase.has_impact());
    let right_category = u8::from(!right.phase.has_impact());
    left_category.cmp(&right_category).then_with(|| {
        if left.phase.has_impact() && right.phase.has_impact() {
            left.target
                .cmp(&right.target)
                .then_with(|| left.reaction_priority.cmp(&right.reaction_priority))
                .then_with(|| canonical_tie_key(left).cmp(&canonical_tie_key(right)))
        } else {
            let left_claim_rank = u8::from(left.phase == ContactPhase::Grab);
            let right_claim_rank = u8::from(right.phase == ContactPhase::Grab);
            left_claim_rank
                .cmp(&right_claim_rank)
                .then_with(|| owner_key(left.owner).cmp(&owner_key(right.owner)))
                .then_with(|| left.payload_id.cmp(&right.payload_id))
                .then_with(|| left.contact_ordinal.cmp(&right.contact_ordinal))
                .then_with(|| left.source.cmp(&right.source))
                .then_with(|| left.target.cmp(&right.target))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::{ImpactFeedbackIntensity, ImpactSource, impact_profile};
    use crate::determinism::SimEntityKind;
    use crate::reactions::ReactionFamilyId;

    fn record(
        source_index: u32,
        owner: u8,
        target: u8,
        priority_family: ReactionFamilyId,
        phase: ContactPhase,
    ) -> ContactRecord {
        let owner = FighterId::new(owner).unwrap();
        let target = FighterId::new(target).unwrap();
        let mut impact = impact_profile(
            owner.index(),
            ImpactSource::FighterStrike,
            10.0,
            4.0,
            2.0,
            false,
            true,
            4.0,
            ImpactFeedbackIntensity::Light,
            priority_family,
        );
        impact.payload_id = None;
        ContactRecord::new(
            phase,
            ContactSourceKind::FighterStrike,
            SimEntityId::new(SimEntityKind::Hitbox, source_index, 1),
            Some(owner),
            target,
            source_index as u16,
            0,
            0,
            Vec3::new(source_index as f32, 0.0, 0.0),
            Vec3::ZERO,
            impact,
            ContactFlags::default(),
        )
    }

    fn ordered_sources(buffer: &ContactBuffer) -> Vec<u32> {
        (0..buffer.len())
            .map(|index| {
                buffer
                    .record(index)
                    .unwrap()
                    .source
                    .entity()
                    .unwrap()
                    .index()
            })
            .collect()
    }

    #[test]
    fn sorting_is_independent_of_collection_order_and_keeps_strike_trades() {
        let contacts = [
            record(
                3,
                1,
                0,
                ReactionFamilyId::LauncherDown,
                ContactPhase::Strike,
            ),
            record(
                1,
                0,
                1,
                ReactionFamilyId::ShortStandingStagger,
                ContactPhase::Strike,
            ),
            record(
                2,
                2,
                1,
                ReactionFamilyId::GroundBounceDown,
                ContactPhase::Strike,
            ),
        ];
        let mut forward = ContactBuffer::with_capacity(8);
        let mut reverse = ContactBuffer::with_capacity(8);
        for contact in contacts {
            assert_eq!(forward.push(contact), ContactInsertResult::Inserted);
        }
        for contact in contacts.into_iter().rev() {
            assert_eq!(reverse.push(contact), ContactInsertResult::Inserted);
        }
        forward.sort_for_resolution();
        reverse.sort_for_resolution();

        assert_eq!(ordered_sources(&forward), ordered_sources(&reverse));
        assert_eq!(ordered_sources(&forward), vec![3, 1, 2]);
        assert_eq!(
            forward.ordinary_damage_participants(),
            [true, true, true, false]
        );
    }

    #[test]
    fn damaging_catches_resolve_as_impacts_before_non_damaging_grabs() {
        let mut buffer = ContactBuffer::with_capacity(8);
        let contacts = [
            record(
                4,
                0,
                2,
                ReactionFamilyId::ShortStandingStagger,
                ContactPhase::Grab,
            ),
            record(
                3,
                2,
                1,
                ReactionFamilyId::ShortStandingStagger,
                ContactPhase::CinematicCatch,
            ),
            record(
                2,
                1,
                3,
                ReactionFamilyId::ShortStandingStagger,
                ContactPhase::CinematicCatch,
            ),
        ];
        for contact in contacts.into_iter().rev() {
            buffer.push(contact);
        }
        buffer.sort_for_resolution();
        // Catch impacts follow the ordinary target/reaction ordering. Their
        // relationship claims are subsequently arbitrated by cinematic class
        // and holder ID in `combat::resolve_hitboxes`.
        assert_eq!(ordered_sources(&buffer), vec![3, 2, 4]);
    }

    #[test]
    fn capacity_overflow_retains_same_canonical_set_for_every_permutation() {
        let contacts = [
            record(
                4,
                0,
                1,
                ReactionFamilyId::ShortStandingStagger,
                ContactPhase::Strike,
            ),
            record(
                3,
                0,
                1,
                ReactionFamilyId::ShortStandingStagger,
                ContactPhase::Strike,
            ),
            record(
                2,
                0,
                1,
                ReactionFamilyId::LauncherDown,
                ContactPhase::Strike,
            ),
            record(
                1,
                0,
                1,
                ReactionFamilyId::GroundBounceDown,
                ContactPhase::Strike,
            ),
        ];
        let mut forward = ContactBuffer::with_capacity(2);
        let mut reverse = ContactBuffer::with_capacity(2);
        for contact in contacts {
            forward.push(contact);
        }
        for contact in contacts.into_iter().rev() {
            reverse.push(contact);
        }
        forward.sort_for_resolution();
        reverse.sort_for_resolution();

        assert_eq!(ordered_sources(&forward), ordered_sources(&reverse));
        assert_eq!(ordered_sources(&forward), vec![2, 1]);
        assert_eq!(forward.metrics().overflow_this_tick, 2);
        assert_eq!(reverse.metrics().overflow_this_tick, 2);
    }

    #[test]
    fn duplicate_identity_is_not_a_second_contact() {
        let original = record(
            1,
            0,
            1,
            ReactionFamilyId::ShortStandingStagger,
            ContactPhase::Strike,
        );
        let mut buffer = ContactBuffer::with_capacity(2);
        assert_eq!(buffer.push(original), ContactInsertResult::Inserted);
        assert_eq!(buffer.push(original), ContactInsertResult::Duplicate);

        let conflicting = ContactRecord {
            payload_id: original.payload_id + 1,
            ..original
        };
        assert_eq!(
            buffer.push(conflicting),
            ContactInsertResult::ConflictingDuplicate
        );
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer.metrics().duplicates_this_tick, 1);
        assert_eq!(buffer.metrics().conflicting_duplicates_this_tick, 1);
    }

    #[test]
    fn duplicate_identity_with_changed_impact_payload_is_a_conflict() {
        let original = record(
            1,
            0,
            1,
            ReactionFamilyId::ShortStandingStagger,
            ContactPhase::Strike,
        );
        let mut buffer = ContactBuffer::with_capacity(4);
        assert_eq!(buffer.push(original), ContactInsertResult::Inserted);

        let mut changed_damage = original;
        changed_damage.impact.as_mut().unwrap().damage += 1.0;
        assert_eq!(
            buffer.push(changed_damage),
            ContactInsertResult::ConflictingDuplicate
        );

        let mut changed_guard = original;
        changed_guard.impact.as_mut().unwrap().guard_stamina_damage += 1.0;
        assert_eq!(
            buffer.push(changed_guard),
            ContactInsertResult::ConflictingDuplicate
        );

        let mut changed_reaction = original;
        changed_reaction.impact.as_mut().unwrap().reaction =
            crate::reactions::reaction_profile_for_family(ReactionFamilyId::LauncherDown);
        changed_reaction.impact.as_mut().unwrap().reaction_family = ReactionFamilyId::LauncherDown;
        assert_eq!(
            buffer.push(changed_reaction),
            ContactInsertResult::ConflictingDuplicate
        );
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer.metrics().conflicting_duplicates_this_tick, 3);
    }

    #[test]
    fn status_contacts_have_typed_static_identity_without_an_impact_profile() {
        let source = ContactSourceId::ArenaHazard {
            arena_index: 2,
            hazard_index: 7,
        };
        let status = ContactRecord::new_status(
            ContactSourceKind::PersistentArenaHazard,
            source,
            None,
            FighterId::ZERO,
            4,
            9,
            0,
            Vec3::new(1.0, 2.0, 3.0),
            Vec3::ZERO,
            ContactFlags::default(),
        );

        assert_eq!(status.phase, ContactPhase::Status);
        assert_eq!(status.source, source);
        assert_eq!(status.source.arena_hazard(), Some((2, 7)));
        assert!(status.impact.is_none());
        assert!(!status.has_authored_damage());
    }

    #[test]
    fn contact_points_are_frozen_on_the_canonical_grid() {
        let point = Vec3::new(1.234_567, -0.333_333, 8.765_432);
        let frozen = QuantizedContactPoint::from_vec3(point);
        assert_eq!(
            frozen.components(),
            [
                quantize_f32(point.x, DEFAULT_F32_QUANTIZATION),
                quantize_f32(point.y, DEFAULT_F32_QUANTIZATION),
                quantize_f32(point.z, DEFAULT_F32_QUANTIZATION),
            ]
        );
        assert_eq!(
            frozen.to_vec3(),
            Vec3::new(
                dequantize_f32(frozen.components()[0], DEFAULT_F32_QUANTIZATION),
                dequantize_f32(frozen.components()[1], DEFAULT_F32_QUANTIZATION),
                dequantize_f32(frozen.components()[2], DEFAULT_F32_QUANTIZATION),
            )
        );
    }
}
