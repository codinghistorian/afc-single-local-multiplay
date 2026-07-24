//! Production snapshot codecs for live Bee and Chick skill entities.
//!
//! Both codecs use the same versioned 128-byte envelope. The authoritative
//! payload occupies exactly 72 bytes and leaves 56 canonical zero-padding bytes.
//! Stable ownership, optional target, and per-fighter hit memory live in the
//! canonical dynamic-record fields rather than being duplicated in the payload.
//!
//! [`SimPosition`] is the authoritative translation used by skill collision and
//! motion. Bevy transforms, render handles, names, and child visuals are
//! deliberately excluded and must be rehydrated by presentation.

use bevy::prelude::*;

use crate::bee_skills::{ActiveBeeSkill, BeeSkillKind};
use crate::chick_skills::{ActiveChickSkill, ChickSkillKind};
use crate::combat::ImpactSource;
use crate::components::SimPosition;
use crate::determinism::{
    DEFAULT_F32_QUANTIZATION, FighterHitMask, FighterId, SimEntityId, SimEntityKind,
    canonicalize_f32,
};
use crate::ecs_identity::StableSimEntity;
use crate::simulation::{ElapsedTicks, TickTimer, seconds_to_ticks_ceil};
use crate::snapshot::{DYNAMIC_PAYLOAD_BYTES, DynamicObjectSnapshot};
use crate::snapshot_ecs::{DynamicSnapshotCodec, SnapshotCodecError};
use crate::styles::FighterStyleKind;
use crate::techniques::{AttackPayloadId, AttackShapeId};

const PAYLOAD_VERSION: u8 = 1;

const ERR_WRONG_KIND: u16 = 1;
const ERR_MISSING_COMPONENT: u16 = 2;
const ERR_IDENTITY_MISMATCH: u16 = 3;
const ERR_COMPONENT_IDENTITY: u16 = 4;
const ERR_DEFINITION: u16 = 5;
const ERR_OUTER_FIELDS: u16 = 6;
const ERR_PAYLOAD_VERSION: u16 = 7;
const ERR_ENUM_CODE: u16 = 8;
const ERR_PADDING: u16 = 9;
const ERR_NON_CANONICAL_FLOAT: u16 = 10;
const ERR_TIMER: u16 = 11;
const ERR_STATIC_DEFINITION: u16 = 12;
const ERR_RELATIONSHIP: u16 = 13;

const VERSION_OFFSET: usize = 0;
const KIND_OFFSET: usize = 1;
const STYLE_OFFSET: usize = 2;
const ATTACK_PAYLOAD_OFFSET: usize = 3;
const SHAPE_OFFSET: usize = 4;
const SOURCE_OFFSET: usize = 5;
const REPEAT_PRESENT_OFFSET: usize = 6;
const RESERVED_OFFSET: usize = 7;
const POSITION_OFFSET: usize = 8;
const FACING_OFFSET: usize = 20;
const VELOCITY_OFFSET: usize = 32;
const LIFETIME_OFFSET: usize = 44;
const AGE_OFFSET: usize = 48;
const RADIUS_OFFSET: usize = 52;
const GUARD_DAMAGE_OFFSET: usize = 56;
const REPEAT_INTERVAL_OFFSET: usize = 60;
const REPEAT_TIMER_OFFSET: usize = 64;
const SIZE_SCALE_OFFSET: usize = 68;

/// Exact number of meaningful bytes in either character-skill payload.
pub const CHARACTER_SKILL_PAYLOAD_USED_BYTES: usize = 72;
/// Exact number of canonical zero-padding bytes in either payload.
pub const CHARACTER_SKILL_PAYLOAD_PADDING_BYTES: usize =
    DYNAMIC_PAYLOAD_BYTES - CHARACTER_SKILL_PAYLOAD_USED_BYTES;

const _: () = assert!(CHARACTER_SKILL_PAYLOAD_USED_BYTES <= DYNAMIC_PAYLOAD_BYTES);
const _: () = assert!(CHARACTER_SKILL_PAYLOAD_PADDING_BYTES == 56);

/// Live codec for stable [`SimEntityKind::BeeSkill`] entities.
#[derive(Clone, Copy, Debug, Default)]
pub struct LiveBeeSkillSnapshotCodec;

/// Live codec for stable [`SimEntityKind::ChickSkill`] entities.
#[derive(Clone, Copy, Debug, Default)]
pub struct LiveChickSkillSnapshotCodec;

#[derive(Clone, Copy)]
struct BeeDefinition {
    payload_id: AttackPayloadId,
    shape_id: AttackShapeId,
    source: ImpactSource,
    maximum_lifetime_ticks: u32,
    base_radius: f32,
    guard_stamina_damage: f32,
    repeat_interval_ticks: Option<u32>,
    allows_target: bool,
}

#[derive(Clone, Copy)]
struct ChickDefinition {
    payload_id: Option<AttackPayloadId>,
    shape_id: AttackShapeId,
    source: ImpactSource,
    maximum_lifetime_ticks: u32,
    base_radius: f32,
    guard_stamina_damage: f32,
    repeat_interval_ticks: Option<u32>,
    uses_hit_memory: bool,
}

struct DecodedBee {
    translation: Vec3,
    skill: ActiveBeeSkill,
}

struct DecodedChick {
    translation: Vec3,
    skill: ActiveChickSkill,
}

impl DynamicSnapshotCodec for LiveBeeSkillSnapshotCodec {
    fn capture(
        &self,
        world: &World,
        entity: Entity,
        id: SimEntityId,
    ) -> Result<DynamicObjectSnapshot, SnapshotCodecError> {
        require_kind(id, SimEntityKind::BeeSkill)?;
        require_stable_identity(world, entity, id)?;
        if world.get::<ActiveChickSkill>(entity).is_some() {
            return Err(error(
                ERR_COMPONENT_IDENTITY,
                "Bee skill entity also contains a Chick skill component",
            ));
        }
        let position = required::<SimPosition>(world, entity)?;
        let skill = required::<ActiveBeeSkill>(world, entity)?;
        let mut payload = [0; DYNAMIC_PAYLOAD_BYTES];
        encode_common(
            &mut payload,
            bee_kind_code(skill.kind),
            skill.owner_style,
            Some(skill.payload_id),
            skill.shape_id,
            skill.source,
            position.translation,
            skill.facing,
            skill.velocity,
            skill.lifetime,
            skill.age,
            skill.radius,
            skill.guard_stamina_damage,
            skill.repeat_interval,
            skill.repeat_timer,
            skill.size_scale,
        )?;
        let snapshot = DynamicObjectSnapshot {
            id,
            definition_id: u16::from(bee_kind_code(skill.kind)),
            flags: 0,
            owner: Some(skill.owner),
            target: skill.target,
            related_entity: None,
            fighter_hit_mask: skill.already_hit.bits(),
            payload,
        };
        decode_bee(&snapshot)?;
        Ok(snapshot)
    }

    fn validate_restore(
        &self,
        _world: &World,
        snapshot: &DynamicObjectSnapshot,
    ) -> Result<(), SnapshotCodecError> {
        decode_bee(snapshot).map(|_| ())
    }

    fn restore_validated(
        &self,
        world: &mut World,
        entity: Entity,
        snapshot: &DynamicObjectSnapshot,
    ) {
        let decoded = decode_bee(snapshot)
            .expect("Bee skill payload was fully validated before restore mutation");
        world
            .entity_mut(entity)
            .insert((SimPosition::new(decoded.translation), decoded.skill));
    }
}

impl DynamicSnapshotCodec for LiveChickSkillSnapshotCodec {
    fn capture(
        &self,
        world: &World,
        entity: Entity,
        id: SimEntityId,
    ) -> Result<DynamicObjectSnapshot, SnapshotCodecError> {
        require_kind(id, SimEntityKind::ChickSkill)?;
        require_stable_identity(world, entity, id)?;
        if world.get::<ActiveBeeSkill>(entity).is_some() {
            return Err(error(
                ERR_COMPONENT_IDENTITY,
                "Chick skill entity also contains a Bee skill component",
            ));
        }
        let position = required::<SimPosition>(world, entity)?;
        let skill = required::<ActiveChickSkill>(world, entity)?;
        let mut payload = [0; DYNAMIC_PAYLOAD_BYTES];
        encode_common(
            &mut payload,
            chick_kind_code(skill.kind),
            skill.owner_style,
            skill.payload_id,
            skill.shape_id,
            skill.source,
            position.translation,
            skill.facing,
            skill.velocity,
            skill.lifetime,
            skill.age,
            skill.radius,
            skill.guard_stamina_damage,
            skill.repeat_interval,
            skill.repeat_timer,
            skill.size_scale,
        )?;
        let snapshot = DynamicObjectSnapshot {
            id,
            definition_id: u16::from(chick_kind_code(skill.kind)),
            flags: 0,
            owner: Some(skill.owner),
            target: None,
            related_entity: None,
            fighter_hit_mask: skill.already_hit.bits(),
            payload,
        };
        decode_chick(&snapshot)?;
        Ok(snapshot)
    }

    fn validate_restore(
        &self,
        _world: &World,
        snapshot: &DynamicObjectSnapshot,
    ) -> Result<(), SnapshotCodecError> {
        decode_chick(snapshot).map(|_| ())
    }

    fn restore_validated(
        &self,
        world: &mut World,
        entity: Entity,
        snapshot: &DynamicObjectSnapshot,
    ) {
        let decoded = decode_chick(snapshot)
            .expect("Chick skill payload was fully validated before restore mutation");
        world
            .entity_mut(entity)
            .insert((SimPosition::new(decoded.translation), decoded.skill));
    }
}

#[allow(clippy::too_many_arguments)]
fn encode_common(
    payload: &mut [u8; DYNAMIC_PAYLOAD_BYTES],
    kind_code: u8,
    owner_style: FighterStyleKind,
    payload_id: Option<AttackPayloadId>,
    shape_id: AttackShapeId,
    source: ImpactSource,
    translation: Vec3,
    facing: Vec3,
    velocity: Vec3,
    lifetime: TickTimer,
    age: ElapsedTicks,
    radius: f32,
    guard_stamina_damage: f32,
    repeat_interval: Option<TickTimer>,
    repeat_timer: Option<TickTimer>,
    size_scale: f32,
) -> Result<(), SnapshotCodecError> {
    payload[VERSION_OFFSET] = PAYLOAD_VERSION;
    payload[KIND_OFFSET] = kind_code;
    payload[STYLE_OFFSET] = style_code(owner_style);
    payload[ATTACK_PAYLOAD_OFFSET] = attack_payload_code(payload_id).ok_or(error(
        ERR_ENUM_CODE,
        "character skill uses an unsupported attack payload",
    ))?;
    payload[SHAPE_OFFSET] = shape_code(shape_id).ok_or(error(
        ERR_ENUM_CODE,
        "character skill uses an unsupported attack shape",
    ))?;
    payload[SOURCE_OFFSET] = source_code(source).ok_or(error(
        ERR_ENUM_CODE,
        "character skill uses an unsupported impact source",
    ))?;
    payload[REPEAT_PRESENT_OFFSET] = u8::from(repeat_interval.is_some());
    write_vec3(payload, POSITION_OFFSET, translation);
    write_vec3(payload, FACING_OFFSET, facing);
    write_vec3(payload, VELOCITY_OFFSET, velocity);
    write_u32(payload, LIFETIME_OFFSET, lifetime.remaining());
    write_u32(payload, AGE_OFFSET, age.get());
    write_f32(payload, RADIUS_OFFSET, radius);
    write_f32(payload, GUARD_DAMAGE_OFFSET, guard_stamina_damage);
    write_u32(
        payload,
        REPEAT_INTERVAL_OFFSET,
        repeat_interval.map_or(0, TickTimer::remaining),
    );
    write_u32(
        payload,
        REPEAT_TIMER_OFFSET,
        repeat_timer.map_or(0, TickTimer::remaining),
    );
    write_f32(payload, SIZE_SCALE_OFFSET, size_scale);
    Ok(())
}

fn decode_bee(snapshot: &DynamicObjectSnapshot) -> Result<DecodedBee, SnapshotCodecError> {
    require_kind(snapshot.id, SimEntityKind::BeeSkill)?;
    validate_outer_common(snapshot)?;
    let owner = required_owner(snapshot)?;
    let hit_mask = validated_hit_mask(snapshot, owner)?;
    validate_payload_envelope(snapshot)?;

    let kind = bee_kind_from_code(snapshot.payload[KIND_OFFSET]).ok_or(error(
        ERR_ENUM_CODE,
        "Bee skill payload contains an unknown kind code",
    ))?;
    if snapshot.definition_id != u16::from(bee_kind_code(kind)) {
        return Err(error(
            ERR_DEFINITION,
            "Bee skill definition ID disagrees with its payload kind",
        ));
    }
    let definition = bee_definition(kind);
    if snapshot.target == Some(owner) || (snapshot.target.is_some() && !definition.allows_target) {
        return Err(error(
            ERR_RELATIONSHIP,
            "Bee skill target is illegal for its owner or skill kind",
        ));
    }
    let common = decode_common(snapshot)?;
    validate_common_static(
        &common,
        definition.payload_id.into(),
        definition.shape_id,
        definition.source,
        definition.maximum_lifetime_ticks,
        definition.base_radius,
        definition.guard_stamina_damage,
        definition.repeat_interval_ticks,
    )?;

    Ok(DecodedBee {
        translation: common.translation,
        skill: ActiveBeeSkill {
            kind,
            owner,
            owner_style: common.owner_style,
            payload_id: definition.payload_id,
            shape_id: definition.shape_id,
            source: definition.source,
            facing: common.facing,
            velocity: common.velocity,
            target: snapshot.target,
            lifetime: common.lifetime,
            age: common.age,
            radius: common.radius,
            guard_stamina_damage: common.guard_stamina_damage,
            repeat_interval: common.repeat_interval,
            repeat_timer: common.repeat_timer,
            already_hit: hit_mask,
            size_scale: common.size_scale,
        },
    })
}

fn decode_chick(snapshot: &DynamicObjectSnapshot) -> Result<DecodedChick, SnapshotCodecError> {
    require_kind(snapshot.id, SimEntityKind::ChickSkill)?;
    validate_outer_common(snapshot)?;
    if snapshot.target.is_some() {
        return Err(error(
            ERR_RELATIONSHIP,
            "Chick skill cannot contain a target relationship",
        ));
    }
    let owner = required_owner(snapshot)?;
    let hit_mask = validated_hit_mask(snapshot, owner)?;
    validate_payload_envelope(snapshot)?;

    let kind = chick_kind_from_code(snapshot.payload[KIND_OFFSET]).ok_or(error(
        ERR_ENUM_CODE,
        "Chick skill payload contains an unknown kind code",
    ))?;
    if snapshot.definition_id != u16::from(chick_kind_code(kind)) {
        return Err(error(
            ERR_DEFINITION,
            "Chick skill definition ID disagrees with its payload kind",
        ));
    }
    let definition = chick_definition(kind);
    if !definition.uses_hit_memory && !hit_mask.is_empty() {
        return Err(error(
            ERR_RELATIONSHIP,
            "visual or continuously-hitting Chick skill has noncanonical hit memory",
        ));
    }
    let common = decode_common(snapshot)?;
    validate_common_static(
        &common,
        definition.payload_id,
        definition.shape_id,
        definition.source,
        definition.maximum_lifetime_ticks,
        definition.base_radius,
        definition.guard_stamina_damage,
        definition.repeat_interval_ticks,
    )?;

    Ok(DecodedChick {
        translation: common.translation,
        skill: ActiveChickSkill {
            kind,
            owner,
            owner_style: common.owner_style,
            payload_id: definition.payload_id,
            shape_id: definition.shape_id,
            source: definition.source,
            facing: common.facing,
            velocity: common.velocity,
            lifetime: common.lifetime,
            age: common.age,
            radius: common.radius,
            guard_stamina_damage: common.guard_stamina_damage,
            repeat_interval: common.repeat_interval,
            repeat_timer: common.repeat_timer,
            already_hit: hit_mask,
            size_scale: common.size_scale,
        },
    })
}

struct DecodedCommon {
    owner_style: FighterStyleKind,
    payload_id: Option<AttackPayloadId>,
    shape_id: AttackShapeId,
    source: ImpactSource,
    translation: Vec3,
    facing: Vec3,
    velocity: Vec3,
    lifetime: TickTimer,
    age: ElapsedTicks,
    radius: f32,
    guard_stamina_damage: f32,
    repeat_interval: Option<TickTimer>,
    repeat_timer: Option<TickTimer>,
    size_scale: f32,
}

fn decode_common(snapshot: &DynamicObjectSnapshot) -> Result<DecodedCommon, SnapshotCodecError> {
    let owner_style = style_from_code(snapshot.payload[STYLE_OFFSET]).ok_or(error(
        ERR_ENUM_CODE,
        "character skill payload contains an unknown fighter style",
    ))?;
    let payload_id =
        attack_payload_from_code(snapshot.payload[ATTACK_PAYLOAD_OFFSET]).ok_or(error(
            ERR_ENUM_CODE,
            "character skill payload contains an unknown attack payload",
        ))?;
    let shape_id = shape_from_code(snapshot.payload[SHAPE_OFFSET]).ok_or(error(
        ERR_ENUM_CODE,
        "character skill payload contains an unknown attack shape",
    ))?;
    let source = source_from_code(snapshot.payload[SOURCE_OFFSET]).ok_or(error(
        ERR_ENUM_CODE,
        "character skill payload contains an unknown impact source",
    ))?;
    let lifetime = finite_active_timer(read_u32(&snapshot.payload, LIFETIME_OFFSET))?;
    let age = ElapsedTicks::from_ticks(read_u32(&snapshot.payload, AGE_OFFSET));
    let repeat_present = match snapshot.payload[REPEAT_PRESENT_OFFSET] {
        0 => false,
        1 => true,
        _ => {
            return Err(error(
                ERR_TIMER,
                "character skill repeat presence is not a canonical boolean",
            ));
        }
    };
    let repeat_interval_ticks = read_u32(&snapshot.payload, REPEAT_INTERVAL_OFFSET);
    let repeat_timer_ticks = read_u32(&snapshot.payload, REPEAT_TIMER_OFFSET);
    let (repeat_interval, repeat_timer) = if repeat_present {
        let interval = finite_active_timer(repeat_interval_ticks)?;
        let timer = finite_active_timer(repeat_timer_ticks)?;
        if timer.remaining() > interval.remaining() {
            return Err(error(
                ERR_TIMER,
                "character skill repeat timer exceeds its interval",
            ));
        }
        (Some(interval), Some(timer))
    } else if repeat_interval_ticks == 0 && repeat_timer_ticks == 0 {
        (None, None)
    } else {
        return Err(error(
            ERR_TIMER,
            "absent repeat timers must have canonical zero payloads",
        ));
    };

    Ok(DecodedCommon {
        owner_style,
        payload_id,
        shape_id,
        source,
        translation: read_canonical_vec3(&snapshot.payload, POSITION_OFFSET)?,
        facing: read_canonical_vec3(&snapshot.payload, FACING_OFFSET)?,
        velocity: read_canonical_vec3(&snapshot.payload, VELOCITY_OFFSET)?,
        lifetime,
        age,
        radius: read_canonical_f32(&snapshot.payload, RADIUS_OFFSET)?,
        guard_stamina_damage: read_canonical_f32(&snapshot.payload, GUARD_DAMAGE_OFFSET)?,
        repeat_interval,
        repeat_timer,
        size_scale: read_canonical_f32(&snapshot.payload, SIZE_SCALE_OFFSET)?,
    })
}

#[allow(clippy::too_many_arguments)]
fn validate_common_static(
    common: &DecodedCommon,
    expected_payload: Option<AttackPayloadId>,
    expected_shape: AttackShapeId,
    expected_source: ImpactSource,
    maximum_lifetime_ticks: u32,
    base_radius: f32,
    expected_guard_damage: f32,
    expected_repeat_ticks: Option<u32>,
) -> Result<(), SnapshotCodecError> {
    if common.payload_id != expected_payload
        || common.shape_id != expected_shape
        || common.source != expected_source
    {
        return Err(error(
            ERR_STATIC_DEFINITION,
            "character skill attack identity disagrees with its static definition",
        ));
    }
    if common.lifetime.remaining() > maximum_lifetime_ticks
        || common
            .age
            .get()
            .checked_add(common.lifetime.remaining())
            .is_none_or(|total| total > maximum_lifetime_ticks)
    {
        return Err(error(
            ERR_TIMER,
            "character skill lifetime and age exceed its static lifetime",
        ));
    }
    if common.size_scale < canonicalize_f32(0.1, DEFAULT_F32_QUANTIZATION) {
        return Err(error(
            ERR_STATIC_DEFINITION,
            "character skill size scale is below the authored minimum",
        ));
    }
    let expected_radius =
        canonicalize_f32(base_radius * common.size_scale, DEFAULT_F32_QUANTIZATION);
    let expected_guard_damage = canonicalize_f32(expected_guard_damage, DEFAULT_F32_QUANTIZATION);
    if common.radius.to_bits() != expected_radius.to_bits()
        || common.guard_stamina_damage.to_bits() != expected_guard_damage.to_bits()
    {
        return Err(error(
            ERR_STATIC_DEFINITION,
            "character skill radius or guard damage disagrees with its static definition",
        ));
    }

    match (
        expected_repeat_ticks,
        common.repeat_interval,
        common.repeat_timer,
    ) {
        (None, None, None) => Ok(()),
        (Some(expected), Some(interval), Some(_)) if interval.remaining() == expected => Ok(()),
        _ => Err(error(
            ERR_STATIC_DEFINITION,
            "character skill repeat timers disagree with its static definition",
        )),
    }
}

fn validate_outer_common(snapshot: &DynamicObjectSnapshot) -> Result<(), SnapshotCodecError> {
    if snapshot.flags != 0 || snapshot.related_entity.is_some() {
        return Err(error(
            ERR_OUTER_FIELDS,
            "character skill has noncanonical flags or dynamic relationship",
        ));
    }
    Ok(())
}

fn required_owner(snapshot: &DynamicObjectSnapshot) -> Result<FighterId, SnapshotCodecError> {
    snapshot.owner.ok_or(error(
        ERR_RELATIONSHIP,
        "character skill is missing its fighter owner",
    ))
}

fn validated_hit_mask(
    snapshot: &DynamicObjectSnapshot,
    owner: FighterId,
) -> Result<FighterHitMask, SnapshotCodecError> {
    let hit_mask = FighterHitMask::from_bits(snapshot.fighter_hit_mask).ok_or(error(
        ERR_RELATIONSHIP,
        "character skill hit mask uses reserved fighter bits",
    ))?;
    if hit_mask.contains(owner) {
        return Err(error(
            ERR_RELATIONSHIP,
            "character skill hit memory contains its owner",
        ));
    }
    Ok(hit_mask)
}

fn validate_payload_envelope(snapshot: &DynamicObjectSnapshot) -> Result<(), SnapshotCodecError> {
    if snapshot.payload[VERSION_OFFSET] != PAYLOAD_VERSION {
        return Err(error(
            ERR_PAYLOAD_VERSION,
            "unsupported character skill payload version",
        ));
    }
    if snapshot.payload[RESERVED_OFFSET] != 0
        || snapshot.payload[CHARACTER_SKILL_PAYLOAD_USED_BYTES..]
            .iter()
            .any(|byte| *byte != 0)
    {
        return Err(error(
            ERR_PADDING,
            "character skill reserved or padding bytes are nonzero",
        ));
    }
    Ok(())
}

fn bee_definition(kind: BeeSkillKind) -> BeeDefinition {
    match kind {
        BeeSkillKind::WorkerBee => BeeDefinition {
            payload_id: AttackPayloadId::BeeWorkerSting,
            shape_id: AttackShapeId::ProjectileBolt,
            source: ImpactSource::Projectile,
            maximum_lifetime_ticks: seconds_to_ticks_ceil(0.78),
            base_radius: 0.3,
            guard_stamina_damage: 8.0,
            repeat_interval_ticks: None,
            allows_target: true,
        },
        BeeSkillKind::HoneyGlob => BeeDefinition {
            payload_id: AttackPayloadId::BeeHoneyGlob,
            shape_id: AttackShapeId::ProjectileBolt,
            source: ImpactSource::Projectile,
            maximum_lifetime_ticks: seconds_to_ticks_ceil(1.15),
            base_radius: 0.42,
            guard_stamina_damage: 12.0,
            repeat_interval_ticks: None,
            allows_target: false,
        },
        BeeSkillKind::HoneyPuddle => BeeDefinition {
            payload_id: AttackPayloadId::BeeHoneyPuddle,
            shape_id: AttackShapeId::HazardField,
            source: ImpactSource::Hazard,
            maximum_lifetime_ticks: seconds_to_ticks_ceil(2.4),
            base_radius: 0.68,
            guard_stamina_damage: 6.0,
            repeat_interval_ticks: Some(seconds_to_ticks_ceil(0.45)),
            allows_target: false,
        },
        BeeSkillKind::HomingSting => BeeDefinition {
            payload_id: AttackPayloadId::BeeHomingSting,
            shape_id: AttackShapeId::ProjectileBolt,
            source: ImpactSource::Projectile,
            maximum_lifetime_ticks: seconds_to_ticks_ceil(1.05),
            base_radius: 0.34,
            guard_stamina_damage: 14.0,
            repeat_interval_ticks: None,
            allows_target: true,
        },
        BeeSkillKind::UltimateSwarm => BeeDefinition {
            payload_id: AttackPayloadId::BeeUltimateSwarmTick,
            shape_id: AttackShapeId::HazardField,
            source: ImpactSource::Hazard,
            maximum_lifetime_ticks: seconds_to_ticks_ceil(2.4),
            base_radius: 2.0,
            guard_stamina_damage: 5.0,
            repeat_interval_ticks: Some(seconds_to_ticks_ceil(0.3)),
            allows_target: false,
        },
    }
}

fn chick_definition(kind: ChickSkillKind) -> ChickDefinition {
    match kind {
        ChickSkillKind::ShellChip => {
            chick_projectile_definition(AttackPayloadId::ChickShellChip, 0.62, 0.25, 5.0)
        }
        ChickSkillKind::FriedEggDisc => {
            chick_projectile_definition(AttackPayloadId::ChickFriedEggDisc, 0.74, 0.34, 8.0)
        }
        ChickSkillKind::EggCupMortar => {
            chick_projectile_definition(AttackPayloadId::ChickEggCupMortar, 1.12, 0.42, 11.0)
        }
        ChickSkillKind::OrbitEgg => ChickDefinition {
            payload_id: Some(AttackPayloadId::ChickOrbitEgg),
            shape_id: AttackShapeId::ProjectileBolt,
            source: ImpactSource::Projectile,
            maximum_lifetime_ticks: seconds_to_ticks_ceil(8.0),
            base_radius: 0.36,
            guard_stamina_damage: 2.0,
            repeat_interval_ticks: None,
            uses_hit_memory: false,
        },
        ChickSkillKind::OrbitEggLaunch => ChickDefinition {
            payload_id: Some(AttackPayloadId::ChickOrbitEgg),
            shape_id: AttackShapeId::ProjectileBolt,
            source: ImpactSource::Projectile,
            // Ultimate eggs reuse this kind and deliberately extend it to 4 s.
            maximum_lifetime_ticks: seconds_to_ticks_ceil(4.0),
            base_radius: 0.78,
            guard_stamina_damage: 2.0,
            repeat_interval_ticks: None,
            uses_hit_memory: true,
        },
        ChickSkillKind::OrbitEggReturn => {
            chick_projectile_definition(AttackPayloadId::ChickOrbitEggLaunch, 1.2, 0.78, 10.0)
        }
        ChickSkillKind::FreshEggDrop => {
            chick_projectile_definition(AttackPayloadId::ChickFreshEggDrop, 1.0, 1.14, 7.0)
        }
        ChickSkillKind::FreshEggRide => ChickDefinition {
            payload_id: None,
            shape_id: AttackShapeId::ProjectileBolt,
            source: ImpactSource::Projectile,
            maximum_lifetime_ticks: seconds_to_ticks_ceil(0.56),
            base_radius: 0.0,
            guard_stamina_damage: 0.0,
            repeat_interval_ticks: None,
            uses_hit_memory: false,
        },
        ChickSkillKind::EggplantRoll => {
            chick_projectile_definition(AttackPayloadId::ChickEggplantRoll, 1.22, 0.46, 12.0)
        }
        ChickSkillKind::SunnySplash => {
            chick_hazard_definition(AttackPayloadId::ChickSunnySplash, 1.15, 0.84, 5.0, 0.36)
        }
        ChickSkillKind::OmeletField => {
            chick_hazard_definition(AttackPayloadId::ChickOmeletField, 2.05, 1.55, 6.0, 0.42)
        }
    }
}

fn chick_projectile_definition(
    payload_id: AttackPayloadId,
    lifetime_seconds: f32,
    radius: f32,
    guard_stamina_damage: f32,
) -> ChickDefinition {
    ChickDefinition {
        payload_id: Some(payload_id),
        shape_id: AttackShapeId::ProjectileBolt,
        source: ImpactSource::Projectile,
        maximum_lifetime_ticks: seconds_to_ticks_ceil(lifetime_seconds),
        base_radius: radius,
        guard_stamina_damage,
        repeat_interval_ticks: None,
        uses_hit_memory: true,
    }
}

fn chick_hazard_definition(
    payload_id: AttackPayloadId,
    lifetime_seconds: f32,
    radius: f32,
    guard_stamina_damage: f32,
    repeat_seconds: f32,
) -> ChickDefinition {
    ChickDefinition {
        payload_id: Some(payload_id),
        shape_id: AttackShapeId::HazardField,
        source: ImpactSource::Hazard,
        maximum_lifetime_ticks: seconds_to_ticks_ceil(lifetime_seconds),
        base_radius: radius,
        guard_stamina_damage,
        repeat_interval_ticks: Some(seconds_to_ticks_ceil(repeat_seconds)),
        uses_hit_memory: true,
    }
}

const fn bee_kind_code(kind: BeeSkillKind) -> u8 {
    match kind {
        BeeSkillKind::WorkerBee => 0,
        BeeSkillKind::HoneyGlob => 1,
        BeeSkillKind::HoneyPuddle => 2,
        BeeSkillKind::HomingSting => 3,
        BeeSkillKind::UltimateSwarm => 4,
    }
}

const fn bee_kind_from_code(code: u8) -> Option<BeeSkillKind> {
    match code {
        0 => Some(BeeSkillKind::WorkerBee),
        1 => Some(BeeSkillKind::HoneyGlob),
        2 => Some(BeeSkillKind::HoneyPuddle),
        3 => Some(BeeSkillKind::HomingSting),
        4 => Some(BeeSkillKind::UltimateSwarm),
        _ => None,
    }
}

const fn chick_kind_code(kind: ChickSkillKind) -> u8 {
    match kind {
        ChickSkillKind::ShellChip => 0,
        ChickSkillKind::FriedEggDisc => 1,
        ChickSkillKind::EggCupMortar => 2,
        ChickSkillKind::OrbitEgg => 3,
        ChickSkillKind::OrbitEggLaunch => 4,
        ChickSkillKind::OrbitEggReturn => 5,
        ChickSkillKind::FreshEggDrop => 6,
        ChickSkillKind::FreshEggRide => 7,
        ChickSkillKind::EggplantRoll => 8,
        ChickSkillKind::SunnySplash => 9,
        ChickSkillKind::OmeletField => 10,
    }
}

const fn chick_kind_from_code(code: u8) -> Option<ChickSkillKind> {
    match code {
        0 => Some(ChickSkillKind::ShellChip),
        1 => Some(ChickSkillKind::FriedEggDisc),
        2 => Some(ChickSkillKind::EggCupMortar),
        3 => Some(ChickSkillKind::OrbitEgg),
        4 => Some(ChickSkillKind::OrbitEggLaunch),
        5 => Some(ChickSkillKind::OrbitEggReturn),
        6 => Some(ChickSkillKind::FreshEggDrop),
        7 => Some(ChickSkillKind::FreshEggRide),
        8 => Some(ChickSkillKind::EggplantRoll),
        9 => Some(ChickSkillKind::SunnySplash),
        10 => Some(ChickSkillKind::OmeletField),
        _ => None,
    }
}

const fn style_code(style: FighterStyleKind) -> u8 {
    match style {
        FighterStyleKind::Anchor => 0,
        FighterStyleKind::Vector => 1,
        FighterStyleKind::Catalyst => 2,
    }
}

const fn style_from_code(code: u8) -> Option<FighterStyleKind> {
    match code {
        0 => Some(FighterStyleKind::Anchor),
        1 => Some(FighterStyleKind::Vector),
        2 => Some(FighterStyleKind::Catalyst),
        _ => None,
    }
}

const fn attack_payload_code(payload: Option<AttackPayloadId>) -> Option<u8> {
    match payload {
        None => Some(0),
        Some(AttackPayloadId::BeeWorkerSting) => Some(1),
        Some(AttackPayloadId::BeeHoneyGlob) => Some(2),
        Some(AttackPayloadId::BeeHoneyPuddle) => Some(3),
        Some(AttackPayloadId::BeeHomingSting) => Some(4),
        Some(AttackPayloadId::BeeUltimateSwarmTick) => Some(5),
        Some(AttackPayloadId::ChickShellChip) => Some(6),
        Some(AttackPayloadId::ChickFriedEggDisc) => Some(7),
        Some(AttackPayloadId::ChickEggCupMortar) => Some(8),
        Some(AttackPayloadId::ChickOrbitEgg) => Some(9),
        Some(AttackPayloadId::ChickOrbitEggLaunch) => Some(10),
        Some(AttackPayloadId::ChickFreshEggDrop) => Some(11),
        Some(AttackPayloadId::ChickEggplantRoll) => Some(12),
        Some(AttackPayloadId::ChickSunnySplash) => Some(13),
        Some(AttackPayloadId::ChickOmeletField) => Some(14),
        Some(_) => None,
    }
}

const fn attack_payload_from_code(code: u8) -> Option<Option<AttackPayloadId>> {
    match code {
        0 => Some(None),
        1 => Some(Some(AttackPayloadId::BeeWorkerSting)),
        2 => Some(Some(AttackPayloadId::BeeHoneyGlob)),
        3 => Some(Some(AttackPayloadId::BeeHoneyPuddle)),
        4 => Some(Some(AttackPayloadId::BeeHomingSting)),
        5 => Some(Some(AttackPayloadId::BeeUltimateSwarmTick)),
        6 => Some(Some(AttackPayloadId::ChickShellChip)),
        7 => Some(Some(AttackPayloadId::ChickFriedEggDisc)),
        8 => Some(Some(AttackPayloadId::ChickEggCupMortar)),
        9 => Some(Some(AttackPayloadId::ChickOrbitEgg)),
        10 => Some(Some(AttackPayloadId::ChickOrbitEggLaunch)),
        11 => Some(Some(AttackPayloadId::ChickFreshEggDrop)),
        12 => Some(Some(AttackPayloadId::ChickEggplantRoll)),
        13 => Some(Some(AttackPayloadId::ChickSunnySplash)),
        14 => Some(Some(AttackPayloadId::ChickOmeletField)),
        _ => None,
    }
}

const fn shape_code(shape: AttackShapeId) -> Option<u8> {
    match shape {
        AttackShapeId::ProjectileBolt => Some(0),
        AttackShapeId::HazardField => Some(1),
        _ => None,
    }
}

const fn shape_from_code(code: u8) -> Option<AttackShapeId> {
    match code {
        0 => Some(AttackShapeId::ProjectileBolt),
        1 => Some(AttackShapeId::HazardField),
        _ => None,
    }
}

const fn source_code(source: ImpactSource) -> Option<u8> {
    match source {
        ImpactSource::Projectile => Some(0),
        ImpactSource::Hazard => Some(1),
        _ => None,
    }
}

const fn source_from_code(code: u8) -> Option<ImpactSource> {
    match code {
        0 => Some(ImpactSource::Projectile),
        1 => Some(ImpactSource::Hazard),
        _ => None,
    }
}

fn required<T: Component>(world: &World, entity: Entity) -> Result<&T, SnapshotCodecError> {
    world.get::<T>(entity).ok_or(error(
        ERR_MISSING_COMPONENT,
        "character skill entity is missing a required authoritative component",
    ))
}

fn require_stable_identity(
    world: &World,
    entity: Entity,
    id: SimEntityId,
) -> Result<(), SnapshotCodecError> {
    if required::<StableSimEntity>(world, entity)?.id() == id {
        Ok(())
    } else {
        Err(error(
            ERR_IDENTITY_MISMATCH,
            "character skill StableSimEntity disagrees with allocator ID",
        ))
    }
}

fn require_kind(id: SimEntityId, expected: SimEntityKind) -> Result<(), SnapshotCodecError> {
    if id.kind() == expected {
        Ok(())
    } else {
        Err(error(
            ERR_WRONG_KIND,
            "character skill codec received the wrong stable pool kind",
        ))
    }
}

fn finite_active_timer(ticks: u32) -> Result<TickTimer, SnapshotCodecError> {
    if ticks == 0 || ticks == u32::MAX {
        Err(error(
            ERR_TIMER,
            "character skill timer must be finite and active",
        ))
    } else {
        Ok(TickTimer::from_ticks(ticks))
    }
}

fn write_u32(payload: &mut [u8; DYNAMIC_PAYLOAD_BYTES], offset: usize, value: u32) {
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
            .expect("fixed character skill payload offsets are compile-time bounded"),
    )
}

fn read_canonical_f32(
    payload: &[u8; DYNAMIC_PAYLOAD_BYTES],
    offset: usize,
) -> Result<f32, SnapshotCodecError> {
    let value = f32::from_bits(read_u32(payload, offset));
    if value.is_finite()
        && canonicalize_f32(value, DEFAULT_F32_QUANTIZATION).to_bits() == value.to_bits()
    {
        Ok(value)
    } else {
        Err(error(
            ERR_NON_CANONICAL_FLOAT,
            "character skill float is non-finite or off the canonical grid",
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

    const BEE_KINDS: [BeeSkillKind; 5] = [
        BeeSkillKind::WorkerBee,
        BeeSkillKind::HoneyGlob,
        BeeSkillKind::HoneyPuddle,
        BeeSkillKind::HomingSting,
        BeeSkillKind::UltimateSwarm,
    ];
    const CHICK_KINDS: [ChickSkillKind; 11] = [
        ChickSkillKind::ShellChip,
        ChickSkillKind::FriedEggDisc,
        ChickSkillKind::EggCupMortar,
        ChickSkillKind::OrbitEgg,
        ChickSkillKind::OrbitEggLaunch,
        ChickSkillKind::OrbitEggReturn,
        ChickSkillKind::FreshEggDrop,
        ChickSkillKind::FreshEggRide,
        ChickSkillKind::EggplantRoll,
        ChickSkillKind::SunnySplash,
        ChickSkillKind::OmeletField,
    ];

    fn fighter(index: usize) -> FighterId {
        FighterId::from_index(index).unwrap()
    }

    fn q(value: f32) -> f32 {
        canonicalize_f32(value, DEFAULT_F32_QUANTIZATION)
    }

    fn qv(x: f32, y: f32, z: f32) -> Vec3 {
        Vec3::new(q(x), q(y), q(z))
    }

    fn id(kind: SimEntityKind, index: u32) -> SimEntityId {
        SimEntityId::new(kind, index, 1)
    }

    fn sample_bee(kind: BeeSkillKind, target: Option<FighterId>) -> ActiveBeeSkill {
        let definition = bee_definition(kind);
        let interval = definition.repeat_interval_ticks.map(TickTimer::from_ticks);
        ActiveBeeSkill {
            kind,
            owner: fighter(0),
            owner_style: FighterStyleKind::Vector,
            payload_id: definition.payload_id,
            shape_id: definition.shape_id,
            source: definition.source,
            facing: qv(0.75, 0.0, 0.5),
            velocity: qv(4.25, -0.5, 1.75),
            target,
            lifetime: TickTimer::from_ticks(definition.maximum_lifetime_ticks - 7),
            age: ElapsedTicks::from_ticks(7),
            radius: q(definition.base_radius * q(1.25)),
            guard_stamina_damage: q(definition.guard_stamina_damage),
            repeat_interval: interval,
            repeat_timer: interval.map(|_| TickTimer::from_ticks(1)),
            already_hit: FighterHitMask::from_bits(1 << fighter(2).get()).unwrap(),
            size_scale: q(1.25),
        }
    }

    fn sample_chick(kind: ChickSkillKind) -> ActiveChickSkill {
        let definition = chick_definition(kind);
        let interval = definition.repeat_interval_ticks.map(TickTimer::from_ticks);
        let already_hit = if definition.uses_hit_memory {
            FighterHitMask::from_bits(1 << fighter(2).get()).unwrap()
        } else {
            FighterHitMask::default()
        };
        ActiveChickSkill {
            kind,
            owner: fighter(0),
            owner_style: FighterStyleKind::Catalyst,
            payload_id: definition.payload_id,
            shape_id: definition.shape_id,
            source: definition.source,
            facing: qv(-0.5, 0.0, 0.75),
            velocity: qv(2.0, 3.25, -0.5),
            lifetime: TickTimer::from_ticks(definition.maximum_lifetime_ticks - 5),
            age: ElapsedTicks::from_ticks(5),
            radius: q(definition.base_radius * q(1.5)),
            guard_stamina_damage: q(definition.guard_stamina_damage),
            repeat_interval: interval,
            repeat_timer: interval.map(|_| TickTimer::from_ticks(2)),
            already_hit,
            size_scale: q(1.5),
        }
    }

    fn assert_bee_eq(actual: &ActiveBeeSkill, expected: &ActiveBeeSkill) {
        assert_eq!(actual.kind, expected.kind);
        assert_eq!(actual.owner, expected.owner);
        assert_eq!(actual.owner_style, expected.owner_style);
        assert_eq!(actual.payload_id, expected.payload_id);
        assert_eq!(actual.shape_id, expected.shape_id);
        assert_eq!(actual.source, expected.source);
        assert_eq!(actual.facing, expected.facing);
        assert_eq!(actual.velocity, expected.velocity);
        assert_eq!(actual.target, expected.target);
        assert_eq!(actual.lifetime, expected.lifetime);
        assert_eq!(actual.age, expected.age);
        assert_eq!(actual.radius, expected.radius);
        assert_eq!(actual.guard_stamina_damage, expected.guard_stamina_damage);
        assert_eq!(actual.repeat_interval, expected.repeat_interval);
        assert_eq!(actual.repeat_timer, expected.repeat_timer);
        assert_eq!(actual.already_hit, expected.already_hit);
        assert_eq!(actual.size_scale, expected.size_scale);
    }

    fn assert_chick_eq(actual: &ActiveChickSkill, expected: &ActiveChickSkill) {
        assert_eq!(actual.kind, expected.kind);
        assert_eq!(actual.owner, expected.owner);
        assert_eq!(actual.owner_style, expected.owner_style);
        assert_eq!(actual.payload_id, expected.payload_id);
        assert_eq!(actual.shape_id, expected.shape_id);
        assert_eq!(actual.source, expected.source);
        assert_eq!(actual.facing, expected.facing);
        assert_eq!(actual.velocity, expected.velocity);
        assert_eq!(actual.lifetime, expected.lifetime);
        assert_eq!(actual.age, expected.age);
        assert_eq!(actual.radius, expected.radius);
        assert_eq!(actual.guard_stamina_damage, expected.guard_stamina_damage);
        assert_eq!(actual.repeat_interval, expected.repeat_interval);
        assert_eq!(actual.repeat_timer, expected.repeat_timer);
        assert_eq!(actual.already_hit, expected.already_hit);
        assert_eq!(actual.size_scale, expected.size_scale);
    }

    #[test]
    fn every_mapping_table_round_trips_exhaustively() {
        for kind in BEE_KINDS {
            assert_eq!(bee_kind_from_code(bee_kind_code(kind)), Some(kind));
        }
        for kind in CHICK_KINDS {
            assert_eq!(chick_kind_from_code(chick_kind_code(kind)), Some(kind));
        }
        for style in [
            FighterStyleKind::Anchor,
            FighterStyleKind::Vector,
            FighterStyleKind::Catalyst,
        ] {
            assert_eq!(style_from_code(style_code(style)), Some(style));
        }
        let payloads = [
            None,
            Some(AttackPayloadId::BeeWorkerSting),
            Some(AttackPayloadId::BeeHoneyGlob),
            Some(AttackPayloadId::BeeHoneyPuddle),
            Some(AttackPayloadId::BeeHomingSting),
            Some(AttackPayloadId::BeeUltimateSwarmTick),
            Some(AttackPayloadId::ChickShellChip),
            Some(AttackPayloadId::ChickFriedEggDisc),
            Some(AttackPayloadId::ChickEggCupMortar),
            Some(AttackPayloadId::ChickOrbitEgg),
            Some(AttackPayloadId::ChickOrbitEggLaunch),
            Some(AttackPayloadId::ChickFreshEggDrop),
            Some(AttackPayloadId::ChickEggplantRoll),
            Some(AttackPayloadId::ChickSunnySplash),
            Some(AttackPayloadId::ChickOmeletField),
        ];
        for payload in payloads {
            let code = attack_payload_code(payload).unwrap();
            assert_eq!(attack_payload_from_code(code), Some(payload));
        }
        for shape in [AttackShapeId::ProjectileBolt, AttackShapeId::HazardField] {
            assert_eq!(shape_from_code(shape_code(shape).unwrap()), Some(shape));
        }
        for source in [ImpactSource::Projectile, ImpactSource::Hazard] {
            assert_eq!(source_from_code(source_code(source).unwrap()), Some(source));
        }
        assert_eq!(bee_kind_from_code(5), None);
        assert_eq!(chick_kind_from_code(11), None);
        assert_eq!(attack_payload_from_code(15), None);
        assert_eq!(style_from_code(3), None);
    }

    #[test]
    fn every_bee_kind_and_target_option_round_trips_losslessly() {
        let codec = LiveBeeSkillSnapshotCodec;
        let translation = qv(3.25, 1.5, -2.75);
        let mut next_index = 0;
        for kind in BEE_KINDS {
            let targets: &[Option<FighterId>] = if bee_definition(kind).allows_target {
                &[None, Some(fighter(1))]
            } else {
                &[None]
            };
            for target in targets {
                let stable_id = id(SimEntityKind::BeeSkill, next_index);
                next_index += 1;
                let expected = sample_bee(kind, *target);
                let mut source_world = World::new();
                let source_entity = source_world
                    .spawn((
                        StableSimEntity::new(stable_id),
                        SimPosition::new(translation),
                        expected,
                    ))
                    .id();
                let snapshot = codec
                    .capture(&source_world, source_entity, stable_id)
                    .unwrap();
                assert_eq!(
                    snapshot.payload[CHARACTER_SKILL_PAYLOAD_USED_BYTES..],
                    [0; 56]
                );

                let mut restored_world = World::new();
                let restored_entity = restored_world.spawn_empty().id();
                codec.validate_restore(&restored_world, &snapshot).unwrap();
                codec.restore_validated(&mut restored_world, restored_entity, &snapshot);
                let actual = restored_world
                    .get::<ActiveBeeSkill>(restored_entity)
                    .unwrap();
                let expected = source_world.get::<ActiveBeeSkill>(source_entity).unwrap();
                assert_bee_eq(actual, expected);
                assert_eq!(
                    restored_world
                        .get::<SimPosition>(restored_entity)
                        .unwrap()
                        .translation,
                    translation
                );
                assert!(restored_world.get::<Transform>(restored_entity).is_none());
            }
        }
    }

    #[test]
    fn every_chick_kind_including_repeat_options_round_trips_losslessly() {
        let codec = LiveChickSkillSnapshotCodec;
        let translation = qv(-4.5, 2.25, 8.0);
        for (index, kind) in CHICK_KINDS.into_iter().enumerate() {
            let stable_id = id(SimEntityKind::ChickSkill, index as u32);
            let expected = sample_chick(kind);
            let mut source_world = World::new();
            let source_entity = source_world
                .spawn((
                    StableSimEntity::new(stable_id),
                    SimPosition::new(translation),
                    expected,
                ))
                .id();
            let snapshot = codec
                .capture(&source_world, source_entity, stable_id)
                .unwrap();
            let definition = chick_definition(kind);
            assert_eq!(
                snapshot.payload[REPEAT_PRESENT_OFFSET],
                u8::from(definition.repeat_interval_ticks.is_some())
            );

            let mut restored_world = World::new();
            let restored_entity = restored_world.spawn_empty().id();
            codec.validate_restore(&restored_world, &snapshot).unwrap();
            codec.restore_validated(&mut restored_world, restored_entity, &snapshot);
            let actual = restored_world
                .get::<ActiveChickSkill>(restored_entity)
                .unwrap();
            let expected = source_world.get::<ActiveChickSkill>(source_entity).unwrap();
            assert_chick_eq(actual, expected);
            assert_eq!(
                restored_world
                    .get::<SimPosition>(restored_entity)
                    .unwrap()
                    .translation,
                translation
            );
            assert!(restored_world.get::<Transform>(restored_entity).is_none());
        }
    }

    #[test]
    fn hostile_payloads_relationships_and_static_mismatches_are_rejected() {
        let codec = LiveBeeSkillSnapshotCodec;
        let stable_id = id(SimEntityKind::BeeSkill, 0);
        let mut world = World::new();
        let entity = world
            .spawn((
                StableSimEntity::new(stable_id),
                SimPosition::new(qv(1.0, 2.0, 3.0)),
                sample_bee(BeeSkillKind::WorkerBee, Some(fighter(1))),
            ))
            .id();
        let valid = codec.capture(&world, entity, stable_id).unwrap();

        let mut cases = Vec::new();
        let mut hostile = valid.clone();
        hostile.payload[CHARACTER_SKILL_PAYLOAD_USED_BYTES] = 1;
        cases.push(hostile);
        let mut hostile = valid.clone();
        hostile.definition_id = u16::from(bee_kind_code(BeeSkillKind::HoneyGlob));
        cases.push(hostile);
        let mut hostile = valid.clone();
        hostile.payload[STYLE_OFFSET] = u8::MAX;
        cases.push(hostile);
        let mut hostile = valid.clone();
        write_f32(&mut hostile.payload, RADIUS_OFFSET, 0.123_456);
        cases.push(hostile);
        let mut hostile = valid.clone();
        hostile.payload[SHAPE_OFFSET] = shape_code(AttackShapeId::HazardField).unwrap();
        cases.push(hostile);
        let mut hostile = valid.clone();
        hostile.target = hostile.owner;
        cases.push(hostile);
        let mut hostile = valid.clone();
        hostile.fighter_hit_mask |= 1 << fighter(0).get();
        cases.push(hostile);
        let mut hostile = valid.clone();
        hostile.payload[REPEAT_PRESENT_OFFSET] = 1;
        write_u32(&mut hostile.payload, REPEAT_INTERVAL_OFFSET, 5);
        write_u32(&mut hostile.payload, REPEAT_TIMER_OFFSET, 6);
        cases.push(hostile);

        for hostile in cases {
            assert!(codec.validate_restore(&world, &hostile).is_err());
        }

        let chick_codec = LiveChickSkillSnapshotCodec;
        let chick_id = id(SimEntityKind::ChickSkill, 0);
        let chick_entity = world
            .spawn((
                StableSimEntity::new(chick_id),
                SimPosition::new(qv(0.0, 1.0, 0.0)),
                sample_chick(ChickSkillKind::FreshEggRide),
            ))
            .id();
        let mut hostile = chick_codec.capture(&world, chick_entity, chick_id).unwrap();
        hostile.target = Some(fighter(1));
        assert!(chick_codec.validate_restore(&world, &hostile).is_err());
        let mut hostile = chick_codec.capture(&world, chick_entity, chick_id).unwrap();
        hostile.fighter_hit_mask = 1 << fighter(2).get();
        assert!(chick_codec.validate_restore(&world, &hostile).is_err());
    }

    #[test]
    fn capture_rejects_missing_mismatched_and_mixed_components() {
        let codec = LiveBeeSkillSnapshotCodec;
        let expected_id = id(SimEntityKind::BeeSkill, 0);
        let mut world = World::new();
        let missing = world.spawn(StableSimEntity::new(expected_id)).id();
        assert!(codec.capture(&world, missing, expected_id).is_err());

        let mismatched = world
            .spawn((
                StableSimEntity::new(id(SimEntityKind::BeeSkill, 1)),
                SimPosition::default(),
                sample_bee(BeeSkillKind::WorkerBee, None),
            ))
            .id();
        assert!(codec.capture(&world, mismatched, expected_id).is_err());

        let mixed_id = id(SimEntityKind::BeeSkill, 2);
        let mixed = world
            .spawn((
                StableSimEntity::new(mixed_id),
                SimPosition::default(),
                sample_bee(BeeSkillKind::WorkerBee, None),
                sample_chick(ChickSkillKind::ShellChip),
            ))
            .id();
        assert!(codec.capture(&world, mixed, mixed_id).is_err());
    }
}
