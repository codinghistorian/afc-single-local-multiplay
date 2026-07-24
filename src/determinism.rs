//! Deterministic primitives shared by the simulation and protocol layers.
//!
//! This module deliberately avoids Bevy identities, platform hashers, memory-layout
//! hashing, and process-global random state. The byte order, float quantization,
//! allocation order, and random-number algorithm are part of the simulation
//! contract and must only be changed with replay/version migration.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::error::Error;
use std::fmt;
use std::ops::Range;

/// The number of stable fighter slots in a match.
pub const FIGHTER_CAPACITY: u8 = 4;

/// The one canonical simulation clock used by gameplay, snapshots, replays, and
/// every network message. Tick zero is the initial state before the first step.
#[repr(transparent)]
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct SimTick(pub u64);

impl SimTick {
    pub const ZERO: Self = Self(0);

    pub const fn get(self) -> u64 {
        self.0
    }

    pub const fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }

    pub const fn wrapping_add(self, ticks: u64) -> Self {
        Self(self.0.wrapping_add(ticks))
    }

    pub const fn wrapping_sub(self, ticks: u64) -> Self {
        Self(self.0.wrapping_sub(ticks))
    }

    pub fn advance(&mut self) {
        *self = self.next();
    }
}

/// A stable fighter slot in `0..4`.
///
/// This is intentionally not interchangeable with a Bevy entity or a local input
/// device index. A peer may own several fighter slots, while every fighter-only
/// simulation relationship stores one of these IDs.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FighterId(u8);

impl FighterId {
    pub const ZERO: Self = Self(0);
    pub const ALL: [Self; FIGHTER_CAPACITY as usize] = [Self(0), Self(1), Self(2), Self(3)];

    pub const fn new(value: u8) -> Option<Self> {
        if value < FIGHTER_CAPACITY {
            Some(Self(value))
        } else {
            None
        }
    }

    pub const fn from_index(index: usize) -> Option<Self> {
        if index < FIGHTER_CAPACITY as usize {
            Some(Self(index as u8))
        } else {
            None
        }
    }

    pub const fn get(self) -> u8 {
        self.0
    }

    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

impl TryFrom<u8> for FighterId {
    type Error = InvalidFighterId;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value).ok_or(InvalidFighterId {
            value: usize::from(value),
        })
    }
}

impl TryFrom<usize> for FighterId {
    type Error = InvalidFighterId;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        Self::from_index(value).ok_or(InvalidFighterId { value })
    }
}

impl From<FighterId> for u8 {
    fn from(value: FighterId) -> Self {
        value.get()
    }
}

impl fmt::Display for FighterId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "fighter {}", self.0)
    }
}

impl Serialize for FighterId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u8(self.0)
    }
}

impl<'de> Deserialize<'de> for FighterId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u8::deserialize(deserializer)?;
        Self::new(value).ok_or_else(|| {
            serde::de::Error::custom(format_args!(
                "fighter ID {value} is outside 0..{FIGHTER_CAPACITY}"
            ))
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidFighterId {
    pub value: usize,
}

impl fmt::Display for InvalidFighterId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "fighter ID {} is outside 0..{}",
            self.value, FIGHTER_CAPACITY
        )
    }
}

impl Error for InvalidFighterId {}

/// A bounded set of fighter slots represented by the low four bits of one byte.
///
/// This replaces process-local `Vec<Entity>` hit tracking for attacks and other
/// effects that can affect each fighter at most once. Its wire representation is
/// canonical: bits outside the configured fighter capacity are invalid.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FighterHitMask(u8);

impl FighterHitMask {
    pub const VALID_BITS: u8 = (1_u8 << FIGHTER_CAPACITY) - 1;

    pub const fn from_bits(bits: u8) -> Option<Self> {
        if bits & !Self::VALID_BITS == 0 {
            Some(Self(bits))
        } else {
            None
        }
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn contains(self, fighter: FighterId) -> bool {
        self.0 & (1 << fighter.get()) != 0
    }

    /// Marks `fighter` and returns `true` only when it was newly inserted.
    pub fn insert(&mut self, fighter: FighterId) -> bool {
        let bit = 1 << fighter.get();
        let was_absent = self.0 & bit == 0;
        self.0 |= bit;
        was_absent
    }

    /// Removes `fighter` and returns `true` only when it was present.
    pub fn remove(&mut self, fighter: FighterId) -> bool {
        let bit = 1 << fighter.get();
        let was_present = self.0 & bit != 0;
        self.0 &= !bit;
        was_present
    }

    pub fn clear(&mut self) {
        self.0 = 0;
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn len(self) -> u32 {
        self.0.count_ones()
    }
}

impl Serialize for FighterHitMask {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u8(self.0)
    }
}

impl<'de> Deserialize<'de> for FighterHitMask {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bits = u8::deserialize(deserializer)?;
        Self::from_bits(bits).ok_or_else(|| {
            serde::de::Error::custom(format_args!(
                "fighter hit mask {bits:#010b} contains bits outside {:#010b}",
                Self::VALID_BITS
            ))
        })
    }
}

/// Stable pool namespaces for rollback-relevant dynamic objects.
///
/// The explicit numeric values are protocol data. Additions must be appended;
/// existing values must never be reordered or reused.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SimEntityKind {
    Hitbox = 0,
    Item = 1,
    Special = 2,
    BeeSkill = 3,
    ChickSkill = 4,
    PenguinSkill = 5,
    PenguinSurface = 6,
    ArenaOrdnance = 7,
}

impl SimEntityKind {
    pub const ALL: [Self; 8] = [
        Self::Hitbox,
        Self::Item,
        Self::Special,
        Self::BeeSkill,
        Self::ChickSkill,
        Self::PenguinSkill,
        Self::PenguinSurface,
        Self::ArenaOrdnance,
    ];

    pub const fn code(self) -> u8 {
        self as u8
    }

    pub const fn from_code(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Hitbox),
            1 => Some(Self::Item),
            2 => Some(Self::Special),
            3 => Some(Self::BeeSkill),
            4 => Some(Self::ChickSkill),
            5 => Some(Self::PenguinSkill),
            6 => Some(Self::PenguinSurface),
            7 => Some(Self::ArenaOrdnance),
            _ => None,
        }
    }
}

impl Serialize for SimEntityKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u8(self.code())
    }
}

impl<'de> Deserialize<'de> for SimEntityKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u8::deserialize(deserializer)?;
        Self::from_code(value).ok_or_else(|| {
            serde::de::Error::custom(format_args!("unknown simulation entity kind {value}"))
        })
    }
}

/// Stable identity for one dynamic simulation object.
///
/// A pool slot may be reused, but its generation changes first. Equality therefore
/// fails closed for stale references rather than silently selecting the new object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SimEntityId {
    kind: SimEntityKind,
    index: u32,
    generation: u32,
}

impl SimEntityId {
    pub const fn new(kind: SimEntityKind, index: u32, generation: u32) -> Self {
        Self {
            kind,
            index,
            generation,
        }
    }

    pub const fn kind(self) -> SimEntityKind {
        self.kind
    }

    pub const fn index(self) -> u32 {
        self.index
    }

    pub const fn generation(self) -> u32 {
        self.generation
    }
}

/// The only supported response to a full authoritative dynamic-object pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolOverflowPolicy {
    /// Preserve existing objects and reject the new object.
    RejectNew,
}

#[derive(Debug)]
pub struct PoolInsertError<T> {
    value: T,
    capacity: u32,
}

impl<T> PoolInsertError<T> {
    pub const fn capacity(&self) -> u32 {
        self.capacity
    }

    /// Returns the value that was rejected without changing pool state.
    pub fn into_value(self) -> T {
        self.value
    }
}

impl<T> fmt::Display for PoolInsertError<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "simulation entity pool is at its fixed capacity of {}",
            self.capacity
        )
    }
}

impl<T: fmt::Debug> Error for PoolInsertError<T> {}

#[derive(Debug)]
struct PoolSlot<T> {
    generation: u32,
    value: Option<T>,
    // Generation values never wrap: a slot that exhausts u32 is permanently
    // retired so no historical ID can ever alias a future object.
    retired: bool,
}

/// A bounded, deterministic, generational pool for one [`SimEntityKind`].
///
/// Storage is allocated once by [`Self::new`]. Allocation always selects the
/// lowest free index, iteration always uses ascending index order, and full pools
/// reject the new value without evicting or growing. Free indices are kept in
/// descending order so allocation is a constant-time `pop`; returning a slot may
/// move at most `capacity` compact integers but never grows the backing allocation.
#[derive(Debug)]
pub struct GenerationalPool<T> {
    kind: SimEntityKind,
    slots: Vec<PoolSlot<T>>,
    // Descending, so pop() returns the lowest index.
    free_indices: Vec<u32>,
    len: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PoolRestoreError {
    LengthMismatch,
    ZeroGeneration,
    InvalidFreeIndex,
    NonCanonicalFreeOrder,
    OccupancyMismatch,
}

impl<T> GenerationalPool<T> {
    const FIRST_GENERATION: u32 = 1;

    pub fn new(kind: SimEntityKind, capacity: u32) -> Self {
        let capacity_usize = capacity as usize;
        let slots = (0..capacity_usize)
            .map(|_| PoolSlot {
                generation: Self::FIRST_GENERATION,
                value: None,
                retired: false,
            })
            .collect();
        let free_indices = (0..capacity).rev().collect();

        Self {
            kind,
            slots,
            free_indices,
            len: 0,
        }
    }

    /// Reconstructs a pool from a snapshot layout that has already crossed the
    /// wire decoder's size limits. This performs its own invariant checks so an
    /// ECS restore cannot create an allocator whose next allocation differs from
    /// the authority.
    pub(crate) fn restore_layout(
        kind: SimEntityKind,
        generations: &[u32],
        values: Vec<Option<T>>,
        free_indices: &[u32],
    ) -> Result<Self, PoolRestoreError> {
        if generations.len() != values.len() {
            return Err(PoolRestoreError::LengthMismatch);
        }
        if generations.contains(&0) {
            return Err(PoolRestoreError::ZeroGeneration);
        }
        for pair in free_indices.windows(2) {
            if pair[0] <= pair[1] {
                return Err(PoolRestoreError::NonCanonicalFreeOrder);
            }
        }

        let mut free = vec![false; values.len()];
        for &index in free_indices {
            let Some(marker) = free.get_mut(index as usize) else {
                return Err(PoolRestoreError::InvalidFreeIndex);
            };
            if *marker {
                return Err(PoolRestoreError::InvalidFreeIndex);
            }
            *marker = true;
        }

        let mut len = 0_u32;
        let slots = values
            .into_iter()
            .enumerate()
            .map(|(index, value)| {
                let occupied = value.is_some();
                let retired = !occupied && generations[index] == u32::MAX;
                if free[index] != (!occupied && !retired) {
                    return Err(PoolRestoreError::OccupancyMismatch);
                }
                if occupied {
                    len = len.saturating_add(1);
                }
                Ok(PoolSlot {
                    generation: generations[index],
                    value,
                    retired,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            kind,
            slots,
            free_indices: free_indices.to_vec(),
            len,
        })
    }

    pub const fn kind(&self) -> SimEntityKind {
        self.kind
    }

    pub fn capacity(&self) -> u32 {
        self.slots.len() as u32
    }

    pub const fn len(&self) -> u32 {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn is_full(&self) -> bool {
        self.free_indices.is_empty()
    }

    pub const fn overflow_policy(&self) -> PoolOverflowPolicy {
        PoolOverflowPolicy::RejectNew
    }

    /// Inserts a value at the lowest available pool index.
    ///
    /// On exhaustion, the rejected value is returned and no allocator state is
    /// changed. This is the deterministic overflow contract used by gameplay.
    pub fn try_insert(&mut self, value: T) -> Result<SimEntityId, PoolInsertError<T>> {
        let Some(index) = self.free_indices.pop() else {
            return Err(PoolInsertError {
                value,
                capacity: self.capacity(),
            });
        };

        let slot = &mut self.slots[index as usize];
        debug_assert!(slot.value.is_none());
        slot.value = Some(value);
        self.len += 1;

        Ok(SimEntityId::new(self.kind, index, slot.generation))
    }

    pub fn contains(&self, id: SimEntityId) -> bool {
        self.get(id).is_some()
    }

    pub fn get(&self, id: SimEntityId) -> Option<&T> {
        if id.kind != self.kind {
            return None;
        }

        let slot = self.slots.get(id.index as usize)?;
        (slot.generation == id.generation)
            .then_some(slot.value.as_ref())
            .flatten()
    }

    pub fn get_mut(&mut self, id: SimEntityId) -> Option<&mut T> {
        if id.kind != self.kind {
            return None;
        }

        let slot = self.slots.get_mut(id.index as usize)?;
        if slot.generation != id.generation {
            return None;
        }
        slot.value.as_mut()
    }

    /// Removes the object if and only if the full kind/index/generation matches.
    ///
    /// The slot generation changes before it returns to the free list, immediately
    /// invalidating every stale copy of `id`.
    pub fn remove(&mut self, id: SimEntityId) -> Option<T> {
        if id.kind != self.kind {
            return None;
        }

        let slot = self.slots.get_mut(id.index as usize)?;
        if slot.generation != id.generation {
            return None;
        }

        let value = slot.value.take()?;
        let reusable = if let Some(generation) = next_generation(slot.generation) {
            slot.generation = generation;
            true
        } else {
            slot.retired = true;
            false
        };
        self.len -= 1;

        if reusable {
            let insertion_index = self
                .free_indices
                .partition_point(|free_index| *free_index > id.index);
            debug_assert_ne!(
                self.free_indices.get(insertion_index),
                Some(&id.index),
                "an occupied pool slot cannot already be free"
            );
            self.free_indices.insert(insertion_index, id.index);
        }
        Some(value)
    }

    /// Drops every live value while preserving stale-ID safety.
    pub fn clear(&mut self) {
        for slot in &mut self.slots {
            if slot.value.take().is_some() {
                if let Some(generation) = next_generation(slot.generation) {
                    slot.generation = generation;
                } else {
                    slot.retired = true;
                }
            }
        }
        self.free_indices.clear();
        self.free_indices.extend(
            self.slots
                .iter()
                .enumerate()
                .rev()
                .filter(|(_, slot)| !slot.retired)
                .map(|(index, _)| index as u32),
        );
        self.len = 0;
    }

    /// Iterates live values in canonical ascending pool-index order.
    pub fn iter(&self) -> impl Iterator<Item = (SimEntityId, &T)> {
        let kind = self.kind;
        self.slots
            .iter()
            .enumerate()
            .filter_map(move |(index, slot)| {
                slot.value
                    .as_ref()
                    .map(|value| (SimEntityId::new(kind, index as u32, slot.generation), value))
            })
    }

    /// Iterates live values mutably in canonical ascending pool-index order.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (SimEntityId, &mut T)> {
        let kind = self.kind;
        self.slots
            .iter_mut()
            .enumerate()
            .filter_map(move |(index, slot)| {
                let generation = slot.generation;
                slot.value
                    .as_mut()
                    .map(|value| (SimEntityId::new(kind, index as u32, generation), value))
            })
    }

    /// Returns the live identity and value stored at `index`.
    ///
    /// This is the allocation-free bridge used by ECS systems that must process
    /// dynamic objects in stable pool-index order instead of Bevy archetype or
    /// entity order.
    pub fn entry_at(&self, index: u32) -> Option<(SimEntityId, &T)> {
        let slot = self.slots.get(index as usize)?;
        slot.value
            .as_ref()
            .map(|value| (SimEntityId::new(self.kind, index, slot.generation), value))
    }

    /// Returns the current generation for snapshot/hash construction.
    pub fn generation_at(&self, index: u32) -> Option<u32> {
        self.slots.get(index as usize).map(|slot| slot.generation)
    }

    /// Returns free indices in the allocator's canonical descending storage order.
    pub fn free_indices(&self) -> impl ExactSizeIterator<Item = u32> + '_ {
        self.free_indices.iter().copied()
    }
}

fn next_generation(current: u32) -> Option<u32> {
    current.checked_add(1).filter(|generation| *generation != 0)
}

/// Integer units used to quantize one simulation unit.
///
/// Quantization rounds half away from zero, maps all NaNs to zero, maps infinities
/// to their corresponding signed limit, and saturates finite overflow. These rules
/// avoid hashing float bit patterns (including NaN payloads and signed zero).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Quantization(u32);

impl Quantization {
    pub const fn new(units_per_unit: u32) -> Option<Self> {
        if units_per_unit == 0 {
            None
        } else {
            Some(Self(units_per_unit))
        }
    }

    pub const fn units_per_unit(self) -> u32 {
        self.0
    }

    pub fn quantize(self, value: f32) -> i32 {
        quantize_f32(value, self)
    }
}

/// Default canonical precision: one quantized unit is 1/4096 simulation unit.
pub const DEFAULT_F32_QUANTIZATION: Quantization = Quantization(4096);

pub fn quantize_f32(value: f32, quantization: Quantization) -> i32 {
    if value.is_nan() {
        return 0;
    }
    if value == f32::INFINITY {
        return i32::MAX;
    }
    if value == f32::NEG_INFINITY {
        return i32::MIN;
    }

    // f32 -> f64 is exact, as is conversion of the u32 scale. The explicit
    // half-away-from-zero adjustment plus Rust's saturating float-to-int cast
    // freezes the rounding and overflow rules without relying on float memory bits.
    let scaled = f64::from(value) * f64::from(quantization.0);
    let rounded = if scaled >= 0.0 {
        scaled + 0.5
    } else {
        scaled - 0.5
    };

    if rounded >= f64::from(i32::MAX) {
        i32::MAX
    } else if rounded <= f64::from(i32::MIN) {
        i32::MIN
    } else {
        rounded as i32
    }
}

/// Converts canonical integer units back to simulation units. The default
/// 1/4096 scale is a power of two, so every value in the practical gameplay
/// range is represented exactly as `f32` after this conversion.
pub fn dequantize_f32(value: i32, quantization: Quantization) -> f32 {
    value as f32 / quantization.units_per_unit() as f32
}

/// Rounds an authoritative scalar onto the canonical simulation grid.
pub fn canonicalize_f32(value: f32, quantization: Quantization) -> f32 {
    dequantize_f32(quantize_f32(value, quantization), quantization)
}

const FNV1A_64_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV1A_64_PRIME: u64 = 0x0000_0100_0000_01b3;

/// A stable FNV-1a 64-bit writer for canonical simulation fields.
///
/// Callers must write fields in the documented snapshot order. Every numeric
/// method writes a fixed-width little-endian representation. This type offers no
/// `Hash`-trait or raw-struct-memory entry point by design.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanonicalHash64 {
    state: u64,
}

impl CanonicalHash64 {
    pub const fn new() -> Self {
        Self {
            state: FNV1A_64_OFFSET_BASIS,
        }
    }

    /// Writes bytes exactly as provided, without a length prefix.
    pub fn write_bytes(&mut self, bytes: &[u8]) -> &mut Self {
        for byte in bytes {
            self.state ^= u64::from(*byte);
            self.state = self.state.wrapping_mul(FNV1A_64_PRIME);
        }
        self
    }

    /// Writes a u64 length followed by bytes, preventing concatenation ambiguity.
    pub fn write_len_prefixed_bytes(&mut self, bytes: &[u8]) -> &mut Self {
        self.write_u64(bytes.len() as u64);
        self.write_bytes(bytes)
    }

    pub fn write_str(&mut self, value: &str) -> &mut Self {
        self.write_len_prefixed_bytes(value.as_bytes())
    }

    pub fn write_bool(&mut self, value: bool) -> &mut Self {
        self.write_u8(u8::from(value))
    }

    pub fn write_u8(&mut self, value: u8) -> &mut Self {
        self.write_bytes(&[value])
    }

    pub fn write_i8(&mut self, value: i8) -> &mut Self {
        self.write_bytes(&value.to_le_bytes())
    }

    pub fn write_u16(&mut self, value: u16) -> &mut Self {
        self.write_bytes(&value.to_le_bytes())
    }

    pub fn write_i16(&mut self, value: i16) -> &mut Self {
        self.write_bytes(&value.to_le_bytes())
    }

    pub fn write_u32(&mut self, value: u32) -> &mut Self {
        self.write_bytes(&value.to_le_bytes())
    }

    pub fn write_i32(&mut self, value: i32) -> &mut Self {
        self.write_bytes(&value.to_le_bytes())
    }

    pub fn write_u64(&mut self, value: u64) -> &mut Self {
        self.write_bytes(&value.to_le_bytes())
    }

    pub fn write_i64(&mut self, value: i64) -> &mut Self {
        self.write_bytes(&value.to_le_bytes())
    }

    pub fn write_fighter_id(&mut self, value: FighterId) -> &mut Self {
        self.write_u8(value.get())
    }

    pub fn write_sim_entity_id(&mut self, value: SimEntityId) -> &mut Self {
        self.write_u8(value.kind().code())
            .write_u32(value.index())
            .write_u32(value.generation())
    }

    pub fn write_quantized_f32(&mut self, value: f32, quantization: Quantization) -> &mut Self {
        self.write_i32(quantize_f32(value, quantization))
    }

    pub const fn finish(&self) -> u64 {
        self.state
    }
}

impl Default for CanonicalHash64 {
    fn default() -> Self {
        Self::new()
    }
}

/// Stable identity of one gameplay RNG stream.
///
/// `from_label` uses a fixed hash rather than `std::hash`, whose output is not a
/// replay contract. Protocol manifests should catalog labels to prevent accidental
/// renames or collisions.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RngStreamName(u64);

impl RngStreamName {
    pub const fn from_code(code: u64) -> Self {
        Self(code)
    }

    pub fn from_label(label: &str) -> Self {
        Self(stable_label_hash(label.as_bytes()))
    }

    pub const fn code(self) -> u64 {
        self.0
    }
}

/// Complete rollback state for one named gameplay random stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RngSnapshot {
    stream: RngStreamName,
    state: u64,
    counter: u64,
}

impl RngSnapshot {
    pub const fn stream(self) -> RngStreamName {
        self.stream
    }

    pub const fn state(self) -> u64 {
        self.state
    }

    pub const fn counter(self) -> u64 {
        self.counter
    }
}

/// SplitMix64 gameplay random stream derived from a match seed and stable name.
///
/// Separate subsystems own separate instances. Consequently, adding a draw to bot
/// logic cannot shift item, arena, or character random results.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeterministicRngStream {
    stream: RngStreamName,
    state: u64,
    counter: u64,
}

impl DeterministicRngStream {
    pub fn from_master_seed(master_seed: u64, stream: RngStreamName) -> Self {
        const STREAM_DERIVATION_DOMAIN: u64 = 0xd1b5_4a32_d192_ed03;
        let state = splitmix64_mix(
            master_seed
                .wrapping_add(STREAM_DERIVATION_DOMAIN)
                .wrapping_add(stream.code().wrapping_mul(SPLITMIX64_GAMMA)),
        );
        Self {
            stream,
            state,
            counter: 0,
        }
    }

    pub const fn from_snapshot(snapshot: RngSnapshot) -> Self {
        Self {
            stream: snapshot.stream,
            state: snapshot.state,
            counter: snapshot.counter,
        }
    }

    pub const fn stream(&self) -> RngStreamName {
        self.stream
    }

    /// Number of raw 64-bit candidates consumed, including rejected range samples.
    pub const fn counter(&self) -> u64 {
        self.counter
    }

    pub const fn snapshot(&self) -> RngSnapshot {
        RngSnapshot {
            stream: self.stream,
            state: self.state,
            counter: self.counter,
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(SPLITMIX64_GAMMA);
        self.counter = self.counter.wrapping_add(1);
        splitmix64_mix(self.state)
    }

    pub fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// Samples uniformly from a half-open u32 range using rejection sampling.
    ///
    /// Modulo-biased candidates are rejected. An empty or reversed range returns an
    /// error without consuming the stream.
    pub fn gen_range_u32(&mut self, range: Range<u32>) -> Result<u32, InvalidRngRange> {
        if range.start >= range.end {
            return Err(InvalidRngRange {
                start: range.start,
                end: range.end,
            });
        }

        let width = u64::from(range.end - range.start);
        let offset = rejection_sample_below(width, || self.next_u64()) as u32;
        Ok(range.start + offset)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidRngRange {
    pub start: u32,
    pub end: u32,
}

impl fmt::Display for InvalidRngRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "RNG range must be non-empty and ascending, got {}..{}",
            self.start, self.end
        )
    }
}

impl Error for InvalidRngRange {}

const SPLITMIX64_GAMMA: u64 = 0x9e37_79b9_7f4a_7c15;

fn splitmix64_mix(mut value: u64) -> u64 {
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn stable_label_hash(bytes: &[u8]) -> u64 {
    let mut hash = FNV1A_64_OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV1A_64_PRIME);
    }
    hash
}

fn rejection_sample_below(upper_exclusive: u64, mut next: impl FnMut() -> u64) -> u64 {
    debug_assert!(upper_exclusive > 0);

    // 2^64 modulo width. Values below this threshold form the short, biased tail
    // and are discarded. `wrapping_neg` represents 2^64 in u64 arithmetic.
    let rejection_threshold = upper_exclusive.wrapping_neg() % upper_exclusive;
    loop {
        let candidate = next();
        if candidate >= rejection_threshold {
            return candidate % upper_exclusive;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fighter_id_accepts_exactly_four_slots() {
        assert_eq!(FighterId::ALL.map(FighterId::get), [0, 1, 2, 3]);
        for value in 0..FIGHTER_CAPACITY {
            assert_eq!(FighterId::new(value).unwrap().get(), value);
        }
        assert_eq!(FighterId::new(FIGHTER_CAPACITY), None);
        assert_eq!(FighterId::from_index(usize::MAX), None);
        assert!(FighterId::try_from(FIGHTER_CAPACITY).is_err());
    }

    #[test]
    fn fighter_hit_mask_is_bounded_and_reports_duplicate_hits() {
        let mut mask = FighterHitMask::default();
        assert!(mask.is_empty());
        assert!(mask.insert(FighterId::ALL[1]));
        assert!(!mask.insert(FighterId::ALL[1]));
        assert!(mask.insert(FighterId::ALL[3]));
        assert_eq!(mask.bits(), 0b1010);
        assert_eq!(mask.len(), 2);
        assert!(mask.contains(FighterId::ALL[1]));
        assert!(!mask.contains(FighterId::ALL[2]));
        assert!(mask.remove(FighterId::ALL[1]));
        assert!(!mask.remove(FighterId::ALL[1]));
        mask.clear();
        assert!(mask.is_empty());
        assert_eq!(FighterHitMask::from_bits(0b1111).unwrap().bits(), 0b1111);
        assert_eq!(FighterHitMask::from_bits(0b1_0000), None);
    }

    #[test]
    fn simulation_entity_kind_codes_are_frozen() {
        for (code, kind) in SimEntityKind::ALL.into_iter().enumerate() {
            assert_eq!(kind.code(), code as u8);
            assert_eq!(SimEntityKind::from_code(code as u8), Some(kind));
        }
        assert_eq!(SimEntityKind::from_code(8), None);
        assert_eq!(SimEntityKind::from_code(u8::MAX), None);
    }

    #[test]
    fn pool_allocates_and_iterates_in_lowest_index_order() {
        let mut pool = GenerationalPool::new(SimEntityKind::Item, 4);
        let ids = ["a", "b", "c", "d"].map(|value| pool.try_insert(value).unwrap());

        assert_eq!(ids.map(SimEntityId::index), [0, 1, 2, 3]);
        assert_eq!(pool.len(), 4);
        assert!(pool.is_full());
        assert_eq!(pool.entry_at(2), Some((ids[2], &"c")));
        assert_eq!(pool.entry_at(4), None);
        assert_eq!(
            pool.iter()
                .map(|(id, value)| (id.index(), *value))
                .collect::<Vec<_>>(),
            vec![(0, "a"), (1, "b"), (2, "c"), (3, "d")]
        );
    }

    #[test]
    fn pool_reuse_increments_generation_and_stale_ids_fail_closed() {
        let mut pool = GenerationalPool::new(SimEntityKind::Hitbox, 1);
        let old_id = pool.try_insert(10).unwrap();
        assert_eq!(pool.remove(old_id), Some(10));
        assert_eq!(pool.get(old_id), None);

        let new_id = pool.try_insert(20).unwrap();
        assert_eq!(new_id.index(), old_id.index());
        assert_eq!(new_id.generation(), old_id.generation() + 1);
        assert_ne!(new_id, old_id);
        assert_eq!(pool.remove(old_id), None);
        assert_eq!(pool.get(new_id), Some(&20));
    }

    #[test]
    fn pool_reuses_lowest_free_index_independent_of_free_order() {
        let mut pool = GenerationalPool::new(SimEntityKind::Special, 4);
        let ids = [10, 11, 12, 13].map(|value| pool.try_insert(value).unwrap());
        assert_eq!(pool.remove(ids[3]), Some(13));
        assert_eq!(pool.remove(ids[1]), Some(11));

        let first = pool.try_insert(21).unwrap();
        let second = pool.try_insert(23).unwrap();
        assert_eq!([first.index(), second.index()], [1, 3]);
        assert_eq!(pool.free_indices().collect::<Vec<_>>(), Vec::<u32>::new());
    }

    #[test]
    fn full_pool_rejects_new_value_without_mutation_or_growth() {
        let mut pool = GenerationalPool::new(SimEntityKind::BeeSkill, 1);
        let existing = pool.try_insert("existing").unwrap();

        let error = pool.try_insert("rejected").unwrap_err();
        assert_eq!(error.capacity(), 1);
        assert_eq!(error.into_value(), "rejected");
        assert_eq!(pool.capacity(), 1);
        assert_eq!(pool.len(), 1);
        assert_eq!(pool.get(existing), Some(&"existing"));
        assert_eq!(pool.overflow_policy(), PoolOverflowPolicy::RejectNew);
    }

    #[test]
    fn zero_capacity_pool_always_rejects() {
        let mut pool = GenerationalPool::new(SimEntityKind::ArenaOrdnance, 0);
        assert!(pool.is_empty());
        assert!(pool.is_full());
        assert_eq!(pool.try_insert(7).unwrap_err().into_value(), 7);
        assert_eq!(pool.capacity(), 0);
    }

    #[test]
    fn pool_rejects_ids_from_other_kinds_and_out_of_bounds_indices() {
        let mut pool = GenerationalPool::new(SimEntityKind::Item, 1);
        let id = pool.try_insert(42).unwrap();
        let wrong_kind = SimEntityId::new(SimEntityKind::Hitbox, id.index(), id.generation());
        let out_of_bounds = SimEntityId::new(pool.kind(), 99, id.generation());

        assert_eq!(pool.get(wrong_kind), None);
        assert_eq!(pool.remove(wrong_kind), None);
        assert_eq!(pool.get(out_of_bounds), None);
        assert_eq!(pool.remove(out_of_bounds), None);
        assert_eq!(pool.get(id), Some(&42));
    }

    #[test]
    fn clearing_pool_invalidates_live_ids_and_restores_all_free_slots() {
        let mut pool = GenerationalPool::new(SimEntityKind::ChickSkill, 3);
        let first = pool.try_insert(1).unwrap();
        let second = pool.try_insert(2).unwrap();
        let untouched_generation = pool.generation_at(2).unwrap();

        pool.clear();

        assert!(pool.is_empty());
        assert_eq!(pool.get(first), None);
        assert_eq!(pool.get(second), None);
        assert_eq!(pool.free_indices().collect::<Vec<_>>(), vec![2, 1, 0]);
        assert_eq!(pool.generation_at(2), Some(untouched_generation));
        let replacement = pool.try_insert(3).unwrap();
        assert_eq!(replacement.index(), 0);
        assert_ne!(replacement.generation(), first.generation());
    }

    #[test]
    fn mutable_pool_iteration_remains_canonical() {
        let mut pool = GenerationalPool::new(SimEntityKind::PenguinSkill, 3);
        let ids = [1, 2, 3].map(|value| pool.try_insert(value).unwrap());
        pool.remove(ids[1]);

        for (id, value) in pool.iter_mut() {
            *value += id.index() as i32;
        }

        assert_eq!(
            pool.iter().map(|(_, value)| *value).collect::<Vec<_>>(),
            vec![1, 5]
        );
    }

    #[test]
    fn generation_exhaustion_retires_slot_instead_of_aliasing_stale_id() {
        assert_eq!(next_generation(0), Some(1));
        assert_eq!(next_generation(1), Some(2));
        assert_eq!(next_generation(u32::MAX), None);

        let mut pool = GenerationalPool::new(SimEntityKind::Hitbox, 1);
        pool.slots[0].generation = u32::MAX;
        let final_id = pool.try_insert(7).unwrap();
        assert_eq!(pool.remove(final_id), Some(7));
        assert!(pool.is_full());
        assert!(pool.try_insert(8).is_err());
        assert_eq!(pool.get(final_id), None);
    }

    #[test]
    fn canonical_hash_matches_frozen_fnv1a_vector() {
        let mut hash = CanonicalHash64::new();
        hash.write_bytes(b"abc");
        assert_eq!(hash.finish(), 0xe71f_a219_0541_574b);
    }

    #[test]
    fn canonical_integer_writes_are_explicit_little_endian_bytes() {
        let mut typed = CanonicalHash64::new();
        typed.write_u16(0x0201).write_i32(-7);

        let mut bytes = CanonicalHash64::new();
        bytes
            .write_bytes(&[1, 2])
            .write_bytes(&(-7_i32).to_le_bytes());
        assert_eq!(typed.finish(), bytes.finish());
    }

    #[test]
    fn canonical_hash_changes_with_field_order() {
        let mut first = CanonicalHash64::new();
        first.write_u32(10).write_u32(20);
        let mut second = CanonicalHash64::new();
        second.write_u32(20).write_u32(10);
        assert_ne!(first.finish(), second.finish());
    }

    #[test]
    fn length_prefix_prevents_byte_sequence_ambiguity() {
        let mut first = CanonicalHash64::new();
        first.write_str("a").write_str("bc");
        let mut second = CanonicalHash64::new();
        second.write_str("ab").write_str("c");
        assert_ne!(first.finish(), second.finish());
    }

    #[test]
    fn canonical_entity_id_hash_includes_kind_index_and_generation() {
        let base = SimEntityId::new(SimEntityKind::Item, 4, 9);
        let variants = [
            SimEntityId::new(SimEntityKind::Hitbox, 4, 9),
            SimEntityId::new(SimEntityKind::Item, 5, 9),
            SimEntityId::new(SimEntityKind::Item, 4, 10),
        ];
        let mut base_hash = CanonicalHash64::new();
        base_hash.write_sim_entity_id(base);

        for variant in variants {
            let mut variant_hash = CanonicalHash64::new();
            variant_hash.write_sim_entity_id(variant);
            assert_ne!(base_hash.finish(), variant_hash.finish());
        }
    }

    #[test]
    fn quantization_rejects_zero_scale() {
        assert_eq!(Quantization::new(0), None);
        assert_eq!(Quantization::new(1).unwrap().units_per_unit(), 1);
    }

    #[test]
    fn quantization_rounds_half_away_from_zero() {
        let whole = Quantization::new(1).unwrap();
        assert_eq!(whole.quantize(1.49), 1);
        assert_eq!(whole.quantize(1.5), 2);
        assert_eq!(whole.quantize(-1.49), -1);
        assert_eq!(whole.quantize(-1.5), -2);
    }

    #[test]
    fn quantization_collapses_signed_zero_and_nan_payloads() {
        let quantization = DEFAULT_F32_QUANTIZATION;
        let positive_nan = f32::from_bits(0x7fc0_0001);
        let negative_nan = f32::from_bits(0xffc0_4321);

        assert_eq!(quantization.quantize(0.0), 0);
        assert_eq!(quantization.quantize(-0.0), 0);
        assert_eq!(quantization.quantize(positive_nan), 0);
        assert_eq!(quantization.quantize(negative_nan), 0);

        let mut positive = CanonicalHash64::new();
        positive.write_quantized_f32(0.0, quantization);
        let mut negative = CanonicalHash64::new();
        negative.write_quantized_f32(-0.0, quantization);
        assert_eq!(positive.finish(), negative.finish());
    }

    #[test]
    fn quantization_saturates_non_finite_and_finite_overflow() {
        let quantization = DEFAULT_F32_QUANTIZATION;
        assert_eq!(quantization.quantize(f32::INFINITY), i32::MAX);
        assert_eq!(quantization.quantize(f32::NEG_INFINITY), i32::MIN);
        assert_eq!(quantization.quantize(f32::MAX), i32::MAX);
        assert_eq!(quantization.quantize(f32::MIN), i32::MIN);
    }

    #[test]
    fn quantization_uses_documented_numeric_buckets() {
        let thousandths = Quantization::new(1_000).unwrap();
        assert_eq!(thousandths.quantize(1.2344), 1_234);
        assert_eq!(thousandths.quantize(1.2346), 1_235);
        assert_eq!(thousandths.quantize(-1.2344), -1_234);
        assert_eq!(thousandths.quantize(-1.2346), -1_235);
    }

    #[test]
    fn splitmix64_core_matches_frozen_reference_vector() {
        assert_eq!(splitmix64_mix(SPLITMIX64_GAMMA), 0xe220_a839_7b1d_cdaf);
    }

    #[test]
    fn stream_labels_use_a_frozen_stable_hash() {
        assert_eq!(
            RngStreamName::from_label("abc").code(),
            0xe71f_a219_0541_574b
        );
        assert_eq!(
            RngStreamName::from_label("items"),
            RngStreamName::from_label("items")
        );
        assert_ne!(
            RngStreamName::from_label("items"),
            RngStreamName::from_label("bots")
        );
    }

    #[test]
    fn named_stream_isolation_prevents_cross_subsystem_consumption() {
        let seed = 0x1234_5678_9abc_def0;
        let item_name = RngStreamName::from_label("items");
        let bot_name = RngStreamName::from_label("bots/fighter/0");
        let mut items = DeterministicRngStream::from_master_seed(seed, item_name);
        let mut fresh_items = DeterministicRngStream::from_master_seed(seed, item_name);
        let mut bots = DeterministicRngStream::from_master_seed(seed, bot_name);

        for _ in 0..100 {
            bots.next_u64();
        }

        assert_eq!(items.next_u64(), fresh_items.next_u64());
        assert_eq!(items.counter(), 1);
        assert_eq!(bots.counter(), 100);
    }

    #[test]
    fn stream_snapshot_restores_state_and_counter_exactly() {
        let mut original = DeterministicRngStream::from_master_seed(
            77,
            RngStreamName::from_label("arena/hazards"),
        );
        for _ in 0..17 {
            original.next_u64();
        }
        let snapshot = original.snapshot();
        let mut restored = DeterministicRngStream::from_snapshot(snapshot);

        assert_eq!(snapshot.counter(), 17);
        assert_eq!(snapshot.stream(), original.stream());
        for _ in 0..32 {
            assert_eq!(restored.next_u64(), original.next_u64());
        }
        assert_eq!(restored.snapshot(), original.snapshot());
    }

    #[test]
    fn master_seed_and_stream_name_both_change_random_sequence() {
        let name = RngStreamName::from_label("character/bee");
        let mut first = DeterministicRngStream::from_master_seed(1, name);
        let mut first_again = DeterministicRngStream::from_master_seed(1, name);
        let mut other_seed = DeterministicRngStream::from_master_seed(2, name);
        let mut other_name = DeterministicRngStream::from_master_seed(
            1,
            RngStreamName::from_label("character/chick"),
        );

        assert_ne!(first.next_u64(), other_seed.next_u64());
        assert_ne!(first_again.next_u64(), other_name.next_u64());
    }

    #[test]
    fn rejection_sampler_discards_biased_tail_candidates() {
        let candidates = [5_u64, 16];
        let mut consumed = 0;
        let sampled = rejection_sample_below(10, || {
            let candidate = candidates[consumed];
            consumed += 1;
            candidate
        });

        assert_eq!(sampled, 6);
        assert_eq!(consumed, 2);
    }

    #[test]
    fn range_sampling_stays_in_bounds_and_is_replayable() {
        let name = RngStreamName::from_label("item/rewards");
        let mut first = DeterministicRngStream::from_master_seed(99, name);
        let mut replay = DeterministicRngStream::from_master_seed(99, name);

        for _ in 0..10_000 {
            let first_value = first.gen_range_u32(7..19).unwrap();
            let replay_value = replay.gen_range_u32(7..19).unwrap();
            assert!((7..19).contains(&first_value));
            assert_eq!(first_value, replay_value);
        }
        assert_eq!(first.snapshot(), replay.snapshot());
    }

    #[test]
    fn invalid_range_does_not_consume_rng_state() {
        let mut stream =
            DeterministicRngStream::from_master_seed(5, RngStreamName::from_label("test/range"));
        let before = stream.snapshot();

        assert!(stream.gen_range_u32(4..4).is_err());
        assert!(stream.gen_range_u32(9..2).is_err());
        assert_eq!(stream.snapshot(), before);
    }
}
