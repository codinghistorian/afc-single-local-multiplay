//! Production snapshot mapping for live combat hitboxes.
//!
//! The canonical dynamic record already carries the stable hitbox identity, its
//! owner, and the bounded fighter-hit mask. The fixed payload below retains all
//! remaining authoritative [`Hitbox`] state plus the translation read by combat
//! collision on the next simulation tick. [`SimPosition`] is authoritative;
//! Bevy [`Transform`] is presentation-only and is neither required nor mutated
//! by capture or restore.
//!
//! `Hitbox::path` and `Hitbox::impact_cue` never cross the wire as pointers or
//! strings. Every production spawn derives the path from `shape_id`; payload
//! attacks derive their cue from `payload_id`, while the only payload-less live
//! hitbox is the closed `ItemSwing` definition. Capture rejects a component that
//! violates either derivation, and restore rehydrates the static catalog value.

use bevy::prelude::*;

use crate::characters::CharacterKind;
use crate::components::{AttackKind, Hitbox, SimPosition};
use crate::determinism::{
    DEFAULT_F32_QUANTIZATION, FighterHitMask, SimEntityId, SimEntityKind, canonicalize_f32,
};
use crate::ecs_identity::StableSimEntity;
use crate::effects::HitImpactEffectId;
use crate::equipment::EquipmentKind;
use crate::reactions::ReactionFamilyId;
use crate::simulation::{ElapsedTicks, TickTimer};
use crate::snapshot::{DYNAMIC_PAYLOAD_BYTES, DynamicObjectSnapshot};
use crate::snapshot_ecs::{DynamicSnapshotCodec, SnapshotCodecError};
use crate::styles::FighterStyleKind;
use crate::techniques::{
    AttackPayloadId, AttackShapeId, DamageElement, DamageProfileId, TechniqueId,
    attack_payload_definition, attack_shape_definition,
};

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
const ERR_TIMER: u16 = 10;
const ERR_INVARIANT: u16 = 11;
const ERR_STATIC_DEFINITION: u16 = 12;

const FLAG_GUARDABLE: u32 = 1 << 0;
const FLAG_SCALES_WITH_OWNER_SIZE: u32 = 1 << 1;
const FLAG_PARENTED: u32 = 1 << 2;
const FLAG_EXPIRES_ON_OWNER_LANDING: u32 = 1 << 3;
const FLAG_LANDING_LINGER_STARTED: u32 = 1 << 4;
const FLAG_GROUND_PATH_END: u32 = 1 << 5;
const VALID_FLAGS: u32 = (1 << 6) - 1;

const VERSION_OFFSET: usize = 0;
const ATTACK_KIND_OFFSET: usize = 1;
const CHARACTER_OFFSET: usize = 2;
const TECHNIQUE_OFFSET: usize = 3;
const HIT_EFFECT_OFFSET: usize = 4;
const SHAPE_OFFSET: usize = 5;
const REACTION_OFFSET: usize = 6;
const DAMAGE_PROFILE_OFFSET: usize = 7;
const ELEMENT_OFFSET: usize = 8;
const EQUIPMENT_OFFSET: usize = 9;
const STYLE_OFFSET: usize = 10;
const FEEDBACK_PRIORITY_OFFSET: usize = 11;
const RESERVED_OFFSET: usize = 12;
const POWER_OFFSET: usize = 16;
const STR_SCALE_OFFSET: usize = 20;
const DAMAGE_OFFSET: usize = 24;
const KNOCKBACK_OFFSET: usize = 28;
const VERTICAL_KNOCKBACK_OFFSET: usize = 32;
const BASE_RADIUS_OFFSET: usize = 36;
const RADIUS_OFFSET: usize = 40;
const BASE_RANGE_OFFSET: usize = 44;
const RANGE_OFFSET: usize = 48;
const VERTICAL_OFFSET_SCALE_OFFSET: usize = 52;
const GROUND_PATH_CLEARANCE_OFFSET: usize = 56;
const HITSTOP_SCALE_OFFSET: usize = 60;
const SHAKE_SCALE_OFFSET: usize = 64;
const LIFETIME_OFFSET: usize = 68;
const ELAPSED_OFFSET: usize = 72;
const TOTAL_LIFETIME_OFFSET: usize = 76;
const LANDING_LINGER_OFFSET: usize = 80;
const SPAWN_ORIGIN_OFFSET: usize = 84;
const FACING_OFFSET: usize = 96;
const TRANSLATION_OFFSET: usize = 108;
const USED_BYTES: usize = 120;

const OPTION_NONE: u8 = u8::MAX;

const _: () = assert!(USED_BYTES <= DYNAMIC_PAYLOAD_BYTES);

/// Declares one closed wire table. The generated `to_code` match is deliberately
/// exhaustive, so adding an enum variant cannot silently inherit a discriminant.
macro_rules! wire_table {
    ($table:ident, $to_code:ident, $from_code:ident, $ty:path, [$($variant:path),+ $(,)?]) => {
        const $table: &[$ty] = &[$($variant),+];
        const _: () = assert!($table.len() < u8::MAX as usize);

        fn $to_code(value: $ty) -> u8 {
            match value {
                $($variant => (),)+
            }
            $table
                .iter()
                .position(|candidate| *candidate == value)
                .and_then(|index| u8::try_from(index).ok())
                .expect("closed hitbox wire enum tables fit in u8")
        }

        fn $from_code(code: u8) -> Option<$ty> {
            $table.get(usize::from(code)).copied()
        }
    };
}

wire_table!(
    ATTACK_KINDS,
    attack_kind_code,
    attack_kind_from_code,
    AttackKind,
    [
        AttackKind::Light1,
        AttackKind::Light2,
        AttackKind::ComboFinisher,
        AttackKind::Heavy,
        AttackKind::Ultimate,
        AttackKind::Grab,
        AttackKind::Dash,
        AttackKind::Jump,
        AttackKind::GuardCounter,
        AttackKind::ItemSwing,
        AttackKind::ItemThrow,
        AttackKind::ItemBlast,
        AttackKind::Special,
    ]
);

wire_table!(
    CHARACTERS,
    character_code,
    character_from_code,
    CharacterKind,
    [
        CharacterKind::Cat,
        CharacterKind::Pig,
        CharacterKind::Dog,
        CharacterKind::Fox,
        CharacterKind::Panda,
        CharacterKind::Bee,
        CharacterKind::Penguin,
        CharacterKind::Chick,
    ]
);

wire_table!(
    TECHNIQUES,
    technique_code,
    technique_from_code,
    TechniqueId,
    [
        TechniqueId::CatLight1,
        TechniqueId::CatLight2,
        TechniqueId::CatComboFinisher,
        TechniqueId::CatDashComboFinisher,
        TechniqueId::CatHeavy,
        TechniqueId::CatHeavy2,
        TechniqueId::CatDashAttack,
        TechniqueId::CatJumpAttack,
        TechniqueId::CatJumpHeavy,
        TechniqueId::CatUltimateStartup,
        TechniqueId::CatUltimateRush,
        TechniqueId::PigLight1,
        TechniqueId::PigLight2,
        TechniqueId::PigComboFinisher,
        TechniqueId::PigHeavy,
        TechniqueId::PigHeavy2,
        TechniqueId::PigDashAttack,
        TechniqueId::PigJumpAttack,
        TechniqueId::PigJumpHeavy,
        TechniqueId::PigUltimateStartup,
        TechniqueId::PigUltimateRush,
        TechniqueId::DogLight1,
        TechniqueId::DogLight2,
        TechniqueId::DogComboFinisher,
        TechniqueId::DogHeavy,
        TechniqueId::DogHeavy2,
        TechniqueId::DogDashAttack,
        TechniqueId::DogJumpAttack,
        TechniqueId::DogJumpHeavy,
        TechniqueId::DogUltimateStartup,
        TechniqueId::DogUltimateRush,
        TechniqueId::FoxLight1,
        TechniqueId::FoxLight2,
        TechniqueId::FoxComboFinisher,
        TechniqueId::FoxHeavy,
        TechniqueId::FoxHeavy2,
        TechniqueId::FoxDashAttack,
        TechniqueId::FoxJumpAttack,
        TechniqueId::FoxJumpHeavy,
        TechniqueId::FoxUltimateStartup,
        TechniqueId::FoxUltimateRush,
        TechniqueId::PandaLight1,
        TechniqueId::PandaLight2,
        TechniqueId::PandaComboFinisher,
        TechniqueId::PandaHeavy,
        TechniqueId::PandaHeavy2,
        TechniqueId::PandaDashAttack,
        TechniqueId::PandaJumpAttack,
        TechniqueId::PandaJumpHeavy,
        TechniqueId::PandaUltimateStartup,
        TechniqueId::PandaUltimateRush,
        TechniqueId::BeeLight1,
        TechniqueId::BeeLight2,
        TechniqueId::BeeComboFinisher,
        TechniqueId::BeeHeavy,
        TechniqueId::BeeHeavy2,
        TechniqueId::BeeDashAttack,
        TechniqueId::BeeJumpAttack,
        TechniqueId::BeeJumpHeavy,
        TechniqueId::BeeUltimateStartup,
        TechniqueId::BeeLegacyUltimateStartup,
        TechniqueId::BeeLegacyUltimateRush,
        TechniqueId::PenguinLight1,
        TechniqueId::PenguinLight2,
        TechniqueId::PenguinComboFinisher,
        TechniqueId::PenguinHeavy,
        TechniqueId::PenguinHeavy2,
        TechniqueId::PenguinDashAttack,
        TechniqueId::PenguinDashHeavy,
        TechniqueId::PenguinJumpAttack,
        TechniqueId::PenguinJumpHeavy,
        TechniqueId::PenguinUltimateStartup,
        TechniqueId::PenguinUltimateRush,
        TechniqueId::ChickLight1,
        TechniqueId::ChickLight2,
        TechniqueId::ChickComboFinisher,
        TechniqueId::ChickHeavy,
        TechniqueId::ChickHeavy2,
        TechniqueId::ChickDashAttack,
        TechniqueId::ChickDashHeavy,
        TechniqueId::ChickJumpAttack,
        TechniqueId::ChickJumpHeavy,
        TechniqueId::ChickUltimateStartup,
        TechniqueId::Grab,
        TechniqueId::Throw,
        TechniqueId::GuardCounter,
        TechniqueId::SpecialCast,
        TechniqueId::ItemPickup,
        TechniqueId::ItemSwing,
        TechniqueId::ItemThrow,
        TechniqueId::ItemDrop,
        TechniqueId::GuardStep,
        TechniqueId::QuickStand,
        TechniqueId::RecoveryRoll,
        TechniqueId::LandingRecovery,
    ]
);

wire_table!(
    HIT_EFFECTS,
    hit_effect_code,
    hit_effect_from_code,
    HitImpactEffectId,
    [
        HitImpactEffectId::GenericLight,
        HitImpactEffectId::GenericHeavy,
        HitImpactEffectId::LauncherCyan,
        HitImpactEffectId::GroundBounceOrange,
        HitImpactEffectId::UltimateSlashRed,
        HitImpactEffectId::UltimateBombRed,
        HitImpactEffectId::LightBlue,
        HitImpactEffectId::PigHamSlamHeart,
        HitImpactEffectId::PigHamSwing,
        HitImpactEffectId::PigAirMeatSlam,
    ]
);

wire_table!(
    SHAPES,
    shape_code,
    shape_from_code,
    AttackShapeId,
    [
        AttackShapeId::CompactSlashLead,
        AttackShapeId::CompactSlashFollow,
        AttackShapeId::CompactSlashTight,
        AttackShapeId::LauncherRiser,
        AttackShapeId::BodyRoll,
        AttackShapeId::CompactThrust,
        AttackShapeId::DelayedRiser,
        AttackShapeId::SweepingArcWide,
        AttackShapeId::HookSweep,
        AttackShapeId::RisingColumn,
        AttackShapeId::FallingSpikeArc,
        AttackShapeId::AirFishShot,
        AttackShapeId::CatPounceCatch,
        AttackShapeId::UltimateCatch,
        AttackShapeId::UltimateScratchLeft,
        AttackShapeId::UltimateScratchRight,
        AttackShapeId::UltimateBomb,
        AttackShapeId::ShoulderLine,
        AttackShapeId::CatBodySkid,
        AttackShapeId::GroundSkid,
        AttackShapeId::PenguinSlopeBody,
        AttackShapeId::PenguinUltimateSlopeBody,
        AttackShapeId::PigBodyShove,
        AttackShapeId::PigBellyCrash,
        AttackShapeId::PigRollingPinLine,
        AttackShapeId::PigHamLob,
        AttackShapeId::PigHalfCircleSwing,
        AttackShapeId::PigMeatSlam,
        AttackShapeId::PigAirMeatSlam,
        AttackShapeId::PigUltimateGrab,
        AttackShapeId::CurvedLob,
        AttackShapeId::CounterArc,
        AttackShapeId::ProjectileBolt,
        AttackShapeId::TrapPlate,
        AttackShapeId::ShockwaveRing,
        AttackShapeId::HazardField,
        AttackShapeId::ItemLob,
        AttackShapeId::BombBurst,
        AttackShapeId::GrabCatch,
        AttackShapeId::DashShoulder,
        AttackShapeId::JumpKick,
        AttackShapeId::ItemMelee,
    ]
);

wire_table!(
    REACTIONS,
    reaction_code,
    reaction_from_code,
    ReactionFamilyId,
    [
        ReactionFamilyId::ShortStandingStagger,
        ReactionFamilyId::MediumStandingStagger,
        ReactionFamilyId::HeavyStandingStagger,
        ReactionFamilyId::FrozenStun,
        ReactionFamilyId::LauncherDown,
        ReactionFamilyId::GroundedDownGetup,
        ReactionFamilyId::SlidingKnockdown,
        ReactionFamilyId::LightAirPop,
        ReactionFamilyId::CounterPop,
        ReactionFamilyId::GroundBounceDown,
        ReactionFamilyId::AerialSpikeDown,
        ReactionFamilyId::AirFishKnockdown,
        ReactionFamilyId::UltimateLockedStagger,
        ReactionFamilyId::UltimateBombDown,
    ]
);

wire_table!(
    DAMAGE_PROFILES,
    damage_profile_code,
    damage_profile_from_code,
    DamageProfileId,
    [
        DamageProfileId::Direct,
        DamageProfileId::BasicStrike,
        DamageProfileId::FollowupStrike,
        DamageProfileId::LauncherCommit,
        DamageProfileId::GroundBounce,
        DamageProfileId::GrabControl,
        DamageProfileId::DashBody,
        DamageProfileId::AerialSpike,
        DamageProfileId::CounterBlow,
        DamageProfileId::UltimateRush,
        DamageProfileId::ItemHeavy,
    ]
);

wire_table!(
    DAMAGE_ELEMENTS,
    element_code,
    element_from_code,
    DamageElement,
    [
        DamageElement::Neutral,
        DamageElement::Strike,
        DamageElement::Launch,
        DamageElement::Shock,
        DamageElement::Wind,
        DamageElement::Earth,
        DamageElement::Hazard,
        DamageElement::Blast,
    ]
);

wire_table!(
    EQUIPMENT,
    equipment_code,
    equipment_from_code,
    EquipmentKind,
    [
        EquipmentKind::DashCoil,
        EquipmentKind::AerialSpur,
        EquipmentKind::CounterCell,
        EquipmentKind::HeavySeal,
    ]
);

wire_table!(
    STYLES,
    style_code,
    style_from_code,
    FighterStyleKind,
    [
        FighterStyleKind::Anchor,
        FighterStyleKind::Vector,
        FighterStyleKind::Catalyst,
    ]
);

wire_table!(
    PAYLOADS,
    payload_code,
    payload_from_code,
    AttackPayloadId,
    [
        AttackPayloadId::AsBeat1,
        AttackPayloadId::AsBeat2,
        AttackPayloadId::AssBeat1,
        AttackPayloadId::AssBeat2,
        AttackPayloadId::KiriageBeat1,
        AttackPayloadId::KiriageBeat2,
        AttackPayloadId::HeavyStep,
        AttackPayloadId::ComboFinisherLift,
        AttackPayloadId::ComboFinisher,
        AttackPayloadId::DashComboFinisher,
        AttackPayloadId::PigSnoutShove,
        AttackPayloadId::PigBellyBump,
        AttackPayloadId::PigHamSlam,
        AttackPayloadId::PigRollingPinStep,
        AttackPayloadId::PigHamLauncher,
        AttackPayloadId::PigHamSwingTap,
        AttackPayloadId::PigHamSwingPartial,
        AttackPayloadId::PigHamSwingFull,
        AttackPayloadId::PigAirMeatSlam,
        AttackPayloadId::DogBite1,
        AttackPayloadId::DogBite2,
        AttackPayloadId::DogBodyPounce,
        AttackPayloadId::DogShoulderStep,
        AttackPayloadId::DogLaunchBite,
        AttackPayloadId::FoxSwipe1,
        AttackPayloadId::FoxSwipe2,
        AttackPayloadId::FoxTailSweep,
        AttackPayloadId::FoxSkitterStep,
        AttackPayloadId::FoxFlipLaunch,
        AttackPayloadId::PandaPalm1,
        AttackPayloadId::PandaPalm2,
        AttackPayloadId::PandaBodyDrop,
        AttackPayloadId::PandaWeightShift,
        AttackPayloadId::PandaRisingScoop,
        AttackPayloadId::BeeNeedleTap,
        AttackPayloadId::BeeCrossSting,
        AttackPayloadId::BeeSpiralSting,
        AttackPayloadId::BeePiercingStep,
        AttackPayloadId::BeeHiveLauncher,
        AttackPayloadId::BeeAirSting,
        AttackPayloadId::BeeHiveDive,
        AttackPayloadId::BeeWorkerSting,
        AttackPayloadId::BeeHoneyGlob,
        AttackPayloadId::BeeHoneyPuddle,
        AttackPayloadId::BeeHomingSting,
        AttackPayloadId::PenguinFishSlap1,
        AttackPayloadId::PenguinFishSlap2,
        AttackPayloadId::PenguinBellySlide,
        AttackPayloadId::PenguinPanBonk,
        AttackPayloadId::PenguinSledScoop,
        AttackPayloadId::PenguinSlopeCrash,
        AttackPayloadId::PenguinIceSlide,
        AttackPayloadId::PenguinPopsiclePeck,
        AttackPayloadId::PenguinFrozenFishDive,
        AttackPayloadId::PenguinFishTorpedo,
        AttackPayloadId::PenguinPopsicleBounce,
        AttackPayloadId::PenguinSledWake,
        AttackPayloadId::PenguinSnowflakeShard,
        AttackPayloadId::PenguinSnowBoulder,
        AttackPayloadId::PenguinSnowmanDrop,
        AttackPayloadId::PenguinBodySlamShockwave,
        AttackPayloadId::ChickShellChip,
        AttackPayloadId::ChickFriedEggDisc,
        AttackPayloadId::ChickEggCupMortar,
        AttackPayloadId::ChickOrbitEgg,
        AttackPayloadId::ChickOrbitEggLaunch,
        AttackPayloadId::ChickFreshEggDrop,
        AttackPayloadId::ChickEggplantRoll,
        AttackPayloadId::ChickSunnySplash,
        AttackPayloadId::ChickOmeletField,
        AttackPayloadId::ChickShellScoot,
        AttackPayloadId::ChickShellScramble,
        AttackPayloadId::GrabCatch,
        AttackPayloadId::DashStrike,
        AttackPayloadId::DashShoulderBeat,
        AttackPayloadId::JumpStrike,
        AttackPayloadId::JumpSpike,
        AttackPayloadId::JumpFishShot,
        AttackPayloadId::PigJumpBellyDrop,
        AttackPayloadId::PigHamLob,
        AttackPayloadId::DogJumpPounce,
        AttackPayloadId::DogJumpFishShot,
        AttackPayloadId::FoxJumpSwipe,
        AttackPayloadId::FoxJumpFishShot,
        AttackPayloadId::PandaJumpDrop,
        AttackPayloadId::PandaJumpFishShot,
        AttackPayloadId::UltimateCatch,
        AttackPayloadId::UltimateScratchLight,
        AttackPayloadId::UltimateScratchHeavy,
        AttackPayloadId::UltimateBomb,
        AttackPayloadId::PigUltimateCatch,
        AttackPayloadId::PigUltimateScratchLight,
        AttackPayloadId::PigUltimateScratchHeavy,
        AttackPayloadId::PigUltimateBomb,
        AttackPayloadId::DogUltimateCatch,
        AttackPayloadId::DogUltimateScratchLight,
        AttackPayloadId::DogUltimateScratchHeavy,
        AttackPayloadId::DogUltimateBomb,
        AttackPayloadId::FoxUltimateCatch,
        AttackPayloadId::FoxUltimateScratchLight,
        AttackPayloadId::FoxUltimateScratchHeavy,
        AttackPayloadId::FoxUltimateBomb,
        AttackPayloadId::PandaUltimateCatch,
        AttackPayloadId::PandaUltimateScratchLight,
        AttackPayloadId::PandaUltimateScratchHeavy,
        AttackPayloadId::PandaUltimateBomb,
        AttackPayloadId::BeeUltimateCatch,
        AttackPayloadId::BeeUltimateScratchLight,
        AttackPayloadId::BeeUltimateScratchHeavy,
        AttackPayloadId::BeeUltimateBomb,
        AttackPayloadId::BeeUltimateSwarmTick,
        AttackPayloadId::PenguinUltimateCatch,
        AttackPayloadId::PenguinUltimateScratchLight,
        AttackPayloadId::PenguinUltimateScratchHeavy,
        AttackPayloadId::PenguinUltimateBomb,
        AttackPayloadId::PenguinUltimateSlopeCrash,
        AttackPayloadId::GuardCounter,
        AttackPayloadId::SpecialProjectile,
        AttackPayloadId::SpecialTrap,
        AttackPayloadId::SpecialShockwave,
        AttackPayloadId::SpecialHazard,
        AttackPayloadId::ItemThrowLight,
        AttackPayloadId::ItemThrowHeavy,
        AttackPayloadId::BombBlast,
    ]
);

/// Live codec for [`Hitbox`] entities in the stable hitbox pool.
#[derive(Clone, Copy, Debug, Default)]
pub struct LiveHitboxSnapshotCodec;

struct DecodedHitbox {
    translation: Vec3,
    hitbox: Hitbox,
}

impl DynamicSnapshotCodec for LiveHitboxSnapshotCodec {
    fn capture(
        &self,
        world: &World,
        entity: Entity,
        id: SimEntityId,
    ) -> Result<DynamicObjectSnapshot, SnapshotCodecError> {
        require_kind(id)?;
        require_stable_identity(world, entity, id)?;
        let position = required::<SimPosition>(world, entity)?;
        let hitbox = required::<Hitbox>(world, entity)?;
        validate_static_fields(hitbox)?;

        let mut payload = [0; DYNAMIC_PAYLOAD_BYTES];
        payload[VERSION_OFFSET] = PAYLOAD_VERSION;
        payload[ATTACK_KIND_OFFSET] = attack_kind_code(hitbox.kind);
        payload[CHARACTER_OFFSET] = encode_option(hitbox.attacker_character, character_code);
        payload[TECHNIQUE_OFFSET] = encode_option(hitbox.technique_id, technique_code);
        payload[HIT_EFFECT_OFFSET] = encode_option(hitbox.hit_effect, hit_effect_code);
        payload[SHAPE_OFFSET] = shape_code(hitbox.shape_id);
        payload[REACTION_OFFSET] = reaction_code(hitbox.reaction_family);
        payload[DAMAGE_PROFILE_OFFSET] = damage_profile_code(hitbox.damage_profile);
        payload[ELEMENT_OFFSET] = element_code(hitbox.element);
        payload[EQUIPMENT_OFFSET] = encode_option(hitbox.attacker_equipment, equipment_code);
        payload[STYLE_OFFSET] = encode_option(hitbox.attacker_style, style_code);
        payload[FEEDBACK_PRIORITY_OFFSET] = hitbox.feedback_priority_bonus;
        write_f32(&mut payload, POWER_OFFSET, hitbox.power);
        write_f32(&mut payload, STR_SCALE_OFFSET, hitbox.str_scale);
        write_f32(&mut payload, DAMAGE_OFFSET, hitbox.damage);
        write_f32(&mut payload, KNOCKBACK_OFFSET, hitbox.knockback);
        write_f32(
            &mut payload,
            VERTICAL_KNOCKBACK_OFFSET,
            hitbox.vertical_knockback,
        );
        write_f32(&mut payload, BASE_RADIUS_OFFSET, hitbox.base_radius);
        write_f32(&mut payload, RADIUS_OFFSET, hitbox.radius);
        write_f32(&mut payload, BASE_RANGE_OFFSET, hitbox.base_range);
        write_f32(&mut payload, RANGE_OFFSET, hitbox.range);
        write_f32(
            &mut payload,
            VERTICAL_OFFSET_SCALE_OFFSET,
            hitbox.vertical_offset_scale,
        );
        write_f32(
            &mut payload,
            GROUND_PATH_CLEARANCE_OFFSET,
            hitbox.ground_path_clearance,
        );
        write_f32(&mut payload, HITSTOP_SCALE_OFFSET, hitbox.hitstop_scale);
        write_f32(&mut payload, SHAKE_SCALE_OFFSET, hitbox.shake_scale);
        write_u32(&mut payload, LIFETIME_OFFSET, hitbox.lifetime.remaining());
        write_u32(&mut payload, ELAPSED_OFFSET, hitbox.elapsed.get());
        write_u32(&mut payload, TOTAL_LIFETIME_OFFSET, hitbox.total_lifetime);
        write_u32(
            &mut payload,
            LANDING_LINGER_OFFSET,
            hitbox.landing_linger.remaining(),
        );
        write_vec3(&mut payload, SPAWN_ORIGIN_OFFSET, hitbox.spawn_origin);
        write_vec3(&mut payload, FACING_OFFSET, hitbox.facing);
        write_vec3(&mut payload, TRANSLATION_OFFSET, position.translation);

        let snapshot = DynamicObjectSnapshot {
            id,
            definition_id: payload_definition_id(hitbox.payload_id),
            flags: encode_flags(hitbox),
            owner: Some(hitbox.owner),
            target: None,
            related_entity: None,
            fighter_hit_mask: hitbox.already_hit.bits(),
            payload,
        };
        decode_hitbox(&snapshot)?;
        Ok(snapshot)
    }

    fn validate_restore(
        &self,
        _world: &World,
        snapshot: &DynamicObjectSnapshot,
    ) -> Result<(), SnapshotCodecError> {
        decode_hitbox(snapshot).map(|_| ())
    }

    fn restore_validated(
        &self,
        world: &mut World,
        entity: Entity,
        snapshot: &DynamicObjectSnapshot,
    ) {
        let decoded = decode_hitbox(snapshot)
            .expect("hitbox payload was fully validated before snapshot restore mutation");
        let mut entity = world.entity_mut(entity);
        entity.insert((SimPosition::new(decoded.translation), decoded.hitbox));
    }
}

fn decode_hitbox(snapshot: &DynamicObjectSnapshot) -> Result<DecodedHitbox, SnapshotCodecError> {
    require_kind(snapshot.id)?;
    if snapshot.flags & !VALID_FLAGS != 0 {
        return Err(error(ERR_OUTER_FIELDS, "hitbox flags use reserved bits"));
    }
    let owner = snapshot.owner.ok_or(error(
        ERR_OUTER_FIELDS,
        "hitbox snapshot is missing its fighter owner",
    ))?;
    if snapshot.target.is_some() || snapshot.related_entity.is_some() {
        return Err(error(
            ERR_OUTER_FIELDS,
            "hitbox snapshot contains an unsupported relationship",
        ));
    }
    let already_hit = FighterHitMask::from_bits(snapshot.fighter_hit_mask).ok_or(error(
        ERR_OUTER_FIELDS,
        "hitbox fighter-hit mask uses reserved fighter bits",
    ))?;
    if already_hit.contains(owner) {
        return Err(error(
            ERR_INVARIANT,
            "hitbox fighter-hit mask contains its owner",
        ));
    }
    if snapshot.payload[VERSION_OFFSET] != PAYLOAD_VERSION {
        return Err(error(
            ERR_PAYLOAD_VERSION,
            "unsupported hitbox payload version",
        ));
    }
    if snapshot.payload[RESERVED_OFFSET..POWER_OFFSET]
        .iter()
        .chain(snapshot.payload[USED_BYTES..].iter())
        .any(|byte| *byte != 0)
    {
        return Err(error(
            ERR_PADDING,
            "hitbox payload reserved or padding bytes are nonzero",
        ));
    }

    let payload_id = payload_from_definition_id(snapshot.definition_id)?;
    let kind = decode_code(
        snapshot.payload[ATTACK_KIND_OFFSET],
        attack_kind_from_code,
        "hitbox payload contains an unknown attack-kind code",
    )?;
    let attacker_character = decode_option(
        snapshot.payload[CHARACTER_OFFSET],
        character_from_code,
        "hitbox payload contains an invalid optional character code",
    )?;
    let technique_id = decode_option(
        snapshot.payload[TECHNIQUE_OFFSET],
        technique_from_code,
        "hitbox payload contains an invalid optional technique code",
    )?;
    let hit_effect = decode_option(
        snapshot.payload[HIT_EFFECT_OFFSET],
        hit_effect_from_code,
        "hitbox payload contains an invalid optional hit-effect code",
    )?;
    let shape_id = decode_code(
        snapshot.payload[SHAPE_OFFSET],
        shape_from_code,
        "hitbox payload contains an unknown attack-shape code",
    )?;
    let reaction_family = decode_code(
        snapshot.payload[REACTION_OFFSET],
        reaction_from_code,
        "hitbox payload contains an unknown reaction-family code",
    )?;
    let damage_profile = decode_code(
        snapshot.payload[DAMAGE_PROFILE_OFFSET],
        damage_profile_from_code,
        "hitbox payload contains an unknown damage-profile code",
    )?;
    let element = decode_code(
        snapshot.payload[ELEMENT_OFFSET],
        element_from_code,
        "hitbox payload contains an unknown damage-element code",
    )?;
    let attacker_equipment = decode_option(
        snapshot.payload[EQUIPMENT_OFFSET],
        equipment_from_code,
        "hitbox payload contains an invalid optional equipment code",
    )?;
    let attacker_style = decode_option(
        snapshot.payload[STYLE_OFFSET],
        style_from_code,
        "hitbox payload contains an invalid optional style code",
    )?;

    let power = read_canonical_f32(&snapshot.payload, POWER_OFFSET)?;
    let str_scale = read_canonical_f32(&snapshot.payload, STR_SCALE_OFFSET)?;
    let damage = read_canonical_f32(&snapshot.payload, DAMAGE_OFFSET)?;
    let knockback = read_canonical_f32(&snapshot.payload, KNOCKBACK_OFFSET)?;
    let vertical_knockback = read_canonical_f32(&snapshot.payload, VERTICAL_KNOCKBACK_OFFSET)?;
    let base_radius = read_canonical_f32(&snapshot.payload, BASE_RADIUS_OFFSET)?;
    let radius = read_canonical_f32(&snapshot.payload, RADIUS_OFFSET)?;
    let base_range = read_canonical_f32(&snapshot.payload, BASE_RANGE_OFFSET)?;
    let range = read_canonical_f32(&snapshot.payload, RANGE_OFFSET)?;
    let vertical_offset_scale =
        read_canonical_f32(&snapshot.payload, VERTICAL_OFFSET_SCALE_OFFSET)?;
    let ground_path_clearance =
        read_canonical_f32(&snapshot.payload, GROUND_PATH_CLEARANCE_OFFSET)?;
    let hitstop_scale = read_canonical_f32(&snapshot.payload, HITSTOP_SCALE_OFFSET)?;
    let shake_scale = read_canonical_f32(&snapshot.payload, SHAKE_SCALE_OFFSET)?;
    let lifetime_ticks = read_u32(&snapshot.payload, LIFETIME_OFFSET);
    let elapsed_ticks = read_u32(&snapshot.payload, ELAPSED_OFFSET);
    let total_lifetime = read_u32(&snapshot.payload, TOTAL_LIFETIME_OFFSET);
    let landing_linger_ticks = read_u32(&snapshot.payload, LANDING_LINGER_OFFSET);
    let spawn_origin = read_canonical_vec3(&snapshot.payload, SPAWN_ORIGIN_OFFSET)?;
    let facing = read_canonical_vec3(&snapshot.payload, FACING_OFFSET)?;
    let translation = read_canonical_vec3(&snapshot.payload, TRANSLATION_OFFSET)?;

    let guardable = flag(snapshot.flags, FLAG_GUARDABLE);
    let scales_with_owner_size = flag(snapshot.flags, FLAG_SCALES_WITH_OWNER_SIZE);
    let parented = flag(snapshot.flags, FLAG_PARENTED);
    let expires_on_owner_landing = flag(snapshot.flags, FLAG_EXPIRES_ON_OWNER_LANDING);
    let landing_linger_started = flag(snapshot.flags, FLAG_LANDING_LINGER_STARTED);
    let ground_path_end = flag(snapshot.flags, FLAG_GROUND_PATH_END);

    validate_numeric_invariants(
        power,
        str_scale,
        damage,
        knockback,
        base_radius,
        radius,
        base_range,
        range,
        vertical_offset_scale,
        ground_path_clearance,
        hitstop_scale,
        shake_scale,
        scales_with_owner_size,
        ground_path_end,
    )?;
    let lifetime = finite_active_timer(lifetime_ticks)?;
    if total_lifetime == 0 {
        return Err(error(ERR_TIMER, "hitbox total lifetime must be nonzero"));
    }
    let landing_linger = validate_temporal_state(
        expires_on_owner_landing,
        landing_linger_started,
        landing_linger_ticks,
        lifetime_ticks,
        elapsed_ticks,
        total_lifetime,
    )?;

    let (path, impact_cue) = static_fields(payload_id, kind, shape_id)?;
    let hitbox = Hitbox {
        owner,
        kind,
        payload_id,
        attacker_character,
        technique_id,
        hit_effect,
        shape_id,
        reaction_family,
        damage_profile,
        element,
        attacker_equipment,
        attacker_style,
        power,
        str_scale,
        damage,
        knockback,
        vertical_knockback,
        guardable,
        base_radius,
        radius,
        lifetime,
        elapsed: ElapsedTicks::from_ticks(elapsed_ticks),
        total_lifetime,
        spawn_origin,
        facing,
        base_range,
        range,
        scales_with_owner_size,
        vertical_offset_scale,
        parented,
        path,
        expires_on_owner_landing,
        landing_linger,
        landing_linger_started,
        ground_path_end,
        ground_path_clearance,
        impact_cue,
        hitstop_scale,
        shake_scale,
        feedback_priority_bonus: snapshot.payload[FEEDBACK_PRIORITY_OFFSET],
        already_hit,
    };
    Ok(DecodedHitbox {
        translation,
        hitbox,
    })
}

fn validate_static_fields(hitbox: &Hitbox) -> Result<(), SnapshotCodecError> {
    let (path, impact_cue) = static_fields(hitbox.payload_id, hitbox.kind, hitbox.shape_id)?;
    if hitbox.path != path || hitbox.impact_cue != impact_cue {
        return Err(error(
            ERR_STATIC_DEFINITION,
            "live hitbox path or impact cue disagrees with its closed definition",
        ));
    }
    Ok(())
}

fn static_fields(
    payload_id: Option<AttackPayloadId>,
    kind: AttackKind,
    shape_id: AttackShapeId,
) -> Result<(&'static [[f32; 3]], &'static str), SnapshotCodecError> {
    let path = attack_shape_definition(shape_id).path;
    let impact_cue = match payload_id {
        Some(payload_id) => {
            let payload = attack_payload_definition(payload_id);
            if payload.kind != kind || payload.shape_id != shape_id {
                return Err(error(
                    ERR_STATIC_DEFINITION,
                    "hitbox attack kind or shape disagrees with its payload definition",
                ));
            }
            payload.impact_cue
        }
        None if kind == AttackKind::ItemSwing && shape_id == AttackShapeId::ItemMelee => {
            "impact_item_swing"
        }
        None => {
            return Err(error(
                ERR_STATIC_DEFINITION,
                "unknown payload-less hitbox definition",
            ));
        }
    };
    Ok((path, impact_cue))
}

fn validate_numeric_invariants(
    power: f32,
    str_scale: f32,
    damage: f32,
    knockback: f32,
    base_radius: f32,
    radius: f32,
    base_range: f32,
    range: f32,
    vertical_offset_scale: f32,
    ground_path_clearance: f32,
    hitstop_scale: f32,
    shake_scale: f32,
    scales_with_owner_size: bool,
    ground_path_end: bool,
) -> Result<(), SnapshotCodecError> {
    if power < 0.0
        || str_scale < 0.0
        || damage < 0.0
        || knockback < 0.0
        || base_radius <= 0.0
        || radius <= 0.0
        || base_range < 0.0
        || range < 0.0
        || vertical_offset_scale < 0.0
        || ground_path_clearance < 0.0
        || hitstop_scale < 0.0
        || shake_scale < 0.0
    {
        return Err(error(
            ERR_INVARIANT,
            "hitbox numeric fields violate nonnegative size or combat bounds",
        ));
    }
    if !scales_with_owner_size
        && (base_radius.to_bits() != radius.to_bits() || base_range.to_bits() != range.to_bits())
    {
        return Err(error(
            ERR_INVARIANT,
            "unscaled hitbox range or radius differs from its base value",
        ));
    }
    if !ground_path_end && ground_path_clearance.to_bits() != 0.0_f32.to_bits() {
        return Err(error(
            ERR_INVARIANT,
            "hitbox without a ground-path end has nonzero clearance",
        ));
    }
    Ok(())
}

fn validate_temporal_state(
    expires_on_owner_landing: bool,
    landing_linger_started: bool,
    landing_linger_ticks: u32,
    lifetime_ticks: u32,
    elapsed_ticks: u32,
    total_lifetime: u32,
) -> Result<TickTimer, SnapshotCodecError> {
    if elapsed_ticks > total_lifetime {
        return Err(error(
            ERR_TIMER,
            "hitbox elapsed time exceeds its total lifetime",
        ));
    }
    if !landing_linger_started && elapsed_ticks.checked_add(lifetime_ticks) != Some(total_lifetime)
    {
        return Err(error(
            ERR_TIMER,
            "hitbox elapsed and remaining time disagree with its total lifetime",
        ));
    }
    if !expires_on_owner_landing {
        if landing_linger_started || landing_linger_ticks != 0 {
            return Err(error(
                ERR_INVARIANT,
                "non-landing hitbox contains landing-linger state",
            ));
        }
        return Ok(TickTimer::ZERO);
    }
    let landing_linger = finite_active_timer(landing_linger_ticks)?;
    if landing_linger_started && lifetime_ticks > landing_linger_ticks {
        return Err(error(
            ERR_TIMER,
            "started landing linger has more time than its authored duration",
        ));
    }
    Ok(landing_linger)
}

fn encode_flags(hitbox: &Hitbox) -> u32 {
    u32::from(hitbox.guardable) * FLAG_GUARDABLE
        | u32::from(hitbox.scales_with_owner_size) * FLAG_SCALES_WITH_OWNER_SIZE
        | u32::from(hitbox.parented) * FLAG_PARENTED
        | u32::from(hitbox.expires_on_owner_landing) * FLAG_EXPIRES_ON_OWNER_LANDING
        | u32::from(hitbox.landing_linger_started) * FLAG_LANDING_LINGER_STARTED
        | u32::from(hitbox.ground_path_end) * FLAG_GROUND_PATH_END
}

fn flag(flags: u32, bit: u32) -> bool {
    flags & bit != 0
}

fn payload_definition_id(payload: Option<AttackPayloadId>) -> u16 {
    payload.map_or(0, |payload| u16::from(payload_code(payload)) + 1)
}

fn payload_from_definition_id(
    definition_id: u16,
) -> Result<Option<AttackPayloadId>, SnapshotCodecError> {
    if definition_id == 0 {
        return Ok(None);
    }
    let code = u8::try_from(definition_id - 1).map_err(|_| {
        error(
            ERR_DEFINITION,
            "hitbox definition ID is outside the payload catalog",
        )
    })?;
    payload_from_code(code).map(Some).ok_or(error(
        ERR_DEFINITION,
        "hitbox definition ID names an unknown payload",
    ))
}

fn encode_option<T: Copy>(value: Option<T>, to_code: fn(T) -> u8) -> u8 {
    value.map_or(OPTION_NONE, to_code)
}

fn decode_option<T: Copy>(
    code: u8,
    from_code: fn(u8) -> Option<T>,
    message: &'static str,
) -> Result<Option<T>, SnapshotCodecError> {
    if code == OPTION_NONE {
        Ok(None)
    } else {
        from_code(code)
            .map(Some)
            .ok_or(error(ERR_ENUM_CODE, message))
    }
}

fn decode_code<T: Copy>(
    code: u8,
    from_code: fn(u8) -> Option<T>,
    message: &'static str,
) -> Result<T, SnapshotCodecError> {
    from_code(code).ok_or(error(ERR_ENUM_CODE, message))
}

fn finite_active_timer(ticks: u32) -> Result<TickTimer, SnapshotCodecError> {
    if ticks == 0 || ticks == u32::MAX {
        Err(error(
            ERR_TIMER,
            "live hitbox timer must be active and finite",
        ))
    } else {
        Ok(TickTimer::from_ticks(ticks))
    }
}

fn required<T: Component>(world: &World, entity: Entity) -> Result<&T, SnapshotCodecError> {
    world.get::<T>(entity).ok_or(error(
        ERR_MISSING_COMPONENT,
        "stable hitbox entity is missing a required component",
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
            "stable hitbox identity disagrees with allocator identity",
        ))
    }
}

fn require_kind(id: SimEntityId) -> Result<(), SnapshotCodecError> {
    if id.kind() == SimEntityKind::Hitbox {
        Ok(())
    } else {
        Err(error(
            ERR_WRONG_KIND,
            "hitbox codec received a non-hitbox stable ID",
        ))
    }
}

fn write_f32(payload: &mut [u8; DYNAMIC_PAYLOAD_BYTES], offset: usize, value: f32) {
    write_u32(payload, offset, value.to_bits());
}

fn write_u32(payload: &mut [u8; DYNAMIC_PAYLOAD_BYTES], offset: usize, value: u32) {
    payload[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
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
            .expect("fixed hitbox payload offsets are in bounds"),
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
    if !value.is_finite()
        || canonicalize_f32(value, DEFAULT_F32_QUANTIZATION).to_bits() != value.to_bits()
    {
        return Err(error(
            ERR_NON_CANONICAL_FLOAT,
            "hitbox payload contains a nonfinite or noncanonical float",
        ));
    }
    Ok(value)
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
    use crate::canonical_state::canonicalize_authoritative_state;
    use crate::determinism::FighterId;
    use crate::game_state::MatchTelemetry;
    use crate::simulation::milliseconds_to_ticks_ceil;

    #[derive(Component, Debug, PartialEq, Eq)]
    struct PresentationSentinel(u32);

    fn id(index: u32) -> SimEntityId {
        SimEntityId::new(SimEntityKind::Hitbox, index, 1)
    }

    fn canonical(value: f32) -> f32 {
        canonicalize_f32(value, DEFAULT_F32_QUANTIZATION)
    }

    fn canonical_vec3(value: Vec3) -> Vec3 {
        Vec3::new(canonical(value.x), canonical(value.y), canonical(value.z))
    }

    fn test_hitbox(payload_id: AttackPayloadId) -> Hitbox {
        let payload = attack_payload_definition(payload_id);
        let shape = attack_shape_definition(payload.shape_id);
        let total_lifetime = milliseconds_to_ticks_ceil(payload.time_ms);
        let elapsed = 2;
        Hitbox {
            owner: FighterId::ZERO,
            kind: payload.kind,
            payload_id: Some(payload_id),
            attacker_character: Some(CharacterKind::Cat),
            technique_id: Some(TechniqueId::CatLight1),
            hit_effect: Some(HitImpactEffectId::GenericLight),
            shape_id: payload.shape_id,
            reaction_family: payload.reaction_family,
            damage_profile: payload.damage_profile,
            element: payload.element,
            attacker_equipment: Some(EquipmentKind::DashCoil),
            attacker_style: Some(FighterStyleKind::Vector),
            power: canonical(payload.power),
            str_scale: canonical(payload.str_scale),
            damage: canonical(payload.damage * 1.125),
            knockback: canonical(payload.knockback * 1.25),
            vertical_knockback: canonical(payload.vertical_knockback),
            guardable: payload.guardable,
            base_radius: canonical(shape.radius),
            radius: canonical(shape.radius * 1.25),
            lifetime: TickTimer::from_ticks(total_lifetime - elapsed),
            elapsed: ElapsedTicks::from_ticks(elapsed),
            total_lifetime,
            spawn_origin: canonical_vec3(Vec3::new(1.25, 2.5, -3.75)),
            facing: Vec3::Z,
            base_range: canonical(shape.range),
            range: canonical(shape.range * 1.25),
            scales_with_owner_size: true,
            vertical_offset_scale: canonical(shape.vertical_offset_scale),
            parented: shape.parented,
            path: shape.path,
            expires_on_owner_landing: false,
            landing_linger: TickTimer::ZERO,
            landing_linger_started: false,
            ground_path_end: false,
            ground_path_clearance: 0.0,
            impact_cue: payload.impact_cue,
            hitstop_scale: canonical(payload.hitstop_scale),
            shake_scale: canonical(payload.shake_scale),
            feedback_priority_bonus: payload.feedback_priority_bonus,
            already_hit: FighterHitMask::from_bits(0b0010).unwrap(),
        }
    }

    fn spawn_source(world: &mut World, stable_id: SimEntityId) -> Entity {
        world
            .spawn((
                StableSimEntity::new(stable_id),
                test_hitbox(AttackPayloadId::AsBeat1),
                SimPosition::new(canonical_vec3(Vec3::new(4.0, 5.0, 6.0))),
                Transform::from_translation(canonical_vec3(Vec3::new(4.0, 5.0, 6.0)))
                    .with_rotation(Quat::from_rotation_y(0.75))
                    .with_scale(Vec3::splat(1.75)),
            ))
            .id()
    }

    fn assert_table<T: Copy + std::fmt::Debug + PartialEq>(
        table: &[T],
        to_code: fn(T) -> u8,
        from_code: fn(u8) -> Option<T>,
    ) {
        assert!(table.len() < usize::from(OPTION_NONE));
        for (index, value) in table.iter().copied().enumerate() {
            let code = u8::try_from(index).unwrap();
            assert_eq!(to_code(value), code);
            assert_eq!(from_code(code), Some(value));
        }
        assert_eq!(from_code(OPTION_NONE), None);
    }

    #[test]
    fn every_closed_enum_table_round_trips_and_rejects_the_none_sentinel() {
        assert_table(ATTACK_KINDS, attack_kind_code, attack_kind_from_code);
        assert_table(CHARACTERS, character_code, character_from_code);
        assert_table(TECHNIQUES, technique_code, technique_from_code);
        assert_table(HIT_EFFECTS, hit_effect_code, hit_effect_from_code);
        assert_table(SHAPES, shape_code, shape_from_code);
        assert_table(REACTIONS, reaction_code, reaction_from_code);
        assert_table(
            DAMAGE_PROFILES,
            damage_profile_code,
            damage_profile_from_code,
        );
        assert_table(DAMAGE_ELEMENTS, element_code, element_from_code);
        assert_table(EQUIPMENT, equipment_code, equipment_from_code);
        assert_table(STYLES, style_code, style_from_code);
        assert_table(PAYLOADS, payload_code, payload_from_code);
    }

    #[test]
    fn live_hitbox_round_trip_is_lossless_and_preserves_presentation_components() {
        let stable_id = id(0);
        let mut source = World::new();
        let source_entity = spawn_source(&mut source, stable_id);
        let snapshot = LiveHitboxSnapshotCodec
            .capture(&source, source_entity, stable_id)
            .unwrap();
        assert_eq!(
            snapshot.payload[USED_BYTES..],
            [0; DYNAMIC_PAYLOAD_BYTES - USED_BYTES]
        );

        let preserved_rotation = Quat::from_rotation_x(-0.42);
        let preserved_scale = Vec3::new(2.0, 3.0, 4.0);
        let preserved_translation = Vec3::splat(-9.0);
        let mut restored = World::new();
        let target = restored
            .spawn((
                StableSimEntity::new(stable_id),
                Transform::from_translation(preserved_translation)
                    .with_rotation(preserved_rotation)
                    .with_scale(preserved_scale),
                PresentationSentinel(77),
            ))
            .id();

        LiveHitboxSnapshotCodec
            .validate_restore(&restored, &snapshot)
            .unwrap();
        LiveHitboxSnapshotCodec.restore_validated(&mut restored, target, &snapshot);

        let transform = restored.get::<Transform>(target).unwrap();
        assert_eq!(transform.translation, preserved_translation);
        assert_eq!(transform.rotation, preserved_rotation);
        assert_eq!(transform.scale, preserved_scale);
        assert_eq!(
            restored.get::<SimPosition>(target).unwrap().translation,
            canonical_vec3(Vec3::new(4.0, 5.0, 6.0))
        );
        assert_eq!(
            restored.get::<PresentationSentinel>(target),
            Some(&PresentationSentinel(77))
        );
        assert_eq!(
            LiveHitboxSnapshotCodec
                .capture(&restored, target, stable_id)
                .unwrap(),
            snapshot
        );
        let restored_hitbox = restored.get::<Hitbox>(target).unwrap();
        assert_eq!(
            restored_hitbox.path,
            attack_shape_definition(restored_hitbox.shape_id).path
        );
        assert_eq!(
            restored_hitbox.impact_cue,
            attack_payload_definition(restored_hitbox.payload_id.unwrap()).impact_cue
        );
    }

    #[test]
    fn payload_less_item_swing_uses_the_closed_static_definition() {
        let stable_id = id(0);
        let shape = attack_shape_definition(AttackShapeId::ItemMelee);
        let mut hitbox = test_hitbox(AttackPayloadId::AsBeat1);
        hitbox.kind = AttackKind::ItemSwing;
        hitbox.payload_id = None;
        hitbox.attacker_character = None;
        hitbox.technique_id = None;
        hitbox.hit_effect = None;
        hitbox.shape_id = shape.id;
        hitbox.damage_profile = DamageProfileId::ItemHeavy;
        hitbox.element = DamageElement::Earth;
        hitbox.attacker_equipment = None;
        hitbox.attacker_style = None;
        hitbox.base_radius = canonical(0.5);
        hitbox.radius = hitbox.base_radius;
        hitbox.base_range = canonical(1.25);
        hitbox.range = hitbox.base_range;
        hitbox.scales_with_owner_size = false;
        hitbox.vertical_offset_scale = canonical(shape.vertical_offset_scale);
        hitbox.parented = shape.parented;
        hitbox.path = shape.path;
        hitbox.impact_cue = "impact_item_swing";
        let mut world = World::new();
        let entity = world
            .spawn((
                StableSimEntity::new(stable_id),
                hitbox,
                SimPosition::default(),
            ))
            .id();
        let snapshot = LiveHitboxSnapshotCodec
            .capture(&world, entity, stable_id)
            .unwrap();
        assert_eq!(snapshot.definition_id, 0);
        LiveHitboxSnapshotCodec
            .validate_restore(&world, &snapshot)
            .unwrap();
    }

    #[test]
    fn hostile_payloads_fail_closed_without_mutating_the_target() {
        let stable_id = id(0);
        let mut source = World::new();
        let source_entity = spawn_source(&mut source, stable_id);
        let valid = LiveHitboxSnapshotCodec
            .capture(&source, source_entity, stable_id)
            .unwrap();

        let sentinel_translation = Vec3::new(-4.0, -5.0, -6.0);
        let mut target_world = World::new();
        let target = target_world
            .spawn((
                StableSimEntity::new(stable_id),
                test_hitbox(AttackPayloadId::AsBeat2),
                SimPosition::new(sentinel_translation),
                Transform::from_translation(Vec3::splat(91.0)),
                PresentationSentinel(123),
            ))
            .id();
        let sentinel_damage = target_world.get::<Hitbox>(target).unwrap().damage;

        let mut hostile = Vec::new();
        let mut bad = valid.clone();
        bad.payload[VERSION_OFFSET] = PAYLOAD_VERSION + 1;
        hostile.push(bad);
        let mut bad = valid.clone();
        bad.payload[SHAPE_OFFSET] = OPTION_NONE;
        hostile.push(bad);
        let mut bad = valid.clone();
        bad.payload[SHAPE_OFFSET] = shape_code(AttackShapeId::CompactSlashFollow);
        hostile.push(bad);
        let mut bad = valid.clone();
        bad.payload[CHARACTER_OFFSET] = OPTION_NONE - 1;
        hostile.push(bad);
        let mut bad = valid.clone();
        bad.payload[RESERVED_OFFSET] = 1;
        hostile.push(bad);
        let mut bad = valid.clone();
        bad.payload[DYNAMIC_PAYLOAD_BYTES - 1] = 1;
        hostile.push(bad);
        let mut bad = valid.clone();
        write_f32(&mut bad.payload, DAMAGE_OFFSET, 0.1);
        hostile.push(bad);
        let mut bad = valid.clone();
        write_u32(&mut bad.payload, DAMAGE_OFFSET, f32::NAN.to_bits());
        hostile.push(bad);
        let mut bad = valid.clone();
        bad.flags |= 1 << 31;
        hostile.push(bad);
        let mut bad = valid.clone();
        bad.owner = None;
        hostile.push(bad);
        let mut bad = valid.clone();
        bad.target = Some(FighterId::ALL[1]);
        hostile.push(bad);
        let mut bad = valid.clone();
        bad.related_entity = Some(SimEntityId::new(SimEntityKind::Item, 0, 1));
        hostile.push(bad);
        let mut bad = valid.clone();
        bad.fighter_hit_mask |= 1 << FighterId::ZERO.get();
        hostile.push(bad);
        let mut bad = valid.clone();
        bad.definition_id = u16::MAX;
        hostile.push(bad);
        let mut bad = valid.clone();
        write_u32(&mut bad.payload, LIFETIME_OFFSET, 0);
        hostile.push(bad);
        let mut bad = valid.clone();
        write_u32(&mut bad.payload, LIFETIME_OFFSET, u32::MAX);
        hostile.push(bad);
        let mut bad = valid.clone();
        write_u32(&mut bad.payload, TOTAL_LIFETIME_OFFSET, 0);
        hostile.push(bad);
        let mut bad = valid.clone();
        write_u32(&mut bad.payload, ELAPSED_OFFSET, 3);
        hostile.push(bad);
        let mut bad = valid.clone();
        write_u32(&mut bad.payload, ELAPSED_OFFSET, 7);
        hostile.push(bad);
        let mut bad = valid.clone();
        write_u32(&mut bad.payload, LANDING_LINGER_OFFSET, 2);
        hostile.push(bad);
        let mut bad = valid.clone();
        bad.payload[ATTACK_KIND_OFFSET] = attack_kind_code(AttackKind::Heavy);
        hostile.push(bad);

        for bad in hostile {
            assert!(
                LiveHitboxSnapshotCodec
                    .validate_restore(&target_world, &bad)
                    .is_err()
            );
            assert_eq!(
                target_world.get::<SimPosition>(target).unwrap().translation,
                sentinel_translation
            );
            assert_eq!(
                target_world.get::<Transform>(target).unwrap().translation,
                Vec3::splat(91.0)
            );
            assert_eq!(
                target_world.get::<Hitbox>(target).unwrap().damage,
                sentinel_damage
            );
            assert_eq!(
                target_world.get::<PresentationSentinel>(target),
                Some(&PresentationSentinel(123))
            );
        }
    }

    #[test]
    fn wrong_kind_missing_identity_and_missing_components_are_rejected() {
        let stable_id = id(0);
        let mut world = World::new();
        let missing_identity = world
            .spawn((
                test_hitbox(AttackPayloadId::AsBeat1),
                SimPosition::default(),
            ))
            .id();
        assert!(
            LiveHitboxSnapshotCodec
                .capture(&world, missing_identity, stable_id)
                .is_err()
        );

        let missing_position = world
            .spawn((
                StableSimEntity::new(stable_id),
                test_hitbox(AttackPayloadId::AsBeat1),
                Transform::default(),
            ))
            .id();
        assert!(
            LiveHitboxSnapshotCodec
                .capture(&world, missing_position, stable_id)
                .is_err()
        );

        let missing_hitbox = world
            .spawn((StableSimEntity::new(stable_id), SimPosition::default()))
            .id();
        assert!(
            LiveHitboxSnapshotCodec
                .capture(&world, missing_hitbox, stable_id)
                .is_err()
        );

        let mismatched_id = id(1);
        let entity = spawn_source(&mut world, stable_id);
        assert!(
            LiveHitboxSnapshotCodec
                .capture(&world, entity, mismatched_id)
                .is_err()
        );
        let wrong_kind = SimEntityId::new(SimEntityKind::Special, 0, 1);
        assert!(
            LiveHitboxSnapshotCodec
                .capture(&world, entity, wrong_kind)
                .is_err()
        );
    }

    #[test]
    fn tick_end_canonicalization_covers_every_serialized_hitbox_float() {
        let stable_id = id(0);
        let payload = attack_payload_definition(AttackPayloadId::AsBeat1);
        let shape = attack_shape_definition(payload.shape_id);
        let mut hitbox = test_hitbox(payload.id);
        hitbox.power = 9.123_456;
        hitbox.str_scale = 0.812_345;
        hitbox.damage = 7.234_567;
        hitbox.knockback = 3.345_678;
        hitbox.vertical_knockback = 1.456_789;
        hitbox.base_radius = 0.567_891;
        hitbox.radius = 0.678_912;
        hitbox.base_range = 1.789_123;
        hitbox.range = 1.891_234;
        hitbox.vertical_offset_scale = 0.512_345;
        hitbox.ground_path_clearance = 0.0;
        hitbox.hitstop_scale = 1.123_456;
        hitbox.shake_scale = 0.923_456;
        hitbox.spawn_origin = Vec3::new(0.123_456, 1.234_567, -2.345_678);
        hitbox.facing = Vec3::new(0.123_456, 0.0, 0.987_654);
        hitbox.path = shape.path;
        hitbox.impact_cue = payload.impact_cue;

        let mut app = App::new();
        app.insert_resource(MatchTelemetry::default());
        app.add_systems(Update, canonicalize_authoritative_state);
        let entity = app
            .world_mut()
            .spawn((
                StableSimEntity::new(stable_id),
                hitbox,
                SimPosition::new(Vec3::new(3.456_789, 4.567_891, 5.678_912)),
            ))
            .id();

        assert!(
            LiveHitboxSnapshotCodec
                .capture(app.world(), entity, stable_id)
                .is_err()
        );
        app.update();
        LiveHitboxSnapshotCodec
            .capture(app.world(), entity, stable_id)
            .expect("TickEnd canonicalization must make every wire float canonical");

        let canonicalized = app.world().get::<Hitbox>(entity).unwrap();
        for value in [
            canonicalized.power,
            canonicalized.str_scale,
            canonicalized.damage,
            canonicalized.knockback,
            canonicalized.vertical_knockback,
            canonicalized.base_radius,
            canonicalized.radius,
            canonicalized.base_range,
            canonicalized.range,
            canonicalized.vertical_offset_scale,
            canonicalized.ground_path_clearance,
            canonicalized.hitstop_scale,
            canonicalized.shake_scale,
        ] {
            assert_eq!(canonical(value).to_bits(), value.to_bits());
        }
    }
}
