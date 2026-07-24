//! Bevy ECS capture and restore boundary for canonical AFC snapshots.
//!
//! The schema in [`crate::snapshot`] deliberately contains no Bevy handles. This
//! module bridges that schema to a `World` without relying on query iteration or
//! Bevy entity allocation order. Dynamic objects are visited through the exact
//! stable-ID allocator layout and restored through kind-specific codecs.
//!
//! Legacy fighter components are still being migrated to integer time and stable
//! relationships. Their mapping is therefore an explicit [`FighterSnapshotCodec`]
//! supplied by that boundary rather than a lossy conversion in this module.

use bevy::prelude::*;
use std::array;
use std::error::Error;
use std::fmt;

use crate::determinism::{FIGHTER_CAPACITY, FighterHitMask, FighterId, SimEntityId, SimEntityKind};
use crate::ecs_identity::{
    SimulationIdentityAllocator, SimulationIdentityRestoreError, StableSimEntity,
};
use crate::simulation::SimTick;
use crate::snapshot::{
    ArenaRuntimeSnapshot, CanonicalSnapshot, DYNAMIC_PAYLOAD_BYTES, DynamicObjectSnapshot,
    FighterSnapshot, MATCH_ID_BYTES, MatchStateSnapshot, MatchStatsSnapshot, NamedRngSnapshot,
    SnapshotError, SnapshotHeader,
};

/// Immutable compatibility fields plus mutable non-fighter canonical state.
///
/// This resource is suitable for the extracted headless simulation. Presentation,
/// socket, asset, and local-input state must not be added here. Dynamic entities
/// and fighters remain in ECS components and are handled by their codecs.
#[derive(Resource, Clone, Debug, PartialEq, Eq)]
pub struct CanonicalNonFighterState {
    pub contract: SnapshotContract,
    pub match_state: MatchStateSnapshot,
    pub arena: ArenaRuntimeSnapshot,
    pub rng_streams: Vec<NamedRngSnapshot>,
    pub stats: MatchStatsSnapshot,
}

/// Maps match, arena, RNG, and telemetry resources into the canonical schema.
///
/// The default implementation below stores those sections in one extracted-sim
/// resource. The shipping game can instead supply a codec over its existing
/// resources, avoiding a second mutable copy of authoritative state.
pub trait NonFighterSnapshotCodec {
    type RestorePlan;

    /// Returns the immutable compatibility contract for this world. The ECS
    /// adapter uses it to validate allocator geometry before restore staging.
    fn snapshot_contract(&self, world: &World) -> Result<SnapshotContract, SnapshotCodecError>;

    fn capture_non_fighter(
        &self,
        world: &World,
        tick: SimTick,
    ) -> Result<CanonicalNonFighterState, SnapshotCodecError>;

    /// Performs every fallible compatibility and shape check before restore
    /// mutates the ECS. The returned plan owns everything needed by commit.
    fn prepare_restore(
        &self,
        world: &World,
        snapshot: &CanonicalSnapshot,
    ) -> Result<Self::RestorePlan, SnapshotCodecError>;

    fn commit_restore(&self, world: &mut World, plan: Self::RestorePlan);
}

/// Resource-backed non-fighter codec used by extracted/headless fixture worlds.
#[derive(Clone, Copy, Debug, Default)]
pub struct CanonicalNonFighterStateCodec;

impl NonFighterSnapshotCodec for CanonicalNonFighterStateCodec {
    type RestorePlan = CanonicalNonFighterState;

    fn snapshot_contract(&self, world: &World) -> Result<SnapshotContract, SnapshotCodecError> {
        world
            .get_resource::<CanonicalNonFighterState>()
            .map(|state| state.contract)
            .ok_or(SnapshotCodecError::new(
                1,
                "missing CanonicalNonFighterState resource",
            ))
    }

    fn capture_non_fighter(
        &self,
        world: &World,
        _tick: SimTick,
    ) -> Result<CanonicalNonFighterState, SnapshotCodecError> {
        world
            .get_resource::<CanonicalNonFighterState>()
            .cloned()
            .ok_or(SnapshotCodecError::new(
                1,
                "missing CanonicalNonFighterState resource",
            ))
    }

    fn prepare_restore(
        &self,
        world: &World,
        snapshot: &CanonicalSnapshot,
    ) -> Result<Self::RestorePlan, SnapshotCodecError> {
        let current =
            world
                .get_resource::<CanonicalNonFighterState>()
                .ok_or(SnapshotCodecError::new(
                    1,
                    "missing CanonicalNonFighterState resource",
                ))?;
        current
            .contract
            .validate_header(&snapshot.header)
            .map_err(|_| SnapshotCodecError::new(2, "snapshot contract mismatch"))?;
        Ok(CanonicalNonFighterState {
            contract: current.contract,
            match_state: snapshot.match_state,
            arena: snapshot.arena.clone(),
            rng_streams: snapshot.rng_streams.clone(),
            stats: snapshot.stats.clone(),
        })
    }

    fn commit_restore(&self, world: &mut World, plan: Self::RestorePlan) {
        world.insert_resource(plan);
    }
}

/// Snapshot compatibility values fixed for one match instance.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapshotContract {
    pub simulation_version: u32,
    pub protocol_version: u32,
    pub gameplay_content_hash: u64,
    pub match_id: [u8; MATCH_ID_BYTES],
    pub master_seed: u64,
    /// Exact allocator geometry agreed for this match, indexed by
    /// [`SimEntityKind::code`]. This is immutable session compatibility data,
    /// not a size selected by snapshot input.
    pub pool_capacities: [u32; SimEntityKind::ALL.len()],
}

impl SnapshotContract {
    pub const fn header(self, tick: SimTick) -> SnapshotHeader {
        SnapshotHeader::new(
            self.simulation_version,
            self.protocol_version,
            self.gameplay_content_hash,
            self.match_id,
            tick,
            self.master_seed,
        )
    }

    pub(crate) fn validate_header(self, header: &SnapshotHeader) -> Result<(), EcsSnapshotError> {
        for (matches, field) in [
            (
                self.simulation_version == header.simulation_version,
                "simulation version",
            ),
            (
                self.protocol_version == header.protocol_version,
                "protocol version",
            ),
            (
                self.gameplay_content_hash == header.gameplay_content_hash,
                "gameplay content hash",
            ),
            (self.match_id == header.match_id, "match ID"),
            (self.master_seed == header.master_seed, "master seed"),
        ] {
            if !matches {
                return Err(EcsSnapshotError::ContractMismatch { field });
            }
        }
        Ok(())
    }

    fn validate_capture_allocator_capacities(
        self,
        identities: &SimulationIdentityAllocator,
    ) -> Result<(), EcsSnapshotError> {
        for kind in SimEntityKind::ALL {
            let expected = self.pool_capacities[kind.code() as usize];
            let found = identities.capacity(kind);
            if found != expected {
                return Err(EcsSnapshotError::CaptureAllocatorCapacityMismatch {
                    kind,
                    expected,
                    found,
                });
            }
        }
        Ok(())
    }

    fn validate_restore_allocator_capacities(
        self,
        snapshot: &CanonicalSnapshot,
    ) -> Result<(), EcsSnapshotError> {
        debug_assert_eq!(snapshot.allocators.len(), SimEntityKind::ALL.len());
        for (index, kind) in SimEntityKind::ALL.into_iter().enumerate() {
            let expected = self.pool_capacities[index];
            let found = snapshot.allocators[index].capacity;
            if found != expected {
                return Err(EcsSnapshotError::RestoreAllocatorCapacityMismatch {
                    kind,
                    expected,
                    found,
                });
            }
        }
        Ok(())
    }
}

/// Schema-shaped authoritative data attached to a stable dynamic ECS entity.
///
/// This component is the default codec for an extracted/headless world. Existing
/// gameplay components can instead register a kind-specific codec that maps their
/// fields into the same bounded record.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct CanonicalDynamicState {
    pub definition_id: u16,
    pub flags: u32,
    pub owner: Option<FighterId>,
    pub target: Option<FighterId>,
    pub related_entity: Option<SimEntityId>,
    pub fighter_hit_mask: FighterHitMask,
    pub payload: [u8; DYNAMIC_PAYLOAD_BYTES],
}

impl Default for CanonicalDynamicState {
    fn default() -> Self {
        Self {
            definition_id: 0,
            flags: 0,
            owner: None,
            target: None,
            related_entity: None,
            fighter_hit_mask: FighterHitMask::default(),
            payload: [0; DYNAMIC_PAYLOAD_BYTES],
        }
    }
}

impl CanonicalDynamicState {
    pub fn to_snapshot(&self, id: SimEntityId) -> DynamicObjectSnapshot {
        DynamicObjectSnapshot {
            id,
            definition_id: self.definition_id,
            flags: self.flags,
            owner: self.owner,
            target: self.target,
            related_entity: self.related_entity,
            fighter_hit_mask: self.fighter_hit_mask.bits(),
            payload: self.payload,
        }
    }

    fn from_snapshot(snapshot: &DynamicObjectSnapshot) -> Self {
        Self {
            definition_id: snapshot.definition_id,
            flags: snapshot.flags,
            owner: snapshot.owner,
            target: snapshot.target,
            related_entity: snapshot.related_entity,
            fighter_hit_mask: FighterHitMask::from_bits(snapshot.fighter_hit_mask)
                .expect("validated snapshots contain only canonical fighter mask bits"),
            payload: snapshot.payload,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapshotCodecError {
    pub code: u16,
    pub message: &'static str,
}

impl SnapshotCodecError {
    pub const fn new(code: u16, message: &'static str) -> Self {
        Self { code, message }
    }
}

impl fmt::Display for SnapshotCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "snapshot codec error {}: {}",
            self.code, self.message
        )
    }
}

impl Error for SnapshotCodecError {}

/// Maps one stable dynamic-entity kind between gameplay components and the fixed
/// canonical dynamic record.
///
/// `validate_restore` is the fallible preparation phase. `restore_validated` runs
/// only after every snapshot section and entity has passed validation and therefore
/// must perform an infallible overwrite of canonical components. The supplied
/// entity may be newly spawned, or it may be an exact-ID survivor retaining
/// presentation-only components from the pre-rollback world.
pub trait DynamicSnapshotCodec: Send + Sync + 'static {
    fn capture(
        &self,
        world: &World,
        entity: Entity,
        id: SimEntityId,
    ) -> Result<DynamicObjectSnapshot, SnapshotCodecError>;

    fn validate_restore(
        &self,
        world: &World,
        snapshot: &DynamicObjectSnapshot,
    ) -> Result<(), SnapshotCodecError>;

    fn restore_validated(
        &self,
        world: &mut World,
        entity: Entity,
        snapshot: &DynamicObjectSnapshot,
    );
}

/// Default codec for entities that store [`CanonicalDynamicState`] directly.
#[derive(Clone, Copy, Debug, Default)]
pub struct CanonicalDynamicStateCodec;

impl DynamicSnapshotCodec for CanonicalDynamicStateCodec {
    fn capture(
        &self,
        world: &World,
        entity: Entity,
        id: SimEntityId,
    ) -> Result<DynamicObjectSnapshot, SnapshotCodecError> {
        world
            .get::<CanonicalDynamicState>(entity)
            .map(|state| state.to_snapshot(id))
            .ok_or(SnapshotCodecError::new(
                1,
                "stable entity is missing CanonicalDynamicState",
            ))
    }

    fn validate_restore(
        &self,
        _world: &World,
        _snapshot: &DynamicObjectSnapshot,
    ) -> Result<(), SnapshotCodecError> {
        Ok(())
    }

    fn restore_validated(
        &self,
        world: &mut World,
        entity: Entity,
        snapshot: &DynamicObjectSnapshot,
    ) {
        world
            .entity_mut(entity)
            .insert(CanonicalDynamicState::from_snapshot(snapshot));
    }
}

/// Fixed, kind-indexed codec registry. Registration is bounded by the stable kind
/// catalog and lookup never hashes or depends on insertion order.
pub struct DynamicSnapshotCodecRegistry {
    codecs: [Option<Box<dyn DynamicSnapshotCodec>>; SimEntityKind::ALL.len()],
}

impl Default for DynamicSnapshotCodecRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl DynamicSnapshotCodecRegistry {
    pub fn new() -> Self {
        Self {
            codecs: array::from_fn(|_| None),
        }
    }

    /// Creates a registry for worlds using [`CanonicalDynamicState`] for every
    /// dynamic kind.
    pub fn canonical_state_for_all_kinds() -> Self {
        let mut registry = Self::new();
        for kind in SimEntityKind::ALL {
            registry.codecs[kind.code() as usize] = Some(Box::new(CanonicalDynamicStateCodec));
        }
        registry
    }

    pub fn register(
        &mut self,
        kind: SimEntityKind,
        codec: impl DynamicSnapshotCodec,
    ) -> Result<(), DynamicCodecRegistrationError> {
        let slot = &mut self.codecs[kind.code() as usize];
        if slot.is_some() {
            return Err(DynamicCodecRegistrationError::DuplicateKind(kind));
        }
        *slot = Some(Box::new(codec));
        Ok(())
    }

    pub fn contains(&self, kind: SimEntityKind) -> bool {
        self.codecs[kind.code() as usize].is_some()
    }

    fn get(&self, kind: SimEntityKind) -> Option<&dyn DynamicSnapshotCodec> {
        self.codecs[kind.code() as usize].as_deref()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DynamicCodecRegistrationError {
    DuplicateKind(SimEntityKind),
}

impl fmt::Display for DynamicCodecRegistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "dynamic snapshot codec registration failed: {self:?}"
        )
    }
}

impl Error for DynamicCodecRegistrationError {}

/// Fighter mapping boundary used while legacy fighter components are migrated.
/// Restore plans must own their data; validation finishes before the ECS is changed.
pub trait FighterSnapshotCodec {
    type RestorePlan;

    fn capture_fighters(
        &self,
        world: &World,
    ) -> Result<[FighterSnapshot; FIGHTER_CAPACITY as usize], SnapshotCodecError>;

    fn prepare_restore(
        &self,
        world: &World,
        fighters: &[FighterSnapshot; FIGHTER_CAPACITY as usize],
    ) -> Result<Self::RestorePlan, SnapshotCodecError>;

    fn commit_restore(&self, world: &mut World, plan: Self::RestorePlan);
}

/// Codec for setup/loading worlds that contain no occupied fighter slots.
#[derive(Clone, Copy, Debug, Default)]
pub struct EmptyFighterSnapshotCodec;

impl FighterSnapshotCodec for EmptyFighterSnapshotCodec {
    type RestorePlan = ();

    fn capture_fighters(
        &self,
        _world: &World,
    ) -> Result<[FighterSnapshot; FIGHTER_CAPACITY as usize], SnapshotCodecError> {
        Ok(FighterId::ALL.map(FighterSnapshot::empty))
    }

    fn prepare_restore(
        &self,
        _world: &World,
        fighters: &[FighterSnapshot; FIGHTER_CAPACITY as usize],
    ) -> Result<Self::RestorePlan, SnapshotCodecError> {
        if fighters
            .iter()
            .enumerate()
            .all(|(index, fighter)| *fighter == FighterSnapshot::empty(FighterId::ALL[index]))
        {
            Ok(())
        } else {
            Err(SnapshotCodecError::new(
                1,
                "empty-fighter codec cannot restore an occupied fighter",
            ))
        }
    }

    fn commit_restore(&self, _world: &mut World, _plan: Self::RestorePlan) {}
}

#[derive(Debug, PartialEq, Eq)]
pub enum EcsSnapshotError {
    MissingResource(&'static str),
    Snapshot(SnapshotError),
    ContractMismatch {
        field: &'static str,
    },
    CaptureAllocatorCapacityMismatch {
        kind: SimEntityKind,
        expected: u32,
        found: u32,
    },
    RestoreAllocatorCapacityMismatch {
        kind: SimEntityKind,
        expected: u32,
        found: u32,
    },
    MissingDynamicCodec(SimEntityKind),
    DynamicCodec {
        id: SimEntityId,
        source: SnapshotCodecError,
    },
    NonFighterCodec(SnapshotCodecError),
    FighterCodec(SnapshotCodecError),
    AllocatorEntityMissingStableComponent {
        id: SimEntityId,
        entity: Entity,
    },
    AllocatorEntityStableIdMismatch {
        expected: SimEntityId,
        found: SimEntityId,
        entity: Entity,
    },
    UntrackedStableEntity {
        id: SimEntityId,
        entity: Entity,
    },
    DynamicCodecReturnedWrongId {
        expected: SimEntityId,
        found: SimEntityId,
    },
    IdentityRestore(SimulationIdentityRestoreError),
}

impl From<SnapshotError> for EcsSnapshotError {
    fn from(error: SnapshotError) -> Self {
        Self::Snapshot(error)
    }
}

impl From<SimulationIdentityRestoreError> for EcsSnapshotError {
    fn from(error: SimulationIdentityRestoreError) -> Self {
        Self::IdentityRestore(error)
    }
}

impl fmt::Display for EcsSnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Bevy ECS snapshot operation failed: {self:?}")
    }
}

impl Error for EcsSnapshotError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Snapshot(source) => Some(source),
            Self::DynamicCodec { source, .. }
            | Self::NonFighterCodec(source)
            | Self::FighterCodec(source) => Some(source),
            Self::IdentityRestore(source) => Some(source),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EcsSnapshotRestoreReport {
    pub restored_tick: SimTick,
    /// Snapshot objects restored on their existing exact-ID ECS entity.
    pub reused_dynamic_entities: usize,
    /// Snapshot objects that had no exact-ID survivor and needed a new ECS entity.
    pub created_dynamic_entities: usize,
    /// Previous authoritative entities absent from the restored exact-ID set.
    pub removed_dynamic_entities: usize,
    /// Total number of canonical dynamic objects restored (reused plus created).
    pub restored_dynamic_entities: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DynamicRestoreBinding {
    id: SimEntityId,
    entity: Entity,
    created: bool,
}

/// Builds a replacement identity resource without touching the current one.
///
/// The caller must mark only entities spawned during restore as `created`. On
/// failure those staging entities are discarded, while every pre-existing entity
/// and resource remains untouched. `bindings` are canonical ID order.
fn reconstruct_identities_or_discard_staging(
    world: &mut World,
    snapshot: &CanonicalSnapshot,
    pool_capacities: [u32; SimEntityKind::ALL.len()],
    bindings: &[DynamicRestoreBinding],
) -> Result<SimulationIdentityAllocator, EcsSnapshotError> {
    // Capacity geometry comes from the already-validated match contract. The
    // snapshot supplies generations/occupancy only after proving it has exactly
    // this geometry.
    let mut restored = SimulationIdentityAllocator::with_capacities(pool_capacities);
    if let Err(error) = restored.restore_allocator_snapshots(
        &snapshot.allocators,
        snapshot.stats.rejected_dynamic_spawns,
        |id| {
            bindings
                .binary_search_by_key(&id, |binding| binding.id)
                .ok()
                .map(|index| bindings[index].entity)
        },
    ) {
        for binding in bindings.iter().filter(|binding| binding.created) {
            let _ = world.despawn(binding.entity);
        }
        return Err(EcsSnapshotError::IdentityRestore(error));
    }
    Ok(restored)
}

/// Captures and restores one authoritative Bevy world using stable IDs.
pub struct EcsSnapshotAdapter {
    dynamic_codecs: DynamicSnapshotCodecRegistry,
}

impl EcsSnapshotAdapter {
    pub fn new(dynamic_codecs: DynamicSnapshotCodecRegistry) -> Self {
        Self { dynamic_codecs }
    }

    pub fn dynamic_codecs(&self) -> &DynamicSnapshotCodecRegistry {
        &self.dynamic_codecs
    }

    pub fn capture<F: FighterSnapshotCodec>(
        &self,
        world: &World,
        fighter_codec: &F,
    ) -> Result<CanonicalSnapshot, EcsSnapshotError> {
        if !world.contains_resource::<CanonicalNonFighterState>() {
            return Err(EcsSnapshotError::MissingResource(
                "CanonicalNonFighterState",
            ));
        }
        self.capture_with_non_fighter(world, &CanonicalNonFighterStateCodec, fighter_codec)
    }

    /// Captures with a caller-supplied mapping for live match/arena resources.
    /// This is the production path for the existing game, where authoritative
    /// state must not be mirrored into `CanonicalNonFighterState` each tick.
    pub fn capture_with_non_fighter<N: NonFighterSnapshotCodec, F: FighterSnapshotCodec>(
        &self,
        world: &World,
        non_fighter_codec: &N,
        fighter_codec: &F,
    ) -> Result<CanonicalSnapshot, EcsSnapshotError> {
        self.capture_with_non_fighter_reusing(world, non_fighter_codec, fighter_codec, None)
    }

    /// Captures into the backing storage owned by an expired bounded-history
    /// entry. Compatibility validation is identical to a fresh capture.
    pub fn capture_with_non_fighter_reusing<N: NonFighterSnapshotCodec, F: FighterSnapshotCodec>(
        &self,
        world: &World,
        non_fighter_codec: &N,
        fighter_codec: &F,
        reusable: Option<CanonicalSnapshot>,
    ) -> Result<CanonicalSnapshot, EcsSnapshotError> {
        let tick = *world
            .get_resource::<SimTick>()
            .ok_or(EcsSnapshotError::MissingResource("SimTick"))?;
        let state = non_fighter_codec
            .capture_non_fighter(world, tick)
            .map_err(EcsSnapshotError::NonFighterCodec)?;
        {
            let identities = world.get_resource::<SimulationIdentityAllocator>().ok_or(
                EcsSnapshotError::MissingResource("SimulationIdentityAllocator"),
            )?;
            state
                .contract
                .validate_capture_allocator_capacities(identities)?;
        }
        self.validate_live_identity_bridge(world)?;
        let identities = world.get_resource::<SimulationIdentityAllocator>().ok_or(
            EcsSnapshotError::MissingResource("SimulationIdentityAllocator"),
        )?;

        let total_dynamic_capacity: usize = SimEntityKind::ALL
            .into_iter()
            .map(|kind| identities.capacity(kind) as usize)
            .sum();
        let (allocator_storage, mut dynamic_objects, mut rng_streams) =
            if let Some(snapshot) = reusable {
                (
                    snapshot.allocators,
                    snapshot.dynamic_objects,
                    snapshot.rng_streams,
                )
            } else {
                (
                    Vec::with_capacity(SimEntityKind::ALL.len()),
                    Vec::with_capacity(total_dynamic_capacity),
                    Vec::new(),
                )
            };
        dynamic_objects.clear();
        if dynamic_objects.capacity() < total_dynamic_capacity {
            dynamic_objects.reserve_exact(total_dynamic_capacity);
        }
        for kind in SimEntityKind::ALL {
            for index in 0..identities.capacity(kind) {
                let Some((id, entity)) = identities.entry_at(kind, index) else {
                    continue;
                };
                let codec = self
                    .dynamic_codecs
                    .get(kind)
                    .ok_or(EcsSnapshotError::MissingDynamicCodec(kind))?;
                let object = codec
                    .capture(world, entity, id)
                    .map_err(|source| EcsSnapshotError::DynamicCodec { id, source })?;
                if object.id != id {
                    return Err(EcsSnapshotError::DynamicCodecReturnedWrongId {
                        expected: id,
                        found: object.id,
                    });
                }
                dynamic_objects.push(object);
            }
        }

        let mut stats = state.stats;
        stats.rejected_dynamic_spawns = identities.rejected_spawn_counts();
        rng_streams.clear();
        rng_streams.extend(state.rng_streams);
        let snapshot = CanonicalSnapshot {
            header: state.contract.header(tick),
            match_state: state.match_state,
            fighters: fighter_codec
                .capture_fighters(world)
                .map_err(EcsSnapshotError::FighterCodec)?,
            arena: state.arena,
            allocators: identities.capture_allocator_snapshots_reusing(allocator_storage),
            dynamic_objects,
            rng_streams,
            stats,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn restore<F: FighterSnapshotCodec>(
        &self,
        world: &mut World,
        snapshot: &CanonicalSnapshot,
        fighter_codec: &F,
    ) -> Result<EcsSnapshotRestoreReport, EcsSnapshotError> {
        let contract = world
            .get_resource::<CanonicalNonFighterState>()
            .ok_or(EcsSnapshotError::MissingResource(
                "CanonicalNonFighterState",
            ))?
            .contract;
        contract.validate_header(&snapshot.header)?;
        self.restore_with_non_fighter(
            world,
            snapshot,
            &CanonicalNonFighterStateCodec,
            fighter_codec,
        )
    }

    /// Restores through caller-supplied live-resource and fighter mappings. All
    /// codec preparation completes before any stable entity or resource changes.
    pub fn restore_with_non_fighter<N: NonFighterSnapshotCodec, F: FighterSnapshotCodec>(
        &self,
        world: &mut World,
        snapshot: &CanonicalSnapshot,
        non_fighter_codec: &N,
        fighter_codec: &F,
    ) -> Result<EcsSnapshotRestoreReport, EcsSnapshotError> {
        snapshot.validate()?;
        let contract = non_fighter_codec
            .snapshot_contract(world)
            .map_err(EcsSnapshotError::NonFighterCodec)?;
        contract.validate_header(&snapshot.header)?;
        contract.validate_restore_allocator_capacities(snapshot)?;

        let non_fighter_plan = non_fighter_codec
            .prepare_restore(world, snapshot)
            .map_err(EcsSnapshotError::NonFighterCodec)?;
        let fighter_plan = fighter_codec
            .prepare_restore(world, &snapshot.fighters)
            .map_err(EcsSnapshotError::FighterCodec)?;
        for object in &snapshot.dynamic_objects {
            let codec = self
                .dynamic_codecs
                .get(object.id.kind())
                .ok_or(EcsSnapshotError::MissingDynamicCodec(object.id.kind()))?;
            codec.validate_restore(world, object).map_err(|source| {
                EcsSnapshotError::DynamicCodec {
                    id: object.id,
                    source,
                }
            })?;
        }

        // Resolve exact-ID survivors only through a fully agreeing current bridge:
        // the allocator mapping and StableSimEntity component must both name the
        // same full generational ID. Untracked or inconsistent stable components
        // are cleanup candidates, never trusted as restore targets. Only genuinely
        // missing IDs are staged as new empty entities.
        let stable_entities = {
            let mut query = world.query::<(Entity, &StableSimEntity)>();
            let entities = query
                .iter(world)
                .map(|(entity, stable)| (stable.id(), entity))
                .collect::<Vec<_>>();
            entities
        };
        let allocator_entities = world
            .get_resource::<SimulationIdentityAllocator>()
            .map(|identities| {
                let mut entities = Vec::new();
                for kind in SimEntityKind::ALL {
                    for index in 0..identities.capacity(kind) {
                        if let Some((_, entity)) = identities.entry_at(kind, index)
                            && world.get_entity(entity).is_ok()
                        {
                            entities.push(entity);
                        }
                    }
                }
                entities
            })
            .unwrap_or_default();

        let mut bindings = Vec::with_capacity(snapshot.dynamic_objects.len());
        for object in &snapshot.dynamic_objects {
            let mapped = world
                .get_resource::<SimulationIdentityAllocator>()
                .and_then(|identities| identities.mapped_entity(object.id))
                .filter(|entity| {
                    world
                        .get::<StableSimEntity>(*entity)
                        .is_some_and(|stable| stable.id() == object.id)
                });
            let (entity, created) = if let Some(entity) = mapped {
                (entity, false)
            } else {
                let entity = world.spawn_empty().id();
                (entity, true)
            };
            bindings.push(DynamicRestoreBinding {
                id: object.id,
                entity,
                created,
            });
        }

        // Construct the exact allocator before mutating any pre-existing entity
        // or resource. If reconstruction detects an invalid local binding, only
        // the newly staged entities are discarded; the old world is untouched.
        let restored_identities = reconstruct_identities_or_discard_staging(
            world,
            snapshot,
            contract.pool_capacities,
            &bindings,
        )?;

        let mut previous_entities = stable_entities
            .iter()
            .map(|(_, entity)| *entity)
            .chain(allocator_entities)
            .collect::<Vec<_>>();
        previous_entities.sort_unstable_by_key(|entity| entity.to_bits());
        previous_entities.dedup();
        let mut reused_entities = bindings
            .iter()
            .filter_map(|binding| (!binding.created).then_some(binding.entity))
            .collect::<Vec<_>>();
        reused_entities.sort_unstable_by_key(|entity| entity.to_bits());
        reused_entities.dedup();
        let removed_entities = previous_entities
            .into_iter()
            .filter(|entity| {
                reused_entities
                    .binary_search_by_key(&entity.to_bits(), |candidate| candidate.to_bits())
                    .is_err()
            })
            .collect::<Vec<_>>();
        for entity in &removed_entities {
            let _ = world.despawn(*entity);
        }

        // Make the entire stable-ID bridge and all non-fighter resources visible
        // before codecs rebuild component relationships. Exact-ID survivors keep
        // all presentation-only components; every target receives its canonical
        // stable identity before forward relationships are resolved.
        for binding in bindings.iter().copied() {
            world
                .entity_mut(binding.entity)
                .insert(StableSimEntity::new(binding.id));
        }
        world.insert_resource(restored_identities);
        world.insert_resource(snapshot.header.tick);
        non_fighter_codec.commit_restore(world, non_fighter_plan);

        for (binding, object) in bindings.iter().copied().zip(&snapshot.dynamic_objects) {
            debug_assert_eq!(binding.id, object.id);
            self.dynamic_codecs
                .get(binding.id.kind())
                .expect("all dynamic codecs were validated before commit")
                .restore_validated(world, binding.entity, object);
        }

        fighter_codec.commit_restore(world, fighter_plan);

        Ok(EcsSnapshotRestoreReport {
            restored_tick: snapshot.header.tick,
            reused_dynamic_entities: reused_entities.len(),
            created_dynamic_entities: bindings.iter().filter(|binding| binding.created).count(),
            removed_dynamic_entities: removed_entities.len(),
            restored_dynamic_entities: bindings.len(),
        })
    }

    fn validate_live_identity_bridge(&self, world: &World) -> Result<(), EcsSnapshotError> {
        let identities = world.get_resource::<SimulationIdentityAllocator>().ok_or(
            EcsSnapshotError::MissingResource("SimulationIdentityAllocator"),
        )?;
        for kind in SimEntityKind::ALL {
            for index in 0..identities.capacity(kind) {
                let Some((id, entity)) = identities.entry_at(kind, index) else {
                    continue;
                };
                let Some(stable) = world.get::<StableSimEntity>(entity) else {
                    return Err(EcsSnapshotError::AllocatorEntityMissingStableComponent {
                        id,
                        entity,
                    });
                };
                if stable.id() != id {
                    return Err(EcsSnapshotError::AllocatorEntityStableIdMismatch {
                        expected: id,
                        found: stable.id(),
                        entity,
                    });
                }
            }
        }

        for archetype in world.archetypes().iter() {
            for entry in archetype.entities() {
                let entity = entry.id();
                if let Some(stable) = world.get::<StableSimEntity>(entity)
                    && identities.mapped_entity(stable.id()) != Some(entity)
                {
                    return Err(EcsSnapshotError::UntrackedStableEntity {
                        id: stable.id(),
                        entity,
                    });
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::determinism::{DeterministicRngStream, RngStreamName};
    use crate::ecs_identity::{SimulationIdentityOverflow, SimulationIdentityRestoreError};
    use crate::snapshot::PoolAllocatorSnapshot;

    #[derive(Resource, Clone, Debug, PartialEq, Eq)]
    struct TestFighters([FighterSnapshot; FIGHTER_CAPACITY as usize]);

    #[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
    struct PresentationOnlyMarker(u32);

    #[derive(Clone, Copy)]
    struct TestFighterCodec;

    impl FighterSnapshotCodec for TestFighterCodec {
        type RestorePlan = TestFighters;

        fn capture_fighters(
            &self,
            world: &World,
        ) -> Result<[FighterSnapshot; FIGHTER_CAPACITY as usize], SnapshotCodecError> {
            world
                .get_resource::<TestFighters>()
                .map(|fighters| fighters.0)
                .ok_or(SnapshotCodecError::new(1, "missing test fighters"))
        }

        fn prepare_restore(
            &self,
            _world: &World,
            fighters: &[FighterSnapshot; FIGHTER_CAPACITY as usize],
        ) -> Result<Self::RestorePlan, SnapshotCodecError> {
            Ok(TestFighters(*fighters))
        }

        fn commit_restore(&self, world: &mut World, plan: Self::RestorePlan) {
            world.insert_resource(plan);
        }
    }

    #[derive(Resource, Clone, Debug, PartialEq, Eq)]
    struct TestLiveNonFighter(CanonicalNonFighterState);

    #[derive(Clone, Copy)]
    struct TestLiveNonFighterCodec;

    impl NonFighterSnapshotCodec for TestLiveNonFighterCodec {
        type RestorePlan = TestLiveNonFighter;

        fn snapshot_contract(&self, world: &World) -> Result<SnapshotContract, SnapshotCodecError> {
            world
                .get_resource::<TestLiveNonFighter>()
                .map(|state| state.0.contract)
                .ok_or(SnapshotCodecError::new(1, "missing live fixture state"))
        }

        fn capture_non_fighter(
            &self,
            world: &World,
            _tick: SimTick,
        ) -> Result<CanonicalNonFighterState, SnapshotCodecError> {
            world
                .get_resource::<TestLiveNonFighter>()
                .map(|state| state.0.clone())
                .ok_or(SnapshotCodecError::new(1, "missing live fixture state"))
        }

        fn prepare_restore(
            &self,
            world: &World,
            snapshot: &CanonicalSnapshot,
        ) -> Result<Self::RestorePlan, SnapshotCodecError> {
            let contract = world
                .get_resource::<TestLiveNonFighter>()
                .map(|state| state.0.contract)
                .ok_or(SnapshotCodecError::new(1, "missing live fixture state"))?;
            contract
                .validate_header(&snapshot.header)
                .map_err(|_| SnapshotCodecError::new(2, "fixture contract mismatch"))?;
            Ok(TestLiveNonFighter(CanonicalNonFighterState {
                contract,
                match_state: snapshot.match_state,
                arena: snapshot.arena.clone(),
                rng_streams: snapshot.rng_streams.clone(),
                stats: snapshot.stats.clone(),
            }))
        }

        fn commit_restore(&self, world: &mut World, plan: Self::RestorePlan) {
            world.insert_resource(plan);
        }
    }

    fn contract() -> SnapshotContract {
        SnapshotContract {
            simulation_version: 7,
            protocol_version: 3,
            gameplay_content_hash: 0xAFC0,
            match_id: [9; MATCH_ID_BYTES],
            master_seed: 0x1234_5678,
            pool_capacities: capacities(2),
        }
    }

    fn empty_fighters() -> [FighterSnapshot; FIGHTER_CAPACITY as usize] {
        FighterId::ALL.map(FighterSnapshot::empty)
    }

    fn non_fighter_state() -> CanonicalNonFighterState {
        let mut item_rng = DeterministicRngStream::from_master_seed(
            contract().master_seed,
            RngStreamName::from_label("items"),
        );
        let _ = item_rng.next_u64();
        CanonicalNonFighterState {
            contract: contract(),
            match_state: MatchStateSnapshot::default(),
            arena: ArenaRuntimeSnapshot {
                arena_ticks: 77,
                ..ArenaRuntimeSnapshot::default()
            },
            rng_streams: vec![item_rng.snapshot().into()],
            stats: MatchStatsSnapshot {
                gameplay_ticks: 42,
                ..MatchStatsSnapshot::default()
            },
        }
    }

    fn capacities(per_kind: u32) -> [u32; SimEntityKind::ALL.len()] {
        [per_kind; SimEntityKind::ALL.len()]
    }

    fn insert_stable(
        world: &mut World,
        kind: SimEntityKind,
        state: CanonicalDynamicState,
    ) -> Result<(Entity, StableSimEntity), SimulationIdentityOverflow> {
        let entity = world.spawn_empty().id();
        let stable = world
            .resource_mut::<SimulationIdentityAllocator>()
            .try_allocate(kind, entity)?;
        world.entity_mut(entity).insert((stable, state));
        Ok((entity, stable))
    }

    fn test_world() -> World {
        let mut world = World::new();
        world.insert_resource(SimTick(42));
        world.insert_resource(non_fighter_state());
        world.insert_resource(TestFighters(empty_fighters()));
        world.insert_resource(SimulationIdentityAllocator::with_capacities(capacities(2)));
        world
    }

    #[test]
    fn capture_and_restore_round_trip_exact_ecs_allocator_and_dynamic_state() {
        let adapter =
            EcsSnapshotAdapter::new(DynamicSnapshotCodecRegistry::canonical_state_for_all_kinds());
        let mut world = test_world();

        let (released_entity, released) = insert_stable(
            &mut world,
            SimEntityKind::Item,
            CanonicalDynamicState::default(),
        )
        .unwrap();
        let mut live_state = CanonicalDynamicState {
            definition_id: 17,
            flags: 0x55,
            owner: None,
            target: None,
            related_entity: None,
            fighter_hit_mask: FighterHitMask::default(),
            payload: [0; DYNAMIC_PAYLOAD_BYTES],
        };
        live_state.payload[0..4].copy_from_slice(&123_i32.to_le_bytes());
        let (live_entity, live) =
            insert_stable(&mut world, SimEntityKind::Item, live_state.clone()).unwrap();
        world
            .entity_mut(live_entity)
            .insert(PresentationOnlyMarker(0xAFC));
        let rejected_entity = world.spawn_empty().id();
        assert!(
            world
                .resource_mut::<SimulationIdentityAllocator>()
                .try_allocate(SimEntityKind::Item, rejected_entity)
                .is_err()
        );
        assert!(world.despawn(rejected_entity));
        assert!(
            world
                .resource_mut::<SimulationIdentityAllocator>()
                .release(released_entity, released)
        );
        assert!(world.despawn(released_entity));

        let snapshot = adapter.capture(&mut world, &TestFighterCodec).unwrap();
        assert_eq!(snapshot.dynamic_objects.len(), 1);
        assert_eq!(snapshot.dynamic_objects[0].id, live.id());
        assert_eq!(snapshot.dynamic_objects[0].definition_id, 17);
        assert_eq!(
            snapshot.stats.rejected_dynamic_spawns[SimEntityKind::Item.code() as usize],
            1
        );
        let encoded = snapshot.encode().unwrap();
        assert_eq!(CanonicalSnapshot::decode(&encoded).unwrap(), snapshot);

        // Record the exact next allocation, then corrupt every captured section.
        let (temporary_entity, expected_next) = insert_stable(
            &mut world,
            SimEntityKind::Item,
            CanonicalDynamicState::default(),
        )
        .unwrap();
        assert_eq!(expected_next.id().index(), 0);
        world.insert_resource(SimTick(900));
        world
            .resource_mut::<CanonicalNonFighterState>()
            .arena
            .arena_ticks = 999;
        world
            .get_mut::<CanonicalDynamicState>(live_entity)
            .unwrap()
            .definition_id = 99;

        let report = adapter
            .restore(&mut world, &snapshot, &TestFighterCodec)
            .unwrap();
        assert_eq!(report.restored_tick, SimTick(42));
        assert_eq!(report.reused_dynamic_entities, 1);
        assert_eq!(report.created_dynamic_entities, 0);
        assert_eq!(report.removed_dynamic_entities, 1);
        assert_eq!(report.restored_dynamic_entities, 1);
        assert_eq!(
            world.get::<PresentationOnlyMarker>(live_entity),
            Some(&PresentationOnlyMarker(0xAFC))
        );
        assert_eq!(
            world
                .get::<CanonicalDynamicState>(live_entity)
                .unwrap()
                .definition_id,
            17
        );
        assert!(world.get_entity(temporary_entity).is_err());

        let recaptured = adapter.capture(&mut world, &TestFighterCodec).unwrap();
        assert_eq!(recaptured, snapshot);
        assert_eq!(
            world
                .resource::<SimulationIdentityAllocator>()
                .rejected_spawns(SimEntityKind::Item),
            1
        );
        let replacement_entity = world.spawn_empty().id();
        let restored_next = world
            .resource_mut::<SimulationIdentityAllocator>()
            .try_allocate(SimEntityKind::Item, replacement_entity)
            .unwrap();
        assert_eq!(restored_next.id(), expected_next.id());
    }

    #[test]
    fn restore_reuses_exact_ids_but_replaces_a_future_generation_in_the_same_slot() {
        let adapter =
            EcsSnapshotAdapter::new(DynamicSnapshotCodecRegistry::canonical_state_for_all_kinds());
        let mut world = test_world();

        let (past_entity, past) = insert_stable(
            &mut world,
            SimEntityKind::Item,
            CanonicalDynamicState {
                definition_id: 10,
                ..CanonicalDynamicState::default()
            },
        )
        .unwrap();
        let (survivor_entity, survivor) = insert_stable(
            &mut world,
            SimEntityKind::Item,
            CanonicalDynamicState {
                definition_id: 20,
                ..CanonicalDynamicState::default()
            },
        )
        .unwrap();
        world
            .entity_mut(survivor_entity)
            .insert(PresentationOnlyMarker(77));
        let snapshot = adapter.capture(&mut world, &TestFighterCodec).unwrap();

        assert!(
            world
                .resource_mut::<SimulationIdentityAllocator>()
                .release(past_entity, past)
        );
        assert!(world.despawn(past_entity));
        let (future_entity, future) = insert_stable(
            &mut world,
            SimEntityKind::Item,
            CanonicalDynamicState {
                definition_id: 99,
                ..CanonicalDynamicState::default()
            },
        )
        .unwrap();
        assert_eq!(future.id().index(), past.id().index());
        assert!(future.id().generation() > past.id().generation());
        let untracked_duplicate = world
            .spawn((
                StableSimEntity::new(survivor.id()),
                PresentationOnlyMarker(88),
            ))
            .id();
        world
            .get_mut::<CanonicalDynamicState>(survivor_entity)
            .unwrap()
            .definition_id = 88;

        let report = adapter
            .restore(&mut world, &snapshot, &TestFighterCodec)
            .unwrap();
        assert_eq!(report.reused_dynamic_entities, 1);
        assert_eq!(report.created_dynamic_entities, 1);
        assert_eq!(report.removed_dynamic_entities, 2);
        assert_eq!(report.restored_dynamic_entities, 2);
        assert!(world.get_entity(future_entity).is_err());
        assert!(world.get_entity(untracked_duplicate).is_err());
        assert_eq!(
            world.get::<PresentationOnlyMarker>(survivor_entity),
            Some(&PresentationOnlyMarker(77))
        );

        let identities = world.resource::<SimulationIdentityAllocator>();
        let restored_past_entity = identities.mapped_entity(past.id()).unwrap();
        assert_ne!(restored_past_entity, future_entity);
        assert_eq!(
            identities.mapped_entity(survivor.id()),
            Some(survivor_entity)
        );
        assert_eq!(
            world
                .get::<CanonicalDynamicState>(restored_past_entity)
                .unwrap()
                .definition_id,
            10
        );
        assert_eq!(
            world
                .get::<CanonicalDynamicState>(survivor_entity)
                .unwrap()
                .definition_id,
            20
        );
        assert_eq!(
            adapter.capture(&mut world, &TestFighterCodec).unwrap(),
            snapshot
        );
    }

    #[test]
    fn allocator_reconstruction_failure_discards_only_new_staging_entities() {
        let adapter =
            EcsSnapshotAdapter::new(DynamicSnapshotCodecRegistry::canonical_state_for_all_kinds());
        let mut source = test_world();
        source
            .resource_mut::<CanonicalNonFighterState>()
            .contract
            .pool_capacities = capacities(3);
        source.insert_resource(SimulationIdentityAllocator::with_capacities(capacities(3)));
        for definition_id in [1, 2, 3] {
            insert_stable(
                &mut source,
                SimEntityKind::Item,
                CanonicalDynamicState {
                    definition_id,
                    ..CanonicalDynamicState::default()
                },
            )
            .unwrap();
        }
        let snapshot = adapter.capture(&mut source, &TestFighterCodec).unwrap();
        assert_eq!(snapshot.dynamic_objects.len(), 3);

        let mut world = test_world();
        let old_entity = world.spawn(PresentationOnlyMarker(101)).id();
        let unrelated_entity = world.spawn(PresentationOnlyMarker(202)).id();
        let staged_entity = world.spawn_empty().id();
        let bindings = [
            DynamicRestoreBinding {
                id: snapshot.dynamic_objects[0].id,
                entity: old_entity,
                created: false,
            },
            DynamicRestoreBinding {
                id: snapshot.dynamic_objects[1].id,
                entity: old_entity,
                created: false,
            },
            DynamicRestoreBinding {
                id: snapshot.dynamic_objects[2].id,
                entity: staged_entity,
                created: true,
            },
        ];
        let before_tick = *world.resource::<SimTick>();
        let before_allocator = world
            .resource::<SimulationIdentityAllocator>()
            .capture_allocator_snapshots();

        let result = reconstruct_identities_or_discard_staging(
            &mut world,
            &snapshot,
            capacities(3),
            &bindings,
        );
        assert!(matches!(
            result,
            Err(EcsSnapshotError::IdentityRestore(
                SimulationIdentityRestoreError::DuplicateEntity(entity)
            )) if entity == old_entity
        ));
        assert_eq!(*world.resource::<SimTick>(), before_tick);
        assert_eq!(
            world
                .resource::<SimulationIdentityAllocator>()
                .capture_allocator_snapshots(),
            before_allocator
        );
        assert_eq!(
            world.get::<PresentationOnlyMarker>(old_entity),
            Some(&PresentationOnlyMarker(101))
        );
        assert_eq!(
            world.get::<PresentationOnlyMarker>(unrelated_entity),
            Some(&PresentationOnlyMarker(202))
        );
        assert!(world.get_entity(staged_entity).is_err());
    }

    #[test]
    fn custom_non_fighter_codec_uses_live_resources_without_a_mirrored_resource() {
        let adapter =
            EcsSnapshotAdapter::new(DynamicSnapshotCodecRegistry::canonical_state_for_all_kinds());
        let mut world = test_world();
        let canonical = world.remove_resource::<CanonicalNonFighterState>().unwrap();
        world.insert_resource(TestLiveNonFighter(canonical));

        let snapshot = adapter
            .capture_with_non_fighter(&mut world, &TestLiveNonFighterCodec, &TestFighterCodec)
            .unwrap();
        world
            .resource_mut::<TestLiveNonFighter>()
            .0
            .arena
            .arena_ticks = 999;
        world.insert_resource(SimTick(900));

        let report = adapter
            .restore_with_non_fighter(
                &mut world,
                &snapshot,
                &TestLiveNonFighterCodec,
                &TestFighterCodec,
            )
            .unwrap();
        assert_eq!(report.restored_tick, SimTick(42));
        assert_eq!(
            world.resource::<TestLiveNonFighter>().0.arena.arena_ticks,
            77
        );
        assert!(!world.contains_resource::<CanonicalNonFighterState>());
        assert_eq!(
            adapter
                .capture_with_non_fighter(&mut world, &TestLiveNonFighterCodec, &TestFighterCodec,)
                .unwrap(),
            snapshot
        );
    }

    #[test]
    fn capture_rejects_allocator_capacity_outside_the_match_contract() {
        let adapter =
            EcsSnapshotAdapter::new(DynamicSnapshotCodecRegistry::canonical_state_for_all_kinds());
        let mut world = test_world();
        let mut mismatched = capacities(2);
        mismatched[SimEntityKind::Item.code() as usize] = 3;
        world.insert_resource(SimulationIdentityAllocator::with_capacities(mismatched));

        assert_eq!(
            adapter.capture(&mut world, &TestFighterCodec),
            Err(EcsSnapshotError::CaptureAllocatorCapacityMismatch {
                kind: SimEntityKind::Item,
                expected: 2,
                found: 3,
            })
        );
    }

    #[test]
    fn restore_rejects_snapshot_capacity_outside_contract_before_mutation() {
        let adapter =
            EcsSnapshotAdapter::new(DynamicSnapshotCodecRegistry::canonical_state_for_all_kinds());
        let mut world = test_world();
        let mut snapshot = adapter.capture(&mut world, &TestFighterCodec).unwrap();
        let item_index = SimEntityKind::Item.code() as usize;
        snapshot.allocators[item_index] =
            PoolAllocatorSnapshot::empty(SimEntityKind::Item, 3).unwrap();
        snapshot.validate().unwrap();

        world.insert_resource(SimTick(900));
        world
            .resource_mut::<CanonicalNonFighterState>()
            .arena
            .arena_ticks = 999;
        let before_allocator = world
            .resource::<SimulationIdentityAllocator>()
            .capture_allocator_snapshots();

        assert_eq!(
            adapter.restore(&mut world, &snapshot, &TestFighterCodec),
            Err(EcsSnapshotError::RestoreAllocatorCapacityMismatch {
                kind: SimEntityKind::Item,
                expected: 2,
                found: 3,
            })
        );
        assert_eq!(*world.resource::<SimTick>(), SimTick(900));
        assert_eq!(
            world
                .resource::<CanonicalNonFighterState>()
                .arena
                .arena_ticks,
            999
        );
        assert_eq!(
            world
                .resource::<SimulationIdentityAllocator>()
                .capture_allocator_snapshots(),
            before_allocator
        );
    }

    #[test]
    fn capture_rejects_allocator_and_ecs_identity_disagreement() {
        let adapter =
            EcsSnapshotAdapter::new(DynamicSnapshotCodecRegistry::canonical_state_for_all_kinds());
        let mut world = test_world();
        let (entity, stable) = insert_stable(
            &mut world,
            SimEntityKind::Special,
            CanonicalDynamicState::default(),
        )
        .unwrap();
        world.entity_mut(entity).remove::<StableSimEntity>();
        assert_eq!(
            adapter.capture(&mut world, &TestFighterCodec),
            Err(EcsSnapshotError::AllocatorEntityMissingStableComponent {
                id: stable.id(),
                entity,
            })
        );
    }

    #[test]
    fn missing_dynamic_codec_fails_closed() {
        let mut world = test_world();
        let (_, stable) = insert_stable(
            &mut world,
            SimEntityKind::BeeSkill,
            CanonicalDynamicState::default(),
        )
        .unwrap();
        let adapter = EcsSnapshotAdapter::new(DynamicSnapshotCodecRegistry::new());
        assert_eq!(
            adapter.capture(&mut world, &TestFighterCodec),
            Err(EcsSnapshotError::MissingDynamicCodec(stable.id().kind()))
        );
    }

    struct RejectRestoreCodec;

    impl DynamicSnapshotCodec for RejectRestoreCodec {
        fn capture(
            &self,
            world: &World,
            entity: Entity,
            id: SimEntityId,
        ) -> Result<DynamicObjectSnapshot, SnapshotCodecError> {
            CanonicalDynamicStateCodec.capture(world, entity, id)
        }

        fn validate_restore(
            &self,
            _world: &World,
            _snapshot: &DynamicObjectSnapshot,
        ) -> Result<(), SnapshotCodecError> {
            Err(SnapshotCodecError::new(88, "fixture rejection"))
        }

        fn restore_validated(
            &self,
            _world: &mut World,
            _entity: Entity,
            _snapshot: &DynamicObjectSnapshot,
        ) {
            unreachable!("rejected restore must not enter commit phase")
        }
    }

    #[test]
    fn restore_preflight_failure_leaves_world_unchanged() {
        let capture_adapter =
            EcsSnapshotAdapter::new(DynamicSnapshotCodecRegistry::canonical_state_for_all_kinds());
        let mut world = test_world();
        let (entity, stable) = insert_stable(
            &mut world,
            SimEntityKind::Special,
            CanonicalDynamicState::default(),
        )
        .unwrap();
        let snapshot = capture_adapter
            .capture(&mut world, &TestFighterCodec)
            .unwrap();

        let mut registry = DynamicSnapshotCodecRegistry::new();
        registry
            .register(SimEntityKind::Special, RejectRestoreCodec)
            .unwrap();
        let restore_adapter = EcsSnapshotAdapter::new(registry);
        let before_tick = *world.resource::<SimTick>();
        let before_allocators = world
            .resource::<SimulationIdentityAllocator>()
            .capture_allocator_snapshots();
        assert!(matches!(
            restore_adapter.restore(&mut world, &snapshot, &TestFighterCodec),
            Err(EcsSnapshotError::DynamicCodec {
                id,
                source: SnapshotCodecError { code: 88, .. }
            }) if id == stable.id()
        ));
        assert_eq!(*world.resource::<SimTick>(), before_tick);
        assert!(world.get_entity(entity).is_ok());
        assert_eq!(
            world
                .resource::<SimulationIdentityAllocator>()
                .capture_allocator_snapshots(),
            before_allocators
        );
    }

    #[test]
    fn restore_rejects_another_match_before_mutation() {
        let adapter =
            EcsSnapshotAdapter::new(DynamicSnapshotCodecRegistry::canonical_state_for_all_kinds());
        let mut world = test_world();
        let mut snapshot = adapter.capture(&mut world, &TestFighterCodec).unwrap();
        snapshot.header.match_id = [8; MATCH_ID_BYTES];
        let before_tick = *world.resource::<SimTick>();
        assert_eq!(
            adapter.restore(&mut world, &snapshot, &TestFighterCodec),
            Err(EcsSnapshotError::ContractMismatch { field: "match ID" })
        );
        assert_eq!(*world.resource::<SimTick>(), before_tick);
    }

    #[test]
    fn registry_is_fixed_by_kind_and_rejects_duplicates() {
        let mut registry = DynamicSnapshotCodecRegistry::new();
        registry
            .register(SimEntityKind::Hitbox, CanonicalDynamicStateCodec)
            .unwrap();
        assert!(registry.contains(SimEntityKind::Hitbox));
        assert_eq!(
            registry.register(SimEntityKind::Hitbox, CanonicalDynamicStateCodec),
            Err(DynamicCodecRegistrationError::DuplicateKind(
                SimEntityKind::Hitbox
            ))
        );
    }

    #[test]
    fn empty_fighter_codec_is_strict() {
        let codec = EmptyFighterSnapshotCodec;
        let world = World::new();
        let empty = empty_fighters();
        assert_eq!(codec.capture_fighters(&world).unwrap(), empty);
        assert_eq!(codec.prepare_restore(&world, &empty), Ok(()));
        let mut occupied = empty;
        occupied[0].occupied = true;
        assert!(codec.prepare_restore(&world, &occupied).is_err());
    }
}
