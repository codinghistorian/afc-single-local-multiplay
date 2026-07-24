//! Production dynamic snapshot codecs for arena items and cannon ordnance.
//!
//! Each codec owns a versioned, fixed-width 128-byte payload. Authoritative
//! floats retain their exact finite `f32` bits, but capture and restore accept
//! only values already on the simulation's 1/4096 canonical grid. This makes a
//! capture/restore round trip lossless while rejecting state taken before the
//! end-of-tick canonicalization boundary.
//!
//! An item's entire `Transform`, plus mesh/scene handles, material handles,
//! `Name`, and `Visibility`, are deliberately presentation-only. Canonical
//! gameplay position lives in [`ArenaItem`] and is the only item position this
//! codec captures. Cannon position lives in [`SimPosition`]. The presentation
//! layer must rehydrate excluded components after restore; none may influence
//! simulation. The authored bob phase is retained only to avoid a visible
//! phase jump after rollback; bob displacement never enters gameplay position.

use bevy::prelude::*;

use crate::arena::ArenaCannonBomb;
use crate::components::SimPosition;
use crate::determinism::{
    DEFAULT_F32_QUANTIZATION, FighterHitMask, SimEntityId, SimEntityKind, canonicalize_f32,
};
use crate::ecs_identity::StableSimEntity;
use crate::items::{ArenaItem, ItemKind, ItemState};
use crate::simulation::{ElapsedTicks, TickTimer};
use crate::snapshot::{DYNAMIC_PAYLOAD_BYTES, DynamicObjectSnapshot};
use crate::snapshot_ecs::{DynamicSnapshotCodec, SnapshotCodecError};

const PAYLOAD_VERSION: u8 = 1;

const ERR_WRONG_KIND: u16 = 1;
const ERR_MISSING_COMPONENT: u16 = 2;
const ERR_IDENTITY_MISMATCH: u16 = 3;
const ERR_DEFINITION: u16 = 4;
const ERR_OUTER_FIELDS: u16 = 5;
const ERR_PAYLOAD_VERSION: u16 = 6;
const ERR_ENUM_CODE: u16 = 7;
const ERR_PADDING: u16 = 8;
const ERR_NON_CANONICAL_FLOAT: u16 = 9;
const ERR_ITEM_STATE: u16 = 10;
const ERR_TIMER: u16 = 11;

const ITEM_VERSION_OFFSET: usize = 0;
const ITEM_STATE_OFFSET: usize = 1;
const ITEM_KIND_OFFSET: usize = 2;
const ITEM_RESERVED_OFFSET: usize = 3;
const ITEM_POSITION_OFFSET: usize = 4;
const ITEM_ANCHOR_OFFSET: usize = 16;
const ITEM_VELOCITY_OFFSET: usize = 28;
const ITEM_BASE_Y_OFFSET: usize = 40;
const ITEM_PHASE_OFFSET: usize = 44;
const ITEM_DURABILITY_OFFSET: usize = 48;
const ITEM_MAX_DURABILITY_OFFSET: usize = 52;
const ITEM_RESPAWN_TIMER_OFFSET: usize = 56;
const ITEM_PICKUP_LOCKOUT_OFFSET: usize = 60;
const ITEM_STATE_AGE_OFFSET: usize = 64;
const ITEM_STATE_TIMER_A_OFFSET: usize = 68;
const ITEM_STATE_TIMER_B_OFFSET: usize = 72;
const ITEM_STATE_SCALAR_A_OFFSET: usize = 76;
const ITEM_STATE_SCALAR_B_OFFSET: usize = 80;
const ITEM_USED_BYTES: usize = 84;

const BOMB_VERSION_OFFSET: usize = 0;
const BOMB_TYPE_OFFSET: usize = 1;
const BOMB_RESERVED_OFFSET: usize = 2;
const BOMB_POSITION_OFFSET: usize = 4;
const BOMB_VELOCITY_OFFSET: usize = 16;
const BOMB_LIFETIME_OFFSET: usize = 28;
const BOMB_USED_BYTES: usize = 32;

const CANNON_BOMB_DEFINITION_ID: u16 = 0;
const CANNON_BOMB_TYPE_CODE: u8 = 0;

const _: () = assert!(ITEM_USED_BYTES <= DYNAMIC_PAYLOAD_BYTES);
const _: () = assert!(BOMB_USED_BYTES <= DYNAMIC_PAYLOAD_BYTES);

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ItemStateCode {
    Loose = 0,
    Held = 1,
    Thrown = 2,
    Armed = 3,
    Spraying = 4,
    Rolling = 5,
    Respawning = 6,
}

impl ItemStateCode {
    const fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Loose),
            1 => Some(Self::Held),
            2 => Some(Self::Thrown),
            3 => Some(Self::Armed),
            4 => Some(Self::Spraying),
            5 => Some(Self::Rolling),
            6 => Some(Self::Respawning),
            _ => None,
        }
    }
}

/// Live codec for [`ArenaItem`] entities in the stable item pool.
#[derive(Clone, Copy, Debug, Default)]
pub struct LiveItemSnapshotCodec;

/// Live codec for [`ArenaCannonBomb`] entities in the stable arena-ordnance pool.
#[derive(Clone, Copy, Debug, Default)]
pub struct LiveArenaOrdnanceSnapshotCodec;

struct DecodedItem {
    item: ArenaItem,
}

#[derive(Clone, Copy)]
struct DecodedBomb {
    translation: Vec3,
    velocity: Vec3,
    lifetime: TickTimer,
}

impl DynamicSnapshotCodec for LiveItemSnapshotCodec {
    fn capture(
        &self,
        world: &World,
        entity: Entity,
        id: SimEntityId,
    ) -> Result<DynamicObjectSnapshot, SnapshotCodecError> {
        require_kind(id, SimEntityKind::Item)?;
        require_stable_identity(world, entity, id)?;
        let item = required::<ArenaItem>(world, entity)?;

        let state_code = item_state_code(item.state);
        let owner = item_state_owner(item.state);
        let mut payload = [0; DYNAMIC_PAYLOAD_BYTES];
        payload[ITEM_VERSION_OFFSET] = PAYLOAD_VERSION;
        payload[ITEM_STATE_OFFSET] = state_code as u8;
        payload[ITEM_KIND_OFFSET] = item_kind_code(item.kind);
        write_vec3(&mut payload, ITEM_POSITION_OFFSET, item.position);
        write_vec3(&mut payload, ITEM_ANCHOR_OFFSET, item.anchor);
        write_vec3(&mut payload, ITEM_VELOCITY_OFFSET, item.velocity);
        write_f32(&mut payload, ITEM_BASE_Y_OFFSET, item.base_y);
        write_f32(&mut payload, ITEM_PHASE_OFFSET, item.snapshot_phase());
        write_i32(&mut payload, ITEM_DURABILITY_OFFSET, item.durability);
        write_i32(
            &mut payload,
            ITEM_MAX_DURABILITY_OFFSET,
            item.max_durability,
        );
        write_u32(
            &mut payload,
            ITEM_RESPAWN_TIMER_OFFSET,
            item.respawn_timer.remaining(),
        );
        write_u32(
            &mut payload,
            ITEM_PICKUP_LOCKOUT_OFFSET,
            item.pickup_lockout.remaining(),
        );
        write_u32(&mut payload, ITEM_STATE_AGE_OFFSET, item.state_age.get());
        write_item_state_payload(&mut payload, item.state);

        let snapshot = DynamicObjectSnapshot {
            id,
            definition_id: u16::from(item_kind_code(item.kind)),
            flags: 0,
            owner,
            target: None,
            related_entity: item.crate_source(),
            fighter_hit_mask: item.already_hit.bits(),
            payload,
        };
        // Capture is also a validation boundary. This rejects noncanonical live
        // values and impossible component combinations instead of serializing a
        // state that no peer can safely restore.
        decode_item(&snapshot)?;
        Ok(snapshot)
    }

    fn validate_restore(
        &self,
        _world: &World,
        snapshot: &DynamicObjectSnapshot,
    ) -> Result<(), SnapshotCodecError> {
        decode_item(snapshot).map(|_| ())
    }

    fn restore_validated(
        &self,
        world: &mut World,
        entity: Entity,
        snapshot: &DynamicObjectSnapshot,
    ) {
        let decoded = decode_item(snapshot)
            .expect("item payload was fully validated before snapshot restore mutation");
        // Existing rendered worlds retain their presentation components;
        // newly spawned authority/prediction entities remain render-free.
        world.entity_mut(entity).insert(decoded.item);
    }
}

impl DynamicSnapshotCodec for LiveArenaOrdnanceSnapshotCodec {
    fn capture(
        &self,
        world: &World,
        entity: Entity,
        id: SimEntityId,
    ) -> Result<DynamicObjectSnapshot, SnapshotCodecError> {
        require_kind(id, SimEntityKind::ArenaOrdnance)?;
        require_stable_identity(world, entity, id)?;
        let position = required::<SimPosition>(world, entity)?;
        let bomb = required::<ArenaCannonBomb>(world, entity)?;

        let mut payload = [0; DYNAMIC_PAYLOAD_BYTES];
        payload[BOMB_VERSION_OFFSET] = PAYLOAD_VERSION;
        payload[BOMB_TYPE_OFFSET] = CANNON_BOMB_TYPE_CODE;
        write_vec3(&mut payload, BOMB_POSITION_OFFSET, position.translation);
        write_vec3(&mut payload, BOMB_VELOCITY_OFFSET, bomb.velocity);
        write_u32(
            &mut payload,
            BOMB_LIFETIME_OFFSET,
            bomb.lifetime.remaining(),
        );
        let snapshot = DynamicObjectSnapshot {
            id,
            definition_id: CANNON_BOMB_DEFINITION_ID,
            flags: 0,
            owner: None,
            target: None,
            related_entity: None,
            fighter_hit_mask: 0,
            payload,
        };
        decode_bomb(&snapshot)?;
        Ok(snapshot)
    }

    fn validate_restore(
        &self,
        _world: &World,
        snapshot: &DynamicObjectSnapshot,
    ) -> Result<(), SnapshotCodecError> {
        decode_bomb(snapshot).map(|_| ())
    }

    fn restore_validated(
        &self,
        world: &mut World,
        entity: Entity,
        snapshot: &DynamicObjectSnapshot,
    ) {
        let decoded = decode_bomb(snapshot)
            .expect("cannon-bomb payload was fully validated before snapshot restore mutation");
        world.entity_mut(entity).insert((
            SimPosition::new(decoded.translation),
            ArenaCannonBomb {
                velocity: decoded.velocity,
                lifetime: decoded.lifetime,
            },
        ));
    }
}

fn decode_item(snapshot: &DynamicObjectSnapshot) -> Result<DecodedItem, SnapshotCodecError> {
    require_kind(snapshot.id, SimEntityKind::Item)?;
    if snapshot.flags != 0 || snapshot.target.is_some() {
        return Err(error(
            ERR_OUTER_FIELDS,
            "item snapshot has noncanonical flags or unsupported relationships",
        ));
    }
    if snapshot
        .related_entity
        .is_some_and(|source| source.kind() != SimEntityKind::Item || source == snapshot.id)
    {
        return Err(error(
            ERR_OUTER_FIELDS,
            "item crate provenance must reference a different item entity",
        ));
    }
    let hit_mask = FighterHitMask::from_bits(snapshot.fighter_hit_mask).ok_or(error(
        ERR_OUTER_FIELDS,
        "item snapshot fighter-hit mask uses reserved fighter bits",
    ))?;
    if snapshot.payload[ITEM_VERSION_OFFSET] != PAYLOAD_VERSION {
        return Err(error(
            ERR_PAYLOAD_VERSION,
            "unsupported item payload version",
        ));
    }
    if snapshot.payload[ITEM_RESERVED_OFFSET] != 0
        || snapshot.payload[ITEM_USED_BYTES..]
            .iter()
            .any(|byte| *byte != 0)
    {
        return Err(error(
            ERR_PADDING,
            "item payload reserved or padding bytes are nonzero",
        ));
    }

    let kind = item_kind_from_code(snapshot.payload[ITEM_KIND_OFFSET]).ok_or(error(
        ERR_ENUM_CODE,
        "item payload contains an unknown item-kind code",
    ))?;
    if snapshot.definition_id != u16::from(item_kind_code(kind)) {
        return Err(error(
            ERR_DEFINITION,
            "item definition ID disagrees with payload item-kind code",
        ));
    }
    let state_code = ItemStateCode::from_u8(snapshot.payload[ITEM_STATE_OFFSET]).ok_or(error(
        ERR_ENUM_CODE,
        "item payload contains an unknown item-state code",
    ))?;

    let position = read_canonical_vec3(&snapshot.payload, ITEM_POSITION_OFFSET)?;
    let anchor = read_canonical_vec3(&snapshot.payload, ITEM_ANCHOR_OFFSET)?;
    let velocity = read_canonical_vec3(&snapshot.payload, ITEM_VELOCITY_OFFSET)?;
    let base_y = read_canonical_f32(&snapshot.payload, ITEM_BASE_Y_OFFSET)?;
    let phase = read_canonical_f32(&snapshot.payload, ITEM_PHASE_OFFSET)?;
    let durability = read_i32(&snapshot.payload, ITEM_DURABILITY_OFFSET);
    let max_durability = read_i32(&snapshot.payload, ITEM_MAX_DURABILITY_OFFSET);
    let respawn_timer =
        TickTimer::from_ticks(read_u32(&snapshot.payload, ITEM_RESPAWN_TIMER_OFFSET));
    let pickup_lockout =
        TickTimer::from_ticks(read_u32(&snapshot.payload, ITEM_PICKUP_LOCKOUT_OFFSET));
    let state_age = ElapsedTicks::from_ticks(read_u32(&snapshot.payload, ITEM_STATE_AGE_OFFSET));
    let timer_a = read_u32(&snapshot.payload, ITEM_STATE_TIMER_A_OFFSET);
    let timer_b = read_u32(&snapshot.payload, ITEM_STATE_TIMER_B_OFFSET);
    let scalar_a_bits = read_u32(&snapshot.payload, ITEM_STATE_SCALAR_A_OFFSET);
    let scalar_b_bits = read_u32(&snapshot.payload, ITEM_STATE_SCALAR_B_OFFSET);

    let state = decode_item_state(
        state_code,
        snapshot.owner,
        timer_a,
        timer_b,
        scalar_a_bits,
        scalar_b_bits,
    )?;
    let item = ArenaItem::from_snapshot_parts(
        kind,
        state,
        respawn_timer,
        durability,
        max_durability,
        pickup_lockout,
        snapshot.related_entity,
        position,
        anchor,
        velocity,
        hit_mask,
        base_y,
        state_age,
        phase,
    )
    .ok_or(error(
        ERR_ITEM_STATE,
        "item snapshot violates live component state invariants",
    ))?;

    Ok(DecodedItem { item })
}

fn decode_item_state(
    code: ItemStateCode,
    owner: Option<crate::determinism::FighterId>,
    timer_a: u32,
    timer_b: u32,
    scalar_a_bits: u32,
    scalar_b_bits: u32,
) -> Result<ItemState, SnapshotCodecError> {
    let no_aux = || {
        if timer_a == 0 && timer_b == 0 && scalar_a_bits == 0 && scalar_b_bits == 0 {
            Ok(())
        } else {
            Err(error(
                ERR_ITEM_STATE,
                "item state has nonzero data in an unused state slot",
            ))
        }
    };
    let no_owner = || {
        if owner.is_none() {
            Ok(())
        } else {
            Err(error(
                ERR_ITEM_STATE,
                "ownerless item state contains an owner relationship",
            ))
        }
    };
    let required_owner = || {
        owner.ok_or(error(
            ERR_ITEM_STATE,
            "owned item state is missing its fighter relationship",
        ))
    };

    match code {
        ItemStateCode::Loose => {
            no_owner()?;
            no_aux()?;
            Ok(ItemState::Loose)
        }
        ItemStateCode::Held => {
            let holder = required_owner()?;
            no_aux()?;
            Ok(ItemState::Held { holder })
        }
        ItemStateCode::Thrown => {
            let owner = required_owner()?;
            require_zero_scalar_slots(scalar_a_bits, scalar_b_bits)?;
            let lifetime = finite_active_timer(timer_a)?;
            let grace = finite_timer(timer_b)?;
            Ok(ItemState::Thrown {
                owner,
                lifetime,
                grace,
            })
        }
        ItemStateCode::Armed => {
            let owner = required_owner()?;
            require_zero_scalar_slots(scalar_a_bits, scalar_b_bits)?;
            let timer = finite_active_timer(timer_a)?;
            let grace = finite_timer(timer_b)?;
            Ok(ItemState::Armed {
                owner,
                timer,
                grace,
            })
        }
        ItemStateCode::Spraying => {
            let owner = required_owner()?;
            let lifetime = finite_active_timer(timer_a)?;
            let spray_timer = finite_timer(timer_b)?;
            let spiral_phase = canonical_f32_from_bits(scalar_a_bits)?;
            let spiral_radius = canonical_f32_from_bits(scalar_b_bits)?;
            Ok(ItemState::Spraying {
                owner,
                lifetime,
                spray_timer,
                spiral_phase,
                spiral_radius,
            })
        }
        ItemStateCode::Rolling => {
            no_owner()?;
            if timer_b != 0 || scalar_a_bits != 0 || scalar_b_bits != 0 {
                return Err(error(
                    ERR_ITEM_STATE,
                    "rolling item has nonzero data in an unused state slot",
                ));
            }
            Ok(ItemState::Rolling {
                lifetime: finite_active_timer(timer_a)?,
            })
        }
        ItemStateCode::Respawning => {
            no_owner()?;
            no_aux()?;
            Ok(ItemState::Respawning)
        }
    }
}

fn decode_bomb(snapshot: &DynamicObjectSnapshot) -> Result<DecodedBomb, SnapshotCodecError> {
    require_kind(snapshot.id, SimEntityKind::ArenaOrdnance)?;
    if snapshot.definition_id != CANNON_BOMB_DEFINITION_ID {
        return Err(error(
            ERR_DEFINITION,
            "arena ordnance definition is not a cannon bomb",
        ));
    }
    if snapshot.flags != 0
        || snapshot.owner.is_some()
        || snapshot.target.is_some()
        || snapshot.related_entity.is_some()
        || snapshot.fighter_hit_mask != 0
    {
        return Err(error(
            ERR_OUTER_FIELDS,
            "cannon-bomb snapshot has noncanonical outer fields",
        ));
    }
    if snapshot.payload[BOMB_VERSION_OFFSET] != PAYLOAD_VERSION {
        return Err(error(
            ERR_PAYLOAD_VERSION,
            "unsupported cannon-bomb payload version",
        ));
    }
    if snapshot.payload[BOMB_TYPE_OFFSET] != CANNON_BOMB_TYPE_CODE {
        return Err(error(ERR_ENUM_CODE, "unknown arena-ordnance payload type"));
    }
    if snapshot.payload[BOMB_RESERVED_OFFSET..BOMB_POSITION_OFFSET]
        .iter()
        .chain(snapshot.payload[BOMB_USED_BYTES..].iter())
        .any(|byte| *byte != 0)
    {
        return Err(error(
            ERR_PADDING,
            "cannon-bomb payload reserved or padding bytes are nonzero",
        ));
    }

    Ok(DecodedBomb {
        translation: read_canonical_vec3(&snapshot.payload, BOMB_POSITION_OFFSET)?,
        velocity: read_canonical_vec3(&snapshot.payload, BOMB_VELOCITY_OFFSET)?,
        lifetime: finite_active_timer(read_u32(&snapshot.payload, BOMB_LIFETIME_OFFSET))?,
    })
}

fn required<T: Component>(world: &World, entity: Entity) -> Result<&T, SnapshotCodecError> {
    world.get::<T>(entity).ok_or(error(
        ERR_MISSING_COMPONENT,
        "dynamic entity is missing a required authoritative component",
    ))
}

fn require_stable_identity(
    world: &World,
    entity: Entity,
    id: SimEntityId,
) -> Result<(), SnapshotCodecError> {
    let stable = required::<StableSimEntity>(world, entity)?;
    if stable.id() == id {
        Ok(())
    } else {
        Err(error(
            ERR_IDENTITY_MISMATCH,
            "dynamic entity StableSimEntity disagrees with allocator ID",
        ))
    }
}

fn require_kind(id: SimEntityId, expected: SimEntityKind) -> Result<(), SnapshotCodecError> {
    if id.kind() == expected {
        Ok(())
    } else {
        Err(error(
            ERR_WRONG_KIND,
            "dynamic snapshot codec received the wrong stable pool kind",
        ))
    }
}

const fn item_kind_code(kind: ItemKind) -> u8 {
    match kind {
        ItemKind::Crate => 0,
        ItemKind::Steamer => 1,
        ItemKind::Apple => 2,
        ItemKind::WineWhite => 3,
        ItemKind::Turkey => 4,
        ItemKind::Barrel => 5,
        ItemKind::CupCoffee => 6,
        ItemKind::Mushroom => 7,
    }
}

const fn item_kind_from_code(code: u8) -> Option<ItemKind> {
    match code {
        0 => Some(ItemKind::Crate),
        1 => Some(ItemKind::Steamer),
        2 => Some(ItemKind::Apple),
        3 => Some(ItemKind::WineWhite),
        4 => Some(ItemKind::Turkey),
        5 => Some(ItemKind::Barrel),
        6 => Some(ItemKind::CupCoffee),
        7 => Some(ItemKind::Mushroom),
        _ => None,
    }
}

const fn item_state_code(state: ItemState) -> ItemStateCode {
    match state {
        ItemState::Loose => ItemStateCode::Loose,
        ItemState::Held { .. } => ItemStateCode::Held,
        ItemState::Thrown { .. } => ItemStateCode::Thrown,
        ItemState::Armed { .. } => ItemStateCode::Armed,
        ItemState::Spraying { .. } => ItemStateCode::Spraying,
        ItemState::Rolling { .. } => ItemStateCode::Rolling,
        ItemState::Respawning => ItemStateCode::Respawning,
    }
}

const fn item_state_owner(state: ItemState) -> Option<crate::determinism::FighterId> {
    match state {
        ItemState::Held { holder } => Some(holder),
        ItemState::Thrown { owner, .. }
        | ItemState::Armed { owner, .. }
        | ItemState::Spraying { owner, .. } => Some(owner),
        ItemState::Loose | ItemState::Rolling { .. } | ItemState::Respawning => None,
    }
}

fn write_item_state_payload(payload: &mut [u8; DYNAMIC_PAYLOAD_BYTES], state: ItemState) {
    match state {
        ItemState::Loose | ItemState::Held { .. } | ItemState::Respawning => {}
        ItemState::Thrown {
            lifetime, grace, ..
        } => {
            write_u32(payload, ITEM_STATE_TIMER_A_OFFSET, lifetime.remaining());
            write_u32(payload, ITEM_STATE_TIMER_B_OFFSET, grace.remaining());
        }
        ItemState::Armed { timer, grace, .. } => {
            write_u32(payload, ITEM_STATE_TIMER_A_OFFSET, timer.remaining());
            write_u32(payload, ITEM_STATE_TIMER_B_OFFSET, grace.remaining());
        }
        ItemState::Spraying {
            lifetime,
            spray_timer,
            spiral_phase,
            spiral_radius,
            ..
        } => {
            write_u32(payload, ITEM_STATE_TIMER_A_OFFSET, lifetime.remaining());
            write_u32(payload, ITEM_STATE_TIMER_B_OFFSET, spray_timer.remaining());
            write_f32(payload, ITEM_STATE_SCALAR_A_OFFSET, spiral_phase);
            write_f32(payload, ITEM_STATE_SCALAR_B_OFFSET, spiral_radius);
        }
        ItemState::Rolling { lifetime } => {
            write_u32(payload, ITEM_STATE_TIMER_A_OFFSET, lifetime.remaining());
        }
    }
}

fn finite_timer(ticks: u32) -> Result<TickTimer, SnapshotCodecError> {
    let timer = TickTimer::from_ticks(ticks);
    if timer.is_indefinite() {
        Err(error(
            ERR_TIMER,
            "finite item-state timer uses the indefinite sentinel",
        ))
    } else {
        Ok(timer)
    }
}

fn finite_active_timer(ticks: u32) -> Result<TickTimer, SnapshotCodecError> {
    let timer = finite_timer(ticks)?;
    if timer.active() {
        Ok(timer)
    } else {
        Err(error(
            ERR_TIMER,
            "active dynamic state has an expired timer",
        ))
    }
}

fn require_zero_scalar_slots(a: u32, b: u32) -> Result<(), SnapshotCodecError> {
    if a == 0 && b == 0 {
        Ok(())
    } else {
        Err(error(
            ERR_ITEM_STATE,
            "item state has nonzero data in an unused scalar slot",
        ))
    }
}

fn write_u32(payload: &mut [u8; DYNAMIC_PAYLOAD_BYTES], offset: usize, value: u32) {
    payload[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_i32(payload: &mut [u8; DYNAMIC_PAYLOAD_BYTES], offset: usize, value: i32) {
    payload[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_f32(payload: &mut [u8; DYNAMIC_PAYLOAD_BYTES], offset: usize, value: f32) {
    write_u32(payload, offset, value.to_bits());
}

fn write_vec3(payload: &mut [u8; DYNAMIC_PAYLOAD_BYTES], offset: usize, value: Vec3) {
    write_f32(payload, offset, value.x);
    write_f32(payload, offset + 4, value.y);
    write_f32(payload, offset + 8, value.z);
}

fn read_u32(payload: &[u8; DYNAMIC_PAYLOAD_BYTES], offset: usize) -> u32 {
    u32::from_le_bytes(
        payload[offset..offset + 4]
            .try_into()
            .expect("fixed payload offsets are compile-time bounded"),
    )
}

fn read_i32(payload: &[u8; DYNAMIC_PAYLOAD_BYTES], offset: usize) -> i32 {
    i32::from_le_bytes(
        payload[offset..offset + 4]
            .try_into()
            .expect("fixed payload offsets are compile-time bounded"),
    )
}

fn read_canonical_f32(
    payload: &[u8; DYNAMIC_PAYLOAD_BYTES],
    offset: usize,
) -> Result<f32, SnapshotCodecError> {
    canonical_f32_from_bits(read_u32(payload, offset))
}

fn canonical_f32_from_bits(bits: u32) -> Result<f32, SnapshotCodecError> {
    let value = f32::from_bits(bits);
    if value.is_finite()
        && canonicalize_f32(value, DEFAULT_F32_QUANTIZATION).to_bits() == value.to_bits()
    {
        Ok(value)
    } else {
        Err(error(
            ERR_NON_CANONICAL_FLOAT,
            "dynamic payload float is non-finite or off the canonical grid",
        ))
    }
}

fn read_canonical_vec3(
    payload: &[u8; DYNAMIC_PAYLOAD_BYTES],
    offset: usize,
) -> Result<Vec3, SnapshotCodecError> {
    Ok(Vec3::new(
        read_canonical_f32(payload, offset)?,
        read_canonical_f32(payload, offset + 4)?,
        read_canonical_f32(payload, offset + 8)?,
    ))
}

const fn error(code: u16, message: &'static str) -> SnapshotCodecError {
    SnapshotCodecError::new(code, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::determinism::FighterId;

    fn item_id(index: u32) -> SimEntityId {
        SimEntityId::new(SimEntityKind::Item, index, 1)
    }

    fn bomb_id(index: u32) -> SimEntityId {
        SimEntityId::new(SimEntityKind::ArenaOrdnance, index, 1)
    }

    fn q(value: f32) -> f32 {
        canonicalize_f32(value, DEFAULT_F32_QUANTIZATION)
    }

    fn canonical_vec3(x: f32, y: f32, z: f32) -> Vec3 {
        Vec3::new(q(x), q(y), q(z))
    }

    #[allow(clippy::too_many_arguments)]
    fn test_item(
        kind: ItemKind,
        state: ItemState,
        respawn_timer: TickTimer,
        velocity: Vec3,
        hit_mask: FighterHitMask,
        pickup_lockout: TickTimer,
        state_age: ElapsedTicks,
    ) -> ArenaItem {
        let max_durability: i32 = match kind {
            ItemKind::Turkey | ItemKind::Barrel => 3,
            _ => 1,
        };
        ArenaItem::from_snapshot_parts(
            kind,
            state,
            respawn_timer,
            max_durability.saturating_sub(1),
            max_durability,
            pickup_lockout,
            None,
            canonical_vec3(1.25, 0.5, -2.0),
            canonical_vec3(1.25, 0.5, -2.0),
            velocity,
            hit_mask,
            q(0.5),
            state_age,
            q(0.25),
        )
        .unwrap()
    }

    fn spawn_item(
        world: &mut World,
        id: SimEntityId,
        mut item: ArenaItem,
        position: Vec3,
    ) -> Entity {
        item.position = position;
        world
            .spawn((
                StableSimEntity::new(id),
                item,
                Transform::from_translation(position)
                    .with_rotation(Quat::from_rotation_y(0.75))
                    .with_scale(Vec3::splat(3.0)),
            ))
            .id()
    }

    #[test]
    fn every_item_state_round_trips_all_authoritative_fields() {
        let fighter = FighterId::new(2).unwrap();
        let mut mask = FighterHitMask::default();
        mask.insert(FighterId::new(1).unwrap());
        let cases = [
            test_item(
                ItemKind::Apple,
                ItemState::Loose,
                TickTimer::ZERO,
                Vec3::ZERO,
                FighterHitMask::default(),
                TickTimer::from_ticks(3),
                ElapsedTicks::from_ticks(11),
            ),
            test_item(
                ItemKind::Turkey,
                ItemState::Held { holder: fighter },
                TickTimer::ZERO,
                Vec3::ZERO,
                FighterHitMask::default(),
                TickTimer::ZERO,
                ElapsedTicks::ZERO,
            ),
            test_item(
                ItemKind::Apple,
                ItemState::Thrown {
                    owner: fighter,
                    lifetime: TickTimer::from_ticks(51),
                    grace: TickTimer::from_ticks(2),
                },
                TickTimer::ZERO,
                canonical_vec3(3.0, 1.25, -0.5),
                mask,
                TickTimer::from_ticks(8),
                ElapsedTicks::from_ticks(4),
            ),
            test_item(
                ItemKind::Steamer,
                ItemState::Armed {
                    owner: fighter,
                    timer: TickTimer::from_ticks(42),
                    grace: TickTimer::ZERO,
                },
                TickTimer::ZERO,
                canonical_vec3(2.0, 0.25, -0.75),
                FighterHitMask::default(),
                TickTimer::from_ticks(5),
                ElapsedTicks::from_ticks(7),
            ),
            test_item(
                ItemKind::Barrel,
                ItemState::Spraying {
                    owner: fighter,
                    lifetime: TickTimer::from_ticks(35),
                    spray_timer: TickTimer::from_ticks(4),
                    spiral_phase: q(1.5),
                    spiral_radius: q(0.75),
                },
                TickTimer::ZERO,
                canonical_vec3(0.5, 0.0, -1.25),
                FighterHitMask::default(),
                TickTimer::from_ticks(35),
                ElapsedTicks::from_ticks(9),
            ),
            test_item(
                ItemKind::Turkey,
                ItemState::Rolling {
                    lifetime: TickTimer::from_ticks(18),
                },
                TickTimer::ZERO,
                canonical_vec3(1.0, 0.25, 1.5),
                FighterHitMask::default(),
                TickTimer::from_ticks(2),
                ElapsedTicks::from_ticks(6),
            ),
            test_item(
                ItemKind::Mushroom,
                ItemState::Respawning,
                TickTimer::from_ticks(120),
                Vec3::ZERO,
                FighterHitMask::default(),
                TickTimer::ZERO,
                ElapsedTicks::ZERO,
            ),
        ];

        for (index, item) in cases.into_iter().enumerate() {
            let id = item_id(index as u32);
            let position = canonical_vec3(index as f32, 2.25, -3.5);
            let mut source = World::new();
            let source_entity = spawn_item(&mut source, id, item, position);
            let snapshot = LiveItemSnapshotCodec
                .capture(&source, source_entity, id)
                .unwrap();

            let mut restored = World::new();
            let target = restored.spawn(StableSimEntity::new(id)).id();
            LiveItemSnapshotCodec
                .validate_restore(&restored, &snapshot)
                .unwrap();
            LiveItemSnapshotCodec.restore_validated(&mut restored, target, &snapshot);
            let captured_again = LiveItemSnapshotCodec
                .capture(&restored, target, id)
                .unwrap();
            assert_eq!(captured_again, snapshot);

            assert!(restored.get::<Transform>(target).is_none());
            assert_eq!(
                restored.get::<ArenaItem>(target).unwrap().position,
                position
            );
        }
    }

    #[test]
    fn cannon_bomb_round_trips_authoritative_state_only() {
        let id = bomb_id(3);
        let mut source = World::new();
        let entity = source
            .spawn((
                StableSimEntity::new(id),
                ArenaCannonBomb {
                    velocity: canonical_vec3(7.5, 4.0, -1.0),
                    lifetime: TickTimer::from_ticks(177),
                },
                SimPosition::new(canonical_vec3(-4.0, 2.5, 3.0)),
                Transform::from_translation(canonical_vec3(-4.0, 2.5, 3.0))
                    .with_rotation(Quat::from_rotation_x(1.2))
                    .with_scale(Vec3::splat(2.0)),
            ))
            .id();
        let snapshot = LiveArenaOrdnanceSnapshotCodec
            .capture(&source, entity, id)
            .unwrap();

        let mut restored = World::new();
        let target = restored.spawn(StableSimEntity::new(id)).id();
        LiveArenaOrdnanceSnapshotCodec
            .validate_restore(&restored, &snapshot)
            .unwrap();
        LiveArenaOrdnanceSnapshotCodec.restore_validated(&mut restored, target, &snapshot);
        assert_eq!(
            LiveArenaOrdnanceSnapshotCodec
                .capture(&restored, target, id)
                .unwrap(),
            snapshot
        );
        assert_eq!(
            restored.get::<SimPosition>(target).unwrap().translation,
            canonical_vec3(-4.0, 2.5, 3.0)
        );
        assert!(restored.get::<Transform>(target).is_none());
    }

    #[test]
    fn crate_reward_provenance_round_trips_as_stable_item_relationship() {
        let source_id = item_id(0);
        let reward_id = item_id(1);
        let position = canonical_vec3(2.5, 0.5, -1.25);
        let reward = ArenaItem::new_crate_reward(ItemKind::Apple, position, q(0.75), source_id);
        let mut world = World::new();
        let entity = spawn_item(&mut world, reward_id, reward, position);

        let snapshot = LiveItemSnapshotCodec
            .capture(&world, entity, reward_id)
            .unwrap();
        assert_eq!(snapshot.related_entity, Some(source_id));

        let mut restored = World::new();
        let target = restored.spawn(StableSimEntity::new(reward_id)).id();
        LiveItemSnapshotCodec
            .validate_restore(&restored, &snapshot)
            .unwrap();
        LiveItemSnapshotCodec.restore_validated(&mut restored, target, &snapshot);
        assert_eq!(
            restored.get::<ArenaItem>(target).unwrap().crate_source(),
            Some(source_id)
        );
        assert_eq!(
            LiveItemSnapshotCodec
                .capture(&restored, target, reward_id)
                .unwrap(),
            snapshot
        );
    }

    #[test]
    fn capture_is_independent_of_reversed_bevy_entity_allocation_order() {
        let high = item_id(1);
        let low = item_id(0);
        let mut world = World::new();
        let high_entity = spawn_item(
            &mut world,
            high,
            test_item(
                ItemKind::Apple,
                ItemState::Loose,
                TickTimer::ZERO,
                Vec3::ZERO,
                FighterHitMask::default(),
                TickTimer::ZERO,
                ElapsedTicks::ZERO,
            ),
            canonical_vec3(2.0, 0.5, 0.0),
        );
        let low_entity = spawn_item(
            &mut world,
            low,
            test_item(
                ItemKind::Mushroom,
                ItemState::Loose,
                TickTimer::ZERO,
                Vec3::ZERO,
                FighterHitMask::default(),
                TickTimer::ZERO,
                ElapsedTicks::ZERO,
            ),
            canonical_vec3(-2.0, 0.5, 0.0),
        );
        assert!(high_entity.index() < low_entity.index());

        let low_snapshot = LiveItemSnapshotCodec
            .capture(&world, low_entity, low)
            .unwrap();
        let high_snapshot = LiveItemSnapshotCodec
            .capture(&world, high_entity, high)
            .unwrap();
        assert!(low_snapshot.id < high_snapshot.id);
        assert_eq!(
            low_snapshot.definition_id,
            u16::from(item_kind_code(ItemKind::Mushroom))
        );
        assert_eq!(
            high_snapshot.definition_id,
            u16::from(item_kind_code(ItemKind::Apple))
        );
    }

    #[test]
    fn hostile_item_payloads_and_outer_fields_fail_without_mutation() {
        let id = item_id(0);
        let mut source = World::new();
        let entity = spawn_item(
            &mut source,
            id,
            test_item(
                ItemKind::Apple,
                ItemState::Loose,
                TickTimer::ZERO,
                Vec3::ZERO,
                FighterHitMask::default(),
                TickTimer::ZERO,
                ElapsedTicks::ZERO,
            ),
            canonical_vec3(1.0, 2.0, 3.0),
        );
        let valid = LiveItemSnapshotCodec.capture(&source, entity, id).unwrap();

        let mut restored = World::new();
        let sentinel = canonical_vec3(9.0, 8.0, 7.0);
        let target = restored
            .spawn((
                StableSimEntity::new(id),
                Transform::from_translation(sentinel),
            ))
            .id();
        let mut hostile = Vec::new();

        let mut bad = valid.clone();
        bad.payload[ITEM_VERSION_OFFSET] = PAYLOAD_VERSION + 1;
        hostile.push(bad);
        let mut bad = valid.clone();
        bad.payload[DYNAMIC_PAYLOAD_BYTES - 1] = 1;
        hostile.push(bad);
        let mut bad = valid.clone();
        bad.payload[ITEM_STATE_OFFSET] = u8::MAX;
        hostile.push(bad);
        let mut bad = valid.clone();
        write_f32(&mut bad.payload, ITEM_POSITION_OFFSET, 0.1);
        hostile.push(bad);
        let mut bad = valid.clone();
        bad.owner = Some(FighterId::ZERO);
        hostile.push(bad);
        let mut bad = valid.clone();
        bad.related_entity = Some(id);
        hostile.push(bad);
        let mut bad = valid.clone();
        bad.related_entity = Some(SimEntityId::new(SimEntityKind::Hitbox, 0, 1));
        hostile.push(bad);
        let mut bad = valid.clone();
        bad.fighter_hit_mask = 0x80;
        hostile.push(bad);
        let mut bad = valid.clone();
        write_u32(&mut bad.payload, ITEM_STATE_TIMER_A_OFFSET, 1);
        hostile.push(bad);

        for bad in hostile {
            assert!(
                LiveItemSnapshotCodec
                    .validate_restore(&restored, &bad)
                    .is_err()
            );
            assert_eq!(
                restored.get::<Transform>(target).unwrap().translation,
                sentinel
            );
            assert!(restored.get::<ArenaItem>(target).is_none());
        }
    }

    #[test]
    fn wrong_kind_and_invalid_owner_timer_combinations_fail_closed() {
        let id = item_id(0);
        let mut source = World::new();
        let entity = spawn_item(
            &mut source,
            id,
            test_item(
                ItemKind::Apple,
                ItemState::Thrown {
                    owner: FighterId::ZERO,
                    lifetime: TickTimer::from_ticks(10),
                    grace: TickTimer::ZERO,
                },
                TickTimer::ZERO,
                canonical_vec3(1.0, 1.0, 0.0),
                FighterHitMask::default(),
                TickTimer::ZERO,
                ElapsedTicks::from_ticks(1),
            ),
            canonical_vec3(1.0, 2.0, 3.0),
        );
        let valid = LiveItemSnapshotCodec.capture(&source, entity, id).unwrap();

        let mut missing_owner = valid.clone();
        missing_owner.owner = None;
        assert!(
            LiveItemSnapshotCodec
                .validate_restore(&source, &missing_owner)
                .is_err()
        );
        let mut expired = valid.clone();
        write_u32(&mut expired.payload, ITEM_STATE_TIMER_A_OFFSET, 0);
        assert!(
            LiveItemSnapshotCodec
                .validate_restore(&source, &expired)
                .is_err()
        );
        let mut wrong_kind = valid;
        wrong_kind.id = SimEntityId::new(SimEntityKind::Hitbox, 0, 1);
        assert!(
            LiveItemSnapshotCodec
                .validate_restore(&source, &wrong_kind)
                .is_err()
        );
    }

    #[test]
    fn hostile_bomb_payload_and_expired_timer_fail_closed() {
        let id = bomb_id(0);
        let mut source = World::new();
        let entity = source
            .spawn((
                StableSimEntity::new(id),
                ArenaCannonBomb {
                    velocity: canonical_vec3(1.0, 2.0, 3.0),
                    lifetime: TickTimer::from_ticks(10),
                },
                SimPosition::new(canonical_vec3(4.0, 5.0, 6.0)),
                Transform::from_translation(canonical_vec3(4.0, 5.0, 6.0)),
            ))
            .id();
        let valid = LiveArenaOrdnanceSnapshotCodec
            .capture(&source, entity, id)
            .unwrap();
        let mut bad_padding = valid.clone();
        bad_padding.payload[DYNAMIC_PAYLOAD_BYTES - 1] = 9;
        assert!(
            LiveArenaOrdnanceSnapshotCodec
                .validate_restore(&source, &bad_padding)
                .is_err()
        );
        let mut expired = valid.clone();
        write_u32(&mut expired.payload, BOMB_LIFETIME_OFFSET, 0);
        assert!(
            LiveArenaOrdnanceSnapshotCodec
                .validate_restore(&source, &expired)
                .is_err()
        );
        let mut off_grid = valid;
        write_f32(&mut off_grid.payload, BOMB_VELOCITY_OFFSET, 0.1);
        assert!(
            LiveArenaOrdnanceSnapshotCodec
                .validate_restore(&source, &off_grid)
                .is_err()
        );
    }

    #[test]
    fn missing_authoritative_components_and_noncanonical_live_floats_are_rejected() {
        let first_item_id = item_id(0);
        let mut world = World::new();
        let presentation_free_item = world
            .spawn((
                StableSimEntity::new(first_item_id),
                test_item(
                    ItemKind::Apple,
                    ItemState::Loose,
                    TickTimer::ZERO,
                    Vec3::ZERO,
                    FighterHitMask::default(),
                    TickTimer::ZERO,
                    ElapsedTicks::ZERO,
                ),
            ))
            .id();
        assert!(
            LiveItemSnapshotCodec
                .capture(&world, presentation_free_item, first_item_id)
                .is_ok(),
            "an item render Transform is not authoritative snapshot state"
        );
        let missing_item = world
            .spawn((
                StableSimEntity::new(first_item_id),
                Transform::from_translation(Vec3::ZERO),
            ))
            .id();
        assert!(
            LiveItemSnapshotCodec
                .capture(&world, missing_item, first_item_id)
                .is_err()
        );

        let noncanonical_id = item_id(1);
        let noncanonical = spawn_item(
            &mut world,
            noncanonical_id,
            test_item(
                ItemKind::Apple,
                ItemState::Loose,
                TickTimer::ZERO,
                Vec3::ZERO,
                FighterHitMask::default(),
                TickTimer::ZERO,
                ElapsedTicks::ZERO,
            ),
            Vec3::new(0.1, 0.0, 0.0),
        );
        assert!(
            LiveItemSnapshotCodec
                .capture(&world, noncanonical, noncanonical_id)
                .is_err()
        );

        let bomb_id = bomb_id(0);
        let missing_bomb = world
            .spawn((
                StableSimEntity::new(bomb_id),
                Transform::from_translation(Vec3::ZERO),
            ))
            .id();
        assert!(
            LiveArenaOrdnanceSnapshotCodec
                .capture(&world, missing_bomb, bomb_id)
                .is_err()
        );
    }

    #[test]
    fn item_snapshot_bytes_ignore_arbitrary_render_transform_mutation() {
        let id = item_id(7);
        let mut world = World::new();
        let canonical_position = canonical_vec3(3.25, 1.5, -4.75);
        let entity = spawn_item(
            &mut world,
            id,
            test_item(
                ItemKind::Apple,
                ItemState::Loose,
                TickTimer::ZERO,
                Vec3::ZERO,
                FighterHitMask::default(),
                TickTimer::ZERO,
                ElapsedTicks::from_ticks(12),
            ),
            canonical_position,
        );
        let before = LiveItemSnapshotCodec.capture(&world, entity, id).unwrap();

        *world.get_mut::<Transform>(entity).unwrap() =
            Transform::from_translation(Vec3::new(-9_999.25, 88.5, 42.0))
                .with_rotation(Quat::from_xyzw(0.1, 0.2, 0.3, 0.9).normalize())
                .with_scale(Vec3::new(77.0, 0.01, 5.5));

        let after = LiveItemSnapshotCodec.capture(&world, entity, id).unwrap();
        assert_eq!(after, before);
        assert_eq!(
            world.get::<ArenaItem>(entity).unwrap().position,
            canonical_position
        );

        world.entity_mut(entity).remove::<Transform>();
        assert_eq!(
            LiveItemSnapshotCodec.capture(&world, entity, id).unwrap(),
            before
        );
    }
}
