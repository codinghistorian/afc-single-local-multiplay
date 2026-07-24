//! Versioned, bounded, canonical simulation snapshots.
//!
//! This module defines the wire-independent rollback state schema. The Bevy bridge
//! lives in [`crate::snapshot_ecs`], keeping ECS handles and component types out of
//! the canonical representation. All encoded integers use explicit little-endian
//! byte order, vector lengths are bounded before allocation, and variable
//! collections must already be in their documented canonical order.

use crate::determinism::{
    CanonicalHash64, FIGHTER_CAPACITY, FighterId, RngSnapshot, RngStreamName, SimEntityId,
    SimEntityKind,
};
use crate::simulation::SimTick;
use std::error::Error;
use std::fmt;
use std::time::{Duration, Instant};

pub const SNAPSHOT_MAGIC: [u8; 4] = *b"AFCS";
pub const SNAPSHOT_SCHEMA_VERSION: u16 = 2;
pub const SNAPSHOT_QUANTIZATION_UNITS: u32 = 4_096;

pub const DYNAMIC_PAYLOAD_BYTES: usize = 128;
pub const ARENA_PAYLOAD_BYTES: usize = 64;
pub const MAX_POOL_CAPACITY: usize = 1_024;
pub const MAX_TOTAL_POOL_SLOTS: usize = 4_096;
pub const MAX_DYNAMIC_OBJECTS: usize = 2_048;
pub const MAX_RNG_STREAMS: usize = 64;
pub const MAX_SNAPSHOT_BYTES: usize = 512 * 1_024;

pub const MIN_SNAPSHOT_HISTORY: usize = 32;
pub const MAX_SNAPSHOT_HISTORY: usize = 512;

pub const MATCH_ID_BYTES: usize = 16;
pub const EQUIPMENT_SLOTS: usize = 4;
pub const COOLDOWN_SLOTS: usize = 8;
pub const STATUS_TIMER_SLOTS: usize = 8;
pub const SIM_ENTITY_KIND_COUNT: usize = SimEntityKind::ALL.len();

/// The exact wire width of [`FighterRollbackExtensionSnapshot`].
///
/// This is deliberately fixed: adding a field requires a schema-version bump,
/// and decoding never allocates or follows an attacker-controlled length.
pub const FIGHTER_ROLLBACK_EXTENSION_BYTES: usize = 243;

/// Stable schema-v2 discriminant counts. The ECS bridge must map the gameplay
/// enums explicitly rather than relying on Rust's unspecified enum layout.
pub const FIGHTER_ACTION_CODE_COUNT: u16 = 36;
pub const TECHNIQUE_CODE_COUNT: u16 = 95;
pub const TECHNIQUE_BUTTON_CODE_COUNT: u8 = 9;
pub const REACTION_FAMILY_CODE_COUNT: u8 = 14;
pub const DAMAGE_ELEMENT_CODE_COUNT: u8 = 8;

/// Header fields that bind a snapshot to one simulation/content contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotHeader {
    pub schema_version: u16,
    pub simulation_version: u32,
    pub protocol_version: u32,
    pub gameplay_content_hash: u64,
    pub match_id: [u8; MATCH_ID_BYTES],
    pub tick: SimTick,
    pub master_seed: u64,
    pub quantization_units_per_unit: u32,
}

impl SnapshotHeader {
    pub const fn new(
        simulation_version: u32,
        protocol_version: u32,
        gameplay_content_hash: u64,
        match_id: [u8; MATCH_ID_BYTES],
        tick: SimTick,
        master_seed: u64,
    ) -> Self {
        Self {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            simulation_version,
            protocol_version,
            gameplay_content_hash,
            match_id,
            tick,
            master_seed,
            quantization_units_per_unit: SNAPSHOT_QUANTIZATION_UNITS,
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MatchPhaseSnapshot {
    #[default]
    Setup = 0,
    Countdown = 1,
    Fight = 2,
    SuddenDeath = 3,
    Result = 4,
    TimeUp = 5,
    Resetting = 6,
}

impl MatchPhaseSnapshot {
    const fn code(self) -> u8 {
        self as u8
    }

    const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Setup),
            1 => Some(Self::Countdown),
            2 => Some(Self::Fight),
            3 => Some(Self::SuddenDeath),
            4 => Some(Self::Result),
            5 => Some(Self::TimeUp),
            6 => Some(Self::Resetting),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MatchResultSnapshot {
    #[default]
    Pending,
    Draw {
        decided_tick: SimTick,
    },
    FighterWinner {
        fighter: FighterId,
        decided_tick: SimTick,
    },
    TeamWinner {
        team: u8,
        decided_tick: SimTick,
    },
    Aborted {
        reason: u16,
        decided_tick: SimTick,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MatchRulesSnapshot {
    pub ruleset_id: u32,
    pub arena_id: u32,
    pub duration_ticks: u32,
    pub starting_stocks: u8,
    pub score_limit: u16,
    pub team_mode: bool,
    pub friendly_fire: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MatchStateSnapshot {
    pub phase: MatchPhaseSnapshot,
    pub phase_ticks: u32,
    pub match_ticks_remaining: u32,
    pub hitstop_ticks: u32,
    pub next_event_ordinal: u32,
    /// Bit `n` is set exactly when fighter slot `n` is active.
    pub active_slots_mask: u8,
    pub teams: [u8; FIGHTER_CAPACITY as usize],
    pub stocks: [u8; FIGHTER_CAPACITY as usize],
    pub rules: MatchRulesSnapshot,
    pub result: MatchResultSnapshot,
}

/// Quantized X/Z value. The scale is recorded in [`SnapshotHeader`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QuantizedVec2 {
    pub x: i32,
    pub z: i32,
}

/// Quantized X/Y/Z value. The scale is recorded in [`SnapshotHeader`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QuantizedVec3 {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FighterInputSnapshot {
    pub move_x: i16,
    pub move_y: i16,
    pub held_buttons: u32,
    pub pressed_latches: u32,
    pub released_latches: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FighterPoseSnapshot {
    pub position: QuantizedVec3,
    pub velocity: QuantizedVec3,
    pub facing: QuantizedVec2,
    pub grounded: bool,
    pub collision_flags: u16,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FighterActionSnapshot {
    pub action_id: u16,
    pub elapsed_ticks: u32,
    pub flags: u32,
    pub buffered_action_id: u16,
    pub reaction_id: u16,
    pub reaction_ticks: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FighterCooldownSnapshot {
    /// Stable semantic slots defined by the simulation version.
    pub ticks: [u32; COOLDOWN_SLOTS],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FighterStatusSnapshot {
    pub flags: u64,
    /// Stable semantic slots defined by the simulation version.
    pub timers: [u32; STATUS_TIMER_SLOTS],
    pub elemental_carry: i32,
    pub size_scale: i32,
    pub speed_scale: i32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FighterLoadoutSnapshot {
    pub character_id: u16,
    pub style_id: u16,
    pub move_set_id: u16,
    pub equipment_ids: [u16; EQUIPMENT_SLOTS],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FighterRelationshipsSnapshot {
    pub held_item: Option<SimEntityId>,
    pub linked_entity: Option<SimEntityId>,
    pub holding: Option<FighterId>,
    pub held_by: Option<FighterId>,
    pub ultimate_owner: Option<FighterId>,
    pub ultimate_target: Option<FighterId>,
    pub last_attacker: Option<FighterId>,
}

/// Three finite `f32` values stored by raw IEEE-754 bits.
///
/// Canonical simulation values are finite. Keeping their bits (instead of a
/// second quantization pass) makes capture/restore lossless, including `-0.0`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct F32Vec3BitsSnapshot {
    pub x: u32,
    pub y: u32,
    pub z: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct F32Vec2BitsSnapshot {
    pub x: u32,
    pub y: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OptionalU8CodeSnapshot {
    pub present: bool,
    /// Must be zero when `present` is false.
    pub code: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OptionalU16CodeSnapshot {
    pub present: bool,
    /// Must be zero when `present` is false.
    pub code: u16,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OptionalU32Snapshot {
    pub present: bool,
    /// Must be zero when `present` is false.
    pub value: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OptionalF32Vec3BitsSnapshot {
    pub present: bool,
    /// Must be all-zero when `present` is false.
    pub value: F32Vec3BitsSnapshot,
}

/// Rollback-relevant portion of `QueuedAftermath`.
///
/// `QueuedAftermath::cue` is intentionally excluded: it is a presentation SFX
/// key re-derived from this complete canonical tuple. Restore rejects an
/// unknown or ambiguous tuple instead of retaining future-side cue state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct QueuedAftermathSnapshot {
    pub present: bool,
    /// Schema-v2 `ReactionFamilyId` discriminant; zero when absent.
    pub family_code: u8,
    pub getup_transition_ms: u32,
    pub recover_ms: u32,
    pub landing_stick_ms: u32,
    pub horizontal_damping_bits: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FighterStatsRollbackSnapshot {
    /// Exact finite source bits. The outer `health` and `stamina` fields remain
    /// useful quantized summaries, but are not lossless for arbitrary `f32`s.
    pub health_bits: u32,
    pub stamina_bits: u32,
    pub invulnerability_ticks: u32,
    pub health_refill_ticks: u32,
    pub respawn_ticks: u32,
    /// Schema-v2 `DamageElement` discriminant.
    pub element_carry: OptionalU8CodeSnapshot,
    pub element_carry_strength_bits: u32,
    pub element_carry_ticks: u32,
    pub item_speed_ticks: u32,
    pub item_giant_ticks: u32,
}

pub const FIGHTER_MOTOR_KNOCKDOWN_ON_LAND: u16 = 1 << 0;
pub const FIGHTER_MOTOR_AIR_ATTACK_USED: u16 = 1 << 1;
pub const FIGHTER_MOTOR_JUMP_ATTACK_LANDING_RECOVERY: u16 = 1 << 2;
pub const FIGHTER_MOTOR_BEE_AIR_DASH_MOTION_ACTIVE: u16 = 1 << 3;
pub const FIGHTER_MOTOR_BEE_AIR_DASH_SHOT_AVAILABLE: u16 = 1 << 4;
pub const FIGHTER_MOTOR_GUARD_WAS_REQUESTED: u16 = 1 << 5;
pub const FIGHTER_MOTOR_GUARD_COUNTER_BUFFERED: u16 = 1 << 6;
const FIGHTER_MOTOR_FLAG_MASK: u16 = FIGHTER_MOTOR_KNOCKDOWN_ON_LAND
    | FIGHTER_MOTOR_AIR_ATTACK_USED
    | FIGHTER_MOTOR_JUMP_ATTACK_LANDING_RECOVERY
    | FIGHTER_MOTOR_BEE_AIR_DASH_MOTION_ACTIVE
    | FIGHTER_MOTOR_BEE_AIR_DASH_SHOT_AVAILABLE
    | FIGHTER_MOTOR_GUARD_WAS_REQUESTED
    | FIGHTER_MOTOR_GUARD_COUNTER_BUFFERED;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FighterMotorRollbackSnapshot {
    pub velocity: F32Vec3BitsSnapshot,
    pub facing: F32Vec3BitsSnapshot,
    /// The seven `FighterMotor` booleans not already stored as `pose.grounded`.
    pub flags: u16,
    pub landing_aftermath: QueuedAftermathSnapshot,
    /// Schema-v2 `TechniqueButton` discriminant.
    pub queued_air_attack: OptionalU8CodeSnapshot,
    pub queued_air_attack_ticks: u32,
    pub ledge_grace_ticks: u32,
    pub landing_stick_ticks: u32,
    pub jump_takeoff_ticks: u32,
    pub reaction_bounces: u8,
    pub pig_air_meat_slam_air_hits: u8,
    pub dash_slide_ticks: u32,
    pub dash_jump_carry_ticks: u32,
    pub dash_jump_carry_speed_limit_bits: u32,
    pub impact_speed_limit_ticks: u32,
    pub impact_speed_limit_bits: u32,
    pub penguin_ice_slide_direction: OptionalF32Vec3BitsSnapshot,
    pub penguin_ice_slide_speed_bits: u32,
    pub guard_active_elapsed_ticks: u32,
    pub guard_cooldown_ticks: u32,
    pub guard_start_buffer_ticks: u32,
    pub guard_counter_window_ticks: u32,
    pub guard_counter_source: OptionalF32Vec3BitsSnapshot,
}

pub const FIGHTER_ACTION_HITBOX_SPAWNED: u8 = 1 << 0;
pub const FIGHTER_ACTION_QUEUED_COMBO: u8 = 1 << 1;
pub const FIGHTER_ACTION_CONFIRMED_HIT: u8 = 1 << 2;
pub const FIGHTER_ACTION_CANCEL_WINDOW_OPEN: u8 = 1 << 3;
pub const FIGHTER_ACTION_BRANCH_WINDOW_OPEN: u8 = 1 << 4;
pub const FIGHTER_ACTION_CHARGE_RELEASE_REQUESTED: u8 = 1 << 5;
const FIGHTER_ACTION_ROLLBACK_FLAG_MASK: u8 = FIGHTER_ACTION_HITBOX_SPAWNED
    | FIGHTER_ACTION_QUEUED_COMBO
    | FIGHTER_ACTION_CONFIRMED_HIT
    | FIGHTER_ACTION_CANCEL_WINDOW_OPEN
    | FIGHTER_ACTION_BRANCH_WINDOW_OPEN
    | FIGHTER_ACTION_CHARGE_RELEASE_REQUESTED;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FighterActionRollbackSnapshot {
    /// The six rollback-relevant `FighterActionState` booleans.
    pub flags: u8,
    /// Schema-v2 `TechniqueId` discriminants.
    pub queued_technique: OptionalU16CodeSnapshot,
    /// Schema-v2 `TechniqueButton` discriminants.
    pub queued_button: OptionalU8CodeSnapshot,
    pub buffered_button: OptionalU8CodeSnapshot,
    pub buffered_button_elapsed_ticks: u32,
    pub technique_id: OptionalU16CodeSnapshot,
    pub timeline_events_fired: u64,
    pub reaction_getup_ms: OptionalU32Snapshot,
    pub reaction_recover_ms: OptionalU32Snapshot,
    /// Schema-v2 `ReactionFamilyId` discriminant.
    pub reaction_family: OptionalU8CodeSnapshot,
    pub charge_elapsed_ticks: u32,
}

/// Fixed-width, heap-free schema extension for rollback-relevant fighter ECS
/// components that are absent from, or lossy in, the original fighter record.
///
/// Audit mapping for fields intentionally kept in the outer record:
///
/// - `Fighter.id` -> [`FighterSnapshot::id`].
/// - `FighterStats.score` -> [`FighterSnapshot::score`].
/// - `FighterStats.last_attacker`, `FighterInventory.held`, grab links, and
///   ultimate links -> [`FighterSnapshot::relationships`].
/// - `DrunkStatus.remaining` -> `FighterStatusSnapshot::timers[6]`.
/// - `FighterMotor.grounded` -> [`FighterPoseSnapshot::grounded`].
/// - `FighterActionState.action` and `elapsed` ->
///   [`FighterActionSnapshot::action_id`] and
///   [`FighterActionSnapshot::elapsed_ticks`].
///
/// Presentation-only exclusions (none may feed authoritative gameplay):
///
/// - `Fighter.name` and `Fighter.color` label/render a fighter.
/// - `FighterStats.hud_flash` drives HUD feedback and is never read by a
///   semantic-event condition, ordering decision, or payload builder.
/// - `FighterActionState.reaction_visual_side` changes reaction posing only;
///   canonical reaction family and motion select all events.
/// - `QueuedAftermath::cue` is documented on [`QueuedAftermathSnapshot`] and is
///   re-derived before a landing presentation intent is emitted.
/// - Fighter `Transform` rotation and scale are render pose/size outputs derived
///   from facing and status; translation is retained exactly below.
///
/// Dash trails and drunk bubbles have no mutable presentation cadence fields:
/// their event ticks/phases derive from rollback-canonical
/// `FighterActionState::elapsed` and `DrunkStatus::remaining`, respectively.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FighterRollbackExtensionSnapshot {
    pub position: F32Vec3BitsSnapshot,
    pub input_movement: F32Vec2BitsSnapshot,
    pub spawn: F32Vec3BitsSnapshot,
    pub stats: FighterStatsRollbackSnapshot,
    pub motor: FighterMotorRollbackSnapshot,
    pub action: FighterActionRollbackSnapshot,
    pub regrab_lockout_ticks: u32,
}

impl FighterRollbackExtensionSnapshot {
    /// Validates finite float bits, enum ranges, reserved flag bits, and
    /// canonical zero padding for every fixed-width optional value.
    pub fn validate(self) -> Result<(), SnapshotError> {
        validate_fighter_rollback(self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FighterSnapshot {
    pub occupied: bool,
    pub active: bool,
    pub id: FighterId,
    pub input: FighterInputSnapshot,
    pub pose: FighterPoseSnapshot,
    /// Quantized using the header scale.
    pub health: i32,
    /// Quantized using the header scale.
    pub stamina: i32,
    pub score: i32,
    pub action: FighterActionSnapshot,
    pub cooldowns: FighterCooldownSnapshot,
    pub status: FighterStatusSnapshot,
    pub loadout: FighterLoadoutSnapshot,
    pub relationships: FighterRelationshipsSnapshot,
    pub rollback: FighterRollbackExtensionSnapshot,
}

impl FighterSnapshot {
    pub fn empty(id: FighterId) -> Self {
        Self {
            occupied: false,
            active: false,
            id,
            input: FighterInputSnapshot {
                move_x: 0,
                move_y: 0,
                held_buttons: 0,
                pressed_latches: 0,
                released_latches: 0,
            },
            pose: FighterPoseSnapshot {
                position: QuantizedVec3 { x: 0, y: 0, z: 0 },
                velocity: QuantizedVec3 { x: 0, y: 0, z: 0 },
                facing: QuantizedVec2 { x: 0, z: 0 },
                grounded: false,
                collision_flags: 0,
            },
            health: 0,
            stamina: 0,
            score: 0,
            action: FighterActionSnapshot {
                action_id: 0,
                elapsed_ticks: 0,
                flags: 0,
                buffered_action_id: 0,
                reaction_id: 0,
                reaction_ticks: 0,
            },
            cooldowns: FighterCooldownSnapshot {
                ticks: [0; COOLDOWN_SLOTS],
            },
            status: FighterStatusSnapshot {
                flags: 0,
                timers: [0; STATUS_TIMER_SLOTS],
                elemental_carry: 0,
                size_scale: 0,
                speed_scale: 0,
            },
            loadout: FighterLoadoutSnapshot {
                character_id: 0,
                style_id: 0,
                move_set_id: 0,
                equipment_ids: [0; EQUIPMENT_SLOTS],
            },
            relationships: FighterRelationshipsSnapshot {
                held_item: None,
                linked_entity: None,
                holding: None,
                held_by: None,
                ultimate_owner: None,
                ultimate_target: None,
                last_attacker: None,
            },
            rollback: FighterRollbackExtensionSnapshot::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FighterPipeSnapshot {
    pub flags: u8,
    pub entry_endpoint: u16,
    pub exit_endpoint: u16,
    pub dwell_ticks: u32,
    pub cooldown_ticks: u32,
    pub transit_ticks: u32,
    pub entry_position: QuantizedVec3,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArenaRuntimeSnapshot {
    pub arena_ticks: u64,
    pub hazard_clock_ticks: u32,
    pub logical_device_flags: u64,
    pub per_fighter_hazard_cooldowns: [u32; FIGHTER_CAPACITY as usize],
    pub cannon_fire_cooldown_ticks: u32,
    pub cannon_index: u8,
    pub pipes: [FighterPipeSnapshot; FIGHTER_CAPACITY as usize],
    /// Versioned fixed-width extension space for arena-specific canonical state.
    pub payload: [u8; ARENA_PAYLOAD_BYTES],
}

impl Default for ArenaRuntimeSnapshot {
    fn default() -> Self {
        Self {
            arena_ticks: 0,
            hazard_clock_ticks: 0,
            logical_device_flags: 0,
            per_fighter_hazard_cooldowns: [0; FIGHTER_CAPACITY as usize],
            cannon_fire_cooldown_ticks: 0,
            cannon_index: 0,
            pipes: [FighterPipeSnapshot::default(); FIGHTER_CAPACITY as usize],
            payload: [0; ARENA_PAYLOAD_BYTES],
        }
    }
}

/// Complete allocation state for one dynamic-object kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolAllocatorSnapshot {
    pub kind: SimEntityKind,
    pub capacity: u32,
    pub generations: Vec<u32>,
    /// One occupied bit per slot, least-significant bit first in each byte.
    pub occupied_bits: Vec<u8>,
    /// Descending storage order; allocation pops the lowest free index.
    pub free_indices: Vec<u32>,
}

impl PoolAllocatorSnapshot {
    pub fn empty(kind: SimEntityKind, capacity: u32) -> Result<Self, SnapshotError> {
        enforce_cap("pool capacity", capacity as usize, MAX_POOL_CAPACITY)?;
        Ok(Self {
            kind,
            capacity,
            generations: vec![1; capacity as usize],
            occupied_bits: vec![0; occupied_byte_len(capacity)],
            free_indices: (0..capacity).rev().collect(),
        })
    }

    pub fn is_occupied(&self, index: u32) -> Option<bool> {
        if index >= self.capacity {
            return None;
        }
        let byte = self.occupied_bits.get(index as usize / 8)?;
        Some((byte & (1 << (index % 8))) != 0)
    }

    pub fn validate(&self) -> Result<(), SnapshotError> {
        validate_allocator(self)
    }

    /// Convenience for capture code and fixtures. Canonical validation still runs
    /// before serialization.
    pub fn set_slot(
        &mut self,
        index: u32,
        generation: u32,
        occupied: bool,
    ) -> Result<(), SnapshotError> {
        if index >= self.capacity {
            return Err(SnapshotError::LimitExceeded {
                field: "pool slot index",
                value: index as usize,
                max: self.capacity.saturating_sub(1) as usize,
            });
        }
        if generation == 0 {
            return Err(SnapshotError::InvalidValue {
                field: "pool generation",
                value: 0,
            });
        }

        self.generations[index as usize] = generation;
        let byte = &mut self.occupied_bits[index as usize / 8];
        let bit = 1_u8 << (index % 8);
        if occupied {
            *byte |= bit;
            self.free_indices.retain(|free| *free != index);
        } else {
            *byte &= !bit;
            self.free_indices.retain(|free| *free != index);
            if generation != u32::MAX {
                let position = self.free_indices.partition_point(|free| *free > index);
                self.free_indices.insert(position, index);
            }
        }
        Ok(())
    }
}

/// A bounded record with a fixed-width payload. Payload interpretation is selected
/// by kind, definition, and simulation version; no record can force an unbounded
/// decode allocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DynamicObjectSnapshot {
    pub id: SimEntityId,
    pub definition_id: u16,
    pub flags: u32,
    pub owner: Option<FighterId>,
    pub target: Option<FighterId>,
    pub related_entity: Option<SimEntityId>,
    pub fighter_hit_mask: u8,
    pub payload: [u8; DYNAMIC_PAYLOAD_BYTES],
}

impl DynamicObjectSnapshot {
    pub const fn empty(id: SimEntityId) -> Self {
        Self {
            id,
            definition_id: 0,
            flags: 0,
            owner: None,
            target: None,
            related_entity: None,
            fighter_hit_mask: 0,
            payload: [0; DYNAMIC_PAYLOAD_BYTES],
        }
    }
}

/// Snapshot-compatible representation of [`RngSnapshot`].
///
/// This duplicate schema type provides a public constructor for strict manual
/// decoding while retaining lossless conversion from the RNG implementation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NamedRngSnapshot {
    pub stream: RngStreamName,
    pub state: u64,
    pub counter: u64,
}

impl NamedRngSnapshot {
    pub const fn new(stream: RngStreamName, state: u64, counter: u64) -> Self {
        Self {
            stream,
            state,
            counter,
        }
    }
}

impl From<RngSnapshot> for NamedRngSnapshot {
    fn from(value: RngSnapshot) -> Self {
        Self::new(value.stream(), value.state(), value.counter())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FighterMatchStatsSnapshot {
    pub damage_dealt: u64,
    pub damage_taken: u64,
    pub knockouts: u32,
    pub deaths: u32,
    pub item_uses: u32,
    pub actions_started: u32,
    pub quantized_distance_travelled: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchStatsSnapshot {
    pub gameplay_ticks: u64,
    pub resolved_contacts: u64,
    pub emitted_events: u64,
    /// Exact current-game telemetry counters retained for rollback/replay.
    pub ring_outs: u32,
    pub falls: u32,
    pub item_hits: u32,
    pub throws: u32,
    pub guard_breaks: u32,
    /// Per-fighter accumulated damage on the canonical 1/4096 grid.
    pub damage_by_fighter: [i32; FIGHTER_CAPACITY as usize],
    pub fighter: [FighterMatchStatsSnapshot; FIGHTER_CAPACITY as usize],
    /// Indexed by [`SimEntityKind::code`](SimEntityKind::code).
    pub rejected_dynamic_spawns: [u32; SIM_ENTITY_KIND_COUNT],
}

impl Default for MatchStatsSnapshot {
    fn default() -> Self {
        Self {
            gameplay_ticks: 0,
            resolved_contacts: 0,
            emitted_events: 0,
            ring_outs: 0,
            falls: 0,
            item_hits: 0,
            throws: 0,
            guard_breaks: 0,
            damage_by_fighter: [0; FIGHTER_CAPACITY as usize],
            fighter: [FighterMatchStatsSnapshot::default(); FIGHTER_CAPACITY as usize],
            rejected_dynamic_spawns: [0; SIM_ENTITY_KIND_COUNT],
        }
    }
}

/// Full canonical state at one completed simulation-tick boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalSnapshot {
    pub header: SnapshotHeader,
    pub match_state: MatchStateSnapshot,
    /// Always indexed by the matching [`FighterId`].
    pub fighters: [FighterSnapshot; FIGHTER_CAPACITY as usize],
    pub arena: ArenaRuntimeSnapshot,
    /// Exactly one allocator per kind, in [`SimEntityKind::ALL`] order.
    pub allocators: Vec<PoolAllocatorSnapshot>,
    /// Strictly sorted by `(kind, index, generation)`.
    pub dynamic_objects: Vec<DynamicObjectSnapshot>,
    /// Strictly sorted by stream code.
    pub rng_streams: Vec<NamedRngSnapshot>,
    pub stats: MatchStatsSnapshot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnapshotError {
    InvalidMagic([u8; 4]),
    UnsupportedSchemaVersion {
        found: u16,
        supported: u16,
    },
    DeclaredLengthMismatch {
        declared: usize,
        actual: usize,
    },
    UnexpectedEnd {
        offset: usize,
        needed: usize,
        remaining: usize,
    },
    LimitExceeded {
        field: &'static str,
        value: usize,
        max: usize,
    },
    InvalidValue {
        field: &'static str,
        value: u64,
    },
    NonCanonicalOrder {
        field: &'static str,
    },
    InvariantViolation(&'static str),
    DuplicateHistoryTick(u64),
    InvalidHistoryCapacity {
        requested: usize,
        min: usize,
        max: usize,
    },
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMagic(found) => write!(formatter, "invalid snapshot magic {found:?}"),
            Self::UnsupportedSchemaVersion { found, supported } => write!(
                formatter,
                "unsupported snapshot schema version {found}; supported version is {supported}"
            ),
            Self::DeclaredLengthMismatch { declared, actual } => write!(
                formatter,
                "snapshot declares {declared} bytes but contains {actual} bytes"
            ),
            Self::UnexpectedEnd {
                offset,
                needed,
                remaining,
            } => write!(
                formatter,
                "snapshot ended at byte {offset}: needed {needed} bytes, {remaining} remain"
            ),
            Self::LimitExceeded { field, value, max } => {
                write!(formatter, "{field} value {value} exceeds limit {max}")
            }
            Self::InvalidValue { field, value } => {
                write!(formatter, "invalid {field} value {value}")
            }
            Self::NonCanonicalOrder { field } => {
                write!(formatter, "{field} is not in strict canonical order")
            }
            Self::InvariantViolation(message) => {
                write!(formatter, "snapshot invariant violated: {message}")
            }
            Self::DuplicateHistoryTick(tick) => {
                write!(formatter, "snapshot history already contains tick {tick}")
            }
            Self::InvalidHistoryCapacity {
                requested,
                min,
                max,
            } => write!(
                formatter,
                "snapshot history capacity {requested} is outside {min}..={max}"
            ),
        }
    }
}

impl Error for SnapshotError {}

fn enforce_cap(field: &'static str, value: usize, max: usize) -> Result<(), SnapshotError> {
    if value > max {
        Err(SnapshotError::LimitExceeded { field, value, max })
    } else {
        Ok(())
    }
}

const fn occupied_byte_len(capacity: u32) -> usize {
    capacity.div_ceil(8) as usize
}

fn validate_f32_bits(bits: u32, field: &'static str) -> Result<(), SnapshotError> {
    if f32::from_bits(bits).is_finite() {
        Ok(())
    } else {
        Err(SnapshotError::InvalidValue {
            field,
            value: u64::from(bits),
        })
    }
}

fn validate_f32_vec3(
    value: F32Vec3BitsSnapshot,
    fields: [&'static str; 3],
) -> Result<(), SnapshotError> {
    validate_f32_bits(value.x, fields[0])?;
    validate_f32_bits(value.y, fields[1])?;
    validate_f32_bits(value.z, fields[2])
}

fn validate_f32_vec2(
    value: F32Vec2BitsSnapshot,
    fields: [&'static str; 2],
) -> Result<(), SnapshotError> {
    validate_f32_bits(value.x, fields[0])?;
    validate_f32_bits(value.y, fields[1])
}

fn validate_optional_u8_code(
    value: OptionalU8CodeSnapshot,
    count: u8,
    field: &'static str,
) -> Result<(), SnapshotError> {
    if !value.present {
        if value.code != 0 {
            return Err(SnapshotError::InvalidValue {
                field,
                value: u64::from(value.code),
            });
        }
        return Ok(());
    }
    if value.code >= count {
        return Err(SnapshotError::InvalidValue {
            field,
            value: u64::from(value.code),
        });
    }
    Ok(())
}

fn validate_optional_u16_code(
    value: OptionalU16CodeSnapshot,
    count: u16,
    field: &'static str,
) -> Result<(), SnapshotError> {
    if !value.present {
        if value.code != 0 {
            return Err(SnapshotError::InvalidValue {
                field,
                value: u64::from(value.code),
            });
        }
        return Ok(());
    }
    if value.code >= count {
        return Err(SnapshotError::InvalidValue {
            field,
            value: u64::from(value.code),
        });
    }
    Ok(())
}

fn validate_optional_u32(
    value: OptionalU32Snapshot,
    field: &'static str,
) -> Result<(), SnapshotError> {
    if !value.present && value.value != 0 {
        return Err(SnapshotError::InvalidValue {
            field,
            value: u64::from(value.value),
        });
    }
    Ok(())
}

fn validate_optional_f32_vec3(
    value: OptionalF32Vec3BitsSnapshot,
    field: &'static str,
    fields: [&'static str; 3],
) -> Result<(), SnapshotError> {
    if !value.present {
        if value.value != F32Vec3BitsSnapshot::default() {
            return Err(SnapshotError::InvariantViolation(field));
        }
        return Ok(());
    }
    validate_f32_vec3(value.value, fields)
}

fn validate_fighter_rollback(
    rollback: FighterRollbackExtensionSnapshot,
) -> Result<(), SnapshotError> {
    validate_f32_vec3(
        rollback.position,
        [
            "fighter exact position X",
            "fighter exact position Y",
            "fighter exact position Z",
        ],
    )?;
    validate_f32_vec2(
        rollback.input_movement,
        ["fighter input movement X", "fighter input movement Y"],
    )?;
    validate_f32_vec3(
        rollback.spawn,
        ["fighter spawn X", "fighter spawn Y", "fighter spawn Z"],
    )?;

    let stats = rollback.stats;
    validate_f32_bits(stats.health_bits, "fighter exact health")?;
    validate_f32_bits(stats.stamina_bits, "fighter exact stamina")?;
    validate_optional_u8_code(
        stats.element_carry,
        DAMAGE_ELEMENT_CODE_COUNT,
        "fighter elemental-carry code or absent padding",
    )?;
    validate_f32_bits(
        stats.element_carry_strength_bits,
        "fighter elemental-carry strength",
    )?;

    let motor = rollback.motor;
    if motor.flags & !FIGHTER_MOTOR_FLAG_MASK != 0 {
        return Err(SnapshotError::InvalidValue {
            field: "fighter motor rollback flags",
            value: u64::from(motor.flags),
        });
    }
    validate_f32_vec3(
        motor.velocity,
        [
            "fighter exact velocity X",
            "fighter exact velocity Y",
            "fighter exact velocity Z",
        ],
    )?;
    validate_f32_vec3(
        motor.facing,
        [
            "fighter exact facing X",
            "fighter exact facing Y",
            "fighter exact facing Z",
        ],
    )?;
    if !motor.landing_aftermath.present {
        if motor.landing_aftermath != QueuedAftermathSnapshot::default() {
            return Err(SnapshotError::InvariantViolation(
                "absent landing aftermath must use its canonical zero payload",
            ));
        }
    } else {
        if motor.landing_aftermath.family_code >= REACTION_FAMILY_CODE_COUNT {
            return Err(SnapshotError::InvalidValue {
                field: "landing-aftermath reaction-family code",
                value: u64::from(motor.landing_aftermath.family_code),
            });
        }
        validate_f32_bits(
            motor.landing_aftermath.horizontal_damping_bits,
            "landing-aftermath horizontal damping",
        )?;
    }
    validate_optional_u8_code(
        motor.queued_air_attack,
        TECHNIQUE_BUTTON_CODE_COUNT,
        "queued-air-attack button code or absent padding",
    )?;
    validate_f32_bits(
        motor.dash_jump_carry_speed_limit_bits,
        "dash-jump carry speed limit",
    )?;
    validate_f32_bits(motor.impact_speed_limit_bits, "impact speed limit")?;
    validate_optional_f32_vec3(
        motor.penguin_ice_slide_direction,
        "absent ice-slide direction must use its canonical zero payload",
        [
            "ice-slide direction X",
            "ice-slide direction Y",
            "ice-slide direction Z",
        ],
    )?;
    validate_f32_bits(
        motor.penguin_ice_slide_speed_bits,
        "penguin ice-slide speed",
    )?;
    validate_optional_f32_vec3(
        motor.guard_counter_source,
        "absent guard-counter source must use its canonical zero payload",
        [
            "guard-counter source X",
            "guard-counter source Y",
            "guard-counter source Z",
        ],
    )?;

    let action = rollback.action;
    if action.flags & !FIGHTER_ACTION_ROLLBACK_FLAG_MASK != 0 {
        return Err(SnapshotError::InvalidValue {
            field: "fighter action rollback flags",
            value: u64::from(action.flags),
        });
    }
    validate_optional_u16_code(
        action.queued_technique,
        TECHNIQUE_CODE_COUNT,
        "queued technique code or absent padding",
    )?;
    validate_optional_u8_code(
        action.queued_button,
        TECHNIQUE_BUTTON_CODE_COUNT,
        "queued button code or absent padding",
    )?;
    validate_optional_u8_code(
        action.buffered_button,
        TECHNIQUE_BUTTON_CODE_COUNT,
        "buffered button code or absent padding",
    )?;
    validate_optional_u16_code(
        action.technique_id,
        TECHNIQUE_CODE_COUNT,
        "active technique code or absent padding",
    )?;
    validate_optional_u32(
        action.reaction_getup_ms,
        "reaction getup time absent padding",
    )?;
    validate_optional_u32(
        action.reaction_recover_ms,
        "reaction recovery time absent padding",
    )?;
    validate_optional_u8_code(
        action.reaction_family,
        REACTION_FAMILY_CODE_COUNT,
        "reaction-family code or absent padding",
    )?;
    Ok(())
}

impl CanonicalSnapshot {
    /// Validates canonical ordering, fixed-array identities, pool allocation state,
    /// relationship targets, and every configured bound.
    pub fn validate(&self) -> Result<(), SnapshotError> {
        if self.header.schema_version != SNAPSHOT_SCHEMA_VERSION {
            return Err(SnapshotError::UnsupportedSchemaVersion {
                found: self.header.schema_version,
                supported: SNAPSHOT_SCHEMA_VERSION,
            });
        }
        if self.header.quantization_units_per_unit != SNAPSHOT_QUANTIZATION_UNITS {
            return Err(SnapshotError::InvalidValue {
                field: "quantization units per unit",
                value: u64::from(self.header.quantization_units_per_unit),
            });
        }
        if self.match_state.active_slots_mask & !((1 << FIGHTER_CAPACITY) - 1) != 0 {
            return Err(SnapshotError::InvalidValue {
                field: "active fighter mask",
                value: u64::from(self.match_state.active_slots_mask),
            });
        }

        for (index, fighter) in self.fighters.iter().enumerate() {
            let expected_id = FighterId::from_index(index)
                .expect("the fixed fighter snapshot array has exactly four slots");
            if fighter.id != expected_id {
                return Err(SnapshotError::InvariantViolation(
                    "fighter array index does not match FighterId",
                ));
            }
            let mask_active = self.match_state.active_slots_mask & (1 << index) != 0;
            if fighter.active != mask_active {
                return Err(SnapshotError::InvariantViolation(
                    "fighter active flag does not match match active-slot mask",
                ));
            }
            if fighter.active && !fighter.occupied {
                return Err(SnapshotError::InvariantViolation(
                    "an active fighter slot must be occupied",
                ));
            }
            if fighter.action.action_id >= FIGHTER_ACTION_CODE_COUNT {
                return Err(SnapshotError::InvalidValue {
                    field: "fighter action code",
                    value: u64::from(fighter.action.action_id),
                });
            }
            fighter.rollback.validate()?;
            if !fighter.occupied && *fighter != FighterSnapshot::empty(expected_id) {
                return Err(SnapshotError::InvariantViolation(
                    "an unoccupied fighter slot must use its canonical empty value",
                ));
            }
        }

        if let MatchResultSnapshot::FighterWinner { fighter, .. } = self.match_state.result
            && !self.fighters[fighter.index()].occupied
        {
            return Err(SnapshotError::InvariantViolation(
                "match result names an unoccupied fighter",
            ));
        }
        if let MatchResultSnapshot::TeamWinner { team, .. } = self.match_state.result
            && team >= FIGHTER_CAPACITY
        {
            return Err(SnapshotError::InvalidValue {
                field: "winning team",
                value: u64::from(team),
            });
        }

        if self.allocators.len() != SIM_ENTITY_KIND_COUNT {
            return Err(SnapshotError::InvalidValue {
                field: "allocator count",
                value: self.allocators.len() as u64,
            });
        }

        let mut total_slots = 0_usize;
        let mut occupied_slots = 0_usize;
        for (index, allocator) in self.allocators.iter().enumerate() {
            let expected_kind = SimEntityKind::ALL[index];
            if allocator.kind != expected_kind {
                return Err(SnapshotError::NonCanonicalOrder {
                    field: "allocator kinds",
                });
            }
            validate_allocator(allocator)?;
            total_slots = total_slots.checked_add(allocator.capacity as usize).ok_or(
                SnapshotError::LimitExceeded {
                    field: "total pool slots",
                    value: usize::MAX,
                    max: MAX_TOTAL_POOL_SLOTS,
                },
            )?;
            enforce_cap("total pool slots", total_slots, MAX_TOTAL_POOL_SLOTS)?;
            occupied_slots += allocator
                .occupied_bits
                .iter()
                .map(|byte| byte.count_ones() as usize)
                .sum::<usize>();
        }

        enforce_cap(
            "dynamic object count",
            self.dynamic_objects.len(),
            MAX_DYNAMIC_OBJECTS,
        )?;
        for pair in self.dynamic_objects.windows(2) {
            if pair[0].id >= pair[1].id {
                return Err(SnapshotError::NonCanonicalOrder {
                    field: "dynamic objects",
                });
            }
        }
        if occupied_slots != self.dynamic_objects.len() {
            return Err(SnapshotError::InvariantViolation(
                "dynamic record count does not match occupied pool slots",
            ));
        }

        for object in &self.dynamic_objects {
            if object.id.generation() == 0 {
                return Err(SnapshotError::InvalidValue {
                    field: "dynamic object generation",
                    value: 0,
                });
            }
            if object.fighter_hit_mask & !((1 << FIGHTER_CAPACITY) - 1) != 0 {
                return Err(SnapshotError::InvalidValue {
                    field: "dynamic object fighter-hit mask",
                    value: u64::from(object.fighter_hit_mask),
                });
            }

            let allocator = &self.allocators[object.id.kind().code() as usize];
            if object.id.index() >= allocator.capacity {
                return Err(SnapshotError::LimitExceeded {
                    field: "dynamic object pool index",
                    value: object.id.index() as usize,
                    max: allocator.capacity.saturating_sub(1) as usize,
                });
            }
            if allocator.is_occupied(object.id.index()) != Some(true) {
                return Err(SnapshotError::InvariantViolation(
                    "dynamic object points at an unoccupied allocator slot",
                ));
            }
            if allocator.generations[object.id.index() as usize] != object.id.generation() {
                return Err(SnapshotError::InvariantViolation(
                    "dynamic object generation differs from allocator generation",
                ));
            }
            validate_optional_fighter_relationship(
                object.owner,
                &self.fighters,
                "dynamic object owner is unoccupied",
            )?;
            validate_optional_fighter_relationship(
                object.target,
                &self.fighters,
                "dynamic object target is unoccupied",
            )?;
            if let Some(related) = object.related_entity {
                self.require_dynamic_relationship(related)?;
            }
        }

        for fighter in self.fighters.iter().filter(|fighter| fighter.occupied) {
            if let Some(held_item) = fighter.relationships.held_item {
                if held_item.kind() != SimEntityKind::Item {
                    return Err(SnapshotError::InvariantViolation(
                        "fighter held-item relationship does not reference the item pool",
                    ));
                }
                self.require_dynamic_relationship(held_item)?;
            }
            if let Some(linked) = fighter.relationships.linked_entity {
                self.require_dynamic_relationship(linked)?;
            }
            for related_fighter in [
                fighter.relationships.holding,
                fighter.relationships.held_by,
                fighter.relationships.ultimate_owner,
                fighter.relationships.ultimate_target,
                fighter.relationships.last_attacker,
            ] {
                validate_optional_fighter_relationship(
                    related_fighter,
                    &self.fighters,
                    "fighter relationship points at an unoccupied slot",
                )?;
            }
        }

        enforce_cap("RNG stream count", self.rng_streams.len(), MAX_RNG_STREAMS)?;
        for pair in self.rng_streams.windows(2) {
            if pair[0].stream.code() >= pair[1].stream.code() {
                return Err(SnapshotError::NonCanonicalOrder {
                    field: "RNG streams",
                });
            }
        }

        Ok(())
    }

    fn require_dynamic_relationship(&self, id: SimEntityId) -> Result<(), SnapshotError> {
        if self
            .dynamic_objects
            .binary_search_by_key(&id, |object| object.id)
            .is_err()
        {
            Err(SnapshotError::InvariantViolation(
                "dynamic relationship points at a missing or stale object",
            ))
        } else {
            Ok(())
        }
    }
}

fn validate_optional_fighter_relationship(
    fighter: Option<FighterId>,
    fighters: &[FighterSnapshot; FIGHTER_CAPACITY as usize],
    message: &'static str,
) -> Result<(), SnapshotError> {
    if let Some(fighter) = fighter
        && !fighters[fighter.index()].occupied
    {
        return Err(SnapshotError::InvariantViolation(message));
    }
    Ok(())
}

fn validate_allocator(allocator: &PoolAllocatorSnapshot) -> Result<(), SnapshotError> {
    enforce_cap(
        "pool capacity",
        allocator.capacity as usize,
        MAX_POOL_CAPACITY,
    )?;
    if allocator.generations.len() != allocator.capacity as usize {
        return Err(SnapshotError::InvalidValue {
            field: "allocator generation count",
            value: allocator.generations.len() as u64,
        });
    }
    if allocator.occupied_bits.len() != occupied_byte_len(allocator.capacity) {
        return Err(SnapshotError::InvalidValue {
            field: "allocator occupied-bit byte count",
            value: allocator.occupied_bits.len() as u64,
        });
    }
    if allocator.generations.contains(&0) {
        return Err(SnapshotError::InvalidValue {
            field: "allocator generation",
            value: 0,
        });
    }
    if allocator.capacity % 8 != 0
        && let Some(last) = allocator.occupied_bits.last()
    {
        let used_mask = (1_u16 << (allocator.capacity % 8)) as u8 - 1;
        if last & !used_mask != 0 {
            return Err(SnapshotError::InvalidValue {
                field: "allocator unused occupied bits",
                value: u64::from(*last),
            });
        }
    }
    if allocator.free_indices.len() > allocator.capacity as usize {
        return Err(SnapshotError::LimitExceeded {
            field: "allocator free-index count",
            value: allocator.free_indices.len(),
            max: allocator.capacity as usize,
        });
    }
    for pair in allocator.free_indices.windows(2) {
        if pair[0] <= pair[1] {
            return Err(SnapshotError::NonCanonicalOrder {
                field: "allocator free indices",
            });
        }
    }

    for index in &allocator.free_indices {
        if *index >= allocator.capacity {
            return Err(SnapshotError::LimitExceeded {
                field: "allocator free index",
                value: *index as usize,
                max: allocator.capacity.saturating_sub(1) as usize,
            });
        }
        if allocator.is_occupied(*index) != Some(false) {
            return Err(SnapshotError::InvariantViolation(
                "occupied allocator slot appears in free list",
            ));
        }
        if allocator.generations[*index as usize] == u32::MAX {
            return Err(SnapshotError::InvariantViolation(
                "retired allocator slot appears in free list",
            ));
        }
    }
    // Canonical free indices are strictly descending, so walking them in
    // reverse gives a merge against `0..capacity` without a heap-backed
    // membership bitmap.
    let mut free_indices = allocator.free_indices.iter().rev().copied().peekable();
    for index in 0..allocator.capacity {
        let occupied = allocator.is_occupied(index) == Some(true);
        let retired = allocator.generations[index as usize] == u32::MAX;
        let expected_free = !occupied && !retired;
        let listed_free = free_indices.peek().copied() == Some(index);
        if listed_free {
            free_indices.next();
        }
        if listed_free != expected_free {
            return Err(SnapshotError::InvariantViolation(
                "allocator occupancy, generation, and free list disagree",
            ));
        }
    }
    debug_assert!(free_indices.next().is_none());
    Ok(())
}

impl CanonicalSnapshot {
    /// Serializes the state in the schema's canonical field order.
    ///
    /// Layout order is envelope/header, match, four fighters, arena runtime,
    /// allocators, dynamic records, RNG streams, then canonical statistics.
    pub fn encode(&self) -> Result<Vec<u8>, SnapshotError> {
        self.validate()?;
        let encoded_length = self.canonical_encoded_length()?;
        let mut encoder = Encoder::with_capacity(encoded_length);
        self.encode_canonical_fields(&mut encoder, encoded_length as u32)?;
        debug_assert_eq!(encoder.len(), encoded_length);
        Ok(encoder.finish())
    }

    fn canonical_encoded_length(&self) -> Result<usize, SnapshotError> {
        let mut encoder = Encoder::counting();
        // The field's value does not affect its frozen four-byte width.
        self.encode_canonical_fields(&mut encoder, 0)?;
        let encoded_length = encoder.len();
        enforce_cap("encoded snapshot bytes", encoded_length, MAX_SNAPSHOT_BYTES)?;
        Ok(encoded_length)
    }

    fn encode_canonical_fields(
        &self,
        encoder: &mut Encoder,
        declared_length: u32,
    ) -> Result<(), SnapshotError> {
        encoder.write_bytes(&SNAPSHOT_MAGIC)?;
        encoder.write_u16(self.header.schema_version)?;
        encoder.write_u32(declared_length)?;

        encoder.write_u32(self.header.simulation_version)?;
        encoder.write_u32(self.header.protocol_version)?;
        encoder.write_u64(self.header.gameplay_content_hash)?;
        encoder.write_bytes(&self.header.match_id)?;
        encoder.write_u64(self.header.tick.get())?;
        encoder.write_u64(self.header.master_seed)?;
        encoder.write_u32(self.header.quantization_units_per_unit)?;

        encode_match_state(encoder, &self.match_state)?;
        for fighter in &self.fighters {
            encode_fighter(encoder, fighter)?;
        }
        encode_arena(encoder, &self.arena)?;

        encoder.write_u8(self.allocators.len() as u8)?;
        for allocator in &self.allocators {
            encode_allocator(encoder, allocator)?;
        }

        encoder.write_u32(self.dynamic_objects.len() as u32)?;
        for object in &self.dynamic_objects {
            encode_dynamic_object(encoder, object)?;
        }

        encoder.write_u16(self.rng_streams.len() as u16)?;
        for rng in &self.rng_streams {
            encoder.write_u64(rng.stream.code())?;
            encoder.write_u64(rng.state)?;
            encoder.write_u64(rng.counter)?;
        }

        encode_match_stats(encoder, &self.stats)?;
        Ok(())
    }

    /// Hashes the exact canonical encoding. This is a desync diagnostic, not an
    /// authentication primitive.
    pub fn canonical_hash(&self) -> Result<u64, SnapshotError> {
        self.validate()?;
        let encoded_length = self.canonical_encoded_length()?;
        let mut encoder = Encoder::hashing();
        self.encode_canonical_fields(&mut encoder, encoded_length as u32)?;
        debug_assert_eq!(encoder.len(), encoded_length);
        Ok(encoder.finish_hash())
    }

    pub fn encode_with_metrics(
        &self,
        metrics: &mut impl SnapshotMetricsHook,
    ) -> Result<Vec<u8>, SnapshotError> {
        let encoded = self.encode()?;
        metrics.on_snapshot_encoded(encoded.len());
        Ok(encoded)
    }
}

pub fn hash_canonical_bytes(encoded: &[u8]) -> u64 {
    let mut hash = CanonicalHash64::new();
    hash.write_bytes(encoded);
    hash.finish()
}

enum EncoderSink {
    Bytes(Vec<u8>),
    Counting,
    Hash(CanonicalHash64),
}

struct Encoder {
    sink: EncoderSink,
    len: usize,
}

impl Encoder {
    #[cfg(test)]
    fn new() -> Self {
        Self::with_capacity(4 * 1_024)
    }

    fn with_capacity(capacity: usize) -> Self {
        Self {
            sink: EncoderSink::Bytes(Vec::with_capacity(capacity)),
            len: 0,
        }
    }

    const fn counting() -> Self {
        Self {
            sink: EncoderSink::Counting,
            len: 0,
        }
    }

    const fn hashing() -> Self {
        Self {
            sink: EncoderSink::Hash(CanonicalHash64::new()),
            len: 0,
        }
    }

    const fn len(&self) -> usize {
        self.len
    }

    fn finish(self) -> Vec<u8> {
        match self.sink {
            EncoderSink::Bytes(bytes) => bytes,
            EncoderSink::Counting | EncoderSink::Hash(_) => {
                unreachable!("only a byte encoder can return encoded bytes")
            }
        }
    }

    fn finish_hash(self) -> u64 {
        match self.sink {
            EncoderSink::Hash(hash) => hash.finish(),
            EncoderSink::Bytes(_) | EncoderSink::Counting => {
                unreachable!("only a hash encoder can return a canonical hash")
            }
        }
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), SnapshotError> {
        let attempted = self
            .len
            .checked_add(bytes.len())
            .ok_or(SnapshotError::LimitExceeded {
                field: "encoded snapshot bytes",
                value: usize::MAX,
                max: MAX_SNAPSHOT_BYTES,
            })?;
        enforce_cap("encoded snapshot bytes", attempted, MAX_SNAPSHOT_BYTES)?;
        match &mut self.sink {
            EncoderSink::Bytes(encoded) => encoded.extend_from_slice(bytes),
            EncoderSink::Counting => {}
            EncoderSink::Hash(hash) => {
                hash.write_bytes(bytes);
            }
        }
        self.len = attempted;
        Ok(())
    }

    fn write_bool(&mut self, value: bool) -> Result<(), SnapshotError> {
        self.write_u8(u8::from(value))
    }

    fn write_u8(&mut self, value: u8) -> Result<(), SnapshotError> {
        self.write_bytes(&[value])
    }

    fn write_i16(&mut self, value: i16) -> Result<(), SnapshotError> {
        self.write_bytes(&value.to_le_bytes())
    }

    fn write_u16(&mut self, value: u16) -> Result<(), SnapshotError> {
        self.write_bytes(&value.to_le_bytes())
    }

    fn write_i32(&mut self, value: i32) -> Result<(), SnapshotError> {
        self.write_bytes(&value.to_le_bytes())
    }

    fn write_u32(&mut self, value: u32) -> Result<(), SnapshotError> {
        self.write_bytes(&value.to_le_bytes())
    }

    fn write_u64(&mut self, value: u64) -> Result<(), SnapshotError> {
        self.write_bytes(&value.to_le_bytes())
    }
}

fn encode_match_state(
    encoder: &mut Encoder,
    state: &MatchStateSnapshot,
) -> Result<(), SnapshotError> {
    encoder.write_u8(state.phase.code())?;
    encoder.write_u32(state.phase_ticks)?;
    encoder.write_u32(state.match_ticks_remaining)?;
    encoder.write_u32(state.hitstop_ticks)?;
    encoder.write_u32(state.next_event_ordinal)?;
    encoder.write_u8(state.active_slots_mask)?;
    encoder.write_bytes(&state.teams)?;
    encoder.write_bytes(&state.stocks)?;

    encoder.write_u32(state.rules.ruleset_id)?;
    encoder.write_u32(state.rules.arena_id)?;
    encoder.write_u32(state.rules.duration_ticks)?;
    encoder.write_u8(state.rules.starting_stocks)?;
    encoder.write_u16(state.rules.score_limit)?;
    encoder.write_bool(state.rules.team_mode)?;
    encoder.write_bool(state.rules.friendly_fire)?;

    match state.result {
        MatchResultSnapshot::Pending => encoder.write_u8(0)?,
        MatchResultSnapshot::Draw { decided_tick } => {
            encoder.write_u8(1)?;
            encoder.write_u64(decided_tick.get())?;
        }
        MatchResultSnapshot::FighterWinner {
            fighter,
            decided_tick,
        } => {
            encoder.write_u8(2)?;
            encoder.write_u8(fighter.get())?;
            encoder.write_u64(decided_tick.get())?;
        }
        MatchResultSnapshot::TeamWinner { team, decided_tick } => {
            encoder.write_u8(3)?;
            encoder.write_u8(team)?;
            encoder.write_u64(decided_tick.get())?;
        }
        MatchResultSnapshot::Aborted {
            reason,
            decided_tick,
        } => {
            encoder.write_u8(4)?;
            encoder.write_u16(reason)?;
            encoder.write_u64(decided_tick.get())?;
        }
    }
    Ok(())
}

fn encode_fighter(encoder: &mut Encoder, fighter: &FighterSnapshot) -> Result<(), SnapshotError> {
    encoder.write_bool(fighter.occupied)?;
    encoder.write_bool(fighter.active)?;
    encoder.write_u8(fighter.id.get())?;

    encoder.write_i16(fighter.input.move_x)?;
    encoder.write_i16(fighter.input.move_y)?;
    encoder.write_u32(fighter.input.held_buttons)?;
    encoder.write_u32(fighter.input.pressed_latches)?;
    encoder.write_u32(fighter.input.released_latches)?;

    encode_vec3(encoder, fighter.pose.position)?;
    encode_vec3(encoder, fighter.pose.velocity)?;
    encode_vec2(encoder, fighter.pose.facing)?;
    encoder.write_bool(fighter.pose.grounded)?;
    encoder.write_u16(fighter.pose.collision_flags)?;
    encoder.write_i32(fighter.health)?;
    encoder.write_i32(fighter.stamina)?;
    encoder.write_i32(fighter.score)?;

    encoder.write_u16(fighter.action.action_id)?;
    encoder.write_u32(fighter.action.elapsed_ticks)?;
    encoder.write_u32(fighter.action.flags)?;
    encoder.write_u16(fighter.action.buffered_action_id)?;
    encoder.write_u16(fighter.action.reaction_id)?;
    encoder.write_u32(fighter.action.reaction_ticks)?;

    for ticks in fighter.cooldowns.ticks {
        encoder.write_u32(ticks)?;
    }
    encoder.write_u64(fighter.status.flags)?;
    for ticks in fighter.status.timers {
        encoder.write_u32(ticks)?;
    }
    encoder.write_i32(fighter.status.elemental_carry)?;
    encoder.write_i32(fighter.status.size_scale)?;
    encoder.write_i32(fighter.status.speed_scale)?;

    encoder.write_u16(fighter.loadout.character_id)?;
    encoder.write_u16(fighter.loadout.style_id)?;
    encoder.write_u16(fighter.loadout.move_set_id)?;
    for equipment in fighter.loadout.equipment_ids {
        encoder.write_u16(equipment)?;
    }

    encode_optional_sim_id(encoder, fighter.relationships.held_item)?;
    encode_optional_sim_id(encoder, fighter.relationships.linked_entity)?;
    encode_optional_fighter_id(encoder, fighter.relationships.holding)?;
    encode_optional_fighter_id(encoder, fighter.relationships.held_by)?;
    encode_optional_fighter_id(encoder, fighter.relationships.ultimate_owner)?;
    encode_optional_fighter_id(encoder, fighter.relationships.ultimate_target)?;
    encode_optional_fighter_id(encoder, fighter.relationships.last_attacker)?;
    encode_fighter_rollback(encoder, fighter.rollback)
}

fn encode_fighter_rollback(
    encoder: &mut Encoder,
    rollback: FighterRollbackExtensionSnapshot,
) -> Result<(), SnapshotError> {
    let start = encoder.len();
    encode_f32_vec3_bits(encoder, rollback.position)?;
    encoder.write_u32(rollback.input_movement.x)?;
    encoder.write_u32(rollback.input_movement.y)?;
    encode_f32_vec3_bits(encoder, rollback.spawn)?;

    let stats = rollback.stats;
    encoder.write_u32(stats.health_bits)?;
    encoder.write_u32(stats.stamina_bits)?;
    encoder.write_u32(stats.invulnerability_ticks)?;
    encoder.write_u32(stats.health_refill_ticks)?;
    encoder.write_u32(stats.respawn_ticks)?;
    encode_optional_u8_code(encoder, stats.element_carry)?;
    encoder.write_u32(stats.element_carry_strength_bits)?;
    encoder.write_u32(stats.element_carry_ticks)?;
    encoder.write_u32(stats.item_speed_ticks)?;
    encoder.write_u32(stats.item_giant_ticks)?;

    let motor = rollback.motor;
    encode_f32_vec3_bits(encoder, motor.velocity)?;
    encode_f32_vec3_bits(encoder, motor.facing)?;
    encoder.write_u16(motor.flags)?;
    encoder.write_bool(motor.landing_aftermath.present)?;
    encoder.write_u8(motor.landing_aftermath.family_code)?;
    encoder.write_u32(motor.landing_aftermath.getup_transition_ms)?;
    encoder.write_u32(motor.landing_aftermath.recover_ms)?;
    encoder.write_u32(motor.landing_aftermath.landing_stick_ms)?;
    encoder.write_u32(motor.landing_aftermath.horizontal_damping_bits)?;
    encode_optional_u8_code(encoder, motor.queued_air_attack)?;
    encoder.write_u32(motor.queued_air_attack_ticks)?;
    encoder.write_u32(motor.ledge_grace_ticks)?;
    encoder.write_u32(motor.landing_stick_ticks)?;
    encoder.write_u32(motor.jump_takeoff_ticks)?;
    encoder.write_u8(motor.reaction_bounces)?;
    encoder.write_u8(motor.pig_air_meat_slam_air_hits)?;
    encoder.write_u32(motor.dash_slide_ticks)?;
    encoder.write_u32(motor.dash_jump_carry_ticks)?;
    encoder.write_u32(motor.dash_jump_carry_speed_limit_bits)?;
    encoder.write_u32(motor.impact_speed_limit_ticks)?;
    encoder.write_u32(motor.impact_speed_limit_bits)?;
    encode_optional_f32_vec3_bits(encoder, motor.penguin_ice_slide_direction)?;
    encoder.write_u32(motor.penguin_ice_slide_speed_bits)?;
    encoder.write_u32(motor.guard_active_elapsed_ticks)?;
    encoder.write_u32(motor.guard_cooldown_ticks)?;
    encoder.write_u32(motor.guard_start_buffer_ticks)?;
    encoder.write_u32(motor.guard_counter_window_ticks)?;
    encode_optional_f32_vec3_bits(encoder, motor.guard_counter_source)?;

    let action = rollback.action;
    encoder.write_u8(action.flags)?;
    encode_optional_u16_code(encoder, action.queued_technique)?;
    encode_optional_u8_code(encoder, action.queued_button)?;
    encode_optional_u8_code(encoder, action.buffered_button)?;
    encoder.write_u32(action.buffered_button_elapsed_ticks)?;
    encode_optional_u16_code(encoder, action.technique_id)?;
    encoder.write_u64(action.timeline_events_fired)?;
    encode_optional_u32(encoder, action.reaction_getup_ms)?;
    encode_optional_u32(encoder, action.reaction_recover_ms)?;
    encode_optional_u8_code(encoder, action.reaction_family)?;
    encoder.write_u32(action.charge_elapsed_ticks)?;
    encoder.write_u32(rollback.regrab_lockout_ticks)?;

    debug_assert_eq!(encoder.len() - start, FIGHTER_ROLLBACK_EXTENSION_BYTES);
    Ok(())
}

fn encode_f32_vec3_bits(
    encoder: &mut Encoder,
    value: F32Vec3BitsSnapshot,
) -> Result<(), SnapshotError> {
    encoder.write_u32(value.x)?;
    encoder.write_u32(value.y)?;
    encoder.write_u32(value.z)
}

fn encode_optional_u8_code(
    encoder: &mut Encoder,
    value: OptionalU8CodeSnapshot,
) -> Result<(), SnapshotError> {
    encoder.write_bool(value.present)?;
    encoder.write_u8(value.code)
}

fn encode_optional_u16_code(
    encoder: &mut Encoder,
    value: OptionalU16CodeSnapshot,
) -> Result<(), SnapshotError> {
    encoder.write_bool(value.present)?;
    encoder.write_u16(value.code)
}

fn encode_optional_u32(
    encoder: &mut Encoder,
    value: OptionalU32Snapshot,
) -> Result<(), SnapshotError> {
    encoder.write_bool(value.present)?;
    encoder.write_u32(value.value)
}

fn encode_optional_f32_vec3_bits(
    encoder: &mut Encoder,
    value: OptionalF32Vec3BitsSnapshot,
) -> Result<(), SnapshotError> {
    encoder.write_bool(value.present)?;
    encode_f32_vec3_bits(encoder, value.value)
}

fn encode_arena(encoder: &mut Encoder, arena: &ArenaRuntimeSnapshot) -> Result<(), SnapshotError> {
    encoder.write_u64(arena.arena_ticks)?;
    encoder.write_u32(arena.hazard_clock_ticks)?;
    encoder.write_u64(arena.logical_device_flags)?;
    for cooldown in arena.per_fighter_hazard_cooldowns {
        encoder.write_u32(cooldown)?;
    }
    encoder.write_u32(arena.cannon_fire_cooldown_ticks)?;
    encoder.write_u8(arena.cannon_index)?;
    for pipe in arena.pipes {
        encoder.write_u8(pipe.flags)?;
        encoder.write_u16(pipe.entry_endpoint)?;
        encoder.write_u16(pipe.exit_endpoint)?;
        encoder.write_u32(pipe.dwell_ticks)?;
        encoder.write_u32(pipe.cooldown_ticks)?;
        encoder.write_u32(pipe.transit_ticks)?;
        encode_vec3(encoder, pipe.entry_position)?;
    }
    encoder.write_bytes(&arena.payload)
}

fn encode_allocator(
    encoder: &mut Encoder,
    allocator: &PoolAllocatorSnapshot,
) -> Result<(), SnapshotError> {
    encoder.write_u8(allocator.kind.code())?;
    encoder.write_u32(allocator.capacity)?;
    encoder.write_u32(allocator.generations.len() as u32)?;
    for generation in &allocator.generations {
        encoder.write_u32(*generation)?;
    }
    encoder.write_u32(allocator.occupied_bits.len() as u32)?;
    encoder.write_bytes(&allocator.occupied_bits)?;
    encoder.write_u32(allocator.free_indices.len() as u32)?;
    for index in &allocator.free_indices {
        encoder.write_u32(*index)?;
    }
    Ok(())
}

fn encode_dynamic_object(
    encoder: &mut Encoder,
    object: &DynamicObjectSnapshot,
) -> Result<(), SnapshotError> {
    encode_sim_id(encoder, object.id)?;
    encoder.write_u16(object.definition_id)?;
    encoder.write_u32(object.flags)?;
    encode_optional_fighter_id(encoder, object.owner)?;
    encode_optional_fighter_id(encoder, object.target)?;
    encode_optional_sim_id(encoder, object.related_entity)?;
    encoder.write_u8(object.fighter_hit_mask)?;
    encoder.write_bytes(&object.payload)
}

fn encode_match_stats(
    encoder: &mut Encoder,
    stats: &MatchStatsSnapshot,
) -> Result<(), SnapshotError> {
    encoder.write_u64(stats.gameplay_ticks)?;
    encoder.write_u64(stats.resolved_contacts)?;
    encoder.write_u64(stats.emitted_events)?;
    encoder.write_u32(stats.ring_outs)?;
    encoder.write_u32(stats.falls)?;
    encoder.write_u32(stats.item_hits)?;
    encoder.write_u32(stats.throws)?;
    encoder.write_u32(stats.guard_breaks)?;
    for damage in stats.damage_by_fighter {
        encoder.write_i32(damage)?;
    }
    for fighter in stats.fighter {
        encoder.write_u64(fighter.damage_dealt)?;
        encoder.write_u64(fighter.damage_taken)?;
        encoder.write_u32(fighter.knockouts)?;
        encoder.write_u32(fighter.deaths)?;
        encoder.write_u32(fighter.item_uses)?;
        encoder.write_u32(fighter.actions_started)?;
        encoder.write_u64(fighter.quantized_distance_travelled)?;
    }
    for rejected in stats.rejected_dynamic_spawns {
        encoder.write_u32(rejected)?;
    }
    Ok(())
}

fn encode_vec2(encoder: &mut Encoder, value: QuantizedVec2) -> Result<(), SnapshotError> {
    encoder.write_i32(value.x)?;
    encoder.write_i32(value.z)
}

fn encode_vec3(encoder: &mut Encoder, value: QuantizedVec3) -> Result<(), SnapshotError> {
    encoder.write_i32(value.x)?;
    encoder.write_i32(value.y)?;
    encoder.write_i32(value.z)
}

fn encode_sim_id(encoder: &mut Encoder, id: SimEntityId) -> Result<(), SnapshotError> {
    encoder.write_u8(id.kind().code())?;
    encoder.write_u32(id.index())?;
    encoder.write_u32(id.generation())
}

fn encode_optional_sim_id(
    encoder: &mut Encoder,
    id: Option<SimEntityId>,
) -> Result<(), SnapshotError> {
    match id {
        None => encoder.write_u8(0),
        Some(id) => {
            encoder.write_u8(1)?;
            encode_sim_id(encoder, id)
        }
    }
}

fn encode_optional_fighter_id(
    encoder: &mut Encoder,
    id: Option<FighterId>,
) -> Result<(), SnapshotError> {
    match id {
        None => encoder.write_u8(0),
        Some(id) => {
            encoder.write_u8(1)?;
            encoder.write_u8(id.get())
        }
    }
}

impl CanonicalSnapshot {
    /// Decodes and fully validates a canonical snapshot. Length caps are checked
    /// before any count-controlled allocation.
    pub fn decode(bytes: &[u8]) -> Result<Self, SnapshotError> {
        enforce_cap("encoded snapshot bytes", bytes.len(), MAX_SNAPSHOT_BYTES)?;
        let mut decoder = Decoder::new(bytes);

        let magic = decoder.read_array::<4>()?;
        if magic != SNAPSHOT_MAGIC {
            return Err(SnapshotError::InvalidMagic(magic));
        }
        let schema_version = decoder.read_u16()?;
        if schema_version != SNAPSHOT_SCHEMA_VERSION {
            return Err(SnapshotError::UnsupportedSchemaVersion {
                found: schema_version,
                supported: SNAPSHOT_SCHEMA_VERSION,
            });
        }
        let declared_length = decoder.read_u32()? as usize;
        enforce_cap(
            "declared snapshot bytes",
            declared_length,
            MAX_SNAPSHOT_BYTES,
        )?;
        if declared_length != bytes.len() {
            return Err(SnapshotError::DeclaredLengthMismatch {
                declared: declared_length,
                actual: bytes.len(),
            });
        }

        let header = SnapshotHeader {
            schema_version,
            simulation_version: decoder.read_u32()?,
            protocol_version: decoder.read_u32()?,
            gameplay_content_hash: decoder.read_u64()?,
            match_id: decoder.read_array::<MATCH_ID_BYTES>()?,
            tick: SimTick(decoder.read_u64()?),
            master_seed: decoder.read_u64()?,
            quantization_units_per_unit: decoder.read_u32()?,
        };
        let match_state = decode_match_state(&mut decoder)?;

        let mut fighters = FighterId::ALL.map(FighterSnapshot::empty);
        for fighter in &mut fighters {
            *fighter = decode_fighter(&mut decoder)?;
        }
        let arena = decode_arena(&mut decoder)?;

        let allocator_count = decoder.read_u8()? as usize;
        if allocator_count != SIM_ENTITY_KIND_COUNT {
            return Err(SnapshotError::InvalidValue {
                field: "allocator count",
                value: allocator_count as u64,
            });
        }
        let mut allocators = Vec::with_capacity(allocator_count);
        let mut total_pool_slots = 0_usize;
        for _ in 0..allocator_count {
            let allocator = decode_allocator(&mut decoder)?;
            total_pool_slots = total_pool_slots
                .checked_add(allocator.capacity as usize)
                .ok_or(SnapshotError::LimitExceeded {
                    field: "total pool slots",
                    value: usize::MAX,
                    max: MAX_TOTAL_POOL_SLOTS,
                })?;
            enforce_cap("total pool slots", total_pool_slots, MAX_TOTAL_POOL_SLOTS)?;
            allocators.push(allocator);
        }

        let dynamic_count = decoder.read_count_u32("dynamic object count", MAX_DYNAMIC_OBJECTS)?;
        let mut dynamic_objects = Vec::with_capacity(dynamic_count);
        for _ in 0..dynamic_count {
            dynamic_objects.push(decode_dynamic_object(&mut decoder)?);
        }

        let rng_count = decoder.read_count_u16("RNG stream count", MAX_RNG_STREAMS)?;
        let mut rng_streams = Vec::with_capacity(rng_count);
        for _ in 0..rng_count {
            rng_streams.push(NamedRngSnapshot::new(
                RngStreamName::from_code(decoder.read_u64()?),
                decoder.read_u64()?,
                decoder.read_u64()?,
            ));
        }

        let stats = decode_match_stats(&mut decoder)?;
        if decoder.remaining() != 0 {
            return Err(SnapshotError::InvariantViolation(
                "canonical snapshot contains trailing bytes",
            ));
        }

        let snapshot = Self {
            header,
            match_state,
            fighters,
            arena,
            allocators,
            dynamic_objects,
            rng_streams,
            stats,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn decode_with_metrics(
        bytes: &[u8],
        metrics: &mut impl SnapshotMetricsHook,
    ) -> Result<Self, SnapshotError> {
        let started = Instant::now();
        let snapshot = Self::decode(bytes)?;
        metrics.on_snapshot_restored(bytes.len(), started.elapsed());
        Ok(snapshot)
    }
}

struct Decoder<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Decoder<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.offset
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N], SnapshotError> {
        let end = self
            .offset
            .checked_add(N)
            .ok_or(SnapshotError::UnexpectedEnd {
                offset: self.offset,
                needed: N,
                remaining: self.remaining(),
            })?;
        if end > self.bytes.len() {
            return Err(SnapshotError::UnexpectedEnd {
                offset: self.offset,
                needed: N,
                remaining: self.remaining(),
            });
        }
        let mut result = [0; N];
        result.copy_from_slice(&self.bytes[self.offset..end]);
        self.offset = end;
        Ok(result)
    }

    fn read_vec(&mut self, len: usize) -> Result<Vec<u8>, SnapshotError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(SnapshotError::UnexpectedEnd {
                offset: self.offset,
                needed: len,
                remaining: self.remaining(),
            })?;
        if end > self.bytes.len() {
            return Err(SnapshotError::UnexpectedEnd {
                offset: self.offset,
                needed: len,
                remaining: self.remaining(),
            });
        }
        let result = self.bytes[self.offset..end].to_vec();
        self.offset = end;
        Ok(result)
    }

    fn read_bool(&mut self, field: &'static str) -> Result<bool, SnapshotError> {
        match self.read_u8()? {
            0 => Ok(false),
            1 => Ok(true),
            value => Err(SnapshotError::InvalidValue {
                field,
                value: u64::from(value),
            }),
        }
    }

    fn read_u8(&mut self) -> Result<u8, SnapshotError> {
        Ok(self.read_array::<1>()?[0])
    }

    fn read_i16(&mut self) -> Result<i16, SnapshotError> {
        Ok(i16::from_le_bytes(self.read_array()?))
    }

    fn read_u16(&mut self) -> Result<u16, SnapshotError> {
        Ok(u16::from_le_bytes(self.read_array()?))
    }

    fn read_i32(&mut self) -> Result<i32, SnapshotError> {
        Ok(i32::from_le_bytes(self.read_array()?))
    }

    fn read_u32(&mut self) -> Result<u32, SnapshotError> {
        Ok(u32::from_le_bytes(self.read_array()?))
    }

    fn read_u64(&mut self) -> Result<u64, SnapshotError> {
        Ok(u64::from_le_bytes(self.read_array()?))
    }

    fn read_count_u16(&mut self, field: &'static str, max: usize) -> Result<usize, SnapshotError> {
        let count = self.read_u16()? as usize;
        enforce_cap(field, count, max)?;
        Ok(count)
    }

    fn read_count_u32(&mut self, field: &'static str, max: usize) -> Result<usize, SnapshotError> {
        let count = self.read_u32()? as usize;
        enforce_cap(field, count, max)?;
        Ok(count)
    }
}

fn decode_match_state(decoder: &mut Decoder<'_>) -> Result<MatchStateSnapshot, SnapshotError> {
    let phase_code = decoder.read_u8()?;
    let phase = MatchPhaseSnapshot::from_code(phase_code).ok_or(SnapshotError::InvalidValue {
        field: "match phase",
        value: u64::from(phase_code),
    })?;
    let phase_ticks = decoder.read_u32()?;
    let match_ticks_remaining = decoder.read_u32()?;
    let hitstop_ticks = decoder.read_u32()?;
    let next_event_ordinal = decoder.read_u32()?;
    let active_slots_mask = decoder.read_u8()?;
    let teams = decoder.read_array::<{ FIGHTER_CAPACITY as usize }>()?;
    let stocks = decoder.read_array::<{ FIGHTER_CAPACITY as usize }>()?;
    let rules = MatchRulesSnapshot {
        ruleset_id: decoder.read_u32()?,
        arena_id: decoder.read_u32()?,
        duration_ticks: decoder.read_u32()?,
        starting_stocks: decoder.read_u8()?,
        score_limit: decoder.read_u16()?,
        team_mode: decoder.read_bool("team-mode flag")?,
        friendly_fire: decoder.read_bool("friendly-fire flag")?,
    };

    let result_tag = decoder.read_u8()?;
    let result = match result_tag {
        0 => MatchResultSnapshot::Pending,
        1 => MatchResultSnapshot::Draw {
            decided_tick: SimTick(decoder.read_u64()?),
        },
        2 => MatchResultSnapshot::FighterWinner {
            fighter: decode_fighter_id(decoder, "winning fighter")?,
            decided_tick: SimTick(decoder.read_u64()?),
        },
        3 => MatchResultSnapshot::TeamWinner {
            team: decoder.read_u8()?,
            decided_tick: SimTick(decoder.read_u64()?),
        },
        4 => MatchResultSnapshot::Aborted {
            reason: decoder.read_u16()?,
            decided_tick: SimTick(decoder.read_u64()?),
        },
        value => {
            return Err(SnapshotError::InvalidValue {
                field: "match result tag",
                value: u64::from(value),
            });
        }
    };

    Ok(MatchStateSnapshot {
        phase,
        phase_ticks,
        match_ticks_remaining,
        hitstop_ticks,
        next_event_ordinal,
        active_slots_mask,
        teams,
        stocks,
        rules,
        result,
    })
}

fn decode_fighter(decoder: &mut Decoder<'_>) -> Result<FighterSnapshot, SnapshotError> {
    let occupied = decoder.read_bool("fighter occupied flag")?;
    let active = decoder.read_bool("fighter active flag")?;
    let id = decode_fighter_id(decoder, "fighter ID")?;
    let input = FighterInputSnapshot {
        move_x: decoder.read_i16()?,
        move_y: decoder.read_i16()?,
        held_buttons: decoder.read_u32()?,
        pressed_latches: decoder.read_u32()?,
        released_latches: decoder.read_u32()?,
    };
    let pose = FighterPoseSnapshot {
        position: decode_vec3(decoder)?,
        velocity: decode_vec3(decoder)?,
        facing: decode_vec2(decoder)?,
        grounded: decoder.read_bool("fighter grounded flag")?,
        collision_flags: decoder.read_u16()?,
    };
    let health = decoder.read_i32()?;
    let stamina = decoder.read_i32()?;
    let score = decoder.read_i32()?;
    let action = FighterActionSnapshot {
        action_id: decoder.read_u16()?,
        elapsed_ticks: decoder.read_u32()?,
        flags: decoder.read_u32()?,
        buffered_action_id: decoder.read_u16()?,
        reaction_id: decoder.read_u16()?,
        reaction_ticks: decoder.read_u32()?,
    };
    let mut cooldown_ticks = [0; COOLDOWN_SLOTS];
    for ticks in &mut cooldown_ticks {
        *ticks = decoder.read_u32()?;
    }
    let status_flags = decoder.read_u64()?;
    let mut status_timers = [0; STATUS_TIMER_SLOTS];
    for ticks in &mut status_timers {
        *ticks = decoder.read_u32()?;
    }
    let status = FighterStatusSnapshot {
        flags: status_flags,
        timers: status_timers,
        elemental_carry: decoder.read_i32()?,
        size_scale: decoder.read_i32()?,
        speed_scale: decoder.read_i32()?,
    };
    let character_id = decoder.read_u16()?;
    let style_id = decoder.read_u16()?;
    let move_set_id = decoder.read_u16()?;
    let mut equipment_ids = [0; EQUIPMENT_SLOTS];
    for equipment in &mut equipment_ids {
        *equipment = decoder.read_u16()?;
    }
    let loadout = FighterLoadoutSnapshot {
        character_id,
        style_id,
        move_set_id,
        equipment_ids,
    };
    let relationships = FighterRelationshipsSnapshot {
        held_item: decode_optional_sim_id(decoder, "held-item relationship tag")?,
        linked_entity: decode_optional_sim_id(decoder, "linked-entity relationship tag")?,
        holding: decode_optional_fighter_id(decoder, "holding relationship tag")?,
        held_by: decode_optional_fighter_id(decoder, "held-by relationship tag")?,
        ultimate_owner: decode_optional_fighter_id(decoder, "ultimate-owner relationship tag")?,
        ultimate_target: decode_optional_fighter_id(decoder, "ultimate-target relationship tag")?,
        last_attacker: decode_optional_fighter_id(decoder, "last-attacker relationship tag")?,
    };
    let rollback = decode_fighter_rollback(decoder)?;

    Ok(FighterSnapshot {
        occupied,
        active,
        id,
        input,
        pose,
        health,
        stamina,
        score,
        action,
        cooldowns: FighterCooldownSnapshot {
            ticks: cooldown_ticks,
        },
        status,
        loadout,
        relationships,
        rollback,
    })
}

fn decode_fighter_rollback(
    decoder: &mut Decoder<'_>,
) -> Result<FighterRollbackExtensionSnapshot, SnapshotError> {
    let start = decoder.offset;
    let position = decode_f32_vec3_bits(decoder)?;
    let input_movement = F32Vec2BitsSnapshot {
        x: decoder.read_u32()?,
        y: decoder.read_u32()?,
    };
    let spawn = decode_f32_vec3_bits(decoder)?;
    let stats = FighterStatsRollbackSnapshot {
        health_bits: decoder.read_u32()?,
        stamina_bits: decoder.read_u32()?,
        invulnerability_ticks: decoder.read_u32()?,
        health_refill_ticks: decoder.read_u32()?,
        respawn_ticks: decoder.read_u32()?,
        element_carry: decode_optional_u8_code(decoder, "elemental-carry presence tag")?,
        element_carry_strength_bits: decoder.read_u32()?,
        element_carry_ticks: decoder.read_u32()?,
        item_speed_ticks: decoder.read_u32()?,
        item_giant_ticks: decoder.read_u32()?,
    };
    let velocity = decode_f32_vec3_bits(decoder)?;
    let facing = decode_f32_vec3_bits(decoder)?;
    let flags = decoder.read_u16()?;
    let landing_aftermath = QueuedAftermathSnapshot {
        present: decoder.read_bool("landing-aftermath presence tag")?,
        family_code: decoder.read_u8()?,
        getup_transition_ms: decoder.read_u32()?,
        recover_ms: decoder.read_u32()?,
        landing_stick_ms: decoder.read_u32()?,
        horizontal_damping_bits: decoder.read_u32()?,
    };
    let motor = FighterMotorRollbackSnapshot {
        velocity,
        facing,
        flags,
        landing_aftermath,
        queued_air_attack: decode_optional_u8_code(decoder, "queued-air-attack presence tag")?,
        queued_air_attack_ticks: decoder.read_u32()?,
        ledge_grace_ticks: decoder.read_u32()?,
        landing_stick_ticks: decoder.read_u32()?,
        jump_takeoff_ticks: decoder.read_u32()?,
        reaction_bounces: decoder.read_u8()?,
        pig_air_meat_slam_air_hits: decoder.read_u8()?,
        dash_slide_ticks: decoder.read_u32()?,
        dash_jump_carry_ticks: decoder.read_u32()?,
        dash_jump_carry_speed_limit_bits: decoder.read_u32()?,
        impact_speed_limit_ticks: decoder.read_u32()?,
        impact_speed_limit_bits: decoder.read_u32()?,
        penguin_ice_slide_direction: decode_optional_f32_vec3_bits(
            decoder,
            "ice-slide-direction presence tag",
        )?,
        penguin_ice_slide_speed_bits: decoder.read_u32()?,
        guard_active_elapsed_ticks: decoder.read_u32()?,
        guard_cooldown_ticks: decoder.read_u32()?,
        guard_start_buffer_ticks: decoder.read_u32()?,
        guard_counter_window_ticks: decoder.read_u32()?,
        guard_counter_source: decode_optional_f32_vec3_bits(
            decoder,
            "guard-counter-source presence tag",
        )?,
    };
    let action = FighterActionRollbackSnapshot {
        flags: decoder.read_u8()?,
        queued_technique: decode_optional_u16_code(decoder, "queued-technique presence tag")?,
        queued_button: decode_optional_u8_code(decoder, "queued-button presence tag")?,
        buffered_button: decode_optional_u8_code(decoder, "buffered-button presence tag")?,
        buffered_button_elapsed_ticks: decoder.read_u32()?,
        technique_id: decode_optional_u16_code(decoder, "active-technique presence tag")?,
        timeline_events_fired: decoder.read_u64()?,
        reaction_getup_ms: decode_optional_u32(decoder, "reaction-getup presence tag")?,
        reaction_recover_ms: decode_optional_u32(decoder, "reaction-recovery presence tag")?,
        reaction_family: decode_optional_u8_code(decoder, "reaction-family presence tag")?,
        charge_elapsed_ticks: decoder.read_u32()?,
    };
    let rollback = FighterRollbackExtensionSnapshot {
        position,
        input_movement,
        spawn,
        stats,
        motor,
        action,
        regrab_lockout_ticks: decoder.read_u32()?,
    };
    debug_assert_eq!(decoder.offset - start, FIGHTER_ROLLBACK_EXTENSION_BYTES);
    Ok(rollback)
}

fn decode_f32_vec3_bits(decoder: &mut Decoder<'_>) -> Result<F32Vec3BitsSnapshot, SnapshotError> {
    Ok(F32Vec3BitsSnapshot {
        x: decoder.read_u32()?,
        y: decoder.read_u32()?,
        z: decoder.read_u32()?,
    })
}

fn decode_optional_u8_code(
    decoder: &mut Decoder<'_>,
    field: &'static str,
) -> Result<OptionalU8CodeSnapshot, SnapshotError> {
    Ok(OptionalU8CodeSnapshot {
        present: decoder.read_bool(field)?,
        code: decoder.read_u8()?,
    })
}

fn decode_optional_u16_code(
    decoder: &mut Decoder<'_>,
    field: &'static str,
) -> Result<OptionalU16CodeSnapshot, SnapshotError> {
    Ok(OptionalU16CodeSnapshot {
        present: decoder.read_bool(field)?,
        code: decoder.read_u16()?,
    })
}

fn decode_optional_u32(
    decoder: &mut Decoder<'_>,
    field: &'static str,
) -> Result<OptionalU32Snapshot, SnapshotError> {
    Ok(OptionalU32Snapshot {
        present: decoder.read_bool(field)?,
        value: decoder.read_u32()?,
    })
}

fn decode_optional_f32_vec3_bits(
    decoder: &mut Decoder<'_>,
    field: &'static str,
) -> Result<OptionalF32Vec3BitsSnapshot, SnapshotError> {
    Ok(OptionalF32Vec3BitsSnapshot {
        present: decoder.read_bool(field)?,
        value: decode_f32_vec3_bits(decoder)?,
    })
}

fn decode_arena(decoder: &mut Decoder<'_>) -> Result<ArenaRuntimeSnapshot, SnapshotError> {
    let arena_ticks = decoder.read_u64()?;
    let hazard_clock_ticks = decoder.read_u32()?;
    let logical_device_flags = decoder.read_u64()?;
    let mut per_fighter_hazard_cooldowns = [0; FIGHTER_CAPACITY as usize];
    for cooldown in &mut per_fighter_hazard_cooldowns {
        *cooldown = decoder.read_u32()?;
    }
    let cannon_fire_cooldown_ticks = decoder.read_u32()?;
    let cannon_index = decoder.read_u8()?;
    let mut pipes = [FighterPipeSnapshot::default(); FIGHTER_CAPACITY as usize];
    for pipe in &mut pipes {
        *pipe = FighterPipeSnapshot {
            flags: decoder.read_u8()?,
            entry_endpoint: decoder.read_u16()?,
            exit_endpoint: decoder.read_u16()?,
            dwell_ticks: decoder.read_u32()?,
            cooldown_ticks: decoder.read_u32()?,
            transit_ticks: decoder.read_u32()?,
            entry_position: decode_vec3(decoder)?,
        };
    }
    Ok(ArenaRuntimeSnapshot {
        arena_ticks,
        hazard_clock_ticks,
        logical_device_flags,
        per_fighter_hazard_cooldowns,
        cannon_fire_cooldown_ticks,
        cannon_index,
        pipes,
        payload: decoder.read_array::<ARENA_PAYLOAD_BYTES>()?,
    })
}

fn decode_allocator(decoder: &mut Decoder<'_>) -> Result<PoolAllocatorSnapshot, SnapshotError> {
    let kind = decode_sim_kind(decoder, "allocator kind")?;
    let capacity = decoder.read_u32()?;
    enforce_cap("pool capacity", capacity as usize, MAX_POOL_CAPACITY)?;

    let generation_count =
        decoder.read_count_u32("allocator generation count", MAX_POOL_CAPACITY)?;
    if generation_count != capacity as usize {
        return Err(SnapshotError::InvalidValue {
            field: "allocator generation count",
            value: generation_count as u64,
        });
    }
    let mut generations = Vec::with_capacity(generation_count);
    for _ in 0..generation_count {
        generations.push(decoder.read_u32()?);
    }

    let expected_occupied_bytes = occupied_byte_len(capacity);
    let occupied_count = decoder.read_count_u32(
        "allocator occupied-bit byte count",
        occupied_byte_len(MAX_POOL_CAPACITY as u32),
    )?;
    if occupied_count != expected_occupied_bytes {
        return Err(SnapshotError::InvalidValue {
            field: "allocator occupied-bit byte count",
            value: occupied_count as u64,
        });
    }
    let occupied_bits = decoder.read_vec(occupied_count)?;

    let free_count = decoder.read_count_u32("allocator free-index count", capacity as usize)?;
    let mut free_indices = Vec::with_capacity(free_count);
    for _ in 0..free_count {
        free_indices.push(decoder.read_u32()?);
    }

    Ok(PoolAllocatorSnapshot {
        kind,
        capacity,
        generations,
        occupied_bits,
        free_indices,
    })
}

fn decode_dynamic_object(
    decoder: &mut Decoder<'_>,
) -> Result<DynamicObjectSnapshot, SnapshotError> {
    Ok(DynamicObjectSnapshot {
        id: decode_sim_id(decoder)?,
        definition_id: decoder.read_u16()?,
        flags: decoder.read_u32()?,
        owner: decode_optional_fighter_id(decoder, "dynamic owner tag")?,
        target: decode_optional_fighter_id(decoder, "dynamic target tag")?,
        related_entity: decode_optional_sim_id(decoder, "related-entity tag")?,
        fighter_hit_mask: decoder.read_u8()?,
        payload: decoder.read_array::<DYNAMIC_PAYLOAD_BYTES>()?,
    })
}

fn decode_match_stats(decoder: &mut Decoder<'_>) -> Result<MatchStatsSnapshot, SnapshotError> {
    let gameplay_ticks = decoder.read_u64()?;
    let resolved_contacts = decoder.read_u64()?;
    let emitted_events = decoder.read_u64()?;
    let ring_outs = decoder.read_u32()?;
    let falls = decoder.read_u32()?;
    let item_hits = decoder.read_u32()?;
    let throws = decoder.read_u32()?;
    let guard_breaks = decoder.read_u32()?;
    let mut damage_by_fighter = [0; FIGHTER_CAPACITY as usize];
    for damage in &mut damage_by_fighter {
        *damage = decoder.read_i32()?;
    }
    let mut fighter = [FighterMatchStatsSnapshot::default(); FIGHTER_CAPACITY as usize];
    for stats in &mut fighter {
        *stats = FighterMatchStatsSnapshot {
            damage_dealt: decoder.read_u64()?,
            damage_taken: decoder.read_u64()?,
            knockouts: decoder.read_u32()?,
            deaths: decoder.read_u32()?,
            item_uses: decoder.read_u32()?,
            actions_started: decoder.read_u32()?,
            quantized_distance_travelled: decoder.read_u64()?,
        };
    }
    let mut rejected_dynamic_spawns = [0; SIM_ENTITY_KIND_COUNT];
    for rejected in &mut rejected_dynamic_spawns {
        *rejected = decoder.read_u32()?;
    }
    Ok(MatchStatsSnapshot {
        gameplay_ticks,
        resolved_contacts,
        emitted_events,
        ring_outs,
        falls,
        item_hits,
        throws,
        guard_breaks,
        damage_by_fighter,
        fighter,
        rejected_dynamic_spawns,
    })
}

fn decode_vec2(decoder: &mut Decoder<'_>) -> Result<QuantizedVec2, SnapshotError> {
    Ok(QuantizedVec2 {
        x: decoder.read_i32()?,
        z: decoder.read_i32()?,
    })
}

fn decode_vec3(decoder: &mut Decoder<'_>) -> Result<QuantizedVec3, SnapshotError> {
    Ok(QuantizedVec3 {
        x: decoder.read_i32()?,
        y: decoder.read_i32()?,
        z: decoder.read_i32()?,
    })
}

fn decode_sim_kind(
    decoder: &mut Decoder<'_>,
    field: &'static str,
) -> Result<SimEntityKind, SnapshotError> {
    let value = decoder.read_u8()?;
    SimEntityKind::from_code(value).ok_or(SnapshotError::InvalidValue {
        field,
        value: u64::from(value),
    })
}

fn decode_sim_id(decoder: &mut Decoder<'_>) -> Result<SimEntityId, SnapshotError> {
    let kind = decode_sim_kind(decoder, "simulation entity kind")?;
    let index = decoder.read_u32()?;
    let generation = decoder.read_u32()?;
    if generation == 0 {
        return Err(SnapshotError::InvalidValue {
            field: "simulation entity generation",
            value: 0,
        });
    }
    Ok(SimEntityId::new(kind, index, generation))
}

fn decode_fighter_id(
    decoder: &mut Decoder<'_>,
    field: &'static str,
) -> Result<FighterId, SnapshotError> {
    let value = decoder.read_u8()?;
    FighterId::new(value).ok_or(SnapshotError::InvalidValue {
        field,
        value: u64::from(value),
    })
}

fn decode_optional_sim_id(
    decoder: &mut Decoder<'_>,
    field: &'static str,
) -> Result<Option<SimEntityId>, SnapshotError> {
    match decoder.read_u8()? {
        0 => Ok(None),
        1 => Ok(Some(decode_sim_id(decoder)?)),
        value => Err(SnapshotError::InvalidValue {
            field,
            value: u64::from(value),
        }),
    }
}

fn decode_optional_fighter_id(
    decoder: &mut Decoder<'_>,
    field: &'static str,
) -> Result<Option<FighterId>, SnapshotError> {
    match decoder.read_u8()? {
        0 => Ok(None),
        1 => Ok(Some(decode_fighter_id(decoder, field)?)),
        value => Err(SnapshotError::InvalidValue {
            field,
            value: u64::from(value),
        }),
    }
}

/// Non-canonical instrumentation sink. Timing is deliberately kept outside the
/// snapshot and simulation state.
pub trait SnapshotMetricsHook {
    fn on_snapshot_encoded(&mut self, _encoded_bytes: usize) {}

    fn on_snapshot_restored(&mut self, _encoded_bytes: usize, _elapsed: Duration) {}
}

/// Basic accumulator suitable for diagnostics, soak tests, and telemetry export.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SnapshotMetricTotals {
    pub encoded_snapshots: u64,
    pub restored_snapshots: u64,
    pub last_encoded_bytes: usize,
    pub peak_encoded_bytes: usize,
    pub total_restore_nanoseconds: u128,
    pub peak_restore_nanoseconds: u128,
}

impl SnapshotMetricsHook for SnapshotMetricTotals {
    fn on_snapshot_encoded(&mut self, encoded_bytes: usize) {
        self.encoded_snapshots = self.encoded_snapshots.saturating_add(1);
        self.last_encoded_bytes = encoded_bytes;
        self.peak_encoded_bytes = self.peak_encoded_bytes.max(encoded_bytes);
    }

    fn on_snapshot_restored(&mut self, encoded_bytes: usize, elapsed: Duration) {
        self.restored_snapshots = self.restored_snapshots.saturating_add(1);
        self.last_encoded_bytes = encoded_bytes;
        self.peak_encoded_bytes = self.peak_encoded_bytes.max(encoded_bytes);
        let elapsed = elapsed.as_nanos();
        self.total_restore_nanoseconds = self.total_restore_nanoseconds.saturating_add(elapsed);
        self.peak_restore_nanoseconds = self.peak_restore_nanoseconds.max(elapsed);
    }
}

/// One encoded history entry. Keeping bytes instead of an ECS clone makes restore
/// exercise the same strict decoder used by rollback corrections and replays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotHistoryEntry {
    tick: SimTick,
    hash: u64,
    encoded: Vec<u8>,
}

impl SnapshotHistoryEntry {
    pub const fn tick(&self) -> SimTick {
        self.tick
    }

    pub const fn hash(&self) -> u64 {
        self.hash
    }

    pub fn encoded(&self) -> &[u8] {
        &self.encoded
    }

    pub fn encoded_len(&self) -> usize {
        self.encoded.len()
    }

    pub fn restore(&self) -> Result<CanonicalSnapshot, SnapshotError> {
        CanonicalSnapshot::decode(&self.encoded)
    }

    pub fn restore_with_metrics(
        &self,
        metrics: &mut impl SnapshotMetricsHook,
    ) -> Result<CanonicalSnapshot, SnapshotError> {
        CanonicalSnapshot::decode_with_metrics(&self.encoded, metrics)
    }
}

/// Bounded insertion-ordered ring used by client prediction and rollback.
#[derive(Clone, Debug)]
pub struct SnapshotHistory {
    slots: Vec<Option<SnapshotHistoryEntry>>,
    start: usize,
    len: usize,
    stored_bytes: usize,
}

impl SnapshotHistory {
    pub fn new(capacity: usize) -> Result<Self, SnapshotError> {
        if !(MIN_SNAPSHOT_HISTORY..=MAX_SNAPSHOT_HISTORY).contains(&capacity) {
            return Err(SnapshotError::InvalidHistoryCapacity {
                requested: capacity,
                min: MIN_SNAPSHOT_HISTORY,
                max: MAX_SNAPSHOT_HISTORY,
            });
        }
        Ok(Self {
            slots: vec![None; capacity],
            start: 0,
            len: 0,
            stored_bytes: 0,
        })
    }

    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub const fn stored_bytes(&self) -> usize {
        self.stored_bytes
    }

    pub fn oldest(&self) -> Option<&SnapshotHistoryEntry> {
        if self.len == 0 {
            None
        } else {
            self.slots[self.start].as_ref()
        }
    }

    pub fn newest(&self) -> Option<&SnapshotHistoryEntry> {
        if self.len == 0 {
            None
        } else {
            self.slots[self.logical_slot(self.len - 1)].as_ref()
        }
    }

    pub fn get(&self, tick: SimTick) -> Option<&SnapshotHistoryEntry> {
        self.iter().find(|entry| entry.tick == tick)
    }

    pub fn iter(&self) -> impl Iterator<Item = &SnapshotHistoryEntry> {
        (0..self.len).filter_map(move |offset| self.slots[self.logical_slot(offset)].as_ref())
    }

    /// Encodes and appends a snapshot, returning the overwritten oldest entry when
    /// the ring is full. Duplicate live ticks are rejected to keep lookup unique.
    pub fn insert(
        &mut self,
        snapshot: &CanonicalSnapshot,
    ) -> Result<Option<SnapshotHistoryEntry>, SnapshotError> {
        let encoded = snapshot.encode()?;
        self.insert_encoded(snapshot.header.tick, encoded)
    }

    pub fn insert_with_metrics(
        &mut self,
        snapshot: &CanonicalSnapshot,
        metrics: &mut impl SnapshotMetricsHook,
    ) -> Result<Option<SnapshotHistoryEntry>, SnapshotError> {
        let encoded = snapshot.encode_with_metrics(metrics)?;
        self.insert_encoded(snapshot.header.tick, encoded)
    }

    fn insert_encoded(
        &mut self,
        tick: SimTick,
        encoded: Vec<u8>,
    ) -> Result<Option<SnapshotHistoryEntry>, SnapshotError> {
        if self.get(tick).is_some() {
            return Err(SnapshotError::DuplicateHistoryTick(tick.get()));
        }
        let entry = SnapshotHistoryEntry {
            tick,
            hash: hash_canonical_bytes(&encoded),
            encoded,
        };

        if self.len < self.capacity() {
            let slot = self.logical_slot(self.len);
            self.stored_bytes += entry.encoded_len();
            self.slots[slot] = Some(entry);
            self.len += 1;
            Ok(None)
        } else {
            let slot = self.start;
            let overwritten = self.slots[slot]
                .take()
                .expect("a full history ring has no empty logical slots");
            self.stored_bytes -= overwritten.encoded_len();
            self.stored_bytes += entry.encoded_len();
            self.slots[slot] = Some(entry);
            self.start = (self.start + 1) % self.capacity();
            Ok(Some(overwritten))
        }
    }

    /// Removes entries inserted after `tick`. Returns `None` if the retained tick
    /// is no longer in history, otherwise the number removed.
    pub fn truncate_after(&mut self, tick: SimTick) -> Option<usize> {
        let retained_offset = self.iter().position(|entry| entry.tick == tick)?;
        let target_len = retained_offset + 1;
        let removed = self.len - target_len;
        while self.len > target_len {
            let slot = self.logical_slot(self.len - 1);
            let entry = self.slots[slot]
                .take()
                .expect("every logical history slot is occupied");
            self.stored_bytes -= entry.encoded_len();
            self.len -= 1;
        }
        Some(removed)
    }

    pub fn clear(&mut self) {
        for slot in &mut self.slots {
            *slot = None;
        }
        self.start = 0;
        self.len = 0;
        self.stored_bytes = 0;
    }

    fn logical_slot(&self, offset: usize) -> usize {
        (self.start + offset) % self.capacity()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::determinism::DeterministicRngStream;

    fn vec3_bits(x: f32, y: f32, z: f32) -> F32Vec3BitsSnapshot {
        F32Vec3BitsSnapshot {
            x: x.to_bits(),
            y: y.to_bits(),
            z: z.to_bits(),
        }
    }

    fn fixture(tick: u64) -> CanonicalSnapshot {
        let hitbox_id = SimEntityId::new(SimEntityKind::Hitbox, 1, 2);
        let item_id = SimEntityId::new(SimEntityKind::Item, 0, 7);

        let mut allocators = Vec::with_capacity(SIM_ENTITY_KIND_COUNT);
        for kind in SimEntityKind::ALL {
            let capacity = match kind {
                SimEntityKind::Hitbox => 2,
                SimEntityKind::Item => 3,
                _ => 1,
            };
            allocators.push(PoolAllocatorSnapshot::empty(kind, capacity).unwrap());
        }
        allocators[SimEntityKind::Hitbox.code() as usize]
            .set_slot(hitbox_id.index(), hitbox_id.generation(), true)
            .unwrap();
        allocators[SimEntityKind::Item.code() as usize]
            .set_slot(item_id.index(), item_id.generation(), true)
            .unwrap();

        let fighter_0 = FighterId::new(0).unwrap();
        let fighter_1 = FighterId::new(1).unwrap();
        let mut fighters = FighterId::ALL.map(FighterSnapshot::empty);
        fighters[0] = FighterSnapshot {
            occupied: true,
            active: true,
            id: fighter_0,
            input: FighterInputSnapshot {
                move_x: -12_345,
                move_y: 22_222,
                held_buttons: 0x0102_0304,
                pressed_latches: 0x1020_3040,
                released_latches: 0xa0b0_c0d0,
            },
            pose: FighterPoseSnapshot {
                position: QuantizedVec3 {
                    x: -20_000,
                    y: 4_096,
                    z: 99_999,
                },
                velocity: QuantizedVec3 {
                    x: 123,
                    y: -456,
                    z: 789,
                },
                facing: QuantizedVec2 { x: -4_096, z: 0 },
                grounded: true,
                collision_flags: 0x1234,
            },
            health: 355_123,
            stamina: 87_654,
            score: 12,
            action: FighterActionSnapshot {
                action_id: 14,
                elapsed_ticks: 13,
                flags: 0x1020_4080,
                buffered_action_id: 9,
                reaction_id: 4,
                reaction_ticks: 6,
            },
            cooldowns: FighterCooldownSnapshot {
                ticks: [1, 2, 3, 5, 8, 13, 21, 34],
            },
            status: FighterStatusSnapshot {
                flags: 0x0102_0304_0506_0708,
                timers: [55, 34, 21, 13, 8, 5, 3, 2],
                elemental_carry: -1_024,
                size_scale: 4_608,
                speed_scale: 3_840,
            },
            loadout: FighterLoadoutSnapshot {
                character_id: 3,
                style_id: 7,
                move_set_id: 11,
                equipment_ids: [100, 200, 300, 400],
            },
            relationships: FighterRelationshipsSnapshot {
                held_item: Some(item_id),
                linked_entity: Some(hitbox_id),
                holding: Some(fighter_1),
                held_by: None,
                ultimate_owner: None,
                ultimate_target: Some(fighter_1),
                last_attacker: Some(fighter_1),
            },
            rollback: FighterRollbackExtensionSnapshot {
                position: vec3_bits(8.25, -0.5, 91.75),
                input_movement: F32Vec2BitsSnapshot {
                    x: (-0.75_f32).to_bits(),
                    y: 0.4_f32.to_bits(),
                },
                spawn: vec3_bits(-13.25, 1.5, 42.75),
                stats: FighterStatsRollbackSnapshot {
                    health_bits: 86.700_195_f32.to_bits(),
                    stamina_bits: 21.399_902_f32.to_bits(),
                    invulnerability_ticks: 17,
                    health_refill_ticks: 18,
                    respawn_ticks: 19,
                    element_carry: OptionalU8CodeSnapshot {
                        present: true,
                        code: 3,
                    },
                    element_carry_strength_bits: 0.625_f32.to_bits(),
                    element_carry_ticks: 20,
                    item_speed_ticks: 21,
                    item_giant_ticks: 22,
                },
                motor: FighterMotorRollbackSnapshot {
                    velocity: vec3_bits(3.25, -7.5, 0.125),
                    facing: vec3_bits(-0.0, 0.0, -1.0),
                    flags: FIGHTER_MOTOR_KNOCKDOWN_ON_LAND
                        | FIGHTER_MOTOR_AIR_ATTACK_USED
                        | FIGHTER_MOTOR_BEE_AIR_DASH_SHOT_AVAILABLE
                        | FIGHTER_MOTOR_GUARD_COUNTER_BUFFERED,
                    landing_aftermath: QueuedAftermathSnapshot {
                        present: true,
                        family_code: 4,
                        getup_transition_ms: 500,
                        recover_ms: 700,
                        landing_stick_ms: 83,
                        horizontal_damping_bits: 0.375_f32.to_bits(),
                    },
                    queued_air_attack: OptionalU8CodeSnapshot {
                        present: true,
                        code: 5,
                    },
                    queued_air_attack_ticks: 23,
                    ledge_grace_ticks: 24,
                    landing_stick_ticks: 25,
                    jump_takeoff_ticks: 26,
                    reaction_bounces: 2,
                    pig_air_meat_slam_air_hits: 3,
                    dash_slide_ticks: 27,
                    dash_jump_carry_ticks: 28,
                    dash_jump_carry_speed_limit_bits: 14.25_f32.to_bits(),
                    impact_speed_limit_ticks: 29,
                    impact_speed_limit_bits: 8.75_f32.to_bits(),
                    penguin_ice_slide_direction: OptionalF32Vec3BitsSnapshot {
                        present: true,
                        value: vec3_bits(0.5, 0.0, -0.5),
                    },
                    penguin_ice_slide_speed_bits: 11.5_f32.to_bits(),
                    guard_active_elapsed_ticks: 30,
                    guard_cooldown_ticks: 31,
                    guard_start_buffer_ticks: 32,
                    guard_counter_window_ticks: 33,
                    guard_counter_source: OptionalF32Vec3BitsSnapshot {
                        present: true,
                        value: vec3_bits(-1.25, 2.5, 3.75),
                    },
                },
                action: FighterActionRollbackSnapshot {
                    flags: FIGHTER_ACTION_HITBOX_SPAWNED
                        | FIGHTER_ACTION_QUEUED_COMBO
                        | FIGHTER_ACTION_CONFIRMED_HIT
                        | FIGHTER_ACTION_BRANCH_WINDOW_OPEN
                        | FIGHTER_ACTION_CHARGE_RELEASE_REQUESTED,
                    queued_technique: OptionalU16CodeSnapshot {
                        present: true,
                        code: 59,
                    },
                    queued_button: OptionalU8CodeSnapshot {
                        present: true,
                        code: 2,
                    },
                    buffered_button: OptionalU8CodeSnapshot {
                        present: true,
                        code: 8,
                    },
                    buffered_button_elapsed_ticks: 34,
                    technique_id: OptionalU16CodeSnapshot {
                        present: true,
                        code: 71,
                    },
                    timeline_events_fired: 0x0123_4567_89ab_cdef,
                    reaction_getup_ms: OptionalU32Snapshot {
                        present: true,
                        value: 450,
                    },
                    reaction_recover_ms: OptionalU32Snapshot {
                        present: true,
                        value: 0,
                    },
                    reaction_family: OptionalU8CodeSnapshot {
                        present: true,
                        code: 12,
                    },
                    charge_elapsed_ticks: 35,
                },
                regrab_lockout_ticks: 36,
            },
        };
        fighters[1] = FighterSnapshot {
            occupied: true,
            active: true,
            id: fighter_1,
            health: 200_000,
            stamina: 50_000,
            score: -3,
            relationships: FighterRelationshipsSnapshot {
                held_by: Some(fighter_0),
                last_attacker: Some(fighter_0),
                ..Default::default()
            },
            ..FighterSnapshot::empty(fighter_1)
        };

        let mut hitbox = DynamicObjectSnapshot::empty(hitbox_id);
        hitbox.definition_id = 15;
        hitbox.flags = 0x00ff_00aa;
        hitbox.owner = Some(fighter_0);
        hitbox.target = Some(fighter_1);
        hitbox.related_entity = Some(item_id);
        hitbox.fighter_hit_mask = 0b0010;
        hitbox.payload[0..8].copy_from_slice(&0x0123_4567_89ab_cdef_u64.to_le_bytes());

        let mut item = DynamicObjectSnapshot::empty(item_id);
        item.definition_id = 99;
        item.flags = 0x55aa_aa55;
        item.owner = Some(fighter_0);
        item.payload[0] = 0xfe;
        item.payload[DYNAMIC_PAYLOAD_BYTES - 1] = 0xef;

        let mut item_rng = DeterministicRngStream::from_master_seed(
            0xfeed_beef_dead_cafe,
            RngStreamName::from_label("items"),
        );
        let mut arena_rng = DeterministicRngStream::from_master_seed(
            0xfeed_beef_dead_cafe,
            RngStreamName::from_label("arena/hazards"),
        );
        for _ in 0..3 {
            item_rng.next_u64();
        }
        for _ in 0..5 {
            arena_rng.next_u64();
        }
        let mut rng_streams = vec![item_rng.snapshot().into(), arena_rng.snapshot().into()];
        rng_streams.sort_by_key(|rng: &NamedRngSnapshot| rng.stream.code());

        let mut arena = ArenaRuntimeSnapshot {
            arena_ticks: 2_222,
            hazard_clock_ticks: 333,
            logical_device_flags: 0x1020_3040_5060_7080,
            per_fighter_hazard_cooldowns: [1, 20, 300, 4_000],
            cannon_fire_cooldown_ticks: 17,
            cannon_index: 1,
            ..Default::default()
        };
        arena.pipes[0] = FighterPipeSnapshot {
            flags: 3,
            entry_endpoint: 4,
            exit_endpoint: 7,
            dwell_ticks: 8,
            cooldown_ticks: 9,
            transit_ticks: 10,
            entry_position: QuantizedVec3 {
                x: 11,
                y: 12,
                z: 13,
            },
        };
        arena.payload[0] = 0x45;
        arena.payload[ARENA_PAYLOAD_BYTES - 1] = 0x54;

        let mut stats = MatchStatsSnapshot {
            gameplay_ticks: 1_234,
            resolved_contacts: 567,
            emitted_events: 890,
            ring_outs: 3,
            falls: 2,
            item_hits: 11,
            throws: 7,
            guard_breaks: 5,
            damage_by_fighter: [4_096, 8_192, 12_288, 16_384],
            ..Default::default()
        };
        stats.fighter[0] = FighterMatchStatsSnapshot {
            damage_dealt: 10_000,
            damage_taken: 9_000,
            knockouts: 3,
            deaths: 2,
            item_uses: 4,
            actions_started: 500,
            quantized_distance_travelled: 1_000_000,
        };
        stats.rejected_dynamic_spawns[SimEntityKind::Hitbox.code() as usize] = 2;

        CanonicalSnapshot {
            header: SnapshotHeader::new(
                7,
                11,
                0x0123_4567_89ab_cdef,
                *b"fixture-match-id",
                SimTick(tick),
                0xfeed_beef_dead_cafe,
            ),
            match_state: MatchStateSnapshot {
                phase: MatchPhaseSnapshot::Fight,
                phase_ticks: 123,
                match_ticks_remaining: 7_200,
                hitstop_ticks: 2,
                next_event_ordinal: 44,
                active_slots_mask: 0b0011,
                teams: [0, 1, 0, 1],
                stocks: [2, 1, 0, 0],
                rules: MatchRulesSnapshot {
                    ruleset_id: 5,
                    arena_id: 8,
                    duration_ticks: 10_800,
                    starting_stocks: 3,
                    score_limit: 9,
                    team_mode: true,
                    friendly_fire: false,
                },
                result: MatchResultSnapshot::Pending,
            },
            fighters,
            arena,
            allocators,
            dynamic_objects: vec![hitbox, item],
            rng_streams,
            stats,
        }
    }

    fn decoder_after_arena(encoded: &[u8]) -> Decoder<'_> {
        let mut decoder = Decoder::new(encoded);
        decoder.read_array::<4>().unwrap();
        decoder.read_u16().unwrap();
        decoder.read_u32().unwrap();
        decoder.read_u32().unwrap();
        decoder.read_u32().unwrap();
        decoder.read_u64().unwrap();
        decoder.read_array::<MATCH_ID_BYTES>().unwrap();
        decoder.read_u64().unwrap();
        decoder.read_u64().unwrap();
        decoder.read_u32().unwrap();
        decode_match_state(&mut decoder).unwrap();
        for _ in 0..FIGHTER_CAPACITY {
            decode_fighter(&mut decoder).unwrap();
        }
        decode_arena(&mut decoder).unwrap();
        decoder
    }

    fn unique_subslice_offset(haystack: &[u8], needle: &[u8]) -> usize {
        let matches = haystack
            .windows(needle.len())
            .enumerate()
            .filter_map(|(offset, candidate)| (candidate == needle).then_some(offset))
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1, "test marker must occur exactly once");
        matches[0]
    }

    #[test]
    fn snapshot_round_trip_preserves_every_schema_group_and_bytes() {
        let snapshot = fixture(777);
        let encoded = snapshot.encode().unwrap();
        let restored = CanonicalSnapshot::decode(&encoded).unwrap();

        assert_eq!(restored, snapshot);
        assert_eq!(restored.encode().unwrap(), encoded);
        assert!(encoded.len() < MAX_SNAPSHOT_BYTES);
    }

    #[test]
    fn fighter_rollback_extension_has_frozen_width_and_round_trips_all_fields() {
        let rollback = fixture(777).fighters[0].rollback;
        let mut encoder = Encoder::new();
        encode_fighter_rollback(&mut encoder, rollback).unwrap();
        let encoded = encoder.finish();
        assert_eq!(encoded.len(), FIGHTER_ROLLBACK_EXTENSION_BYTES);

        let mut decoder = Decoder::new(&encoded);
        let restored = decode_fighter_rollback(&mut decoder).unwrap();
        assert_eq!(decoder.remaining(), 0);
        validate_fighter_rollback(restored).unwrap();
        assert_eq!(restored, rollback);

        let mut empty_encoder = Encoder::new();
        encode_fighter_rollback(&mut empty_encoder, Default::default()).unwrap();
        assert_eq!(empty_encoder.len(), FIGHTER_ROLLBACK_EXTENSION_BYTES);
    }

    #[test]
    fn hostile_decode_rejects_non_finite_exact_fighter_float() {
        let snapshot = fixture(778);
        let marker = snapshot.fighters[0]
            .rollback
            .stats
            .health_bits
            .to_le_bytes();
        let mut encoded = snapshot.encode().unwrap();
        let offset = unique_subslice_offset(&encoded, &marker);
        encoded[offset..offset + 4].copy_from_slice(&f32::NAN.to_bits().to_le_bytes());

        assert!(matches!(
            CanonicalSnapshot::decode(&encoded),
            Err(SnapshotError::InvalidValue {
                field: "fighter exact health",
                ..
            })
        ));
    }

    #[test]
    fn hostile_decode_rejects_invalid_fixed_option_tag() {
        let snapshot = fixture(779);
        let stats = snapshot.fighters[0].rollback.stats;
        let mut marker = Vec::new();
        marker.extend_from_slice(&stats.respawn_ticks.to_le_bytes());
        marker.push(1);
        marker.push(stats.element_carry.code);
        marker.extend_from_slice(&stats.element_carry_strength_bits.to_le_bytes());

        let mut encoded = snapshot.encode().unwrap();
        let offset = unique_subslice_offset(&encoded, &marker) + 4;
        encoded[offset] = 2;

        assert!(matches!(
            CanonicalSnapshot::decode(&encoded),
            Err(SnapshotError::InvalidValue {
                field: "elemental-carry presence tag",
                value: 2,
            })
        ));
    }

    #[test]
    fn hostile_decode_rejects_reserved_flags_and_absent_option_payload() {
        let snapshot = fixture(780);
        let motor = snapshot.fighters[0].rollback.motor;
        let mut marker = Vec::new();
        marker.extend_from_slice(&motor.facing.x.to_le_bytes());
        marker.extend_from_slice(&motor.facing.y.to_le_bytes());
        marker.extend_from_slice(&motor.facing.z.to_le_bytes());
        marker.extend_from_slice(&motor.flags.to_le_bytes());
        marker.push(1);
        marker.push(motor.landing_aftermath.family_code);

        let mut encoded = snapshot.encode().unwrap();
        let flags_offset = unique_subslice_offset(&encoded, &marker) + 12;
        encoded[flags_offset..flags_offset + 2].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(matches!(
            CanonicalSnapshot::decode(&encoded),
            Err(SnapshotError::InvalidValue {
                field: "fighter motor rollback flags",
                ..
            })
        ));

        let mut snapshot = fixture(781);
        snapshot.fighters[0].rollback.action.queued_technique = Default::default();
        let action = snapshot.fighters[0].rollback.action;
        let marker = [
            action.flags,
            0,
            0,
            0,
            1,
            action.queued_button.code,
            1,
            action.buffered_button.code,
        ];
        let mut encoded = snapshot.encode().unwrap();
        let absent_code_offset = unique_subslice_offset(&encoded, &marker) + 2;
        encoded[absent_code_offset] = 1;
        assert!(matches!(
            CanonicalSnapshot::decode(&encoded),
            Err(SnapshotError::InvalidValue {
                field: "queued technique code or absent padding",
                value: 1,
            })
        ));
    }

    #[test]
    fn envelope_has_frozen_magic_version_length_and_little_endian_header_order() {
        let snapshot = fixture(0x0102_0304_0506_0708);
        let encoded = snapshot.encode().unwrap();

        assert_eq!(&encoded[0..4], &SNAPSHOT_MAGIC);
        assert_eq!(
            u16::from_le_bytes(encoded[4..6].try_into().unwrap()),
            SNAPSHOT_SCHEMA_VERSION
        );
        assert_eq!(
            u32::from_le_bytes(encoded[6..10].try_into().unwrap()) as usize,
            encoded.len()
        );
        assert_eq!(
            u32::from_le_bytes(encoded[10..14].try_into().unwrap()),
            snapshot.header.simulation_version
        );
        assert_eq!(
            u32::from_le_bytes(encoded[14..18].try_into().unwrap()),
            snapshot.header.protocol_version
        );
        assert_eq!(
            u64::from_le_bytes(encoded[18..26].try_into().unwrap()),
            snapshot.header.gameplay_content_hash
        );
    }

    #[test]
    fn every_match_result_variant_round_trips() {
        let fighter = FighterId::new(0).unwrap();
        let variants = [
            MatchResultSnapshot::Draw {
                decided_tick: SimTick(50),
            },
            MatchResultSnapshot::FighterWinner {
                fighter,
                decided_tick: SimTick(51),
            },
            MatchResultSnapshot::TeamWinner {
                team: 1,
                decided_tick: SimTick(52),
            },
            MatchResultSnapshot::Aborted {
                reason: 600,
                decided_tick: SimTick(53),
            },
        ];

        for result in variants {
            let mut snapshot = fixture(60);
            snapshot.match_state.phase = MatchPhaseSnapshot::Result;
            snapshot.match_state.result = result;
            let restored = CanonicalSnapshot::decode(&snapshot.encode().unwrap()).unwrap();
            assert_eq!(restored.match_state.result, result);
        }
    }

    #[test]
    fn canonical_hash_is_stable_and_covers_state_changes() {
        let snapshot = fixture(99);
        let encoded = snapshot.encode().unwrap();
        let hash = snapshot.canonical_hash().unwrap();
        assert_eq!(hash, hash_canonical_bytes(&encoded));
        assert_eq!(hash, snapshot.clone().canonical_hash().unwrap());

        let mut changed = snapshot.clone();
        changed.fighters[0].health += 1;
        assert_ne!(hash, changed.canonical_hash().unwrap());

        changed = snapshot.clone();
        changed.header.gameplay_content_hash ^= 1;
        assert_ne!(hash, changed.canonical_hash().unwrap());
    }

    #[test]
    fn noncanonical_dynamic_and_rng_order_is_rejected() {
        let mut snapshot = fixture(10);
        snapshot.dynamic_objects.swap(0, 1);
        assert!(matches!(
            snapshot.encode(),
            Err(SnapshotError::NonCanonicalOrder {
                field: "dynamic objects"
            })
        ));

        let mut snapshot = fixture(10);
        snapshot.rng_streams.reverse();
        assert!(matches!(
            snapshot.encode(),
            Err(SnapshotError::NonCanonicalOrder {
                field: "RNG streams"
            })
        ));
    }

    #[test]
    fn fixed_fighter_slots_validate_identity_activity_and_empty_values() {
        let mut snapshot = fixture(3);
        snapshot.fighters.swap(0, 1);
        assert!(snapshot.validate().is_err());

        let mut snapshot = fixture(3);
        snapshot.match_state.active_slots_mask = 0b0001;
        assert!(snapshot.validate().is_err());

        let mut snapshot = fixture(3);
        snapshot.fighters[3].health = 1;
        assert!(snapshot.validate().is_err());
    }

    #[test]
    fn allocator_snapshot_covers_generation_occupancy_free_and_retired_slots() {
        let mut allocator = PoolAllocatorSnapshot::empty(SimEntityKind::Special, 3).unwrap();
        allocator.set_slot(1, 9, true).unwrap();
        allocator.set_slot(2, u32::MAX, false).unwrap();

        assert_eq!(allocator.generations, vec![1, 9, u32::MAX]);
        assert_eq!(allocator.free_indices, vec![0]);
        assert_eq!(allocator.is_occupied(0), Some(false));
        assert_eq!(allocator.is_occupied(1), Some(true));
        assert_eq!(allocator.is_occupied(2), Some(false));
        validate_allocator(&allocator).unwrap();

        allocator.free_indices.push(2);
        assert!(validate_allocator(&allocator).is_err());
    }

    #[test]
    fn stale_or_missing_dynamic_relationships_fail_closed() {
        let mut snapshot = fixture(3);
        snapshot.fighters[0].relationships.held_item = Some(SimEntityId::new(
            SimEntityKind::Item,
            0,
            snapshot.dynamic_objects[1].id.generation() + 1,
        ));
        assert!(snapshot.validate().is_err());

        let mut snapshot = fixture(3);
        snapshot.dynamic_objects[0].id = SimEntityId::new(SimEntityKind::Hitbox, 1, 99);
        assert!(snapshot.validate().is_err());
    }

    #[test]
    fn decoder_rejects_bad_magic_version_and_declared_lengths() {
        let encoded = fixture(1).encode().unwrap();

        let mut bad_magic = encoded.clone();
        bad_magic[0] ^= 0xff;
        assert!(matches!(
            CanonicalSnapshot::decode(&bad_magic),
            Err(SnapshotError::InvalidMagic(_))
        ));

        let mut bad_version = encoded.clone();
        bad_version[4..6].copy_from_slice(&(SNAPSHOT_SCHEMA_VERSION + 1).to_le_bytes());
        assert!(matches!(
            CanonicalSnapshot::decode(&bad_version),
            Err(SnapshotError::UnsupportedSchemaVersion { .. })
        ));

        let mut mismatch = encoded.clone();
        mismatch[6..10].copy_from_slice(&((encoded.len() + 1) as u32).to_le_bytes());
        assert!(matches!(
            CanonicalSnapshot::decode(&mismatch),
            Err(SnapshotError::DeclaredLengthMismatch { .. })
        ));

        let mut over_cap = encoded.clone();
        over_cap[6..10].copy_from_slice(&((MAX_SNAPSHOT_BYTES + 1) as u32).to_le_bytes());
        assert!(matches!(
            CanonicalSnapshot::decode(&over_cap),
            Err(SnapshotError::LimitExceeded {
                field: "declared snapshot bytes",
                ..
            })
        ));

        let mut trailing = encoded.clone();
        trailing.push(0);
        assert!(matches!(
            CanonicalSnapshot::decode(&trailing),
            Err(SnapshotError::DeclaredLengthMismatch { .. })
        ));
    }

    #[test]
    fn every_truncated_prefix_is_rejected() {
        let encoded = fixture(5).encode().unwrap();
        for length in 0..encoded.len() {
            assert!(
                CanonicalSnapshot::decode(&encoded[..length]).is_err(),
                "prefix length {length} unexpectedly decoded"
            );
        }
    }

    #[test]
    fn decoder_rejects_corrupt_vector_count_before_allocating() {
        let mut encoded = fixture(5).encode().unwrap();
        let mut decoder = decoder_after_arena(&encoded);
        let allocator_count = decoder.read_u8().unwrap();
        for _ in 0..allocator_count {
            decode_allocator(&mut decoder).unwrap();
        }
        let count_offset = decoder.offset;
        encoded[count_offset..count_offset + 4]
            .copy_from_slice(&((MAX_DYNAMIC_OBJECTS + 1) as u32).to_le_bytes());

        assert!(matches!(
            CanonicalSnapshot::decode(&encoded),
            Err(SnapshotError::LimitExceeded {
                field: "dynamic object count",
                ..
            })
        ));
    }

    #[test]
    fn decoder_rejects_corrupt_pool_capacity_before_pool_allocations() {
        let mut encoded = fixture(5).encode().unwrap();
        let mut decoder = decoder_after_arena(&encoded);
        decoder.read_u8().unwrap();
        decoder.read_u8().unwrap();
        let capacity_offset = decoder.offset;
        encoded[capacity_offset..capacity_offset + 4]
            .copy_from_slice(&((MAX_POOL_CAPACITY + 1) as u32).to_le_bytes());

        assert!(matches!(
            CanonicalSnapshot::decode(&encoded),
            Err(SnapshotError::LimitExceeded {
                field: "pool capacity",
                ..
            })
        ));
    }

    #[test]
    fn decoder_rejects_noncanonical_boolean_and_enum_values() {
        let encoded = fixture(5).encode().unwrap();
        let mut decoder = Decoder::new(&encoded);
        decoder.read_array::<4>().unwrap();
        decoder.read_u16().unwrap();
        decoder.read_u32().unwrap();
        decoder.read_u32().unwrap();
        decoder.read_u32().unwrap();
        decoder.read_u64().unwrap();
        decoder.read_array::<MATCH_ID_BYTES>().unwrap();
        decoder.read_u64().unwrap();
        decoder.read_u64().unwrap();
        decoder.read_u32().unwrap();
        let phase_offset = decoder.offset;

        let mut bad_phase = encoded.clone();
        bad_phase[phase_offset] = u8::MAX;
        assert!(matches!(
            CanonicalSnapshot::decode(&bad_phase),
            Err(SnapshotError::InvalidValue {
                field: "match phase",
                ..
            })
        ));

        decode_match_state(&mut decoder).unwrap();
        let occupied_offset = decoder.offset;
        let mut bad_bool = encoded;
        bad_bool[occupied_offset] = 2;
        assert!(matches!(
            CanonicalSnapshot::decode(&bad_bool),
            Err(SnapshotError::InvalidValue {
                field: "fighter occupied flag",
                ..
            })
        ));
    }

    #[test]
    fn configured_caps_are_enforced_for_encode_and_decode() {
        let mut too_many_objects = fixture(1);
        too_many_objects.dynamic_objects =
            vec![too_many_objects.dynamic_objects[0].clone(); MAX_DYNAMIC_OBJECTS + 1];
        assert!(matches!(
            too_many_objects.encode(),
            Err(SnapshotError::LimitExceeded {
                field: "dynamic object count",
                ..
            })
        ));

        let mut too_many_rng = fixture(1);
        too_many_rng.rng_streams = (0..=MAX_RNG_STREAMS)
            .map(|code| NamedRngSnapshot::new(RngStreamName::from_code(code as u64), 0, 0))
            .collect();
        assert!(matches!(
            too_many_rng.encode(),
            Err(SnapshotError::LimitExceeded {
                field: "RNG stream count",
                ..
            })
        ));

        let mut pool_too_large = fixture(1);
        pool_too_large.allocators[0].capacity = (MAX_POOL_CAPACITY + 1) as u32;
        assert!(matches!(
            pool_too_large.encode(),
            Err(SnapshotError::LimitExceeded {
                field: "pool capacity",
                ..
            })
        ));

        assert!(matches!(
            CanonicalSnapshot::decode(&vec![0; MAX_SNAPSHOT_BYTES + 1]),
            Err(SnapshotError::LimitExceeded {
                field: "encoded snapshot bytes",
                ..
            })
        ));

        let mut encoder = Encoder::new();
        assert!(matches!(
            encoder.write_bytes(&vec![0; MAX_SNAPSHOT_BYTES + 1]),
            Err(SnapshotError::LimitExceeded {
                field: "encoded snapshot bytes",
                ..
            })
        ));
    }

    #[test]
    fn total_pool_slot_cap_is_enforced_across_kinds() {
        let mut snapshot = fixture(1);
        snapshot.allocators = SimEntityKind::ALL
            .into_iter()
            .map(|kind| PoolAllocatorSnapshot::empty(kind, MAX_POOL_CAPACITY as u32).unwrap())
            .collect();
        snapshot.dynamic_objects.clear();
        snapshot.fighters[0].relationships.held_item = None;
        snapshot.fighters[0].relationships.linked_entity = None;

        assert!(matches!(
            snapshot.validate(),
            Err(SnapshotError::LimitExceeded {
                field: "total pool slots",
                ..
            })
        ));
    }

    #[test]
    fn metrics_hooks_observe_size_and_successful_restore_time() {
        let snapshot = fixture(10);
        let mut metrics = SnapshotMetricTotals::default();
        let encoded = snapshot.encode_with_metrics(&mut metrics).unwrap();
        let restored = CanonicalSnapshot::decode_with_metrics(&encoded, &mut metrics).unwrap();

        assert_eq!(restored, snapshot);
        assert_eq!(metrics.encoded_snapshots, 1);
        assert_eq!(metrics.restored_snapshots, 1);
        assert_eq!(metrics.last_encoded_bytes, encoded.len());
        assert_eq!(metrics.peak_encoded_bytes, encoded.len());
        assert!(metrics.peak_restore_nanoseconds <= metrics.total_restore_nanoseconds);
    }

    #[test]
    fn history_capacity_has_explicit_supported_bounds() {
        assert!(SnapshotHistory::new(MIN_SNAPSHOT_HISTORY).is_ok());
        assert!(SnapshotHistory::new(MAX_SNAPSHOT_HISTORY).is_ok());
        assert!(matches!(
            SnapshotHistory::new(MIN_SNAPSHOT_HISTORY - 1),
            Err(SnapshotError::InvalidHistoryCapacity { .. })
        ));
        assert!(matches!(
            SnapshotHistory::new(MAX_SNAPSHOT_HISTORY + 1),
            Err(SnapshotError::InvalidHistoryCapacity { .. })
        ));
    }

    #[test]
    fn full_history_overwrites_oldest_and_preserves_lookup_restore_and_hash() {
        let mut history = SnapshotHistory::new(MIN_SNAPSHOT_HISTORY).unwrap();
        for tick in 0..(MIN_SNAPSHOT_HISTORY as u64 + 3) {
            let snapshot = fixture(tick);
            let overwritten = history.insert(&snapshot).unwrap();
            if tick < MIN_SNAPSHOT_HISTORY as u64 {
                assert!(overwritten.is_none());
            } else {
                assert_eq!(
                    overwritten.unwrap().tick(),
                    SimTick(tick - MIN_SNAPSHOT_HISTORY as u64)
                );
            }
        }

        assert_eq!(history.len(), MIN_SNAPSHOT_HISTORY);
        assert_eq!(history.oldest().unwrap().tick(), SimTick(3));
        assert_eq!(
            history.newest().unwrap().tick(),
            SimTick(MIN_SNAPSHOT_HISTORY as u64 + 2)
        );
        assert!(history.get(SimTick(2)).is_none());
        let entry = history.get(SimTick(10)).unwrap();
        assert_eq!(entry.restore().unwrap(), fixture(10));
        assert_eq!(entry.hash(), fixture(10).canonical_hash().unwrap());
        assert_eq!(
            history.stored_bytes(),
            history
                .iter()
                .map(SnapshotHistoryEntry::encoded_len)
                .sum::<usize>()
        );
    }

    #[test]
    fn history_truncates_newer_entries_across_a_wrapped_ring() {
        let mut history = SnapshotHistory::new(MIN_SNAPSHOT_HISTORY).unwrap();
        for tick in 0..=40 {
            history.insert(&fixture(tick)).unwrap();
        }
        assert_eq!(history.oldest().unwrap().tick(), SimTick(9));

        assert_eq!(history.truncate_after(SimTick(20)), Some(20));
        assert_eq!(history.len(), 12);
        assert_eq!(history.oldest().unwrap().tick(), SimTick(9));
        assert_eq!(history.newest().unwrap().tick(), SimTick(20));
        assert!(history.get(SimTick(21)).is_none());
        assert_eq!(history.truncate_after(SimTick(8)), None);

        history.insert(&fixture(21)).unwrap();
        assert_eq!(history.newest().unwrap().tick(), SimTick(21));
        assert_eq!(
            history
                .iter()
                .map(|entry| entry.tick().get())
                .collect::<Vec<_>>(),
            (9..=21).collect::<Vec<_>>()
        );
    }

    #[test]
    fn history_rejects_duplicate_ticks_and_clear_resets_memory_accounting() {
        let mut history = SnapshotHistory::new(MIN_SNAPSHOT_HISTORY).unwrap();
        history.insert(&fixture(1)).unwrap();
        assert!(matches!(
            history.insert(&fixture(1)),
            Err(SnapshotError::DuplicateHistoryTick(1))
        ));
        assert!(history.stored_bytes() > 0);

        history.clear();
        assert!(history.is_empty());
        assert_eq!(history.stored_bytes(), 0);
        assert!(history.oldest().is_none());
        assert!(history.newest().is_none());
    }

    #[test]
    fn history_metrics_cover_encoding_and_entry_restore() {
        let mut history = SnapshotHistory::new(MIN_SNAPSHOT_HISTORY).unwrap();
        let mut metrics = SnapshotMetricTotals::default();
        history
            .insert_with_metrics(&fixture(88), &mut metrics)
            .unwrap();
        let entry = history.get(SimTick(88)).unwrap();
        entry.restore_with_metrics(&mut metrics).unwrap();

        assert_eq!(metrics.encoded_snapshots, 1);
        assert_eq!(metrics.restored_snapshots, 1);
        assert_eq!(metrics.last_encoded_bytes, entry.encoded_len());
    }
}
