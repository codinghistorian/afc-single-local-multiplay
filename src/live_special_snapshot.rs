//! Production snapshot codec for stable special entities.
//!
//! The payload is an explicit versioned protocol record. It never serializes a
//! Rust discriminant, a Bevy entity/handle, or either of the cue strings stored
//! by [`ActiveSpecial`]. Cue and enum values are translated through fixed code
//! tables below. The outer dynamic record owns the stable identity, fighter
//! owner, feedback flags, and per-fighter hit mask.
//!
//! Payload v1 layout (little endian):
//!
//! - bytes `0..16`: version, explicit catalog codes, option bits, reserved
//! - bytes `16..52`: translation, facing, and velocity (`f32` bits)
//! - bytes `52..96`: tick timers and millisecond timing data
//! - bytes `96..104`: stamina disruption and guard stamina damage
//! - bytes `104..128`: required zero padding
//!
//! [`SimPosition`] is the authoritative translation. Bevy `Transform` is
//! presentation-only: rotation only spins the mesh, while scale is re-derived
//! from kind and age. Restore inserts canonical position and leaves presentation
//! to rehydrate the render transform and visuals.

use bevy::prelude::*;

use crate::combat::ImpactSource;
use crate::components::SimPosition;
use crate::determinism::{
    DEFAULT_F32_QUANTIZATION, FighterHitMask, SimEntityId, SimEntityKind, canonicalize_f32,
};
use crate::ecs_identity::StableSimEntity;
use crate::effects::FeedbackPackageId;
use crate::simulation::{ElapsedTicks, TickTimer, milliseconds_to_ticks_ceil};
use crate::snapshot::{DYNAMIC_PAYLOAD_BYTES, DynamicObjectSnapshot};
use crate::snapshot_ecs::{DynamicSnapshotCodec, SnapshotCodecError};
use crate::specials::{ActiveSpecial, SpecialKind};
use crate::styles::FighterStyleKind;
use crate::techniques::{AttackPayloadId, AttackShapeId, MsTimingWindow};

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
const ERR_TIMING_WINDOW: u16 = 12;
const ERR_OPTION_RELATIONSHIP: u16 = 13;
const ERR_FEEDBACK_STATE: u16 = 14;
const ERR_SCALAR_RANGE: u16 = 15;

const ACTIVE_FEEDBACK_SENT: u32 = 1 << 0;
const AFTERMATH_FEEDBACK_SENT: u32 = 1 << 1;
const VALID_OUTER_FLAGS: u32 = ACTIVE_FEEDBACK_SENT | AFTERMATH_FEEDBACK_SENT;

const END_MS_PRESENT: u8 = 1 << 0;
const REPEAT_MS_PRESENT: u8 = 1 << 1;
const NEXT_REPEAT_MS_PRESENT: u8 = 1 << 2;
const AFTERMATH_MS_PRESENT: u8 = 1 << 3;
const VALID_OPTION_FLAGS: u8 =
    END_MS_PRESENT | REPEAT_MS_PRESENT | NEXT_REPEAT_MS_PRESENT | AFTERMATH_MS_PRESENT;

const VERSION_OFFSET: usize = 0;
const KIND_OFFSET: usize = 1;
const STYLE_OFFSET: usize = 2;
const SOURCE_OFFSET: usize = 3;
const ATTACK_PAYLOAD_OFFSET: usize = 4;
const ATTACK_SHAPE_OFFSET: usize = 5;
const ACTIVE_CUE_OFFSET: usize = 6;
const AFTERMATH_CUE_OFFSET: usize = 7;
const ACTIVE_PACKAGE_OFFSET: usize = 8;
const REPEAT_PACKAGE_OFFSET: usize = 9;
const IMPACT_PACKAGE_OFFSET: usize = 10;
const AFTERMATH_PACKAGE_OFFSET: usize = 11;
const DESPAWN_PACKAGE_OFFSET: usize = 12;
const OPTION_FLAGS_OFFSET: usize = 13;
const RESERVED_OFFSET: usize = 14;
const TRANSLATION_OFFSET: usize = 16;
const FACING_OFFSET: usize = 28;
const VELOCITY_OFFSET: usize = 40;
const LIFETIME_OFFSET: usize = 52;
const AGE_OFFSET: usize = 56;
const TOTAL_LIFETIME_MS_OFFSET: usize = 60;
const RADIUS_OFFSET: usize = 64;
const GRACE_OFFSET: usize = 68;
const LAUNCH_MS_OFFSET: usize = 72;
const ACTIVE_START_MS_OFFSET: usize = 76;
const ACTIVE_END_MS_OFFSET: usize = 80;
const REPEAT_MS_OFFSET: usize = 84;
const NEXT_REPEAT_MS_OFFSET: usize = 88;
const AFTERMATH_MS_OFFSET: usize = 92;
const STAMINA_DISRUPT_OFFSET: usize = 96;
const GUARD_STAMINA_DAMAGE_OFFSET: usize = 100;
const SPECIAL_USED_BYTES: usize = 104;
const SPECIAL_PADDING_BYTES: usize = DYNAMIC_PAYLOAD_BYTES - SPECIAL_USED_BYTES;

const _: () = assert!(SPECIAL_USED_BYTES == 104);
const _: () = assert!(SPECIAL_PADDING_BYTES == 24);
const _: () = assert!(SPECIAL_USED_BYTES <= DYNAMIC_PAYLOAD_BYTES);

/// Live codec for [`ActiveSpecial`] entities in the stable special pool.
#[derive(Clone, Copy, Debug, Default)]
pub struct LiveSpecialSnapshotCodec;

struct DecodedSpecial {
    translation: Vec3,
    special: ActiveSpecial,
}

#[derive(Clone, Copy)]
struct StaticSpecialIdentity {
    payload_id: AttackPayloadId,
    shape_id: AttackShapeId,
    source: ImpactSource,
    active_cue: &'static str,
    aftermath_cue: &'static str,
    active_package: FeedbackPackageId,
    repeat_package: Option<FeedbackPackageId>,
    impact_package: FeedbackPackageId,
    aftermath_package: FeedbackPackageId,
    despawn_package: FeedbackPackageId,
}

impl DynamicSnapshotCodec for LiveSpecialSnapshotCodec {
    fn capture(
        &self,
        world: &World,
        entity: Entity,
        id: SimEntityId,
    ) -> Result<DynamicObjectSnapshot, SnapshotCodecError> {
        require_kind(id)?;
        require_stable_identity(world, entity, id)?;
        let position = required::<SimPosition>(world, entity)?;
        let special = required::<ActiveSpecial>(world, entity)?;

        let mut payload = [0; DYNAMIC_PAYLOAD_BYTES];
        payload[VERSION_OFFSET] = PAYLOAD_VERSION;
        payload[KIND_OFFSET] = special_kind_code(special.kind);
        payload[STYLE_OFFSET] = style_code(special.owner_style);
        payload[SOURCE_OFFSET] = impact_source_code(special.source).ok_or(error(
            ERR_ENUM_CODE,
            "special uses an unsupported impact-source value",
        ))?;
        payload[ATTACK_PAYLOAD_OFFSET] = attack_payload_code(special.payload_id).ok_or(error(
            ERR_ENUM_CODE,
            "special uses an unsupported attack-payload value",
        ))?;
        payload[ATTACK_SHAPE_OFFSET] = attack_shape_code(special.shape_id).ok_or(error(
            ERR_ENUM_CODE,
            "special uses an unsupported attack-shape value",
        ))?;
        payload[ACTIVE_CUE_OFFSET] = cue_code(special.active_cue).ok_or(error(
            ERR_ENUM_CODE,
            "special uses an unsupported active cue",
        ))?;
        payload[AFTERMATH_CUE_OFFSET] = cue_code(special.aftermath_cue).ok_or(error(
            ERR_ENUM_CODE,
            "special uses an unsupported aftermath cue",
        ))?;
        payload[ACTIVE_PACKAGE_OFFSET] =
            feedback_package_code(special.active_package).ok_or(error(
                ERR_ENUM_CODE,
                "special uses an unsupported active feedback package",
            ))?;
        payload[REPEAT_PACKAGE_OFFSET] = match special.repeat_package {
            Some(package) => feedback_package_code(package).ok_or(error(
                ERR_ENUM_CODE,
                "special uses an unsupported repeat feedback package",
            ))?,
            None => 0,
        };
        payload[IMPACT_PACKAGE_OFFSET] =
            feedback_package_code(special.impact_package).ok_or(error(
                ERR_ENUM_CODE,
                "special uses an unsupported impact feedback package",
            ))?;
        payload[AFTERMATH_PACKAGE_OFFSET] = feedback_package_code(special.aftermath_package)
            .ok_or(error(
                ERR_ENUM_CODE,
                "special uses an unsupported aftermath feedback package",
            ))?;
        payload[DESPAWN_PACKAGE_OFFSET] =
            feedback_package_code(special.despawn_package).ok_or(error(
                ERR_ENUM_CODE,
                "special uses an unsupported despawn feedback package",
            ))?;

        let mut option_flags = 0;
        write_optional_u32(
            &mut payload,
            ACTIVE_END_MS_OFFSET,
            special.active_window.end_ms,
            END_MS_PRESENT,
            &mut option_flags,
        );
        write_optional_u32(
            &mut payload,
            REPEAT_MS_OFFSET,
            special.repeat_ms,
            REPEAT_MS_PRESENT,
            &mut option_flags,
        );
        write_optional_u32(
            &mut payload,
            NEXT_REPEAT_MS_OFFSET,
            special.next_repeat_ms,
            NEXT_REPEAT_MS_PRESENT,
            &mut option_flags,
        );
        write_optional_u32(
            &mut payload,
            AFTERMATH_MS_OFFSET,
            special.aftermath_ms,
            AFTERMATH_MS_PRESENT,
            &mut option_flags,
        );
        payload[OPTION_FLAGS_OFFSET] = option_flags;

        write_vec3(&mut payload, TRANSLATION_OFFSET, position.translation);
        write_vec3(&mut payload, FACING_OFFSET, special.facing);
        write_vec3(&mut payload, VELOCITY_OFFSET, special.velocity);
        write_u32(&mut payload, LIFETIME_OFFSET, special.lifetime.remaining());
        write_u32(&mut payload, AGE_OFFSET, special.age.get());
        write_u32(
            &mut payload,
            TOTAL_LIFETIME_MS_OFFSET,
            special.total_lifetime_ms,
        );
        write_f32(&mut payload, RADIUS_OFFSET, special.radius);
        write_u32(&mut payload, GRACE_OFFSET, special.grace.remaining());
        write_u32(&mut payload, LAUNCH_MS_OFFSET, special.launch_ms);
        write_u32(
            &mut payload,
            ACTIVE_START_MS_OFFSET,
            special.active_window.start_ms,
        );
        write_f32(
            &mut payload,
            STAMINA_DISRUPT_OFFSET,
            special.stamina_disrupt,
        );
        write_f32(
            &mut payload,
            GUARD_STAMINA_DAMAGE_OFFSET,
            special.guard_stamina_damage,
        );

        let snapshot = DynamicObjectSnapshot {
            id,
            definition_id: u16::from(special_kind_code(special.kind)),
            flags: u32::from(special.active_feedback_sent) * ACTIVE_FEEDBACK_SENT
                | u32::from(special.aftermath_feedback_sent) * AFTERMATH_FEEDBACK_SENT,
            owner: Some(special.owner),
            target: None,
            related_entity: None,
            fighter_hit_mask: special.already_hit.bits(),
            payload,
        };
        decode_special(&snapshot)?;
        Ok(snapshot)
    }

    fn validate_restore(
        &self,
        _world: &World,
        snapshot: &DynamicObjectSnapshot,
    ) -> Result<(), SnapshotCodecError> {
        decode_special(snapshot).map(|_| ())
    }

    fn restore_validated(
        &self,
        world: &mut World,
        entity: Entity,
        snapshot: &DynamicObjectSnapshot,
    ) {
        let decoded = decode_special(snapshot)
            .expect("special payload was fully validated before snapshot restore mutation");
        world
            .entity_mut(entity)
            .insert((SimPosition::new(decoded.translation), decoded.special));
    }
}

fn decode_special(snapshot: &DynamicObjectSnapshot) -> Result<DecodedSpecial, SnapshotCodecError> {
    require_kind(snapshot.id)?;
    if snapshot.flags & !VALID_OUTER_FLAGS != 0
        || snapshot.target.is_some()
        || snapshot.related_entity.is_some()
    {
        return Err(error(
            ERR_OUTER_FIELDS,
            "special snapshot uses reserved flags or unsupported relationships",
        ));
    }
    let owner = snapshot.owner.ok_or(error(
        ERR_OUTER_FIELDS,
        "special snapshot is missing its fighter owner",
    ))?;
    let already_hit = FighterHitMask::from_bits(snapshot.fighter_hit_mask).ok_or(error(
        ERR_OUTER_FIELDS,
        "special snapshot fighter-hit mask uses reserved fighter bits",
    ))?;

    if snapshot.payload[VERSION_OFFSET] != PAYLOAD_VERSION {
        return Err(error(
            ERR_PAYLOAD_VERSION,
            "unsupported special payload version",
        ));
    }
    if snapshot.payload[RESERVED_OFFSET..TRANSLATION_OFFSET]
        .iter()
        .chain(snapshot.payload[SPECIAL_USED_BYTES..].iter())
        .any(|byte| *byte != 0)
    {
        return Err(error(
            ERR_PADDING,
            "special payload reserved or padding bytes are nonzero",
        ));
    }
    let option_flags = snapshot.payload[OPTION_FLAGS_OFFSET];
    if option_flags & !VALID_OPTION_FLAGS != 0 {
        return Err(error(
            ERR_OPTION_RELATIONSHIP,
            "special payload uses reserved option bits",
        ));
    }

    let kind = special_kind_from_code(snapshot.payload[KIND_OFFSET]).ok_or(error(
        ERR_ENUM_CODE,
        "special payload contains an unknown kind code",
    ))?;
    if snapshot.definition_id != u16::from(special_kind_code(kind)) {
        return Err(error(
            ERR_DEFINITION,
            "special definition ID disagrees with payload kind code",
        ));
    }
    let owner_style = style_from_code(snapshot.payload[STYLE_OFFSET]).ok_or(error(
        ERR_ENUM_CODE,
        "special payload contains an unknown style code",
    ))?;
    let source = impact_source_from_code(snapshot.payload[SOURCE_OFFSET]).ok_or(error(
        ERR_ENUM_CODE,
        "special payload contains an unknown impact-source code",
    ))?;
    let payload_id =
        attack_payload_from_code(snapshot.payload[ATTACK_PAYLOAD_OFFSET]).ok_or(error(
            ERR_ENUM_CODE,
            "special payload contains an unknown attack-payload code",
        ))?;
    let shape_id = attack_shape_from_code(snapshot.payload[ATTACK_SHAPE_OFFSET]).ok_or(error(
        ERR_ENUM_CODE,
        "special payload contains an unknown attack-shape code",
    ))?;
    let active_cue = cue_from_code(snapshot.payload[ACTIVE_CUE_OFFSET]).ok_or(error(
        ERR_ENUM_CODE,
        "special payload contains an unknown active-cue code",
    ))?;
    let aftermath_cue = cue_from_code(snapshot.payload[AFTERMATH_CUE_OFFSET]).ok_or(error(
        ERR_ENUM_CODE,
        "special payload contains an unknown aftermath-cue code",
    ))?;
    let active_package = feedback_package_from_code(snapshot.payload[ACTIVE_PACKAGE_OFFSET])
        .ok_or(error(
            ERR_ENUM_CODE,
            "special payload contains an unknown active-package code",
        ))?;
    let impact_package = feedback_package_from_code(snapshot.payload[IMPACT_PACKAGE_OFFSET])
        .ok_or(error(
            ERR_ENUM_CODE,
            "special payload contains an unknown impact-package code",
        ))?;
    let aftermath_package = feedback_package_from_code(snapshot.payload[AFTERMATH_PACKAGE_OFFSET])
        .ok_or(error(
            ERR_ENUM_CODE,
            "special payload contains an unknown aftermath-package code",
        ))?;
    let despawn_package = feedback_package_from_code(snapshot.payload[DESPAWN_PACKAGE_OFFSET])
        .ok_or(error(
            ERR_ENUM_CODE,
            "special payload contains an unknown despawn-package code",
        ))?;
    let repeat_package = if option_flags & REPEAT_MS_PRESENT != 0 {
        Some(
            feedback_package_from_code(snapshot.payload[REPEAT_PACKAGE_OFFSET]).ok_or(error(
                ERR_ENUM_CODE,
                "special payload contains an unknown repeat-package code",
            ))?,
        )
    } else if snapshot.payload[REPEAT_PACKAGE_OFFSET] == 0 {
        None
    } else {
        return Err(error(
            ERR_OPTION_RELATIONSHIP,
            "special without a repeat interval has a repeat package",
        ));
    };

    let expected = static_special_identity(kind);
    if payload_id != expected.payload_id
        || shape_id != expected.shape_id
        || source != expected.source
        || active_cue != expected.active_cue
        || aftermath_cue != expected.aftermath_cue
        || active_package != expected.active_package
        || repeat_package != expected.repeat_package
        || impact_package != expected.impact_package
        || aftermath_package != expected.aftermath_package
        || despawn_package != expected.despawn_package
    {
        return Err(error(
            ERR_STATIC_RELATIONSHIP,
            "special kind disagrees with its static payload, shape, cue, source, or package",
        ));
    }

    let translation = read_canonical_vec3(&snapshot.payload, TRANSLATION_OFFSET)?;
    let facing = read_canonical_vec3(&snapshot.payload, FACING_OFFSET)?;
    let velocity = read_canonical_vec3(&snapshot.payload, VELOCITY_OFFSET)?;
    let lifetime_ticks = read_u32(&snapshot.payload, LIFETIME_OFFSET);
    let age_ticks = read_u32(&snapshot.payload, AGE_OFFSET);
    let total_lifetime_ms = read_u32(&snapshot.payload, TOTAL_LIFETIME_MS_OFFSET);
    let radius = read_canonical_f32(&snapshot.payload, RADIUS_OFFSET)?;
    let grace_ticks = read_u32(&snapshot.payload, GRACE_OFFSET);
    let launch_ms = read_u32(&snapshot.payload, LAUNCH_MS_OFFSET);
    let active_start_ms = read_u32(&snapshot.payload, ACTIVE_START_MS_OFFSET);
    let active_end_ms = read_optional_u32(
        &snapshot.payload,
        ACTIVE_END_MS_OFFSET,
        option_flags,
        END_MS_PRESENT,
    )?;
    let repeat_ms = read_optional_u32(
        &snapshot.payload,
        REPEAT_MS_OFFSET,
        option_flags,
        REPEAT_MS_PRESENT,
    )?;
    let next_repeat_ms = read_optional_u32(
        &snapshot.payload,
        NEXT_REPEAT_MS_OFFSET,
        option_flags,
        NEXT_REPEAT_MS_PRESENT,
    )?;
    let aftermath_ms = read_optional_u32(
        &snapshot.payload,
        AFTERMATH_MS_OFFSET,
        option_flags,
        AFTERMATH_MS_PRESENT,
    )?;
    let stamina_disrupt = read_canonical_f32(&snapshot.payload, STAMINA_DISRUPT_OFFSET)?;
    let guard_stamina_damage = read_canonical_f32(&snapshot.payload, GUARD_STAMINA_DAMAGE_OFFSET)?;

    if radius <= 0.0 || stamina_disrupt < 0.0 || guard_stamina_damage < 0.0 {
        return Err(error(
            ERR_SCALAR_RANGE,
            "special radius or stamina values are outside their canonical range",
        ));
    }

    validate_timers(lifetime_ticks, age_ticks, total_lifetime_ms, grace_ticks)?;
    validate_timing_options(
        launch_ms,
        MsTimingWindow {
            start_ms: active_start_ms,
            end_ms: active_end_ms,
        },
        repeat_ms,
        next_repeat_ms,
        aftermath_ms,
        total_lifetime_ms,
        ElapsedTicks::from_ticks(age_ticks).as_millis_floor(),
    )?;

    let active_feedback_sent = snapshot.flags & ACTIVE_FEEDBACK_SENT != 0;
    let aftermath_feedback_sent = snapshot.flags & AFTERMATH_FEEDBACK_SENT != 0;
    let age_ms = ElapsedTicks::from_ticks(age_ticks).as_millis_floor();
    if active_feedback_sent != (age_ms >= active_start_ms)
        || aftermath_feedback_sent != aftermath_ms.is_some_and(|value| age_ms >= value)
    {
        return Err(error(
            ERR_FEEDBACK_STATE,
            "special feedback flags disagree with its authoritative age",
        ));
    }

    Ok(DecodedSpecial {
        translation,
        special: ActiveSpecial {
            kind,
            owner,
            owner_style,
            payload_id,
            shape_id,
            source,
            facing,
            velocity,
            lifetime: TickTimer::from_ticks(lifetime_ticks),
            age: ElapsedTicks::from_ticks(age_ticks),
            total_lifetime_ms,
            radius,
            grace: TickTimer::from_ticks(grace_ticks),
            launch_ms,
            active_window: MsTimingWindow {
                start_ms: active_start_ms,
                end_ms: active_end_ms,
            },
            repeat_ms,
            next_repeat_ms,
            active_feedback_sent,
            aftermath_ms,
            aftermath_feedback_sent,
            active_cue,
            aftermath_cue,
            active_package,
            repeat_package,
            impact_package,
            aftermath_package,
            despawn_package,
            stamina_disrupt,
            guard_stamina_damage,
            already_hit,
        },
    })
}

fn validate_timers(
    lifetime_ticks: u32,
    age_ticks: u32,
    total_lifetime_ms: u32,
    grace_ticks: u32,
) -> Result<(), SnapshotCodecError> {
    if total_lifetime_ms == 0 || lifetime_ticks == 0 || lifetime_ticks == u32::MAX {
        return Err(error(
            ERR_TIMER,
            "live special has a zero, expired, or indefinite lifetime",
        ));
    }
    let total_ticks = milliseconds_to_ticks_ceil(total_lifetime_ms);
    if total_ticks == u32::MAX
        || age_ticks.checked_add(lifetime_ticks) != Some(total_ticks)
        || grace_ticks == u32::MAX
        || grace_ticks > total_ticks
    {
        return Err(error(
            ERR_TIMER,
            "special lifetime, age, grace, and total duration are inconsistent",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_timing_options(
    launch_ms: u32,
    active_window: MsTimingWindow,
    repeat_ms: Option<u32>,
    next_repeat_ms: Option<u32>,
    aftermath_ms: Option<u32>,
    total_lifetime_ms: u32,
    age_ms: u32,
) -> Result<(), SnapshotCodecError> {
    if launch_ms > active_window.start_ms || active_window.start_ms > total_lifetime_ms {
        return Err(error(
            ERR_TIMING_WINDOW,
            "special launch or active-window start lies outside its lifetime",
        ));
    }
    if active_window
        .end_ms
        .is_some_and(|end| end < active_window.start_ms || end > total_lifetime_ms)
        || aftermath_ms.is_some_and(|value| value > total_lifetime_ms)
    {
        return Err(error(
            ERR_TIMING_WINDOW,
            "special active-window end or aftermath lies outside its lifetime",
        ));
    }

    match (repeat_ms, next_repeat_ms) {
        (None, None) => {}
        (Some(0), _) => {
            return Err(error(
                ERR_OPTION_RELATIONSHIP,
                "special repeat interval must be nonzero",
            ));
        }
        (Some(repeat), Some(next)) => {
            let threshold = active_window.end_ms.map_or(age_ms, |end| age_ms.min(end));
            let first = active_window.start_ms.checked_add(repeat).ok_or(error(
                ERR_OPTION_RELATIONSHIP,
                "special first repeat time overflows",
            ))?;
            let expected = if threshold < first {
                first
            } else {
                let advances = (threshold - first) / repeat + 1;
                first
                    .checked_add(advances.checked_mul(repeat).ok_or(error(
                        ERR_OPTION_RELATIONSHIP,
                        "special repeat schedule overflows",
                    ))?)
                    .ok_or(error(
                        ERR_OPTION_RELATIONSHIP,
                        "special repeat schedule overflows",
                    ))?
            };
            if next != expected {
                return Err(error(
                    ERR_OPTION_RELATIONSHIP,
                    "special next-repeat time disagrees with its interval and age",
                ));
            }
        }
        _ => {
            return Err(error(
                ERR_OPTION_RELATIONSHIP,
                "special repeat interval and next-repeat option presence disagree",
            ));
        }
    }
    Ok(())
}

fn required<T: Component>(world: &World, entity: Entity) -> Result<&T, SnapshotCodecError> {
    world.get::<T>(entity).ok_or(error(
        ERR_MISSING_COMPONENT,
        "special entity is missing a required authoritative component",
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
            "special StableSimEntity disagrees with allocator ID",
        ))
    }
}

fn require_kind(id: SimEntityId) -> Result<(), SnapshotCodecError> {
    if id.kind() == SimEntityKind::Special {
        Ok(())
    } else {
        Err(error(
            ERR_WRONG_KIND,
            "special snapshot codec received the wrong stable pool kind",
        ))
    }
}

const fn special_kind_code(value: SpecialKind) -> u8 {
    match value {
        SpecialKind::Projectile => 0,
        SpecialKind::Trap => 1,
        SpecialKind::Shockwave => 2,
        SpecialKind::Hazard => 3,
    }
}

const fn special_kind_from_code(code: u8) -> Option<SpecialKind> {
    match code {
        0 => Some(SpecialKind::Projectile),
        1 => Some(SpecialKind::Trap),
        2 => Some(SpecialKind::Shockwave),
        3 => Some(SpecialKind::Hazard),
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
        AttackPayloadId::SpecialProjectile => Some(0),
        AttackPayloadId::SpecialTrap => Some(1),
        AttackPayloadId::SpecialShockwave => Some(2),
        AttackPayloadId::SpecialHazard => Some(3),
        _ => None,
    }
}

const fn attack_payload_from_code(code: u8) -> Option<AttackPayloadId> {
    match code {
        0 => Some(AttackPayloadId::SpecialProjectile),
        1 => Some(AttackPayloadId::SpecialTrap),
        2 => Some(AttackPayloadId::SpecialShockwave),
        3 => Some(AttackPayloadId::SpecialHazard),
        _ => None,
    }
}

const fn attack_shape_code(value: AttackShapeId) -> Option<u8> {
    match value {
        AttackShapeId::ProjectileBolt => Some(0),
        AttackShapeId::TrapPlate => Some(1),
        AttackShapeId::ShockwaveRing => Some(2),
        AttackShapeId::HazardField => Some(3),
        _ => None,
    }
}

const fn attack_shape_from_code(code: u8) -> Option<AttackShapeId> {
    match code {
        0 => Some(AttackShapeId::ProjectileBolt),
        1 => Some(AttackShapeId::TrapPlate),
        2 => Some(AttackShapeId::ShockwaveRing),
        3 => Some(AttackShapeId::HazardField),
        _ => None,
    }
}

const fn impact_source_code(value: ImpactSource) -> Option<u8> {
    match value {
        ImpactSource::Projectile => Some(0),
        ImpactSource::Trap => Some(1),
        ImpactSource::Shockwave => Some(2),
        ImpactSource::Hazard => Some(3),
        _ => None,
    }
}

const fn impact_source_from_code(code: u8) -> Option<ImpactSource> {
    match code {
        0 => Some(ImpactSource::Projectile),
        1 => Some(ImpactSource::Trap),
        2 => Some(ImpactSource::Shockwave),
        3 => Some(ImpactSource::Hazard),
        _ => None,
    }
}

const fn feedback_package_code(value: FeedbackPackageId) -> Option<u8> {
    match value {
        FeedbackPackageId::SpecialProjectileRelease => Some(0),
        FeedbackPackageId::SpecialProjectileImpact => Some(1),
        FeedbackPackageId::SpecialProjectileRecover => Some(2),
        FeedbackPackageId::SpecialTrapArm => Some(3),
        FeedbackPackageId::SpecialTrapImpact => Some(4),
        FeedbackPackageId::SpecialTrapRecover => Some(5),
        FeedbackPackageId::SpecialShockwaveRelease => Some(6),
        FeedbackPackageId::SpecialShockwaveImpact => Some(7),
        FeedbackPackageId::SpecialShockwaveRecover => Some(8),
        FeedbackPackageId::SpecialHazardPulse => Some(9),
        FeedbackPackageId::SpecialHazardImpact => Some(10),
        FeedbackPackageId::SpecialHazardFade => Some(11),
        _ => None,
    }
}

const fn feedback_package_from_code(code: u8) -> Option<FeedbackPackageId> {
    match code {
        0 => Some(FeedbackPackageId::SpecialProjectileRelease),
        1 => Some(FeedbackPackageId::SpecialProjectileImpact),
        2 => Some(FeedbackPackageId::SpecialProjectileRecover),
        3 => Some(FeedbackPackageId::SpecialTrapArm),
        4 => Some(FeedbackPackageId::SpecialTrapImpact),
        5 => Some(FeedbackPackageId::SpecialTrapRecover),
        6 => Some(FeedbackPackageId::SpecialShockwaveRelease),
        7 => Some(FeedbackPackageId::SpecialShockwaveImpact),
        8 => Some(FeedbackPackageId::SpecialShockwaveRecover),
        9 => Some(FeedbackPackageId::SpecialHazardPulse),
        10 => Some(FeedbackPackageId::SpecialHazardImpact),
        11 => Some(FeedbackPackageId::SpecialHazardFade),
        _ => None,
    }
}

const fn cue_code(value: &'static str) -> Option<u8> {
    match value.as_bytes() {
        b"release_special_projectile" => Some(0),
        b"recover_special_projectile" => Some(1),
        b"arm_special_trap" => Some(2),
        b"recover_special_trap" => Some(3),
        b"release_special_shockwave" => Some(4),
        b"recover_special_shockwave" => Some(5),
        b"pulse_special_hazard" => Some(6),
        b"fade_special_hazard" => Some(7),
        _ => None,
    }
}

const fn cue_from_code(code: u8) -> Option<&'static str> {
    match code {
        0 => Some("release_special_projectile"),
        1 => Some("recover_special_projectile"),
        2 => Some("arm_special_trap"),
        3 => Some("recover_special_trap"),
        4 => Some("release_special_shockwave"),
        5 => Some("recover_special_shockwave"),
        6 => Some("pulse_special_hazard"),
        7 => Some("fade_special_hazard"),
        _ => None,
    }
}

const fn static_special_identity(kind: SpecialKind) -> StaticSpecialIdentity {
    match kind {
        SpecialKind::Projectile => StaticSpecialIdentity {
            payload_id: AttackPayloadId::SpecialProjectile,
            shape_id: AttackShapeId::ProjectileBolt,
            source: ImpactSource::Projectile,
            active_cue: "release_special_projectile",
            aftermath_cue: "recover_special_projectile",
            active_package: FeedbackPackageId::SpecialProjectileRelease,
            repeat_package: None,
            impact_package: FeedbackPackageId::SpecialProjectileImpact,
            aftermath_package: FeedbackPackageId::SpecialProjectileRecover,
            despawn_package: FeedbackPackageId::SpecialProjectileRecover,
        },
        SpecialKind::Trap => StaticSpecialIdentity {
            payload_id: AttackPayloadId::SpecialTrap,
            shape_id: AttackShapeId::TrapPlate,
            source: ImpactSource::Trap,
            active_cue: "arm_special_trap",
            aftermath_cue: "recover_special_trap",
            active_package: FeedbackPackageId::SpecialTrapArm,
            repeat_package: None,
            impact_package: FeedbackPackageId::SpecialTrapImpact,
            aftermath_package: FeedbackPackageId::SpecialTrapRecover,
            despawn_package: FeedbackPackageId::SpecialTrapRecover,
        },
        SpecialKind::Shockwave => StaticSpecialIdentity {
            payload_id: AttackPayloadId::SpecialShockwave,
            shape_id: AttackShapeId::ShockwaveRing,
            source: ImpactSource::Shockwave,
            active_cue: "release_special_shockwave",
            aftermath_cue: "recover_special_shockwave",
            active_package: FeedbackPackageId::SpecialShockwaveRelease,
            repeat_package: None,
            impact_package: FeedbackPackageId::SpecialShockwaveImpact,
            aftermath_package: FeedbackPackageId::SpecialShockwaveRecover,
            despawn_package: FeedbackPackageId::SpecialShockwaveRecover,
        },
        SpecialKind::Hazard => StaticSpecialIdentity {
            payload_id: AttackPayloadId::SpecialHazard,
            shape_id: AttackShapeId::HazardField,
            source: ImpactSource::Hazard,
            active_cue: "pulse_special_hazard",
            aftermath_cue: "fade_special_hazard",
            active_package: FeedbackPackageId::SpecialHazardPulse,
            repeat_package: Some(FeedbackPackageId::SpecialHazardPulse),
            impact_package: FeedbackPackageId::SpecialHazardImpact,
            aftermath_package: FeedbackPackageId::SpecialHazardFade,
            despawn_package: FeedbackPackageId::SpecialHazardFade,
        },
    }
}

fn write_optional_u32(
    payload: &mut [u8; DYNAMIC_PAYLOAD_BYTES],
    offset: usize,
    value: Option<u32>,
    present_bit: u8,
    option_flags: &mut u8,
) {
    if let Some(value) = value {
        *option_flags |= present_bit;
        write_u32(payload, offset, value);
    }
}

fn read_optional_u32(
    payload: &[u8; DYNAMIC_PAYLOAD_BYTES],
    offset: usize,
    option_flags: u8,
    present_bit: u8,
) -> Result<Option<u32>, SnapshotCodecError> {
    let value = read_u32(payload, offset);
    if option_flags & present_bit != 0 {
        Ok(Some(value))
    } else if value == 0 {
        Ok(None)
    } else {
        Err(error(
            ERR_OPTION_RELATIONSHIP,
            "absent special option contains nonzero payload data",
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
            .expect("fixed special payload offsets are compile-time bounded"),
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
            "special payload float is non-finite or off the canonical grid",
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

    fn special_id(index: u32) -> SimEntityId {
        SimEntityId::new(SimEntityKind::Special, index, 1)
    }

    fn q(value: f32) -> f32 {
        canonicalize_f32(value, DEFAULT_F32_QUANTIZATION)
    }

    fn canonical_vec3(x: f32, y: f32, z: f32) -> Vec3 {
        Vec3::new(q(x), q(y), q(z))
    }

    fn timing(kind: SpecialKind) -> (u32, MsTimingWindow, Option<u32>, Option<u32>, u32) {
        match kind {
            SpecialKind::Projectile => {
                (90, MsTimingWindow::closed(120, 980), None, Some(1120), 1500)
            }
            SpecialKind::Trap => (0, MsTimingWindow::open_ended(260), None, Some(520), 5500),
            SpecialKind::Shockwave => (0, MsTimingWindow::closed(70, 300), None, Some(300), 320),
            SpecialKind::Hazard => (
                0,
                MsTimingWindow::closed(340, 3600),
                Some(420),
                Some(3600),
                3800,
            ),
        }
    }

    fn next_repeat_for(window: MsTimingWindow, repeat: Option<u32>, age_ticks: u32) -> Option<u32> {
        let repeat = repeat?;
        let age_ms = ElapsedTicks::from_ticks(age_ticks).as_millis_floor();
        let threshold = window.end_ms.map_or(age_ms, |end| age_ms.min(end));
        let first = window.start_ms + repeat;
        if threshold < first {
            Some(first)
        } else {
            Some(first + ((threshold - first) / repeat + 1) * repeat)
        }
    }

    fn test_special(kind: SpecialKind, style: FighterStyleKind, age_ticks: u32) -> ActiveSpecial {
        let identity = static_special_identity(kind);
        let (launch_ms, active_window, repeat_ms, aftermath_ms, total_lifetime_ms) = timing(kind);
        let age_ms = ElapsedTicks::from_ticks(age_ticks).as_millis_floor();
        let total_ticks = milliseconds_to_ticks_ceil(total_lifetime_ms);
        let mut already_hit = FighterHitMask::default();
        already_hit.insert(FighterId::new(1).unwrap());
        ActiveSpecial {
            kind,
            owner: FighterId::new(2).unwrap(),
            owner_style: style,
            payload_id: identity.payload_id,
            shape_id: identity.shape_id,
            source: identity.source,
            facing: canonical_vec3(1.0, 0.0, -0.25),
            velocity: canonical_vec3(3.5, 0.25, -1.0),
            lifetime: TickTimer::from_ticks(total_ticks - age_ticks),
            age: ElapsedTicks::from_ticks(age_ticks),
            total_lifetime_ms,
            radius: q(0.75),
            grace: TickTimer::from_ticks(3),
            launch_ms,
            active_window,
            repeat_ms,
            next_repeat_ms: next_repeat_for(active_window, repeat_ms, age_ticks),
            active_feedback_sent: age_ms >= active_window.start_ms,
            aftermath_ms,
            aftermath_feedback_sent: aftermath_ms.is_some_and(|value| age_ms >= value),
            active_cue: identity.active_cue,
            aftermath_cue: identity.aftermath_cue,
            active_package: identity.active_package,
            repeat_package: identity.repeat_package,
            impact_package: identity.impact_package,
            aftermath_package: identity.aftermath_package,
            despawn_package: identity.despawn_package,
            stamina_disrupt: q(12.0),
            guard_stamina_damage: q(18.0),
            already_hit,
        }
    }

    fn spawn_special(world: &mut World, id: SimEntityId, special: ActiveSpecial) -> Entity {
        world
            .spawn((
                StableSimEntity::new(id),
                special,
                SimPosition::new(canonical_vec3(1.25, 2.5, -3.75)),
            ))
            .id()
    }

    fn capture_fixture(kind: SpecialKind, age_ticks: u32) -> DynamicObjectSnapshot {
        let id = special_id(special_kind_code(kind).into());
        let mut world = World::new();
        let entity = spawn_special(
            &mut world,
            id,
            test_special(kind, FighterStyleKind::Catalyst, age_ticks),
        );
        LiveSpecialSnapshotCodec
            .capture(&world, entity, id)
            .unwrap()
    }

    #[test]
    fn fixed_payload_budget_is_exact_and_padding_is_zero() {
        assert_eq!(DYNAMIC_PAYLOAD_BYTES, 128);
        assert_eq!(SPECIAL_USED_BYTES, 104);
        assert_eq!(SPECIAL_PADDING_BYTES, 24);
        let snapshot = capture_fixture(SpecialKind::Hazard, 100);
        assert!(
            snapshot.payload[SPECIAL_USED_BYTES..]
                .iter()
                .all(|byte| *byte == 0)
        );
    }

    #[test]
    fn every_explicit_catalog_mapping_round_trips() {
        let kinds = [
            SpecialKind::Projectile,
            SpecialKind::Trap,
            SpecialKind::Shockwave,
            SpecialKind::Hazard,
        ];
        for (code, value) in kinds.into_iter().enumerate() {
            assert_eq!(special_kind_code(value), code as u8);
            assert_eq!(special_kind_from_code(code as u8), Some(value));
        }

        let styles = [
            FighterStyleKind::Anchor,
            FighterStyleKind::Vector,
            FighterStyleKind::Catalyst,
        ];
        for (code, value) in styles.into_iter().enumerate() {
            assert_eq!(style_code(value), code as u8);
            assert_eq!(style_from_code(code as u8), Some(value));
        }

        let payloads = [
            AttackPayloadId::SpecialProjectile,
            AttackPayloadId::SpecialTrap,
            AttackPayloadId::SpecialShockwave,
            AttackPayloadId::SpecialHazard,
        ];
        for (code, value) in payloads.into_iter().enumerate() {
            assert_eq!(attack_payload_code(value), Some(code as u8));
            assert_eq!(attack_payload_from_code(code as u8), Some(value));
        }

        let shapes = [
            AttackShapeId::ProjectileBolt,
            AttackShapeId::TrapPlate,
            AttackShapeId::ShockwaveRing,
            AttackShapeId::HazardField,
        ];
        for (code, value) in shapes.into_iter().enumerate() {
            assert_eq!(attack_shape_code(value), Some(code as u8));
            assert_eq!(attack_shape_from_code(code as u8), Some(value));
        }

        let sources = [
            ImpactSource::Projectile,
            ImpactSource::Trap,
            ImpactSource::Shockwave,
            ImpactSource::Hazard,
        ];
        for (code, value) in sources.into_iter().enumerate() {
            assert_eq!(impact_source_code(value), Some(code as u8));
            assert_eq!(impact_source_from_code(code as u8), Some(value));
        }

        let packages = [
            FeedbackPackageId::SpecialProjectileRelease,
            FeedbackPackageId::SpecialProjectileImpact,
            FeedbackPackageId::SpecialProjectileRecover,
            FeedbackPackageId::SpecialTrapArm,
            FeedbackPackageId::SpecialTrapImpact,
            FeedbackPackageId::SpecialTrapRecover,
            FeedbackPackageId::SpecialShockwaveRelease,
            FeedbackPackageId::SpecialShockwaveImpact,
            FeedbackPackageId::SpecialShockwaveRecover,
            FeedbackPackageId::SpecialHazardPulse,
            FeedbackPackageId::SpecialHazardImpact,
            FeedbackPackageId::SpecialHazardFade,
        ];
        for (code, value) in packages.into_iter().enumerate() {
            assert_eq!(feedback_package_code(value), Some(code as u8));
            assert_eq!(feedback_package_from_code(code as u8), Some(value));
        }

        let cues = [
            "release_special_projectile",
            "recover_special_projectile",
            "arm_special_trap",
            "recover_special_trap",
            "release_special_shockwave",
            "recover_special_shockwave",
            "pulse_special_hazard",
            "fade_special_hazard",
        ];
        for (code, value) in cues.into_iter().enumerate() {
            assert_eq!(cue_code(value), Some(code as u8));
            assert_eq!(cue_from_code(code as u8), Some(value));
        }
    }

    #[test]
    fn all_special_kinds_and_optional_variants_round_trip_authoritative_state() {
        let cases = [
            (SpecialKind::Projectile, FighterStyleKind::Anchor, 30),
            (SpecialKind::Trap, FighterStyleKind::Vector, 40),
            (SpecialKind::Shockwave, FighterStyleKind::Catalyst, 10),
            (SpecialKind::Hazard, FighterStyleKind::Catalyst, 100),
        ];

        for (index, (kind, style, age_ticks)) in cases.into_iter().enumerate() {
            let id = special_id(index as u32);
            let mut source = World::new();
            let entity = spawn_special(&mut source, id, test_special(kind, style, age_ticks));
            let snapshot = LiveSpecialSnapshotCodec
                .capture(&source, entity, id)
                .unwrap();

            let mut restored = World::new();
            let target = restored.spawn(StableSimEntity::new(id)).id();
            LiveSpecialSnapshotCodec
                .validate_restore(&restored, &snapshot)
                .unwrap();
            LiveSpecialSnapshotCodec.restore_validated(&mut restored, target, &snapshot);
            assert_eq!(
                LiveSpecialSnapshotCodec
                    .capture(&restored, target, id)
                    .unwrap(),
                snapshot
            );
            assert_eq!(
                restored.get::<SimPosition>(target).unwrap().translation,
                canonical_vec3(1.25, 2.5, -3.75)
            );
            assert!(restored.get::<Transform>(target).is_none());
        }

        let id = special_id(9);
        let mut without_aftermath =
            test_special(SpecialKind::Projectile, FighterStyleKind::Vector, 30);
        without_aftermath.aftermath_ms = None;
        without_aftermath.aftermath_feedback_sent = false;
        let mut world = World::new();
        let entity = spawn_special(&mut world, id, without_aftermath);
        let snapshot = LiveSpecialSnapshotCodec
            .capture(&world, entity, id)
            .unwrap();
        assert_eq!(
            snapshot.payload[OPTION_FLAGS_OFFSET] & AFTERMATH_MS_PRESENT,
            0
        );
        assert_eq!(read_u32(&snapshot.payload, AFTERMATH_MS_OFFSET), 0);

        let mut restored = World::new();
        let target = restored.spawn(StableSimEntity::new(id)).id();
        LiveSpecialSnapshotCodec
            .validate_restore(&restored, &snapshot)
            .unwrap();
        LiveSpecialSnapshotCodec.restore_validated(&mut restored, target, &snapshot);
        assert_eq!(
            LiveSpecialSnapshotCodec
                .capture(&restored, target, id)
                .unwrap(),
            snapshot
        );
    }

    #[test]
    fn tick_end_canonicalizes_every_serialized_special_float() {
        let id = special_id(0);
        let mut special = test_special(SpecialKind::Projectile, FighterStyleKind::Anchor, 10);
        special.stamina_disrupt = 12.000_1;
        special.guard_stamina_damage = 18.000_1;
        let mut app = App::new();
        app.init_resource::<crate::game_state::MatchTelemetry>()
            .add_systems(
                Update,
                crate::canonical_state::canonicalize_authoritative_state,
            );
        let entity = spawn_special(app.world_mut(), id, special);

        assert!(
            LiveSpecialSnapshotCodec
                .capture(app.world(), entity, id)
                .is_err()
        );
        app.update();
        let live = app.world().get::<ActiveSpecial>(entity).unwrap();
        assert_eq!(live.stamina_disrupt, q(12.000_1));
        assert_eq!(live.guard_stamina_damage, q(18.000_1));
        LiveSpecialSnapshotCodec
            .capture(app.world(), entity, id)
            .unwrap();
    }

    #[test]
    fn capture_rejects_missing_components_wrong_identity_and_wrong_kind() {
        let id = special_id(0);
        let special = test_special(SpecialKind::Projectile, FighterStyleKind::Anchor, 10);
        let mut missing = World::new();
        let entity = missing.spawn(StableSimEntity::new(id)).id();
        assert_eq!(
            LiveSpecialSnapshotCodec
                .capture(&missing, entity, id)
                .unwrap_err()
                .code,
            ERR_MISSING_COMPONENT
        );

        let entity = missing
            .spawn((StableSimEntity::new(id), SimPosition::default()))
            .id();
        assert_eq!(
            LiveSpecialSnapshotCodec
                .capture(&missing, entity, id)
                .unwrap_err()
                .code,
            ERR_MISSING_COMPONENT
        );

        let mut mismatch = World::new();
        let entity = spawn_special(&mut mismatch, special_id(1), special);
        assert_eq!(
            LiveSpecialSnapshotCodec
                .capture(&mismatch, entity, id)
                .unwrap_err()
                .code,
            ERR_IDENTITY_MISMATCH
        );

        let wrong_kind = SimEntityId::new(SimEntityKind::Hitbox, 0, 1);
        assert_eq!(
            LiveSpecialSnapshotCodec
                .capture(&mismatch, entity, wrong_kind)
                .unwrap_err()
                .code,
            ERR_WRONG_KIND
        );
    }

    fn assert_mutation_rejected(
        base: &DynamicObjectSnapshot,
        expected_code: u16,
        mutate: impl FnOnce(&mut DynamicObjectSnapshot),
    ) {
        let mut hostile = base.clone();
        mutate(&mut hostile);
        assert_eq!(
            LiveSpecialSnapshotCodec
                .validate_restore(&World::new(), &hostile)
                .unwrap_err()
                .code,
            expected_code
        );
    }

    #[test]
    fn hostile_outer_enum_static_and_padding_mutations_are_rejected() {
        let base = capture_fixture(SpecialKind::Hazard, 100);
        assert_mutation_rejected(&base, ERR_WRONG_KIND, |value| {
            value.id = SimEntityId::new(SimEntityKind::Hitbox, 0, 1)
        });
        assert_mutation_rejected(&base, ERR_OUTER_FIELDS, |value| value.flags |= 1 << 9);
        assert_mutation_rejected(&base, ERR_OUTER_FIELDS, |value| value.owner = None);
        assert_mutation_rejected(&base, ERR_OUTER_FIELDS, |value| {
            value.target = Some(FighterId::ZERO)
        });
        assert_mutation_rejected(&base, ERR_OUTER_FIELDS, |value| {
            value.fighter_hit_mask = 0b1_0000
        });
        assert_mutation_rejected(&base, ERR_DEFINITION, |value| value.definition_id = 2);
        assert_mutation_rejected(&base, ERR_PAYLOAD_VERSION, |value| {
            value.payload[VERSION_OFFSET] = 99
        });
        assert_mutation_rejected(&base, ERR_ENUM_CODE, |value| {
            value.payload[KIND_OFFSET] = 99
        });
        assert_mutation_rejected(&base, ERR_ENUM_CODE, |value| {
            value.payload[STYLE_OFFSET] = 99
        });
        for offset in [
            SOURCE_OFFSET,
            ATTACK_PAYLOAD_OFFSET,
            ATTACK_SHAPE_OFFSET,
            ACTIVE_CUE_OFFSET,
            ACTIVE_PACKAGE_OFFSET,
            REPEAT_PACKAGE_OFFSET,
        ] {
            assert_mutation_rejected(&base, ERR_ENUM_CODE, |value| value.payload[offset] = 99);
        }
        assert_mutation_rejected(&base, ERR_STATIC_RELATIONSHIP, |value| {
            value.payload[ACTIVE_CUE_OFFSET] = cue_code("arm_special_trap").unwrap()
        });
        assert_mutation_rejected(&base, ERR_STATIC_RELATIONSHIP, |value| {
            value.payload[IMPACT_PACKAGE_OFFSET] =
                feedback_package_code(FeedbackPackageId::SpecialTrapImpact).unwrap()
        });
        assert_mutation_rejected(&base, ERR_PADDING, |value| {
            value.payload[RESERVED_OFFSET] = 1
        });
        assert_mutation_rejected(&base, ERR_PADDING, |value| {
            value.payload[DYNAMIC_PAYLOAD_BYTES - 1] = 1
        });
    }

    #[test]
    fn hostile_float_timer_window_and_option_mutations_are_rejected() {
        let base = capture_fixture(SpecialKind::Hazard, 100);
        assert_mutation_rejected(&base, ERR_NON_CANONICAL_FLOAT, |value| {
            write_f32(&mut value.payload, RADIUS_OFFSET, 0.1)
        });
        assert_mutation_rejected(&base, ERR_NON_CANONICAL_FLOAT, |value| {
            write_f32(&mut value.payload, VELOCITY_OFFSET, f32::NAN)
        });
        assert_mutation_rejected(&base, ERR_SCALAR_RANGE, |value| {
            write_f32(&mut value.payload, RADIUS_OFFSET, q(-1.0))
        });
        assert_mutation_rejected(&base, ERR_TIMER, |value| {
            write_u32(&mut value.payload, LIFETIME_OFFSET, 0)
        });
        assert_mutation_rejected(&base, ERR_TIMER, |value| {
            write_u32(&mut value.payload, GRACE_OFFSET, u32::MAX)
        });
        assert_mutation_rejected(&base, ERR_TIMING_WINDOW, |value| {
            write_u32(&mut value.payload, ACTIVE_END_MS_OFFSET, 100)
        });
        assert_mutation_rejected(&base, ERR_OPTION_RELATIONSHIP, |value| {
            value.payload[OPTION_FLAGS_OFFSET] &= !NEXT_REPEAT_MS_PRESENT
        });
        assert_mutation_rejected(&base, ERR_OPTION_RELATIONSHIP, |value| {
            write_u32(&mut value.payload, NEXT_REPEAT_MS_OFFSET, 2440)
        });
        assert_mutation_rejected(&base, ERR_OPTION_RELATIONSHIP, |value| {
            value.payload[OPTION_FLAGS_OFFSET] |= 1 << 7
        });
        assert_mutation_rejected(&base, ERR_FEEDBACK_STATE, |value| {
            value.flags &= !ACTIVE_FEEDBACK_SENT
        });
    }
}
