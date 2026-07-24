//! Production snapshot codecs for penguin skills and persistent surfaces.
//!
//! Both records use explicit catalog codes and fixed-width little-endian
//! payloads. Rust enum discriminants, Bevy entities, asset handles, names, and
//! scene state never cross this boundary. The stable entity ID, fighter owner,
//! optional target, and per-fighter contact mask live in the outer dynamic
//! record.
//!
//! Skill payload v1 (72/128 bytes):
//!
//! - `0..8`: version, kind/style/payload/shape/source codes, option bits, zero
//! - `8..44`: translation, facing, velocity
//! - `44..72`: lifetime, age, radius, guard damage, repeat timers, size scale
//!
//! Surface payload v1 (48/128 bytes):
//!
//! - `0..4`: version, kind code, zero reserved bytes
//! - `4..28`: translation and facing
//! - `28..48`: lifetime, age, radius, next-tick timer, size scale
//!
//! [`SimPosition`] owns authoritative translation for both kinds. Bevy
//! `Transform` rotation and scale are presentation-only and are re-derived by
//! presentation/update systems after restore.

use bevy::prelude::*;

use crate::combat::ImpactSource;
use crate::components::SimPosition;
use crate::determinism::{
    DEFAULT_F32_QUANTIZATION, FighterHitMask, SimEntityId, SimEntityKind, canonicalize_f32,
};
use crate::ecs_identity::StableSimEntity;
use crate::penguin_skills::{
    ActivePenguinSkill, ActivePenguinSurface, PenguinSkillKind, PenguinSurfaceKind,
};
use crate::simulation::{ElapsedTicks, TickTimer};
use crate::snapshot::{DYNAMIC_PAYLOAD_BYTES, DynamicObjectSnapshot};
use crate::snapshot_ecs::{DynamicSnapshotCodec, SnapshotCodecError};
use crate::styles::FighterStyleKind;
use crate::techniques::{AttackPayloadId, AttackShapeId};

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
const ERR_STATIC_RELATIONSHIP: u16 = 10;
const ERR_TIMER: u16 = 11;
const ERR_OPTION_RELATIONSHIP: u16 = 12;
const ERR_SCALAR_RANGE: u16 = 13;
const ERR_CONTACT_MASK: u16 = 14;

const REPEAT_INTERVAL_PRESENT: u8 = 1 << 0;
const REPEAT_TIMER_PRESENT: u8 = 1 << 1;
const VALID_SKILL_OPTIONS: u8 = REPEAT_INTERVAL_PRESENT | REPEAT_TIMER_PRESENT;

const SKILL_VERSION_OFFSET: usize = 0;
const SKILL_KIND_OFFSET: usize = 1;
const SKILL_STYLE_OFFSET: usize = 2;
const SKILL_PAYLOAD_OFFSET: usize = 3;
const SKILL_SHAPE_OFFSET: usize = 4;
const SKILL_SOURCE_OFFSET: usize = 5;
const SKILL_OPTIONS_OFFSET: usize = 6;
const SKILL_RESERVED_OFFSET: usize = 7;
const SKILL_TRANSLATION_OFFSET: usize = 8;
const SKILL_FACING_OFFSET: usize = 20;
const SKILL_VELOCITY_OFFSET: usize = 32;
const SKILL_LIFETIME_OFFSET: usize = 44;
const SKILL_AGE_OFFSET: usize = 48;
const SKILL_RADIUS_OFFSET: usize = 52;
const SKILL_GUARD_DAMAGE_OFFSET: usize = 56;
const SKILL_REPEAT_INTERVAL_OFFSET: usize = 60;
const SKILL_REPEAT_TIMER_OFFSET: usize = 64;
const SKILL_SIZE_SCALE_OFFSET: usize = 68;
const SKILL_USED_BYTES: usize = 72;
const SKILL_PADDING_BYTES: usize = DYNAMIC_PAYLOAD_BYTES - SKILL_USED_BYTES;

const SURFACE_VERSION_OFFSET: usize = 0;
const SURFACE_KIND_OFFSET: usize = 1;
const SURFACE_RESERVED_OFFSET: usize = 2;
const SURFACE_TRANSLATION_OFFSET: usize = 4;
const SURFACE_FACING_OFFSET: usize = 16;
const SURFACE_LIFETIME_OFFSET: usize = 28;
const SURFACE_AGE_OFFSET: usize = 32;
const SURFACE_RADIUS_OFFSET: usize = 36;
const SURFACE_NEXT_TICK_OFFSET: usize = 40;
const SURFACE_SIZE_SCALE_OFFSET: usize = 44;
const SURFACE_USED_BYTES: usize = 48;
const SURFACE_PADDING_BYTES: usize = DYNAMIC_PAYLOAD_BYTES - SURFACE_USED_BYTES;

const _: () = assert!(SKILL_USED_BYTES == 72);
const _: () = assert!(SKILL_PADDING_BYTES == 56);
const _: () = assert!(SURFACE_USED_BYTES == 48);
const _: () = assert!(SURFACE_PADDING_BYTES == 80);
const _: () = assert!(SKILL_USED_BYTES <= DYNAMIC_PAYLOAD_BYTES);
const _: () = assert!(SURFACE_USED_BYTES <= DYNAMIC_PAYLOAD_BYTES);

/// Live codec for [`ActivePenguinSkill`] entities.
#[derive(Clone, Copy, Debug, Default)]
pub struct LivePenguinSkillSnapshotCodec;

/// Live codec for [`ActivePenguinSurface`] entities.
#[derive(Clone, Copy, Debug, Default)]
pub struct LivePenguinSurfaceSnapshotCodec;

struct DecodedSkill {
    translation: Vec3,
    skill: ActivePenguinSkill,
}

struct DecodedSurface {
    translation: Vec3,
    surface: ActivePenguinSurface,
}

#[derive(Clone, Copy)]
struct StaticSkillDefinition {
    payload_id: AttackPayloadId,
    shape_id: AttackShapeId,
    source: ImpactSource,
    lifetime_ticks: u32,
    base_radius: f32,
    guard_stamina_damage: f32,
    repeat_ticks: Option<u32>,
    target_allowed: bool,
}

impl DynamicSnapshotCodec for LivePenguinSkillSnapshotCodec {
    fn capture(
        &self,
        world: &World,
        entity: Entity,
        id: SimEntityId,
    ) -> Result<DynamicObjectSnapshot, SnapshotCodecError> {
        require_kind(id, SimEntityKind::PenguinSkill)?;
        require_stable_identity(world, entity, id, "penguin skill")?;
        let position = required::<SimPosition>(world, entity, "penguin skill")?;
        let skill = required::<ActivePenguinSkill>(world, entity, "penguin skill")?;

        let mut payload = [0; DYNAMIC_PAYLOAD_BYTES];
        payload[SKILL_VERSION_OFFSET] = PAYLOAD_VERSION;
        payload[SKILL_KIND_OFFSET] = skill_kind_code(skill.kind);
        payload[SKILL_STYLE_OFFSET] = style_code(skill.owner_style);
        payload[SKILL_PAYLOAD_OFFSET] = attack_payload_code(skill.payload_id).ok_or(error(
            ERR_ENUM_CODE,
            "penguin skill uses an unsupported attack-payload value",
        ))?;
        payload[SKILL_SHAPE_OFFSET] = attack_shape_code(skill.shape_id).ok_or(error(
            ERR_ENUM_CODE,
            "penguin skill uses an unsupported attack-shape value",
        ))?;
        payload[SKILL_SOURCE_OFFSET] = impact_source_code(skill.source).ok_or(error(
            ERR_ENUM_CODE,
            "penguin skill uses an unsupported impact-source value",
        ))?;

        let mut options = 0;
        write_optional_timer(
            &mut payload,
            SKILL_REPEAT_INTERVAL_OFFSET,
            skill.repeat_interval,
            REPEAT_INTERVAL_PRESENT,
            &mut options,
        );
        write_optional_timer(
            &mut payload,
            SKILL_REPEAT_TIMER_OFFSET,
            skill.repeat_timer,
            REPEAT_TIMER_PRESENT,
            &mut options,
        );
        payload[SKILL_OPTIONS_OFFSET] = options;

        write_vec3(&mut payload, SKILL_TRANSLATION_OFFSET, position.translation);
        write_vec3(&mut payload, SKILL_FACING_OFFSET, skill.facing);
        write_vec3(&mut payload, SKILL_VELOCITY_OFFSET, skill.velocity);
        write_u32(
            &mut payload,
            SKILL_LIFETIME_OFFSET,
            skill.lifetime.remaining(),
        );
        write_u32(&mut payload, SKILL_AGE_OFFSET, skill.age.get());
        write_f32(&mut payload, SKILL_RADIUS_OFFSET, skill.radius);
        write_f32(
            &mut payload,
            SKILL_GUARD_DAMAGE_OFFSET,
            skill.guard_stamina_damage,
        );
        write_f32(&mut payload, SKILL_SIZE_SCALE_OFFSET, skill.size_scale);

        let snapshot = DynamicObjectSnapshot {
            id,
            definition_id: u16::from(skill_kind_code(skill.kind)),
            flags: 0,
            owner: Some(skill.owner),
            target: skill.target,
            related_entity: None,
            fighter_hit_mask: skill.already_hit.bits(),
            payload,
        };
        decode_skill(&snapshot)?;
        Ok(snapshot)
    }

    fn validate_restore(
        &self,
        _world: &World,
        snapshot: &DynamicObjectSnapshot,
    ) -> Result<(), SnapshotCodecError> {
        decode_skill(snapshot).map(|_| ())
    }

    fn restore_validated(
        &self,
        world: &mut World,
        entity: Entity,
        snapshot: &DynamicObjectSnapshot,
    ) {
        let decoded = decode_skill(snapshot)
            .expect("penguin skill payload was fully validated before restore mutation");
        world
            .entity_mut(entity)
            .insert((SimPosition::new(decoded.translation), decoded.skill));
    }
}

impl DynamicSnapshotCodec for LivePenguinSurfaceSnapshotCodec {
    fn capture(
        &self,
        world: &World,
        entity: Entity,
        id: SimEntityId,
    ) -> Result<DynamicObjectSnapshot, SnapshotCodecError> {
        require_kind(id, SimEntityKind::PenguinSurface)?;
        require_stable_identity(world, entity, id, "penguin surface")?;
        let position = required::<SimPosition>(world, entity, "penguin surface")?;
        let surface = required::<ActivePenguinSurface>(world, entity, "penguin surface")?;

        let mut payload = [0; DYNAMIC_PAYLOAD_BYTES];
        payload[SURFACE_VERSION_OFFSET] = PAYLOAD_VERSION;
        payload[SURFACE_KIND_OFFSET] = surface_kind_code(surface.kind);
        write_vec3(
            &mut payload,
            SURFACE_TRANSLATION_OFFSET,
            position.translation,
        );
        write_vec3(&mut payload, SURFACE_FACING_OFFSET, surface.facing);
        write_u32(
            &mut payload,
            SURFACE_LIFETIME_OFFSET,
            surface.lifetime.remaining(),
        );
        write_u32(&mut payload, SURFACE_AGE_OFFSET, surface.age.get());
        write_f32(&mut payload, SURFACE_RADIUS_OFFSET, surface.radius);
        write_u32(
            &mut payload,
            SURFACE_NEXT_TICK_OFFSET,
            surface.next_tick.remaining(),
        );
        write_f32(&mut payload, SURFACE_SIZE_SCALE_OFFSET, surface.size_scale);

        let snapshot = DynamicObjectSnapshot {
            id,
            definition_id: u16::from(surface_kind_code(surface.kind)),
            flags: 0,
            owner: Some(surface.owner),
            target: None,
            related_entity: None,
            fighter_hit_mask: surface.already_touched.bits(),
            payload,
        };
        decode_surface(&snapshot)?;
        Ok(snapshot)
    }

    fn validate_restore(
        &self,
        _world: &World,
        snapshot: &DynamicObjectSnapshot,
    ) -> Result<(), SnapshotCodecError> {
        decode_surface(snapshot).map(|_| ())
    }

    fn restore_validated(
        &self,
        world: &mut World,
        entity: Entity,
        snapshot: &DynamicObjectSnapshot,
    ) {
        let decoded = decode_surface(snapshot)
            .expect("penguin surface payload was fully validated before restore mutation");
        world
            .entity_mut(entity)
            .insert((SimPosition::new(decoded.translation), decoded.surface));
    }
}

fn decode_skill(snapshot: &DynamicObjectSnapshot) -> Result<DecodedSkill, SnapshotCodecError> {
    require_kind(snapshot.id, SimEntityKind::PenguinSkill)?;
    if snapshot.flags != 0 || snapshot.related_entity.is_some() {
        return Err(error(
            ERR_OUTER_FIELDS,
            "penguin skill uses reserved flags or an unsupported entity relationship",
        ));
    }
    let owner = snapshot.owner.ok_or(error(
        ERR_OUTER_FIELDS,
        "penguin skill is missing its fighter owner",
    ))?;
    let already_hit = FighterHitMask::from_bits(snapshot.fighter_hit_mask).ok_or(error(
        ERR_OUTER_FIELDS,
        "penguin skill hit mask uses reserved fighter bits",
    ))?;

    if snapshot.payload[SKILL_VERSION_OFFSET] != PAYLOAD_VERSION {
        return Err(error(
            ERR_PAYLOAD_VERSION,
            "unsupported penguin skill payload version",
        ));
    }
    if snapshot.payload[SKILL_RESERVED_OFFSET] != 0
        || snapshot.payload[SKILL_USED_BYTES..]
            .iter()
            .any(|byte| *byte != 0)
    {
        return Err(error(
            ERR_PADDING,
            "penguin skill payload reserved or padding bytes are nonzero",
        ));
    }
    let options = snapshot.payload[SKILL_OPTIONS_OFFSET];
    if options & !VALID_SKILL_OPTIONS != 0 {
        return Err(error(
            ERR_OPTION_RELATIONSHIP,
            "penguin skill payload uses reserved option bits",
        ));
    }

    let kind = skill_kind_from_code(snapshot.payload[SKILL_KIND_OFFSET]).ok_or(error(
        ERR_ENUM_CODE,
        "penguin skill payload contains an unknown kind code",
    ))?;
    if snapshot.definition_id != u16::from(skill_kind_code(kind)) {
        return Err(error(
            ERR_DEFINITION,
            "penguin skill definition ID disagrees with its kind code",
        ));
    }
    let owner_style = style_from_code(snapshot.payload[SKILL_STYLE_OFFSET]).ok_or(error(
        ERR_ENUM_CODE,
        "penguin skill payload contains an unknown style code",
    ))?;
    let payload_id =
        attack_payload_from_code(snapshot.payload[SKILL_PAYLOAD_OFFSET]).ok_or(error(
            ERR_ENUM_CODE,
            "penguin skill payload contains an unknown attack-payload code",
        ))?;
    let shape_id = attack_shape_from_code(snapshot.payload[SKILL_SHAPE_OFFSET]).ok_or(error(
        ERR_ENUM_CODE,
        "penguin skill payload contains an unknown attack-shape code",
    ))?;
    let source = impact_source_from_code(snapshot.payload[SKILL_SOURCE_OFFSET]).ok_or(error(
        ERR_ENUM_CODE,
        "penguin skill payload contains an unknown impact-source code",
    ))?;

    let definition = static_skill_definition(kind);
    if payload_id != definition.payload_id
        || shape_id != definition.shape_id
        || source != definition.source
    {
        return Err(error(
            ERR_STATIC_RELATIONSHIP,
            "penguin skill kind disagrees with its payload, shape, or impact source",
        ));
    }
    if snapshot.target == Some(owner) || (snapshot.target.is_some() && !definition.target_allowed) {
        return Err(error(
            ERR_STATIC_RELATIONSHIP,
            "only a fish torpedo may retain a non-owner fighter target",
        ));
    }

    let translation = read_canonical_vec3(&snapshot.payload, SKILL_TRANSLATION_OFFSET)?;
    let facing = read_canonical_vec3(&snapshot.payload, SKILL_FACING_OFFSET)?;
    let velocity = read_canonical_vec3(&snapshot.payload, SKILL_VELOCITY_OFFSET)?;
    let lifetime_ticks = read_u32(&snapshot.payload, SKILL_LIFETIME_OFFSET);
    let age_ticks = read_u32(&snapshot.payload, SKILL_AGE_OFFSET);
    let radius = read_canonical_f32(&snapshot.payload, SKILL_RADIUS_OFFSET)?;
    let guard_stamina_damage = read_canonical_f32(&snapshot.payload, SKILL_GUARD_DAMAGE_OFFSET)?;
    let repeat_interval = read_optional_timer(
        &snapshot.payload,
        SKILL_REPEAT_INTERVAL_OFFSET,
        options,
        REPEAT_INTERVAL_PRESENT,
    )?;
    let repeat_timer = read_optional_timer(
        &snapshot.payload,
        SKILL_REPEAT_TIMER_OFFSET,
        options,
        REPEAT_TIMER_PRESENT,
    )?;
    let size_scale = read_canonical_f32(&snapshot.payload, SKILL_SIZE_SCALE_OFFSET)?;

    validate_live_timer(lifetime_ticks, age_ticks, definition.lifetime_ticks)?;
    validate_skill_options(definition.repeat_ticks, repeat_interval, repeat_timer)?;
    if !valid_facing(facing)
        || size_scale < canonical(0.1)
        || radius <= 0.0
        || !canonical_scaled_eq(radius, definition.base_radius, size_scale)
        || guard_stamina_damage.to_bits() != canonical(definition.guard_stamina_damage).to_bits()
    {
        return Err(error(
            ERR_SCALAR_RANGE,
            "penguin skill facing, radius, guard damage, or size violates its definition",
        ));
    }
    if kind == PenguinSkillKind::BodySlamShockwave && velocity != Vec3::ZERO {
        return Err(error(
            ERR_STATIC_RELATIONSHIP,
            "body-slam shockwave must retain zero authoritative velocity",
        ));
    }

    Ok(DecodedSkill {
        translation,
        skill: ActivePenguinSkill {
            kind,
            owner,
            owner_style,
            payload_id,
            shape_id,
            source,
            facing,
            velocity,
            target: snapshot.target,
            lifetime: TickTimer::from_ticks(lifetime_ticks),
            age: ElapsedTicks::from_ticks(age_ticks),
            radius,
            guard_stamina_damage,
            repeat_interval,
            repeat_timer,
            already_hit,
            size_scale,
        },
    })
}

fn decode_surface(snapshot: &DynamicObjectSnapshot) -> Result<DecodedSurface, SnapshotCodecError> {
    require_kind(snapshot.id, SimEntityKind::PenguinSurface)?;
    if snapshot.flags != 0 || snapshot.target.is_some() || snapshot.related_entity.is_some() {
        return Err(error(
            ERR_OUTER_FIELDS,
            "penguin surface uses reserved flags or unsupported relationships",
        ));
    }
    let owner = snapshot.owner.ok_or(error(
        ERR_OUTER_FIELDS,
        "penguin surface is missing its fighter owner",
    ))?;
    let already_touched = FighterHitMask::from_bits(snapshot.fighter_hit_mask).ok_or(error(
        ERR_OUTER_FIELDS,
        "penguin surface contact mask uses reserved fighter bits",
    ))?;

    if snapshot.payload[SURFACE_VERSION_OFFSET] != PAYLOAD_VERSION {
        return Err(error(
            ERR_PAYLOAD_VERSION,
            "unsupported penguin surface payload version",
        ));
    }
    if snapshot.payload[SURFACE_RESERVED_OFFSET..SURFACE_TRANSLATION_OFFSET]
        .iter()
        .chain(snapshot.payload[SURFACE_USED_BYTES..].iter())
        .any(|byte| *byte != 0)
    {
        return Err(error(
            ERR_PADDING,
            "penguin surface payload reserved or padding bytes are nonzero",
        ));
    }

    let kind = surface_kind_from_code(snapshot.payload[SURFACE_KIND_OFFSET]).ok_or(error(
        ERR_ENUM_CODE,
        "penguin surface payload contains an unknown kind code",
    ))?;
    if snapshot.definition_id != u16::from(surface_kind_code(kind)) {
        return Err(error(
            ERR_DEFINITION,
            "penguin surface definition ID disagrees with its kind code",
        ));
    }

    let translation = read_canonical_vec3(&snapshot.payload, SURFACE_TRANSLATION_OFFSET)?;
    let facing = read_canonical_vec3(&snapshot.payload, SURFACE_FACING_OFFSET)?;
    let lifetime_ticks = read_u32(&snapshot.payload, SURFACE_LIFETIME_OFFSET);
    let age_ticks = read_u32(&snapshot.payload, SURFACE_AGE_OFFSET);
    let radius = read_canonical_f32(&snapshot.payload, SURFACE_RADIUS_OFFSET)?;
    let next_tick = read_u32(&snapshot.payload, SURFACE_NEXT_TICK_OFFSET);
    let size_scale = read_canonical_f32(&snapshot.payload, SURFACE_SIZE_SCALE_OFFSET)?;
    let total_ticks = age_ticks.checked_add(lifetime_ticks).ok_or(error(
        ERR_TIMER,
        "penguin surface lifetime and age overflow",
    ))?;

    if lifetime_ticks == 0
        || lifetime_ticks == u32::MAX
        || size_scale < canonical(0.1)
        || !valid_facing(facing)
        || !surface_definition_matches(kind, total_ticks, radius, size_scale)
    {
        return Err(error(
            ERR_STATIC_RELATIONSHIP,
            "penguin surface timers, facing, radius, or size violate its definition",
        ));
    }

    let glacier_repeat_ticks = TickTimer::from_seconds_ceil(0.34).remaining();
    if kind == PenguinSurfaceKind::GlacierTrailPrinter {
        if next_tick > glacier_repeat_ticks || next_tick == u32::MAX {
            return Err(error(
                ERR_TIMER,
                "glacier trail printer next-tick timer exceeds its authored interval",
            ));
        }
    } else if next_tick != 0 {
        return Err(error(
            ERR_OPTION_RELATIONSHIP,
            "non-printer penguin surface contains a next-tick timer",
        ));
    }

    let contact_kind = matches!(
        kind,
        PenguinSurfaceKind::SnowHillRamp
            | PenguinSurfaceKind::SnowSlopeRide
            | PenguinSurfaceKind::SpringPad
    );
    if !contact_kind && !already_touched.is_empty() {
        return Err(error(
            ERR_CONTACT_MASK,
            "non-contact penguin surface contains fighter contact history",
        ));
    }

    Ok(DecodedSurface {
        translation,
        surface: ActivePenguinSurface {
            kind,
            owner,
            facing,
            lifetime: TickTimer::from_ticks(lifetime_ticks),
            age: ElapsedTicks::from_ticks(age_ticks),
            radius,
            next_tick: TickTimer::from_ticks(next_tick),
            already_touched,
            size_scale,
        },
    })
}

fn validate_live_timer(
    lifetime_ticks: u32,
    age_ticks: u32,
    total_ticks: u32,
) -> Result<(), SnapshotCodecError> {
    if lifetime_ticks == 0
        || lifetime_ticks == u32::MAX
        || age_ticks.checked_add(lifetime_ticks) != Some(total_ticks)
    {
        return Err(error(
            ERR_TIMER,
            "penguin skill lifetime and age disagree with its authored duration",
        ));
    }
    Ok(())
}

fn validate_skill_options(
    expected_interval: Option<u32>,
    repeat_interval: Option<TickTimer>,
    repeat_timer: Option<TickTimer>,
) -> Result<(), SnapshotCodecError> {
    match (expected_interval, repeat_interval, repeat_timer) {
        (None, None, None) => Ok(()),
        (Some(expected), Some(interval), Some(timer))
            if interval.remaining() == expected
                && timer.remaining() >= 1
                && timer.remaining() <= expected =>
        {
            Ok(())
        }
        _ => Err(error(
            ERR_OPTION_RELATIONSHIP,
            "penguin skill repeat options disagree with its kind or timer interval",
        )),
    }
}

fn surface_definition_matches(
    kind: PenguinSurfaceKind,
    total_ticks: u32,
    radius: f32,
    size_scale: f32,
) -> bool {
    let profile = |seconds: f32, base_radius: f32| {
        total_ticks == TickTimer::from_seconds_ceil(seconds).remaining()
            && canonical_scaled_eq(radius, base_radius, size_scale)
    };
    match kind {
        PenguinSurfaceKind::IceTrailSegment => {
            profile(15.0, 1.05)
                || profile(15.0 * 0.35, 0.78)
                || profile(15.0 * 0.5, 1.05 * 0.82)
                || profile(15.0, 1.05 * 1.08)
        }
        PenguinSurfaceKind::UltimateIceTile => profile(10.0, 0.78),
        PenguinSurfaceKind::SnowHillRamp => profile(6.5, 1.08),
        PenguinSurfaceKind::SnowSlopeRide => profile(2.4, 1.08),
        PenguinSurfaceKind::SnowfortCannon => profile(1.55, 0.0),
        PenguinSurfaceKind::GlacierTrailPrinter => profile(15.0, 0.0),
        PenguinSurfaceKind::SpringPad => profile(1.6, 0.64),
    }
}

fn static_skill_definition(kind: PenguinSkillKind) -> StaticSkillDefinition {
    match kind {
        PenguinSkillKind::FishTorpedo => StaticSkillDefinition {
            payload_id: AttackPayloadId::PenguinFishTorpedo,
            shape_id: AttackShapeId::ProjectileBolt,
            source: ImpactSource::Projectile,
            lifetime_ticks: TickTimer::from_seconds_ceil(0.86).remaining(),
            base_radius: 0.38,
            guard_stamina_damage: 8.0,
            repeat_ticks: None,
            target_allowed: true,
        },
        PenguinSkillKind::PopsicleBounce => StaticSkillDefinition {
            payload_id: AttackPayloadId::PenguinPopsicleBounce,
            shape_id: AttackShapeId::ProjectileBolt,
            source: ImpactSource::Projectile,
            lifetime_ticks: TickTimer::from_seconds_ceil(1.12).remaining(),
            base_radius: 0.34,
            guard_stamina_damage: 9.0,
            repeat_ticks: None,
            target_allowed: false,
        },
        PenguinSkillKind::SledWake => StaticSkillDefinition {
            payload_id: AttackPayloadId::PenguinSledWake,
            shape_id: AttackShapeId::HazardField,
            source: ImpactSource::Hazard,
            lifetime_ticks: TickTimer::from_seconds_ceil(1.18).remaining(),
            base_radius: 0.92,
            guard_stamina_damage: 6.0,
            repeat_ticks: Some(TickTimer::from_seconds_ceil(0.38).remaining()),
            target_allowed: false,
        },
        PenguinSkillKind::SnowflakeShard => StaticSkillDefinition {
            payload_id: AttackPayloadId::PenguinSnowflakeShard,
            shape_id: AttackShapeId::ProjectileBolt,
            source: ImpactSource::Projectile,
            lifetime_ticks: TickTimer::from_seconds_ceil(1.08).remaining(),
            base_radius: 0.32,
            guard_stamina_damage: 10.0,
            repeat_ticks: None,
            target_allowed: false,
        },
        PenguinSkillKind::SnowBoulder => StaticSkillDefinition {
            payload_id: AttackPayloadId::PenguinSnowBoulder,
            shape_id: AttackShapeId::ProjectileBolt,
            source: ImpactSource::Projectile,
            lifetime_ticks: TickTimer::from_seconds_ceil(1.08).remaining(),
            base_radius: 0.46,
            guard_stamina_damage: 11.0,
            repeat_ticks: None,
            target_allowed: false,
        },
        PenguinSkillKind::SnowmanDrop => StaticSkillDefinition {
            payload_id: AttackPayloadId::PenguinSnowmanDrop,
            shape_id: AttackShapeId::ProjectileBolt,
            source: ImpactSource::Projectile,
            lifetime_ticks: TickTimer::from_seconds_ceil(1.05).remaining(),
            base_radius: 0.85 * 1.5,
            guard_stamina_damage: 7.0,
            repeat_ticks: None,
            target_allowed: false,
        },
        PenguinSkillKind::BodySlamShockwave => StaticSkillDefinition {
            payload_id: AttackPayloadId::PenguinBodySlamShockwave,
            shape_id: AttackShapeId::ShockwaveRing,
            source: ImpactSource::Hazard,
            lifetime_ticks: TickTimer::from_seconds_ceil(0.34).remaining(),
            base_radius: 1.55,
            guard_stamina_damage: 14.0,
            repeat_ticks: None,
            target_allowed: false,
        },
    }
}

fn required<'world, T: Component>(
    world: &'world World,
    entity: Entity,
    subject: &'static str,
) -> Result<&'world T, SnapshotCodecError> {
    world.get::<T>(entity).ok_or(error(
        ERR_MISSING_COMPONENT,
        match subject {
            "penguin skill" => "penguin skill is missing a required authoritative component",
            _ => "penguin surface is missing a required authoritative component",
        },
    ))
}

fn require_stable_identity(
    world: &World,
    entity: Entity,
    id: SimEntityId,
    subject: &'static str,
) -> Result<(), SnapshotCodecError> {
    if required::<StableSimEntity>(world, entity, subject)?.id() == id {
        Ok(())
    } else {
        Err(error(
            ERR_IDENTITY_MISMATCH,
            "penguin entity StableSimEntity disagrees with allocator ID",
        ))
    }
}

fn require_kind(id: SimEntityId, expected: SimEntityKind) -> Result<(), SnapshotCodecError> {
    if id.kind() == expected {
        Ok(())
    } else {
        Err(error(
            ERR_WRONG_KIND,
            "penguin snapshot codec received the wrong stable pool kind",
        ))
    }
}

const fn skill_kind_code(value: PenguinSkillKind) -> u8 {
    match value {
        PenguinSkillKind::FishTorpedo => 0,
        PenguinSkillKind::PopsicleBounce => 1,
        PenguinSkillKind::SledWake => 2,
        PenguinSkillKind::SnowflakeShard => 3,
        PenguinSkillKind::SnowBoulder => 4,
        PenguinSkillKind::SnowmanDrop => 5,
        PenguinSkillKind::BodySlamShockwave => 6,
    }
}

const fn skill_kind_from_code(code: u8) -> Option<PenguinSkillKind> {
    match code {
        0 => Some(PenguinSkillKind::FishTorpedo),
        1 => Some(PenguinSkillKind::PopsicleBounce),
        2 => Some(PenguinSkillKind::SledWake),
        3 => Some(PenguinSkillKind::SnowflakeShard),
        4 => Some(PenguinSkillKind::SnowBoulder),
        5 => Some(PenguinSkillKind::SnowmanDrop),
        6 => Some(PenguinSkillKind::BodySlamShockwave),
        _ => None,
    }
}

const fn surface_kind_code(value: PenguinSurfaceKind) -> u8 {
    match value {
        PenguinSurfaceKind::IceTrailSegment => 0,
        PenguinSurfaceKind::UltimateIceTile => 1,
        PenguinSurfaceKind::SnowHillRamp => 2,
        PenguinSurfaceKind::SnowSlopeRide => 3,
        PenguinSurfaceKind::SnowfortCannon => 4,
        PenguinSurfaceKind::GlacierTrailPrinter => 5,
        PenguinSurfaceKind::SpringPad => 6,
    }
}

const fn surface_kind_from_code(code: u8) -> Option<PenguinSurfaceKind> {
    match code {
        0 => Some(PenguinSurfaceKind::IceTrailSegment),
        1 => Some(PenguinSurfaceKind::UltimateIceTile),
        2 => Some(PenguinSurfaceKind::SnowHillRamp),
        3 => Some(PenguinSurfaceKind::SnowSlopeRide),
        4 => Some(PenguinSurfaceKind::SnowfortCannon),
        5 => Some(PenguinSurfaceKind::GlacierTrailPrinter),
        6 => Some(PenguinSurfaceKind::SpringPad),
        _ => None,
    }
}

const fn style_code(value: FighterStyleKind) -> u8 {
    match value {
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

const fn attack_payload_code(value: AttackPayloadId) -> Option<u8> {
    match value {
        AttackPayloadId::PenguinFishTorpedo => Some(0),
        AttackPayloadId::PenguinPopsicleBounce => Some(1),
        AttackPayloadId::PenguinSledWake => Some(2),
        AttackPayloadId::PenguinSnowflakeShard => Some(3),
        AttackPayloadId::PenguinSnowBoulder => Some(4),
        AttackPayloadId::PenguinSnowmanDrop => Some(5),
        AttackPayloadId::PenguinBodySlamShockwave => Some(6),
        _ => None,
    }
}

const fn attack_payload_from_code(code: u8) -> Option<AttackPayloadId> {
    match code {
        0 => Some(AttackPayloadId::PenguinFishTorpedo),
        1 => Some(AttackPayloadId::PenguinPopsicleBounce),
        2 => Some(AttackPayloadId::PenguinSledWake),
        3 => Some(AttackPayloadId::PenguinSnowflakeShard),
        4 => Some(AttackPayloadId::PenguinSnowBoulder),
        5 => Some(AttackPayloadId::PenguinSnowmanDrop),
        6 => Some(AttackPayloadId::PenguinBodySlamShockwave),
        _ => None,
    }
}

const fn attack_shape_code(value: AttackShapeId) -> Option<u8> {
    match value {
        AttackShapeId::ProjectileBolt => Some(0),
        AttackShapeId::HazardField => Some(1),
        AttackShapeId::ShockwaveRing => Some(2),
        _ => None,
    }
}

const fn attack_shape_from_code(code: u8) -> Option<AttackShapeId> {
    match code {
        0 => Some(AttackShapeId::ProjectileBolt),
        1 => Some(AttackShapeId::HazardField),
        2 => Some(AttackShapeId::ShockwaveRing),
        _ => None,
    }
}

const fn impact_source_code(value: ImpactSource) -> Option<u8> {
    match value {
        ImpactSource::Projectile => Some(0),
        ImpactSource::Hazard => Some(1),
        _ => None,
    }
}

const fn impact_source_from_code(code: u8) -> Option<ImpactSource> {
    match code {
        0 => Some(ImpactSource::Projectile),
        1 => Some(ImpactSource::Hazard),
        _ => None,
    }
}

fn write_optional_timer(
    payload: &mut [u8; DYNAMIC_PAYLOAD_BYTES],
    offset: usize,
    timer: Option<TickTimer>,
    present_bit: u8,
    options: &mut u8,
) {
    if let Some(timer) = timer {
        *options |= present_bit;
        write_u32(payload, offset, timer.remaining());
    }
}

fn read_optional_timer(
    payload: &[u8; DYNAMIC_PAYLOAD_BYTES],
    offset: usize,
    options: u8,
    present_bit: u8,
) -> Result<Option<TickTimer>, SnapshotCodecError> {
    let ticks = read_u32(payload, offset);
    if options & present_bit != 0 {
        if ticks == u32::MAX {
            Err(error(
                ERR_TIMER,
                "penguin skill optional timer may not be indefinite",
            ))
        } else {
            Ok(Some(TickTimer::from_ticks(ticks)))
        }
    } else if ticks == 0 {
        Ok(None)
    } else {
        Err(error(
            ERR_OPTION_RELATIONSHIP,
            "absent penguin skill timer contains nonzero payload data",
        ))
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
            .expect("fixed penguin payload offsets are compile-time bounded"),
    )
}

fn read_canonical_f32(
    payload: &[u8; DYNAMIC_PAYLOAD_BYTES],
    offset: usize,
) -> Result<f32, SnapshotCodecError> {
    let value = f32::from_bits(read_u32(payload, offset));
    if value.is_finite() && canonical(value).to_bits() == value.to_bits() {
        Ok(value)
    } else {
        Err(error(
            ERR_NON_CANONICAL_FLOAT,
            "penguin payload float is non-finite or off the canonical grid",
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

fn canonical(value: f32) -> f32 {
    canonicalize_f32(value, DEFAULT_F32_QUANTIZATION)
}

fn canonical_scaled_eq(actual: f32, base: f32, scale: f32) -> bool {
    actual.to_bits() == canonical(base * scale).to_bits()
}

fn valid_facing(facing: Vec3) -> bool {
    let length_squared = crate::canonical_math::vec3_length_squared(facing);
    (0.998..=1.002).contains(&length_squared)
}

const fn error(code: u16, message: &'static str) -> SnapshotCodecError {
    SnapshotCodecError::new(code, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::determinism::FighterId;

    const SKILL_KINDS: [PenguinSkillKind; 7] = [
        PenguinSkillKind::FishTorpedo,
        PenguinSkillKind::PopsicleBounce,
        PenguinSkillKind::SledWake,
        PenguinSkillKind::SnowflakeShard,
        PenguinSkillKind::SnowBoulder,
        PenguinSkillKind::SnowmanDrop,
        PenguinSkillKind::BodySlamShockwave,
    ];

    const SURFACE_KINDS: [PenguinSurfaceKind; 7] = [
        PenguinSurfaceKind::IceTrailSegment,
        PenguinSurfaceKind::UltimateIceTile,
        PenguinSurfaceKind::SnowHillRamp,
        PenguinSurfaceKind::SnowSlopeRide,
        PenguinSurfaceKind::SnowfortCannon,
        PenguinSurfaceKind::GlacierTrailPrinter,
        PenguinSurfaceKind::SpringPad,
    ];

    fn fighter(index: u8) -> FighterId {
        FighterId::new(index).unwrap()
    }

    fn skill_id(index: u32) -> SimEntityId {
        SimEntityId::new(SimEntityKind::PenguinSkill, index, 1)
    }

    fn surface_id(index: u32) -> SimEntityId {
        SimEntityId::new(SimEntityKind::PenguinSurface, index, 1)
    }

    fn q_vec3(x: f32, y: f32, z: f32) -> Vec3 {
        Vec3::new(canonical(x), canonical(y), canonical(z))
    }

    fn fixture_skill(kind: PenguinSkillKind) -> ActivePenguinSkill {
        let definition = static_skill_definition(kind);
        let size_scale = canonical(1.5);
        let repeat_interval = definition.repeat_ticks.map(TickTimer::from_ticks);
        let mut already_hit = FighterHitMask::default();
        if matches!(
            kind,
            PenguinSkillKind::SledWake
                | PenguinSkillKind::SnowmanDrop
                | PenguinSkillKind::BodySlamShockwave
        ) {
            already_hit.insert(fighter(1));
        }
        ActivePenguinSkill {
            kind,
            owner: fighter(2),
            owner_style: FighterStyleKind::Catalyst,
            payload_id: definition.payload_id,
            shape_id: definition.shape_id,
            source: definition.source,
            facing: q_vec3(1.0, 0.0, 0.0),
            velocity: if kind == PenguinSkillKind::BodySlamShockwave {
                Vec3::ZERO
            } else {
                q_vec3(3.25, -0.5, 1.75)
            },
            target: (kind == PenguinSkillKind::FishTorpedo).then_some(fighter(1)),
            lifetime: TickTimer::from_ticks(definition.lifetime_ticks - 3),
            age: ElapsedTicks::from_ticks(3),
            radius: canonical(definition.base_radius * size_scale),
            guard_stamina_damage: canonical(definition.guard_stamina_damage),
            repeat_interval,
            repeat_timer: repeat_interval.map(|_| TickTimer::from_ticks(7)),
            already_hit,
            size_scale,
        }
    }

    fn surface_profile(kind: PenguinSurfaceKind) -> (f32, f32) {
        match kind {
            PenguinSurfaceKind::IceTrailSegment => (15.0, 1.05),
            PenguinSurfaceKind::UltimateIceTile => (10.0, 0.78),
            PenguinSurfaceKind::SnowHillRamp => (6.5, 1.08),
            PenguinSurfaceKind::SnowSlopeRide => (2.4, 1.08),
            PenguinSurfaceKind::SnowfortCannon => (1.55, 0.0),
            PenguinSurfaceKind::GlacierTrailPrinter => (15.0, 0.0),
            PenguinSurfaceKind::SpringPad => (1.6, 0.64),
        }
    }

    fn fixture_surface(kind: PenguinSurfaceKind) -> ActivePenguinSurface {
        let (seconds, base_radius) = surface_profile(kind);
        let total = TickTimer::from_seconds_ceil(seconds).remaining();
        let size_scale = canonical(1.5);
        let mut touched = FighterHitMask::default();
        if matches!(
            kind,
            PenguinSurfaceKind::SnowHillRamp
                | PenguinSurfaceKind::SnowSlopeRide
                | PenguinSurfaceKind::SpringPad
        ) {
            touched.insert(fighter(2));
        }
        ActivePenguinSurface {
            kind,
            owner: fighter(2),
            facing: q_vec3(0.0, 0.0, 1.0),
            lifetime: TickTimer::from_ticks(total - 4),
            age: ElapsedTicks::from_ticks(4),
            radius: canonical(base_radius * size_scale),
            next_tick: if kind == PenguinSurfaceKind::GlacierTrailPrinter {
                TickTimer::from_ticks(9)
            } else {
                TickTimer::ZERO
            },
            already_touched: touched,
            size_scale,
        }
    }

    fn capture_skill(kind: PenguinSkillKind) -> DynamicObjectSnapshot {
        let id = skill_id(u32::from(skill_kind_code(kind)));
        let mut world = World::new();
        let entity = world
            .spawn((
                StableSimEntity::new(id),
                fixture_skill(kind),
                SimPosition::new(q_vec3(1.25, 2.5, -3.75)),
            ))
            .id();
        LivePenguinSkillSnapshotCodec
            .capture(&world, entity, id)
            .unwrap()
    }

    fn capture_surface(kind: PenguinSurfaceKind) -> DynamicObjectSnapshot {
        let id = surface_id(u32::from(surface_kind_code(kind)));
        let mut world = World::new();
        let entity = world
            .spawn((
                StableSimEntity::new(id),
                fixture_surface(kind),
                SimPosition::new(q_vec3(-2.0, 0.25, 4.5)),
            ))
            .id();
        LivePenguinSurfaceSnapshotCodec
            .capture(&world, entity, id)
            .unwrap()
    }

    #[test]
    fn fixed_payload_budgets_and_padding_are_exact() {
        assert_eq!(DYNAMIC_PAYLOAD_BYTES, 128);
        assert_eq!((SKILL_USED_BYTES, SKILL_PADDING_BYTES), (72, 56));
        assert_eq!((SURFACE_USED_BYTES, SURFACE_PADDING_BYTES), (48, 80));
        assert!(
            capture_skill(PenguinSkillKind::SledWake).payload[SKILL_USED_BYTES..]
                .iter()
                .all(|byte| *byte == 0)
        );
        assert!(
            capture_surface(PenguinSurfaceKind::GlacierTrailPrinter).payload[SURFACE_USED_BYTES..]
                .iter()
                .all(|byte| *byte == 0)
        );
    }

    #[test]
    fn every_explicit_catalog_mapping_round_trips() {
        for (code, kind) in SKILL_KINDS.into_iter().enumerate() {
            assert_eq!(skill_kind_code(kind), code as u8);
            assert_eq!(skill_kind_from_code(code as u8), Some(kind));
            let definition = static_skill_definition(kind);
            assert_eq!(
                attack_payload_from_code(attack_payload_code(definition.payload_id).unwrap()),
                Some(definition.payload_id)
            );
            assert_eq!(
                attack_shape_from_code(attack_shape_code(definition.shape_id).unwrap()),
                Some(definition.shape_id)
            );
            assert_eq!(
                impact_source_from_code(impact_source_code(definition.source).unwrap()),
                Some(definition.source)
            );
        }
        for (code, kind) in SURFACE_KINDS.into_iter().enumerate() {
            assert_eq!(surface_kind_code(kind), code as u8);
            assert_eq!(surface_kind_from_code(code as u8), Some(kind));
        }
        for (code, style) in [
            FighterStyleKind::Anchor,
            FighterStyleKind::Vector,
            FighterStyleKind::Catalyst,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(style_code(style), code as u8);
            assert_eq!(style_from_code(code as u8), Some(style));
        }
        assert!(skill_kind_from_code(7).is_none());
        assert!(surface_kind_from_code(7).is_none());
        assert!(style_from_code(3).is_none());
        assert!(attack_payload_from_code(7).is_none());
        assert!(attack_shape_from_code(3).is_none());
        assert!(impact_source_from_code(2).is_none());
    }

    #[test]
    fn every_skill_kind_round_trips_all_authoritative_fields() {
        for kind in SKILL_KINDS {
            let snapshot = capture_skill(kind);
            let mut restored = World::new();
            let entity = restored.spawn_empty().id();
            LivePenguinSkillSnapshotCodec.restore_validated(&mut restored, entity, &snapshot);
            let position = restored.get::<SimPosition>(entity).unwrap();
            let skill = restored.get::<ActivePenguinSkill>(entity).unwrap();
            let expected = fixture_skill(kind);
            assert_eq!(position.translation, q_vec3(1.25, 2.5, -3.75));
            assert!(restored.get::<Transform>(entity).is_none());
            assert_eq!(skill.kind, expected.kind);
            assert_eq!(skill.owner, expected.owner);
            assert_eq!(skill.owner_style, expected.owner_style);
            assert_eq!(skill.payload_id, expected.payload_id);
            assert_eq!(skill.shape_id, expected.shape_id);
            assert_eq!(skill.source, expected.source);
            assert_eq!(skill.facing, expected.facing);
            assert_eq!(skill.velocity, expected.velocity);
            assert_eq!(skill.target, expected.target);
            assert_eq!(skill.lifetime, expected.lifetime);
            assert_eq!(skill.age, expected.age);
            assert_eq!(skill.radius, expected.radius);
            assert_eq!(skill.guard_stamina_damage, expected.guard_stamina_damage);
            assert_eq!(skill.repeat_interval, expected.repeat_interval);
            assert_eq!(skill.repeat_timer, expected.repeat_timer);
            assert_eq!(skill.already_hit, expected.already_hit);
            assert_eq!(skill.size_scale, expected.size_scale);
        }
    }

    #[test]
    fn every_surface_kind_round_trips_all_authoritative_fields() {
        for kind in SURFACE_KINDS {
            let snapshot = capture_surface(kind);
            let mut restored = World::new();
            let entity = restored.spawn_empty().id();
            LivePenguinSurfaceSnapshotCodec.restore_validated(&mut restored, entity, &snapshot);
            let position = restored.get::<SimPosition>(entity).unwrap();
            let surface = restored.get::<ActivePenguinSurface>(entity).unwrap();
            let expected = fixture_surface(kind);
            assert_eq!(position.translation, q_vec3(-2.0, 0.25, 4.5));
            assert!(restored.get::<Transform>(entity).is_none());
            assert_eq!(surface.kind, expected.kind);
            assert_eq!(surface.owner, expected.owner);
            assert_eq!(surface.facing, expected.facing);
            assert_eq!(surface.lifetime, expected.lifetime);
            assert_eq!(surface.age, expected.age);
            assert_eq!(surface.radius, expected.radius);
            assert_eq!(surface.next_tick, expected.next_tick);
            assert_eq!(surface.already_touched, expected.already_touched);
            assert_eq!(surface.size_scale, expected.size_scale);
        }
    }

    #[test]
    fn all_authored_ice_trail_profiles_validate() {
        for (seconds, radius) in [
            (15.0, 1.05),
            (15.0 * 0.35, 0.78),
            (15.0 * 0.5, 1.05 * 0.82),
            (15.0, 1.05 * 1.08),
        ] {
            let mut snapshot = capture_surface(PenguinSurfaceKind::IceTrailSegment);
            let total = TickTimer::from_seconds_ceil(seconds).remaining();
            write_u32(&mut snapshot.payload, SURFACE_AGE_OFFSET, 2);
            write_u32(&mut snapshot.payload, SURFACE_LIFETIME_OFFSET, total - 2);
            write_f32(
                &mut snapshot.payload,
                SURFACE_RADIUS_OFFSET,
                canonical(radius * canonical(1.5)),
            );
            LivePenguinSurfaceSnapshotCodec
                .validate_restore(&World::new(), &snapshot)
                .unwrap();
        }
    }

    #[test]
    fn every_kind_rejects_definition_and_timer_corruption() {
        for kind in SKILL_KINDS {
            let mut snapshot = capture_skill(kind);
            snapshot.definition_id = 99;
            assert!(
                LivePenguinSkillSnapshotCodec
                    .validate_restore(&World::new(), &snapshot)
                    .is_err(),
                "skill kind {kind:?} accepted a mismatched definition"
            );
        }
        for kind in SURFACE_KINDS {
            let mut snapshot = capture_surface(kind);
            write_u32(&mut snapshot.payload, SURFACE_LIFETIME_OFFSET, 1);
            assert!(
                LivePenguinSurfaceSnapshotCodec
                    .validate_restore(&World::new(), &snapshot)
                    .is_err(),
                "surface kind {kind:?} accepted an inconsistent timer"
            );
        }
    }

    #[test]
    fn hostile_outer_options_float_and_padding_are_rejected() {
        let world = World::new();

        let mut skill = capture_skill(PenguinSkillKind::SledWake);
        skill.related_entity = Some(surface_id(0));
        assert!(
            LivePenguinSkillSnapshotCodec
                .validate_restore(&world, &skill)
                .is_err()
        );

        let mut skill = capture_skill(PenguinSkillKind::SledWake);
        skill.payload[SKILL_OPTIONS_OFFSET] = REPEAT_INTERVAL_PRESENT;
        assert!(
            LivePenguinSkillSnapshotCodec
                .validate_restore(&world, &skill)
                .is_err()
        );

        let mut skill = capture_skill(PenguinSkillKind::FishTorpedo);
        write_u32(&mut skill.payload, SKILL_RADIUS_OFFSET, f32::NAN.to_bits());
        assert!(
            LivePenguinSkillSnapshotCodec
                .validate_restore(&world, &skill)
                .is_err()
        );

        let mut skill = capture_skill(PenguinSkillKind::FishTorpedo);
        skill.payload[SKILL_USED_BYTES] = 1;
        assert!(
            LivePenguinSkillSnapshotCodec
                .validate_restore(&world, &skill)
                .is_err()
        );

        let mut skill = capture_skill(PenguinSkillKind::FishTorpedo);
        skill.target = skill.owner;
        assert!(
            LivePenguinSkillSnapshotCodec
                .validate_restore(&world, &skill)
                .is_err()
        );

        let mut surface = capture_surface(PenguinSurfaceKind::UltimateIceTile);
        surface.fighter_hit_mask = 1;
        assert!(
            LivePenguinSurfaceSnapshotCodec
                .validate_restore(&world, &surface)
                .is_err()
        );

        let mut surface = capture_surface(PenguinSurfaceKind::SpringPad);
        surface.payload[SURFACE_USED_BYTES] = 1;
        assert!(
            LivePenguinSurfaceSnapshotCodec
                .validate_restore(&world, &surface)
                .is_err()
        );
    }

    #[test]
    fn every_serialized_penguin_float_joins_the_tick_end_grid() {
        let mut app = App::new();
        app.init_resource::<crate::game_state::MatchTelemetry>()
            .add_systems(
                Update,
                crate::canonical_state::canonicalize_authoritative_state,
            );

        let mut skill = fixture_skill(PenguinSkillKind::FishTorpedo);
        skill.facing = Vec3::new(0.123_456, -0.234_567, 0.345_678);
        skill.velocity = Vec3::new(-1.234_567, 2.345_678, -3.456_789);
        skill.radius = 0.456_789;
        skill.guard_stamina_damage = 8.123_456;
        skill.size_scale = 1.234_567;
        let skill_entity = app
            .world_mut()
            .spawn((
                StableSimEntity::new(skill_id(30)),
                SimPosition::new(Vec3::new(4.567_891, -5.678_912, 6.789_123)),
                skill,
            ))
            .id();

        let mut surface = fixture_surface(PenguinSurfaceKind::SpringPad);
        surface.facing = Vec3::new(-0.765_432, 0.654_321, -0.543_219);
        surface.radius = 1.345_678;
        surface.size_scale = 1.456_789;
        let surface_entity = app
            .world_mut()
            .spawn((
                StableSimEntity::new(surface_id(31)),
                SimPosition::new(Vec3::new(-7.891_234, 8.912_345, -9.123_456)),
                surface,
            ))
            .id();

        app.update();

        let skill_position = app.world().get::<SimPosition>(skill_entity).unwrap();
        let skill = app.world().get::<ActivePenguinSkill>(skill_entity).unwrap();
        assert_eq!(
            skill_position.translation,
            q_vec3(4.567_891, -5.678_912, 6.789_123)
        );
        assert_eq!(skill.facing, q_vec3(0.123_456, -0.234_567, 0.345_678));
        assert_eq!(skill.velocity, q_vec3(-1.234_567, 2.345_678, -3.456_789));
        assert_eq!(skill.radius, canonical(0.456_789));
        assert_eq!(skill.guard_stamina_damage, canonical(8.123_456));
        assert_eq!(skill.size_scale, canonical(1.234_567));

        let surface_position = app.world().get::<SimPosition>(surface_entity).unwrap();
        let surface = app
            .world()
            .get::<ActivePenguinSurface>(surface_entity)
            .unwrap();
        assert_eq!(
            surface_position.translation,
            q_vec3(-7.891_234, 8.912_345, -9.123_456)
        );
        assert_eq!(surface.facing, q_vec3(-0.765_432, 0.654_321, -0.543_219));
        assert_eq!(surface.radius, canonical(1.345_678));
        assert_eq!(surface.size_scale, canonical(1.456_789));
    }

    #[test]
    fn capture_rejects_wrong_identity_kind_and_precanonical_float() {
        let mut world = World::new();
        let id = skill_id(1);
        let entity = world
            .spawn((
                StableSimEntity::new(skill_id(2)),
                fixture_skill(PenguinSkillKind::FishTorpedo),
                SimPosition::new(Vec3::new(0.123_456, 0.0, 0.0)),
            ))
            .id();
        assert!(
            LivePenguinSkillSnapshotCodec
                .capture(&world, entity, id)
                .is_err()
        );
        world.entity_mut(entity).insert(StableSimEntity::new(id));
        assert!(
            LivePenguinSkillSnapshotCodec
                .capture(&world, entity, id)
                .is_err()
        );
        assert!(
            LivePenguinSkillSnapshotCodec
                .capture(&world, entity, surface_id(1))
                .is_err()
        );
    }
}
