//! Stable identities for rollback-relevant Bevy entities.
//!
//! Bevy [`Entity`] values are process-local storage handles. They are useful for
//! presentation and ECS access, but they must not cross an authoritative state,
//! snapshot, replay, or network boundary. This module owns the bounded bridge
//! from canonical [`SimEntityId`] values to those local handles.

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use std::collections::HashSet;
use std::error::Error;
use std::fmt;

use crate::determinism::{GenerationalPool, SimEntityId, SimEntityKind};
use crate::snapshot::{PoolAllocatorSnapshot, SIM_ENTITY_KIND_COUNT, SnapshotError};

/// Production-wide, fixed allocation ceilings in [`SimEntityKind::ALL`] order.
///
/// These are fail-closed *global* ceilings: exhausting a namespace rejects the
/// new object without growing or evicting. The values cover the measured live
/// worst cases while keeping a maximum canonical snapshot below the resync cap.
/// TODO: enforce lower semantic per-owner/per-ability concurrency limits at the
/// spawn sites; this allocator intentionally does not claim to enforce them.
pub const SIM_ENTITY_POOL_CAPACITIES: [u32; SimEntityKind::ALL.len()] = [
    32,  // Hitbox: four fighters' bounded overlapping attack windows.
    16,  // Item: the arena's simultaneously spawned and held item budget.
    24,  // Special: concurrent fighter special-effect gameplay objects.
    128, // BeeSkill: the highest bounded bee projectile/burst fan-out.
    96,  // ChickSkill: bounded persistent and projectile skill fan-out.
    32,  // PenguinSkill: bounded moving penguin skill objects.
    256, // PenguinSurface: worst-case persistent ice/surface segmentation.
    4,   // ArenaOrdnance: one bounded ordnance slot per fighter/arena lane.
];

/// Canonical identity attached to one rollback-relevant Bevy entity.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StableSimEntity(SimEntityId);

impl StableSimEntity {
    pub const fn new(id: SimEntityId) -> Self {
        Self(id)
    }

    pub const fn id(self) -> SimEntityId {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SimulationIdentityOverflow {
    pub kind: SimEntityKind,
    pub capacity: u32,
}

impl fmt::Display for SimulationIdentityOverflow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:?} simulation identity pool is at its fixed capacity of {}",
            self.kind, self.capacity
        )
    }
}

impl Error for SimulationIdentityOverflow {}

#[derive(Debug, PartialEq, Eq)]
pub enum SimulationIdentityRestoreError {
    InvalidAllocatorCount { found: usize },
    AllocatorKindOrder { index: usize, found: SimEntityKind },
    InvalidAllocator(SnapshotError),
    MissingEntity(SimEntityId),
    DuplicateEntity(Entity),
    InvalidPoolLayout,
}

impl fmt::Display for SimulationIdentityRestoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid simulation identity restore: {self:?}")
    }
}

impl Error for SimulationIdentityRestoreError {}

/// Fixed-capacity canonical identity allocator plus the local ECS lookup bridge.
///
/// Allocation always uses the lowest free index in a kind-specific pool. A full
/// pool rejects the new object, increments a deterministic counter, and never
/// evicts a live object. Releases validate both the stable ID and the mapped
/// Bevy entity so a stale component cannot release a newer generation.
#[derive(Resource, Debug)]
pub struct SimulationIdentityAllocator {
    pools: [GenerationalPool<Entity>; SimEntityKind::ALL.len()],
    rejected_spawns: [u32; SimEntityKind::ALL.len()],
}

/// Keeps the command queue and identity allocator within one top-level Bevy
/// system parameter. Large lifecycle systems can use this without exceeding
/// Bevy's supported function-parameter tuple arity.
#[derive(SystemParam)]
pub struct StableEntityCommands<'w, 's> {
    pub commands: Commands<'w, 's>,
    pub identities: ResMut<'w, SimulationIdentityAllocator>,
}

impl Default for SimulationIdentityAllocator {
    fn default() -> Self {
        Self::with_capacities(SIM_ENTITY_POOL_CAPACITIES)
    }
}

impl SimulationIdentityAllocator {
    pub fn with_capacities(capacities: [u32; SimEntityKind::ALL.len()]) -> Self {
        let pools = std::array::from_fn(|index| {
            GenerationalPool::new(SimEntityKind::ALL[index], capacities[index])
        });
        Self {
            pools,
            rejected_spawns: [0; SimEntityKind::ALL.len()],
        }
    }

    pub fn try_allocate(
        &mut self,
        kind: SimEntityKind,
        entity: Entity,
    ) -> Result<StableSimEntity, SimulationIdentityOverflow> {
        let pool = &mut self.pools[kind.code() as usize];
        let capacity = pool.capacity();
        match pool.try_insert(entity) {
            Ok(id) => Ok(StableSimEntity::new(id)),
            Err(rejected) => {
                debug_assert_eq!(rejected.into_value(), entity);
                let rejected_spawns = &mut self.rejected_spawns[kind.code() as usize];
                *rejected_spawns = rejected_spawns.saturating_add(1);
                Err(SimulationIdentityOverflow { kind, capacity })
            }
        }
    }

    pub fn release(&mut self, entity: Entity, stable: StableSimEntity) -> bool {
        let id = stable.id();
        let pool = &mut self.pools[id.kind().code() as usize];
        if pool.get(id).copied() != Some(entity) {
            return false;
        }
        pool.remove(id).is_some()
    }

    pub fn mapped_entity(&self, id: SimEntityId) -> Option<Entity> {
        self.pools[id.kind().code() as usize].get(id).copied()
    }

    pub fn entry_at(&self, kind: SimEntityKind, index: u32) -> Option<(SimEntityId, Entity)> {
        self.pools[kind.code() as usize]
            .entry_at(index)
            .map(|(id, entity)| (id, *entity))
    }

    pub fn capacity(&self, kind: SimEntityKind) -> u32 {
        self.pools[kind.code() as usize].capacity()
    }

    pub fn live_count(&self, kind: SimEntityKind) -> u32 {
        self.pools[kind.code() as usize].len()
    }

    pub fn generation_at(&self, kind: SimEntityKind, index: u32) -> Option<u32> {
        self.pools[kind.code() as usize].generation_at(index)
    }

    pub fn free_indices(&self, kind: SimEntityKind) -> impl ExactSizeIterator<Item = u32> + '_ {
        self.pools[kind.code() as usize].free_indices()
    }

    pub fn rejected_spawns(&self, kind: SimEntityKind) -> u32 {
        self.rejected_spawns[kind.code() as usize]
    }

    pub fn rejected_spawn_counts(&self) -> [u32; SIM_ENTITY_KIND_COUNT] {
        self.rejected_spawns
    }

    /// Captures the exact generation, occupancy, and free-list layout. Restoring
    /// only live IDs is insufficient: the next allocation must choose the same
    /// slot and generation after rollback on every peer.
    pub fn capture_allocator_snapshots(&self) -> Vec<PoolAllocatorSnapshot> {
        self.capture_allocator_snapshots_reusing(Vec::with_capacity(SIM_ENTITY_KIND_COUNT))
    }

    /// Rebuilds allocator snapshots while retaining every compatible backing
    /// allocation from a previously captured canonical snapshot.
    pub fn capture_allocator_snapshots_reusing(
        &self,
        mut snapshots: Vec<PoolAllocatorSnapshot>,
    ) -> Vec<PoolAllocatorSnapshot> {
        if snapshots.len() != SIM_ENTITY_KIND_COUNT {
            snapshots.clear();
            snapshots.reserve(SIM_ENTITY_KIND_COUNT);
            for kind in SimEntityKind::ALL {
                let capacity = self.capacity(kind) as usize;
                snapshots.push(PoolAllocatorSnapshot {
                    kind,
                    capacity: capacity as u32,
                    generations: Vec::with_capacity(capacity),
                    occupied_bits: Vec::with_capacity(capacity.div_ceil(8)),
                    free_indices: Vec::with_capacity(capacity),
                });
            }
        }

        for (snapshot, kind) in snapshots.iter_mut().zip(SimEntityKind::ALL) {
            let pool = &self.pools[kind.code() as usize];
            let capacity = pool.capacity();
            snapshot.kind = kind;
            snapshot.capacity = capacity;

            snapshot.generations.clear();
            snapshot.generations.extend((0..capacity).map(|index| {
                pool.generation_at(index)
                    .expect("pool index below capacity must have a generation")
            }));

            snapshot.occupied_bits.clear();
            snapshot
                .occupied_bits
                .resize(capacity.div_ceil(8) as usize, 0);
            for index in 0..capacity {
                if pool.entry_at(index).is_some() {
                    snapshot.occupied_bits[index as usize / 8] |= 1 << (index % 8);
                }
            }

            snapshot.free_indices.clear();
            snapshot.free_indices.extend(pool.free_indices());
        }
        snapshots
    }

    /// Atomically restores allocator state after callers have recreated every
    /// dynamic ECS entity and attached its exact [`StableSimEntity`].
    pub fn restore_allocator_snapshots(
        &mut self,
        allocators: &[PoolAllocatorSnapshot],
        rejected_spawns: [u32; SIM_ENTITY_KIND_COUNT],
        mut entity_for_id: impl FnMut(SimEntityId) -> Option<Entity>,
    ) -> Result<(), SimulationIdentityRestoreError> {
        if allocators.len() != SIM_ENTITY_KIND_COUNT {
            return Err(SimulationIdentityRestoreError::InvalidAllocatorCount {
                found: allocators.len(),
            });
        }

        let mut seen_entities = HashSet::with_capacity(
            allocators
                .iter()
                .map(|allocator| {
                    allocator
                        .occupied_bits
                        .iter()
                        .map(|byte| byte.count_ones() as usize)
                        .sum::<usize>()
                })
                .sum(),
        );
        let mut restored = Vec::with_capacity(SIM_ENTITY_KIND_COUNT);
        for (index, allocator) in allocators.iter().enumerate() {
            let expected_kind = SimEntityKind::ALL[index];
            if allocator.kind != expected_kind {
                return Err(SimulationIdentityRestoreError::AllocatorKindOrder {
                    index,
                    found: allocator.kind,
                });
            }
            allocator
                .validate()
                .map_err(SimulationIdentityRestoreError::InvalidAllocator)?;

            let mut values = Vec::with_capacity(allocator.capacity as usize);
            for pool_index in 0..allocator.capacity {
                if allocator.is_occupied(pool_index) == Some(true) {
                    let id = SimEntityId::new(
                        allocator.kind,
                        pool_index,
                        allocator.generations[pool_index as usize],
                    );
                    let entity = entity_for_id(id)
                        .ok_or(SimulationIdentityRestoreError::MissingEntity(id))?;
                    if !seen_entities.insert(entity) {
                        return Err(SimulationIdentityRestoreError::DuplicateEntity(entity));
                    }
                    values.push(Some(entity));
                } else {
                    values.push(None);
                }
            }
            restored.push(
                GenerationalPool::restore_layout(
                    allocator.kind,
                    &allocator.generations,
                    values,
                    &allocator.free_indices,
                )
                .map_err(|_| SimulationIdentityRestoreError::InvalidPoolLayout)?,
            );
        }

        self.pools = restored.try_into().map_err(|_| {
            SimulationIdentityRestoreError::InvalidAllocatorCount {
                found: SIM_ENTITY_KIND_COUNT,
            }
        })?;
        self.rejected_spawns = rejected_spawns;
        Ok(())
    }
}

/// Spawns one authoritative entity and binds its local handle to a stable ID.
/// On deterministic pool overflow, the rejected Bevy entity is immediately
/// discarded and the caller receives the rejection instead of an unstable ID.
pub fn try_spawn_stable<B: Bundle>(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    kind: SimEntityKind,
    bundle: B,
) -> Result<Entity, SimulationIdentityOverflow> {
    let entity = commands.spawn_empty().id();
    let stable = match identities.try_allocate(kind, entity) {
        Ok(stable) => stable,
        Err(error) => {
            commands.entity(entity).despawn();
            return Err(error);
        }
    };
    commands.entity(entity).insert((stable, bundle));
    Ok(entity)
}

/// Releases a canonical identity before queuing the local entity's despawn.
pub fn despawn_stable(
    commands: &mut Commands,
    identities: &mut SimulationIdentityAllocator,
    entity: Entity,
    stable: StableSimEntity,
) {
    let released = identities.release(entity, stable);
    debug_assert!(
        released,
        "stable simulation entity must release exactly once"
    );
    commands.entity(entity).despawn();
}

/// Safety net for entities removed by a world teardown or an older call site.
/// Normal authoritative despawns release explicitly in the same simulation tick;
/// this system prevents a leaked slot if the component disappears unexpectedly.
pub fn reclaim_orphaned_sim_entities(
    mut identities: ResMut<SimulationIdentityAllocator>,
    stable_entities: Query<&StableSimEntity>,
) {
    for kind in SimEntityKind::ALL {
        loop {
            let orphan = identities.pools[kind.code() as usize]
                .iter()
                .find_map(|(id, entity)| {
                    (!stable_entities
                        .get(*entity)
                        .is_ok_and(|stable| stable.id() == id))
                    .then_some((id, *entity))
                });
            let Some((id, entity)) = orphan else {
                break;
            };
            let released = identities.release(entity, StableSimEntity::new(id));
            debug_assert!(released);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPECTED_PRODUCTION_CAPACITIES: [(SimEntityKind, u32); 8] = [
        (SimEntityKind::Hitbox, 32),
        (SimEntityKind::Item, 16),
        (SimEntityKind::Special, 24),
        (SimEntityKind::BeeSkill, 128),
        (SimEntityKind::ChickSkill, 96),
        (SimEntityKind::PenguinSkill, 32),
        (SimEntityKind::PenguinSurface, 256),
        (SimEntityKind::ArenaOrdnance, 4),
    ];

    fn entity(index: u32) -> Entity {
        Entity::from_raw_u32(index).expect("fixture entity index should be valid")
    }

    fn capacities(hitbox_capacity: u32) -> [u32; SimEntityKind::ALL.len()] {
        let mut capacities = [0; SimEntityKind::ALL.len()];
        capacities[SimEntityKind::Hitbox.code() as usize] = hitbox_capacity;
        capacities
    }

    #[test]
    fn production_capacities_follow_kind_order_and_total_588_slots() {
        for (index, (expected_kind, expected_capacity)) in
            EXPECTED_PRODUCTION_CAPACITIES.into_iter().enumerate()
        {
            assert_eq!(SimEntityKind::ALL[index], expected_kind);
            assert_eq!(expected_kind.code() as usize, index);
            assert_eq!(SIM_ENTITY_POOL_CAPACITIES[index], expected_capacity);
        }
        assert_eq!(SIM_ENTITY_POOL_CAPACITIES.into_iter().sum::<u32>(), 588);
    }

    #[test]
    fn default_allocator_captures_empty_production_pool_snapshots() {
        let identities = SimulationIdentityAllocator::default();
        let snapshots = identities.capture_allocator_snapshots();

        assert_eq!(snapshots.len(), EXPECTED_PRODUCTION_CAPACITIES.len());
        for (snapshot, (kind, capacity)) in snapshots
            .iter()
            .zip(EXPECTED_PRODUCTION_CAPACITIES.into_iter())
        {
            assert_eq!(
                snapshot,
                &PoolAllocatorSnapshot::empty(kind, capacity).unwrap()
            );
            snapshot.validate().unwrap();
            assert_eq!(identities.capacity(kind), capacity);
            assert_eq!(identities.live_count(kind), 0);
            assert_eq!(identities.rejected_spawns(kind), 0);
        }
    }

    #[test]
    fn repeated_allocator_capture_reuses_every_compatible_vector() {
        let mut identities = SimulationIdentityAllocator::default();
        let snapshots = identities.capture_allocator_snapshots();
        let outer = snapshots.as_ptr();
        let generations = snapshots
            .iter()
            .map(|snapshot| snapshot.generations.as_ptr())
            .collect::<Vec<_>>();
        let occupied = snapshots
            .iter()
            .map(|snapshot| snapshot.occupied_bits.as_ptr())
            .collect::<Vec<_>>();
        let free = snapshots
            .iter()
            .map(|snapshot| snapshot.free_indices.as_ptr())
            .collect::<Vec<_>>();

        let allocated = identities
            .try_allocate(SimEntityKind::Hitbox, entity(90))
            .unwrap();
        let snapshots = identities.capture_allocator_snapshots_reusing(snapshots);

        assert_eq!(snapshots.as_ptr(), outer);
        for index in 0..SIM_ENTITY_KIND_COUNT {
            assert_eq!(snapshots[index].generations.as_ptr(), generations[index]);
            assert_eq!(snapshots[index].occupied_bits.as_ptr(), occupied[index]);
            assert_eq!(snapshots[index].free_indices.as_ptr(), free[index]);
        }
        assert_eq!(
            snapshots[SimEntityKind::Hitbox.code() as usize].is_occupied(allocated.id().index()),
            Some(true)
        );
        for snapshot in snapshots {
            snapshot.validate().unwrap();
        }
    }

    #[test]
    fn allocation_uses_lowest_slot_and_release_advances_generation() {
        let mut identities = SimulationIdentityAllocator::with_capacities(capacities(2));
        let first = identities
            .try_allocate(SimEntityKind::Hitbox, entity(7))
            .unwrap();
        let second = identities
            .try_allocate(SimEntityKind::Hitbox, entity(8))
            .unwrap();

        assert_eq!(first.id().index(), 0);
        assert_eq!(first.id().generation(), 1);
        assert_eq!(second.id().index(), 1);
        assert!(identities.release(entity(7), first));

        let replacement = identities
            .try_allocate(SimEntityKind::Hitbox, entity(9))
            .unwrap();
        assert_eq!(replacement.id().index(), 0);
        assert_eq!(replacement.id().generation(), 2);
        assert_eq!(identities.mapped_entity(first.id()), None);
        assert_eq!(identities.mapped_entity(replacement.id()), Some(entity(9)));
        assert_eq!(identities.generation_at(SimEntityKind::Hitbox, 0), Some(2));
        assert_eq!(
            identities
                .free_indices(SimEntityKind::Hitbox)
                .collect::<Vec<_>>(),
            Vec::<u32>::new()
        );
    }

    #[test]
    fn full_pool_rejects_new_entity_without_evicting_live_mapping() {
        let mut identities = SimulationIdentityAllocator::with_capacities(capacities(1));
        let live = identities
            .try_allocate(SimEntityKind::Hitbox, entity(3))
            .unwrap();

        let error = identities
            .try_allocate(SimEntityKind::Hitbox, entity(4))
            .unwrap_err();

        assert_eq!(error.kind, SimEntityKind::Hitbox);
        assert_eq!(error.capacity, 1);
        assert_eq!(identities.live_count(SimEntityKind::Hitbox), 1);
        assert_eq!(identities.mapped_entity(live.id()), Some(entity(3)));
        assert_eq!(identities.rejected_spawns(SimEntityKind::Hitbox), 1);
    }

    #[test]
    fn stale_or_wrong_entity_cannot_release_live_generation() {
        let mut identities = SimulationIdentityAllocator::with_capacities(capacities(1));
        let stable = identities
            .try_allocate(SimEntityKind::Hitbox, entity(12))
            .unwrap();

        assert!(!identities.release(entity(13), stable));
        assert_eq!(identities.mapped_entity(stable.id()), Some(entity(12)));
        assert!(identities.release(entity(12), stable));
        assert!(!identities.release(entity(12), stable));
    }

    #[test]
    fn orphan_reconciliation_releases_a_despawned_bridge_entry() {
        let mut app = App::new();
        app.insert_resource(SimulationIdentityAllocator::with_capacities(capacities(1)))
            .add_systems(Update, reclaim_orphaned_sim_entities);

        let entity = app.world_mut().spawn_empty().id();
        let stable = app
            .world_mut()
            .resource_mut::<SimulationIdentityAllocator>()
            .try_allocate(SimEntityKind::Hitbox, entity)
            .unwrap();
        app.world_mut().entity_mut(entity).insert(stable);
        assert!(app.world_mut().despawn(entity));

        app.update();

        let identities = app.world().resource::<SimulationIdentityAllocator>();
        assert_eq!(identities.live_count(SimEntityKind::Hitbox), 0);
        assert_eq!(identities.mapped_entity(stable.id()), None);
    }

    #[test]
    fn allocator_snapshot_restore_preserves_next_id_and_rejection_counters() {
        let mut original = SimulationIdentityAllocator::with_capacities(capacities(3));
        let first = original
            .try_allocate(SimEntityKind::Hitbox, entity(20))
            .unwrap();
        let second = original
            .try_allocate(SimEntityKind::Hitbox, entity(21))
            .unwrap();
        assert!(original.release(entity(20), first));
        let replacement = original
            .try_allocate(SimEntityKind::Hitbox, entity(22))
            .unwrap();
        assert_eq!(replacement.id().index(), 0);
        assert_eq!(second.id().index(), 1);

        let snapshots = original.capture_allocator_snapshots();
        for snapshot in &snapshots {
            snapshot.validate().unwrap();
        }
        let rejected = std::array::from_fn(|index| index as u32 + 7);
        let mut restored = SimulationIdentityAllocator::default();
        restored
            .restore_allocator_snapshots(&snapshots, rejected, |id| original.mapped_entity(id))
            .unwrap();

        assert_eq!(restored.capture_allocator_snapshots(), snapshots);
        assert_eq!(restored.rejected_spawn_counts(), rejected);
        let original_next = original
            .try_allocate(SimEntityKind::Hitbox, entity(30))
            .unwrap();
        let restored_next = restored
            .try_allocate(SimEntityKind::Hitbox, entity(31))
            .unwrap();
        assert_eq!(original_next.id(), restored_next.id());
    }

    #[test]
    fn failed_allocator_restore_is_atomic() {
        let mut identities = SimulationIdentityAllocator::with_capacities(capacities(1));
        identities
            .try_allocate(SimEntityKind::Hitbox, entity(40))
            .unwrap();
        let before = identities.capture_allocator_snapshots();

        let error = identities
            .restore_allocator_snapshots(&before, [0; SIM_ENTITY_KIND_COUNT], |_| None)
            .unwrap_err();
        assert!(matches!(
            error,
            SimulationIdentityRestoreError::MissingEntity(_)
        ));
        assert_eq!(identities.capture_allocator_snapshots(), before);
    }
}
